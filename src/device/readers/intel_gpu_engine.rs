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

//! Engine-busy counter discovery, sampling, and delta computation for
//! the Intel client GPU reader.
//!
//! Both the `i915` and `xe` kernel drivers expose per-engine monotonic
//! busy-time counters in sysfs. Sampling them at two points in time and
//! dividing the busy delta by the wall-clock delta yields an
//! engine-busy percentage — the same number `intel_gpu_top` prints. This
//! module owns:
//!
//! 1. Discovery of every counter file the kernel exposes for a card
//!    (handles the i915 flat and nested layouts plus the xe layout
//!    across multiple tiles and GTs).
//! 2. The `EngineState` per-card delta tracker, including mutex
//!    poisoning recovery and seeding semantics on the first sample.
//! 3. The class-name normalisation so an i915 `rcs0` and an xe
//!    `RENDER/0` both surface as `"render"` in the `detail` map.
//!
//! The PMU `perf_event_open(2)` fallback used by `intel_gpu_top` on
//! kernels without sysfs engine counters is **not** included in v1 —
//! when sysfs returns nothing, the reader continues to emit a valid
//! `GpuInfo` with `utilization = 0.0` and an explanatory `detail`
//! entry. Adding the PMU fallback is tracked as follow-up work.

use crate::device::readers::intel_gpu_sysfs::read_u64;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

#[path = "intel_gpu_engine/discovery.rs"]
mod discovery;
pub use discovery::discover_engine_counters;

#[cfg(test)]
#[path = "intel_gpu_engine/tests.rs"]
mod tests;

// Layout cheat-sheet (paths are relative to `device_dir`, i.e.
// `/sys/class/drm/cardN/device`):
//
//   i915, flat layout (most common):
//     ../engine/<class+instance>/busy           e.g. `../engine/rcs0/busy`
//   i915, nested layout (rare, older kernels):
//     ../engine/<class>/<instance>/busy
//   xe (single GT):
//     tile0/gt0/engines/<class+instance>/busy_ns
//   xe (multi-GT, e.g. some Battlemage SKUs):
//     tile<T>/gt<G>/engines/<class+instance>/busy_ns
//
// In every layout the entry that contains the counter is a regular
// file named either `busy` or `busy_ns`. We probe each layout,
// normalise the engine class to a short lowercase name, and emit one
// [`EngineCounter`] per resolved counter file.

/// A discovered engine-busy counter sysfs entry.
///
/// `class` is already normalised to one of the short tokens returned by
/// [`normalize_engine_class`]; clients comparing classes therefore do
/// not need to repeat the i915-vs-xe-vs-mixed-case dance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineCounter {
    /// Normalised engine class — e.g. `"render"`, `"compute"`, `"copy"`,
    /// `"video"`, `"video-enhance"`. Unknown raw names become `"other"`.
    pub class: &'static str,
    /// Instance suffix as a string, e.g. `"0"` or `"1"`. Empty when the
    /// path layout did not encode an instance.
    pub instance: String,
    /// Absolute path to the counter file (`busy` or `busy_ns`).
    pub path: PathBuf,
}

/// Per-engine running snapshot used to compute deltas across refreshes.
#[derive(Debug, Clone)]
pub struct EngineSample {
    /// The static description of the counter — class, instance, path.
    pub counter: EngineCounter,
    /// Last observed busy counter value, in nanoseconds.
    pub last_busy_ns: u64,
    /// The most recently computed engine-busy percent for this engine
    /// (clamped to `[0, 100]`). Surfaced via the `detail` map.
    pub last_busy_pct: f64,
}

/// Per-card mutable state. Held behind a `Mutex` because `get_gpu_info`
/// is invoked concurrently by the collector thread and the API server.
pub struct EngineState {
    /// Engines we have already discovered. When this Vec is empty AND
    /// `discovery_attempted` is true the kernel does not expose engine
    /// counters on this build — we surface the explanatory message and
    /// stop trying.
    pub samples: Vec<EngineSample>,
    /// Wall-clock at the last successful sample. `None` before the very
    /// first sample (seeding call) and after a poisoning recovery.
    pub last_tick: Option<Instant>,
    /// Whether we have already attempted to enumerate engine counters
    /// for this card. Avoids re-walking sysfs on every refresh when the
    /// kernel does not expose the counter tree.
    pub discovery_attempted: bool,
    /// Wall-clock provider. Injected as a function pointer so unit
    /// tests can advance simulated time without hitting the real
    /// monotonic clock. The default is [`Instant::now`].
    now_fn: fn() -> Instant,
}

impl std::fmt::Debug for EngineState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EngineState")
            .field("samples", &self.samples)
            .field("last_tick", &self.last_tick)
            .field("discovery_attempted", &self.discovery_attempted)
            .finish_non_exhaustive()
    }
}

impl EngineState {
    /// Construct an empty state using the real monotonic clock.
    pub fn empty() -> Self {
        Self {
            samples: Vec::new(),
            last_tick: None,
            discovery_attempted: false,
            now_fn: Instant::now,
        }
    }

    /// Test-only constructor that lets the harness drive the wall clock.
    #[cfg(test)]
    pub fn with_clock(now_fn: fn() -> Instant) -> Self {
        Self {
            samples: Vec::new(),
            last_tick: None,
            discovery_attempted: false,
            now_fn,
        }
    }
}

/// Aggregated outcome of one engine-busy refresh.
#[derive(Debug, Clone)]
pub struct EngineReadout {
    /// Primary GPU utilization to surface as `GpuInfo.utilization`.
    /// Equals `max(render, compute)` when both are present, else the
    /// max across all known engines, else `0.0` (no counters or
    /// seeding call).
    pub primary_utilization: f64,
    /// Per-engine percentages keyed by normalised class name. Empty
    /// when the kernel did not expose engine counters or when this is
    /// the seeding call.
    pub per_class: Vec<(&'static str, f64)>,
    /// Set to `Some(msg)` when the reader should surface an explanatory
    /// `detail["Utilization"] = msg` entry; `None` once engine data is
    /// available.
    pub status_note: Option<&'static str>,
}

/// One status-note string used when the kernel does not expose engine
/// counters. Kept as a `'static` for cheap copying into the detail map.
pub const ENGINE_UNAVAILABLE_NOTE: &str =
    "Engine counters unavailable (kernel does not expose engine busy)";

/// One status-note string for the very first sample: we have the
/// counter list but no baseline to delta against yet, so utilization is
/// reported as 0.0 for one cycle.
pub const ENGINE_SEEDING_NOTE: &str = "Engine counters seeded (utilization available next refresh)";

/// Take one refresh: read each counter, compute deltas against the
/// previous sample, update the running state, and return an
/// [`EngineReadout`] the caller folds into the produced `GpuInfo`.
///
/// On the very first call (`last_tick` is `None`) this seeds the
/// samples and returns a zero-utilization readout — there is no
/// baseline to subtract from yet. Every subsequent call computes the
/// per-engine busy percentage.
///
/// Counter-reset and clock-skew safety: each per-engine delta is
/// computed with `saturating_sub`, and the wall delta is taken from a
/// monotonic `Instant` so it cannot go backwards. The percentage is
/// clamped to `[0, 100]` to defend against any future driver bug that
/// reports busy time exceeding wall time.
pub fn refresh(state: &mut EngineState, device_dir: &Path) -> EngineReadout {
    // First refresh ever: walk the sysfs tree to discover counters.
    if !state.discovery_attempted {
        let counters = discover_engine_counters(device_dir);
        state.samples = counters
            .into_iter()
            .map(|counter| EngineSample {
                counter,
                last_busy_ns: 0,
                last_busy_pct: 0.0,
            })
            .collect();
        state.discovery_attempted = true;
    }

    // No counters at all -> nothing we can do.
    if state.samples.is_empty() {
        return EngineReadout {
            primary_utilization: 0.0,
            per_class: Vec::new(),
            status_note: Some(ENGINE_UNAVAILABLE_NOTE),
        };
    }

    let now = (state.now_fn)();
    let prev_tick = state.last_tick;

    // Read current counter values *before* deciding what to do with
    // them. A read failure on one engine is non-fatal — we simply do
    // not advance that engine's baseline.
    let current_values: Vec<Option<u64>> = state
        .samples
        .iter()
        .map(|s| read_u64(&s.counter.path))
        .collect();

    // Seeding call: we have the counters but no previous tick to
    // delta against. Stamp the baselines and return 0%.
    let Some(prev) = prev_tick else {
        for (sample, value) in state.samples.iter_mut().zip(current_values.iter()) {
            if let Some(v) = value {
                sample.last_busy_ns = *v;
            }
            sample.last_busy_pct = 0.0;
        }
        state.last_tick = Some(now);
        return EngineReadout {
            primary_utilization: 0.0,
            per_class: Vec::new(),
            status_note: Some(ENGINE_SEEDING_NOTE),
        };
    };

    let delta_wall_ns = now.saturating_duration_since(prev).as_nanos();
    // `Instant` is monotonic but a zero-length delta is possible if a
    // test or buggy caller invokes the refresh twice without any wall
    // time passing. Treat that as "no data this cycle".
    if delta_wall_ns == 0 {
        return EngineReadout {
            primary_utilization: 0.0,
            per_class: Vec::new(),
            status_note: None,
        };
    }
    let delta_wall_f = delta_wall_ns as f64;

    // Compute per-engine percentages and update baselines.
    for (sample, value) in state.samples.iter_mut().zip(current_values.iter()) {
        let Some(current) = *value else {
            // Couldn't read this engine's counter this cycle. Leave
            // its previous percentage alone and skip updating its
            // baseline — the next successful read will compute a
            // delta from the older baseline, slightly diluted by the
            // missed interval. That is preferable to crediting a
            // single counter reset as a giant jump.
            continue;
        };
        let delta_busy = current.saturating_sub(sample.last_busy_ns);
        let pct = ((delta_busy as f64) / delta_wall_f * 100.0).clamp(0.0, 100.0);
        sample.last_busy_pct = pct;
        sample.last_busy_ns = current;
    }
    state.last_tick = Some(now);

    // Aggregate per class — take the max across instances of the same
    // class so multiple equal-class engines do not get summed past
    // 100%.
    let mut per_class: Vec<(&'static str, f64)> = Vec::new();
    for sample in &state.samples {
        let class = sample.counter.class;
        if let Some(entry) = per_class.iter_mut().find(|(c, _)| *c == class) {
            if sample.last_busy_pct > entry.1 {
                entry.1 = sample.last_busy_pct;
            }
        } else {
            per_class.push((class, sample.last_busy_pct));
        }
    }

    // Pick primary utilization: prefer the busier of render/compute,
    // else fall back to the max across all classes.
    let render_or_compute = per_class
        .iter()
        .filter(|(c, _)| *c == "render" || *c == "compute")
        .map(|(_, pct)| *pct)
        .fold(f64::NEG_INFINITY, f64::max);
    let primary = if render_or_compute.is_finite() {
        render_or_compute
    } else {
        per_class
            .iter()
            .map(|(_, pct)| *pct)
            .fold(0.0_f64, f64::max)
    };

    // Sort the per-class report so the detail map keys remain
    // deterministic across refreshes — render and compute first, then
    // the rest alphabetically.
    per_class.sort_by(|a, b| class_order(a.0).cmp(&class_order(b.0)).then(a.0.cmp(b.0)));

    EngineReadout {
        primary_utilization: primary,
        per_class,
        status_note: None,
    }
}

/// Reader-facing convenience: lock the per-card mutex (recovering on
/// poisoning), drive one [`refresh`], and return the readout. Used by
/// [`crate::device::readers::intel_gpu_linux::IntelGpuReader`] to
/// integrate engine-busy data into its `GpuInfo` output.
///
/// Poisoning recovery mirrors the AMD reader's `VramUsage` pattern: we
/// log a warning and replace the protected state with a fresh empty
/// [`EngineState`] (which will re-walk sysfs on the next call). The
/// reader continues serving the rest of the GPU metrics so a panic in
/// some other thread cannot take down the whole telemetry pipeline.
pub fn refresh_with_lock(state: &Mutex<EngineState>, device_dir: &Path) -> EngineReadout {
    let mut guard = match state.lock() {
        Ok(g) => g,
        Err(poisoned) => {
            eprintln!(
                "Warning: Intel GPU engine-state mutex was poisoned for {}, recovering...",
                device_dir.display()
            );
            // `into_inner()` is documented to panic only when the
            // mutex itself is in an inconsistent state — extremely
            // rare with modern std but worth defending against.
            // `catch_unwind` keeps a faulty mutex from crashing the
            // entire collector.
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| poisoned.into_inner())) {
                Ok(mut g) => {
                    *g = EngineState::empty();
                    g
                }
                Err(_) => {
                    eprintln!(
                        "Critical: failed to recover engine-state mutex for {}, returning unavailable",
                        device_dir.display()
                    );
                    return EngineReadout {
                        primary_utilization: 0.0,
                        per_class: Vec::new(),
                        status_note: Some(ENGINE_UNAVAILABLE_NOTE),
                    };
                }
            }
        }
    };
    refresh(&mut guard, device_dir)
}

/// Fold an [`EngineReadout`] into a `detail` map. Adds one
/// `"Engine: <class>"` entry per known engine class and, when needed,
/// an explanatory `"Utilization"` note. Idempotent for any given
/// readout — the keys are deterministic given the discovered counter
/// set.
pub fn apply_engine_readout(detail: &mut HashMap<String, String>, readout: &EngineReadout) {
    if let Some(note) = readout.status_note {
        detail.insert("Utilization".to_string(), note.to_string());
    } else {
        // Engine data is live for this refresh — no static note.
        detail.remove("Utilization");
    }
    for (class, pct) in &readout.per_class {
        let key = format!("Engine: {class}");
        detail.insert(key, format!("{pct:.2}%"));
    }
}

/// Stable ordering used when laying out the `detail` map: render first,
/// then compute, then everything else alphabetically.
fn class_order(class: &str) -> u8 {
    match class {
        "render" => 0,
        "compute" => 1,
        _ => 2,
    }
}
