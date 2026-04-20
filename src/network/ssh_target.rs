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

//! Parsed SSH target: `user@host[:port]` and hostfile loader (issue #194).
//!
//! Kept distinct from the russh-backed client so tests can lock in the
//! parsing rules without pulling the entire SSH stack into scope.

use std::fs;
use std::path::Path;

/// Default SSH port applied when the operator omits an explicit `:port`
/// suffix in `--ssh user@host` or the hostfile.
pub const DEFAULT_SSH_PORT: u16 = 22;

/// Hard ceiling on the file size we are willing to read as an
/// `--ssh-hostfile`. Matches the RemoteCollectorBuilder convention so
/// `view` mode has a single memory-safety story.
pub const MAX_HOSTFILE_BYTES: u64 = 10 * 1024 * 1024;

/// Hard ceiling on the number of SSH targets we accept from a single
/// hostfile.
pub const MAX_HOSTFILE_ENTRIES: usize = 1000;

/// Errors emitted by [`parse_ssh_target`] and [`parse_hostfile`].
#[derive(Debug, thiserror::Error)]
pub enum SshTargetError {
    #[error("invalid SSH target `{0}`: missing `user@`")]
    MissingUser(String),
    #[error("invalid SSH target `{0}`: missing host after `user@`")]
    MissingHost(String),
    #[error("invalid SSH target `{input}`: port `{port}` is not a valid u16")]
    InvalidPort { input: String, port: String },
    #[error("hostfile I/O error at {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("hostfile {path} exceeds {limit}-byte limit (found {actual} bytes)")]
    FileTooLarge {
        path: String,
        actual: u64,
        limit: u64,
    },
}

/// A single remote SSH target parsed from the CLI or hostfile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshTarget {
    pub user: String,
    pub host: String,
    pub port: u16,
}

impl SshTarget {
    /// Stable identifier used as the host key across the TUI and
    /// connection-status map. Always `user@host:port`, even when the
    /// caller omitted `:port` in the original input (the struct has
    /// already normalised to [`DEFAULT_SSH_PORT`]).
    pub fn host_id(&self) -> String {
        format!("{}@{}:{}", self.user, self.host, self.port)
    }

    /// Display label for the tab row — `ssh://user@host` without the
    /// port suffix when it is the default 22, for compactness.
    pub fn display_label(&self) -> String {
        if self.port == DEFAULT_SSH_PORT {
            format!("ssh://{}@{}", self.user, self.host)
        } else {
            format!("ssh://{}@{}:{}", self.user, self.host, self.port)
        }
    }

    /// Hostname-only string used as the `hostname` field on produced
    /// [`crate::device::GpuInfo`] records. Strips the user and port so
    /// per-GPU labels stay readable.
    pub fn hostname(&self) -> &str {
        &self.host
    }
}

/// Parse a single `user@host[:port]` string into [`SshTarget`].
///
/// IPv6 literals wrapped in `[]` are accepted, matching OpenSSH's
/// syntax (`user@[::1]:2222`).
pub fn parse_ssh_target(raw: &str) -> Result<SshTarget, SshTargetError> {
    let input = raw.trim();
    let at = input
        .find('@')
        .ok_or_else(|| SshTargetError::MissingUser(input.to_string()))?;
    let user = input[..at].to_string();
    if user.is_empty() {
        return Err(SshTargetError::MissingUser(input.to_string()));
    }
    let rest = &input[at + 1..];
    if rest.is_empty() {
        return Err(SshTargetError::MissingHost(input.to_string()));
    }

    // IPv6-in-brackets: `[::1]:2222`
    if let Some(stripped) = rest.strip_prefix('[') {
        let close = stripped
            .find(']')
            .ok_or_else(|| SshTargetError::MissingHost(input.to_string()))?;
        let host = stripped[..close].to_string();
        let tail = &stripped[close + 1..];
        let port = parse_port_suffix(tail, input)?;
        return Ok(SshTarget { user, host, port });
    }

    // Plain host[:port]. Find the LAST colon to support hostnames that
    // legitimately contain no colons (typical case) while still rejecting
    // IPv6 literals written without brackets.
    if let Some((host_part, port_part)) = rest.rsplit_once(':') {
        // Disallow unbracketed IPv6 (multiple colons on the remote side).
        if host_part.contains(':') {
            return Err(SshTargetError::MissingHost(input.to_string()));
        }
        let port = port_part
            .parse::<u16>()
            .map_err(|_| SshTargetError::InvalidPort {
                input: input.to_string(),
                port: port_part.to_string(),
            })?;
        if host_part.is_empty() {
            return Err(SshTargetError::MissingHost(input.to_string()));
        }
        Ok(SshTarget {
            user,
            host: host_part.to_string(),
            port,
        })
    } else {
        Ok(SshTarget {
            user,
            host: rest.to_string(),
            port: DEFAULT_SSH_PORT,
        })
    }
}

fn parse_port_suffix(tail: &str, input: &str) -> Result<u16, SshTargetError> {
    if tail.is_empty() {
        return Ok(DEFAULT_SSH_PORT);
    }
    let tail = tail.strip_prefix(':').unwrap_or(tail);
    tail.parse::<u16>()
        .map_err(|_| SshTargetError::InvalidPort {
            input: input.to_string(),
            port: tail.to_string(),
        })
}

/// Parse a comma-separated `--ssh` argument into multiple targets.
pub fn parse_ssh_arg(arg: &str) -> Result<Vec<SshTarget>, SshTargetError> {
    arg.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(parse_ssh_target)
        .collect()
}

/// Load and parse an SSH hostfile.
///
/// Format: one `user@host[:port]` per line. Blank lines and `#`-prefixed
/// comments are ignored. Inline `#` comments are also stripped. The
/// file size is capped at [`MAX_HOSTFILE_BYTES`]; the number of
/// parsed entries is capped at [`MAX_HOSTFILE_ENTRIES`].
pub fn parse_hostfile(path: impl AsRef<Path>) -> Result<Vec<SshTarget>, SshTargetError> {
    let path = path.as_ref();
    let path_str = path.display().to_string();

    let metadata = fs::metadata(path).map_err(|source| SshTargetError::Io {
        path: path_str.clone(),
        source,
    })?;
    if metadata.len() > MAX_HOSTFILE_BYTES {
        return Err(SshTargetError::FileTooLarge {
            path: path_str,
            actual: metadata.len(),
            limit: MAX_HOSTFILE_BYTES,
        });
    }

    let content = fs::read_to_string(path).map_err(|source| SshTargetError::Io {
        path: path_str,
        source,
    })?;
    parse_hostfile_content(&content)
}

pub fn parse_hostfile_content(content: &str) -> Result<Vec<SshTarget>, SshTargetError> {
    let mut out = Vec::new();
    for raw in content.lines() {
        // Strip inline `#` comments. Never strip inside a quoted
        // string — the hostfile format does not support quotes, so a
        // `#` anywhere on the line starts a comment.
        let line = match raw.find('#') {
            Some(idx) => &raw[..idx],
            None => raw,
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let target = parse_ssh_target(trimmed)?;
        out.push(target);
        if out.len() >= MAX_HOSTFILE_ENTRIES {
            break;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_user_at_host() {
        let t = parse_ssh_target("admin@dgx-01").unwrap();
        assert_eq!(t.user, "admin");
        assert_eq!(t.host, "dgx-01");
        assert_eq!(t.port, DEFAULT_SSH_PORT);
        assert_eq!(t.host_id(), "admin@dgx-01:22");
        assert_eq!(t.display_label(), "ssh://admin@dgx-01");
    }

    #[test]
    fn parses_user_at_host_with_port() {
        let t = parse_ssh_target("admin@dgx-01:2222").unwrap();
        assert_eq!(t.port, 2222);
        assert_eq!(t.display_label(), "ssh://admin@dgx-01:2222");
    }

    #[test]
    fn parses_ipv6_with_brackets_and_port() {
        let t = parse_ssh_target("admin@[fd00::1]:2222").unwrap();
        assert_eq!(t.host, "fd00::1");
        assert_eq!(t.port, 2222);
    }

    #[test]
    fn parses_ipv6_with_brackets_default_port() {
        let t = parse_ssh_target("admin@[::1]").unwrap();
        assert_eq!(t.host, "::1");
        assert_eq!(t.port, DEFAULT_SSH_PORT);
    }

    #[test]
    fn rejects_missing_user() {
        let e = parse_ssh_target("dgx-01").unwrap_err();
        matches!(e, SshTargetError::MissingUser(_));
    }

    #[test]
    fn rejects_empty_user() {
        let e = parse_ssh_target("@dgx-01").unwrap_err();
        matches!(e, SshTargetError::MissingUser(_));
    }

    #[test]
    fn rejects_missing_host() {
        let e = parse_ssh_target("user@").unwrap_err();
        matches!(e, SshTargetError::MissingHost(_));
    }

    #[test]
    fn rejects_bad_port() {
        let e = parse_ssh_target("user@host:not-a-number").unwrap_err();
        matches!(e, SshTargetError::InvalidPort { .. });
    }

    #[test]
    fn rejects_unbracketed_ipv6() {
        // `user@fd00::1` is ambiguous (is `1` a port or a hostname
        // fragment?). OpenSSH rejects this form; so do we.
        let e = parse_ssh_target("user@fd00::1").unwrap_err();
        matches!(
            e,
            SshTargetError::MissingHost(_) | SshTargetError::InvalidPort { .. }
        );
    }

    #[test]
    fn parse_ssh_arg_splits_comma_list() {
        let t = parse_ssh_arg("a@h1,b@h2:2222,c@h3").unwrap();
        assert_eq!(t.len(), 3);
        assert_eq!(t[0].user, "a");
        assert_eq!(t[1].port, 2222);
        assert_eq!(t[2].host, "h3");
    }

    #[test]
    fn parse_ssh_arg_tolerates_whitespace_and_empty_slots() {
        let t = parse_ssh_arg(" a@h1 , , b@h2 ").unwrap();
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].user, "a");
        assert_eq!(t[1].user, "b");
    }

    #[test]
    fn hostfile_ignores_comments_and_blanks() {
        let content = "# a comment\n\nuser1@host1\n   \nuser2@host2:2222 # inline comment\n";
        let t = parse_hostfile_content(content).unwrap();
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].user, "user1");
        assert_eq!(t[1].port, 2222);
    }

    #[test]
    fn hostfile_propagates_parse_errors() {
        let content = "this-is-not-valid\n";
        assert!(parse_hostfile_content(content).is_err());
    }

    #[test]
    fn hostfile_enforces_entry_cap() {
        // Synthesize a large hostfile; parser must stop at
        // MAX_HOSTFILE_ENTRIES without blowing out memory.
        let mut content = String::new();
        for i in 0..(MAX_HOSTFILE_ENTRIES + 100) {
            content.push_str(&format!("u@host{i}\n"));
        }
        let t = parse_hostfile_content(&content).unwrap();
        assert_eq!(t.len(), MAX_HOSTFILE_ENTRIES);
    }

    #[test]
    fn reads_real_hostfile_from_disk() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "alice@a\nbob@b:2200\n").unwrap();
        let t = parse_hostfile(tmp.path()).unwrap();
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].user, "alice");
        assert_eq!(t[1].port, 2200);
    }
}
