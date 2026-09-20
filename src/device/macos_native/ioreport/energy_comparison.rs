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

//! Per-channel publication history and the energy comparison that the
//! hardware diagnostic (`diagnostics.rs`) prints from an unfiltered
//! `Energy Model` subscription. Split out of that file to keep both within the
//! size budget; see its module docs for what the comparison is for.

use super::super::energy::{EnergyRail, joules_per_count, sum_rails};
use super::{EnergyObservation, classify_energy_channel, strip_die_prefix};
use std::collections::BTreeMap;

/// Every publication one channel made while it was sampled.
#[derive(Default)]
pub(super) struct History {
    unit: String,
    /// `(timestamp_ns, value)` at the first observation and at every change
    /// of timestamp after it.
    publications: Vec<(u64, i64)>,
    /// Observations whose value moved while the timestamp stood still, the
    /// case that switches a channel to the observation clock for good.
    moved_under_frozen_stamp: usize,
    /// Observations whose timestamp was earlier than the previous one.
    stamp_went_backwards: usize,
    /// Observations with no timestamp at all.
    unstamped: usize,
    /// Samples that held this name more than once.
    pub(super) duplicated: usize,
}

impl History {
    pub(super) fn observe(&mut self, obs: &EnergyObservation) {
        self.unit.clone_from(&obs.unit);
        let Some(ts) = obs.timestamp_ns else {
            self.unstamped += 1;
            return;
        };
        match self.publications.last() {
            Some(&(last_ts, last_value)) if ts == last_ts => {
                if obs.value != last_value {
                    self.moved_under_frozen_stamp += 1;
                }
            }
            Some(&(last_ts, _)) if ts < last_ts => self.stamp_went_backwards += 1,
            _ => self.publications.push((ts, obs.value)),
        }
    }

    fn first(&self) -> Option<(u64, i64)> {
        self.publications.first().copied()
    }

    fn last(&self) -> Option<(u64, i64)> {
        self.publications.last().copied()
    }

    fn joules(&self, counts: f64) -> f64 {
        counts * joules_per_count(&self.unit)
    }

    /// Cumulative counter at `t`, interpolated linearly between the two
    /// publications around it. `None` outside the recorded publications.
    fn counter_at(&self, t: u64) -> Option<f64> {
        let after = self.publications.iter().position(|(ts, _)| *ts >= t)?;
        let (t1, v1) = self.publications[after];
        if t1 == t {
            return Some(v1 as f64);
        }
        let (t0, v0) = self.publications[after.checked_sub(1)?];
        Some(v0 as f64 + (v1 - v0) as f64 * (t - t0) as f64 / (t1 - t0) as f64)
    }
}

fn rail_label(name: &str) -> &'static str {
    match classify_energy_channel(name) {
        Some(EnergyRail::Cpu) => "cpu",
        Some(EnergyRail::Gpu) => "gpu",
        Some(EnergyRail::GpuFallback) => "gpu-fallback",
        Some(EnergyRail::Ane) => "ane",
        Some(EnergyRail::Dram) => "dram",
        None => "none",
    }
}

/// Print each candidate channel's cadence and energy over its own window,
/// then every candidate's energy over one window common to all of them.
pub(super) fn print_energy_comparison(
    histories: &BTreeMap<String, History>,
    samples: usize,
    secs: f64,
) {
    println!(
        "# rail candidates from an unfiltered Energy Model subscription: {samples} samples over {secs:.1} s"
    );
    println!("# channel\tunit\trail\tpublications\tstamp anomalies\town window\tjoules\twatts");
    for (name, history) in histories {
        let anomalies = format!(
            "moved-under-frozen={} backwards={} unstamped={} duplicated={}",
            history.moved_under_frozen_stamp,
            history.stamp_went_backwards,
            history.unstamped,
            history.duplicated
        );
        let (window, joules, watts) = match (history.first(), history.last()) {
            (Some((t0, v0)), Some((t1, v1))) if t1 > t0 => {
                let joules = history.joules((v1 - v0) as f64);
                let secs = (t1 - t0) as f64 / 1e9;
                (
                    format!("{secs:.3}s"),
                    format!("{joules:.3}"),
                    format!("{:.3}", joules / secs),
                )
            }
            _ => ("n/a".into(), "n/a".into(), "n/a".into()),
        };
        println!(
            "{name}\t{}\t{}\t{}\t{anomalies}\t{window}\t{joules}\t{watts}",
            history.unit,
            rail_label(name),
            history.publications.len()
        );
    }
    for (name, history) in histories {
        let spans: Vec<String> = history
            .publications
            .windows(2)
            .map(|pair| format!("{:.1}", (pair[1].0 - pair[0].0) as f64 / 1e6))
            .collect();
        println!("# spans ms {name}: {}", spans.join(" "));
    }

    // The common window: the span every channel with two or more
    // publications covers, with its edges moved inward onto publications of
    // the channel that publishes least often, so that channel and the ones
    // batched with it are read at their own stamps and only the frequent
    // publishers are interpolated.
    let published: Vec<(&String, &History)> = histories
        .iter()
        .filter(|(_, h)| h.publications.len() >= 2)
        .collect();
    let (Some(start), Some(end)) = (
        published
            .iter()
            .filter_map(|(_, h)| h.first())
            .map(|p| p.0)
            .max(),
        published
            .iter()
            .filter_map(|(_, h)| h.last())
            .map(|p| p.0)
            .min(),
    ) else {
        println!("# no channel published twice; no common window");
        return;
    };
    let reference = published
        .iter()
        .min_by_key(|(_, h)| h.publications.len())
        .map(|(_, h)| *h);
    let (w0, w1) = reference
        .and_then(|h| {
            let w0 = h.publications.iter().map(|p| p.0).find(|ts| *ts >= start)?;
            let w1 = h
                .publications
                .iter()
                .map(|p| p.0)
                .rev()
                .find(|ts| *ts <= end)?;
            (w1 > w0).then_some((w0, w1))
        })
        .unwrap_or((start, end));
    if w1 <= w0 {
        println!("# the channels' windows do not overlap; no common window");
        return;
    }
    let window_secs = (w1 - w0) as f64 / 1e9;
    println!("# common window: {window_secs:.3} s (joules and watts per channel over it)");

    let mut classified: Vec<(&str, f64)> = Vec::new();
    let mut unclassified: Vec<String> = Vec::new();
    let mut gpu_energy_watts = None;
    let mut others: Vec<(&String, f64)> = Vec::new();
    for (name, history) in &published {
        let (Some(c0), Some(c1)) = (history.counter_at(w0), history.counter_at(w1)) else {
            continue;
        };
        let joules = history.joules(c1 - c0);
        let watts = joules / window_secs;
        println!("{name}\t{joules:.3} J\t{watts:.3} W");
        match rail_label(name) {
            "none" => unclassified.push(format!("{name}={watts:.3}W")),
            _ => classified.push((name.as_str(), watts)),
        }
        if name.as_str() == "GPU Energy" {
            gpu_energy_watts = Some(watts);
        } else if strip_die_prefix(name).starts_with("GPU") {
            others.push((name, watts));
        }
    }
    // Exactly what the tracker would report for these channels, fallbacks
    // and package-over-die choices included.
    let rails = sum_rails(classified);
    println!(
        "# summed by rule over the common window: cpu={:.3}W gpu={:.3}W ane={:.3}W dram={:.3}W",
        rails.cpu, rails.gpu, rails.ane, rails.dram
    );
    println!(
        "# candidates no rule sums: {}",
        if unclassified.is_empty() {
            "none".to_string()
        } else {
            unclassified.join(" ")
        }
    );
    if let Some(reference) = gpu_energy_watts.filter(|w| *w > 0.0) {
        let together: f64 = others.iter().map(|(_, watts)| watts).sum();
        for (name, watts) in &others {
            println!(
                "# {name} / GPU Energy over the common window: {:.3}",
                watts / reference
            );
        }
        if others.len() > 1 {
            let names: Vec<&str> = others.iter().map(|(name, _)| name.as_str()).collect();
            println!(
                "# ({}) / GPU Energy over the common window: {:.3}",
                names.join(" + "),
                together / reference
            );
        }
    }
}
