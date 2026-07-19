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

//! Xe GT idle-residency utilization fallback.
//!
//! Xe does not expose the i915-style engine-busy sysfs counters on every
//! kernel. In that case, the time a GT spends outside its deepest idle state
//! is used as an activity estimate. It is deliberately treated as a fallback:
//! active residency is not the same as engine busy time and must not replace a
//! valid zero-percent engine-busy sample.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::time::Instant;

use crate::device::readers::intel_gpu_engine::{ENGINE_UNAVAILABLE_NOTE, EngineReadout};
use crate::device::readers::intel_gpu_sysfs::read_gtidle_ms;

const GTIDLE_SEEDING_NOTE: &str =
    "Xe GT idle residency seeded (utilization available next refresh)";

#[derive(Debug, Clone, Copy, PartialEq)]
enum GtidleReadout {
    Unavailable,
    Seeded,
    Available(f64),
}

#[derive(Debug, Clone, Copy)]
struct GtidleSample {
    idle_ms: u64,
    tick: Instant,
}

#[derive(Debug, Default)]
pub struct GtidleState {
    samples: [Option<GtidleSample>; 2],
}

/// Replace an unavailable Xe engine-counter readout with GT active residency.
/// Valid engine-counter data, including a true zero-percent sample, always wins.
pub fn apply_fallback(
    driver: &str,
    engine_readout: &EngineReadout,
    state: &Mutex<GtidleState>,
    device_dir: &Path,
    detail: &mut HashMap<String, String>,
    utilization: &mut f64,
) {
    if driver != "xe" || engine_readout.status_note != Some(ENGINE_UNAVAILABLE_NOTE) {
        return;
    }

    match refresh_with_lock(state, device_dir) {
        GtidleReadout::Available(value) => {
            *utilization = value;
            detail.remove("Utilization");
            detail.insert(
                "Source: Utilization".to_string(),
                "Xe GT idle residency".to_string(),
            );
        }
        GtidleReadout::Seeded => {
            detail.insert("Utilization".to_string(), GTIDLE_SEEDING_NOTE.to_string());
            detail.insert(
                "Source: Utilization".to_string(),
                "Xe GT idle residency (seeded)".to_string(),
            );
        }
        GtidleReadout::Unavailable => {}
    }
}

impl GtidleState {
    pub fn empty() -> Self {
        Self::default()
    }
}

fn refresh_with_lock(state: &Mutex<GtidleState>, device_dir: &Path) -> GtidleReadout {
    let mut guard = match state.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            eprintln!(
                "Warning: Intel GPU gtidle-state mutex was poisoned for {}, recovering...",
                device_dir.display()
            );
            let mut guard = poisoned.into_inner();
            *guard = GtidleState::empty();
            guard
        }
    };
    refresh_at(&mut guard, device_dir, Instant::now())
}

fn refresh_at(state: &mut GtidleState, device_dir: &Path, now: Instant) -> GtidleReadout {
    let readings = read_gtidle_ms(device_dir);
    if readings.iter().all(Option::is_none) {
        return GtidleReadout::Unavailable;
    }

    let mut max_utilization = 0.0_f64;
    let mut has_delta = false;
    let mut seeded = false;

    for (sample, reading) in state.samples.iter_mut().zip(readings) {
        let Some(current_idle_ms) = reading else {
            // Keep the old sample: if the file becomes readable again, its
            // delta must cover the entire interval in which it was missing.
            continue;
        };

        let Some(previous) = *sample else {
            *sample = Some(GtidleSample {
                idle_ms: current_idle_ms,
                tick: now,
            });
            seeded = true;
            continue;
        };

        if current_idle_ms < previous.idle_ms {
            // Driver reloads and device resets can reset the monotonic
            // counter. Treat this sample as a new baseline instead of
            // turning a zero saturating delta into a false 100% result.
            *sample = Some(GtidleSample {
                idle_ms: current_idle_ms,
                tick: now,
            });
            seeded = true;
            continue;
        }

        let elapsed_us = now.saturating_duration_since(previous.tick).as_micros();
        if elapsed_us == 0 {
            continue;
        }

        let idle_us = u128::from(current_idle_ms - previous.idle_ms) * 1_000;
        let idle_fraction = idle_us as f64 / elapsed_us as f64;
        let utilization = ((1.0 - idle_fraction) * 100.0).clamp(0.0, 100.0);
        max_utilization = max_utilization.max(utilization);
        has_delta = true;
        *sample = Some(GtidleSample {
            idle_ms: current_idle_ms,
            tick: now,
        });
    }

    if has_delta {
        GtidleReadout::Available(max_utilization)
    } else if seeded {
        GtidleReadout::Seeded
    } else {
        GtidleReadout::Unavailable
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::Duration;
    use tempfile::tempdir;

    fn write_gt(device_dir: &Path, gt: usize, value: u64) {
        let dir = device_dir
            .join("tile0")
            .join(format!("gt{gt}"))
            .join("gtidle");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("idle_residency_ms"), value.to_string()).unwrap();
    }

    #[test]
    fn computes_active_residency_after_seeding() {
        let dir = tempdir().unwrap();
        write_gt(dir.path(), 0, 1_000);
        let start = Instant::now();
        let mut state = GtidleState::empty();
        assert_eq!(
            refresh_at(&mut state, dir.path(), start),
            GtidleReadout::Seeded
        );

        write_gt(dir.path(), 0, 1_500);
        let result = refresh_at(&mut state, dir.path(), start + Duration::from_secs(1));
        assert_eq!(result, GtidleReadout::Available(50.0));
    }

    #[test]
    fn zero_percent_is_available_data() {
        let dir = tempdir().unwrap();
        write_gt(dir.path(), 0, 10);
        let start = Instant::now();
        let mut state = GtidleState::empty();
        assert_eq!(
            refresh_at(&mut state, dir.path(), start),
            GtidleReadout::Seeded
        );

        write_gt(dir.path(), 0, 1_010);
        let result = refresh_at(&mut state, dir.path(), start + Duration::from_secs(1));
        assert_eq!(result, GtidleReadout::Available(0.0));
    }

    #[test]
    fn counter_reset_reseeds_instead_of_reporting_full_utilization() {
        let dir = tempdir().unwrap();
        write_gt(dir.path(), 0, 10_000);
        let start = Instant::now();
        let mut state = GtidleState::empty();
        assert_eq!(
            refresh_at(&mut state, dir.path(), start),
            GtidleReadout::Seeded
        );

        write_gt(dir.path(), 0, 5);
        let result = refresh_at(&mut state, dir.path(), start + Duration::from_secs(1));
        assert_eq!(result, GtidleReadout::Seeded);
    }

    #[test]
    fn missing_gt_does_not_shift_another_gts_baseline() {
        let dir = tempdir().unwrap();
        write_gt(dir.path(), 1, 100);
        let start = Instant::now();
        let mut state = GtidleState::empty();
        assert_eq!(
            refresh_at(&mut state, dir.path(), start),
            GtidleReadout::Seeded
        );

        write_gt(dir.path(), 0, 10_000);
        write_gt(dir.path(), 1, 600);
        let result = refresh_at(&mut state, dir.path(), start + Duration::from_secs(1));
        assert_eq!(result, GtidleReadout::Available(50.0));
    }

    fn engine_readout(status_note: Option<&'static str>) -> EngineReadout {
        EngineReadout {
            primary_utilization: 0.0,
            per_class: if status_note.is_none() {
                vec![("render", 0.0)]
            } else {
                Vec::new()
            },
            status_note,
        }
    }

    #[test]
    fn valid_zero_engine_sample_is_not_replaced() {
        let dir = tempdir().unwrap();
        write_gt(dir.path(), 0, 0);
        let state = Mutex::new(GtidleState::empty());
        let mut detail = HashMap::from([(
            "Source: Utilization".to_string(),
            "DRM engine counters".to_string(),
        )]);
        let mut utilization = 0.0;

        apply_fallback(
            "xe",
            &engine_readout(None),
            &state,
            dir.path(),
            &mut detail,
            &mut utilization,
        );

        assert_eq!(utilization, 0.0);
        assert_eq!(
            detail.get("Source: Utilization").map(String::as_str),
            Some("DRM engine counters")
        );
        assert!(state.lock().unwrap().samples.iter().all(Option::is_none));
    }

    #[test]
    fn available_zero_gtidle_sample_is_labeled_as_live_data() {
        let dir = tempdir().unwrap();
        write_gt(dir.path(), 0, 1_000);
        let start = Instant::now() - Duration::from_secs(1);
        let mut seeded_state = GtidleState::empty();
        assert_eq!(
            refresh_at(&mut seeded_state, dir.path(), start),
            GtidleReadout::Seeded
        );
        // More idle time than wall time clamps to a valid zero-percent
        // activity sample, which must not be confused with unavailable data.
        write_gt(dir.path(), 0, 3_000);

        let state = Mutex::new(seeded_state);
        let mut detail = HashMap::from([
            (
                "Utilization".to_string(),
                ENGINE_UNAVAILABLE_NOTE.to_string(),
            ),
            ("Source: Utilization".to_string(), "unavailable".to_string()),
        ]);
        let mut utilization = 0.0;
        apply_fallback(
            "xe",
            &engine_readout(Some(ENGINE_UNAVAILABLE_NOTE)),
            &state,
            dir.path(),
            &mut detail,
            &mut utilization,
        );

        assert_eq!(utilization, 0.0);
        assert!(!detail.contains_key("Utilization"));
        assert_eq!(
            detail.get("Source: Utilization").map(String::as_str),
            Some("Xe GT idle residency")
        );
    }
}
