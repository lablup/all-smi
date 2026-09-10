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

//! Unit tests for Energy Model classification and the publication-timestamp
//! tracker (issue #410). Pulled out of `energy.rs` to keep that file under the
//! 500-line budget.
//!
//! The inventory is a verbatim M5 Max capture and the spans, energies, and
//! split publications are the ones measured on that machine, so these tests
//! pin the rules to the hardware behavior that motivated them.

use super::*;

/// `Energy Model` inventory of an Apple M5 Max (Mac17,7) on macOS 27.0.
const M5_MAX_ENERGY_MODEL: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/ioreport/m5_max_energy_model.tsv"
));

/// Spans between successive batch publications on an M5 Max under load, in
/// the driver's own timestamps (ms).
const M5_MAX_PUBLICATION_SPANS_MS: [f64; 19] = [
    2027.5, 2032.3, 2033.3, 2043.0, 2045.6, 2061.7, 2133.0, 2145.4, 2153.7, 2156.4, 2158.6, 2160.0,
    2160.7, 2162.5, 2168.7, 2175.4, 2176.3, 2182.6, 2228.1,
];

const MS: u64 = 1_000_000;

fn ms(value: f64) -> u64 {
    (value * 1e6).round() as u64
}

fn inventory(tsv: &str) -> Vec<(&str, &str)> {
    tsv.lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            line.split_once('\t')
                .expect("fixture lines are name<TAB>unit")
        })
        .collect()
}

fn obs(channel: &str, unit: &str, value: i64, timestamp_ns: Option<u64>) -> EnergyObservation {
    EnergyObservation {
        channel: channel.to_string(),
        unit: unit.to_string(),
        value,
        timestamp_ns,
    }
}

fn cpu(value: i64, ts: u64) -> EnergyObservation {
    obs("CPU Energy", "mJ", value, Some(ts))
}

/// Counter value for `watts` sustained over `secs`, in `unit`.
fn counts(watts: f64, unit: &str, secs: f64) -> i64 {
    (watts * secs / joules_per_count(unit)).round() as i64
}

fn assert_watts(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() < 1e-6,
        "expected {expected} W, got {actual} W"
    );
}

/// Per-channel watts reproducing the loaded batch in the issue: the
/// `CPU Energy` roll-up at 36.17 W, clusters summing to the same 36.17 W,
/// 300 `DTL` channels at 26.01 W, 12 per-core channels at 5.20 W, and 18
/// CPU `_SRAM` channels at 3.13 W. Every other block draws 0.5 W so that
/// summing any of them shows up.
fn loaded_batch_watts(name: &str) -> f64 {
    match name {
        "CPU Energy" => 36.17,
        "MCPU0" => 11.0,
        "MCPU1" => 11.5,
        "PCPU" => 13.67,
        "GPU Energy" | "GPU0" => 70.0,
        "ANE0" => 0.0,
        "DRAM0" => 1.9,
        _ if name.contains("DTL") => 26.01 / 300.0,
        _ if name.ends_with("_SRAM") && name.contains("CPU") => 3.13 / 18.0,
        _ if name.starts_with("MCPU") => 5.20 / 12.0,
        _ => 0.5,
    }
}

#[test]
fn m5_max_inventory_classifies_only_the_rail_channels() {
    let channels = inventory(M5_MAX_ENERGY_MODEL);
    assert_eq!(channels.len(), 364);
    for (unit, expected) in [("mJ", 358), ("uJ", 5), ("nJ", 1)] {
        assert_eq!(
            channels.iter().filter(|(_, u)| *u == unit).count(),
            expected
        );
    }

    let classified: Vec<(&str, EnergyRail)> = channels
        .iter()
        .filter_map(|(name, _)| classify_energy_channel(name).map(|rail| (*name, rail)))
        .collect();
    assert_eq!(
        classified,
        vec![
            ("CPU Energy", EnergyRail::Cpu),
            ("GPU0", EnergyRail::GpuFallback),
            ("ANE0", EnergyRail::Ane),
            ("DRAM0", EnergyRail::Dram),
            ("GPU Energy", EnergyRail::Gpu),
        ],
        "the other 359 channels must classify to None"
    );
}

#[test]
fn classification_covers_multi_die_and_basic_names() {
    for (name, rail) in [
        ("DIE_0_CPU Energy", Some(EnergyRail::Cpu)),
        ("DIE_1_CPU Energy", Some(EnergyRail::Cpu)),
        ("ANE", Some(EnergyRail::Ane)),
        ("ANE0_1", Some(EnergyRail::Ane)),
        ("DRAM", Some(EnergyRail::Dram)),
        ("DRAM1_0", Some(EnergyRail::Dram)),
        ("GPU12", Some(EnergyRail::GpuFallback)),
        // Sub-channels and look-alikes stay out of every rail.
        ("ANE0_SRAM", None),
        ("ANE0DTL00", None),
        ("ANE0_0_1", None),
        ("DRAM0_SRAM", None),
        ("GPU0_SRAM", None),
        ("GPU", None),
        ("GPUPH", None),
        ("MCPU0", None),
        ("PCPU0_SRAM", None),
        ("PCPUDTL00", None),
        ("", None),
    ] {
        assert_eq!(classify_energy_channel(name), rail, "{name:?}");
    }
}

#[test]
fn m5_max_loaded_batch_matches_the_roll_up() {
    let channels = inventory(M5_MAX_ENERGY_MODEL);

    // The fixture families reproduce the issue's table: the old substring
    // rule summed 334 channels to 106.68 W against a 36.17 W roll-up.
    let substring_rule = |name: &str| name.contains("CPU") && !name.contains("GPU");
    let old: Vec<f64> = channels
        .iter()
        .filter(|(name, _)| substring_rule(name))
        .map(|(name, _)| loaded_batch_watts(name))
        .collect();
    assert_eq!(old.len(), 334);
    assert!((old.iter().sum::<f64>() - 106.68).abs() < 1e-9);

    // Two samples 2 s apart through the tracker, every channel present.
    let t0 = 10_000 * MS;
    let t1 = t0 + 2_000 * MS;
    let mut tracker = EnergyTracker::default();
    tracker.observe_sample(
        t0 + MS,
        channels
            .iter()
            .map(|(name, unit)| obs(name, unit, 0, Some(t0))),
    );
    tracker.observe_sample(
        t1 + MS,
        channels.iter().map(|(name, unit)| {
            let value = counts(loaded_batch_watts(name), unit, 2.0);
            obs(name, unit, value, Some(t1))
        }),
    );

    let readings = tracker.readings();
    assert_watts(readings.cpu, 36.17);
    assert_watts(readings.gpu, 70.0);
    assert_watts(readings.ane, 0.0);
    assert_watts(readings.dram, 1.9);
    assert_watts(readings.package(), 36.17 + 70.0);
}

#[test]
fn gpu_is_counted_once_and_falls_back_to_gpu_n() {
    let both = sum_rails([("GPU0", 70.0), ("GPU Energy", 69.5), ("CPU Energy", 30.0)]);
    assert_watts(both.gpu, 69.5);

    let fallback = sum_rails([("GPU0", 70.0), ("CPU Energy", 30.0)]);
    assert_watts(fallback.gpu, 70.0);

    // An idle GPU Energy channel still wins: its 0 W is a real reading.
    let idle = sum_rails([("GPU0", 3.0), ("GPU Energy", 0.0)]);
    assert_watts(idle.gpu, 0.0);
}

#[test]
fn multi_die_rails_are_summed() {
    let readings = sum_rails([
        ("DIE_0_CPU Energy", 10.0),
        ("DIE_1_CPU Energy", 12.5),
        ("ANE0_0", 1.0),
        ("ANE0_1", 1.5),
        ("GPU Energy", 20.0),
        ("DRAM0", 4.0),
    ]);
    assert_watts(readings.cpu, 22.5);
    assert_watts(readings.ane, 2.5);
    assert_watts(readings.gpu, 20.0);
    assert_watts(readings.dram, 4.0);
    assert_watts(readings.package(), 45.0);
}

#[test]
fn first_sighting_has_no_reading_and_continuous_channels_read_next_sample() {
    // The first collection of a session: one sample from
    // `get_sample_since_last`, then `get_sample`'s two, 0.2 ms and 100 ms
    // later. `GPU Energy` is stamped at sample time; `CPU Energy` last
    // published 700 ms before the session started.
    let t0 = 5_000 * MS;
    let cpu_ts = t0 - 700 * MS;
    let gpu = |value: i64, ts: u64| obs("GPU Energy", "nJ", value, Some(ts));
    let mut tracker = EnergyTracker::default();

    tracker.observe_sample(t0, [gpu(0, t0), cpu(1_000, cpu_ts)]);
    assert_eq!(tracker.readings(), EnergyReadings::default());

    // Inside the 50 ms minimum: folded into the next span.
    let t_short = t0 + MS / 5;
    tracker.observe_sample(t_short, [gpu(16_000_000, t_short), cpu(1_000, cpu_ts)]);
    assert_eq!(tracker.readings(), EnergyReadings::default());

    let t2 = t0 + 100 * MS;
    tracker.observe_sample(t2, [gpu(8_200_000_000, t2), cpu(1_000, cpu_ts)]);
    let readings = tracker.readings();
    assert_watts(readings.gpu, 82.0);
    assert_watts(readings.cpu, 0.0);
}

#[test]
fn batch_landing_in_a_short_window_reads_exactly() {
    // A whole 2.05 s batch published inside a 100 ms window. Dividing it by
    // the window used to report 20x the real 35 W.
    let t0 = 5_000 * MS;
    let mut tracker = EnergyTracker::default();
    tracker.observe_sample(t0, [cpu(1_000, t0 - 2_000 * MS)]);
    tracker.observe_sample(t0 + 100 * MS, [cpu(1_000 + 71_750, t0 + 50 * MS)]);
    assert_watts(tracker.readings().cpu, 35.0);
}

#[test]
fn reading_is_held_until_the_next_publication() {
    let t0 = 1_000 * MS;
    let mut tracker = EnergyTracker::default();
    tracker.observe_sample(t0 + 10 * MS, [cpu(0, t0)]);

    let t1 = t0 + ms(2043.0);
    tracker.observe_sample(t1 + 10 * MS, [cpu(4_969, t1)]);
    let exact = 4.969 / 2.043;
    assert_watts(tracker.readings().cpu, exact);

    // Three polls with nothing published: the reading holds, never 0 W.
    for poll in 1..=3 {
        tracker.observe_sample(t1 + poll * 500 * MS, [cpu(4_969, t1)]);
        assert_watts(tracker.readings().cpu, exact);
    }
}

#[test]
fn split_publication_tail_folds_into_the_next_span() {
    // Measured: `CPU Energy` published 4969 mJ over 2043.0 ms, then a 65 mJ
    // tail 14.8 ms later. Read on its own the tail would be 4.4 W.
    let t0 = 1_000 * MS;
    let mut tracker = EnergyTracker::default();
    tracker.observe_sample(t0 + 10 * MS, [cpu(0, t0)]);

    let batch = t0 + ms(2043.0);
    tracker.observe_sample(batch + 5 * MS, [cpu(4_969, batch)]);
    let batch_watts = 4.969 / 2.043;
    assert_watts(tracker.readings().cpu, batch_watts);

    let tail = batch + ms(14.8);
    tracker.observe_sample(tail + 50 * MS, [cpu(4_969 + 65, tail)]);
    assert_watts(tracker.readings().cpu, batch_watts);

    // The next batch's span starts at the first batch, so the tail's energy
    // and its 14.8 ms both land in it.
    let next = batch + ms(2100.0);
    tracker.observe_sample(next + 5 * MS, [cpu(4_969 + 65 + 5_000, next)]);
    assert_watts(tracker.readings().cpu, 5.065 / 2.1);
}

#[test]
fn channel_published_a_sample_later_gets_its_own_window() {
    // Measured: `DRAM0` published 25 ms after `CPU Energy`, in the next
    // sample, with its own 294 mJ over 2158.5 ms.
    let t0 = 1_000 * MS;
    let dram = |value: i64, ts: u64| obs("DRAM0", "mJ", value, Some(ts));
    let mut tracker = EnergyTracker::default();
    tracker.observe_sample(t0 + 30 * MS, [cpu(0, t0), dram(0, t0 + 25 * MS)]);

    let cpu_pub = t0 + ms(2158.6);
    let dram_pub = t0 + 25 * MS + ms(2158.5);
    tracker.observe_sample(
        cpu_pub + 10 * MS,
        [cpu(5_000, cpu_pub), dram(0, t0 + 25 * MS)],
    );
    let cpu_watts = 5.0 / 2.1586;
    assert_watts(tracker.readings().cpu, cpu_watts);
    assert_watts(tracker.readings().dram, 0.0);

    tracker.observe_sample(
        dram_pub + 80 * MS,
        [cpu(5_000, cpu_pub), dram(294, dram_pub)],
    );
    assert_watts(tracker.readings().cpu, cpu_watts);
    assert_watts(tracker.readings().dram, 0.294 / 2.1585);
}

#[test]
fn idle_publication_reads_zero() {
    // An idle ANE still publishes, with no energy: that is 0 W, not a hold.
    let t0 = 1_000 * MS;
    let ane = |value: i64, ts: u64| obs("ANE0", "mJ", value, Some(ts));
    let mut tracker = EnergyTracker::default();
    tracker.observe_sample(t0 + 10 * MS, [ane(100, t0)]);

    let t1 = t0 + 2_000 * MS;
    tracker.observe_sample(t1 + 10 * MS, [ane(600, t1)]);
    assert_watts(tracker.readings().ane, 0.25);

    let t2 = t1 + 2_100 * MS;
    tracker.observe_sample(t2 + 10 * MS, [ane(600, t2)]);
    assert_watts(tracker.readings().ane, 0.0);
}

#[test]
fn counter_reset_rebaselines_without_a_reading() {
    let t0 = 1_000 * MS;
    let mut tracker = EnergyTracker::default();
    tracker.observe_sample(t0 + 10 * MS, [cpu(10_000, t0)]);
    tracker.observe_sample(t0 + 2_010 * MS, [cpu(12_000, t0 + 2_000 * MS)]);
    assert_watts(tracker.readings().cpu, 1.0);

    // Value went backwards.
    tracker.observe_sample(t0 + 4_010 * MS, [cpu(100, t0 + 4_000 * MS)]);
    assert_watts(tracker.readings().cpu, 0.0);
    tracker.observe_sample(t0 + 6_010 * MS, [cpu(4_100, t0 + 6_000 * MS)]);
    assert_watts(tracker.readings().cpu, 2.0);

    // Timestamp went backwards.
    tracker.observe_sample(t0 + 8_010 * MS, [cpu(5_000, t0 + 1_000 * MS)]);
    assert_watts(tracker.readings().cpu, 0.0);
}

#[test]
fn missing_timestamps_fall_back_to_observation_time() {
    // No timestamp at all: the poll window times the counter, as before.
    let t0 = 1_000 * MS;
    let mut tracker = EnergyTracker::default();
    tracker.observe_sample(t0, [obs("CPU Energy", "mJ", 0, None)]);
    tracker.observe_sample(t0 + 1_000 * MS, [obs("CPU Energy", "mJ", 1_500, None)]);
    assert_watts(tracker.readings().cpu, 1.5);
    tracker.observe_sample(t0 + 2_000 * MS, [obs("CPU Energy", "mJ", 1_500, None)]);
    assert_watts(tracker.readings().cpu, 0.0);
}

#[test]
fn frozen_timestamp_with_a_moving_value_switches_to_observation_time() {
    // The value moved but its timestamp did not, so this driver does not
    // stamp publications. The switch is permanent for the channel, and it
    // rebases onto the previous observation so this first switched span is
    // the 1.0 s poll window (100 ms to 1100 ms), not a span back to the
    // frozen driver timestamp t0.
    let t0 = 1_000 * MS;
    let mut tracker = EnergyTracker::default();
    tracker.observe_sample(t0 + 100 * MS, [cpu(0, t0)]);

    tracker.observe_sample(t0 + 1_100 * MS, [cpu(1_100, t0)]);
    assert_watts(tracker.readings().cpu, 1.1);

    // Later timestamps are ignored even when they advance.
    tracker.observe_sample(t0 + 2_100 * MS, [cpu(3_100, t0 + 2_050 * MS)]);
    assert_watts(tracker.readings().cpu, 2.0);
}

#[test]
fn constant_stale_timestamp_switches_to_the_poll_window() {
    // Every element carries the same old timestamp (frozen at 0) while the
    // value moves. Dividing by a span back to that frozen stamp would read
    // about 0.33 W; the switch must instead use the 1.0 s poll window.
    let t0 = 5_000 * MS;
    let stale = |value: i64, ts: u64| obs("CPU Energy", "mJ", value, Some(ts));
    let mut tracker = EnergyTracker::default();
    tracker.observe_sample(t0, [stale(0, 0)]);

    tracker.observe_sample(t0 + 1_000 * MS, [stale(2_000, 0)]);
    assert_watts(tracker.readings().cpu, 2.0);
}

#[test]
fn timestamps_disappearing_mid_stream_switch_to_the_poll_window() {
    // A stamped first sighting, a stamped batch that reads exactly, and then
    // `RawElements` stops carrying a timestamp entirely. The switched
    // reading must be the value change since the previous observation over
    // the time since the previous observation, and later observations must
    // stay on the observation clock even when a stale driver timestamp
    // reappears.
    let t0 = 1_000 * MS;
    let mut tracker = EnergyTracker::default();
    tracker.observe_sample(t0 + 10 * MS, [cpu(0, t0)]);

    let t1 = t0 + ms(2043.0);
    tracker.observe_sample(t1 + 10 * MS, [cpu(4_969, t1)]);
    let batch_watts = 4.969 / 2.043;
    assert_watts(tracker.readings().cpu, batch_watts);

    // Timestamp goes missing one second after the previous observation
    // (t1 + 10 ms), carrying 1000 mJ more.
    let t2 = t1 + 1_010 * MS;
    tracker.observe_sample(t2, [obs("CPU Energy", "mJ", 5_969, None)]);
    assert_watts(tracker.readings().cpu, 1.0);

    // A later observation with a stale, backward-looking driver timestamp
    // must not be honored: the channel stays on the observation clock.
    let t3 = t2 + 1_000 * MS;
    tracker.observe_sample(t3, [cpu(6_969, t1)]);
    assert_watts(tracker.readings().cpu, 1.0);
}

#[test]
fn stalled_channel_with_a_future_timestamp_does_not_hold_forever() {
    // A stamp far ahead of the observation clock, for example a driver on
    // mach_continuous_time after a system sleep. Left unfiltered it never
    // falls behind observed_at_ns, so the staleness check can never fire and
    // a stalled provider's held reading would live forever.
    let lead = 60_000 * MS;
    let t0 = 1_000 * MS;
    let mut tracker = EnergyTracker::default();
    tracker.observe_sample(t0, [cpu(0, t0 + lead)]);

    let t1 = t0 + 2_000 * MS;
    tracker.observe_sample(t1, [cpu(2_000, t1 + lead)]);
    assert_watts(tracker.readings().cpu, 1.0);

    // The provider stalls: value and stamp both frozen. Fifteen polls at 1 s.
    for step in 1..=15 {
        tracker.observe_sample(t1 + step * 1_000 * MS, [cpu(2_000, t1 + lead)]);
    }
    assert_watts(tracker.readings().cpu, 0.0);
}

#[test]
fn future_timestamp_mid_stream_switches_to_the_poll_window() {
    let t0 = 1_000 * MS;
    let mut tracker = EnergyTracker::default();
    tracker.observe_sample(t0 + 10 * MS, [cpu(0, t0)]);

    let t1 = t0 + ms(2043.0);
    tracker.observe_sample(t1 + 10 * MS, [cpu(4_969, t1)]);
    assert_watts(tracker.readings().cpu, 4.969 / 2.043);

    // A future timestamp arrives mid-stream: unusable, so the channel falls
    // back to the observation clock, rebased onto the previous observation.
    let t2 = t1 + 1_010 * MS;
    tracker.observe_sample(t2, [cpu(5_969, t2 + 30_000 * MS)]);
    assert_watts(tracker.readings().cpu, 1.0);

    // The switch is sticky: a plausible stamp does not undo it.
    tracker.observe_sample(t2 + 1_000 * MS, [cpu(6_969, t2 + 900 * MS)]);
    assert_watts(tracker.readings().cpu, 1.0);
}

#[test]
fn gpu_resolution_drops_channels_missing_from_the_latest_sample() {
    // GPU Energy and GPU0 carry the same energy on real hardware; they are
    // given different watts here so the readings show which source won.
    let t0 = 1_000 * MS;
    let gpu_energy = |value: i64, ts: u64| obs("GPU Energy", "nJ", value, Some(ts));
    let gpu0 = |value: i64, ts: u64| obs("GPU0", "mJ", value, Some(ts));
    let mut tracker = EnergyTracker::default();
    tracker.observe_sample(t0 + 10 * MS, [gpu_energy(0, t0), gpu0(0, t0)]);

    let t1 = t0 + 2_000 * MS;
    tracker.observe_sample(
        t1 + 10 * MS,
        [
            gpu_energy(counts(25.0, "nJ", 2.0), t1),
            gpu0(counts(70.0, "mJ", 2.0), t1),
        ],
    );
    assert_watts(tracker.readings().gpu, 25.0);

    // GPU Energy is absent from this sample: its last reading must not
    // count, and GPU0 becomes the only source for the rail.
    let t2 = t1 + 2_000 * MS;
    let gpu0_value = 2 * counts(70.0, "mJ", 2.0);
    tracker.observe_sample(t2 + 10 * MS, [gpu0(gpu0_value, t2)]);
    assert_watts(tracker.readings().gpu, 70.0);
}

#[test]
fn held_reading_expires_after_ten_seconds() {
    let t0 = 1_000 * MS;
    let mut tracker = EnergyTracker::default();
    tracker.observe_sample(t0 + 10 * MS, [cpu(0, t0)]);
    let t1 = t0 + 2_000 * MS;
    tracker.observe_sample(t1 + 10 * MS, [cpu(2_000, t1)]);
    assert_watts(tracker.readings().cpu, 1.0);

    tracker.observe_sample(t1 + 9_900 * MS, [cpu(2_000, t1)]);
    assert_watts(tracker.readings().cpu, 1.0);
    tracker.observe_sample(t1 + 10_100 * MS, [cpu(2_000, t1)]);
    assert_watts(tracker.readings().cpu, 0.0);
}

/// `(timestamp_ns, cumulative mJ)` for every publication of a channel
/// drawing a constant `watts`, spaced by the measured M5 Max spans.
fn publications(watts: f64) -> Vec<(u64, i64)> {
    let start = 1_000 * MS;
    let mut at = start;
    let mut out = vec![(start, 0)];
    for span in M5_MAX_PUBLICATION_SPANS_MS {
        at += ms(span);
        out.push((at, counts(watts, "mJ", (at - start) as f64 / 1e9)));
    }
    out
}

/// Poll the driver's published state every `poll_ms`. Returns the tracker's
/// reading after each poll, and what energy over the poll window gives.
fn poll(publications: &[(u64, i64)], poll_ms: u64) -> (Vec<f64>, Vec<f64>) {
    let mut tracker = EnergyTracker::default();
    let mut readings = vec![];
    let mut windowed = vec![];
    let mut previous: Option<(u64, i64)> = None;
    let end = publications.last().map_or(0, |(ts, _)| *ts);
    let mut at = publications[0].0 + 300 * MS;

    while at <= end {
        let (ts, value) = *publications
            .iter()
            .rev()
            .find(|(ts, _)| *ts <= at)
            .expect("the first publication precedes every poll");
        tracker.observe_sample(at, [cpu(value, ts)]);
        readings.push(tracker.readings().cpu);
        if let Some((prev_at, prev_value)) = previous {
            windowed.push((value - prev_value) as f64 * 1e-3 / ((at - prev_at) as f64 / 1e9));
        }
        previous = Some((at, value));
        at += poll_ms * MS;
    }
    (readings, windowed)
}

/// After the first span closes, every reading must be the load itself.
fn assert_steady(readings: &[f64], watts: f64, min_checked: usize) {
    let first = readings
        .iter()
        .position(|w| *w > 0.0)
        .expect("a publication span closes");
    let steady = &readings[first..];
    assert!(
        steady.len() >= min_checked,
        "only {} readings",
        steady.len()
    );
    for (i, w) in steady.iter().enumerate() {
        assert!(
            (w - watts).abs() < 0.01,
            "poll {i}: expected {watts} W, got {w} W"
        );
    }
}

#[test]
fn one_second_poll_reads_constant_load_without_zero_ticks() {
    let (readings, windowed) = poll(&publications(35.0), 1_000);

    // The poll-window rate alternates between 0 W and a double batch.
    assert!(windowed.contains(&0.0));
    assert!(windowed.iter().any(|w| *w > 60.0));

    assert_steady(&readings, 35.0, 30);
}

#[test]
fn three_second_poll_reads_constant_load_without_beating() {
    let (readings, windowed) = poll(&publications(35.0), 3_000);

    // Every 3 s window holds one or two batches, so the poll-window rate
    // beats between about 0.7x and 1.4x without ever reaching 0 W.
    assert!(!windowed.contains(&0.0));
    let (low, high) = windowed
        .iter()
        .fold((f64::MAX, f64::MIN), |(lo, hi), w| (lo.min(*w), hi.max(*w)));
    assert!(low < 0.8 * 35.0 && high > 1.3 * 35.0, "{low}..{high}");

    assert_steady(&readings, 35.0, 10);
}
