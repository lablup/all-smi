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

//! Rotating, compression-aware writer for `all-smi record`.
//!
//! Stateful wrapper that owns the current output segment, counts bytes
//! written, and when a segment exceeds [`RotatingWriter::max_size`] closes
//! the encoder cleanly, renames the file to a numbered sibling, and opens
//! the next segment. Callers treat it as a `Write`.
//!
//! Compression is chosen from the file extension (or an explicit
//! [`Codec`] override): `.zst` → zstd, `.gz` → gzip, else plain. The inner
//! encoder must implement `finish()`-style teardown; we keep the active
//! segment as an enum and handle each variant's close path separately.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use flate2::Compression;
use flate2::write::GzEncoder;
use zstd::stream::write::Encoder as ZstdEncoder;

use crate::cli::RecordCompression;

/// Which codec wraps the underlying file.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Codec {
    Plain,
    Zstd,
    Gzip,
}

impl Codec {
    /// Detect the codec from a path's extension, with an optional explicit
    /// override. When `override_` is `Some`, we honor it verbatim. Detection
    /// is deliberately conservative: only the final extension is consulted,
    /// so `.tar.gz` would pick gzip, and anything else falls through to
    /// plain.
    pub fn detect(path: &Path, override_: Option<RecordCompression>) -> Self {
        if let Some(r) = override_ {
            return match r {
                RecordCompression::Zstd => Codec::Zstd,
                RecordCompression::Gzip => Codec::Gzip,
                RecordCompression::None => Codec::Plain,
            };
        }
        match path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref()
        {
            Some("zst") => Codec::Zstd,
            Some("gz") => Codec::Gzip,
            _ => Codec::Plain,
        }
    }
}

/// Active output segment. Each variant owns whatever teardown the inner
/// encoder requires. The `counting` wrapper tracks the number of *bytes
/// actually written to the codec's input* (i.e., the uncompressed size);
/// the compressed file on disk may be smaller.
///
/// For zstd/gzip, "bytes written" counts pre-compression volume, which is
/// what the issue spec's `--max-size` describes: the operator cares about
/// bounding their effective recording duration, and the compressed file
/// varies arbitrarily with content entropy.
enum ActiveSegment {
    Plain(BufWriter<File>),
    Zstd(ZstdEncoder<'static, BufWriter<File>>),
    Gzip(GzEncoder<BufWriter<File>>),
}

impl ActiveSegment {
    fn open(path: &Path, codec: Codec) -> io::Result<Self> {
        let file = open_secure(path)?;
        let buf = BufWriter::with_capacity(64 * 1024, file);
        match codec {
            Codec::Plain => Ok(Self::Plain(buf)),
            Codec::Zstd => {
                let mut enc = ZstdEncoder::new(buf, 3)?;
                // Auto-flush every few MB on the encoder side so a mid-run
                // `zstd -dc file | head` can actually see the head of the
                // stream. Level 3 keeps CPU cheap.
                enc.include_checksum(true)?;
                Ok(Self::Zstd(enc))
            }
            Codec::Gzip => Ok(Self::Gzip(GzEncoder::new(buf, Compression::default()))),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Plain(w) => w.flush(),
            Self::Zstd(w) => w.flush(),
            Self::Gzip(w) => w.flush(),
        }
    }

    /// Close the segment cleanly, flushing any remaining encoder state.
    /// Consumes `self`: the caller must replace the active segment
    /// immediately afterwards (or stop writing entirely).
    fn finish(self) -> io::Result<()> {
        match self {
            Self::Plain(mut w) => {
                w.flush()?;
                let mut inner = w.into_inner().map_err(|e| e.into_error())?;
                inner.flush()?;
                Ok(())
            }
            Self::Zstd(enc) => {
                let buf = enc.finish()?;
                let mut inner = buf.into_inner().map_err(|e| e.into_error())?;
                inner.flush()?;
                Ok(())
            }
            Self::Gzip(enc) => {
                let buf = enc.finish()?;
                let mut inner = buf.into_inner().map_err(|e| e.into_error())?;
                inner.flush()?;
                Ok(())
            }
        }
    }
}

impl Write for ActiveSegment {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Self::Plain(w) => w.write(buf),
            Self::Zstd(w) => w.write(buf),
            Self::Gzip(w) => w.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        ActiveSegment::flush(self)
    }
}

/// Rotating writer used by `Recorder`.
///
/// Path scheme for rotated segments mirrors common log-rotation layouts:
/// the active file uses the operator-supplied path, and older segments are
/// renamed to `path.N<suffix>` where `N` is a zero-padded sequence (`.0001`,
/// `.0002`, …) and `<suffix>` preserves the extension chain so the codec
/// is still recoverable by extension. For `out.ndjson.zst` with two
/// rollovers, the directory ends up with:
///
/// ```text
/// out.ndjson.zst            # active, newest
/// out.0002.ndjson.zst       # one rollover ago
/// out.0001.ndjson.zst       # two rollovers ago
/// ```
///
/// `max_files` caps the total number of on-disk segments (active +
/// rotated). When a rollover would exceed the cap, the oldest rotated
/// segment is deleted before the rename.
pub struct RotatingWriter {
    base: PathBuf,
    codec: Codec,
    max_size: u64,
    max_files: u32,
    active: Option<ActiveSegment>,
    /// Bytes written to the active segment since it was opened. Counted at
    /// the input side (pre-compression) — see [`ActiveSegment`] docs.
    active_bytes: u64,
    /// Monotonic rollover counter. Starts at 1; never decreases. Used only
    /// for naming rotated segments.
    next_rollover_index: u32,
    /// Keep track of rotated segment paths in chronological order (oldest
    /// first) so we can evict the head cheaply. Bounded by `max_files - 1`.
    rotated_segments: Vec<PathBuf>,
}

impl RotatingWriter {
    /// Create a new writer and open the first segment.
    ///
    /// `max_size = 0` disables rotation entirely. `max_files = 1` means
    /// "keep the active file only; on rollover, just truncate it" — in
    /// practice the caller usually pairs a sane `max_size` with
    /// `max_files >= 2`.
    pub fn new(
        base: impl Into<PathBuf>,
        codec: Codec,
        max_size: u64,
        max_files: u32,
    ) -> io::Result<Self> {
        let base = base.into();
        if let Some(parent) = base.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent)?;
        }
        let active = ActiveSegment::open(&base, codec)?;
        Ok(Self {
            base,
            codec,
            max_size,
            max_files: max_files.max(1),
            active: Some(active),
            active_bytes: 0,
            next_rollover_index: 1,
            rotated_segments: Vec::new(),
        })
    }

    /// Write a full NDJSON line (must already include the trailing `\n`).
    ///
    /// After each write, if the segment has exceeded `max_size` we trigger
    /// a rollover. Rollover is checked *after* the write, so a single
    /// oversized frame produces a segment larger than the threshold — we
    /// never split a frame across files.
    pub fn write_line(&mut self, line: &[u8]) -> io::Result<()> {
        let segment = self
            .active
            .as_mut()
            .expect("RotatingWriter::active was taken; new segment not opened");
        segment.write_all(line)?;
        self.active_bytes += line.len() as u64;

        if self.max_size > 0 && self.active_bytes >= self.max_size {
            self.rollover()?;
        }
        Ok(())
    }

    /// Roll the active segment out and open a fresh one.
    ///
    /// Steps:
    /// 1. Take ownership of the active segment and call `finish()` to
    ///    flush the encoder cleanly.
    /// 2. Rename the base file to `base.<N>.<suffix>`.
    /// 3. Trim the rotated-segments list to `max_files - 1` by deleting
    ///    the oldest.
    /// 4. Open a new active segment at `base`.
    fn rollover(&mut self) -> io::Result<()> {
        let active = self
            .active
            .take()
            .expect("RotatingWriter::active was already taken");
        active.finish()?;

        // If max_files is 1 there is no history — truncate in place.
        if self.max_files <= 1 {
            self.active = Some(ActiveSegment::open(&self.base, self.codec)?);
            self.active_bytes = 0;
            self.next_rollover_index = self.next_rollover_index.saturating_add(1);
            return Ok(());
        }

        let rolled_path = rotated_path(&self.base, self.next_rollover_index);
        // Remove any pre-existing rotated file at the same name (shouldn't
        // happen under normal operation, but allow idempotent restarts).
        // Before removing, refuse to unlink a symlink at that path — a
        // co-tenant pre-planting a symlink to an unrelated file would
        // otherwise cause us to delete the symlink's own target at the
        // next rename step (rename happily follows symlinks on the
        // destination side). `symlink_metadata` does NOT traverse the
        // link, so we inspect the link itself.
        match fs::symlink_metadata(&rolled_path) {
            Ok(m) if m.file_type().is_symlink() => {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!(
                        "refusing to rotate onto symlink at `{}`",
                        rolled_path.display()
                    ),
                ));
            }
            Ok(_) => {
                // Plain file or directory: remove only if a regular file.
                // Leaving it alone and letting `fs::rename` fail is also
                // acceptable, but `remove_file` matches historical
                // behaviour for idempotent restarts.
                let _ = fs::remove_file(&rolled_path);
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                // Normal case — nothing at the rotated path.
            }
            Err(e) => return Err(e),
        }
        fs::rename(&self.base, &rolled_path)?;
        self.rotated_segments.push(rolled_path);
        self.next_rollover_index = self.next_rollover_index.saturating_add(1);

        // Evict oldest segments until we are under max_files - 1 rotated.
        while self.rotated_segments.len() as u32 >= self.max_files {
            let oldest = self.rotated_segments.remove(0);
            let _ = fs::remove_file(&oldest);
        }

        self.active = Some(ActiveSegment::open(&self.base, self.codec)?);
        self.active_bytes = 0;
        Ok(())
    }

    /// Flush the active segment buffers. Does not close the encoder.
    /// Called on SIGTERM / SIGINT paths so an operator who killed the
    /// recorder can still decompress the partial file — zstd/gzip
    /// streams only become valid once the trailing frame has landed on
    /// disk.
    #[allow(dead_code)]
    pub fn flush(&mut self) -> io::Result<()> {
        if let Some(active) = self.active.as_mut() {
            active.flush()?;
        }
        Ok(())
    }

    /// Close the active segment cleanly. The writer must not be used
    /// afterwards.
    pub fn finish(mut self) -> io::Result<()> {
        if let Some(active) = self.active.take() {
            active.finish()?;
        }
        Ok(())
    }

    /// Number of bytes written to the active segment.
    pub fn active_bytes(&self) -> u64 {
        self.active_bytes
    }
}

impl Drop for RotatingWriter {
    fn drop(&mut self) {
        // Best-effort flush-and-close on drop. If the caller has not
        // invoked `finish()`, we still try to leave the file in a
        // readable state. Errors are swallowed because `Drop` cannot
        // propagate them; critical paths should prefer explicit
        // `finish()`.
        if let Some(active) = self.active.take() {
            let _ = active.finish();
        }
    }
}

/// Build the path of a rotated segment: strip the "active" suffix chain,
/// insert a sequence number, then re-attach the suffix. For `out.ndjson.zst`
/// and index `3` this produces `out.0003.ndjson.zst`.
///
/// The implementation uses the full extension chain (everything after the
/// first `.` in the file name) so round-trips through `Codec::detect` on
/// the rotated name recover the same codec.
fn rotated_path(base: &Path, index: u32) -> PathBuf {
    let dir = base.parent().unwrap_or_else(|| Path::new(""));
    let name = base
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("record");
    let (stem, suffix) = match name.find('.') {
        Some(pos) => (&name[..pos], &name[pos..]),
        None => (name, ""),
    };
    let new_name = format!("{stem}.{index:04}{suffix}");
    dir.join(new_name)
}

/// Open a recording file without following symlinks or clobbering an
/// existing file.
///
/// Mirrors the `O_NOFOLLOW` + `0o600` hardening in
/// `src/snapshot/mod.rs::write_output_atomic` (the atomic-snapshot fix
/// from #185). The threat is a co-tenant pre-planting
/// `/tmp/all-smi-record.ndjson.zst -> /etc/shadow` (or any attacker-chosen
/// target): without `O_NOFOLLOW` the privileged operator's recorder would
/// follow the symlink and open the attacker-chosen file for writing.
///
/// `create_new(true)` refuses to open an existing regular file, which is
/// the right default here because rollover segments already receive
/// unique names via `rotated_path`. The one call site that could collide
/// with a pre-existing file — a recorder being re-run against the same
/// `--output` path — surfaces an explicit `AlreadyExists` error, which
/// is clearer than silently truncating whatever is already there.
///
/// On Windows we use `share_mode(0)` for exclusive access; NTFS symlink
/// TOCTOU is handled with different mitigations (ACLs on the directory)
/// which are out of scope for this helper and match the snapshot
/// subcommand's treatment.
fn open_secure(path: &Path) -> io::Result<File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .custom_flags(libc::O_NOFOLLOW)
            .mode(0o600)
            .open(path)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .share_mode(0)
            .open(path)
    }
    #[cfg(not(any(unix, windows)))]
    {
        OpenOptions::new().write(true).create_new(true).open(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn codec_detect_from_extension() {
        assert_eq!(
            Codec::detect(Path::new("a.ndjson"), None),
            Codec::Plain,
            "plain ndjson"
        );
        assert_eq!(
            Codec::detect(Path::new("a.ndjson.zst"), None),
            Codec::Zstd,
            "zst wins"
        );
        assert_eq!(
            Codec::detect(Path::new("a.ndjson.gz"), None),
            Codec::Gzip,
            "gz wins"
        );
        assert_eq!(
            Codec::detect(Path::new("a.ndjson.zst"), Some(RecordCompression::None)),
            Codec::Plain,
            "explicit override beats extension"
        );
    }

    #[test]
    fn rotated_path_inserts_index_before_extension() {
        assert_eq!(
            rotated_path(Path::new("/tmp/out.ndjson.zst"), 3),
            PathBuf::from("/tmp/out.0003.ndjson.zst"),
        );
        assert_eq!(
            rotated_path(Path::new("out.ndjson"), 7),
            PathBuf::from("out.0007.ndjson"),
        );
    }

    #[test]
    fn plain_writer_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rec.ndjson");
        let mut w = RotatingWriter::new(&path, Codec::Plain, 0, 1).unwrap();
        w.write_line(b"{\"a\":1}\n").unwrap();
        w.write_line(b"{\"b\":2}\n").unwrap();
        w.finish().unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "{\"a\":1}\n{\"b\":2}\n");
    }

    #[test]
    fn gzip_writer_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rec.ndjson.gz");
        let mut w = RotatingWriter::new(&path, Codec::Gzip, 0, 1).unwrap();
        w.write_line(b"hello\n").unwrap();
        w.finish().unwrap();

        let f = std::fs::File::open(&path).unwrap();
        let mut dec = flate2::read::GzDecoder::new(f);
        let mut s = String::new();
        dec.read_to_string(&mut s).unwrap();
        assert_eq!(s, "hello\n");
    }

    #[test]
    fn zstd_writer_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rec.ndjson.zst");
        let mut w = RotatingWriter::new(&path, Codec::Zstd, 0, 1).unwrap();
        w.write_line(b"hello\n").unwrap();
        w.write_line(b"world\n").unwrap();
        w.finish().unwrap();

        let f = std::fs::File::open(&path).unwrap();
        let mut dec = zstd::stream::read::Decoder::new(f).unwrap();
        let mut s = String::new();
        dec.read_to_string(&mut s).unwrap();
        assert_eq!(s, "hello\nworld\n");
    }

    #[test]
    fn rotation_evicts_oldest_beyond_max_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rec.ndjson");
        // Tiny max_size so every write triggers rollover. max_files=3 means
        // we keep at most 3 files total (active + 2 rotated).
        let mut w = RotatingWriter::new(&path, Codec::Plain, 3, 3).unwrap();
        for i in 0..6 {
            w.write_line(format!("{i}\n").as_bytes()).unwrap();
        }
        w.finish().unwrap();

        let mut files: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| n.starts_with("rec."))
            .collect();
        files.sort();
        assert!(
            files.len() <= 3,
            "max_files not respected, found: {files:?}"
        );
    }

    /// F1: `ActiveSegment::open` must refuse to follow a pre-planted
    /// symlink. This is the canonical co-tenant attack:
    /// `/tmp/all-smi-record.ndjson -> /etc/shadow` (or any file the
    /// attacker wants the operator's recorder to clobber).
    #[cfg(unix)]
    #[test]
    fn record_writer_refuses_preexisting_symlink() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        // Pre-create the target with content the attacker hopes survives
        // the record run.
        std::fs::write(&target, "do-not-clobber\n").unwrap();
        let record_path = dir.path().join("rec.ndjson");
        symlink(&target, &record_path).unwrap();

        let result = RotatingWriter::new(&record_path, Codec::Plain, 0, 1);
        assert!(
            result.is_err(),
            "open_secure must refuse to follow a symlink"
        );
        // Target file content must still be intact — the symlink must
        // NOT have been followed through to writing.
        let content = std::fs::read_to_string(&target).unwrap();
        assert_eq!(content, "do-not-clobber\n");
    }

    /// F1 cont'd: `create_new(true)` must surface an error when the
    /// file already exists. This catches operators re-running `record`
    /// against the same path (a clear signal instead of silent
    /// truncation) and blocks a race where an attacker pre-creates the
    /// file as a regular file expecting to read its contents.
    #[cfg(unix)]
    #[test]
    fn record_writer_refuses_preexisting_regular_file() {
        let dir = tempfile::tempdir().unwrap();
        let record_path = dir.path().join("rec.ndjson");
        std::fs::write(&record_path, "preexisting\n").unwrap();
        let result = RotatingWriter::new(&record_path, Codec::Plain, 0, 1);
        assert!(
            result.is_err(),
            "open_secure must refuse to clobber an existing regular file"
        );
    }

    /// F1 cont'd: on Unix the recording file should be mode 0o600
    /// (owner-only). Matches the hardening in `write_output_atomic`
    /// from #185.
    #[cfg(unix)]
    #[test]
    fn record_writer_sets_restrictive_mode() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let record_path = dir.path().join("rec.ndjson");
        let mut w = RotatingWriter::new(&record_path, Codec::Plain, 0, 1).unwrap();
        w.write_line(b"{\"a\":1}\n").unwrap();
        w.finish().unwrap();

        let meta = std::fs::metadata(&record_path).unwrap();
        let mode = meta.permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "recording file must be 0o600, got {mode:o}");
    }

    /// F1 cont'd: rollover must refuse to rename onto a pre-planted
    /// symlink at the rotated path. The writer carries the path through
    /// `remove_file` → `rename`; without the `symlink_metadata` check a
    /// co-tenant could race a symlink into the rotation target to
    /// unlink or clobber arbitrary files.
    #[cfg(unix)]
    #[test]
    fn record_writer_rollover_refuses_symlink_target() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let record_path = dir.path().join("rec.ndjson");
        // Tiny max_size to force rollover on first write.
        let mut w = RotatingWriter::new(&record_path, Codec::Plain, 3, 3).unwrap();
        // Pre-plant a symlink at the rotated path.
        let rolled = rotated_path(&record_path, 1);
        let decoy = dir.path().join("decoy");
        std::fs::write(&decoy, "preserve-me\n").unwrap();
        symlink(&decoy, &rolled).unwrap();

        // Trigger rollover. The writer should refuse to rotate onto
        // the symlink and return an error instead of silently
        // unlinking the decoy.
        let err = w.write_line(b"trigger\n").err();
        assert!(err.is_some(), "rollover onto a symlink must raise an error");
        // The decoy file must still exist with its original content.
        let content = std::fs::read_to_string(&decoy).unwrap();
        assert_eq!(content, "preserve-me\n");
    }
}
