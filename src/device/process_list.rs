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

use crate::device::types::ProcessInfo;
use std::collections::{HashMap, HashSet};
use sysinfo::{Pid, ProcessRefreshKind, ProcessStatus, System, ThreadKind, UpdateKind};

#[path = "process_list/collect.rs"]
mod collect;
#[path = "process_list/row_caches.rs"]
mod row_caches;

pub use collect::collect_process_rows;

#[cfg(target_os = "macos")]
#[path = "process_list/pidinfo_macos.rs"]
mod pidinfo_macos;
#[cfg(target_os = "macos")]
#[path = "process_list/priority_macos.rs"]
mod priority_macos;
#[cfg(target_os = "macos")]
#[path = "process_list/refresh_macos.rs"]
pub mod refresh_macos;
#[cfg(target_os = "macos")]
#[path = "process_list/sampler_macos.rs"]
pub mod sampler_macos;

#[cfg(target_os = "macos")]
pub use refresh_macos::refresh_processes;
#[cfg(target_os = "macos")]
pub use sampler_macos::ProcessSampler;

#[cfg(target_os = "linux")]
#[path = "process_list/refresh_linux.rs"]
pub mod refresh_linux;
#[cfg(target_os = "linux")]
#[path = "process_list/sampler_linux.rs"]
pub mod sampler_linux;

#[cfg(target_os = "linux")]
pub use refresh_linux::refresh_processes;
#[cfg(target_os = "linux")]
pub use sampler_linux::ProcessSampler;

/// Share of system memory held by a process, in percent.
///
/// Returns 0 when `total_memory` is unknown (a `System` whose memory was never
/// refreshed reports 0), instead of a NaN that serde_json serializes as `null`.
fn memory_percent(process_memory: u64, total_memory: u64) -> f64 {
    if total_memory == 0 {
        return 0.0;
    }
    (process_memory as f64 / total_memory as f64) * 100.0
}

/// The sysinfo refresh kind for every refresh whose result becomes process
/// rows (issue #504): the GPU readers' `get_process_info`, the Linux and
/// non-Unix TUI process passes, and the Intel enrichment.
///
/// It reads exactly what [`get_all_processes`] and the TUI cache use:
///
/// * CPU and memory, refreshed every time.
/// * The user id, read once per process (`OnlyIfNotSet`); it is fixed for
///   the process's lifetime.
/// * The command line, read once per process (`OnlyIfNotSet`). sysinfo does
///   not read `cmdline` at all when `cmd` is `Never`, which left the Linux
///   TUI showing `[name]` for every process it discovered. Kernel threads
///   have an empty command line, so they are re-read on each full refresh;
///   that is one small read per kernel thread.
/// * No tasks: with tasks on, `ProcessesToUpdate::All` adds every userland
///   thread to the process map (and API output got one row per thread), and
///   reads `/proc/<pid>/task` plus a stat/status pair per thread.
/// * No disk usage, `exe`, `environ`, `cwd` or `root`: nothing displays
///   them, and with `OnlyIfNotSet` they were re-attempted every refresh for
///   every process whose value is unreadable (all kernel threads, and every
///   other user's process when not root).
pub(crate) fn process_row_refresh_kind() -> ProcessRefreshKind {
    ProcessRefreshKind::nothing()
        .with_cpu()
        .with_memory()
        .with_user(UpdateKind::OnlyIfNotSet)
        .with_cmd(UpdateKind::OnlyIfNotSet)
        .without_tasks()
}

/// Builds process rows with the process-wide priority and user caches held
/// for one pass, so a pass takes each lock once instead of once per PID.
struct RowBuilder<'a> {
    system: &'a System,
    total_memory: u64,
    #[cfg(target_os = "linux")]
    priorities: std::sync::MutexGuard<'static, row_caches::PriorityCache>,
    #[cfg(unix)]
    users: std::sync::MutexGuard<'static, row_caches::UserCache>,
}

impl<'a> RowBuilder<'a> {
    fn new(system: &'a System) -> Self {
        Self {
            system,
            total_memory: system.total_memory(),
            #[cfg(target_os = "linux")]
            priorities: row_caches::priority_cache(),
            #[cfg(unix)]
            users: row_caches::user_cache(),
        }
    }

    /// The row for one process, or `None` for a userland thread, which is
    /// not a process of its own.
    fn row(&mut self, pid: u32, process: &sysinfo::Process, uses_gpu: bool) -> Option<ProcessInfo> {
        if process.thread_kind() == Some(ThreadKind::Userland) {
            return None;
        }
        #[cfg(target_os = "linux")]
        let (priority, nice_value) =
            self.priorities
                .get_or_read(pid, process.start_time(), &mut get_process_priority_nice);
        #[cfg(not(target_os = "linux"))]
        let (priority, nice_value) = get_process_priority_nice(pid);
        #[cfg(unix)]
        let user = process_user_with(&mut self.users, process);
        #[cfg(not(unix))]
        let user = process_user_with(process);

        Some(ProcessInfo {
            device_id: 0, // Will be set by GPU-specific code if uses_gpu
            device_uuid: if uses_gpu {
                "GPU".to_string()
            } else {
                String::new()
            },
            pid,
            process_name: process.name().to_string_lossy().to_string(),
            used_memory: 0, // GPU memory, will be set by GPU-specific code
            cpu_percent: process.cpu_usage() as f64,
            memory_percent: memory_percent(process.memory(), self.total_memory),
            memory_rss: process.memory(),         // Already in bytes
            memory_vms: process.virtual_memory(), // Already in bytes
            user,
            state: convert_process_state(process.status()),
            start_time: format!("{}", process.start_time()),
            cpu_time: process.run_time(),
            command: get_process_command(process),
            ppid: process.parent().map(|p| p.as_u32()).unwrap_or(0),
            threads: 1, // sysinfo doesn't provide thread count directly
            uses_gpu,
            priority,
            nice_value,
            gpu_utilization: 0.0, // Will be set by GPU-specific code
        })
    }

    /// Evict cached priorities of PIDs that left the process table.
    fn finish(self) {
        #[cfg(target_os = "linux")]
        {
            let system = self.system;
            let mut priorities = self.priorities;
            priorities.retain_live(|pid| system.process(Pid::from_u32(pid)).is_some());
        }
        #[cfg(not(target_os = "linux"))]
        let _ = self.system;
    }
}

/// Get all system processes with GPU usage information.
///
/// One row per process in `system`, sorted by PID; userland threads are
/// skipped. Priority and nice come from a process-wide cache keyed by PID and
/// start time, so a repeated call over the same processes reads no
/// `/proc/<pid>/stat` (issue #504).
pub fn get_all_processes(system: &System, gpu_pids: &HashSet<u32>) -> Vec<ProcessInfo> {
    let mut builder = RowBuilder::new(system);
    let mut processes: Vec<ProcessInfo> = system
        .processes()
        .iter()
        .filter_map(|(pid, process)| {
            let pid = pid.as_u32();
            builder.row(pid, process, gpu_pids.contains(&pid))
        })
        .collect();
    builder.finish();

    // Sort by PID for consistent ordering
    processes.sort_by_key(|p| p.pid);
    processes
}

/// Rows for just `pids`, the rows [`get_all_processes`] would build for them
/// with `pids` as the GPU set (each row has `uses_gpu` set), sorted by PID.
///
/// For a reader that only needs metadata for its handful of accelerator PIDs:
/// pair it with `ProcessesToUpdate::Some` and [`process_row_refresh_kind`] so
/// neither the refresh nor the row building walks the whole process table.
/// PIDs `system` does not hold are skipped.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn rows_for_pids(system: &System, pids: &HashSet<u32>) -> Vec<ProcessInfo> {
    let mut builder = RowBuilder::new(system);
    let mut processes: Vec<ProcessInfo> = pids
        .iter()
        .filter_map(|&pid| {
            let process = system.process(Pid::from_u32(pid))?;
            builder.row(pid, process, true)
        })
        .collect();
    builder.finish();
    processes.sort_by_key(|p| p.pid);
    processes
}

/// Update a process cache in place, reusing existing ProcessInfo objects where possible.
/// This reduces memory allocation overhead compared to creating new objects each cycle.
/// Returns a Vec of ProcessInfo cloned from the cache for the current snapshot.
///
/// On macOS and Linux the per-tick pass is `refresh_processes` instead, which reads the
/// dynamic values natively and runs sysinfo only on full ticks (issues #427 and #428).
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn update_process_cache(
    system: &System,
    gpu_pids: &HashSet<u32>,
    cache: &mut HashMap<u32, ProcessInfo>,
) -> Vec<ProcessInfo> {
    // Track which PIDs are still alive this cycle
    let mut seen_pids: HashSet<u32> = HashSet::with_capacity(system.processes().len());
    let total_memory = system.total_memory();

    for (pid, process) in system.processes() {
        let pid_u32 = pid.as_u32();
        seen_pids.insert(pid_u32);

        let uses_gpu = gpu_pids.contains(&pid_u32);

        if let Some(cached) = cache.get_mut(&pid_u32) {
            // Update existing entry - only update dynamic fields to reduce allocations
            refresh_entry_from_sysinfo(cached, process, total_memory);
            // Update GPU status (may change if process starts/stops using GPU)
            mark_gpu(cached, uses_gpu);
            // Note: Static fields like process_name, user, command, start_time, ppid are kept unchanged
            // They don't change during process lifetime
        } else {
            // New process - create full ProcessInfo entry
            cache.insert(
                pid_u32,
                new_entry_from_sysinfo(pid_u32, process, total_memory, uses_gpu),
            );
        }
    }

    // Remove stale entries (processes that no longer exist)
    cache.retain(|pid, _| seen_pids.contains(pid));

    // Return a clone of the cached data as a Vec
    let mut processes: Vec<ProcessInfo> = cache.values().cloned().collect();
    processes.sort_by_key(|p| p.pid);
    processes
}

/// A full row for a process sysinfo has just discovered.
fn new_entry_from_sysinfo(
    pid: u32,
    process: &sysinfo::Process,
    total_memory: u64,
    uses_gpu: bool,
) -> ProcessInfo {
    let (priority, nice_value) = get_process_priority_nice(pid);
    ProcessInfo {
        device_id: 0,
        device_uuid: if uses_gpu {
            "GPU".to_string()
        } else {
            String::new()
        },
        pid,
        process_name: process.name().to_string_lossy().to_string(),
        used_memory: 0,
        cpu_percent: process.cpu_usage() as f64,
        memory_percent: memory_percent(process.memory(), total_memory),
        memory_rss: process.memory(),
        memory_vms: process.virtual_memory(),
        user: get_process_user(process),
        state: convert_process_state(process.status()),
        start_time: format!("{}", process.start_time()),
        cpu_time: process.run_time(),
        command: get_process_command(process),
        ppid: process.parent().map(|p| p.as_u32()).unwrap_or(0),
        threads: 1,
        uses_gpu,
        priority,
        nice_value,
        gpu_utilization: 0.0,
    }
}

/// The dynamic fields of an existing row, from sysinfo's refreshed process.
fn refresh_entry_from_sysinfo(
    cached: &mut ProcessInfo,
    process: &sysinfo::Process,
    total_memory: u64,
) {
    cached.cpu_percent = process.cpu_usage() as f64;
    cached.memory_percent = memory_percent(process.memory(), total_memory);
    cached.memory_rss = process.memory();
    cached.memory_vms = process.virtual_memory();
    cached.state = convert_process_state(process.status());
    cached.cpu_time = process.run_time();
}

/// GPU attribution for this cycle; a process may start or stop using the GPU.
fn mark_gpu(cached: &mut ProcessInfo, uses_gpu: bool) {
    cached.uses_gpu = uses_gpu;
    if uses_gpu && cached.device_uuid.is_empty() {
        cached.device_uuid = "GPU".to_string();
    }
}

/// Convert sysinfo ProcessStatus to standard Unix state code
fn convert_process_state(status: ProcessStatus) -> String {
    // Convert the status to string and then map to single-letter codes
    let status_str = status.to_string();

    match status_str.as_str() {
        "Run" | "Runnable" | "Running" => "R",            // Running
        "Sleep" | "Sleeping" => "S",                      // Sleeping
        "Idle" => "I",                                    // Idle
        "Stop" | "Stopped" => "T",                        // Stopped (traced)
        "Zombie" => "Z",                                  // Zombie
        "Dead" => "X",                                    // Dead
        "Disk Sleep" | "UninterruptibleDiskSleep" => "D", // Uninterruptible disk sleep
        "Unknown" => "?",                                 // Unknown
        _ => "?",                                         // Any other state
    }
    .to_string()
}

/// Get process user name.
///
/// On Unix the uid is resolved through the process-wide uid cache
/// (issue #504); the name is the passwd name, the numeric uid when there is
/// no entry, or `"unknown"` when sysinfo has no uid.
fn get_process_user(process: &sysinfo::Process) -> String {
    #[cfg(unix)]
    {
        process_user_with(&mut row_caches::user_cache(), process)
    }
    #[cfg(not(unix))]
    {
        process_user_with(process)
    }
}

#[cfg(unix)]
fn process_user_with(cache: &mut row_caches::UserCache, process: &sysinfo::Process) -> String {
    match process.user_id() {
        Some(user_id) => cache.resolve(**user_id, row_caches::lookup_user_name),
        None => "unknown".to_string(),
    }
}

#[cfg(not(unix))]
fn process_user_with(process: &sysinfo::Process) -> String {
    match process.user_id() {
        Some(user_id) => user_id.to_string(),
        None => "unknown".to_string(),
    }
}

/// Get process command line
fn get_process_command(process: &sysinfo::Process) -> String {
    let cmd = process.cmd();
    if cmd.is_empty() {
        format!("[{}]", process.name().to_string_lossy())
    } else {
        // Convert OsStr arguments to String
        cmd.iter()
            .map(|s| s.to_string_lossy().to_string())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// Get process priority and nice value.
///
/// On macOS both come from libc in-process; see `priority_macos` for what the
/// priority column means there and why nothing is spawned.
#[cfg(target_os = "macos")]
fn get_process_priority_nice(pid: u32) -> (i32, i32) {
    priority_macos::priority_nice(pid)
}

/// Get process priority and nice value
#[cfg(not(target_os = "macos"))]
#[allow(unused_variables)]
fn get_process_priority_nice(pid: u32) -> (i32, i32) {
    #[cfg(target_os = "linux")]
    {
        // On Linux, read from /proc/[pid]/stat
        // Format: pid (comm) state ppid ... priority nice ...
        // Priority is field 18 (17 0-indexed), Nice is field 19 (18 0-indexed)
        if let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            // The process name is in parentheses and may contain spaces/parens
            // Find the last ) to properly split the fields
            if let Some(last_paren) = stat.rfind(')') {
                // Everything after the last ) contains the actual stat fields
                let after_name = &stat[last_paren + 1..];
                let fields: Vec<&str> = after_name.split_whitespace().collect();

                // After removing pid and (name), the remaining fields are:
                // 0: state, 1: ppid, 2: pgrp, 3: session, 4: tty_nr, 5: tpgid,
                // 6: flags, 7: minflt, 8: cminflt, 9: majflt, 10: cmajflt,
                // 11: utime, 12: stime, 13: cutime, 14: cstime,
                // 15: priority, 16: nice, ...
                if fields.len() > 16 {
                    let priority = fields
                        .get(15) // priority is at index 15 after the name
                        .and_then(|s| s.parse::<i32>().ok())
                        .unwrap_or(20);
                    let nice = fields
                        .get(16) // nice is at index 16 after the name
                        .and_then(|s| s.parse::<i32>().ok())
                        .unwrap_or(0);
                    return (priority, nice);
                }
            }
        }
    }

    // On Windows, process priority is managed differently than Unix systems.
    // Windows uses priority classes (Idle, Below Normal, Normal, Above Normal, High, Realtime)
    // rather than nice values. The sysinfo crate doesn't expose priority directly.
    // Return default values as Unix-style nice values don't apply to Windows.
    // Default values if unable to retrieve (for Windows or any other OS)
    (20, 0)
}

/// Merge GPU process information with system process list while preserving per-device rows.
pub fn merge_gpu_processes(
    all_processes: Vec<ProcessInfo>,
    gpu_processes: Vec<ProcessInfo>,
) -> Vec<ProcessInfo> {
    // PID-only merge is invalid for multi-GPU workloads because one PID can have
    // multiple independent GPU contexts. Key by (PID, device UUID) instead.
    let mut gpu_map: HashMap<(u32, String), ProcessInfo> = HashMap::new();
    for gpu_process in gpu_processes {
        let key = (gpu_process.pid, gpu_process.device_uuid.clone());

        // NVML/CLI sources can report overlapping rows for the same (PID, device).
        // Use max() defensively to avoid inflating memory/utilization by double-counting.
        gpu_map
            .entry(key)
            .and_modify(|existing| {
                existing.used_memory = existing.used_memory.max(gpu_process.used_memory);
                existing.gpu_utilization =
                    existing.gpu_utilization.max(gpu_process.gpu_utilization);
            })
            .or_insert(gpu_process);
    }

    let process_by_pid: HashMap<u32, ProcessInfo> = all_processes
        .into_iter()
        .map(|process| (process.pid, process))
        .collect();

    let mut merged = Vec::with_capacity(process_by_pid.len() + gpu_map.len());
    let mut pids_with_gpu_rows: HashSet<u32> = HashSet::with_capacity(gpu_map.len());

    for ((_pid, _device_uuid), gpu_process) in gpu_map {
        pids_with_gpu_rows.insert(gpu_process.pid);
        // A PID with several GPU contexts needs one copy of its base row per
        // device, so only these rows are cloned.
        if let Some(base) = process_by_pid.get(&gpu_process.pid) {
            let mut merged_row = base.clone();
            merged_row.device_id = gpu_process.device_id;
            merged_row.device_uuid = gpu_process.device_uuid;
            merged_row.used_memory = gpu_process.used_memory;
            merged_row.gpu_utilization = gpu_process.gpu_utilization;
            merged_row.uses_gpu = true;
            merged.push(merged_row);
        } else {
            merged.push(gpu_process);
        }
    }

    // Keep baseline rows that did not receive any GPU-attributed row, moved
    // rather than cloned: this is every non-GPU process on every tick
    // (issue #504).
    merged.extend(
        process_by_pid
            .into_values()
            .filter(|process| !pids_with_gpu_rows.contains(&process.pid)),
    );

    // Stable ordering for deterministic API output and tests.
    merged.sort_by(|a, b| {
        a.pid
            .cmp(&b.pid)
            .then_with(|| a.device_uuid.cmp(&b.device_uuid))
    });
    merged
}

#[cfg(test)]
#[path = "process_list/tests.rs"]
pub(super) mod tests;
