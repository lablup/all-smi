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

//! Tests for replay seeking and elapsed time (issue #503): `elapsed()`
//! measured from frame 0 however far the cache has moved, seeks inside
//! the cache that touch no file, and far seeks that use index frames.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::time::Duration;

use chrono::{TimeZone, Utc};

use super::{DECODED_FRAMES, FILE_OPENS, FRAME_CACHE_MAX, Replayer};

/// Index frame spacing the recorder uses (`record::INDEX_EVERY_N_FRAMES`).
const INDEX_EVERY: u64 = 1000;

/// Write `frames` data frames, frame `i` stamped `i * cadence_secs` after
/// the epoch below, with index frames where the recorder puts them (after
/// data frames 1000, 2000, ...). `interval_ms` is the header's claim,
/// which may differ from the real cadence; `None` writes no header.
fn write_fixture(path: &Path, frames: u64, cadence_secs: u64, interval_ms: Option<u64>) {
    let mut f = BufWriter::new(File::create(path).unwrap());
    if let Some(interval_ms) = interval_ms {
        writeln!(
            f,
            "{{\"schema\":1,\"header\":true,\"interval_ms\":{interval_ms},\"hosts\":[\"a\"]}}"
        )
        .unwrap();
    }
    let start = Utc.with_ymd_and_hms(2026, 4, 20, 0, 0, 0).unwrap();
    for i in 0..frames {
        let ts = (start + chrono::Duration::seconds((i * cadence_secs) as i64))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        writeln!(
            f,
            "{{\"schema\":1,\"timestamp\":\"{ts}\",\"hostname\":\"a\",\"gpus\":[]}}"
        )
        .unwrap();
        if i > 0 && i % INDEX_EVERY == 0 {
            writeln!(
                f,
                "{{\"schema\":1,\"index\":true,\"seq\":{i},\"byte_offset\":0}}"
            )
            .unwrap();
        }
    }
    f.flush().unwrap();
}

fn read_to_end(r: &mut Replayer) {
    while r.next().unwrap().is_some() {}
    assert!(r.at_eof());
}

fn decoded() -> usize {
    DECODED_FRAMES.with(|count| count.get())
}

fn opens() -> usize {
    FILE_OPENS.with(|count| count.get())
}

fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

/// `elapsed()` is measured from frame 0 after more than `FRAME_CACHE_MAX`
/// frames. It used to subtract the cache's front frame, which stops being
/// frame 0 once the cache wraps, so this read about 1024 s short.
#[test]
fn elapsed_is_measured_from_frame_zero_after_the_cache_wraps() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("long.ndjson");
    write_fixture(&path, 1500, 1, Some(1000));

    let mut r = Replayer::open(&path).unwrap();
    assert_eq!(r.first_timestamp(), r.current().map(|f| f.timestamp));
    read_to_end(&mut r);
    const { assert!(1500 > FRAME_CACHE_MAX) };
    assert_eq!(r.current().unwrap().seq, 1499);
    assert_eq!(r.elapsed(), Some(secs(1499)));

    let landed = r.seek(secs(1400)).unwrap().expect("frame at 1400 s").seq;
    assert_eq!(landed, 1400);
    assert_eq!(r.elapsed(), Some(secs(1400)));
}

/// A seek to a frame that is already cached decodes nothing and opens no
/// file. It used to re-open and decode from frame 0 every time.
#[test]
fn a_seek_inside_the_cache_does_no_io() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cached.ndjson");
    write_fixture(&path, 1500, 1, Some(1000));
    let mut r = Replayer::open(&path).unwrap();
    read_to_end(&mut r);
    r.seek(secs(1400)).unwrap();

    let (decoded_before, opens_before) = (decoded(), opens());
    for target in [1300, 1499, 600, 1400] {
        let landed = r.seek(secs(target)).unwrap().expect("cached frame").seq;
        assert_eq!(landed, target);
        assert_eq!(r.elapsed(), Some(secs(target)));
    }
    // Past the end, with the whole file read: the last frame, no I/O.
    assert_eq!(r.seek(secs(9_999)).unwrap().unwrap().seq, 1499);
    assert_eq!(decoded() - decoded_before, 0, "no frame decoded");
    assert_eq!(opens() - opens_before, 0, "no file opened");
}

/// A far seek with `interval_ms` in the header and the index frames known
/// decodes at most one index spacing of frames, and `elapsed()` is right
/// after a re-open that skipped ahead from an index frame.
#[test]
fn a_far_seek_uses_the_index_and_keeps_elapsed_correct() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("indexed.ndjson");
    write_fixture(&path, 3500, 1, Some(1000));
    let mut r = Replayer::open(&path).unwrap();
    read_to_end(&mut r);
    assert_eq!(r.index_points_seen(), 3);

    // Back to the start region: before the cache, no index at or before
    // the target, so the scan starts at frame 0 and stops at the target.
    let before = decoded();
    assert_eq!(r.seek(secs(100)).unwrap().unwrap().seq, 100);
    assert_eq!(r.elapsed(), Some(secs(100)));
    assert!(decoded() - before <= 110, "decoded {}", decoded() - before);

    // Far ahead of the cache: skip to the index frame at 3000.
    let before = decoded();
    assert_eq!(r.seek(secs(3400)).unwrap().unwrap().seq, 3400);
    assert_eq!(r.elapsed(), Some(secs(3400)));
    let cost = decoded() - before;
    assert!(
        cost <= INDEX_EVERY as usize + 10,
        "a far seek must decode at most one index spacing, decoded {cost}"
    );

    // Far behind the cache: skip to the index frame at 1000.
    let before = decoded();
    assert_eq!(r.seek(secs(1700)).unwrap().unwrap().seq, 1700);
    assert_eq!(r.elapsed(), Some(secs(1700)));
    let cost = decoded() - before;
    assert!(cost <= INDEX_EVERY as usize + 10, "decoded {cost}");

    // Stepping after an index-assisted seek keeps absolute numbering.
    assert_eq!(r.next().unwrap().unwrap().seq, 1701);
    assert_eq!(r.elapsed(), Some(secs(1701)));
}

/// The header's `interval_ms` is only an estimate. Whether the recording
/// really ran slower or faster than it claims, the seek lands on the first
/// frame at or after the target.
#[test]
fn a_seek_corrects_a_wrong_interval_estimate_in_both_directions() {
    let dir = tempfile::tempdir().unwrap();
    for (name, interval_ms) in [("overshoot", 500), ("undershoot", 2000)] {
        let path = dir.path().join(format!("{name}.ndjson"));
        // Real cadence 1 s whatever the header says.
        write_fixture(&path, 2600, 1, Some(interval_ms));
        let mut r = Replayer::open(&path).unwrap();
        read_to_end(&mut r);
        for target in [100, 1200, 40, 2599, 0] {
            let landed = r.seek(secs(target)).unwrap().expect("frame").seq;
            assert_eq!(landed, target, "{name}: seek to {target} s");
            assert_eq!(r.elapsed(), Some(secs(target)));
        }
    }
}

/// Without a header there is no interval to estimate from: seeks still
/// land correctly, forward from the cache and backward by rescanning.
#[test]
fn seeks_without_interval_ms_fall_back_to_a_linear_scan() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("headerless.ndjson");
    write_fixture(&path, 1300, 1, None);
    let mut r = Replayer::open(&path).unwrap();

    // Forward from a freshly opened file: reads on from the cache, no
    // re-open.
    let opens_before = opens();
    assert_eq!(r.seek(secs(1200)).unwrap().unwrap().seq, 1200);
    assert_eq!(opens() - opens_before, 0);
    assert_eq!(r.elapsed(), Some(secs(1200)));

    // Frame 50 has been evicted (the cache holds 177..=1200).
    assert_eq!(r.seek(secs(50)).unwrap().unwrap().seq, 50);
    assert_eq!(r.elapsed(), Some(secs(50)));

    // Between two frames: the first frame at or after the target.
    let path = dir.path().join("sparse.ndjson");
    write_fixture(&path, 10, 10, None);
    let mut r = Replayer::open(&path).unwrap();
    assert_eq!(r.seek(secs(25)).unwrap().unwrap().seq, 3);
    assert_eq!(r.seek(secs(20)).unwrap().unwrap().seq, 2);
    // Past the end: the last frame, and the call returns.
    assert_eq!(r.seek(secs(500)).unwrap().unwrap().seq, 9);
    assert_eq!(r.seek(Duration::ZERO).unwrap().unwrap().seq, 0);
}

/// `prev()` from the frame right after an index frame steps onto the
/// indexed frame. The re-open used to treat a checkpoint as usable for a
/// target equal to its own frame, which lands one frame late, so the
/// cursor could not move back across a checkpoint.
#[test]
fn prev_steps_back_across_an_index_frame() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("checkpoint.ndjson");
    write_fixture(&path, 2300, 1, Some(1000));
    let mut r = Replayer::open(&path).unwrap();
    read_to_end(&mut r);

    // Index frames follow data frames 1000 and 2000. Re-open straight
    // onto frame 2001, which leaves it alone at the front of the cache.
    r.rewind_and_seek_to(2001).unwrap();
    assert_eq!(r.current().unwrap().seq, 2001);
    assert_eq!(r.cache.front().unwrap().seq, 2001);
    assert_eq!(r.prev().unwrap().unwrap().seq, 2000);
    assert_eq!(r.elapsed(), Some(secs(2000)));
    assert_eq!(r.seek(secs(2000)).unwrap().unwrap().seq, 2000);
    assert_eq!(r.seek(secs(1000)).unwrap().unwrap().seq, 1000);
}

/// A recording that ran slower than its header claims (slow remote
/// cycles, a suspended host) makes the `interval_ms` estimate overshoot.
/// Correcting it re-opens the file once at the index frame before the
/// target. It used to step back with `prev()`, which re-opened the file
/// once per index spacing and skipped every line up to the checkpoint
/// each time.
#[test]
fn an_overshooting_estimate_is_corrected_with_one_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("slow.ndjson");
    // Real cadence 2 s, header claims 1 s: the estimate for any target is
    // twice the real frame number.
    write_fixture(&path, 20_000, 2, Some(1000));

    // Checkpoints known up to frame 11 000 (the one after frame 12 000 is
    // not read yet), cursor back near the start.
    let mut r = Replayer::open(&path).unwrap();
    assert_eq!(r.seek(secs(24_000)).unwrap().unwrap().seq, 12_000);
    assert_eq!(r.seek(secs(200)).unwrap().unwrap().seq, 100);

    // Past every known checkpoint, so the estimate decides: frame 29 999,
    // beyond the end. The landing is the last frame, 4 999 frames past the
    // target; the correction goes back to frame 15 000 with one re-open.
    let (decoded_before, opens_before) = (decoded(), opens());
    assert_eq!(r.seek(secs(30_000)).unwrap().unwrap().seq, 15_000);
    assert_eq!(r.elapsed(), Some(secs(30_000)));
    assert_eq!(opens() - opens_before, 2, "overshoot, then one correction");
    // From checkpoint 11 000 to the end, then at most one index spacing.
    assert!(
        decoded() - decoded_before <= 9_000 + INDEX_EVERY as usize + 10,
        "decoded {}",
        decoded() - decoded_before
    );

    // With every checkpoint known, a seek goes straight to the index
    // frame before the target.
    for (target, frame) in [(4_000, 2_000), (39_000, 19_500), (21_000, 10_500)] {
        let (decoded_before, opens_before) = (decoded(), opens());
        assert_eq!(r.seek(secs(target)).unwrap().unwrap().seq, frame);
        assert!(opens() - opens_before <= 1);
        assert!(
            decoded() - decoded_before <= INDEX_EVERY as usize + 10,
            "seek to {target} s decoded {}",
            decoded() - decoded_before
        );
    }
}
