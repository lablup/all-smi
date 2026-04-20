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

//! Append-only WAL for the energy accountant (issue #191).
//!
//! Each record persists the Joule delta accumulated for a single
//! `(host, device)` pair since the last flush. On startup the WAL is
//! replayed to seed the process-wide Prometheus counter so
//! `all_smi_energy_consumed_joules_total` stays monotonic across
//! restarts.
//!
//! # Record format
//!
//! Every record is 24 bytes, little-endian:
//!
//! | offset | field          | type |
//! |--------|----------------|------|
//! | 0      | `host_hash`    | u64  |
//! | 8      | `device_hash`  | u64  |
//! | 16     | `joules_delta` | f64  |
//!
//! The issue body specifies "16-byte record" but lists three 8-byte
//! fields; the actual record width is therefore 24 bytes. The narrower
//! number was a typo.
//!
//! # Crash safety
//!
//! Records are independent. A partial tail (program or power killed
//! mid-write) is detected at replay time by the length check and
//! silently dropped — no record ever overrides the value of another.
//! The writer `fsync`s after each flush batch.
//!
//! # Path hardening
//!
//! The WAL file lives in the user's cache directory (default
//! `~/.cache/all-smi/energy-wal.bin`). On Unix it is opened with
//! `O_NOFOLLOW` and `0o600`, matching the hardening applied by
//! `src/snapshot/mod.rs` and `src/record/writer.rs` (issue #185). On
//! Windows we use `share_mode(0)`.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::energy::{EnergyKey, PowerIntegrator};

/// On-disk record width in bytes.
pub const RECORD_LEN: usize = 24;

/// Default flush cadence (60 s) as specified by the issue body.
pub const DEFAULT_FLUSH_INTERVAL: Duration = Duration::from_secs(60);

/// Expand a leading `~` in `path` to the user's home directory.
///
/// Returns `path` unchanged if it does not start with `~`, or if
/// `$HOME` is unset.
pub fn expand_tilde(path: &Path) -> PathBuf {
    let s = match path.to_str() {
        Some(s) => s,
        None => return path.to_path_buf(),
    };
    if let Some(rest) = s.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home).join(rest);
        }
    } else if s == "~"
        && let Ok(home) = std::env::var("HOME")
    {
        return PathBuf::from(home);
    }
    path.to_path_buf()
}

/// Replay the WAL at `path`, if it exists.
///
/// Returns a [`WalReplayIndex`] that maps each `(host_hash,
/// device_hash)` pair to the accumulated Joule total from the file.
/// The integrator is not touched directly — the caller uses
/// [`WalReplayIndex::seed_if_matches`] each time a new sample arrives
/// to migrate the replay value into the integrator under the correct
/// label set once the live labels are known.
///
/// A truncated final record (file size not a multiple of
/// [`RECORD_LEN`]) is silently dropped. Missing files are not errors —
/// the caller is expected to start from scratch.
///
/// The `_integrator` parameter is accepted and ignored for forward
/// compatibility with a future in-place seeding mode; passing the
/// live integrator today lets callers keep the API stable.
pub fn replay_from_path(
    path: &Path,
    _integrator: &mut PowerIntegrator,
) -> io::Result<WalReplayIndex> {
    let path = expand_tilde(path);
    let mut f = match File::open(&path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(WalReplayIndex::default()),
        Err(e) => return Err(e),
    };
    let size = f.metadata()?.len() as usize;
    let usable = size - (size % RECORD_LEN);
    let record_count = usable / RECORD_LEN;

    let mut index = WalReplayIndex::default();
    let mut buf = [0u8; RECORD_LEN];
    for _ in 0..record_count {
        if let Err(e) = f.read_exact(&mut buf) {
            // Short read right at EOF counts as a torn final record
            // and is dropped per the issue spec.
            if e.kind() == io::ErrorKind::UnexpectedEof {
                break;
            }
            return Err(e);
        }
        let host_hash = u64::from_le_bytes(buf[0..8].try_into().unwrap());
        let device_hash = u64::from_le_bytes(buf[8..16].try_into().unwrap());
        let joules = f64::from_le_bytes(buf[16..24].try_into().unwrap());
        if !joules.is_finite() || joules <= 0.0 {
            // Corrupted / non-positive payload — silently drop to stay
            // consistent with the "each record independent" contract.
            continue;
        }
        index.accumulate(host_hash, device_hash, joules);
    }

    Ok(index)
}

/// Hash-keyed map produced by [`replay_from_path`] and consumed by
/// [`WalWriter::resolve_hashes`] once the live label set is known.
#[derive(Clone, Debug, Default)]
pub struct WalReplayIndex {
    entries: std::collections::HashMap<(u64, u64), f64>,
}

impl WalReplayIndex {
    /// Add `joules` to the existing entry for `(host_hash, device_hash)`.
    fn accumulate(&mut self, host_hash: u64, device_hash: u64, joules: f64) {
        *self.entries.entry((host_hash, device_hash)).or_insert(0.0) += joules;
    }

    /// Returns the replayed Joule total for a given `(host_hash,
    /// device_hash)` pair, or `None` if the WAL did not mention it.
    #[allow(dead_code)] // Exercised by the integration tests.
    pub fn lookup(&self, host_hash: u64, device_hash: u64) -> Option<f64> {
        self.entries.get(&(host_hash, device_hash)).copied()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// If `key` matches a replayed `(host_hash, device_hash)` pair,
    /// seed the integrator's lifetime counter for that key with the
    /// WAL's accumulated Joules and remove the matched entry so a
    /// later call with the same key does not double-seed.
    ///
    /// Returns the number of Joules seeded (0.0 if no match).
    pub fn seed_if_matches(&mut self, key: &EnergyKey, integrator: &mut PowerIntegrator) -> f64 {
        let hash_pair = (key.host_hash(), key.device_hash());
        if let Some(joules) = self.entries.remove(&hash_pair) {
            integrator.seed_lifetime(key.clone(), joules);
            return joules;
        }
        0.0
    }
}

/// Append-only writer for the energy WAL.
#[derive(Debug)]
pub struct WalWriter {
    #[allow(dead_code)] // Kept for callers that want to display the resolved path.
    path: PathBuf,
    writer: Option<BufWriter<File>>,
}

impl WalWriter {
    /// Open the WAL file at `path`, creating the parent directory if
    /// necessary. Existing records are preserved (the writer appends
    /// at the end of the file).
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = expand_tilde(path.as_ref());
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent)?;
        }
        let file = open_secure_append(&path)?;
        Ok(Self {
            path,
            writer: Some(BufWriter::new(file)),
        })
    }

    /// Append a single record.
    pub fn write_record(
        &mut self,
        host_hash: u64,
        device_hash: u64,
        joules: f64,
    ) -> io::Result<()> {
        if !joules.is_finite() || joules <= 0.0 {
            return Ok(());
        }
        let writer = self
            .writer
            .as_mut()
            .ok_or_else(|| io::Error::other("WAL writer already closed"))?;
        let mut buf = [0u8; RECORD_LEN];
        buf[0..8].copy_from_slice(&host_hash.to_le_bytes());
        buf[8..16].copy_from_slice(&device_hash.to_le_bytes());
        buf[16..24].copy_from_slice(&joules.to_le_bytes());
        writer.write_all(&buf)
    }

    /// Flush buffered writes to disk and `fsync` the underlying file.
    ///
    /// A crash before `flush` returns may leave a torn final record on
    /// disk; the replay logic is written to tolerate that.
    pub fn flush_and_fsync(&mut self) -> io::Result<()> {
        let writer = match self.writer.as_mut() {
            Some(w) => w,
            None => return Ok(()),
        };
        writer.flush()?;
        writer.get_ref().sync_data()?;
        Ok(())
    }

    /// Return the resolved on-disk path (with `~` expanded).
    #[allow(dead_code)] // Helper surface for future diagnostics.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for WalWriter {
    fn drop(&mut self) {
        if let Some(mut w) = self.writer.take() {
            let _ = w.flush();
        }
    }
}

/// Convenience: spawn a background tokio task that flushes
/// `integrator.drain_wal_deltas()` to `path` every 60s and on
/// shutdown.
///
/// `shared_state` exposes the integrator to the task; we clone the
/// handle rather than sharing a `&mut PowerIntegrator` across threads.
/// Errors opening the WAL file are logged and the task exits — the
/// in-memory counter continues to work, we just lose cross-restart
/// persistence.
#[cfg(feature = "cli")]
pub fn spawn_wal_flush_task(
    shared_state: std::sync::Arc<tokio::sync::RwLock<crate::app_state::AppState>>,
    wal_path: String,
    flush_interval: std::time::Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut writer = match WalWriter::open(&wal_path) {
            Ok(w) => w,
            Err(e) => {
                tracing::warn!(
                    "energy WAL: failed to open {wal_path} ({e}); counters are in-memory only"
                );
                return;
            }
        };
        let mut ticker = tokio::time::interval(flush_interval);
        // Skip the immediate firing; the first flush happens after
        // `flush_interval` so we never write before any samples have
        // been integrated.
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        ticker.tick().await;
        loop {
            ticker.tick().await;
            let deltas = {
                let mut state = shared_state.write().await;
                state.energy.integrator_mut().drain_wal_deltas()
            };
            for (key, joules) in deltas {
                if let Err(e) = writer.write_record(key.host_hash(), key.device_hash(), joules) {
                    tracing::warn!("energy WAL: write failed: {e}");
                }
            }
            if let Err(e) = writer.flush_and_fsync() {
                tracing::warn!("energy WAL: fsync failed: {e}");
            }
        }
    })
}

/// Secure-append file handle.
///
/// Mirrors the `O_NOFOLLOW` + `0o600` hardening already applied by
/// [`crate::record::writer`] and [`crate::snapshot`]. We allow the file
/// to exist (this is the whole point of the WAL — it accumulates across
/// invocations) but refuse to follow a symlink at the WAL path.
fn open_secure_append(path: &Path) -> io::Result<File> {
    // Match the treatment in other hardened writers: if the target
    // path is already a symlink, refuse to open. `symlink_metadata`
    // does NOT traverse the link; a pre-planted
    // `/home/user/.cache/all-smi/energy-wal.bin -> /etc/shadow` would
    // be detected here and refused before we ever call `open`.
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "refusing to open energy WAL at {} — path is a symlink",
                    path.display()
                ),
            ));
        }
        _ => {}
    }

    let mut file = {
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            OpenOptions::new()
                .create(true)
                .append(true)
                .read(true)
                .custom_flags(libc::O_NOFOLLOW)
                .mode(0o600)
                .open(path)?
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            OpenOptions::new()
                .create(true)
                .append(true)
                .read(true)
                .share_mode(0)
                .open(path)?
        }
        #[cfg(not(any(unix, windows)))]
        {
            OpenOptions::new()
                .create(true)
                .append(true)
                .read(true)
                .open(path)?
        }
    };

    // Seek to end of file in case the append flag was not enough on
    // some platforms (tests, tmpfs, certain filesystems).
    file.seek(SeekFrom::End(0))?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn write_then_replay_round_trips() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("energy-wal.bin");

        {
            let mut writer = WalWriter::open(&path).unwrap();
            writer.write_record(1, 2, 100.0).unwrap();
            writer.write_record(1, 2, 50.0).unwrap();
            writer.write_record(3, 4, 200.0).unwrap();
            writer.flush_and_fsync().unwrap();
        }

        let mut integ = PowerIntegrator::default();
        let index = replay_from_path(&path, &mut integ).unwrap();

        assert_eq!(index.lookup(1, 2), Some(150.0));
        assert_eq!(index.lookup(3, 4), Some(200.0));
        assert_eq!(index.len(), 2);
    }

    #[test]
    fn seed_if_matches_migrates_replay_into_live_key() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("energy-wal.bin");

        let live_key = EnergyKey::gpu("host-a", "uuid-0");
        let host_hash = live_key.host_hash();
        let device_hash = live_key.device_hash();

        {
            let mut writer = WalWriter::open(&path).unwrap();
            writer
                .write_record(host_hash, device_hash, 5_000.0)
                .unwrap();
            writer.flush_and_fsync().unwrap();
        }

        let mut integ = PowerIntegrator::default();
        let mut index = replay_from_path(&path, &mut integ).unwrap();
        assert_eq!(index.len(), 1);
        assert_eq!(integ.lifetime_joules(&live_key), 0.0);

        // First-sample seeding populates the integrator and shrinks
        // the index.
        let seeded = index.seed_if_matches(&live_key, &mut integ);
        assert_eq!(seeded, 5_000.0);
        assert_eq!(integ.lifetime_joules(&live_key), 5_000.0);
        assert_eq!(index.len(), 0);

        // A second call is a no-op — the match was consumed.
        let seeded2 = index.seed_if_matches(&live_key, &mut integ);
        assert_eq!(seeded2, 0.0);
        assert_eq!(integ.lifetime_joules(&live_key), 5_000.0);
    }

    #[test]
    fn missing_wal_returns_empty_index() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("does-not-exist.bin");
        let mut integ = PowerIntegrator::default();
        let index = replay_from_path(&path, &mut integ).unwrap();
        assert!(index.is_empty());
    }

    #[test]
    fn torn_final_record_is_discarded() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("energy-wal.bin");

        {
            let mut writer = WalWriter::open(&path).unwrap();
            writer.write_record(1, 2, 100.0).unwrap();
            writer.write_record(3, 4, 200.0).unwrap();
            writer.flush_and_fsync().unwrap();
        }

        // Truncate mid-record to simulate a crash: leave the first
        // record intact (24 bytes) plus 12 bytes of a second record.
        let metadata = fs::metadata(&path).unwrap();
        assert_eq!(metadata.len(), (RECORD_LEN * 2) as u64);

        let truncated = (RECORD_LEN + 12) as u64;
        let f = OpenOptions::new().write(true).open(&path).unwrap();
        f.set_len(truncated).unwrap();

        let mut integ = PowerIntegrator::default();
        let index = replay_from_path(&path, &mut integ).unwrap();
        assert_eq!(index.len(), 1);
        assert_eq!(index.lookup(1, 2), Some(100.0));
        assert_eq!(index.lookup(3, 4), None);
    }

    #[cfg(unix)]
    #[test]
    fn wal_file_is_mode_0o600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempdir().unwrap();
        let path = dir.path().join("energy-wal.bin");
        {
            let mut writer = WalWriter::open(&path).unwrap();
            writer.write_record(1, 2, 10.0).unwrap();
            writer.flush_and_fsync().unwrap();
        }
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "WAL file must be 0o600, got {mode:o}");
    }

    #[cfg(unix)]
    #[test]
    fn wal_refuses_symlink_path() {
        use std::os::unix::fs::symlink;
        let dir = tempdir().unwrap();
        let target = dir.path().join("actual-target");
        let link = dir.path().join("energy-wal.bin");
        fs::write(&target, b"existing").unwrap();
        symlink(&target, &link).unwrap();

        let err = WalWriter::open(&link).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn non_positive_records_are_ignored_on_write_and_replay() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("energy-wal.bin");

        {
            let mut writer = WalWriter::open(&path).unwrap();
            writer.write_record(1, 2, 100.0).unwrap();
            writer.write_record(1, 2, 0.0).unwrap();
            writer.write_record(1, 2, f64::NAN).unwrap();
            writer.write_record(1, 2, -5.0).unwrap();
            writer.flush_and_fsync().unwrap();
        }

        let mut integ = PowerIntegrator::default();
        let index = replay_from_path(&path, &mut integ).unwrap();
        assert_eq!(index.lookup(1, 2), Some(100.0));
    }

    #[test]
    fn expand_tilde_replaces_home_prefix() {
        // Rust 2024 flags env mutations as unsafe because they can
        // race with concurrent reads. We accept the risk in this
        // single-threaded unit test and isolate by explicitly
        // restoring the HOME variable at the end.
        let original = std::env::var("HOME").ok();
        unsafe {
            std::env::set_var("HOME", "/tmp/fake-home");
        }
        let expanded = expand_tilde(Path::new("~/.cache/all-smi/energy-wal.bin"));
        assert_eq!(
            expanded,
            PathBuf::from("/tmp/fake-home/.cache/all-smi/energy-wal.bin")
        );
        let unchanged = expand_tilde(Path::new("/absolute/path"));
        assert_eq!(unchanged, PathBuf::from("/absolute/path"));
        unsafe {
            match original {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
    }
}
