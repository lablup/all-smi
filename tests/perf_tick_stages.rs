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

//! Per-stage cost of one local-mode collection tick (issue #411).
//!
//! Builds the readers the way the collectors do and replays the stages of
//! `LocalCollector::collect_steady_state` one after another at the real 1 s
//! cadence, so each stage's cost is measured on its own and at the clock
//! speeds an idle machine actually runs the tick at. The GPU pass goes first,
//! as it does in both collectors: stages that run earlier in the tick run on
//! a colder core, so the order changes what each one appears to cost. Prints markdown tables:
//! the first tick, the steady state (first tick excluded), the
//! `collect_once` breakdown on Apple Silicon, and the process CPU the
//! collection alone used while ticking. Since issue #414 the breakdown also
//! averages `IOReportCreateSamples` and the SMC over the ticks that actually
//! sampled or read, next to the per-tick rows that count reused ticks as
//! zero, and the first-tick table shows the CPU warm-up and the manager's
//! first window overlapping reader construction instead of running after it.
//!
//! ```text
//! cargo test --release --test perf_tick_stages -- --ignored --nocapture
//! PERF_TICKS=30 cargo test --release --test perf_tick_stages -- --ignored --nocapture
//! ```

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use all_smi::device::process_list::{merge_gpu_processes, update_process_cache};
use all_smi::device::{
    ProcessInfo, create_chassis_reader, get_cpu_readers, get_gpu_readers, get_memory_readers,
};
use all_smi::storage::DiskCache;
use all_smi::utils::{get_hostname, with_global_system};
use sysinfo::{DiskRefreshKind, Disks, ProcessRefreshKind, ProcessesToUpdate, UpdateKind};

/// `LocalCollector`'s constants, repeated so the replay matches it.
const FULL_REFRESH_INTERVAL: usize = 5;
const MAX_DISPLAY_PROCESSES: usize = 500;

/// One stage's samples.
#[derive(Default)]
struct Stage(Vec<Duration>);

impl Stage {
    fn push(&mut self, sample: Duration) {
        self.0.push(sample);
    }

    fn mean(&self) -> Duration {
        if self.0.is_empty() {
            return Duration::ZERO;
        }
        self.0.iter().sum::<Duration>() / self.0.len() as u32
    }

    fn row(&self, name: &str) {
        if self.0.is_empty() {
            return;
        }
        let min = self.0.iter().min().copied().unwrap_or_default();
        let max = self.0.iter().max().copied().unwrap_or_default();
        println!(
            "| {name} | {} | {} | {} | {} |",
            ms(self.mean()),
            ms(min),
            ms(max),
            self.0.len()
        );
    }
}

fn ms(duration: Duration) -> String {
    format!("{:.3} ms", duration.as_secs_f64() * 1e3)
}

/// User plus system CPU time of this process so far, all threads.
#[cfg(unix)]
fn process_cpu_time() -> Duration {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    // SAFETY: `usage` is a writable `rusage`; the call fills it on success and
    // it starts zeroed, so a failure reads as zero CPU time.
    let usage = unsafe {
        libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr());
        usage.assume_init()
    };
    let micros = |tv: libc::timeval| tv.tv_sec as u64 * 1_000_000 + tv.tv_usec as u64;
    Duration::from_micros(micros(usage.ru_utime) + micros(usage.ru_stime))
}

/// Not measured off Unix; the CPU line then reads zero.
#[cfg(not(unix))]
fn process_cpu_time() -> Duration {
    Duration::ZERO
}

#[derive(Default)]
struct Stages {
    storage: Stage,
    gpu: Stage,
    gpu_rest: Stage,
    cpu: Stage,
    memory: Stage,
    chassis: Stage,
    refresh_full: Stage,
    refresh_selective: Stage,
    cache_full: Stage,
    cache_selective: Stage,
    merge: Stage,
    tick: Stage,
    ioreport_sample: Stage,
    ioreport_parse: Stage,
    smc: Stage,
    native_other: Stage,
    native_total: Stage,
    /// `IOReportCreateSamples` on the ticks that took a sample (issue #414:
    /// the per-tick row above averages the reused ticks in as zero).
    ioreport_sample_taken: Stage,
    /// SMC on the ticks that read the temperature sensors.
    smc_read: Stage,
}

#[test]
#[ignore = "benchmark; run with --release --ignored --nocapture"]
fn perf_tick_stages() {
    let ticks: usize = std::env::var("PERF_TICKS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(11);

    let setup = Instant::now();
    // CPU readers first, as the collectors build them (issue #414).
    let cpu_readers = get_cpu_readers();
    let gpu_readers = get_gpu_readers();
    let memory_readers = get_memory_readers();
    let chassis_reader = create_chassis_reader();
    let t_construction = setup.elapsed();
    println!("reader construction: {}", ms(t_construction));

    let hostname = get_hostname();
    let mut disks = DiskCache::new();
    let mut cache: HashMap<u32, ProcessInfo> = HashMap::new();
    let mut tracked: Vec<sysinfo::Pid> = Vec::new();
    let refresh_kind = ProcessRefreshKind::nothing()
        .with_cpu()
        .with_memory()
        .with_user(UpdateKind::OnlyIfNotSet);

    let mut stages = Stages::default();
    let mut first: Vec<(&str, Duration)> = Vec::new();
    let mut steady_cpu = Duration::ZERO;
    let mut steady_wall = Duration::ZERO;

    for tick in 0..=ticks {
        if tick > 0 {
            std::thread::sleep(Duration::from_secs(1));
        }
        let (tick_wall, tick_cpu) = (Instant::now(), process_cpu_time());

        let started = Instant::now();
        let gpu_info: Vec<_> = gpu_readers.iter().flat_map(|r| r.get_gpu_info()).collect();
        let t_gpu = started.elapsed();

        let started = Instant::now();
        let mut gpu_processes = Vec::new();
        let mut gpu_pids = HashSet::new();
        for reader in gpu_readers.iter() {
            let (processes, pids) = reader.get_gpu_processes();
            gpu_processes.extend(processes);
            gpu_pids.extend(pids);
        }
        let _: Vec<_> = gpu_readers.iter().flat_map(|r| r.get_vgpu_info()).collect();
        let _: Vec<_> = gpu_readers.iter().flat_map(|r| r.get_mig_info()).collect();
        let t_gpu_rest = started.elapsed();

        let started = Instant::now();
        let _: Vec<_> = cpu_readers.iter().flat_map(|r| r.get_cpu_info()).collect();
        let t_cpu = started.elapsed();

        let started = Instant::now();
        let _: Vec<_> = memory_readers
            .iter()
            .flat_map(|r| r.get_memory_info())
            .collect();
        let t_memory = started.elapsed();

        let started = Instant::now();
        let _ = chassis_reader.get_chassis_info();
        let t_chassis = started.elapsed();

        let started = Instant::now();
        let storage = disks.storage_info(&hostname);
        let t_storage = started.elapsed();

        let full = tick % FULL_REFRESH_INTERVAL == 0 || tracked.is_empty();
        let (t_refresh, t_cache, processes) = with_global_system(|system| {
            let started = Instant::now();
            if full {
                system.refresh_processes_specifics(ProcessesToUpdate::All, true, refresh_kind);
            } else {
                system.refresh_processes_specifics(
                    ProcessesToUpdate::Some(&tracked),
                    true,
                    refresh_kind,
                );
            }
            system.refresh_memory();
            let t_refresh = started.elapsed();
            let started = Instant::now();
            let processes = update_process_cache(system, &gpu_pids, &mut cache);
            (t_refresh, started.elapsed(), processes)
        });

        let started = Instant::now();
        let process_count = processes.len();
        let mut merged = merge_gpu_processes(processes, gpu_processes);
        merged.sort_by(|a, b| {
            b.cpu_percent
                .partial_cmp(&a.cpu_percent)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        merged.truncate(MAX_DISPLAY_PROCESSES);
        tracked = merged
            .iter()
            .map(|p| sysinfo::Pid::from_u32(p.pid))
            .collect();
        let t_merge = started.elapsed();
        let (t_tick, t_tick_cpu) = (tick_wall.elapsed(), process_cpu_time() - tick_cpu);

        if tick == 0 {
            println!(
                "first tick: {process_count} processes, {} GPU rows, {} storage rows",
                gpu_info.len(),
                storage.len()
            );
            first = vec![
                ("gpu.get_gpu_info", t_gpu),
                ("cpu.get_cpu_info", t_cpu),
                ("storage (initial list)", t_storage),
                ("process refresh (full)", t_refresh),
                ("update_process_cache", t_cache),
                ("whole tick", t_tick),
                // Since #414 the warm-up waits overlap reader construction,
                // so the two only add up to time to first data together.
                (
                    "reader construction + whole tick (time to first data)",
                    t_construction + t_tick,
                ),
            ];
            continue;
        }

        steady_cpu += t_tick_cpu;
        steady_wall += t_tick;
        stages.storage.push(t_storage);
        stages.gpu.push(t_gpu);
        stages.gpu_rest.push(t_gpu_rest);
        stages.cpu.push(t_cpu);
        stages.memory.push(t_memory);
        stages.chassis.push(t_chassis);
        if full {
            stages.refresh_full.push(t_refresh);
            stages.cache_full.push(t_cache);
        } else {
            stages.refresh_selective.push(t_refresh);
            stages.cache_selective.push(t_cache);
        }
        stages.merge.push(t_merge);
        stages.tick.push(t_tick);
        record_native_timings(&mut stages);
    }

    println!("\n| first tick | time |\n|---|---|");
    for (name, duration) in &first {
        println!("| {name} | {} |", ms(*duration));
    }

    println!("\n| steady-state stage (1 s cadence) | avg | min | max | ticks |");
    println!("|---|---|---|---|---|");
    stages.gpu.row("gpu.get_gpu_info (runs collect_once)");
    stages.gpu_rest.row("gpu processes + vgpu + mig");
    stages.cpu.row("cpu.get_cpu_info");
    stages.memory.row("memory.get_memory_info");
    stages.chassis.row("chassis.get_chassis_info");
    stages.storage.row("storage (DiskCache::storage_info)");
    stages.refresh_full.row("process refresh (full, every 5th)");
    stages.refresh_selective.row("process refresh (selective)");
    stages.cache_full.row("update_process_cache (full ticks)");
    stages
        .cache_selective
        .row("update_process_cache (selective ticks)");
    stages.merge.row("merge + sort + truncate");
    stages.tick.row("whole tick");

    if !stages.native_total.0.is_empty() {
        println!("\n| collect_once breakdown | avg | min | max | ticks |");
        println!("|---|---|---|---|---|");
        stages.ioreport_sample.row("IOReportCreateSamples (floor)");
        stages.ioreport_parse.row("IOReport energy + delta + parse");
        stages.smc.row("SMC");
        stages.native_other.row("thermal + assembly");
        stages.native_total.row("collect_once total");
        stages
            .ioreport_sample_taken
            .row("IOReportCreateSamples (sampled ticks only)");
        stages.smc_read.row("SMC (temperature read ticks only)");
        println!(
            "\nIOReport sampled on {} of {} ticks; SMC temperatures read on {} of {} ticks",
            stages.ioreport_sample_taken.0.len(),
            stages.native_total.0.len(),
            stages.smc_read.0.len(),
            stages.native_total.0.len()
        );
    }

    if !steady_wall.is_zero() {
        println!(
            "\nprocess CPU during steady-state ticks: {} over {} of tick work, {:.2} % of one core at 1 s",
            ms(steady_cpu),
            ms(steady_wall),
            steady_cpu.as_secs_f64() / ticks as f64 * 100.0
        );
    }

    print_enumeration_costs();
}

/// Stage timings of the `collect_once` the GPU reader just ran.
#[cfg(target_os = "macos")]
fn record_native_timings(stages: &mut Stages) {
    let Some(timings) = all_smi::device::macos_native::get_native_metrics_manager()
        .and_then(|manager| manager.last_collection_timings())
    else {
        return;
    };
    stages.ioreport_sample.push(timings.ioreport_sample);
    stages.ioreport_parse.push(timings.ioreport_parse);
    stages.smc.push(timings.smc);
    stages.native_other.push(timings.other);
    stages.native_total.push(timings.total);
    // `ioreport_sampled` means a new window was produced. A failed or
    // too-short sample still pays for `IOReportCreateSamples` and reports
    // that in `ioreport_sample` (so it counts in the per-tick row above)
    // but is not a sampled tick here.
    if timings.ioreport_sampled {
        stages.ioreport_sample_taken.push(timings.ioreport_sample);
    }
    if timings.smc_temperatures_read {
        stages.smc_read.push(timings.smc);
    }
}

#[cfg(not(target_os = "macos"))]
fn record_native_timings(_stages: &mut Stages) {}

/// What enumerating the mount table costs: the background list refresh
/// `DiskCache` runs every 30 s, and the full enumeration storage collection
/// used to run on every tick.
fn print_enumeration_costs() {
    let mut list_refresh = Stage::default();
    let mut old_per_tick = Stage::default();
    for _ in 0..5 {
        std::thread::sleep(Duration::from_secs(1));
        let started = Instant::now();
        let disks =
            Disks::new_with_refreshed_list_specifics(DiskRefreshKind::nothing().with_storage());
        list_refresh.push(started.elapsed());
        drop(disks);

        std::thread::sleep(Duration::from_millis(500));
        let started = Instant::now();
        let disks = Disks::new_with_refreshed_list();
        old_per_tick.push(started.elapsed());
        drop(disks);
    }
    println!("\n| mount table enumeration | avg | min | max | runs |");
    println!("|---|---|---|---|---|");
    list_refresh.row("DiskCache list refresh (background, every 30 s)");
    old_per_tick.row("Disks::new_with_refreshed_list (old per-tick path)");
}
