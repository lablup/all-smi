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

//! Per-process CPU, memory, state and run time on macOS, read in-process
//! through `proc_pidinfo` instead of sysinfo's per-tick refresh (issue #427).
//!
//! The local collector refreshes the tracked top-500 PIDs every tick and
//! every PID every fifth tick. sysinfo 0.39.6 makes that expensive and, on
//! the fifth tick, wrong:
//!
//! 1. **An unconditional copy of every argument and environment area.**
//!    `update_process` (sysinfo, `src/unix/apple/macos/process.rs:750`)
//!    calls `get_process_infos` for every refreshed process, and
//!    `get_process_infos` (`:539-648`) issues the two `KERN_PROCARGS2`
//!    sysctls, which copy the whole area, *before* it checks whether `exe`,
//!    `cmd` or `environ` need updating (`:630-641`). No `ProcessRefreshKind`
//!    disables it. With 967 processes and 500 tracked PIDs those two sysctls
//!    are 20.8 ms of a 27.2 ms selective refresh; the three `proc_pidinfo`
//!    calls that carry the values the collector keeps are 1.9 ms together.
//!    Linux and Windows read the command line only behind the refresh kind
//!    (`src/unix/linux/process.rs:520-531`, `src/windows/process.rs:749-754`).
//! 2. **One global interval for every process.** `get_time_interval`
//!    (`src/unix/apple/macos/system.rs:113-152`) derives the interval from
//!    the CPU ticks elapsed since the *previous refresh call*, and
//!    `compute_cpu_usage` (`process.rs:285-306`) divides each process's
//!    task-time delta by it. A process outside the tracked set is refreshed
//!    only every fifth tick, so on that tick its five-tick delta is divided
//!    by a one-tick interval and it reads about five times its real share
//!    (a `yes` child measured 98.87 percent, then 501.87 on the full tick,
//!    then 100.11). That inflated number is the sort key for the displayed
//!    list, so unrelated processes jump onto the screen every fifth tick.
//!    Windows is immune (it keeps per-process copies of the global times,
//!    `src/windows/process.rs:1049-1105`); Linux has the same defect in a
//!    worse form and is tracked separately.
//! 3. **A reading that never returns to zero.** `compute_cpu_usage` assigns
//!    `cpu_usage` only when the task-time delta is positive (`:297-302`, and
//!    still so in upstream `master`), so a process that used CPU once and
//!    then went idle keeps its last non-zero percentage for as long as it
//!    stays idle: a child that burned 1.44 percent for one second read 1.44
//!    for the next five idle seconds, and `photolibraryd` read 33.78 percent
//!    for a second in which its counter did not move. In the same probe run
//!    28, 29 and 46 of the 639 inspectable processes on an M1 Ultra carried
//!    such a stale value in three consecutive one-second windows.
//!
//! This sampler keeps, per PID, the previous `pti_total_user +
//! pti_total_system` and the `mach_absolute_time` at which it was read, and
//! reports CPU percent as the task-time delta over its own elapsed time for
//! that PID. Both counters are in mach absolute time units, so the ratio
//! needs no timebase conversion (a `yes` child reads 99.98 percent against
//! sysinfo's 100.30 over the same second). A PID it has not seen before
//! reports no CPU reading, as sysinfo does for a process it just discovered
//! (`compute_cpu_usage` requires a non-zero previous total); a counter that
//! does not move reads 0, which is where this sampler deliberately differs
//! from defect 3. Memory comes from the same `PROC_PIDTASKINFO` structure
//! sysinfo reads, so it matches exactly; the state code is derived by the
//! same rule sysinfo's `status()` applies (see [`state_code`]), and run time
//! is seconds since `pbi_start_tvsec`, as sysinfo's `run_time()`.
//!
//! What the sampler does *not* do: discover processes or supply their static
//! metadata (name, user, parent, command). sysinfo still does that on every
//! fifth tick; see `refresh_macos` for how the two are combined.

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use super::pidinfo_macos::{self, PidInfoError};

// `mach_absolute_time` lives in libSystem, which every macOS binary links; it
// is declared here because the `libc` binding for it is deprecated (the
// IOReport reader does the same).
unsafe extern "C" {
    fn mach_absolute_time() -> u64;
}

/// One PID's readings for this tick.
#[derive(Clone, Debug, PartialEq)]
pub struct ProcessSample {
    /// Task-time delta over the sampler's elapsed time for this PID, as a
    /// percentage of one core. `None` on the first sighting of a PID (or of
    /// a reused PID), when there is nothing to take a delta against.
    pub cpu_percent: Option<f64>,
    /// `pti_resident_size`, bytes.
    pub memory_rss: u64,
    /// `pti_virtual_size`, bytes.
    pub memory_vms: u64,
    /// Single-letter state code as the process table shows it.
    pub state: &'static str,
    /// Seconds since the process started.
    pub run_time: u64,
    /// `pbi_start_tvsec`: the process's start time, which is what tells a
    /// reused PID apart from the process that had it before.
    pub start_time: u64,
}

/// What sampling one PID found.
#[derive(Clone, Debug, PartialEq)]
pub enum Sampled {
    /// The process is alive and inspectable.
    Live(ProcessSample),
    /// The kernel would not say (`EPERM`, a short read). The process may be
    /// alive; a caller keeps whatever it had.
    Unreadable,
    /// The kernel has no such task (`ESRCH`): exited, or a zombie.
    Gone,
}

/// Where a PID's counters stood when it was last sampled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Baseline {
    /// `pbi_start_tvsec`; a different value under the same PID means a new
    /// process.
    start_time: u64,
    /// `pbi_status` at the sampler's first sighting, `None` when
    /// `PROC_PIDTBSDINFO` failed then. Fixed for the life of the PID, as
    /// sysinfo's `process_status` is.
    bsd_status: Option<u32>,
    /// `pti_total_user + pti_total_system`, mach absolute time units.
    task_time: u64,
    /// `mach_absolute_time` right before the task info was read.
    sampled_at: u64,
}

/// The kernel readings for one PID, separated from the arithmetic so the
/// latter can be tested without a kernel.
#[derive(Clone, Copy, Debug)]
struct Readings {
    task: Result<libc::proc_taskinfo, PidInfoError>,
    bsd: Option<libc::proc_bsdinfo>,
    thread_state: Option<i32>,
}

/// Per-PID baselines and the sampling that updates them.
#[derive(Debug, Default)]
pub struct ProcessSampler {
    baselines: HashMap<u32, Baseline>,
}

impl ProcessSampler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Sample every PID in `pids`, updating its baseline.
    ///
    /// Each inspectable PID costs three `proc_pidinfo` calls and an
    /// uninspectable one a single failed call, about 2.6 us per PID when
    /// measured on its own on an M1 Ultra. Inside a tick (`perf_tick_stages`,
    /// 986 processes) the pass over the tracked 500, which are the
    /// inspectable ones, averaged 4.1 ms, and the pass over the whole table
    /// 4.0 ms, against 16.3 ms for the sysinfo refresh it replaces.
    pub fn sample<I: IntoIterator<Item = u32>>(&mut self, pids: I) -> HashMap<u32, Sampled> {
        let now_secs = epoch_secs();
        pids.into_iter()
            .map(|pid| {
                let sampled_at = mach_now();
                let readings = Readings::read(pid);
                (pid, self.fold(pid, readings, sampled_at, now_secs))
            })
            .collect()
    }

    /// Drop the baselines of PIDs `keep` rejects, so they do not accumulate
    /// for processes that have left the cache.
    pub fn retain(&mut self, mut keep: impl FnMut(u32) -> bool) {
        self.baselines.retain(|pid, _| keep(*pid));
    }

    /// Whether `pid` has a baseline.
    #[cfg(test)]
    pub(super) fn contains(&self, pid: u32) -> bool {
        self.baselines.contains_key(&pid)
    }

    /// Number of PIDs with a baseline.
    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.baselines.len()
    }

    #[cfg(test)]
    pub(super) fn is_empty(&self) -> bool {
        self.baselines.is_empty()
    }

    /// Fold one PID's readings into its baseline and produce its sample.
    fn fold(&mut self, pid: u32, readings: Readings, sampled_at: u64, now_secs: u64) -> Sampled {
        let task = match readings.task {
            Ok(task) => task,
            Err(PidInfoError::Gone) => {
                self.baselines.remove(&pid);
                return Sampled::Gone;
            }
            Err(PidInfoError::Unreadable) => return Sampled::Unreadable,
        };
        let task_time = task.pti_total_user.saturating_add(task.pti_total_system);
        let previous = self.baselines.get(&pid).copied();
        // sysinfo keeps `start_time` 0 for a process whose BSD info it could
        // not read (`ProcessInner::new_empty`), and run time is then simply
        // "now"; the same fallback keeps run time identical here.
        let start_time = readings
            .bsd
            .map(|bsd| bsd.pbi_start_tvsec)
            .or(previous.map(|baseline| baseline.start_time))
            .unwrap_or(0);
        // A different start time under the same PID is a new process: its
        // counters have nothing to do with the old baseline.
        let previous = previous.filter(|baseline| baseline.start_time == start_time);
        let cpu_percent = cpu_percent(
            previous.map(|baseline| (baseline.task_time, baseline.sampled_at)),
            task_time,
            sampled_at,
        );
        let bsd_status = match previous {
            Some(baseline) => baseline.bsd_status,
            None => readings.bsd.map(|bsd| bsd.pbi_status),
        };
        self.baselines.insert(
            pid,
            Baseline {
                start_time,
                bsd_status,
                task_time,
                sampled_at,
            },
        );
        Sampled::Live(ProcessSample {
            cpu_percent,
            memory_rss: task.pti_resident_size,
            memory_vms: task.pti_virtual_size,
            state: state_code(bsd_status, readings.thread_state),
            run_time: now_secs.saturating_sub(start_time),
            start_time,
        })
    }
}

impl Readings {
    fn read(pid: u32) -> Self {
        let task = pidinfo_macos::read::<libc::proc_taskinfo>(pid);
        if task.is_err() {
            return Self {
                task,
                bsd: None,
                thread_state: None,
            };
        }
        Self {
            task,
            bsd: pidinfo_macos::read::<libc::proc_bsdinfo>(pid).ok(),
            thread_state: pidinfo_macos::read::<libc::proc_threadinfo>(pid)
                .ok()
                .map(|thread| thread.pth_run_state),
        }
    }
}

/// CPU percent of one core: the task-time delta over the elapsed time since
/// the previous sample, both in mach absolute time units.
///
/// `None` without a previous sample or when no time has elapsed; a counter
/// that has not moved reads `Some(0.0)`.
fn cpu_percent(previous: Option<(u64, u64)>, task_time: u64, sampled_at: u64) -> Option<f64> {
    let (previous_task_time, previous_at) = previous?;
    let elapsed = sampled_at.checked_sub(previous_at).filter(|e| *e > 0)?;
    let used = task_time.saturating_sub(previous_task_time);
    Some(used as f64 / elapsed as f64 * 100.0)
}

/// The single-letter state the process table shows, by the rule sysinfo's
/// `status()` applies on macOS (`src/unix/apple/macos/process.rs:168-177`,
/// `src/unix/apple/process.rs:9-33`), mapped through the same table
/// `convert_process_state` uses for the `Display` strings.
///
/// * BSD info unreadable at first sighting: sysinfo built the process with
///   `new_empty`, whose `process_status` is `Unknown(0)` for good: `?`.
/// * `pbi_status == SRUN` (every inspectable live process): the state of
///   thread id 0 from `PROC_PIDTHREADINFO`, running `R`, waiting `S`,
///   stopped `T`, anything else `?`; when that call fails, sysinfo assumes
///   running (`process.rs:773-780`): `R`.
/// * Any other `pbi_status` at first sighting is fixed: `I`, `S`, `T`, `Z`.
fn state_code(bsd_status: Option<u32>, thread_state: Option<i32>) -> &'static str {
    match bsd_status {
        None => "?",
        Some(libc::SRUN) => match thread_state {
            None | Some(libc::TH_STATE_RUNNING) => "R",
            Some(libc::TH_STATE_WAITING) => "S",
            Some(libc::TH_STATE_STOPPED) => "T",
            Some(_) => "?",
        },
        Some(libc::SIDL) => "I",
        Some(libc::SSLEEP) => "S",
        Some(libc::SSTOP) => "T",
        Some(libc::SZOMB) => "Z",
        Some(_) => "?",
    }
}

fn mach_now() -> u64 {
    // SAFETY: takes no arguments and has no preconditions.
    unsafe { mach_absolute_time() }
}

/// Seconds since the epoch, the clock sysinfo's `get_now` uses for run time.
fn epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
#[path = "sampler_macos/tests.rs"]
mod tests;
