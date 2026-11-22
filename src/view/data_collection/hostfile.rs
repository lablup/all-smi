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

//! Hostfile loading shared by the remote view and `record --source=remote`.
//!
//! One parser and one set of bounds serve the startup load, the per-tick
//! reload in the remote view, and the recorder (issue #500). The reload goes
//! through [`HostfileWatcher`], which costs one async `metadata` call per
//! tick and re-reads the file only when its modification time or length
//! changed.

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::common::http_hosts::{http_host_identifier, parse_http_host_url};

/// Largest hostfile accepted, in bytes.
pub(crate) const MAX_HOSTFILE_BYTES: u64 = 10 * 1024 * 1024;
/// Most host entries taken from one hostfile; later entries are dropped.
pub(crate) const MAX_HOSTFILE_HOSTS: usize = 1000;

/// Hosts parsed from a hostfile plus the warnings parsing produced, so each
/// caller can report them on the channel it owns (stderr before the TUI
/// starts, `tracing` while it runs).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct HostfileContents {
    pub(crate) hosts: Vec<String>,
    pub(crate) warnings: Vec<String>,
}

/// Expand a leading `~/` and canonicalize, mapping failures to the error
/// text the startup loader has always used.
fn resolve_hostfile_path(path: &Path) -> io::Result<PathBuf> {
    // `canonicalize` never performs tilde expansion itself, so a config
    // value like `hostfile = "~/.config/all-smi/hosts"` must be expanded
    // first or it fails with `NotFound`.
    let expanded = crate::common::paths::expand_tilde(path);
    expanded.canonicalize().map_err(|e| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("Invalid hostfile path: {e}"),
        )
    })
}

fn check_hostfile_metadata(metadata: &std::fs::Metadata) -> io::Result<()> {
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Hostfile path is not a regular file",
        ));
    }
    if metadata.len() > MAX_HOSTFILE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "Hostfile too large: {} bytes (max: {MAX_HOSTFILE_BYTES} bytes)",
                metadata.len()
            ),
        ));
    }
    Ok(())
}

fn too_large_error(max: u64) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("Hostfile too large: over {max} bytes (max: {max} bytes)"),
    )
}

fn decode_bounded(bytes: Vec<u8>, max: u64) -> io::Result<String> {
    if bytes.len() as u64 > max {
        return Err(too_large_error(max));
    }
    String::from_utf8(bytes).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "stream did not contain valid UTF-8",
        )
    })
}

/// Read at most `max` bytes of the file. The metadata check can be stale
/// (the file may grow or be swapped before the read), so the read itself is
/// bounded with `take(max + 1)` and rejects when it hits the extra byte.
fn read_bounded(path: &Path, max: u64) -> io::Result<String> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(max.saturating_add(1))
        .read_to_end(&mut bytes)?;
    decode_bounded(bytes, max)
}

/// Async counterpart of [`read_bounded`].
async fn read_bounded_async(path: &Path, max: u64) -> io::Result<String> {
    use tokio::io::AsyncReadExt;
    let mut bytes = Vec::new();
    tokio::fs::File::open(path)
        .await?
        .take(max.saturating_add(1))
        .read_to_end(&mut bytes)
        .await?;
    decode_bounded(bytes, max)
}

/// Parse hostfile text: one endpoint per line, blank lines and `#` comments
/// skipped, every entry validated with the same URL contract as `--hosts`,
/// and at most [`MAX_HOSTFILE_HOSTS`] entries kept.
pub(crate) fn parse_hostfile_content(content: &str) -> HostfileContents {
    let mut out = HostfileContents::default();
    let mut truncated = false;
    for entry in content
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty() && !s.starts_with('#'))
    {
        if out.hosts.len() >= MAX_HOSTFILE_HOSTS {
            truncated = true;
            break;
        }
        match parse_http_host_url(entry) {
            Ok(_) => out.hosts.push(entry.to_string()),
            Err(error) => out
                .warnings
                .push(format!("{error}; skipping hostfile entry")),
        }
    }
    if truncated {
        out.warnings.push(format!(
            "Hostfile contains more than {MAX_HOSTFILE_HOSTS} hosts, truncating"
        ));
    }
    out
}

/// Read a hostfile: tilde expansion, canonicalization, regular-file and
/// size checks, then [`parse_hostfile_content`]. Shared by the remote view's
/// startup load and `record --source=remote`; the view's per-tick reload
/// goes through [`HostfileWatcher`] with the same checks and parser.
pub(crate) fn read_hostfile(path: &Path) -> io::Result<HostfileContents> {
    let canonical = resolve_hostfile_path(path)?;
    check_hostfile_metadata(&std::fs::metadata(&canonical)?)?;
    let content = read_bounded(&canonical, MAX_HOSTFILE_BYTES)?;
    Ok(parse_hostfile_content(&content))
}

/// Merge `--hosts` with hostfile hosts, keeping the first occurrence of each
/// endpoint identity (`host:port`) and the order the operator wrote them in.
pub(crate) fn merge_hosts(cli_hosts: &[String], file_hosts: &[String]) -> Vec<String> {
    let mut seen: HashSet<String> = HashSet::with_capacity(cli_hosts.len() + file_hosts.len());
    cli_hosts
        .iter()
        .chain(file_hosts)
        .filter(|host| seen.insert(http_host_identifier(host)))
        .cloned()
        .collect()
}

/// Resolve the host list once: `--hosts` merged with the hostfile's hosts.
/// Used by `record --source=remote`, which does not reload the file.
pub(crate) fn resolve_hosts(
    cli_hosts: &[String],
    hostfile: Option<&str>,
) -> io::Result<HostfileContents> {
    let Some(file) = hostfile else {
        return Ok(HostfileContents {
            hosts: merge_hosts(cli_hosts, &[]),
            warnings: Vec::new(),
        });
    };
    let contents = read_hostfile(Path::new(file))?;
    Ok(HostfileContents {
        hosts: merge_hosts(cli_hosts, &contents.hosts),
        warnings: contents.warnings,
    })
}

/// `(modified, len)` pair used to decide whether the file changed.
type FileSignature = (Option<SystemTime>, u64);

fn signature(metadata: &std::fs::Metadata) -> FileSignature {
    (metadata.modified().ok(), metadata.len())
}

/// Per-tick hostfile reload for the remote view.
///
/// Keeps the canonical path and the last `(modified, len)` signature; each
/// [`refresh`](Self::refresh) costs one `tokio::fs::metadata` call and reads
/// the file only when the signature changed. A missing file or a read error
/// keeps the last good host list and is logged once per distinct error
/// through `tracing` (never stderr, which the TUI owns).
pub(crate) struct HostfileWatcher {
    path: PathBuf,
    signature: Option<FileSignature>,
    hosts: Vec<String>,
    last_error: Option<String>,
    reads: u64,
}

impl HostfileWatcher {
    /// Load the hostfile for the first time. Errors here are fatal for the
    /// caller, matching the startup behavior of `--hostfile`.
    pub(crate) fn open(path: &str) -> io::Result<(Self, Vec<String>)> {
        let canonical = resolve_hostfile_path(Path::new(path))?;
        let metadata = std::fs::metadata(&canonical)?;
        check_hostfile_metadata(&metadata)?;
        // Take the signature before reading so a write racing the read is
        // picked up by the next refresh instead of being missed.
        let sig = signature(&metadata);
        let content = read_bounded(&canonical, MAX_HOSTFILE_BYTES)?;
        let contents = parse_hostfile_content(&content);
        let watcher = Self {
            path: canonical,
            signature: Some(sig),
            hosts: contents.hosts,
            last_error: None,
            reads: 1,
        };
        Ok((watcher, contents.warnings))
    }

    /// Hosts from the last successful read.
    pub(crate) fn hosts(&self) -> &[String] {
        &self.hosts
    }

    /// Number of times the file content has been read (startup included).
    #[cfg(test)]
    pub(crate) fn read_count(&self) -> u64 {
        self.reads
    }

    fn note_error(&mut self, error: String) {
        if self.last_error.as_deref() != Some(error.as_str()) {
            tracing::warn!(
                hostfile = %self.path.display(),
                "hostfile reload failed, keeping the previous host list: {error}"
            );
            self.last_error = Some(error);
        }
    }

    /// Re-check the file and return the current host list.
    pub(crate) async fn refresh(&mut self) -> &[String] {
        let metadata = match tokio::fs::metadata(&self.path).await {
            Ok(metadata) => metadata,
            Err(e) => {
                self.note_error(format!("cannot access hostfile: {e}"));
                return &self.hosts;
            }
        };
        if let Err(e) = check_hostfile_metadata(&metadata) {
            self.note_error(e.to_string());
            return &self.hosts;
        }
        let sig = signature(&metadata);
        if self.signature == Some(sig) {
            return &self.hosts;
        }
        match read_bounded_async(&self.path, MAX_HOSTFILE_BYTES).await {
            Ok(content) => {
                self.reads += 1;
                tracing::debug!(reads = self.reads, "hostfile changed, reloaded");
                let contents = parse_hostfile_content(&content);
                for warning in &contents.warnings {
                    tracing::warn!(hostfile = %self.path.display(), "{warning}");
                }
                self.hosts = contents.hosts;
                self.signature = Some(sig);
                self.last_error = None;
            }
            Err(e) => self.note_error(format!("cannot read hostfile: {e}")),
        }
        &self.hosts
    }
}

#[cfg(test)]
#[path = "hostfile_tests.rs"]
mod tests;
