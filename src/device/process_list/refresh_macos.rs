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

//! One tick of the local process pass on macOS (issue #427).
//!
//! The collector refreshes the tracked top-500 PIDs every tick and the whole
//! table every fifth tick. Here the two kinds of tick take different paths:
//!
//! * **Selective tick**: sysinfo is not called for processes at all. The
//!   sampler reads the tracked PIDs, and the cache is walked directly: a
//!   tracked row takes its CPU percent, memory, state and run time from its
//!   sample, leaves when the sampler says the process is gone (or that the
//!   PID now belongs to a different process), and keeps its values when the
//!   kernel would not answer. Untracked rows keep the values of the last
//!   full tick, as they did when sysinfo's stale map was walked instead.
//!   sysinfo's map is deliberately not walked: it still holds every tracked
//!   PID that has died since the last full tick, and a walk would put those
//!   rows back.
//! * **Full tick**: sysinfo refreshes everything, because it is what
//!   discovers new processes and supplies the static metadata (name, user,
//!   parent, start time, command). The sampler then reads every PID sysinfo
//!   holds, and every row's dynamic fields come from its sample, so CPU
//!   percent for a process outside the tracked set is its task-time delta
//!   over the five ticks since its baseline, not sysinfo's five-tick delta
//!   over a one-tick interval. A row whose sample is unreadable takes
//!   sysinfo's values, as before (memory 0, state `?`).
//!
//! A row whose start time changed under the same PID is replaced (full
//! tick) or dropped until the next full tick rediscovers it (selective
//! tick), so a reused PID never shows the previous process's name.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

use super::sampler_macos::{ProcessSample, ProcessSampler, Sampled};
use super::{mark_gpu, new_entry_from_sysinfo, refresh_entry_from_sysinfo};
use crate::device::types::ProcessInfo;

/// How long the parts of one tick took, for `perf_tick_stages`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProcessRefreshTimings {
    /// sysinfo's `refresh_processes_specifics` (full ticks only) plus
    /// `refresh_memory`.
    pub sysinfo: Duration,
    /// The sampler's pass over the tracked PIDs (selective) or every PID
    /// sysinfo holds (full).
    pub sampler: Duration,
    /// Folding the samples and sysinfo's map into the cache.
    pub cache: Duration,
}

/// The refresh kind the collector always used: CPU, memory, and the user
/// once. sysinfo copies every process's arguments anyway (see
/// `sampler_macos`), which is why it only runs on full ticks now.
fn refresh_kind() -> ProcessRefreshKind {
    ProcessRefreshKind::nothing()
        .with_cpu()
        .with_memory()
        .with_user(UpdateKind::OnlyIfNotSet)
}

/// Run the process pass of one tick and return the rows for it.
///
/// `full` asks for a full tick; an empty `tracked` set forces one, as it
/// always did, since there is nothing to sample selectively.
pub fn refresh_processes(
    system: &mut System,
    sampler: &mut ProcessSampler,
    tracked: &[Pid],
    full: bool,
    gpu_pids: &HashSet<u32>,
    cache: &mut HashMap<u32, ProcessInfo>,
) -> (Vec<ProcessInfo>, ProcessRefreshTimings) {
    let mut timings = ProcessRefreshTimings::default();
    let rows = if full || tracked.is_empty() {
        let started = Instant::now();
        system.refresh_processes_specifics(ProcessesToUpdate::All, true, refresh_kind());
        system.refresh_memory();
        timings.sysinfo = started.elapsed();

        let started = Instant::now();
        let samples = sampler.sample(system.processes().keys().map(|pid| pid.as_u32()));
        timings.sampler = started.elapsed();

        let started = Instant::now();
        let rows = update_cache_full(system, gpu_pids, cache, &samples);
        timings.cache = started.elapsed();
        rows
    } else {
        let started = Instant::now();
        let samples = sampler.sample(tracked.iter().map(|pid| pid.as_u32()));
        timings.sampler = started.elapsed();

        let started = Instant::now();
        system.refresh_memory();
        timings.sysinfo = started.elapsed();

        let started = Instant::now();
        let rows = update_cache_selective(system.total_memory(), gpu_pids, cache, &samples);
        timings.cache = started.elapsed();
        rows
    };
    sampler.retain(|pid| cache.contains_key(&pid));
    (rows, timings)
}

/// Full tick: walk sysinfo's freshly refreshed map, creating rows for new
/// processes and taking every row's dynamic fields from its sample.
fn update_cache_full(
    system: &System,
    gpu_pids: &HashSet<u32>,
    cache: &mut HashMap<u32, ProcessInfo>,
    samples: &HashMap<u32, Sampled>,
) -> Vec<ProcessInfo> {
    let mut seen_pids: HashSet<u32> = HashSet::with_capacity(system.processes().len());
    let total_memory = system.total_memory();

    for (pid, process) in system.processes() {
        let pid_u32 = pid.as_u32();
        let sample = match samples.get(&pid_u32) {
            // Exited between sysinfo's refresh and the sampler's pass: not
            // seen, so the row goes now rather than one tick later.
            Some(Sampled::Gone) => continue,
            Some(Sampled::Live(sample)) => Some(sample),
            Some(Sampled::Unreadable) | None => None,
        };
        seen_pids.insert(pid_u32);
        let uses_gpu = gpu_pids.contains(&pid_u32);

        let same_process = cache
            .get(&pid_u32)
            .is_some_and(|cached| started_at(cached, process.start_time()));
        if let Some(cached) = cache.get_mut(&pid_u32).filter(|_| same_process) {
            match sample {
                Some(sample) => apply_sample(cached, sample, total_memory),
                None => refresh_entry_from_sysinfo(cached, process, total_memory),
            }
            mark_gpu(cached, uses_gpu);
        } else {
            // New, or a reused PID whose row still described the previous
            // process: build it from sysinfo's static metadata.
            let mut entry = new_entry_from_sysinfo(pid_u32, process, total_memory, uses_gpu);
            if let Some(sample) = sample {
                apply_sample(&mut entry, sample, total_memory);
            }
            cache.insert(pid_u32, entry);
        }
    }

    cache.retain(|pid, _| seen_pids.contains(pid));
    rows_of(cache)
}

/// Selective tick: walk the cache, applying the tracked PIDs' samples.
fn update_cache_selective(
    total_memory: u64,
    gpu_pids: &HashSet<u32>,
    cache: &mut HashMap<u32, ProcessInfo>,
    samples: &HashMap<u32, Sampled>,
) -> Vec<ProcessInfo> {
    cache.retain(|pid, cached| {
        match samples.get(pid) {
            Some(Sampled::Gone) => return false,
            Some(Sampled::Live(sample)) => {
                if !started_at(cached, sample.start_time) {
                    // The PID belongs to a different process now; the next
                    // full tick creates its row with the right metadata.
                    return false;
                }
                apply_sample(cached, sample, total_memory);
            }
            // The kernel would not answer, or the row is not tracked this
            // tick: its values stand until the next full tick.
            Some(Sampled::Unreadable) | None => {}
        }
        mark_gpu(cached, gpu_pids.contains(pid));
        true
    });
    rows_of(cache)
}

/// Whether `cached` describes the process that started at `start_time`.
///
/// The cache stores the start time as sysinfo's `start_time()` formatted,
/// so a row that was created from sysinfo compares equal to sysinfo's value
/// and to the sampler's `pbi_start_tvsec`, which is the same field.
fn started_at(cached: &ProcessInfo, start_time: u64) -> bool {
    cached.start_time.parse::<u64>().ok() == Some(start_time)
}

/// The dynamic fields of one row, from its sample.
///
/// A sample without a CPU reading (first sighting) leaves the row's CPU
/// percent alone: 0 on a row just built from sysinfo, which is sysinfo's own
/// value for a process it just discovered, and the previous reading on an
/// existing row. No value is invented either way.
fn apply_sample(row: &mut ProcessInfo, sample: &ProcessSample, total_memory: u64) {
    if let Some(cpu_percent) = sample.cpu_percent {
        row.cpu_percent = cpu_percent;
    }
    row.memory_rss = sample.memory_rss;
    row.memory_vms = sample.memory_vms;
    row.memory_percent = (sample.memory_rss as f64 / total_memory as f64) * 100.0;
    if row.state != sample.state {
        row.state = sample.state.to_string();
    }
    row.cpu_time = sample.run_time;
}

fn rows_of(cache: &HashMap<u32, ProcessInfo>) -> Vec<ProcessInfo> {
    let mut rows: Vec<ProcessInfo> = cache.values().cloned().collect();
    rows.sort_by_key(|row| row.pid);
    rows
}

#[cfg(test)]
#[path = "refresh_macos/tests.rs"]
mod tests;
