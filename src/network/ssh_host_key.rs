// Copyright 2025 Lablup Inc. and Jeongkyu Shin
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Host-key verification policies for `view --ssh` (issue #194).
//!
//! Three verifiers matching the three `--ssh-strict-host-key` CLI
//! modes:
//!
//! * [`PermissiveVerifier`] — `no`, accepts anything (with a prominent
//!   warning log).
//! * [`StrictVerifier`] — `yes`, rejects unless the key is in
//!   `known_hosts`.
//! * [`AcceptNewVerifier`] — `accept-new`, TOFU: accept on first
//!   connect and persist the key, reject if the saved key differs.
//!
//! Kept in its own file so [`crate::network::ssh_client`] stays under
//! the 500-line soft limit.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use russh::keys::ssh_key;

use crate::network::ssh_client::{HostKeyVerifier, SshClientError};
use crate::network::ssh_transport::StrictHostKey;

/// `--ssh-strict-host-key=no`: accept any key. Logs a warning so the
/// audit trail still shows the decision.
pub struct PermissiveVerifier;

#[async_trait]
impl HostKeyVerifier for PermissiveVerifier {
    async fn verify(
        &self,
        host: &str,
        port: u16,
        _key: &ssh_key::PublicKey,
    ) -> Result<bool, SshClientError> {
        tracing::warn!(
            host = host,
            port = port,
            "ssh-strict-host-key=no: accepting any host key without verification"
        );
        Ok(true)
    }
}

/// `--ssh-strict-host-key=yes`: reject unless the key is already
/// present in the supplied `known_hosts` file. A missing file causes
/// an immediate reject.
pub struct StrictVerifier {
    pub known_hosts_path: PathBuf,
}

#[async_trait]
impl HostKeyVerifier for StrictVerifier {
    async fn verify(
        &self,
        host: &str,
        port: u16,
        key: &ssh_key::PublicKey,
    ) -> Result<bool, SshClientError> {
        let known = match known_hosts_lookup(&self.known_hosts_path, host, port) {
            Ok(k) => k,
            Err(e) => {
                tracing::warn!(
                    host = host,
                    port = port,
                    error = %e,
                    "strict host-key verifier could not read known_hosts"
                );
                return Ok(false);
            }
        };
        Ok(known.iter().any(|k| k == key))
    }
}

/// `--ssh-strict-host-key=accept-new`: accept unknown hosts on first
/// connect, persist the key, but reject subsequent connections whose
/// key differs from what we saved.
pub struct AcceptNewVerifier {
    pub known_hosts_path: PathBuf,
}

#[async_trait]
impl HostKeyVerifier for AcceptNewVerifier {
    async fn verify(
        &self,
        host: &str,
        port: u16,
        key: &ssh_key::PublicKey,
    ) -> Result<bool, SshClientError> {
        match known_hosts_lookup(&self.known_hosts_path, host, port) {
            Ok(keys) => {
                if keys.is_empty() {
                    // First-time connection: trust-on-first-use.
                    if let Err(e) = known_hosts_append(&self.known_hosts_path, host, port, key) {
                        tracing::warn!(
                            host = host,
                            error = %e,
                            "accept-new: could not persist host key; still accepting this connection"
                        );
                    }
                    Ok(true)
                } else if keys.iter().any(|k| k == key) {
                    Ok(true)
                } else {
                    Ok(false)
                }
            }
            Err(e) => {
                tracing::warn!(
                    host = host,
                    error = %e,
                    "accept-new: could not read known_hosts, refusing"
                );
                Ok(false)
            }
        }
    }
}

/// Look up every saved public key for `host:port` in the given
/// `known_hosts`-style file. The file format is intentionally
/// simplified: each line is `host[:port] <key type> <base64-key>`.
/// We never persist hashed hostnames so the file is both readable and
/// easy to edit manually.
pub(crate) fn known_hosts_lookup(
    path: &Path,
    host: &str,
    port: u16,
) -> Result<Vec<ssh_key::PublicKey>, std::io::Error> {
    let content = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut out = Vec::new();
    let needle_bracket = format!("[{host}]:{port}");
    let needle_plain = host;
    for raw in content.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (host_field, rest) = match line.split_once(char::is_whitespace) {
            Some(p) => p,
            None => continue,
        };
        let matches = if port == 22 {
            host_field == needle_plain || host_field == needle_bracket
        } else {
            host_field == needle_bracket
        };
        if !matches {
            continue;
        }
        // rest is `<algo> <base64> [comment]`. That format matches
        // the OpenSSH public-key format `from_openssh` expects.
        let trimmed = rest.trim();
        if let Ok(key) = ssh_key::PublicKey::from_openssh(trimmed) {
            out.push(key);
        }
    }
    Ok(out)
}

fn known_hosts_append(
    path: &Path,
    host: &str,
    port: u16,
    key: &ssh_key::PublicKey,
) -> Result<(), std::io::Error> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let host_field = if port == 22 {
        host.to_string()
    } else {
        format!("[{host}]:{port}")
    };
    let key_str = key.to_openssh().map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("could not serialise host key: {e}"),
        )
    })?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    // Entry form: `host <ssh-ed25519 AAAA… comment>` — the openssh
    // encoding already includes the `<alg> <base64>` prefix, so we
    // just prepend the host field.
    writeln!(file, "{host_field} {key_str}")?;
    Ok(())
}

/// Build the right [`HostKeyVerifier`] for a given policy + known-hosts
/// path.
pub fn build_verifier(
    policy: StrictHostKey,
    known_hosts: Option<PathBuf>,
) -> Arc<dyn HostKeyVerifier> {
    let known_hosts_path = known_hosts.unwrap_or_else(default_known_hosts);
    match policy {
        StrictHostKey::No => Arc::new(PermissiveVerifier),
        StrictHostKey::Yes => Arc::new(StrictVerifier { known_hosts_path }),
        StrictHostKey::AcceptNew => Arc::new(AcceptNewVerifier { known_hosts_path }),
    }
}

fn default_known_hosts() -> PathBuf {
    if let Some(home) = dirs::home_dir() {
        home.join(".ssh").join("known_hosts")
    } else {
        PathBuf::from("known_hosts")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_hosts_lookup_missing_file_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent");
        let keys = known_hosts_lookup(&path, "host", 22).unwrap();
        assert!(keys.is_empty());
    }

    #[test]
    fn known_hosts_lookup_skips_comments() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("kh");
        std::fs::write(&path, "# a comment\n\n").unwrap();
        assert!(known_hosts_lookup(&path, "host", 22).unwrap().is_empty());
    }

    #[test]
    fn build_verifier_picks_correct_type() {
        // We can't easily sniff `Arc<dyn HostKeyVerifier>`'s concrete
        // type, but we can smoke-test the factory on every policy.
        let _ = build_verifier(StrictHostKey::No, None);
        let _ = build_verifier(StrictHostKey::Yes, None);
        let _ = build_verifier(
            StrictHostKey::AcceptNew,
            Some(PathBuf::from("/nonexistent-test-path")),
        );
    }
}
