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

//! Tests for the guards in `sum_rails` that keep a per-die sum from being
//! added to a package total (issue #415).
//!
//! No recorded chip exercises them: the M5 Max has only the package channels
//! and the M1 Ultra only the per-die ones, on every rail (CPU, the GPU
//! fallback, ANE, and DRAM). The channel sets here are the
//! combinations the matching rules could meet on an unrecorded multi-die chip,
//! with deliberately different watts per source so the readings show which
//! source fed the rail.

use super::tests::{MS, assert_watts, counts, obs};
use super::*;

/// Every order of `items`.
fn orders<T: Copy>(items: &[T]) -> Vec<Vec<T>> {
    if items.len() <= 1 {
        return vec![items.to_vec()];
    }
    let mut out = Vec::new();
    for i in 0..items.len() {
        let mut rest = items.to_vec();
        let first = rest.remove(i);
        for mut tail in orders(&rest) {
            tail.insert(0, first);
            out.push(tail);
        }
    }
    out
}

/// Every channel the guards choose between still classifies, so the
/// subscription filter keeps it and the tracker follows it; the choice is
/// made only when the rails are summed.
#[test]
fn guarded_channels_are_all_still_tracked() {
    for name in [
        "CPU Energy",
        "DIE_0_CPU Energy",
        "DIE_1_CPU Energy",
        "GPU0",
        "GPU0_0",
        "GPU0_1",
    ] {
        assert!(classify_energy_channel(name).is_some(), "{name}");
    }
}

#[test]
fn package_cpu_energy_alone_feeds_the_cpu_rail() {
    // In any order, the package channel wins over the per-die ones.
    for channels in [
        [
            ("CPU Energy", 30.0),
            ("DIE_0_CPU Energy", 14.0),
            ("DIE_1_CPU Energy", 16.5),
        ],
        [
            ("DIE_0_CPU Energy", 14.0),
            ("CPU Energy", 30.0),
            ("DIE_1_CPU Energy", 16.5),
        ],
        [
            ("DIE_0_CPU Energy", 14.0),
            ("DIE_1_CPU Energy", 16.5),
            ("CPU Energy", 30.0),
        ],
    ] {
        assert_watts(sum_rails(channels).cpu, 30.0);
    }

    // Without it the dies are summed, as on an M1 Ultra.
    let dies = sum_rails([("DIE_0_CPU Energy", 14.0), ("DIE_1_CPU Energy", 16.5)]);
    assert_watts(dies.cpu, 30.5);

    // A package channel reading 0 W still wins, as an idle `GPU Energy` does.
    let idle = sum_rails([("CPU Energy", 0.0), ("DIE_0_CPU Energy", 14.0)]);
    assert_watts(idle.cpu, 0.0);
}

#[test]
fn gpu_fallback_prefers_gpu_n_over_gpu_n_m() {
    // In any order, `GPU<n>` wins over `GPU<n>_<m>` when there is no
    // `GPU Energy`.
    for channels in [
        [("GPU0", 10.0), ("GPU0_0", 9.7), ("GPU0_1", 0.2)],
        [("GPU0_0", 9.7), ("GPU0", 10.0), ("GPU0_1", 0.2)],
        [("GPU0_0", 9.7), ("GPU0_1", 0.2), ("GPU0", 10.0)],
    ] {
        assert_watts(sum_rails(channels).gpu, 10.0);
    }

    // Several `GPU<n>` are summed among themselves.
    let two = sum_rails([("GPU0", 4.0), ("GPU1", 5.0), ("GPU0_0", 9.7)]);
    assert_watts(two.gpu, 9.0);

    // Without `GPU<n>`, the `GPU<n>_<m>` channels are summed, one per die.
    let per_die = sum_rails([("GPU0_0", 9.7), ("GPU0_1", 0.2)]);
    assert_watts(per_die.gpu, 9.9);

    // `GPU Energy` still wins over both families.
    let all = sum_rails([("GPU0", 10.0), ("GPU0_0", 9.7), ("GPU Energy", 10.1)]);
    assert_watts(all.gpu, 10.1);
}

/// Through the tracker the choice follows the latest sample: once the
/// package or `GPU<n>` channel is missing from it, the other source feeds
/// the rail.
#[test]
fn guards_follow_the_latest_sample_through_the_tracker() {
    let mj = |name: &str, value: i64, ts: u64| obs(name, "mJ", value, Some(ts));
    let t0 = 1_000 * MS;
    let t1 = t0 + 2_000 * MS;
    let t2 = t1 + 2_000 * MS;
    let mut tracker = EnergyTracker::default();

    tracker.observe_sample(
        t0 + MS,
        [
            mj("CPU Energy", 0, t0),
            mj("DIE_0_CPU Energy", 0, t0),
            mj("DIE_1_CPU Energy", 0, t0),
            mj("GPU0", 0, t0),
            mj("GPU0_0", 0, t0),
        ],
    );
    // 2 s at 30 W package, 14 W and 16.5 W per die, 10 W `GPU0`, 9.7 W
    // `GPU0_0`.
    tracker.observe_sample(
        t1 + MS,
        [
            mj("CPU Energy", 60_000, t1),
            mj("DIE_0_CPU Energy", 28_000, t1),
            mj("DIE_1_CPU Energy", 33_000, t1),
            mj("GPU0", 20_000, t1),
            mj("GPU0_0", 19_400, t1),
        ],
    );
    let both = tracker.readings();
    assert_watts(both.cpu, 30.0);
    assert_watts(both.gpu, 10.0);

    // The package and `GPU<n>` channels drop out of the next sample.
    tracker.observe_sample(
        t2 + MS,
        [
            mj("DIE_0_CPU Energy", 56_000, t2),
            mj("DIE_1_CPU Energy", 66_000, t2),
            mj("GPU0_0", 38_800, t2),
        ],
    );
    let per_die = tracker.readings();
    assert_watts(per_die.cpu, 14.0 + 16.5);
    assert_watts(per_die.gpu, 9.7);
}

/// ANE and DRAM: bare `ANE` / `DRAM` wins, then `ANE<n>` / `DRAM<n>`, then
/// `ANE<n>_<m>` / `DRAM<n>_<m>`, whatever the order of the channels.
#[test]
fn ane_and_dram_count_only_the_least_specific_family() {
    for prefix in ["ANE", "DRAM"] {
        let names = [
            prefix.to_string(),
            format!("{prefix}0"),
            format!("{prefix}1"),
            format!("{prefix}0_0"),
            format!("{prefix}0_1"),
        ];
        for name in &names {
            assert!(classify_energy_channel(name).is_some(), "{name}");
        }
        let rail = |readings: EnergyReadings| match prefix {
            "ANE" => readings.ane,
            _ => readings.dram,
        };
        let other = |readings: EnergyReadings| match prefix {
            "ANE" => readings.dram,
            _ => readings.ane,
        };
        let all: Vec<(&str, f64)> = names
            .iter()
            .map(String::as_str)
            .zip([3.0, 2.0, 2.5, 1.0, 1.25])
            .collect();

        // Package present: it alone, in every order.
        for order in orders(&all) {
            let readings = sum_rails(order);
            assert_watts(rail(readings), 3.0);
            assert_watts(other(readings), 0.0);
        }
        // Package and per-die only: still the package.
        for order in orders(&[all[0], all[3], all[4]]) {
            assert_watts(rail(sum_rails(order)), 3.0);
        }
        // No package: the numbered channels, summed.
        for order in orders(&all[1..]) {
            assert_watts(rail(sum_rails(order)), 2.0 + 2.5);
        }
        // Only the per-die channels, summed, as on an M1 Ultra.
        for order in orders(&all[3..]) {
            assert_watts(rail(sum_rails(order)), 1.0 + 1.25);
        }
    }
}

/// Through the tracker the ANE and DRAM choice follows the latest sample.
#[test]
fn ane_and_dram_guards_follow_the_latest_sample_through_the_tracker() {
    let watts = [
        ("ANE", 3.0),
        ("ANE0", 2.0),
        ("ANE0_0", 1.0),
        ("DRAM0", 4.0),
        ("DRAM0_0", 1.5),
        ("DRAM0_1", 2.0),
    ];
    let t0 = 1_000 * MS;
    // Step `step` of 2 s each, with only the `present` channels in it.
    let sample = |step: u64, present: &[&str]| -> Vec<EnergyObservation> {
        let ts = t0 + step * 2_000 * MS;
        watts
            .iter()
            .filter(|(name, _)| present.contains(name))
            .map(|(name, w)| obs(name, "mJ", counts(*w, "mJ", 2.0 * step as f64), Some(ts)))
            .collect()
    };
    let every = ["ANE", "ANE0", "ANE0_0", "DRAM0", "DRAM0_0", "DRAM0_1"];
    let mut tracker = EnergyTracker::default();

    tracker.observe_sample(t0 + MS, sample(0, &every));
    tracker.observe_sample(t0 + 2_000 * MS + MS, sample(1, &every));
    assert_watts(tracker.readings().ane, 3.0);
    assert_watts(tracker.readings().dram, 4.0);

    // `ANE` and `DRAM0` drop out.
    let without_package = ["ANE0", "ANE0_0", "DRAM0_0", "DRAM0_1"];
    tracker.observe_sample(t0 + 4_000 * MS + MS, sample(2, &without_package));
    assert_watts(tracker.readings().ane, 2.0);
    assert_watts(tracker.readings().dram, 1.5 + 2.0);

    // `ANE0` drops out too.
    tracker.observe_sample(
        t0 + 6_000 * MS + MS,
        sample(3, &["ANE0_0", "DRAM0_0", "DRAM0_1"]),
    );
    assert_watts(tracker.readings().ane, 1.0);
    assert_watts(tracker.readings().dram, 1.5 + 2.0);
}
