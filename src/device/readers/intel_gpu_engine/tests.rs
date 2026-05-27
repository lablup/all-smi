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

//! Unit tests for the Intel engine-busy counter helpers and the
//! [`EngineState`] delta tracker.
//!
//! Synthetic-clock tests use the `with_clock` constructor to drive
//! `now_fn` from a `static` cell — no real wall-clock sleep is needed.

use super::discovery::{normalize_engine_class, split_class_instance};
use super::*;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tempfile::tempdir;

// ----- Class-name normalisation -----

#[test]
fn normalize_engine_class_handles_known_tokens() {
    assert_eq!(normalize_engine_class("rcs"), "render");
    assert_eq!(normalize_engine_class("RCS"), "render");
    assert_eq!(normalize_engine_class("Render"), "render");
    assert_eq!(normalize_engine_class("ccs"), "compute");
    assert_eq!(normalize_engine_class("Compute"), "compute");
    assert_eq!(normalize_engine_class("COMPUTE"), "compute");
    assert_eq!(normalize_engine_class("bcs"), "copy");
    assert_eq!(normalize_engine_class("COPY"), "copy");
    assert_eq!(normalize_engine_class("vcs"), "video");
    assert_eq!(normalize_engine_class("video_decode"), "video");
    assert_eq!(normalize_engine_class("VIDEO_DECODE"), "video");
    assert_eq!(normalize_engine_class("vecs"), "video-enhance");
    assert_eq!(normalize_engine_class("VIDEO_ENHANCE"), "video-enhance");
}

#[test]
fn normalize_engine_class_unknown_becomes_other() {
    assert_eq!(normalize_engine_class(""), "other");
    assert_eq!(normalize_engine_class("xyzzy"), "other");
}

#[test]
fn split_class_instance_extracts_trailing_digits() {
    assert_eq!(split_class_instance("rcs0"), ("rcs", "0".to_string()));
    assert_eq!(split_class_instance("vcs12"), ("vcs", "12".to_string()));
    assert_eq!(split_class_instance("render"), ("render", String::new()));
    // All-digits name should not eat everything as instance — class
    // should still be the whole name.
    assert_eq!(split_class_instance("0"), ("0", String::new()));
}

// ----- Counter discovery -----

// Build a fake `cardN/device/` layout under `root`. Returns the device
// dir path that production code receives.
fn make_card_device(root: &Path) -> PathBuf {
    let device = root.join("card0").join("device");
    fs::create_dir_all(&device).unwrap();
    device
}

#[test]
fn discover_engine_counters_empty_when_no_engines() {
    let dir = tempdir().unwrap();
    let device = make_card_device(dir.path());
    let counters = discover_engine_counters(&device);
    assert!(
        counters.is_empty(),
        "expected no counters, got {counters:?}"
    );
}

#[test]
fn discover_engine_counters_i915_flat_layout() {
    let dir = tempdir().unwrap();
    let device = make_card_device(dir.path());
    // Canonical i915 flat layout: `cardN/engine/<name>/busy`.
    let engine_root = device.parent().unwrap().join("engine");
    for name in ["rcs0", "bcs0", "vcs0", "vecs0"] {
        let p = engine_root.join(name);
        fs::create_dir_all(&p).unwrap();
        fs::write(p.join("busy"), "0\n").unwrap();
    }

    let counters = discover_engine_counters(&device);
    let classes: Vec<&str> = counters.iter().map(|c| c.class).collect();
    // Sorted by class: copy, render, video, video-enhance.
    assert_eq!(classes, vec!["copy", "render", "video", "video-enhance"]);
    for c in &counters {
        assert_eq!(c.instance, "0");
        assert!(c.path.ends_with("busy"));
    }
}

#[test]
fn discover_engine_counters_i915_nested_layout() {
    let dir = tempdir().unwrap();
    let device = make_card_device(dir.path());
    let engine_root = device.parent().unwrap().join("engine");
    // Nested: `engine/rcs/0/busy`, `engine/rcs/1/busy`.
    for inst in ["0", "1"] {
        let p = engine_root.join("rcs").join(inst);
        fs::create_dir_all(&p).unwrap();
        fs::write(p.join("busy"), "0\n").unwrap();
    }

    let counters = discover_engine_counters(&device);
    assert_eq!(counters.len(), 2);
    for c in &counters {
        assert_eq!(c.class, "render");
    }
    let instances: Vec<&str> = counters.iter().map(|c| c.instance.as_str()).collect();
    assert_eq!(instances, vec!["0", "1"]);
}

#[test]
fn discover_engine_counters_xe_flat_layout() {
    let dir = tempdir().unwrap();
    let device = make_card_device(dir.path());
    // xe single-GT flat: `device/tile0/gt0/engines/<name>/busy_ns`.
    let engines = device.join("tile0").join("gt0").join("engines");
    for name in ["rcs0", "ccs0", "bcs0"] {
        let p = engines.join(name);
        fs::create_dir_all(&p).unwrap();
        fs::write(p.join("busy_ns"), "0\n").unwrap();
    }

    let counters = discover_engine_counters(&device);
    let classes: Vec<&str> = counters.iter().map(|c| c.class).collect();
    // Sorted: compute, copy, render.
    assert_eq!(classes, vec!["compute", "copy", "render"]);
    for c in &counters {
        assert!(c.path.ends_with("busy_ns"));
    }
}

#[test]
fn discover_engine_counters_xe_nested_uppercase() {
    let dir = tempdir().unwrap();
    let device = make_card_device(dir.path());
    // Some xe revisions use uppercase nested layout:
    // `device/tile0/gt0/engines/RENDER/0/busy_ns`.
    let engines = device.join("tile0").join("gt0").join("engines");
    let p = engines.join("RENDER").join("0");
    fs::create_dir_all(&p).unwrap();
    fs::write(p.join("busy_ns"), "0\n").unwrap();

    let counters = discover_engine_counters(&device);
    assert_eq!(counters.len(), 1);
    assert_eq!(counters[0].class, "render");
    assert_eq!(counters[0].instance, "0");
}

#[test]
fn discover_engine_counters_xe_multi_gt() {
    let dir = tempdir().unwrap();
    let device = make_card_device(dir.path());
    // Two GTs each exposing an rcs counter.
    for gt in ["gt0", "gt1"] {
        let p = device.join("tile0").join(gt).join("engines").join("rcs0");
        fs::create_dir_all(&p).unwrap();
        fs::write(p.join("busy_ns"), "0\n").unwrap();
    }

    let counters = discover_engine_counters(&device);
    assert_eq!(counters.len(), 2);
    assert!(counters.iter().all(|c| c.class == "render"));
}

// ----- EngineState delta tracker -----
//
// The clock-injection pattern: a single static AtomicU64 stores the
// number of nanoseconds to advance from a baseline `Instant`. Tests
// bump this counter, then the injected `now_fn` returns
// `baseline + Duration::from_nanos(offset_ns)`. We capture the baseline
// once per test from a fresh `Instant::now()` and feed it to a closure
// through a thread-local static. Function-pointer-typed `now_fn`
// requires a `fn()` (no closure capture), so the synthetic clock is
// implemented via process-global statics behind locks. Tests in this
// module run serially against the clock state.

static CLOCK_BASELINE: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
static CLOCK_OFFSET_NS: AtomicU64 = AtomicU64::new(0);
static CLOCK_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn fake_now() -> Instant {
    let baseline = *CLOCK_BASELINE.get_or_init(Instant::now);
    baseline + Duration::from_nanos(CLOCK_OFFSET_NS.load(Ordering::SeqCst))
}

fn reset_clock() {
    // Lazily ensure the baseline is set so subsequent tests reuse it.
    let _ = CLOCK_BASELINE.get_or_init(Instant::now);
    CLOCK_OFFSET_NS.store(0, Ordering::SeqCst);
}

fn advance_ns(ns: u64) {
    CLOCK_OFFSET_NS.fetch_add(ns, Ordering::SeqCst);
}

// Helper: write an i915 flat `engine/<name>/busy` and return the
// device dir.
fn build_card_with_engines(root: &Path, engines: &[(&str, u64)]) -> PathBuf {
    let device = make_card_device(root);
    let engine_root = device.parent().unwrap().join("engine");
    for (name, initial) in engines {
        let p = engine_root.join(name);
        fs::create_dir_all(&p).unwrap();
        fs::write(p.join("busy"), format!("{initial}\n")).unwrap();
    }
    device
}

fn write_engine_busy(device: &Path, name: &str, value: u64) {
    let engine_root = device.parent().unwrap().join("engine");
    fs::write(engine_root.join(name).join("busy"), format!("{value}\n")).unwrap();
}

#[test]
fn refresh_returns_unavailable_when_no_engines() {
    let _g = CLOCK_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    reset_clock();

    let dir = tempdir().unwrap();
    let device = make_card_device(dir.path());

    let mut state = EngineState::with_clock(fake_now);
    let r = refresh(&mut state, &device);
    assert_eq!(r.primary_utilization, 0.0);
    assert!(r.per_class.is_empty());
    assert_eq!(r.status_note, Some(ENGINE_UNAVAILABLE_NOTE));
    // Second call must not re-walk the sysfs tree.
    assert!(state.discovery_attempted);

    let r2 = refresh(&mut state, &device);
    assert_eq!(r2.status_note, Some(ENGINE_UNAVAILABLE_NOTE));
}

#[test]
fn refresh_seeding_call_returns_zero() {
    let _g = CLOCK_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    reset_clock();

    let dir = tempdir().unwrap();
    let device = build_card_with_engines(dir.path(), &[("rcs0", 1_000)]);

    let mut state = EngineState::with_clock(fake_now);
    let r = refresh(&mut state, &device);
    // Seeding call: counter list now known, baseline stamped, but
    // no delta available yet.
    assert_eq!(r.primary_utilization, 0.0);
    assert!(r.per_class.is_empty());
    assert_eq!(r.status_note, Some(ENGINE_SEEDING_NOTE));
    assert!(state.last_tick.is_some());
    assert_eq!(state.samples.len(), 1);
    assert_eq!(state.samples[0].last_busy_ns, 1_000);
}

#[test]
fn refresh_computes_engine_busy_fraction() {
    let _g = CLOCK_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    reset_clock();

    let dir = tempdir().unwrap();
    // Initial busy = 0 ns at t=0.
    let device = build_card_with_engines(dir.path(), &[("rcs0", 0)]);

    let mut state = EngineState::with_clock(fake_now);
    refresh(&mut state, &device); // seed at t=0

    // Advance wall clock 100 ms; engine recorded 50 ms busy = 50%.
    advance_ns(100_000_000);
    write_engine_busy(&device, "rcs0", 50_000_000);
    let r = refresh(&mut state, &device);

    assert!(
        (r.primary_utilization - 50.0).abs() < 0.01,
        "expected ~50%, got {}",
        r.primary_utilization
    );
    assert_eq!(r.per_class.len(), 1);
    assert_eq!(r.per_class[0].0, "render");
    assert!((r.per_class[0].1 - 50.0).abs() < 0.01);
    assert!(r.status_note.is_none());
}

#[test]
fn refresh_clamps_to_hundred_percent() {
    let _g = CLOCK_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    reset_clock();

    let dir = tempdir().unwrap();
    let device = build_card_with_engines(dir.path(), &[("rcs0", 0)]);

    let mut state = EngineState::with_clock(fake_now);
    refresh(&mut state, &device);

    // Buggy driver reports busy > wall: must clamp to 100, not panic.
    advance_ns(10_000_000); // 10 ms wall
    write_engine_busy(&device, "rcs0", 30_000_000); // 30 ms busy
    let r = refresh(&mut state, &device);
    assert!((r.primary_utilization - 100.0).abs() < 0.01);
}

#[test]
fn refresh_counter_reset_clamps_to_zero() {
    let _g = CLOCK_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    reset_clock();

    let dir = tempdir().unwrap();
    let device = build_card_with_engines(dir.path(), &[("rcs0", 1_000_000_000)]);

    let mut state = EngineState::with_clock(fake_now);
    refresh(&mut state, &device); // seed baseline at 1e9

    // Counter resets (driver reload, suspend/resume): current < last.
    advance_ns(100_000_000);
    write_engine_busy(&device, "rcs0", 0);
    let r = refresh(&mut state, &device);
    // saturating_sub keeps delta at 0, percentage at 0.
    assert_eq!(r.primary_utilization, 0.0);
    // The baseline must be updated to the new (lower) value so the
    // next refresh tracks from there.
    assert_eq!(state.samples[0].last_busy_ns, 0);
}

#[test]
fn refresh_multi_engine_uses_render_or_compute_as_primary() {
    let _g = CLOCK_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    reset_clock();

    let dir = tempdir().unwrap();
    let device = build_card_with_engines(dir.path(), &[("rcs0", 0), ("bcs0", 0), ("ccs0", 0)]);

    let mut state = EngineState::with_clock(fake_now);
    refresh(&mut state, &device);

    advance_ns(100_000_000); // 100 ms
    write_engine_busy(&device, "rcs0", 80_000_000); // 80%
    write_engine_busy(&device, "bcs0", 90_000_000); // 90% — but this is copy
    write_engine_busy(&device, "ccs0", 20_000_000); // 20%
    let r = refresh(&mut state, &device);

    // Primary must be max(render=80, compute=20) = 80, NOT copy=90.
    assert!(
        (r.primary_utilization - 80.0).abs() < 0.01,
        "expected primary 80, got {}",
        r.primary_utilization
    );

    // Detail map should contain all three classes.
    let classes: Vec<&str> = r.per_class.iter().map(|(c, _)| *c).collect();
    assert_eq!(classes, vec!["render", "compute", "copy"]);
    let render_pct = r.per_class.iter().find(|(c, _)| *c == "render").unwrap().1;
    let compute_pct = r.per_class.iter().find(|(c, _)| *c == "compute").unwrap().1;
    let copy_pct = r.per_class.iter().find(|(c, _)| *c == "copy").unwrap().1;
    assert!((render_pct - 80.0).abs() < 0.01);
    assert!((compute_pct - 20.0).abs() < 0.01);
    assert!((copy_pct - 90.0).abs() < 0.01);
}

#[test]
fn refresh_falls_back_to_max_when_no_render_or_compute() {
    let _g = CLOCK_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    reset_clock();

    let dir = tempdir().unwrap();
    // Only video and copy engines (synthetic case).
    let device = build_card_with_engines(dir.path(), &[("vcs0", 0), ("bcs0", 0)]);

    let mut state = EngineState::with_clock(fake_now);
    refresh(&mut state, &device);

    advance_ns(100_000_000);
    write_engine_busy(&device, "vcs0", 35_000_000); // 35%
    write_engine_busy(&device, "bcs0", 60_000_000); // 60%
    let r = refresh(&mut state, &device);
    // No render or compute: fall back to the overall max.
    assert!((r.primary_utilization - 60.0).abs() < 0.01);
}

#[test]
fn refresh_multiple_instances_of_same_class_use_max() {
    let _g = CLOCK_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    reset_clock();

    let dir = tempdir().unwrap();
    let device = build_card_with_engines(dir.path(), &[("vcs0", 0), ("vcs1", 0)]);

    let mut state = EngineState::with_clock(fake_now);
    refresh(&mut state, &device);

    advance_ns(100_000_000);
    write_engine_busy(&device, "vcs0", 30_000_000); // 30%
    write_engine_busy(&device, "vcs1", 70_000_000); // 70%
    let r = refresh(&mut state, &device);
    // Same class aggregated as max(30, 70) = 70 — not summed to 100.
    assert_eq!(r.per_class.len(), 1);
    assert_eq!(r.per_class[0].0, "video");
    assert!((r.per_class[0].1 - 70.0).abs() < 0.01);
}

#[test]
fn refresh_handles_missing_counter_file_gracefully() {
    let _g = CLOCK_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    reset_clock();

    let dir = tempdir().unwrap();
    let device = build_card_with_engines(dir.path(), &[("rcs0", 0)]);

    let mut state = EngineState::with_clock(fake_now);
    refresh(&mut state, &device);

    // Delete the counter file mid-flight; refresh must not panic.
    let engine_root = device.parent().unwrap().join("engine");
    fs::remove_file(engine_root.join("rcs0").join("busy")).unwrap();

    advance_ns(100_000_000);
    let r = refresh(&mut state, &device);
    // last_busy_pct stays at 0.0; primary follows.
    assert_eq!(r.primary_utilization, 0.0);
    // Baseline must NOT be advanced when read fails — preserves the
    // old value for the next successful read.
    assert_eq!(state.samples[0].last_busy_ns, 0);
}

#[test]
fn refresh_zero_wall_delta_returns_quietly() {
    let _g = CLOCK_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    reset_clock();

    let dir = tempdir().unwrap();
    let device = build_card_with_engines(dir.path(), &[("rcs0", 0)]);

    let mut state = EngineState::with_clock(fake_now);
    refresh(&mut state, &device);
    // Do NOT advance the clock.
    let r = refresh(&mut state, &device);
    assert_eq!(r.primary_utilization, 0.0);
    assert!(r.per_class.is_empty());
    assert!(r.status_note.is_none());
}

// ----- Mutex poisoning recovery -----

#[test]
fn refresh_with_lock_recovers_from_poisoned_mutex() {
    use std::sync::Arc;

    let state: Arc<Mutex<EngineState>> = Arc::new(Mutex::new(EngineState::empty()));
    let poisoner = Arc::clone(&state);

    // Poison the mutex: the spawned thread acquires the lock and panics,
    // leaving the mutex in a poisoned state.
    let _ = std::thread::spawn(move || {
        let _guard = poisoner.lock().unwrap();
        panic!("intentional mutex poisoning");
    })
    .join();

    // Confirm the mutex is actually poisoned before we test recovery.
    assert!(
        state.lock().is_err(),
        "mutex must be poisoned before testing recovery"
    );

    // refresh_with_lock must not panic and must return a valid readout.
    // With an empty tempdir (no sysfs engine files) it returns the
    // unavailable readout — that is expected and fine.
    let dir = tempdir().unwrap();
    let readout = refresh_with_lock(&state, dir.path());
    assert_eq!(readout.primary_utilization, 0.0);
    assert!(readout.per_class.is_empty());

    // The std::sync::Mutex poison flag is NOT cleared by into_inner(),
    // so the mutex remains poisoned after recovery. We use
    // unwrap_or_else to inspect the internal state without panicking.
    let guard = state.lock().unwrap_or_else(|e| e.into_inner());
    // refresh() ran discovery on the empty dir: samples empty, discovery done.
    assert!(
        guard.samples.is_empty(),
        "recovered state should have no samples"
    );
    assert!(
        guard.discovery_attempted,
        "discovery should have been attempted in the recovery refresh"
    );
}
