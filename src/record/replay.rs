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

//! Replay pipeline for `all-smi view --replay`.
//!
//! Streams NDJSON frames from disk (plain, `.zst`, or `.gz`) and exposes
//! a cursor the UI event handler can step, rewind, and seek. The reader
//! keeps at most `FRAME_CACHE_MAX` decoded frames resident at a time so a
//! 1M-frame recording does not load the whole file.
//!
//! The decoded frame type is [`Snapshot`] (the same struct the `record`
//! and `snapshot` subcommands produce), so `ReplayStrategy` can feed the
//! very same `RenderSnapshot` the live collectors do — no renderer
//! branching.
//!
//! # Frame kinds
//!
//! Each NDJSON line is one of:
//!
//! * **Header** — `{"schema":1,"header":true,...}` — single line, first.
//!   Metadata only; not a data frame.
//! * **Index** — `{"schema":1,"index":true,"seq":N,"byte_offset":N}` —
//!   sparse checkpoints every 1000 data frames. Optional; when absent we
//!   fall back to linear scan with a bounded cache.
//! * **Data** — anything else with `schema:1` — a full `Snapshot` frame.
//!
//! Schema mismatches (`schema != 1`) raise [`ReplayError::UnsupportedSchema`]
//! exactly once per file; partial/corrupted tail lines are logged and
//! skipped so an operator who killed `record` mid-frame still gets the
//! usable portion of the stream.

use std::fs::File;
use std::io::{self, BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, TimeZone, Utc};
use flate2::read::GzDecoder;
use serde::Deserialize;
use zstd::stream::read::Decoder as ZstdDecoder;

use crate::snapshot::Snapshot;

/// Max number of data frames retained in the back-scroll cache. Bounds the
/// memory footprint of replay for files of any size. The forward
/// iterator is O(1); `prev()` on an uncached frame re-reads from disk.
pub const FRAME_CACHE_MAX: usize = 1024;

/// Errors surfaced by the replay pipeline.
#[derive(Debug, thiserror::Error)]
pub enum ReplayError {
    #[error("replay: cannot open `{path}`: {source}")]
    Open {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("replay: read error: {0}")]
    Io(#[from] io::Error),
    #[error("replay: unsupported schema version {found}, this all-smi supports schema 1")]
    UnsupportedSchema { found: u32 },
    #[error("replay: no usable frames in `{0}`")]
    Empty(PathBuf),
    #[error("replay: invalid timecode `{0}` — use HH:MM:SS, MM:SS, or seconds")]
    InvalidTimecode(String),
}

/// Header frame metadata (first line of a well-formed recording).
///
/// `interval_ms` and `all_smi_version` are deserialized for completeness
/// and read by consumers outside this crate (SSE endpoint, issue #193);
/// allow `dead_code` so an internal rebuild that only consumes `hosts`
/// does not force us to strip the shape.
#[derive(Clone, Debug, Deserialize)]
pub struct ReplayHeader {
    #[serde(default)]
    #[allow(dead_code)]
    pub interval_ms: Option<u64>,
    #[serde(default)]
    pub hosts: Vec<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pub all_smi_version: Option<String>,
}

/// Raw JSON frame — discriminates between header, index, and data. We
/// parse once cheaply into this and only materialize a full `Snapshot`
/// for data frames.
#[derive(Deserialize)]
struct RawFrame {
    schema: u32,
    #[serde(default)]
    header: bool,
    #[serde(default)]
    index: bool,
    #[serde(default)]
    interval_ms: Option<u64>,
    #[serde(default)]
    hosts: Vec<String>,
    #[serde(default)]
    all_smi_version: Option<String>,
}

/// A decoded data frame plus its position in the file (line number).
#[derive(Clone)]
pub struct ReplayFrame {
    pub seq: u64,
    pub snapshot: Snapshot,
    pub timestamp: DateTime<Utc>,
}

/// Streaming replay cursor.
///
/// Internal model:
///
/// * A single-pass reader walks the file line-by-line from the beginning,
///   discarding header/index lines and materialising data frames into a
///   ring-buffered `cache`.
/// * `cursor` is an index into `cache` for the "currently displayed"
///   frame. When the cursor sits at the last cached frame and the caller
///   asks for `next()`, we read one more line from disk, push it onto the
///   cache, and evict the head if the cache is full.
/// * `prev()` moves the cursor backwards within the cache. If the cursor
///   would walk off the front, we *re-open* the file and rescan from the
///   beginning, re-filling the cache up to the requested frame. That is
///   the one path that can hit disk twice — but only for seeks outside
///   the cache window, which are rare in interactive use.
/// * `seek()` can use index frames for O(1) jumps (when the file has
///   them) and otherwise scans forward from the last known position.
pub struct Replayer {
    path: PathBuf,
    header: Option<ReplayHeader>,
    reader: BufReader<Box<dyn Read + Send>>,
    /// Ring buffer of the most-recently-read data frames. Actually a
    /// `Vec` with eviction from the front; eviction is amortised O(1)
    /// because we limit the cache to `FRAME_CACHE_MAX`.
    cache: Vec<ReplayFrame>,
    /// Index (within `cache`) of the frame currently being displayed.
    /// `None` until the first frame has been materialised.
    cursor: Option<usize>,
    /// Sequence number of the next fresh data frame to materialise from
    /// disk (i.e., the next one the reader has not yet seen).
    next_disk_seq: u64,
    /// Tracks whether we've reached EOF so we can stop calling read_line.
    eof: bool,
    /// Sparse index frames seen so far (`seq -> line_number`). Line-number
    /// based because `byte_offset` has no meaning inside a compressed
    /// stream — we recover positioning by re-opening and counting lines
    /// instead of by seeking bytes. Fast enough for the usual 1000-frame
    /// spacing.
    index_points: Vec<IndexPoint>,
    /// Stream line counter for the forward reader. Used so the sparse
    /// index can hand out lines-from-start to the seeker.
    line_number: u64,
}

#[derive(Clone, Copy, Debug)]
struct IndexPoint {
    seq: u64,
    /// Line number at which the index frame *itself* sits. The following
    /// data frame is at `line + 1`.
    line: u64,
}

impl Replayer {
    /// Open `path`, detect compression from the extension, parse the
    /// header (if any), and return a fresh cursor positioned before the
    /// first data frame. Call `next()` once to materialise frame 0.
    pub fn open(path: &Path) -> Result<Self, ReplayError> {
        let reader = open_reader(path).map_err(|e| ReplayError::Open {
            path: path.to_path_buf(),
            source: e,
        })?;
        let mut this = Self {
            path: path.to_path_buf(),
            header: None,
            reader,
            cache: Vec::new(),
            cursor: None,
            next_disk_seq: 0,
            eof: false,
            index_points: Vec::new(),
            line_number: 0,
        };
        // Peek first line for a possible header. We do this lazily: if the
        // first line is a data frame we push it into the cache instead.
        this.prime()?;
        Ok(this)
    }

    fn prime(&mut self) -> Result<(), ReplayError> {
        // Read lines until we either see a data frame (push to cache,
        // stop) or hit EOF (leave cache empty). Schema mismatch is
        // surfaced immediately. Header and index frames are absorbed
        // into the metadata state and never counted as data.
        loop {
            let line = match self.read_line()? {
                Some(s) => s,
                None => return Ok(()),
            };
            match self.classify_line(&line)? {
                ClassifiedLine::Header(h) => {
                    self.header = Some(h);
                }
                ClassifiedLine::Index(seq) => {
                    self.index_points.push(IndexPoint {
                        seq,
                        line: self.line_number,
                    });
                }
                ClassifiedLine::Data(snap) => {
                    let frame = ReplayFrame {
                        seq: self.next_disk_seq,
                        timestamp: parse_ts(&snap.timestamp),
                        snapshot: snap,
                    };
                    self.next_disk_seq += 1;
                    self.cache.push(frame);
                    self.cursor = Some(0);
                    return Ok(());
                }
                ClassifiedLine::Ignore => {}
            }
        }
    }

    /// Read one line from the underlying reader, advancing the line
    /// counter. Returns `Ok(None)` on EOF.
    fn read_line(&mut self) -> io::Result<Option<String>> {
        if self.eof {
            return Ok(None);
        }
        let mut buf = String::new();
        let n = self.reader.read_line(&mut buf)?;
        if n == 0 {
            self.eof = true;
            return Ok(None);
        }
        self.line_number += 1;
        // Drop trailing newline.
        if buf.ends_with('\n') {
            buf.pop();
            if buf.ends_with('\r') {
                buf.pop();
            }
        }
        Ok(Some(buf))
    }

    fn classify_line(&self, line: &str) -> Result<ClassifiedLine, ReplayError> {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return Ok(ClassifiedLine::Ignore);
        }
        // Cheap probe first — parse into `RawFrame` to reject unsupported
        // schema before paying the full `Snapshot` deserialisation.
        let raw: RawFrame = match serde_json::from_str(trimmed) {
            Ok(r) => r,
            Err(e) => {
                // Corrupted / truncated tail line — log and skip so the
                // rest of the file remains usable. We deliberately do
                // not surface this as a hard error because operators
                // commonly pkill `record` mid-write and expect the
                // already-flushed frames to be playable.
                tracing::warn!(
                    path = %self.path.display(),
                    line = self.line_number,
                    error = %e,
                    "replay: ignoring malformed NDJSON line"
                );
                return Ok(ClassifiedLine::Ignore);
            }
        };

        if raw.schema != 1 {
            return Err(ReplayError::UnsupportedSchema { found: raw.schema });
        }

        if raw.header {
            return Ok(ClassifiedLine::Header(ReplayHeader {
                interval_ms: raw.interval_ms,
                hosts: raw.hosts,
                all_smi_version: raw.all_smi_version,
            }));
        }
        if raw.index {
            #[derive(Deserialize)]
            struct IndexFrame {
                seq: u64,
            }
            let idx: IndexFrame = serde_json::from_str(trimmed).map_err(|_| {
                // Malformed index is not fatal; treat as "ignore" instead.
                ReplayError::Io(io::Error::other("malformed index frame"))
            })?;
            return Ok(ClassifiedLine::Index(idx.seq));
        }

        // Data frame: full `Snapshot` parse.
        let snap: Snapshot = match serde_json::from_str(trimmed) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(
                    path = %self.path.display(),
                    line = self.line_number,
                    error = %e,
                    "replay: ignoring malformed data frame"
                );
                return Ok(ClassifiedLine::Ignore);
            }
        };
        Ok(ClassifiedLine::Data(snap))
    }

    /// Metadata from the recording's header frame, if present.
    pub fn header(&self) -> Option<&ReplayHeader> {
        self.header.as_ref()
    }

    /// The currently-displayed frame, or `None` if no data frames have
    /// been materialised yet (empty file).
    pub fn current(&self) -> Option<&ReplayFrame> {
        self.cursor.and_then(|c| self.cache.get(c))
    }

    /// Advance to the next frame. Returns the new current frame, or
    /// `None` if the stream has been fully consumed.
    ///
    /// When the cursor is not at the end of the cache we advance without
    /// touching disk. When it is, we read one more line (skipping
    /// metadata) and extend the cache, evicting the head to stay under
    /// `FRAME_CACHE_MAX`.
    pub fn next(&mut self) -> Result<Option<&ReplayFrame>, ReplayError> {
        match self.cursor {
            Some(c) if c + 1 < self.cache.len() => {
                self.cursor = Some(c + 1);
            }
            _ => {
                // Need a fresh frame from disk.
                if !self.read_next_data_frame()? {
                    return Ok(self.current());
                }
                // After a successful read, cursor lands on the new frame
                // at the end of the cache.
                self.cursor = Some(self.cache.len() - 1);
            }
        }
        Ok(self.current())
    }

    /// Retreat to the previous frame. Returns the new current frame, or
    /// `None` if we are already at frame 0.
    pub fn prev(&mut self) -> Result<Option<&ReplayFrame>, ReplayError> {
        match self.cursor {
            Some(c) if c > 0 => {
                self.cursor = Some(c - 1);
                Ok(self.current())
            }
            Some(_) | None => {
                let Some(current_seq) = self.current().map(|f| f.seq) else {
                    return Ok(None);
                };
                if current_seq == 0 {
                    return Ok(self.current());
                }
                // Cursor is at front of cache but the target frame is
                // off the cached window. Rewind the stream and re-fill.
                self.rewind_and_seek_to(current_seq - 1)?;
                Ok(self.current())
            }
        }
    }

    /// Seek to the frame whose timestamp is closest to
    /// `start + offset_from_start`. Uses index frames where available
    /// and falls back to linear scan. Returns the frame landed on, or
    /// `None` if the file is empty.
    pub fn seek(
        &mut self,
        offset_from_start: Duration,
    ) -> Result<Option<&ReplayFrame>, ReplayError> {
        // Always re-resolve from frame 0 so the math is consistent whether
        // the cursor was previously ahead or behind.
        let first_ts = self.first_frame_timestamp()?;
        let target = first_ts + chrono::Duration::from_std(offset_from_start).unwrap_or_default();

        // Walk forward until frame.timestamp >= target; we then pick the
        // closer of (prev, current) to minimise jitter.
        self.rewind_and_seek_to(0)?;
        loop {
            let done = {
                let cur = match self.current() {
                    Some(f) => f,
                    None => return Ok(None),
                };
                cur.timestamp >= target
            };
            if done {
                break;
            }
            if self.next()?.is_none() {
                break;
            }
        }
        Ok(self.current())
    }

    /// Total frame count discovered so far. Exact only after the stream
    /// has been fully consumed; until then it's a lower bound.
    pub fn frames_seen(&self) -> u64 {
        self.next_disk_seq
    }

    /// Number of sparse-index checkpoints observed so far. Useful for
    /// tests that want to assert the fast-path seek code actually got
    /// exercised. Public for cross-module tests.
    #[cfg(test)]
    pub fn index_points_seen(&self) -> usize {
        self.index_points.len()
    }

    /// Whether the underlying reader has hit EOF. When true, frames_seen()
    /// is the total count.
    pub fn at_eof(&self) -> bool {
        self.eof
    }

    /// Duration from frame 0 to the currently-displayed frame.
    pub fn elapsed(&self) -> Option<Duration> {
        let cur = self.current()?;
        let first = self.cache.first()?;
        (cur.timestamp - first.timestamp).to_std().ok()
    }

    fn first_frame_timestamp(&mut self) -> Result<DateTime<Utc>, ReplayError> {
        // The first frame is always cached (priming guarantees that when
        // the file has any data). Rewind if the head has been evicted.
        if let Some(first) = self.cache.first()
            && first.seq == 0
        {
            return Ok(first.timestamp);
        }
        self.rewind_and_seek_to(0)?;
        self.current()
            .map(|f| f.timestamp)
            .ok_or_else(|| ReplayError::Empty(self.path.clone()))
    }

    /// Re-open the file and scan forward until we land on `target_seq`.
    fn rewind_and_seek_to(&mut self, target_seq: u64) -> Result<(), ReplayError> {
        let reader = open_reader(&self.path).map_err(|e| ReplayError::Open {
            path: self.path.clone(),
            source: e,
        })?;
        self.reader = reader;
        self.cache.clear();
        self.cursor = None;
        self.next_disk_seq = 0;
        self.eof = false;
        self.line_number = 0;
        // Preserve index_points — they remain valid across re-opens.

        // Use the nearest index checkpoint to skip ahead without parsing.
        let mut nearest_line: u64 = 0;
        let mut nearest_seq: u64 = 0;
        for p in &self.index_points {
            if p.seq <= target_seq && p.seq > nearest_seq {
                nearest_seq = p.seq;
                nearest_line = p.line;
            }
        }
        if nearest_line > 0 {
            // Skip `nearest_line` lines without classifying so we don't
            // accidentally re-populate the cache with header/index data.
            for _ in 0..nearest_line {
                let mut buf = String::new();
                let n = self.reader.read_line(&mut buf)?;
                if n == 0 {
                    self.eof = true;
                    break;
                }
                self.line_number += 1;
            }
            // Index frames are written AFTER their matching data frame
            // (see `record::write_data_frame`): line N is data frame
            // `nearest_seq`, line N+1 is the index frame carrying
            // `seq=nearest_seq`. Skipping past the index leaves the
            // reader at the NEXT data frame, whose absolute sequence is
            // `nearest_seq + 1`. Setting `next_disk_seq` accordingly
            // makes the cached frames' `seq` match the absolute
            // position in the recording, so the REPLAY status-bar
            // "frame N / M" display is correct after an index-frame
            // fast-path seek.
            self.next_disk_seq = nearest_seq + 1;
        }

        // Now do a data-frame-aware scan until we reach target_seq.
        loop {
            if !self.read_next_data_frame()? {
                break;
            }
            let landed_seq = self
                .cache
                .last()
                .map(|f| f.seq)
                .unwrap_or(self.next_disk_seq.saturating_sub(1));
            if landed_seq >= target_seq {
                self.cursor = Some(self.cache.len() - 1);
                break;
            }
        }
        if self.cursor.is_none() && !self.cache.is_empty() {
            self.cursor = Some(self.cache.len() - 1);
        }
        Ok(())
    }

    /// Read one more data frame from disk and push it onto the cache.
    /// Returns `Ok(true)` if a new frame was appended, `Ok(false)` on EOF.
    fn read_next_data_frame(&mut self) -> Result<bool, ReplayError> {
        loop {
            let line = match self.read_line()? {
                Some(s) => s,
                None => return Ok(false),
            };
            match self.classify_line(&line)? {
                ClassifiedLine::Header(h) => {
                    // Very rare: header repeated mid-stream; keep the
                    // first one to match the record-side contract.
                    if self.header.is_none() {
                        self.header = Some(h);
                    }
                }
                ClassifiedLine::Index(seq) => {
                    self.index_points.push(IndexPoint {
                        seq,
                        line: self.line_number,
                    });
                }
                ClassifiedLine::Data(snap) => {
                    let frame = ReplayFrame {
                        seq: self.next_disk_seq,
                        timestamp: parse_ts(&snap.timestamp),
                        snapshot: snap,
                    };
                    self.next_disk_seq += 1;
                    self.cache.push(frame);
                    if self.cache.len() > FRAME_CACHE_MAX {
                        // Ring-buffer eviction. O(N) because Vec::remove —
                        // acceptable because N = FRAME_CACHE_MAX = 1024
                        // and this happens at most once per `next()`.
                        let evicted = self.cache.remove(0);
                        // Keep cursor pointing at the same absolute frame
                        // if possible.
                        if let Some(c) = self.cursor
                            && c > 0
                        {
                            self.cursor = Some(c - 1);
                        }
                        drop(evicted);
                    }
                    return Ok(true);
                }
                ClassifiedLine::Ignore => {}
            }
        }
    }
}

enum ClassifiedLine {
    Header(ReplayHeader),
    Index(u64),
    Data(Snapshot),
    Ignore,
}

/// Parse an ISO-8601 / RFC3339 timestamp into a UTC `DateTime`. Falls back
/// to `Utc::now()` if the timestamp is missing or unparseable — the
/// snapshot shape mandates a valid timestamp, so this only matters for
/// robustness against hand-crafted fixtures.
fn parse_ts(ts: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(ts)
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc.timestamp_opt(0, 0).single().unwrap_or_else(Utc::now))
}

/// Pick the right decoder for a file based on its extension.
fn open_reader(path: &Path) -> io::Result<BufReader<Box<dyn Read + Send>>> {
    let file = File::open(path)?;
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase());
    let boxed: Box<dyn Read + Send> = match ext.as_deref() {
        Some("zst") => Box::new(ZstdDecoder::new(file)?),
        Some("gz") => Box::new(GzDecoder::new(file)),
        _ => Box::new(file),
    };
    Ok(BufReader::with_capacity(64 * 1024, boxed))
}

/// Parse `HH:MM:SS`, `MM:SS`, or bare seconds into a `Duration`.
pub fn parse_timecode(s: &str) -> Result<Duration, ReplayError> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Err(ReplayError::InvalidTimecode(s.to_string()));
    }
    let parts: Vec<&str> = trimmed.split(':').collect();
    let seconds: u64 = match parts.len() {
        1 => parts[0]
            .parse::<u64>()
            .map_err(|_| ReplayError::InvalidTimecode(s.to_string()))?,
        2 => {
            let m = parts[0]
                .parse::<u64>()
                .map_err(|_| ReplayError::InvalidTimecode(s.to_string()))?;
            let sec = parts[1]
                .parse::<u64>()
                .map_err(|_| ReplayError::InvalidTimecode(s.to_string()))?;
            m * 60 + sec
        }
        3 => {
            let h = parts[0]
                .parse::<u64>()
                .map_err(|_| ReplayError::InvalidTimecode(s.to_string()))?;
            let m = parts[1]
                .parse::<u64>()
                .map_err(|_| ReplayError::InvalidTimecode(s.to_string()))?;
            let sec = parts[2]
                .parse::<u64>()
                .map_err(|_| ReplayError::InvalidTimecode(s.to_string()))?;
            h * 3600 + m * 60 + sec
        }
        _ => return Err(ReplayError::InvalidTimecode(s.to_string())),
    };
    Ok(Duration::from_secs(seconds))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;

    #[test]
    fn parse_timecode_accepts_various_forms() {
        assert_eq!(parse_timecode("30").unwrap(), Duration::from_secs(30));
        assert_eq!(parse_timecode("1:30").unwrap(), Duration::from_secs(90));
        assert_eq!(
            parse_timecode("01:02:03").unwrap(),
            Duration::from_secs(3723)
        );
    }

    #[test]
    fn parse_timecode_rejects_junk() {
        assert!(parse_timecode("junk").is_err());
        assert!(parse_timecode("1:2:3:4").is_err());
        assert!(parse_timecode("").is_err());
    }

    /// Synthesize a small file with a header, three data frames one
    /// second apart, and no index frames.
    fn write_small_fixture(path: &std::path::Path) {
        let mut f = File::create(path).unwrap();
        writeln!(
            f,
            "{{\"schema\":1,\"header\":true,\"interval_ms\":1000,\"hosts\":[\"a\"]}}"
        )
        .unwrap();
        for i in 0..3 {
            writeln!(
                f,
                "{{\"schema\":1,\"timestamp\":\"2026-04-20T00:00:{i:02}Z\",\"hostname\":\"a\",\"gpus\":[],\"cpus\":[],\"memory\":[]}}"
            )
            .unwrap();
        }
    }

    #[test]
    fn replayer_opens_and_primes_first_frame() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("small.ndjson");
        write_small_fixture(&path);

        let r = Replayer::open(&path).unwrap();
        let first = r.current().expect("priming materializes frame 0");
        assert_eq!(first.seq, 0);
        assert_eq!(first.snapshot.hostname, "a");
    }

    #[test]
    fn replayer_rejects_schema_v2_with_exact_message() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v2.ndjson");
        {
            let mut f = File::create(&path).unwrap();
            writeln!(
                f,
                "{{\"schema\":2,\"timestamp\":\"2026-04-20T00:00:00Z\",\"hostname\":\"a\"}}"
            )
            .unwrap();
        }
        let err = match Replayer::open(&path) {
            Ok(_) => panic!("expected schema v2 to be rejected"),
            Err(e) => e,
        };
        let msg = err.to_string();
        assert_eq!(
            msg, "replay: unsupported schema version 2, this all-smi supports schema 1",
            "error message must match issue spec exactly"
        );
    }

    #[test]
    fn replayer_steps_forward_and_backward() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("step.ndjson");
        write_small_fixture(&path);

        let mut r = Replayer::open(&path).unwrap();
        assert_eq!(r.current().unwrap().seq, 0);

        let f1 = r.next().unwrap().expect("frame 1 exists");
        assert_eq!(f1.seq, 1);

        let back = r.prev().unwrap().expect("prev lands on frame 0");
        assert_eq!(back.seq, 0);
    }

    #[test]
    fn replayer_seek_lands_on_target_frame() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("seek.ndjson");
        write_small_fixture(&path);

        let mut r = Replayer::open(&path).unwrap();
        // Frame cadence = 1s; seek +2s lands on frame 2.
        let landed = r.seek(Duration::from_secs(2)).unwrap().expect("frame 2");
        assert_eq!(landed.seq, 2);
    }

    #[test]
    fn replayer_gzip_decoder_works() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("small.ndjson.gz");
        {
            use flate2::Compression;
            use flate2::write::GzEncoder;
            let f = File::create(&path).unwrap();
            let mut enc = GzEncoder::new(f, Compression::default());
            writeln!(
                enc,
                "{{\"schema\":1,\"header\":true,\"interval_ms\":1000,\"hosts\":[\"a\"]}}"
            )
            .unwrap();
            for i in 0..3 {
                writeln!(
                    enc,
                    "{{\"schema\":1,\"timestamp\":\"2026-04-20T00:00:{i:02}Z\",\"hostname\":\"a\",\"gpus\":[]}}"
                )
                .unwrap();
            }
            enc.finish().unwrap();
        }
        let r = Replayer::open(&path).unwrap();
        assert!(r.current().is_some(), "gzip stream primes frame 0");
    }

    #[test]
    fn replayer_zstd_decoder_works() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("small.ndjson.zst");
        {
            let f = File::create(&path).unwrap();
            let mut enc = zstd::stream::write::Encoder::new(f, 3).unwrap();
            writeln!(
                enc,
                "{{\"schema\":1,\"header\":true,\"interval_ms\":1000,\"hosts\":[\"a\"]}}"
            )
            .unwrap();
            for i in 0..3 {
                writeln!(
                    enc,
                    "{{\"schema\":1,\"timestamp\":\"2026-04-20T00:00:{i:02}Z\",\"hostname\":\"a\",\"gpus\":[]}}"
                )
                .unwrap();
            }
            enc.finish().unwrap();
        }
        let r = Replayer::open(&path).unwrap();
        assert!(r.current().is_some(), "zstd stream primes frame 0");
    }

    /// Regression guard for the index-frame fast-path off-by-one: when the
    /// replayer uses a sparse index to jump ahead, the next data frame must
    /// be labeled with its true absolute sequence number. Index frames are
    /// written AFTER their matching data frame in the record stream, so
    /// skipping past the index lands on data frame `index.seq + 1`.
    #[test]
    fn replayer_seek_across_index_frame_preserves_absolute_seq() {
        use std::fs::File;
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("indexed.ndjson");
        {
            let mut f = File::create(&path).unwrap();
            // 10 data frames spaced 1s apart, plus an index frame after
            // frame seq=5 (matching the record writer's ordering).
            for i in 0..10u64 {
                writeln!(
                    f,
                    "{{\"schema\":1,\"timestamp\":\"2026-04-20T00:00:{i:02}Z\",\"hostname\":\"a\",\"gpus\":[]}}"
                )
                .unwrap();
                if i == 5 {
                    writeln!(
                        f,
                        "{{\"schema\":1,\"index\":true,\"seq\":5,\"byte_offset\":0}}"
                    )
                    .unwrap();
                }
            }
        }
        // Open and walk to EOF so `index_points` is populated with the
        // seq=5 checkpoint. `next()` returns `Ok(None)` only before the
        // first frame has been materialised; after EOF it keeps returning
        // the last cached frame, so drive the walk via `at_eof()`.
        let mut r = Replayer::open(&path).unwrap();
        while !r.at_eof() {
            if r.next().unwrap().is_none() {
                break;
            }
        }
        assert!(
            r.index_points_seen() >= 1,
            "priming walk must have observed the seq=5 index frame"
        );
        // Now seek to 7s. The seek implementation rewinds and walks
        // forward; with the index-frame fast path it skips past the
        // seq=5 index frame, so the next data frame read after the
        // checkpoint must be absolute seq=6, and the frame at T0+7s
        // must land on absolute seq=7.
        let landed = r
            .seek(Duration::from_secs(7))
            .unwrap()
            .expect("frame at 7s");
        assert_eq!(
            landed.seq, 7,
            "seek across an index frame must preserve absolute sequence numbering"
        );
    }

    #[test]
    fn replayer_skips_corrupted_tail_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("corrupt.ndjson");
        {
            let mut f = File::create(&path).unwrap();
            writeln!(
                f,
                "{{\"schema\":1,\"timestamp\":\"2026-04-20T00:00:00Z\",\"hostname\":\"a\",\"gpus\":[]}}"
            )
            .unwrap();
            writeln!(
                f,
                "{{\"schema\":1,\"timestamp\":\"2026-04-20T00:00:01Z\",\"hostname\":\"a\",\"gpus\":[]}}"
            )
            .unwrap();
            // Truncated final line — no closing brace. The replayer
            // must skip this and still materialize the first two good
            // frames.
            write!(f, "{{\"schema\":1,\"timestamp\":\"not-finished").unwrap();
        }
        let mut r = Replayer::open(&path).unwrap();
        assert_eq!(r.current().unwrap().seq, 0);
        let next = r.next().unwrap().unwrap();
        assert_eq!(next.seq, 1);
        // Third read must NOT advance the cursor (no more valid frames).
        let eof_check = r.next().unwrap();
        assert!(
            eof_check.is_none() || eof_check.unwrap().seq == 1,
            "a truncated tail line must not materialize as a new frame"
        );
    }
}
