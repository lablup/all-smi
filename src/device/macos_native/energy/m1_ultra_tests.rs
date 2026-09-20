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

//! Energy Model tests for an Apple M1 Ultra (issue #415), next to the M5 Max
//! ones in `tests.rs`.
//!
//! The inventory is a verbatim capture from a Mac13,2 on macOS 27.0 (26A428),
//! and every energy, span, and timestamp here comes from the two recorded
//! runs of `ioreport_energy_diagnostics` on that machine: one with all-smi
//! idle and one under four `yes` loads. They pin the multi-die rules and the
//! publication timing to what that hardware does.

use super::tests::{MS, assert_steady, assert_watts, counts, inventory, ms, obs, poll};
use super::*;

/// `Energy Model` inventory of an Apple M1 Ultra (Mac13,2) on macOS 27.0.
const M1_ULTRA_ENERGY_MODEL: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/ioreport/m1_ultra_energy_model.tsv"
));

/// Spans between successive publications of the mJ channels (all of them
/// publish together), in the driver's own timestamps (ms). Two publications
/// per ~2.1 s at uneven, drifting intervals; the spans under 50 ms are split
/// tails.
const M1_ULTRA_IDLE_SPANS_MS: [f64; 32] = [
    1260.3, 816.7, 1293.0, 818.5, 1295.4, 814.8, 1247.0, 784.2, 19.9, 1304.0, 804.3, 1304.7, 788.6,
    1319.4, 791.2, 1301.8, 745.5, 1294.4, 813.7, 1277.0, 12.2, 815.6, 1268.9, 841.8, 1260.9, 854.8,
    1242.0, 863.0, 1193.7, 908.3, 1196.9, 874.5,
];
const M1_ULTRA_LOADED_SPANS_MS: [f64; 35] = [
    1689.8, 461.0, 1622.1, 416.1, 26.3, 1613.7, 497.2, 1612.4, 575.3, 1548.8, 575.1, 1519.0, 662.8,
    1401.0, 732.2, 1305.7, 17.6, 780.1, 1321.1, 864.6, 1297.5, 868.3, 1210.0, 28.6, 944.8, 1095.9,
    1006.2, 24.0, 1027.8, 1070.5, 950.2, 26.2, 1208.5, 967.6, 1140.4,
];

/// Watts per channel over the common window of the loaded run. Every
/// channel no rule sums draws 0.5 W, so that summing any of them shows up.
fn loaded_window_watts(name: &str) -> f64 {
    match name {
        "DIE_0_CPU Energy" => 14.661,
        "DIE_1_CPU Energy" => 2.351,
        "GPU Energy" => 0.081,
        "GPU0_0" => 0.076,
        "GPU SRAM0_0" => 0.002,
        "ANE0_0" => 0.038,
        "ANE0_1" => 0.010,
        "DRAM0_0" => 2.000,
        "DRAM0_1" => 1.972,
        _ => 0.5,
    }
}

#[test]
fn m1_ultra_inventory_classifies_only_the_rail_channels() {
    let channels = inventory(M1_ULTRA_ENERGY_MODEL);
    assert_eq!(channels.len(), 321);
    for (unit, expected) in [("mJ", 310), ("uJ", 10), ("nJ", 1)] {
        assert_eq!(
            channels.iter().filter(|(_, u)| *u == unit).count(),
            expected,
            "{unit}"
        );
    }

    let classified: Vec<(&str, EnergyRail)> = channels
        .iter()
        .filter_map(|(name, _)| classify_energy_channel(name).map(|rail| (*name, rail)))
        .collect();
    assert_eq!(
        classified,
        vec![
            ("DIE_0_CPU Energy", EnergyRail::Cpu),
            ("DIE_1_CPU Energy", EnergyRail::Cpu),
            ("GPU0_0", EnergyRail::GpuFallback),
            ("ANE0_0", EnergyRail::Ane),
            ("DRAM0_0", EnergyRail::Dram),
            ("ANE0_1", EnergyRail::Ane),
            ("DRAM0_1", EnergyRail::Dram),
            ("GPU Energy", EnergyRail::Gpu),
        ],
        "the other 313 channels must classify to None"
    );
}

/// The `DIE_<n>_` decision in the module docs, pinned to the inventory it
/// rests on.
#[test]
fn m1_ultra_die_prefix_is_cpu_only_and_no_package_channel_sits_beside_the_dies() {
    let channels = inventory(M1_ULTRA_ENERGY_MODEL);
    let has = |name: &str| channels.iter().any(|(n, _)| *n == name);

    // Only CPU channels carry the prefix (the `EACC`/`PACC` clusters, their
    // cores and `_CPM`, and the per-die roll-up), and of those only the
    // roll-up feeds a rail.
    let prefixed: Vec<&str> = channels
        .iter()
        .map(|(name, _)| *name)
        .filter(|name| name.starts_with("DIE_"))
        .collect();
    assert_eq!(prefixed.len(), 34);
    for name in &prefixed {
        let block = name
            .strip_prefix("DIE_0_")
            .or_else(|| name.strip_prefix("DIE_1_"))
            .expect("an M1 Ultra has dies 0 and 1");
        assert!(
            block.starts_with("EACC_") || block.starts_with("PACC") || block == "CPU Energy",
            "{name} is not a CPU channel"
        );
    }
    let rails: Vec<&str> = prefixed
        .iter()
        .copied()
        .filter(|name| classify_energy_channel(name).is_some())
        .collect();
    assert_eq!(rails, ["DIE_0_CPU Energy", "DIE_1_CPU Energy"]);

    // Every other block names its die with a suffix, and no package channel
    // sits beside the per-die ones, so the per-die sum is the whole rail.
    for block in ["ANE0", "DRAM0", "ISP0", "AVE0", "MSR0", "DCS0", "AMCC0"] {
        assert!(
            has(&format!("{block}_0")) && has(&format!("{block}_1")),
            "{block}"
        );
    }
    for package in ["CPU Energy", "ANE", "ANE0", "DRAM", "DRAM0", "GPU", "GPU0"] {
        assert!(!has(package), "{package} would be counted next to the dies");
    }

    // A `DIE_<n>_` GPU, ANE, or DRAM channel matches no rule until a chip
    // that has one is recorded.
    for name in [
        "DIE_0_GPU Energy",
        "DIE_0_GPU0",
        "DIE_1_GPU0_0",
        "DIE_0_ANE",
        "DIE_1_ANE0",
        "DIE_0_DRAM",
        "DIE_1_DRAM0_1",
    ] {
        assert_eq!(classify_energy_channel(name), None, "{name}");
    }
}

/// `EnergyTracker` keys channels by name, which is safe only while no
/// duplicated name classifies. This inventory lists 130 names twice.
#[test]
fn m1_ultra_duplicated_names_are_all_unclassified_telemetry() {
    let channels = inventory(M1_ULTRA_ENERGY_MODEL);
    let mut seen: HashMap<&str, usize> = HashMap::new();
    for (name, _) in &channels {
        *seen.entry(name).or_default() += 1;
    }
    let duplicated: Vec<(&str, usize)> = seen.into_iter().filter(|(_, count)| *count > 1).collect();
    assert_eq!(duplicated.len(), 130);
    for (name, count) in duplicated {
        assert_eq!(count, 2, "{name}");
        assert!(name.contains("DTL"), "{name}");
        assert_eq!(classify_energy_channel(name), None, "{name}");
    }
}

#[test]
fn gpu_fallback_accepts_the_die_suffix_and_nothing_else() {
    for (name, rail) in [
        ("GPU0_0", Some(EnergyRail::GpuFallback)),
        ("GPU1_1", Some(EnergyRail::GpuFallback)),
        ("GPU0", Some(EnergyRail::GpuFallback)),
        ("GPU Energy", Some(EnergyRail::Gpu)),
        ("GPU", None),
        ("GPUPH", None),
        ("GPU SRAM0_0", None),
        ("GPU0_SRAM", None),
        ("GPU0_0_1", None),
        ("GPU0_", None),
        ("GPU_0", None),
        // The uJ channels end in ` Energy` but are not rails.
        ("apciec0 Energy", None),
        ("PCIe Port 0 Energy", None),
    ] {
        assert_eq!(classify_energy_channel(name), rail, "{name:?}");
    }
}

/// The loaded run through the tracker: CPU is the two `DIE_<n>_CPU Energy`
/// channels, GPU is `GPU Energy` once (with `GPU0_0` only as its fallback),
/// ANE and DRAM are summed per die, and nothing else is summed.
#[test]
fn m1_ultra_loaded_window_sums_the_dies_and_counts_the_gpu_once() {
    let channels = inventory(M1_ULTRA_ENERGY_MODEL);
    let t0 = 10_000 * MS;
    let t1 = t0 + 2_000 * MS;
    let mut tracker = EnergyTracker::default();
    tracker.observe_sample(
        t0 + MS,
        channels
            .iter()
            .map(|(name, unit)| obs(name, unit, 0, Some(t0))),
    );
    let at = |secs: f64, ts: u64| {
        channels.iter().map(move |(name, unit)| {
            let value = counts(loaded_window_watts(name), unit, secs);
            obs(name, unit, value, Some(ts))
        })
    };
    tracker.observe_sample(t1 + MS, at(2.0, t1));

    let readings = tracker.readings();
    assert_watts(readings.cpu, 14.661 + 2.351);
    assert_watts(readings.gpu, 0.081);
    assert_watts(readings.ane, 0.038 + 0.010);
    assert_watts(readings.dram, 2.000 + 1.972);
    assert_watts(readings.package(), 14.661 + 2.351 + 0.081 + 0.048);

    // A sample without `GPU Energy` falls back to `GPU0_0`, and only to it.
    let t2 = t1 + 2_000 * MS;
    tracker.observe_sample(
        t2 + MS,
        at(4.0, t2).filter(|obs| obs.channel != "GPU Energy"),
    );
    assert_watts(tracker.readings().gpu, 0.076);
    assert_watts(tracker.readings().cpu, 14.661 + 2.351);
}

/// Every CPU publication in the first 60 ticks of the loaded run, replayed:
/// `(DIE_0 mJ, DIE_1 mJ, span ms)` and the CPU rail the diagnostic printed
/// after it. The 26.3 ms publication is a split tail: the reading holds and
/// the tail folds into the next span.
#[test]
fn m1_ultra_loaded_cpu_rail_is_the_die_sum_of_each_publication() {
    const PUBLICATIONS: [(i64, i64, f64, f64); 8] = [
        (25_679, 9_851, 1689.8, 21.03),
        (6_808, 1_837, 461.0, 18.75),
        (24_078, 6_609, 1622.1, 18.92),
        (6_260, 1_190, 416.1, 17.90),
        (392, 53, 26.3, 17.90),
        (24_150, 5_755, 1613.7, 18.51),
        (7_497, 2_627, 497.2, 20.36),
        (24_610, 5_128, 1612.4, 18.44),
    ];
    let die =
        |n: u8, value: i64, ts: u64| obs(&format!("DIE_{n}_CPU Energy"), "mJ", value, Some(ts));
    let mut ts = 1_000 * MS;
    let (mut die0, mut die1) = (0, 0);
    let mut tracker = EnergyTracker::default();
    tracker.observe_sample(ts + MS, [die(0, die0, ts), die(1, die1, ts)]);

    let mut held_since: Option<(i64, f64)> = None;
    for (d0, d1, span, printed) in PUBLICATIONS {
        ts += ms(span);
        die0 += d0;
        die1 += d1;
        tracker.observe_sample(ts + 5 * MS, [die(0, die0, ts), die(1, die1, ts)]);
        let cpu = tracker.readings().cpu;

        let (energy, window) = match held_since.take() {
            Some((tail_mj, tail_ms)) => (d0 + d1 + tail_mj, span + tail_ms),
            None => (d0 + d1, span),
        };
        if span < 50.0 {
            held_since = Some((d0 + d1, span));
        } else {
            assert_watts(cpu, energy as f64 / window);
        }
        assert!(
            (cpu - printed).abs() < 0.005,
            "span {span} ms: tracker {cpu} W, diagnostic printed {printed} W"
        );
    }
}

/// `(timestamp_ns, cumulative mJ)` for a channel drawing a constant `watts`,
/// published at `spans_ms`.
fn publications_at(spans_ms: &[f64], watts: f64) -> Vec<(u64, i64)> {
    let start = 1_000 * MS;
    let mut at = start;
    let mut out = vec![(start, 0)];
    for span in spans_ms {
        at += ms(*span);
        out.push((at, counts(watts, "mJ", (at - start) as f64 / 1e9)));
    }
    out
}

#[test]
fn m1_ultra_cadence_reads_constant_load_without_zero_ticks() {
    for spans in [&M1_ULTRA_IDLE_SPANS_MS[..], &M1_ULTRA_LOADED_SPANS_MS[..]] {
        let publications = publications_at(spans, 17.0);
        let (one_second, _) = poll(&publications, 1_000);
        assert_steady(&one_second, 17.0, 30);
        let (three_second, _) = poll(&publications, 3_000);
        assert_steady(&three_second, 17.0, 10);
    }
}

/// `GPU Energy` at an idle GPU, ticks 019 to 023 of the idle run: a
/// publication carrying 0 nJ over a full span is 0 W, as on an M5 Max. A
/// sample that lands before the next publication sees the stamp move only
/// 12.2 ms with no energy (stamped 116.1 ms before the sample): that is not
/// 0 W but no publication yet, so the reading holds and the next span covers
/// both. A fully frozen stamp holds the same way
/// (`reading_is_held_until_the_next_publication`).
#[test]
fn m1_ultra_gpu_energy_zero_is_zero_only_over_a_full_span() {
    let gpu = |value: i64, ts: u64| obs("GPU Energy", "nJ", value, Some(ts));
    // (nJ published, span ms, age ms at the sample)
    let ticks: [(i64, f64, f64); 5] = [
        (0, 125.5, 0.4),
        (3_585_260, 126.0, 0.5),
        (0, 12.2, 116.1),
        (19_421_180, 245.8, 0.4),
        (11_204_410, 127.4, 0.5),
    ];
    let mut ts = 5_000 * MS;
    let mut value = 1_000_000_000;
    let mut tracker = EnergyTracker::default();
    tracker.observe_sample(ts + ms(0.3), [gpu(value, ts)]);

    let mut readings = vec![];
    for (energy, span, age) in ticks {
        ts += ms(span);
        value += energy;
        tracker.observe_sample(ts + ms(age), [gpu(value, ts)]);
        readings.push(tracker.readings().gpu);
    }

    let first = 3_585_260e-9 / 0.126;
    assert_watts(readings[0], 0.0);
    assert_watts(readings[1], first);
    assert_watts(readings[2], first);
    assert_watts(readings[3], 19_421_180e-9 / (0.0122 + 0.2458));
    assert_watts(readings[4], 11_204_410e-9 / 0.1274);
}
