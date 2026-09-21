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

//! Per-process CPU, memory, state and run time on Linux, read in-process
//! from `/proc/<pid>/stat` instead of sysinfo's per-tick refresh (issue
//! #428).
//!
//! The local collector refreshes the tracked top-500 PIDs every tick and
//! every PID every fifth tick. On Linux, sysinfo 0.39.6 makes a process
//! outside the tracked set read about five times its real CPU percent:
//!
//! 1. **One global interval for every process.** `update_procs_cpu`
//!    (sysinfo, `src/unix/linux/system.rs:188-207`) recomputes `cpu_usage`
//!    for *every* process in its map on every refresh call, dividing each
//!    process's task-time delta by the total CPU tick delta since the
//!    previous refresh call divided by the CPU count (`compute_cpu_usage`,
//!    `src/unix/linux/process.rs:346-361`). `set_time`
//!    (`process.rs:363-368`) moves a process's own baseline only when that
//!    process was actually refreshed, so a process outside the tracked set
//!    keeps a five-tick task-time delta while the divisor is a one-tick
//!    interval. Unlike macOS this happens on every tick, not only on full
//!    ticks, because the recomputation loop also covers processes the
//!    current call did not refresh: their five-tick delta keeps being
//!    divided by fresh one-tick intervals. The only guard is the
//!    `cpus * 100` clamp, which does nothing below the ceiling. That
//!    inflated number is the sort key for the displayed list, so unrelated
//!    processes jump onto the screen, and the inflated ranking decides the
//!    next tick's tracked set. Windows is immune (it keeps per-process
//!    copies of the global times, `src/windows/process.rs:1049-1105`);
//!    macOS has the same class of defect through a different path and is
//!    fixed natively in #427.
//!
//! This sampler keeps, per PID, the previous `utime + stime` and the
//! `CLOCK_MONOTONIC` time at which it was read, and reports CPU percent as
//! the task-time delta over that PID's own elapsed wall time, per core:
//! `used_ticks / clk_tck / elapsed_seconds * 100`. A PID it has not seen
//! before reports no CPU reading, as sysinfo does for a process it just
//! discovered (`compute_cpu_usage` requires a non-zero previous total); a
//! counter that does not move reads 0, which is the deliberate difference
//! from sysinfo's stale value. Memory comes from the same `/proc/<pid>/stat`
//! fields sysinfo reads (`rss` times the page size, `vsize` bytes), the
//! state code is derived by the same rule sysinfo's `ProcessStatus::from`
//! applies (see [`state_code`]), and `start_time` is `/proc/stat`'s `btime`
//! plus `starttime / clk_tck`, as sysinfo's `start_time()`, with `run_time`
//! measured the way sysinfo's `run_time()` measures it, against
//! `/proc/uptime`.
//!
//! What the sampler does *not* do: discover processes or supply their
//! static metadata (name, user, parent, command). sysinfo still does that
//! on every fifth tick; see `refresh_linux` for how the two are combined.

use std::collections::HashMap;

/// One PID's readings for this tick.
#[derive(Clone, Debug, PartialEq)]
pub struct ProcessSample {
    /// Task-time delta over the sampler's elapsed time for this PID, as a
    /// percentage of one core. `None` on the first sighting of a PID (or of
    /// a reused PID), when there is nothing to take a delta against.
    pub cpu_percent: Option<f64>,
    /// `rss` (field 24) times the page size, bytes.
    pub memory_rss: u64,
    /// `vsize` (field 23), bytes.
    pub memory_vms: u64,
    /// Single-letter state code as the process table shows it.
    pub state: &'static str,
    /// Seconds since the process started, as sysinfo's `run_time()`.
    pub run_time: u64,
    /// `/proc/stat`'s `btime` plus `starttime / clk_tck`, epoch seconds:
    /// what tells a reused PID apart from the process that had it before.
    pub start_time: u64,
}

/// What sampling one PID found.
#[derive(Clone, Debug, PartialEq)]
pub enum Sampled {
    /// The process is alive and readable.
    Live(ProcessSample),
    /// The kernel would not say (`EPERM`, a malformed or short read). The
    /// process may be alive; a caller keeps whatever it had.
    Unreadable,
    /// The kernel has no such process (`ENOENT`, an empty read): exited.
    Gone,
}

/// Where a PID's counters stood when it was last sampled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Baseline {
    /// `starttime` (field 22), clock ticks since boot; a different value
    /// under the same PID means a new process.
    start_time: u64,
    /// `utime + stime` (fields 14 + 15), clock ticks.
    task_time: u64,
    /// `CLOCK_MONOTONIC` nanoseconds right before the stat was read.
    sampled_at: u64,
}

/// The system constants the sampler's arithmetic needs, separated from the
/// `/proc` reads so tests can feed fixed values.
#[derive(Clone, Copy, Debug)]
struct Constants {
    /// `sysconf(_SC_CLK_TCK)`, clock ticks per second.
    clk_tck: u64,
    /// `sysconf(_SC_PAGESIZE)`, bytes.
    page_size: u64,
    /// `/proc/stat`'s `btime`, epoch seconds, the clock sysinfo's Linux
    /// `start_time()` adds to a process's start time.
    btime: u64,
}

impl Constants {
    fn read(btime: u64) -> Self {
        // SAFETY: sysconf takes no pointers and has no preconditions; both
        // constants are always valid on Linux. A failure reads as -1, which
        // the `max` turns into a division-safe 1.
        Constants {
            clk_tck: unsafe { libc::sysconf(libc::_SC_CLK_TCK) }.max(1) as u64,
            page_size: unsafe { libc::sysconf(libc::_SC_PAGESIZE) }.max(1) as u64,
            btime,
        }
    }
}

/// The kernel readings for one PID, separated from the arithmetic so the
/// latter can be tested without a kernel.
#[derive(Clone, Copy, Debug)]
struct Readings {
    stat: Result<Stat, StatError>,
}

/// One process's `/proc/<pid>/stat` fields.
#[derive(Clone, Copy, Debug)]
struct Stat {
    state: Option<char>,
    /// `utime + stime`, clock ticks.
    task_time: u64,
    /// `starttime`, clock ticks since boot.
    start_time_raw: u64,
    /// `vsize`, bytes.
    vsize: u64,
    /// `rss`, pages.
    rss_pages: u64,
}

/// Why a `/proc/<pid>/stat` read found nothing usable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StatError {
    /// `ENOENT` or an empty read: the process exited.
    Gone,
    /// `EPERM`, or a malformed or short read: the process may be alive.
    Unreadable,
}

/// Per-PID baselines and the sampling that updates them.
#[derive(Debug, Default)]
pub struct ProcessSampler {
    baselines: HashMap<u32, Baseline>,
    /// `/proc/stat`'s boot time, read on the first sample; it only changes
    /// on reboot.
    btime: Option<u64>,
}

impl ProcessSampler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Sample every PID in `pids`, updating its baseline.
    ///
    /// Each PID costs one `/proc/<pid>/stat` read, the same file sysinfo
    /// reads for a refreshed process, so the pass over the tracked 500 is
    /// 500 small reads instead of sysinfo's O(table) refresh.
    pub fn sample<I: IntoIterator<Item = u32>>(&mut self, pids: I) -> HashMap<u32, Sampled> {
        let btime = *self.btime.get_or_insert_with(read_btime);
        let constants = Constants::read(btime);
        let uptime = uptime_secs();
        pids.into_iter()
            .map(|pid| {
                let sampled_at = mono_now();
                let readings = Readings::read(pid);
                (
                    pid,
                    self.fold(pid, readings, sampled_at, uptime, &constants),
                )
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
    fn fold(
        &mut self,
        pid: u32,
        readings: Readings,
        sampled_at: u64,
        uptime: u64,
        constants: &Constants,
    ) -> Sampled {
        let stat = match readings.stat {
            Ok(stat) => stat,
            Err(StatError::Gone) => {
                self.baselines.remove(&pid);
                return Sampled::Gone;
            }
            Err(StatError::Unreadable) => return Sampled::Unreadable,
        };
        let previous = self.baselines.get(&pid).copied();
        // A different start time under the same PID is a new process: its
        // counters have nothing to do with the old baseline.
        let previous = previous.filter(|baseline| baseline.start_time == stat.start_time_raw);
        let cpu_percent = cpu_percent(
            previous.map(|baseline| (baseline.task_time, baseline.sampled_at)),
            stat.task_time,
            sampled_at,
            constants.clk_tck,
        );
        self.baselines.insert(
            pid,
            Baseline {
                start_time: stat.start_time_raw,
                task_time: stat.task_time,
                sampled_at,
            },
        );
        Sampled::Live(ProcessSample {
            cpu_percent,
            memory_rss: stat.rss_pages.saturating_mul(constants.page_size),
            memory_vms: stat.vsize,
            state: state_code(stat.state),
            run_time: uptime.saturating_sub(stat.start_time_raw / constants.clk_tck),
            start_time: constants
                .btime
                .saturating_add(stat.start_time_raw / constants.clk_tck),
        })
    }
}

impl Readings {
    fn read(pid: u32) -> Self {
        Readings {
            stat: read_stat(pid),
        }
    }
}

/// The `/proc/<pid>/stat` fields the sampler keeps, indexed after the `comm`
/// parentheses (`proc(5)`): 0 state, 11 utime, 12 stime, 19 starttime,
/// 20 vsize, 21 rss.
const STATE: usize = 0;
const UTIME: usize = 11;
const STIME: usize = 12;
const START_TIME: usize = 19;
const VSIZE: usize = 20;
const RSS: usize = 21;

/// One `/proc/<pid>/stat` read.
fn read_stat(pid: u32) -> Result<Stat, StatError> {
    let data = std::fs::read_to_string(format!("/proc/{pid}/stat")).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            StatError::Gone
        } else {
            StatError::Unreadable
        }
    })?;
    if data.is_empty() {
        return Err(StatError::Gone);
    }
    parse_stat(&data).ok_or(StatError::Unreadable)
}

/// Parse the stat fields out of one `/proc/<pid>/stat` read.
fn parse_stat(data: &str) -> Option<Stat> {
    // The process name is in parentheses and may itself contain spaces and
    // parens, so the fields start after the last `)`, the way
    // `get_process_priority_nice` in `process_list.rs` already splits it.
    let after_name = data.rfind(')')? + 1;
    let fields: Vec<&str> = data[after_name..].split_whitespace().collect();
    if fields.len() <= RSS {
        return None;
    }
    Some(Stat {
        state: fields[STATE].chars().next(),
        task_time: fields[UTIME]
            .parse::<u64>()
            .unwrap_or(0)
            .saturating_add(fields[STIME].parse::<u64>().unwrap_or(0)),
        start_time_raw: fields[START_TIME].parse().unwrap_or(0),
        vsize: fields[VSIZE].parse().unwrap_or(0),
        rss_pages: fields[RSS].parse().unwrap_or(0),
    })
}

/// CPU percent of one core: the task-time delta (clock ticks) over the
/// elapsed time since the previous sample, scaled by `clk_tck` ticks per
/// second. Per core, so it can exceed 100, as sysinfo's can.
///
/// `None` without a previous sample or when no time has elapsed; a counter
/// that has not moved reads `Some(0.0)`.
fn cpu_percent(
    previous: Option<(u64, u64)>,
    task_time: u64,
    sampled_at: u64,
    clk_tck: u64,
) -> Option<f64> {
    let (previous_task_time, previous_at) = previous?;
    let elapsed = sampled_at
        .checked_sub(previous_at)
        .filter(|elapsed| *elapsed > 0)?;
    let used = task_time.saturating_sub(previous_task_time);
    let seconds = used as f64 / clk_tck as f64;
    Some(seconds / (elapsed as f64 / 1e9) * 100.0)
}

/// The single-letter state the process table shows, by the rule sysinfo's
/// `ProcessStatus::from(char)` applies on Linux
/// (`src/unix/linux/process.rs:37-52`), mapped through the same table
/// `convert_process_state` uses for the `Display` strings.
///
/// `R`, `S`, `I`, `D`, `Z`, `T` and `X` (and `x`) map to their own letters,
/// while `t` (Tracing), `K` (Wakekill), `W` (Waking), `P` (Parked) and
/// anything else map through sysinfo's `Display` strings to `?`.
fn state_code(state: Option<char>) -> &'static str {
    match state {
        Some('R') => "R",
        Some('S') => "S",
        Some('I') => "I",
        Some('D') => "D",
        Some('Z') => "Z",
        Some('T') => "T",
        Some('X') | Some('x') => "X",
        _ => "?",
    }
}

fn mono_now() -> u64 {
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `now` is a writable timespec; the call fills it on success and
    // it starts zeroed, so a failure reads as zero elapsed time.
    unsafe {
        libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now);
    }
    now.tv_sec as u64 * 1_000_000_000 + now.tv_nsec as u64
}

/// Seconds since boot from `/proc/uptime`, the clock sysinfo's `run_time()`
/// measures against (`src/unix/linux/system.rs:349-357`).
fn uptime_secs() -> u64 {
    let Ok(uptime) = std::fs::read_to_string("/proc/uptime") else {
        return 0;
    };
    uptime
        .split('.')
        .next()
        .and_then(|secs| secs.parse().ok())
        .unwrap_or(0)
}

/// Boot time from `/proc/stat`'s `btime` line, the clock sysinfo's Linux
/// `start_time()` adds to a process's start time.
fn read_btime() -> u64 {
    let Ok(stat) = std::fs::read_to_string("/proc/stat") else {
        return 0;
    };
    for line in stat.lines() {
        if line.starts_with("btime") {
            return line
                .split_whitespace()
                .nth(1)
                .and_then(|secs| secs.parse().ok())
                .unwrap_or(0);
        }
    }
    0
}

#[cfg(test)]
#[path = "sampler_linux/tests.rs"]
mod tests;
