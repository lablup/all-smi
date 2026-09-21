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

/// Readings for a live, inspectable process: `task_time` mach units of CPU,
/// `start` as its start time, `SRUN` with a waiting thread 0.
fn live(task_time: u64, start: u64) -> Readings {
    // SAFETY: both are plain-data structs for which all-zero is a valid
    // value; the fields the sampler reads are set below.
    let mut task: libc::proc_taskinfo = unsafe { std::mem::zeroed() };
    task.pti_total_user = task_time / 2;
    task.pti_total_system = task_time - task_time / 2;
    task.pti_resident_size = 4096;
    task.pti_virtual_size = 8192;
    // SAFETY: as above.
    let mut bsd: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    bsd.pbi_status = libc::SRUN;
    bsd.pbi_start_tvsec = start;
    Readings {
        task: Ok(task),
        bsd: Some(bsd),
        thread_state: Some(libc::TH_STATE_WAITING),
    }
}

fn sample_of(sampled: Sampled) -> ProcessSample {
    match sampled {
        Sampled::Live(sample) => sample,
        other => panic!("expected a live sample, got {other:?}"),
    }
}

// ---- the pure arithmetic --------------------------------------------------

#[test]
fn cpu_percent_is_the_task_delta_over_the_elapsed_time() {
    // 250 units of CPU over 1,000 units of wall time: a quarter of a core.
    assert_eq!(
        cpu_percent(Some((1_000, 10_000)), 1_250, 11_000),
        Some(25.0)
    );
    // Two cores busy the whole time reads 200.
    assert_eq!(cpu_percent(Some((0, 0)), 2_000, 1_000), Some(200.0));
}

#[test]
fn first_sighting_has_no_reading() {
    assert_eq!(cpu_percent(None, 5_000, 10_000), None);
}

#[test]
fn a_counter_that_does_not_move_reads_zero_not_the_previous_value() {
    // sysinfo keeps its previous non-zero `cpu_usage` here (defect 3).
    assert_eq!(cpu_percent(Some((5_000, 10_000)), 5_000, 11_000), Some(0.0));
}

#[test]
fn no_elapsed_time_has_no_reading() {
    // Two samples in the same mach tick, or a clock that went backwards:
    // there is no interval to divide by, so no value is invented.
    assert_eq!(cpu_percent(Some((1_000, 10_000)), 1_500, 10_000), None);
    assert_eq!(cpu_percent(Some((1_000, 10_000)), 1_500, 9_000), None);
}

#[test]
fn a_counter_that_went_backwards_reads_zero() {
    assert_eq!(cpu_percent(Some((5_000, 10_000)), 4_000, 11_000), Some(0.0));
}

// ---- the state rule -------------------------------------------------------

#[test]
fn state_follows_sysinfos_rule() {
    // Inspectable live process: thread 0's state decides.
    assert_eq!(
        state_code(Some(libc::SRUN), Some(libc::TH_STATE_RUNNING)),
        "R"
    );
    assert_eq!(
        state_code(Some(libc::SRUN), Some(libc::TH_STATE_WAITING)),
        "S"
    );
    assert_eq!(
        state_code(Some(libc::SRUN), Some(libc::TH_STATE_STOPPED)),
        "T"
    );
    assert_eq!(
        state_code(Some(libc::SRUN), Some(libc::TH_STATE_UNINTERRUPTIBLE)),
        "?"
    );
    assert_eq!(
        state_code(Some(libc::SRUN), Some(libc::TH_STATE_HALTED)),
        "?"
    );
    // Thread 0 did not resolve: sysinfo assumes running.
    assert_eq!(state_code(Some(libc::SRUN), None), "R");
    // BSD info was unreadable when the process was first seen: `?` for good.
    assert_eq!(state_code(None, Some(libc::TH_STATE_WAITING)), "?");
    // Any other BSD status at first sighting is fixed.
    assert_eq!(
        state_code(Some(libc::SIDL), Some(libc::TH_STATE_RUNNING)),
        "I"
    );
    assert_eq!(state_code(Some(libc::SSLEEP), None), "S");
    assert_eq!(state_code(Some(libc::SSTOP), None), "T");
    assert_eq!(state_code(Some(libc::SZOMB), None), "Z");
}

// ---- folding readings into baselines --------------------------------------

#[test]
fn the_first_sample_of_a_pid_carries_everything_but_cpu() {
    let mut sampler = ProcessSampler::new();
    let sample = sample_of(sampler.fold(7, live(1_000, 1_700_000_000), 10_000, 1_700_000_060));
    assert_eq!(sample.cpu_percent, None);
    assert_eq!(sample.memory_rss, 4096);
    assert_eq!(sample.memory_vms, 8192);
    assert_eq!(sample.state, "S");
    assert_eq!(sample.run_time, 60);
    assert_eq!(sample.start_time, 1_700_000_000);
    assert_eq!(sampler.len(), 1);
}

#[test]
fn the_second_sample_reads_the_delta_over_its_own_interval() {
    let mut sampler = ProcessSampler::new();
    let _ = sampler.fold(7, live(1_000, 100), 10_000, 0);
    let sample = sample_of(sampler.fold(7, live(1_500, 100), 12_000, 1));
    assert_eq!(sample.cpu_percent, Some(25.0));
    // The baseline moved: the next delta is taken from this sample.
    let sample = sample_of(sampler.fold(7, live(1_500, 100), 13_000, 2));
    assert_eq!(sample.cpu_percent, Some(0.0));
}

#[test]
fn a_reused_pid_starts_over() {
    let mut sampler = ProcessSampler::new();
    let _ = sampler.fold(7, live(9_000, 100), 10_000, 0);
    // Same PID, a later start time, a counter far below the old one: a new
    // process. No delta against the old baseline, and the new start time is
    // reported so the cache can drop the old row.
    let sample = sample_of(sampler.fold(7, live(50, 200), 11_000, 1));
    assert_eq!(sample.cpu_percent, None);
    assert_eq!(sample.start_time, 200);
    // From here on the new process has a baseline of its own.
    let sample = sample_of(sampler.fold(7, live(150, 200), 12_000, 2));
    assert_eq!(sample.cpu_percent, Some(10.0));
}

#[test]
fn an_exited_pid_is_gone_and_forgotten() {
    let mut sampler = ProcessSampler::new();
    let _ = sampler.fold(7, live(1_000, 100), 10_000, 0);
    let gone = Readings {
        task: Err(PidInfoError::Gone),
        bsd: None,
        thread_state: None,
    };
    assert_eq!(sampler.fold(7, gone, 11_000, 1), Sampled::Gone);
    assert!(sampler.is_empty());
    // Seen again under the same PID later: a first sighting.
    let sample = sample_of(sampler.fold(7, live(2_000, 100), 12_000, 2));
    assert_eq!(sample.cpu_percent, None);
}

#[test]
fn an_unreadable_pid_keeps_its_baseline() {
    let mut sampler = ProcessSampler::new();
    let _ = sampler.fold(7, live(1_000, 100), 10_000, 0);
    let unreadable = Readings {
        task: Err(PidInfoError::Unreadable),
        bsd: None,
        thread_state: None,
    };
    assert_eq!(sampler.fold(7, unreadable, 11_000, 1), Sampled::Unreadable);
    assert_eq!(sampler.len(), 1);
    // When it becomes readable again the delta spans the whole gap.
    let sample = sample_of(sampler.fold(7, live(1_400, 100), 12_000, 2));
    assert_eq!(sample.cpu_percent, Some(20.0));
}

#[test]
fn the_bsd_status_is_fixed_at_first_sighting() {
    let mut sampler = ProcessSampler::new();
    let mut unreadable_bsd = live(1_000, 100);
    unreadable_bsd.bsd = None;
    let sample = sample_of(sampler.fold(7, unreadable_bsd, 10_000, 1_000));
    // No start time either: run time is "now", as sysinfo's `new_empty`.
    assert_eq!(
        (sample.state, sample.start_time, sample.run_time),
        ("?", 0, 1_000)
    );
    // When the BSD info becomes readable the start time changes from 0 to
    // the real one. sysinfo's `update_process` (`process.rs:725`) treats a
    // changed start time as a new process under the same PID and rebuilds
    // it, status included; the sampler starts over the same way.
    let sample = sample_of(sampler.fold(7, live(1_100, 100), 11_000, 1_001));
    assert_eq!(sample.cpu_percent, None);
    assert_eq!((sample.state, sample.start_time), ("S", 100));
}

#[test]
fn retain_drops_baselines_that_left_the_cache() {
    let mut sampler = ProcessSampler::new();
    let _ = sampler.fold(1, live(1, 1), 1, 0);
    let _ = sampler.fold(2, live(1, 1), 1, 0);
    sampler.retain(|pid| pid == 2);
    assert_eq!(sampler.len(), 1);
}

// ---- against the kernel ---------------------------------------------------

/// Our own process is always inspectable: the first sample has no CPU
/// reading, the second one does, and both carry non-zero memory.
#[test]
fn sampling_this_process_twice_produces_a_reading() {
    let pid = std::process::id();
    let mut sampler = ProcessSampler::new();
    let first = sample_of(sampler.sample([pid]).remove(&pid).expect("sampled"));
    assert_eq!(first.cpu_percent, None);
    assert!(first.memory_rss > 0 && first.memory_vms > 0);
    // Thread id 0 resolves for some processes and not others (see
    // `pidinfo_macos`), so the state is `R` or `S` here, never `?`.
    assert!(
        matches!(first.state, "R" | "S"),
        "state of a live inspectable process, read {}",
        first.state
    );

    let started = std::time::Instant::now();
    let mut spin = 0u64;
    while started.elapsed() < std::time::Duration::from_millis(50) {
        spin = spin.wrapping_add(1);
    }
    std::hint::black_box(spin);
    let second = sample_of(sampler.sample([pid]).remove(&pid).expect("sampled"));
    let cpu = second.cpu_percent.expect("a delta exists now");
    assert!(cpu > 0.0, "spun for 50 ms, read {cpu}");
}

/// An exited child is `Gone`, never `Unreadable`, so it can be pruned.
#[test]
fn an_exited_child_is_gone() {
    let Ok(mut child) = crate::utils::command::new_command("/usr/bin/true").spawn() else {
        return;
    };
    let pid = child.id();
    let _ = child.wait();
    let mut sampler = ProcessSampler::new();
    assert_eq!(sampler.sample([pid]).remove(&pid), Some(Sampled::Gone));
}
