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
//!
//! ## Multi-die packages
//!
//! The one multi-die inventory recorded so far is an Apple M1 Ultra
//! (Mac13,2, macOS 27.0, `tests/fixtures/ioreport/m1_ultra_energy_model.tsv`).
//! Of its 321 channels, only CPU channels carry the `DIE_<n>_` prefix (34:
//! clusters, per-core channels, `_CPM`, and one `DIE_<n>_CPU Energy` per
//! die). Every other block names its die with a suffix: `ANE0_0` and
//! `ANE0_1`, `DRAM0_0` and `DRAM0_1`, and the same for `ISP`, `AVE`, `MSR`,
//! `DCS`, and `AMCC`. The GPU has a single `GPU0_0` (and `GPU SRAM0_0`) next
//! to `GPU Energy`. No package `CPU Energy`, `ANE`, or `DRAM` sits beside the
//! per-die channels, so summing `DIE_<n>_CPU Energy` and the `<block><n>_<m>`
//! channels counts each die exactly once, which the rules below already do.
//!
//! `DIE_<n>_` handling for GPU, ANE, and DRAM therefore stays as it is: no
//! recorded chip has such a channel, so none classifies. A chip that adds one
//! has to be recorded first, because only its inventory can show whether a
//! package channel sits next to it, and a per-die sum must never be added to
//! a package total.
//!
//! [`EnergyTracker`] keys channels by name. The M1 Ultra lists 130 `DTL`
//! names (`ECPUDTL*`, `PCPUDTL*`, `PCPU1DTL*`) twice each, with no prefix to
//! tell the two apart. None of them classifies, which is what keeps the key
//! unambiguous.

use std::collections::HashMap;

/// Shortest publication span turned into a reading, in nanoseconds.
///
/// A publication is sometimes followed a few ms later by a small second one:
/// 9 to 26 ms after the batch on an M5 Max, 12 to 29 ms after the mJ channels
/// on an M1 Ultra, whose `GPU Energy` also moved its stamp 6 to 12 ms with no
/// energy when a sample landed before its next publication. Dividing that
/// tail by its own span would report a spike (or, for the empty one, 0 W), so
/// a span this short leaves the baseline in place and the tail's energy and
/// time fold into the next span. Real spans are far longer: ~2.1 s for the
/// batched mJ channels on an M5 Max and 0.42 to 1.69 s on an M1 Ultra (two
/// publications per ~2.1 s), 110 to 246 ms for `GPU Energy` on an M1 Ultra,
/// and one poll interval for channels stamped at sample time.
const MIN_PUBLICATION_SPAN_NS: u64 = 50_000_000;

/// How long a channel may go without publishing before its held reading is
/// dropped, in nanoseconds. Keeps a stalled provider from showing as live.
const STALE_PUBLICATION_NS: u64 = 10_000_000_000;

/// How far ahead of the observation time a driver timestamp may sit and still
/// be trusted, in nanoseconds.
///
/// A stamp on the observation clock is never later than the observation: the
/// driver writes the element before `IOReportCreateSamples` returns, and
/// `observed_at_ns` is read after it returns. A stamp further ahead than this
/// comes from another clock (for example a driver on `mach_continuous_time`
/// after a system sleep) or is not a timestamp at all (the element layout
/// changed), and honoring it would let a stalled channel hold its reading
/// forever, since the staleness check below can never see it fall behind. 1 s
/// of slack is deliberate headroom, not a measured bound.
const MAX_TIMESTAMP_LEAD_NS: u64 = 1_000_000_000;

/// The power rail an `Energy Model` channel is summed into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnergyRail {
    /// `CPU Energy`, or `DIE_<n>_CPU Energy` on multi-die packages (one per
    /// die on an M1 Ultra, which has no package `CPU Energy`).
    Cpu,
    /// `GPU Energy`.
    Gpu,
    /// `GPU<n>` or `GPU<n>_<m>`. Carries nearly the same energy as
    /// `GPU Energy`, so it counts only when a sample has no `GPU Energy`
    /// channel. On an M5 Max `GPU0` matches `GPU Energy`; on an M1 Ultra
    /// `GPU0_0` reads a few percent less, the share of `GPU SRAM0_0`, which
    /// no rule sums, so a chip that fell back to it would read that much low.
    GpuFallback,
    /// `ANE`, `ANE<n>`, or `ANE<n>_<m>` (`ANE0_0` and `ANE0_1`, one per die,
    /// on an M1 Ultra).
    Ane,
    /// `DRAM`, `DRAM<n>`, or `DRAM<n>_<m>` (`DRAM0_0` and `DRAM0_1` on an M1
    /// Ultra).
    Dram,
}

/// Classify an `Energy Model` channel by its exact name.
///
/// This is the single list of energy channels all-smi reads, so anything that
/// needs the set (the IOReport subscription filter, for one) should ask here
/// rather than keep its own names. Every other channel in the group returns
/// `None` and is never summed: on an M5 Max that is 359 of 364 channels
/// (clusters, per-core channels, `_SRAM`, and 300 `DTL` telemetry channels
/// under the `CPU Energy` roll-up), and on an M1 Ultra 313 of 321 (the same
/// families per die, `GPU SRAM0_0`, and the `apciec<n> Energy` and
/// `PCIe Port <n> Energy` channels, whose ` Energy` suffix matches no rule).
pub(crate) fn classify_energy_channel(name: &str) -> Option<EnergyRail> {
    if name == "GPU Energy" {
        Some(EnergyRail::Gpu)
    } else if name.ends_with("CPU Energy") {
        Some(EnergyRail::Cpu)
    } else if name != "GPU" && is_top_level_name(name, "GPU") {
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
pub(super) fn joules_per_count(unit: &str) -> f64 {
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
/// `GPU Energy` channels whenever the set has one and from `GPU<n>` or
/// `GPU<n>_<m>` otherwise, which is why the GPU is resolved only after every
/// channel has been seen.
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
    /// timestamps. Set once those timestamps prove unusable (missing, frozen
    /// while the value moves, or later than the observation), never cleared.
    /// The switch restarts the span from the previous observation, so the
    /// first reading on the observation clock is the poll window, not a
    /// span that mixes a driver timestamp with an observation time.
    observation_clock: bool,
    /// Sequence number of the last sample this channel appeared in.
    last_seen: u64,
}

/// Whether `ts` could plausibly be a publication timestamp on the observation
/// clock, given that the observation happened at `observed_at_ns`. See
/// [`MAX_TIMESTAMP_LEAD_NS`] for why a stamp further ahead cannot be one.
fn plausible_timestamp(ts: u64, observed_at_ns: u64) -> bool {
    ts <= observed_at_ns.saturating_add(MAX_TIMESTAMP_LEAD_NS)
}

impl ChannelState {
    fn first_sighting(obs: &EnergyObservation, observed_at_ns: u64, sample: u64) -> Self {
        let usable_ts = obs
            .timestamp_ns
            .filter(|ts| plausible_timestamp(*ts, observed_at_ns));
        let published_ns = usable_ts.unwrap_or(observed_at_ns);
        Self {
            baseline_value: obs.value,
            baseline_ns: published_ns,
            last_value: obs.value,
            last_ns: published_ns,
            last_observed_ns: observed_at_ns,
            watts: None,
            observation_clock: usable_ts.is_none(),
            last_seen: sample,
        }
    }

    fn observe(&mut self, obs: &EnergyObservation, observed_at_ns: u64) {
        let stamped = match obs.timestamp_ns {
            // A counter that moved while its timestamp did not is not being
            // stamped at publication, so its timestamps cannot time a span.
            // A timestamp further ahead of the observation than plausible is
            // not on our clock at all, so it cannot time a span either.
            Some(ts) if !self.observation_clock && plausible_timestamp(ts, observed_at_ns) => {
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

#[cfg(test)]
#[path = "energy/m1_ultra_tests.rs"]
mod m1_ultra_tests;
