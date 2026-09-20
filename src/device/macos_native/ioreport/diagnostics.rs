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

//! Hardware diagnostic for the Energy Model and the SMC temperature keys
//! (issues #410 and #415), run by hand on a real Mac:
//!
//! ```text
//! cargo test --lib ioreport_energy_diagnostics -- --ignored --nocapture
//! ```
//!
//! It prints, in order:
//!
//! 1. The whole `Energy Model` inventory in the fixture format
//!    (`name<TAB>unit`), enumerated from the group itself rather than from the
//!    subscription, which holds only the tracked channels. A `# subgroups`
//!    line follows it.
//! 2. 60 ticks at 100 ms of every tracked channel's counter delta (`dv`),
//!    publication span (`dts`), and publication age, read from raw samples of
//!    the real subscription, next to the rail readings `EnergyTracker`
//!    produced from them and how long the sample call took.
//! 3. The same sampling continued silently to 32 s, with a second,
//!    unfiltered subscription sampled beside the real one on every tick. For
//!    every channel that could feed a rail under some naming (a name starting
//!    with `GPU`, `ANE`, or `DRAM` after any `DIE_<n>_` prefix, or ending in
//!    `CPU Energy`) it prints the publication spans, the energy and power over
//!    the channel's own first-to-last publication window, and the energy over
//!    one window common to all of them, whose edges sit on publications of
//!    the channel that publishes least often. That is how `GPU0_0` was
//!    compared with `GPU Energy` on an M1 Ultra: the mJ channels publish
//!    twice per ~2.1 s and `GPU Energy` every 109 to 139 ms, so only a window
//!    tens of seconds long compares them to within a few percent.
//! 4. Summary statistics of the tracker's rails over every tick.
//! 5. The SMC temperature key inventory (`smc::temperature_report`).
//!
//! Use it to record another chip next to the fixtures in
//! `tests/fixtures/ioreport/` and to check that its publication cadence
//! matches what the tracker assumes.

use super::super::smc::{SMC, temperature_report::temperature_key_report};
use super::energy_comparison::{History, print_energy_comparison};
use super::*;
use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

/// Ticks printed one line each.
const PRINTED_TICKS: usize = 60;

/// How long sampling continues in total, printed ticks included, before the
/// energy comparison is printed.
const COMPARISON_WINDOW: Duration = Duration::from_secs(32);

/// Upper bound on ticks, so a sample call that returns instantly cannot turn
/// the time-bounded loop into a long one.
const MAX_TICKS: usize = 1_000;

/// A channel whose energy the comparison reports: every block that could
/// feed a rail under some naming, including ones no rule accepts today.
fn is_rail_candidate(name: &str) -> bool {
    let block = strip_die_prefix(name);
    block.starts_with("GPU")
        || block.starts_with("ANE")
        || block.starts_with("DRAM")
        || name.ends_with("CPU Energy")
}

/// A subscription to every `Energy Model` channel, with no filter.
struct UnfilteredEnergy {
    subscription: IOReportSubscriptionRef,
    channels: CFMutableDictionaryRef,
}

impl UnfilteredEnergy {
    /// Subscribe to every channel `description` lists. Borrows `description`;
    /// the caller still owns and releases it.
    fn open(description: CFDictionaryRef) -> Option<Self> {
        if description.is_null() {
            return None;
        }
        // SAFETY: `description` is a live dictionary the caller keeps alive
        // for this call. The mutable copy is +1 and owned by the returned
        // value, which releases it in `Drop`; on the failure path it is
        // released here. The subscription is handled like `IOReport::new`'s.
        unsafe {
            let count = core_foundation::dictionary::CFDictionaryGetCount(description) as isize;
            let channels = core_foundation::dictionary::CFDictionaryCreateMutableCopy(
                core_foundation::base::kCFAllocatorDefault,
                count,
                description,
            );
            if channels.is_null() {
                return None;
            }
            let mut subscribed: CFMutableDictionaryRef = ptr::null_mut();
            let subscription =
                IOReportCreateSubscription(ptr::null(), channels, &mut subscribed, 0, ptr::null());
            if subscription.is_null() {
                CFRelease(channels as *const c_void);
                return None;
            }
            Some(Self {
                subscription,
                channels,
            })
        }
    }

    /// One raw sample: when it was taken, and every rail-candidate channel
    /// in it with its value and publication timestamp.
    fn sample(&self) -> Option<(u64, Vec<EnergyObservation>)> {
        // SAFETY: both pointers are owned by `self` and valid until `Drop`.
        // The sample is +1, read while alive, and released once below.
        let sample =
            unsafe { IOReportCreateSamples(self.subscription, self.channels, ptr::null()) };
        if sample.is_null() {
            return None;
        }
        let observed_at_ns = mach_now_ns();
        let observations = get_io_channels(sample)
            .into_iter()
            .filter_map(|item| {
                let channel = energy_channel_name(item)?;
                is_rail_candidate(&channel).then(|| EnergyObservation {
                    unit: channel_unit(item),
                    // SAFETY: `item` belongs to `sample`, alive until the
                    // release below.
                    value: unsafe { IOReportSimpleGetIntegerValue(item, 0) },
                    timestamp_ns: raw_element_timestamp(item).map(mach_ticks_to_ns),
                    channel,
                })
            })
            .collect();
        unsafe { CFRelease(sample as *const c_void) };
        Some((observed_at_ns, observations))
    }
}

impl Drop for UnfilteredEnergy {
    fn drop(&mut self) {
        // SAFETY: `channels` is the +1 copy made in `open`, released once.
        unsafe { CFRelease(self.channels as *const c_void) };
    }
}

fn ms(ns: u64) -> String {
    format!("{:.1}ms", ns as f64 / 1e6)
}

/// Min, mean, and max of `values`, formatted.
fn spread(values: &[f64]) -> String {
    if values.is_empty() {
        return "n/a".to_string();
    }
    let min = values.iter().copied().fold(f64::MAX, f64::min);
    let max = values.iter().copied().fold(f64::MIN, f64::max);
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    format!("min {min:.2} mean {mean:.2} max {max:.2}")
}

/// Hardware diagnostic for the Energy Model and the SMC temperature keys.
/// See the module docs for what it prints and why.
#[test]
#[ignore = "hardware diagnostic; run by hand with --ignored --nocapture"]
fn ioreport_energy_diagnostics() {
    let Ok(mut report) = IOReport::new() else {
        println!("IOReport is unavailable on this host; nothing to report");
        return;
    };

    let (numer, denom) = mach_timebase();
    println!("# mach timebase {numer}/{denom}");
    // The whole group, not the subscription: the subscription holds only
    // the tracked channels, and the inventory exists to record the rest.
    // SAFETY: the group name is the process-lifetime static CFString and a
    // null subgroup means "all"; the result is a +1 dictionary (or null),
    // released once below after the inventory and the unfiltered
    // subscription have been built from it.
    let description = unsafe {
        IOReportCopyChannelsInGroup(get_cfstring_refs().energy_model, ptr::null(), 0, 0, 0)
    };
    println!("# Energy Model inventory (name<TAB>unit)");
    let mut inventory = 0;
    let mut subgroups: BTreeMap<String, usize> = BTreeMap::new();
    for item in get_io_channels(description) {
        if let Some(name) = energy_channel_name(item) {
            println!("{name}\t{}", channel_unit(item));
            inventory += 1;
            // SAFETY: `item` belongs to `description`, alive until the
            // release below; the subgroup is a get-rule reference.
            let subgroup =
                cfstr_to_string(unsafe { IOReportChannelGetSubGroup(item) }).unwrap_or_default();
            *subgroups.entry(subgroup).or_default() += 1;
        }
    }
    println!("# {inventory} channels");
    let subgroups: Vec<String> = subgroups
        .iter()
        .map(|(name, count)| format!("{name:?} x{count}"))
        .collect();
    println!("# subgroups: {}", subgroups.join(", "));
    let unfiltered = UnfilteredEnergy::open(description);
    if !description.is_null() {
        unsafe { CFRelease(description as *const c_void) };
    }
    if unfiltered.is_none() {
        println!("# the unfiltered subscription could not be opened; no energy comparison");
    }

    println!("# per tracked channel: dv = counter delta, dts = publication span");
    let mut previous: HashMap<String, (i64, Option<u64>)> = HashMap::new();
    let mut histories: BTreeMap<String, History> = BTreeMap::new();
    let mut rails_per_tick = Vec::new();
    let mut unfiltered_samples = 0;
    let start = Instant::now();
    for tick in 0..MAX_TICKS {
        if tick >= PRINTED_TICKS && start.elapsed() >= COMPARISON_WINDOW {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
        let Ok(sample) = report.take_sample() else {
            continue;
        };
        let now_ns = mach_now_ns();
        let observations = energy_observations(sample);
        // SAFETY: `sample` is the +1 reference `take_sample` returns for
        // this iteration, released exactly once here; `energy_observations`
        // has already copied out everything it needs, so `sample` is not
        // used afterwards.
        unsafe { CFRelease(sample as *const c_void) };
        let sample_took = report.last_sample_duration();
        let rails = report.energy_readings();
        rails_per_tick.push(rails);

        if let Some((_, candidates)) = unfiltered.as_ref().and_then(UnfilteredEnergy::sample) {
            unfiltered_samples += 1;
            let mut seen: HashMap<&str, usize> = HashMap::new();
            for obs in &candidates {
                *seen.entry(obs.channel.as_str()).or_default() += 1;
            }
            for obs in &candidates {
                let history = histories.entry(obs.channel.clone()).or_default();
                if seen.get(obs.channel.as_str()).is_some_and(|n| *n > 1) {
                    history.duplicated += 1;
                    continue;
                }
                history.observe(obs);
            }
        }

        if tick >= PRINTED_TICKS {
            continue;
        }
        let fields: Vec<String> = observations
            .iter()
            .map(|obs| {
                let (prev_value, prev_ts) = previous
                    .get(&obs.channel)
                    .copied()
                    .unwrap_or((obs.value, obs.timestamp_ns));
                let dts = match (obs.timestamp_ns, prev_ts) {
                    (Some(ts), Some(prev)) => ms(ts.saturating_sub(prev)),
                    _ => "none".to_string(),
                };
                let age = obs
                    .timestamp_ns
                    .map_or("none".to_string(), |ts| ms(now_ns.saturating_sub(ts)));
                format!(
                    "{} dv={}{} dts={dts} age={age}",
                    obs.channel,
                    obs.value.saturating_sub(prev_value),
                    obs.unit
                )
            })
            .collect();
        for obs in observations {
            previous.insert(obs.channel, (obs.value, obs.timestamp_ns));
        }
        println!(
            "{tick:03} t={:.3}s sample={} {} || cpu={:.2}W gpu={:.2}W ane={:.2}W dram={:.2}W",
            start.elapsed().as_secs_f64(),
            ms(u64::try_from(sample_took.as_nanos()).unwrap_or(u64::MAX)),
            fields.join(" | "),
            rails.cpu,
            rails.gpu,
            rails.ane,
            rails.dram
        );
    }
    let secs = start.elapsed().as_secs_f64();
    println!(
        "# {} ticks over {secs:.1} s, the first {PRINTED_TICKS} printed",
        rails_per_tick.len()
    );

    print_energy_comparison(&histories, unfiltered_samples, secs);

    // Rails from the tracker on every tick after the first CPU reading. The
    // ticks before it have no closed span yet, which is not a zero reading.
    let first = rails_per_tick
        .iter()
        .position(|r| r.cpu > 0.0)
        .unwrap_or(rails_per_tick.len());
    let after = &rails_per_tick[first..];
    let column =
        |pick: fn(&EnergyReadings) -> f64| -> Vec<f64> { after.iter().map(pick).collect() };
    let zeros = |values: &[f64]| values.iter().filter(|w| **w == 0.0).count();
    let (cpu, gpu, ane, dram) = (
        column(|r| r.cpu),
        column(|r| r.gpu),
        column(|r| r.ane),
        column(|r| r.dram),
    );
    println!(
        "# tracker rails over the {} ticks from the first CPU reading (tick {first}), W:",
        after.len()
    );
    println!("# cpu {} zero ticks={}", spread(&cpu), zeros(&cpu));
    println!("# gpu {} zero ticks={}", spread(&gpu), zeros(&gpu));
    println!("# ane {} zero ticks={}", spread(&ane), zeros(&ane));
    println!("# dram {} zero ticks={}", spread(&dram), zeros(&dram));

    match SMC::new() {
        Ok(mut smc) => print!("{}", temperature_key_report(&mut smc)),
        Err(error) => println!("# SMC unavailable: {error}"),
    }
}
