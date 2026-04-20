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

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

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
///
/// Keeps a per-process in-memory cache of accepted `(host, port,
/// fingerprint)` triples so a persistence failure (read-only fs, full
/// disk, symlink refusal) does not silently downgrade to `accept-any`:
/// a subsequent connection in the same process still detects when the
/// remote's key has changed.
pub struct AcceptNewVerifier {
    pub known_hosts_path: PathBuf,
    memory_cache: Mutex<HashSet<MemoryCacheKey>>,
}

/// `(host, port, openssh-fingerprint)`. Comparing by fingerprint rather
/// than by the full key avoids having to keep `PublicKey` in the set
/// (it is not `Eq + Hash`).
type MemoryCacheKey = (String, u16, String);

impl AcceptNewVerifier {
    pub fn new(known_hosts_path: PathBuf) -> Self {
        Self {
            known_hosts_path,
            memory_cache: Mutex::new(HashSet::new()),
        }
    }

    fn memory_cache_key(host: &str, port: u16, key: &ssh_key::PublicKey) -> MemoryCacheKey {
        (
            host.to_string(),
            port,
            key.fingerprint(ssh_key::HashAlg::Sha256).to_string(),
        )
    }

    fn memory_cache_contains(&self, host: &str, port: u16, key: &ssh_key::PublicKey) -> bool {
        let needle = Self::memory_cache_key(host, port, key);
        match self.memory_cache.lock() {
            Ok(guard) => guard.contains(&needle),
            Err(poisoned) => poisoned.into_inner().contains(&needle),
        }
    }

    fn memory_cache_any_for(&self, host: &str, port: u16) -> bool {
        match self.memory_cache.lock() {
            Ok(guard) => guard.iter().any(|(h, p, _)| h == host && *p == port),
            Err(poisoned) => poisoned
                .into_inner()
                .iter()
                .any(|(h, p, _)| h == host && *p == port),
        }
    }

    fn memory_cache_insert(&self, host: &str, port: u16, key: &ssh_key::PublicKey) {
        let entry = Self::memory_cache_key(host, port, key);
        match self.memory_cache.lock() {
            Ok(mut guard) => {
                guard.insert(entry);
            }
            Err(poisoned) => {
                poisoned.into_inner().insert(entry);
            }
        }
    }
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
                if keys.iter().any(|k| k == key) {
                    // Known from persisted file.
                    return Ok(true);
                }
                // File has entries for this host but none match, or file
                // has no entries for this host. Decide based on whether
                // the file already knew about the host at all: if yes,
                // this is a key change → reject. If no, it is a first
                // connect → TOFU.
                if !keys.is_empty() {
                    return Ok(false);
                }
                // First-time from persistent store. Check memory cache
                // in case a previous connect in this process saw this
                // host but could not persist it.
                if self.memory_cache_any_for(host, port) {
                    // We have a prior in-memory decision: only accept
                    // if this specific fingerprint matches it.
                    return Ok(self.memory_cache_contains(host, port, key));
                }
                // Genuinely first sighting anywhere. Persist; fall back
                // to the in-memory cache if persistence fails so a
                // subsequent connection in the same process can still
                // detect a key change.
                match known_hosts_append(&self.known_hosts_path, host, port, key) {
                    Ok(()) => {}
                    Err(e) => {
                        tracing::error!(
                            host = host,
                            port = port,
                            path = %self.known_hosts_path.display(),
                            error = %e,
                            "accept-new: could not persist host key; caching in memory for this process only"
                        );
                        self.memory_cache_insert(host, port, key);
                    }
                }
                Ok(true)
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
/// `known_hosts`-style file.
///
/// Supports the common OpenSSH formats:
/// * plain host: `dgx-01 ssh-ed25519 AAAA…`
/// * host + port: `[dgx-01]:2222 ssh-ed25519 AAAA…`
/// * comma-separated host list: `dgx-01,10.0.0.1 ssh-ed25519 AAAA…`
///
/// Hashed hostnames (`|1|…`) are skipped rather than matched — we do
/// not implement the HMAC-SHA1 hostname lookup OpenSSH uses. Our own
/// [`known_hosts_append`] never writes the hashed form, so this is a
/// no-op for files we created. Third-party files still function; the
/// hashed lines are ignored.
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
    for raw in content.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (host_field, rest) = match line.split_once(char::is_whitespace) {
            Some(p) => p,
            None => continue,
        };
        // Skip OpenSSH hashed-hostname entries — we do not implement
        // the HMAC-SHA1 lookup they require.
        if host_field.starts_with("|1|") {
            continue;
        }
        // OpenSSH allows a comma-separated list of host patterns on a
        // single line. Any one of them may match.
        let mut matched = false;
        for token in host_field.split(',') {
            let token = token.trim();
            if token.is_empty() {
                continue;
            }
            let hit = if port == 22 {
                token == host || token == needle_bracket
            } else {
                token == needle_bracket
            };
            if hit {
                matched = true;
                break;
            }
        }
        if !matched {
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

/// Append a `host key` entry to `known_hosts` without following
/// symlinks.
///
/// Security: if an attacker controls the directory holding the
/// `known_hosts` file, they could pre-plant a symlink pointing at
/// `~/.bashrc` or another shell rc file. When we then append a new
/// "host key" line, the attacker-chosen bytes (host name + key data)
/// would land in that file. We defend with a double-check:
///
/// 1. `symlink_metadata` — explicit refusal if the target path is a
///    symlink at the time we check.
/// 2. `O_NOFOLLOW` on the `open` syscall (Unix) — the kernel itself
///    refuses to open through a final-component symlink, closing the
///    TOCTOU window between the metadata check and the open.
///
/// On create, we set mode `0o600` to match OpenSSH's semantics for new
/// `known_hosts` files.
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
    // Reject a pre-existing symlink up-front. This is deliberately
    // checked even on non-Unix platforms so the attack surface is
    // narrowed everywhere.
    if path.exists() {
        let md = std::fs::symlink_metadata(path)?;
        if md.file_type().is_symlink() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "refusing to append to symlinked known_hosts at {}",
                    path.display()
                ),
            ));
        }
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
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // 0o600 mirrors OpenSSH's default mode for fresh known_hosts
        // files. O_NOFOLLOW closes the TOCTOU window between the
        // symlink_metadata check above and the actual open.
        opts.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = opts.open(path)?;
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
        StrictHostKey::AcceptNew => Arc::new(AcceptNewVerifier::new(known_hosts_path)),
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

    /// Two pre-generated ed25519 public keys used by fixtures that
    /// need to simulate a "key change" between connections. Generated
    /// with `ssh-keygen -t ed25519`; kept as literal strings so tests
    /// are hermetic (no RNG, no filesystem) and deterministic across
    /// runs. These keys exist only as test fixtures — their private
    /// halves are discarded.
    const TEST_KEY_A: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAILbRhbtx7s0p+e18aTwbGaHN+8UqaBcSRNCE+GU5v6Q7 all-smi-test-a";
    const TEST_KEY_B: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIO+UHj3N+jyzN/51w3elCnDai2okb8wc+d4JCKQGd23o all-smi-test-b";

    fn test_public_key() -> ssh_key::PublicKey {
        ssh_key::PublicKey::from_openssh(TEST_KEY_A).expect("fixture key A must parse")
    }

    fn test_public_key_pair() -> (ssh_key::PublicKey, ssh_key::PublicKey) {
        (
            ssh_key::PublicKey::from_openssh(TEST_KEY_A).expect("fixture key A must parse"),
            ssh_key::PublicKey::from_openssh(TEST_KEY_B).expect("fixture key B must parse"),
        )
    }

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
    fn known_hosts_lookup_matches_multi_host_line() {
        // OpenSSH-style comma-separated host list: any of the tokens
        // must be accepted as a match for the requested host.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("kh");
        let key = test_public_key();
        let key_str = key.to_openssh().unwrap();
        std::fs::write(&path, format!("host-a,host-b,host-c {key_str}\n")).unwrap();

        let keys = known_hosts_lookup(&path, "host-b", 22).unwrap();
        assert_eq!(keys.len(), 1, "multi-host entry must match middle token");
        assert_eq!(keys[0], key);

        let keys = known_hosts_lookup(&path, "host-c", 22).unwrap();
        assert_eq!(keys.len(), 1, "multi-host entry must match last token");

        let keys = known_hosts_lookup(&path, "not-in-list", 22).unwrap();
        assert!(keys.is_empty(), "hostname absent from list must not match");
    }

    #[test]
    fn known_hosts_lookup_skips_hashed_hostnames() {
        // Hashed hostname lines (leading `|1|`) are skipped rather
        // than rejected — the file remains usable overall.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("kh");
        let key = test_public_key();
        let key_str = key.to_openssh().unwrap();
        let hashed_line = "|1|AAAA|BBBB ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIGarbage";
        std::fs::write(&path, format!("{hashed_line}\nplain-host {key_str}\n")).unwrap();

        let keys = known_hosts_lookup(&path, "plain-host", 22).unwrap();
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0], key);
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

    #[cfg(unix)]
    #[test]
    fn known_hosts_append_refuses_symlink_target() {
        // C1 regression: a pre-planted symlink at the known_hosts path
        // must NOT be dereferenced when we append. The target file
        // must remain untouched.
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let decoy = dir.path().join("decoy.txt");
        std::fs::write(&decoy, b"original-contents\n").unwrap();
        let kh_path = dir.path().join("kh");
        symlink(&decoy, &kh_path).expect("creating symlink");

        let key = test_public_key();
        let err = known_hosts_append(&kh_path, "attacker-host", 22, &key)
            .expect_err("append through symlink must fail");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);

        // Decoy file must still contain exactly its original bytes.
        let after = std::fs::read_to_string(&decoy).unwrap();
        assert_eq!(
            after, "original-contents\n",
            "symlink target must not be appended to"
        );
    }

    #[cfg(unix)]
    #[test]
    fn known_hosts_append_creates_with_restrictive_mode() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let kh_path = dir.path().join("kh");
        let key = test_public_key();
        known_hosts_append(&kh_path, "host", 22, &key).expect("clean append must succeed");

        let md = std::fs::metadata(&kh_path).unwrap();
        let mode = md.permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "fresh known_hosts must be mode 0o600");
    }

    #[tokio::test]
    async fn accept_new_falls_back_to_memory_on_persist_failure() {
        // M4 regression: when the known_hosts path cannot be written
        // (here: points at a non-creatable location), the verifier
        // must still detect a key change on the NEXT connection of
        // the same process by consulting its in-memory cache.
        let dir = tempfile::tempdir().unwrap();
        // Create a read-only directory so append() fails deterministically
        // without relying on OS-level restricted paths.
        let ro_dir = dir.path().join("ro");
        std::fs::create_dir(&ro_dir).unwrap();
        let kh_path = ro_dir.join("known_hosts");
        // Create the file then make it a symlink to itself would be
        // too convoluted; easier: make the parent read-only so
        // create() fails.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perm = std::fs::metadata(&ro_dir).unwrap().permissions();
            perm.set_mode(0o500);
            std::fs::set_permissions(&ro_dir, perm).unwrap();
        }

        let verifier = AcceptNewVerifier::new(kh_path);
        let (key_a, key_b) = test_public_key_pair();

        // First contact: unknown. Persistence fails silently, but the
        // connection is still accepted.
        let first = verifier.verify("host.example", 22, &key_a).await.unwrap();
        assert!(first, "first contact must be accepted (TOFU)");

        // Second contact with same key: must still be accepted via
        // the in-memory cache.
        let second = verifier.verify("host.example", 22, &key_a).await.unwrap();
        assert!(
            second,
            "repeat with same key must be accepted from memory cache"
        );

        // Third contact with a DIFFERENT key: must be rejected. This
        // is the whole point — a persistence failure must not silently
        // degrade to accept-any.
        let rejected = verifier.verify("host.example", 22, &key_b).await.unwrap();
        assert!(
            !rejected,
            "key change after persist-failure must still be rejected"
        );

        // Restore perms so tempdir cleanup can succeed.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perm = std::fs::metadata(&ro_dir).unwrap().permissions();
            perm.set_mode(0o700);
            let _ = std::fs::set_permissions(&ro_dir, perm);
        }
    }
}
