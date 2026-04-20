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

//! russh-backed SSH session manager for `view --ssh` (issue #194).
//!
//! The [`SshSession`] struct wraps a single long-lived SSH connection
//! to one target, owning the russh client [`client::Handle`] and
//! exposing [`SshSession::exec`] for one-shot command execution on a
//! new channel per call. Connections opened through [`SshSession::connect`]
//! keep a TCP keep-alive interval so idle sessions are pruned by the
//! remote sshd and do not stack up silently.
//!
//! Authentication precedence (from issue #194):
//! 1. `--ssh-key <path>` (explicit key)
//! 2. `SSH_AUTH_SOCK` (OpenSSH agent) — via `ssh-agent-client-rs` crate
//!    if present; see note below.
//! 3. `~/.ssh/id_ed25519`
//! 4. `~/.ssh/id_rsa`
//!
//! NOTE: SSH agent forwarding is not attempted in this initial cut;
//! russh 0.60 includes an `russh::client::Handle::agent_auth` but it
//! requires a running agent. Detection is deferred until we add agent
//! support in a follow-up. Password auth is intentionally unsupported.
//!
//! Host-key policy: the supplied [`HostKeyVerifier`] receives the
//! server's public key on every connect and returns `Ok(true)` /
//! `Ok(false)`. Three built-in verifiers are shipped, one per policy
//! (see [`StrictHostKey`]).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use russh::client::{self, Handle};
use russh::keys::{PrivateKeyWithHashAlg, load_secret_key, ssh_key};
use russh::{ChannelMsg, Disconnect};

use crate::network::ssh_target::SshTarget;
use crate::network::ssh_transport::StrictHostKey;

/// Hard ceiling on the size of a single command's stdout we are
/// willing to buffer. Prevents a misbehaving remote from forcing the
/// view process into OOM. The nvidia-smi CSV path returns hundreds
/// of bytes per GPU; 16 MiB is a generous ceiling.
pub const MAX_COMMAND_STDOUT_BYTES: usize = 16 * 1024 * 1024;

/// Hard ceiling on command stderr (same rationale).
pub const MAX_COMMAND_STDERR_BYTES: usize = 1024 * 1024;

/// Per-call command execution timeout. Individual commands MUST honour
/// this or risk holding the connection open forever on a hung remote.
pub const DEFAULT_EXEC_TIMEOUT: Duration = Duration::from_secs(15);

/// russh's inactivity timeout — the client session drops if no data is
/// exchanged for this long. Set conservatively so keep-alive pings keep
/// long-idle view tabs alive while a real dead connection is still
/// detected in reasonable time.
pub const DEFAULT_INACTIVITY: Duration = Duration::from_secs(60);

/// Errors emitted by [`SshSession::connect`] / [`SshSession::exec`].
#[derive(Debug, thiserror::Error)]
pub enum SshClientError {
    #[error("SSH I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("SSH protocol error: {0}")]
    Protocol(String),
    #[error("SSH authentication failed for {user}@{host}:{port}")]
    AuthFailed {
        user: String,
        host: String,
        port: u16,
    },
    #[error("SSH host-key rejected for {host}:{port}: {reason}")]
    HostKeyRejected {
        host: String,
        port: u16,
        reason: String,
    },
    #[error("SSH connect timeout after {0:?}")]
    ConnectTimeout(Duration),
    #[error("SSH exec timeout after {0:?}")]
    ExecTimeout(Duration),
    #[error("no usable SSH private key found (tried: {0})")]
    NoUsableKey(String),
    #[error("SSH command output exceeded {limit} bytes")]
    OutputTooLarge { limit: usize },
}

impl SshClientError {
    /// Short human label used by the TUI "connection state" chip.
    pub fn ui_label(&self) -> &'static str {
        match self {
            Self::AuthFailed { .. } => "auth-failed",
            Self::HostKeyRejected { .. } => "host-key-rejected",
            Self::ConnectTimeout(_) => "timeout",
            Self::ExecTimeout(_) => "timeout",
            Self::NoUsableKey(_) => "no-key",
            Self::OutputTooLarge { .. } => "disconnected",
            Self::Io(_) | Self::Protocol(_) => "disconnected",
        }
    }
}

/// Trait implemented by the three host-key verification policies.
#[async_trait]
pub trait HostKeyVerifier: Send + Sync + 'static {
    /// Called once with the server's public key at handshake time.
    ///
    /// Returns `Ok(true)` to accept, `Ok(false)` to reject. The `host`
    /// / `port` arguments identify the target so the `accept-new`
    /// implementation can key its known_hosts entry correctly.
    async fn verify(
        &self,
        host: &str,
        port: u16,
        key: &ssh_key::PublicKey,
    ) -> Result<bool, SshClientError>;
}

/// Convenience holder passed to [`SshSession::connect`].
///
/// `strict_host_key` and `known_hosts` are carried through the params
/// struct so higher layers can inspect / log them, even though the
/// actual verification decision is made inside the pre-constructed
/// [`HostKeyVerifier`]. `#[allow(dead_code)]` on those fields keeps
/// clippy happy while preserving the public surface for future wiring
/// (e.g. diagnostics logging in `view --doctor`).
pub struct ConnectParams {
    pub target: SshTarget,
    pub explicit_key: Option<PathBuf>,
    #[allow(dead_code)]
    pub strict_host_key: StrictHostKey,
    pub connect_timeout: Duration,
    pub inactivity: Duration,
    #[allow(dead_code)]
    pub known_hosts: Option<PathBuf>,
}

impl Default for ConnectParams {
    fn default() -> Self {
        Self {
            target: SshTarget {
                user: String::new(),
                host: String::new(),
                port: 22,
            },
            explicit_key: None,
            strict_host_key: StrictHostKey::Yes,
            connect_timeout: Duration::from_secs(10),
            inactivity: DEFAULT_INACTIVITY,
            known_hosts: None,
        }
    }
}

/// A long-lived SSH session against one remote. Cheap to clone because
/// the underlying russh [`Handle`] is internally `Arc`-shared and
/// thread-safe — no external lock is required. Multiple concurrent
/// [`SshSession::exec`] calls on the same session multiplex onto
/// independent SSH channels via russh's built-in channel machinery.
#[derive(Clone)]
pub struct SshSession {
    inner: Arc<Handle<RusshClient>>,
    target: SshTarget,
}

/// russh client handler. Every incoming message is dispatched through
/// this struct; we only care about host-key verification.
struct RusshClient {
    verifier: Arc<dyn HostKeyVerifier>,
    host: String,
    port: u16,
}

impl client::Handler for RusshClient {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &ssh_key::PublicKey,
    ) -> Result<bool, Self::Error> {
        match self
            .verifier
            .verify(&self.host, self.port, server_public_key)
            .await
        {
            Ok(accept) => Ok(accept),
            Err(e) => {
                tracing::warn!(
                    host = %self.host,
                    port = self.port,
                    error = %e,
                    "host-key verifier returned an error; rejecting connection"
                );
                Ok(false)
            }
        }
    }
}

impl SshSession {
    /// Open a new SSH session and authenticate.
    pub async fn connect(
        params: ConnectParams,
        verifier: Arc<dyn HostKeyVerifier>,
    ) -> Result<Self, SshClientError> {
        let target = params.target.clone();
        let host = target.host.clone();
        let port = target.port;

        // Resolve the key to use. We prefer the explicit path; else
        // probe the conventional defaults in ~/.ssh.
        let key_path = resolve_key_path(params.explicit_key.as_deref())
            .ok_or_else(|| SshClientError::NoUsableKey(default_key_probe_paths()))?;

        let key_pair = load_secret_key(&key_path, None).map_err(|e| {
            SshClientError::Protocol(format!(
                "failed to load private key {}: {e}",
                key_path.display()
            ))
        })?;

        let config = Arc::new(client::Config {
            inactivity_timeout: Some(params.inactivity),
            keepalive_interval: Some(Duration::from_secs(30)),
            keepalive_max: 3,
            ..Default::default()
        });

        let handler = RusshClient {
            verifier,
            host: host.clone(),
            port,
        };

        let connect_fut = client::connect(config, (host.as_str(), port), handler);
        let mut session = match tokio::time::timeout(params.connect_timeout, connect_fut).await {
            Err(_) => return Err(SshClientError::ConnectTimeout(params.connect_timeout)),
            Ok(Err(e)) => return Err(map_russh_error(e, &host, port)),
            Ok(Ok(session)) => session,
        };

        // Attempt publickey auth. russh's `authenticate_publickey` takes
        // the key + the best-supported RSA hash; we simply pass what
        // the session negotiated. If the server outright refuses the
        // handshake because of the host key, that surfaces as an I/O
        // error on the await below and is mapped by `map_russh_error`.
        let best_hash = session
            .best_supported_rsa_hash()
            .await
            .map_err(|e| map_russh_error(e, &host, port))?
            .flatten();
        let auth_res = session
            .authenticate_publickey(
                target.user.clone(),
                PrivateKeyWithHashAlg::new(Arc::new(key_pair), best_hash),
            )
            .await
            .map_err(|e| map_russh_error(e, &host, port))?;

        if !auth_res.success() {
            return Err(SshClientError::AuthFailed {
                user: target.user.clone(),
                host: host.clone(),
                port,
            });
        }

        Ok(Self {
            inner: Arc::new(session),
            target,
        })
    }

    /// Execute a command on a fresh channel and collect stdout / exit
    /// status. Bounded by [`DEFAULT_EXEC_TIMEOUT`].
    pub async fn exec(&self, command: &str) -> Result<ExecOutput, SshClientError> {
        self.exec_with_timeout(command, DEFAULT_EXEC_TIMEOUT).await
    }

    pub async fn exec_with_timeout(
        &self,
        command: &str,
        timeout: Duration,
    ) -> Result<ExecOutput, SshClientError> {
        let run = async {
            // russh's `client::Handle` is itself internally Arc-shared
            // and Send + Sync; channel_open_session takes `&self` and
            // is safe to call concurrently. No external lock needed.
            let mut channel = self
                .inner
                .channel_open_session()
                .await
                .map_err(|e| map_russh_error(e, &self.target.host, self.target.port))?;
            channel
                .exec(true, command)
                .await
                .map_err(|e| map_russh_error(e, &self.target.host, self.target.port))?;

            let mut stdout: Vec<u8> = Vec::new();
            let mut stderr: Vec<u8> = Vec::new();
            let mut exit_code: Option<u32> = None;

            while let Some(msg) = channel.wait().await {
                match msg {
                    ChannelMsg::Data { ref data } => {
                        if stdout.len() + data.len() > MAX_COMMAND_STDOUT_BYTES {
                            return Err(SshClientError::OutputTooLarge {
                                limit: MAX_COMMAND_STDOUT_BYTES,
                            });
                        }
                        stdout.extend_from_slice(data);
                    }
                    ChannelMsg::ExtendedData { ref data, ext: 1 } => {
                        if stderr.len() + data.len() > MAX_COMMAND_STDERR_BYTES {
                            return Err(SshClientError::OutputTooLarge {
                                limit: MAX_COMMAND_STDERR_BYTES,
                            });
                        }
                        stderr.extend_from_slice(data);
                    }
                    ChannelMsg::ExitStatus { exit_status } => {
                        exit_code = Some(exit_status);
                    }
                    ChannelMsg::Eof | ChannelMsg::Close => {
                        // Keep looping — additional messages may still
                        // arrive (ExitStatus can follow Eof).
                    }
                    _ => {}
                }
            }

            Ok(ExecOutput {
                stdout: bytes_to_string_zero_copy(stdout),
                stderr: bytes_to_string_zero_copy(stderr),
                exit_status: exit_code,
            })
        };

        match tokio::time::timeout(timeout, run).await {
            Err(_) => Err(SshClientError::ExecTimeout(timeout)),
            Ok(result) => result,
        }
    }

    /// Tear down the session cleanly. Best-effort: errors are logged
    /// but swallowed because the connection is likely already gone by
    /// the time a caller reaches this code path.
    #[allow(dead_code)]
    pub async fn close(&self) {
        if let Err(e) = self
            .inner
            .disconnect(Disconnect::ByApplication, "view --ssh shutting down", "en")
            .await
        {
            tracing::debug!(
                host = %self.target.host,
                port = self.target.port,
                error = %e,
                "ssh disconnect returned an error"
            );
        }
    }

    /// Accessor for the [`SshTarget`] this session was opened against.
    #[allow(dead_code)]
    pub fn target(&self) -> &SshTarget {
        &self.target
    }
}

/// Captured output of a single `exec` call.
#[derive(Debug, Clone)]
pub struct ExecOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_status: Option<u32>,
}

impl ExecOutput {
    /// True when `exit_status == Some(0)`.
    pub fn is_success(&self) -> bool {
        self.exit_status == Some(0)
    }
}

/// Convert a byte buffer into a `String` without copying when the bytes
/// are already valid UTF-8 (the common case for SSH stdout from
/// `nvidia-smi` and friends). Only the rare pre-invalid-byte prefix
/// triggers the lossy re-encode, and only for that non-UTF-8 tail.
fn bytes_to_string_zero_copy(bytes: Vec<u8>) -> String {
    match String::from_utf8(bytes) {
        Ok(s) => s,
        Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
    }
}

/// Map a russh error into our user-facing [`SshClientError`].
fn map_russh_error(err: russh::Error, host: &str, port: u16) -> SshClientError {
    // russh::Error is an enum; convert the classes we explicitly care
    // about (auth, host-key rejection) into typed variants and fall
    // through to Protocol otherwise.
    use russh::Error as E;
    match err {
        E::IO(e) => SshClientError::Io(e),
        E::NotAuthenticated => SshClientError::AuthFailed {
            user: String::new(),
            host: host.to_string(),
            port,
        },
        E::UnknownKey => SshClientError::HostKeyRejected {
            host: host.to_string(),
            port,
            reason: "host key not trusted".to_string(),
        },
        other => SshClientError::Protocol(format!("{other:?}")),
    }
}

fn resolve_key_path(explicit: Option<&Path>) -> Option<PathBuf> {
    if let Some(p) = explicit {
        let expanded = crate::common::paths::expand_tilde(p);
        if expanded.exists() {
            return Some(expanded);
        }
        // Explicit paths must exist; returning None here causes
        // [`NoUsableKey`] to surface.
        return None;
    }
    key_probe_paths()
        .into_iter()
        .find(|candidate| candidate.exists())
}

fn key_probe_paths() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(home) = dirs::home_dir() {
        out.push(home.join(".ssh").join("id_ed25519"));
        out.push(home.join(".ssh").join("id_ecdsa"));
        out.push(home.join(".ssh").join("id_rsa"));
    }
    out
}

fn default_key_probe_paths() -> String {
    key_probe_paths()
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

// ---------------------------------------------------------------------
// Built-in host-key verifiers — see [`crate::network::ssh_host_key`].
// ---------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_ui_labels_are_stable() {
        assert_eq!(
            SshClientError::AuthFailed {
                user: "u".to_string(),
                host: "h".to_string(),
                port: 22,
            }
            .ui_label(),
            "auth-failed"
        );
        assert_eq!(
            SshClientError::ConnectTimeout(Duration::from_secs(1)).ui_label(),
            "timeout"
        );
        assert_eq!(
            SshClientError::HostKeyRejected {
                host: "h".to_string(),
                port: 22,
                reason: "x".to_string()
            }
            .ui_label(),
            "host-key-rejected"
        );
    }

    #[test]
    fn key_probe_paths_contains_home_ssh_defaults() {
        // Only holds when a home directory is detectable. If the test
        // environment is weird (no HOME) the function returns an empty
        // vector, which is correct behaviour.
        if dirs::home_dir().is_some() {
            let paths = key_probe_paths();
            assert!(
                paths.iter().any(|p| p.ends_with("id_ed25519")),
                "expected id_ed25519 in probe list, got {paths:?}"
            );
            assert!(paths.iter().any(|p| p.ends_with("id_rsa")));
        }
    }
}
