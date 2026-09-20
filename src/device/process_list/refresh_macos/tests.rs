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

use super::*;
use crate::device::process_list::convert_process_state;
use crate::utils::command::new_command;
use std::process::{Child, Stdio};

/// A child that is killed and reaped when the test ends, however it ends.
struct Guarded(Child);

impl Guarded {
    fn spawn(program: &str, args: &[&str]) -> Option<Self> {
        new_command(program)
            .args(args)
            .stdout(Stdio::null())
            .spawn()
            .ok()
            .map(Self)
    }

    fn pid(&self) -> u32 {
        self.0.id()
    }
}

impl Drop for Guarded {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The three pieces of state the collector keeps between ticks.
struct Harness {
    system: System,
    sampler: ProcessSampler,
    cache: HashMap<u32, ProcessInfo>,
}

impl Harness {
    fn new() -> Self {
        Self {
            system: System::new(),
            sampler: ProcessSampler::new(),
            cache: HashMap::new(),
        }
    }

    fn tick(&mut self, tracked: &[Pid], full: bool) -> Vec<ProcessInfo> {
        refresh_processes(
            &mut self.system,
            &mut self.sampler,
            tracked,
            full,
            &HashSet::new(),
            &mut self.cache,
        )
        .0
    }

    /// Every cached PID except `excluded`, as the tracked set.
    fn tracked_without(&self, excluded: u32) -> Vec<Pid> {
        self.cache
            .keys()
            .filter(|pid| **pid != excluded)
            .map(|pid| Pid::from_u32(*pid))
            .collect()
    }
}

fn row(rows: &[ProcessInfo], pid: u32) -> Option<&ProcessInfo> {
    rows.iter().find(|row| row.pid == pid)
}

fn sleep_one_second() {
    std::thread::sleep(Duration::from_secs(1));
}

/// Issue #427: a tracked process that exits between ticks leaves the list on
/// the next selective tick, as it did when sysinfo's selective refresh with
/// `remove_dead_processes` pruned it. The sampler must report it gone, and
/// its baseline must go with it.
#[test]
fn a_dead_tracked_process_leaves_on_the_next_selective_tick() {
    let Some(child) = Guarded::spawn("/bin/sleep", &["30"]) else {
        return;
    };
    let pid = child.pid();
    let mut harness = Harness::new();

    let rows = harness.tick(&[], true);
    assert!(
        row(&rows, pid).is_some(),
        "the full tick discovers the child"
    );
    assert!(
        harness.sampler.contains(pid),
        "and the sampler baselines it"
    );
    // Baselines exist only for inspectable processes, the cache for all.
    assert!(harness.sampler.len() <= harness.cache.len());

    drop(child);
    let rows = harness.tick(&[Pid::from_u32(pid)], false);
    assert!(
        row(&rows, pid).is_none(),
        "a dead tracked process must leave on the next selective tick"
    );
    assert!(!harness.cache.contains_key(&pid));
    assert!(!harness.sampler.contains(pid), "its baseline goes with it");
}

/// Issue #427, defect 2: a process outside the tracked set is sampled only
/// on full ticks. Its CPU percent there must be its share over the five
/// ticks since its baseline, not five times its share. `yes` burns about one
/// core; sysinfo read it at 501.87 on the full tick, and the bound here is a
/// ratio against a fresh one-second reading so a busy CI runner that gives
/// `yes` less than a core still passes while the defect (5x) still fails.
#[test]
fn full_tick_cpu_percent_is_not_inflated() {
    let Some(busy) = Guarded::spawn("/usr/bin/yes", &[]) else {
        return;
    };
    let pid = busy.pid();
    let mut harness = Harness::new();

    let _ = harness.tick(&[], true);
    let tracked = harness.tracked_without(pid);
    for _ in 0..4 {
        sleep_one_second();
        let rows = harness.tick(&tracked, false);
        assert!(
            row(&rows, pid).is_some(),
            "an untracked row keeps its last full-tick values"
        );
    }
    sleep_one_second();
    let rows = harness.tick(&[], true);
    let five_tick = row(&rows, pid).expect("still running").cpu_percent;

    sleep_one_second();
    let rows = harness.tick(&[], true);
    let one_tick = row(&rows, pid).expect("still running").cpu_percent;

    println!("yes: five-tick reading {five_tick:.2} %, one-tick reading {one_tick:.2} %");
    assert!(
        one_tick > 10.0,
        "yes should be visibly busy, read {one_tick}"
    );
    assert!(
        five_tick < 2.0 * one_tick,
        "the full-tick reading {five_tick} is inflated against a one-second reading of {one_tick}"
    );
}

/// The values the process table shows must come out the same whether the
/// sampler or sysinfo read them: memory and virtual memory are the same
/// `PROC_PIDTASKINFO` fields (a sleeping child, whose memory does not move
/// between the two reads), the state follows the same rule, and run time is
/// seconds since the same start time (within the second the clock may
/// tick over between the two reads).
#[test]
fn sampler_matches_sysinfo_for_the_same_instant() {
    let Some(child) = Guarded::spawn("/bin/sleep", &["30"]) else {
        return;
    };
    let child_pid = child.pid();
    let own_pid = std::process::id();
    let mut system = System::new();
    let mut sampler = ProcessSampler::new();

    // Let the child finish loading, so its memory is not still moving
    // between the two reads below.
    std::thread::sleep(Duration::from_millis(200));
    system.refresh_processes_specifics(ProcessesToUpdate::All, true, refresh_kind());
    let before: Vec<(u64, u64)> = [child_pid, own_pid]
        .into_iter()
        .map(|pid| {
            let process = system
                .process(Pid::from_u32(pid))
                .expect("sysinfo holds it");
            (process.memory(), process.virtual_memory())
        })
        .collect();
    let samples = sampler.sample([child_pid, own_pid]);
    // A second sysinfo read brackets the sample: when sysinfo's own memory
    // reading did not move across the sample, the sampler's must equal it.
    let mut after = System::new();
    after.refresh_processes_specifics(ProcessesToUpdate::All, true, refresh_kind());

    for (index, pid) in [child_pid, own_pid].into_iter().enumerate() {
        let process = system
            .process(Pid::from_u32(pid))
            .expect("sysinfo holds it");
        let Some(Sampled::Live(sample)) = samples.get(&pid) else {
            panic!("pid {pid} should be live and inspectable");
        };
        assert_eq!(
            sample.state,
            convert_process_state(process.status()),
            "pid {pid}"
        );
        assert_eq!(sample.start_time, process.start_time(), "pid {pid}");
        assert!(
            sample.run_time.abs_diff(process.run_time()) <= 1,
            "pid {pid}: run time {} vs {}",
            sample.run_time,
            process.run_time()
        );
        let later = after
            .process(Pid::from_u32(pid))
            .map(|p| (p.memory(), p.virtual_memory()))
            .expect("still alive");
        if before[index] == later {
            assert_eq!(
                (sample.memory_rss, sample.memory_vms),
                later,
                "pid {pid}: memory did not move across the sample, so it must match"
            );
        } else {
            println!(
                "pid {pid}: memory moved between reads ({:?} -> {later:?}); not compared",
                before[index]
            );
        }
    }
}

/// The selective-tick cache rules, without a kernel: gone rows leave, a
/// reused PID leaves, unreadable and untracked rows keep their values, a
/// sample without a CPU reading keeps the previous one, and GPU attribution
/// is refreshed for every row.
#[test]
fn selective_tick_applies_samples_to_the_cache() {
    fn stored(pid: u32, start_time: u64) -> ProcessInfo {
        let mut row = crate::device::process_list::tests::process(pid, "", 0, false);
        row.start_time = start_time.to_string();
        row.cpu_percent = 7.5;
        row.memory_rss = 1_000;
        row.state = "S".to_string();
        row
    }
    fn live(cpu_percent: Option<f64>, start_time: u64) -> Sampled {
        Sampled::Live(ProcessSample {
            cpu_percent,
            memory_rss: 2_000,
            memory_vms: 4_000,
            state: "R",
            run_time: 42,
            start_time,
        })
    }

    let mut cache: HashMap<u32, ProcessInfo> = [
        (1, stored(1, 100)),
        (2, stored(2, 100)),
        (3, stored(3, 100)),
        (4, stored(4, 100)),
        (5, stored(5, 100)),
        (6, stored(6, 100)),
    ]
    .into_iter()
    .collect();
    let samples: HashMap<u32, Sampled> = [
        (1, live(Some(50.0), 100)),
        (2, Sampled::Gone),
        (3, live(None, 200)),
        (4, Sampled::Unreadable),
        (5, live(None, 100)),
    ]
    .into_iter()
    .collect();
    let gpu_pids: HashSet<u32> = [1, 6].into_iter().collect();

    let rows = update_cache_selective(100_000, &gpu_pids, &mut cache, &samples);
    let pids: Vec<u32> = rows.iter().map(|row| row.pid).collect();
    assert_eq!(pids, vec![1, 4, 5, 6]);

    let sampled = row(&rows, 1).unwrap();
    assert_eq!(sampled.cpu_percent, 50.0);
    assert_eq!(sampled.memory_rss, 2_000);
    assert_eq!(sampled.memory_vms, 4_000);
    assert_eq!(sampled.memory_percent, 2.0);
    assert_eq!(sampled.state, "R");
    assert_eq!(sampled.cpu_time, 42);
    assert!(sampled.uses_gpu && sampled.device_uuid == "GPU");

    let unreadable = row(&rows, 4).unwrap();
    assert_eq!(
        (unreadable.cpu_percent, unreadable.memory_rss),
        (7.5, 1_000)
    );
    assert_eq!(unreadable.state, "S");

    let no_reading = row(&rows, 5).unwrap();
    assert_eq!(
        no_reading.cpu_percent, 7.5,
        "no reading keeps the previous value"
    );
    assert_eq!(no_reading.memory_rss, 2_000, "memory still updates");

    let untracked = row(&rows, 6).unwrap();
    assert_eq!(untracked.cpu_percent, 7.5);
    assert!(untracked.uses_gpu, "GPU attribution updates for every row");
}

/// The defect as sysinfo 0.39.6 exhibits it, driven exactly the way the
/// collector drove it before #427: a full refresh, four selective refreshes
/// of a tracked set that excludes a busy child, then a full refresh. The
/// busy child's `cpu_usage` on that full refresh is about five times its
/// real share. Kept as the record of what `full_tick_cpu_percent_is_not_inflated`
/// guards against; ignored because it only documents sysinfo.
#[test]
#[ignore = "documents sysinfo's full-tick inflation; run with --ignored --nocapture"]
fn sysinfo_inflates_untracked_processes_on_a_full_refresh() {
    let Some(busy) = Guarded::spawn("/usr/bin/yes", &[]) else {
        return;
    };
    let pid = Pid::from_u32(busy.pid());
    let mut system = System::new();
    let reading = |system: &System| system.process(pid).map(|p| p.cpu_usage()).unwrap_or(-1.0);

    system.refresh_processes_specifics(ProcessesToUpdate::All, true, refresh_kind());
    sleep_one_second();
    system.refresh_processes_specifics(ProcessesToUpdate::All, true, refresh_kind());
    let tracked: Vec<Pid> = system
        .processes()
        .keys()
        .filter(|p| **p != pid)
        .copied()
        .collect();
    println!("tick 0 (All): yes {:.2} %", reading(&system));
    for tick in 1..=4 {
        sleep_one_second();
        system.refresh_processes_specifics(ProcessesToUpdate::Some(&tracked), true, refresh_kind());
        println!(
            "tick {tick} (Some, untracked): yes {:.2} % (stale)",
            reading(&system)
        );
    }
    sleep_one_second();
    system.refresh_processes_specifics(ProcessesToUpdate::All, true, refresh_kind());
    let five_tick = reading(&system);
    sleep_one_second();
    system.refresh_processes_specifics(ProcessesToUpdate::All, true, refresh_kind());
    let one_tick = reading(&system);
    println!("tick 5 (All): yes {five_tick:.2} %  <- five ticks of task time over one interval");
    println!("tick 6 (All): yes {one_tick:.2} %");
    assert!(
        five_tick > 3.0 * one_tick,
        "sysinfo 0.39.6 inflates here; if this fails the upstream defect is gone and \
         the sampler's own guard is the only reason to keep the full-tick sampling"
    );
}

/// The sampler against sysinfo over the whole process table, on real
/// hardware: status must match for every PID both hold, memory for all but
/// the few whose memory moved between the two reads, and CPU percent within
/// a small tolerance for every PID whose counter moved. Prints the A/B table
/// the PR carries. Ignored because its tolerances assume an unloaded host.
#[test]
#[ignore = "hardware A/B against sysinfo; run with --ignored --nocapture"]
fn sampler_matches_sysinfo_across_the_process_table() {
    let mut system = System::new();
    let mut sampler = ProcessSampler::new();
    system.refresh_processes_specifics(ProcessesToUpdate::All, true, refresh_kind());
    let _ = sampler.sample(system.processes().keys().map(|pid| pid.as_u32()));

    sleep_one_second();
    system.refresh_processes_specifics(ProcessesToUpdate::All, true, refresh_kind());
    let samples = sampler.sample(system.processes().keys().map(|pid| pid.as_u32()));

    let mut total = 0;
    let mut status_match = 0;
    let mut status_mismatch = Vec::new();
    let mut memory_match = 0;
    let mut memory_mismatch = 0;
    let mut vms_match = 0;
    let mut run_time_match = 0;
    let mut sticky = Vec::new();
    let mut compared: Vec<(u32, String, f32, f64)> = Vec::new();
    for (pid, process) in system.processes() {
        let pid = pid.as_u32();
        let Some(Sampled::Live(sample)) = samples.get(&pid) else {
            continue;
        };
        total += 1;
        let expected = convert_process_state(process.status());
        if sample.state == expected {
            status_match += 1;
        } else {
            status_mismatch.push((pid, expected, sample.state));
        }
        if sample.memory_rss == process.memory() {
            memory_match += 1;
        } else {
            memory_mismatch += 1;
        }
        if sample.memory_vms == process.virtual_memory() {
            vms_match += 1;
        }
        if sample.run_time.abs_diff(process.run_time()) <= 1 {
            run_time_match += 1;
        }
        let Some(cpu) = sample.cpu_percent else {
            continue;
        };
        let name = process.name().to_string_lossy().to_string();
        if cpu == 0.0 && process.cpu_usage() > 0.0 {
            sticky.push((pid, name.clone(), process.cpu_usage()));
            continue;
        }
        if cpu > 0.0 || process.cpu_usage() > 0.0 {
            compared.push((pid, name, process.cpu_usage(), cpu));
        }
    }

    let mean_abs = compared
        .iter()
        .map(|(_, _, s, n)| (f64::from(*s) - n).abs())
        .sum::<f64>()
        / compared.len().max(1) as f64;
    let max_abs = compared
        .iter()
        .map(|(_, _, s, n)| (f64::from(*s) - n).abs())
        .fold(0.0, f64::max);
    compared.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));

    println!(
        "{total} live inspectable PIDs; status match {status_match}, mismatches {status_mismatch:?}"
    );
    println!(
        "memory rss match {memory_match}, mismatch {memory_mismatch}; vms match {vms_match}; run time within 1 s {run_time_match}"
    );
    println!(
        "cpu: {} PIDs with a moving counter compared, mean |sysinfo - sampler| {mean_abs:.3} pt, max {max_abs:.3} pt; {} PIDs where sysinfo holds a stale non-zero value while the counter did not move",
        compared.len(),
        sticky.len()
    );
    println!("| pid | name | sysinfo cpu_usage | sampler cpu_percent |\n|---|---|---|---|");
    for (pid, name, sysinfo_cpu, sampler_cpu) in compared.iter().take(12) {
        println!("| {pid} | {name} | {sysinfo_cpu:.2} | {sampler_cpu:.2} |");
    }
    println!(
        "stale examples: {:?}",
        sticky.iter().take(6).collect::<Vec<_>>()
    );

    assert_eq!(
        status_match, total,
        "status must match sysinfo for every PID"
    );
    assert!(
        memory_match * 100 >= total * 98,
        "memory matched for {memory_match} of {total}"
    );
    assert!(
        vms_match * 100 >= total * 98,
        "virtual memory matched for {vms_match} of {total}"
    );
    assert_eq!(run_time_match, total);
    assert!(mean_abs < 0.2, "mean CPU difference {mean_abs} pt");
}
