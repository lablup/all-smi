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

//! Energy Model channel classification and per-channel power
//!
//! The pure half of the IOReport power readings: which `Energy Model` channels
//! feed each rail, and how a channel's cumulative counter and its publication
//! timestamps become watts. Nothing here calls IOReport, so the behavior is
//! tested from recorded inventories and timestamps. The hardware measurements
//! behind these rules are in the `ioreport` module docs.

use std::collections::HashMap;

/// Shortest publication span turned into a reading, in nanoseconds.
///
/// A batch is sometimes followed 9 to 26 ms later by a small second
/// publication. Dividing that tail by its own span would report a spike, so a
/// span this short leaves the baseline in place and the tail's energy and time
/// fold into the next span. Real spans are far longer: ~2.1 s for the batched
/// mJ channels, one poll interval for channels stamped at sample time.
const MIN_PUBLICATION_SPAN_NS: u64 = 50_000_000;

/// How long a channel may go without publishing before its held reading is
/// dropped, in nanoseconds. Keeps a stalled provider from showing as live.
const STALE_PUBLICATION_NS: u64 = 10_000_000_000;

/// The power rail an `Energy Model` channel is summed into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnergyRail {
    /// `CPU Energy`, or `DIE_<n>_CPU Energy` on multi-die packages.
    Cpu,
    /// `GPU Energy`.
    Gpu,
    /// `GPU<n>`. Carries the same energy as `GPU Energy`, so it counts only
    /// when a sample has no `GPU Energy` channel.
    GpuFallback,
    /// `ANE`, `ANE<n>`, or `ANE<n>_<m>`.
    Ane,
    /// `DRAM`, `DRAM<n>`, or `DRAM<n>_<m>`.
    Dram,
}

/// Classify an `Energy Model` channel by its exact name.
///
/// This is the single list of energy channels all-smi reads, so anything that
/// needs the set (the IOReport subscription filter, for one) should ask here
/// rather than keep its own names. Every other channel in the group, which on
/// an M5 Max means clusters, per-core channels, `_SRAM`, and 300 `DTL`
/// telemetry channels under the `CPU Energy` roll-up, returns `None` and is
/// never summed.
pub(crate) fn classify_energy_channel(name: &str) -> Option<EnergyRail> {
    if name == "GPU Energy" {
        Some(EnergyRail::Gpu)
    } else if name.ends_with("CPU Energy") {
        Some(EnergyRail::Cpu)
    } else if name.strip_prefix("GPU").is_some_and(is_digits) {
        Some(EnergyRail::GpuFallback)
    } else if is_top_level_name(name, "ANE") {
        Some(EnergyRail::Ane)
    } else if is_top_level_name(name, "DRAM") {
        Some(EnergyRail::Dram)
    } else {
        None
    }
}

/// A non-empty run of ASCII digits.
fn is_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// `prefix`, `prefix<n>`, or `prefix<n>_<m>`: a block's own channel, as
/// opposed to a sub-channel such as `ANE0_SRAM`.
fn is_top_level_name(name: &str, prefix: &str) -> bool {
    let Some(rest) = name.strip_prefix(prefix) else {
        return false;
    };
    if rest.is_empty() {
        return true;
    }
    match rest.split_once('_') {
        Some((unit, instance)) => is_digits(unit) && is_digits(instance),
        None => is_digits(rest),
    }
}

/// Joules per counter unit for an IOReport energy unit label.
///
/// Unrecognized labels are read as nanojoules, the finest unit the group uses.
fn joules_per_count(unit: &str) -> f64 {
    match unit {
        "mJ" => 1e-3,
        "uJ" => 1e-6,
        "nJ" => 1e-9,
        _ => 1e-9,
    }
}

/// Power per rail, in watts.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct EnergyReadings {
    pub cpu: f64,
    pub gpu: f64,
    pub ane: f64,
    pub dram: f64,
}

impl EnergyReadings {
    /// SoC package power: CPU + GPU + ANE. DRAM sits outside the package.
    pub fn package(&self) -> f64 {
        self.cpu + self.gpu + self.ane
    }
}

/// Sum per-channel watts into rails.
///
/// Channels that do not classify are ignored. GPU power comes from the
/// `GPU Energy` channels whenever the set has one and from `GPU<n>` otherwise,
/// which is why the GPU is resolved only after every channel has been seen.
pub fn sum_rails<'a>(channels: impl IntoIterator<Item = (&'a str, f64)>) -> EnergyReadings {
    let mut readings = EnergyReadings::default();
    let mut gpu_fallback = 0.0;
    let mut has_gpu_energy = false;

    for (name, watts) in channels {
        match classify_energy_channel(name) {
            Some(EnergyRail::Cpu) => readings.cpu += watts,
            Some(EnergyRail::Gpu) => {
                has_gpu_energy = true;
                readings.gpu += watts;
            }
            Some(EnergyRail::GpuFallback) => gpu_fallback += watts,
            Some(EnergyRail::Ane) => readings.ane += watts,
            Some(EnergyRail::Dram) => readings.dram += watts,
            None => {}
        }
    }

    if !has_gpu_energy {
        readings.gpu = gpu_fallback;
    }
    readings
}

/// One energy channel as read from a raw (non-delta) IOReport sample.
#[derive(Debug, Clone, PartialEq)]
pub struct EnergyObservation {
    pub channel: String,
    pub unit: String,
    /// Cumulative counter value, in `unit`.
    pub value: i64,
    /// When the driver last published `value`, in nanoseconds on the
    /// `mach_absolute_time` clock. `None` when the channel carries no usable
    /// timestamp.
    pub timestamp_ns: Option<u64>,
}

/// Baseline and latest reading for one tracked channel.
#[derive(Debug, Clone)]
struct ChannelState {
    /// Counter value the next span is measured from.
    baseline_value: i64,
    /// Publication time the next span is measured from, in nanoseconds.
    baseline_ns: u64,
    /// The previous observation's value and time, to catch a counter that
    /// moves while its timestamp stands still.
    last_value: i64,
    last_ns: u64,
    /// When the previous observation of this channel was taken, in
    /// nanoseconds on the observation clock. Used to rebase the baseline
    /// when the channel switches from driver timestamps to the observation
    /// clock, so the switch does not mix a driver timestamp with an
    /// observation time.
    last_observed_ns: u64,
    /// Watts over the most recent span. `None` until the first span closes,
    /// after a counter reset, and once the channel goes stale.
    watts: Option<f64>,
    /// Timed by when samples were taken instead of by the driver's
    /// timestamps. Set once those timestamps prove unusable, never cleared.
    /// The switch restarts the span from the previous observation, so the
    /// first reading on the observation clock is the poll window, not a
    /// span that mixes a driver timestamp with an observation time.
    observation_clock: bool,
    /// Sequence number of the last sample this channel appeared in.
    last_seen: u64,
}

impl ChannelState {
    fn first_sighting(obs: &EnergyObservation, observed_at_ns: u64, sample: u64) -> Self {
        let published_ns = obs.timestamp_ns.unwrap_or(observed_at_ns);
        Self {
            baseline_value: obs.value,
            baseline_ns: published_ns,
            last_value: obs.value,
            last_ns: published_ns,
            last_observed_ns: observed_at_ns,
            watts: None,
            observation_clock: obs.timestamp_ns.is_none(),
            last_seen: sample,
        }
    }

    fn observe(&mut self, obs: &EnergyObservation, observed_at_ns: u64) {
        let stamped = match obs.timestamp_ns {
            // A counter that moved while its timestamp did not is not being
            // stamped at publication, so its timestamps cannot time a span.
            Some(ts) if !self.observation_clock => {
                (ts != self.last_ns || obs.value == self.last_value).then_some(ts)
            }
            _ => None,
        };
        let published_ns = match stamped {
            Some(ts) => ts,
            None if self.observation_clock => observed_at_ns,
            None => {
                // First fallback for this channel: the driver's timestamps
                // are unusable. Rebase the baseline onto the observation
                // clock at the previous observation instead of leaving it on
                // a driver timestamp, so this span is the poll window
                // between the previous and current observation rather than
                // a span mixing the two clocks.
                self.baseline_value = self.last_value;
                self.baseline_ns = self.last_observed_ns;
                self.observation_clock = true;
                observed_at_ns
            }
        };
        self.last_value = obs.value;
        self.last_ns = published_ns;
        self.last_observed_ns = observed_at_ns;

        if obs.value < self.baseline_value || published_ns < self.baseline_ns {
            // Counter reset. There is no valid span to report until the next
            // publication closes one from here.
            self.baseline_value = obs.value;
            self.baseline_ns = published_ns;
            self.watts = None;
            return;
        }

        let span_ns = published_ns - self.baseline_ns;
        if span_ns >= MIN_PUBLICATION_SPAN_NS {
            let counts = i128::from(obs.value) - i128::from(self.baseline_value);
            let joules = counts as f64 * joules_per_count(&obs.unit);
            self.watts = Some(joules / (span_ns as f64 / 1e9));
            self.baseline_value = obs.value;
            self.baseline_ns = published_ns;
        } else {
            // Nothing published since the baseline, or only a split
            // publication's tail. Hold the previous reading.
            tracing::trace!(
                channel = obs.channel.as_str(),
                "no new energy publication span; holding the previous reading"
            );
        }

        if observed_at_ns.saturating_sub(published_ns) > STALE_PUBLICATION_NS {
            self.watts = None;
        }
    }
}

/// Turns raw energy counters into watts, timing each channel by its own
/// publication timestamps.
///
/// Power for a channel is `(value - baseline value) / (timestamp - baseline
/// timestamp)`, evaluated when the channel publishes. Between publications the
/// previous reading is held rather than replaced by 0 W, and the poll interval
/// never enters the calculation. Feed it every raw sample, in order.
#[derive(Debug, Default)]
pub struct EnergyTracker {
    channels: HashMap<String, ChannelState>,
    /// Number of samples observed so far.
    samples: u64,
}

impl EnergyTracker {
    /// Feed one raw sample.
    ///
    /// `observed_at_ns` is when the sample was taken, on the same clock as the
    /// timestamps. Channels that do not classify into a rail are ignored.
    pub fn observe_sample(
        &mut self,
        observed_at_ns: u64,
        observations: impl IntoIterator<Item = EnergyObservation>,
    ) {
        self.samples += 1;
        for obs in observations {
            if classify_energy_channel(&obs.channel).is_none() {
                continue;
            }
            if let Some(state) = self.channels.get_mut(&obs.channel) {
                state.observe(&obs, observed_at_ns);
                state.last_seen = self.samples;
            } else {
                let state = ChannelState::first_sighting(&obs, observed_at_ns, self.samples);
                self.channels.insert(obs.channel, state);
            }
        }
    }

    /// Power per rail as of the most recent sample. A channel with no reading
    /// yet counts as 0 W.
    pub fn readings(&self) -> EnergyReadings {
        sum_rails(
            self.channels
                .iter()
                .filter(|(_, state)| state.last_seen == self.samples)
                .map(|(name, state)| (name.as_str(), state.watts.unwrap_or(0.0))),
        )
    }
}

#[cfg(test)]
#[path = "energy/tests.rs"]
mod tests;
