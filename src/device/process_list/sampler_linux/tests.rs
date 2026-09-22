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

/// The constants the tests use: 100 clock ticks per second (one tick per
/// 10 ms), 4 KiB pages, boot at epoch second 1,000,000,000.
fn constants() -> Constants {
    Constants {
        clk_tck: 100,
        page_size: 4096,
        btime: 1_000_000_000,
    }
}

/// Readings for a live process: `task_time` clock ticks of CPU burned since
/// it started, started `start_raw` clock ticks after boot, sleeping.
fn live(task_time: u64, start_raw: u64) -> Readings {
    Readings {
        stat: Ok(Stat {
            state: Some('S'),
            task_time,
            start_time_raw: start_raw,
            vsize: 8192,
            rss_pages: 1,
        }),
    }
}

fn gone() -> Readings {
    Readings {
        stat: Err(StatError::Gone),
    }
}

fn unreadable() -> Readings {
    Readings {
        stat: Err(StatError::Unreadable),
    }
}

/// A live reading with a different state letter.
fn with_state(readings: Readings, state: char) -> Readings {
    let mut readings = readings;
    if let Ok(stat) = &mut readings.stat {
        stat.state = Some(state);
    }
    readings
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
    // 100 ticks (1 s of CPU at 100 Hz) over 1 s of wall time: one core.
    assert_eq!(
        cpu_percent(Some((0, 0)), 100, 1_000_000_000, 100),
        Some(100.0)
    );
    // Two cores busy the whole second reads 200.
    assert_eq!(
        cpu_percent(Some((0, 0)), 200, 1_000_000_000, 100),
        Some(200.0)
    );
    // Half a core.
    assert_eq!(
        cpu_percent(Some((0, 0)), 50, 1_000_000_000, 100),
        Some(50.0)
    );
}

#[test]
fn a_long_delta_over_a_long_gap_reads_the_same_share() {
    // The five-tick task-time delta of a PID refreshed once in five, over
    // its own five seconds of elapsed time, reads the one core it burned.
    // sysinfo divided the same delta by a one-tick global interval and read
    // about five.
    assert_eq!(
        cpu_percent(Some((0, 0)), 500, 5_000_000_000, 100),
        Some(100.0)
    );
}

#[test]
fn first_sighting_has_no_reading() {
    assert_eq!(cpu_percent(None, 5_000, 1_000_000_000, 100), None);
}

#[test]
fn a_counter_that_does_not_move_reads_zero_not_the_previous_value() {
    assert_eq!(
        cpu_percent(Some((5_000, 0)), 5_000, 1_000_000_000, 100),
        Some(0.0)
    );
}

#[test]
fn no_elapsed_time_has_no_reading() {
    // Two samples in the same monotonic nanosecond, or a clock that went
    // backwards: there is no interval to divide by, so no value is invented.
    assert_eq!(
        cpu_percent(Some((1_000, 1_000_000_000)), 1_500, 1_000_000_000, 100),
        None
    );
    assert_eq!(
        cpu_percent(Some((1_000, 2_000_000_000)), 1_500, 1_000_000_000, 100),
        None
    );
}

#[test]
fn a_counter_that_went_backwards_reads_zero() {
    assert_eq!(
        cpu_percent(Some((5_000, 0)), 4_000, 1_000_000_000, 100),
        Some(0.0)
    );
}

// ---- the state rule -------------------------------------------------------

#[test]
fn state_follows_sysinfos_rule() {
    assert_eq!(state_code(Some('R')), "R");
    assert_eq!(state_code(Some('S')), "S");
    assert_eq!(state_code(Some('I')), "I");
    assert_eq!(state_code(Some('D')), "D");
    assert_eq!(state_code(Some('Z')), "Z");
    assert_eq!(state_code(Some('T')), "T");
    assert_eq!(state_code(Some('X')), "X");
    assert_eq!(state_code(Some('x')), "X");
    // 't' is Tracing, 'K' Wakekill, 'W' Waking, 'P' Parked: sysinfo's
    // `Display` strings for them map to `?` in `convert_process_state`.
    assert_eq!(state_code(Some('t')), "?");
    assert_eq!(state_code(Some('K')), "?");
    assert_eq!(state_code(Some('W')), "?");
    assert_eq!(state_code(Some('P')), "?");
    // No state field: sysinfo builds `Unknown(0)`, which reads `?`.
    assert_eq!(state_code(None), "?");
}

// ---- parsing --------------------------------------------------------------

#[test]
fn a_stat_with_spaces_and_parens_in_comm_parses() {
    let stat = parse_stat(
        "42 (my (proc) name) S 1 0 0 0 -1 4194560 100 0 0 0 500 300 0 0 20 0 1 0 12345 67890 25",
    )
    .expect("parsed");
    assert_eq!(stat.state, Some('S'));
    assert_eq!(stat.task_time, 800);
    assert_eq!(stat.start_time_raw, 12345);
    assert_eq!(stat.vsize, 67890);
    assert_eq!(stat.rss_pages, 25);
}

#[test]
fn a_short_stat_read_is_not_parsed() {
    // Fewer than the fields the sampler reads: the read is unusable.
    let stat = parse_stat("42 (proc) S 1 0 0 0");
    assert!(stat.is_none());
    let stat = parse_stat("42 (proc)");
    assert!(stat.is_none());
}

// ---- folding readings into baselines --------------------------------------

#[test]
fn the_first_sample_of_a_pid_carries_everything_but_cpu() {
    let mut sampler = ProcessSampler::new();
    // Boot at 1,000,000,000; the process started 100 ticks (1 s) after boot
    // and was sampled at uptime 61: start time 1,000,000,001, run time 60.
    let sample = sample_of(sampler.fold(7, live(1_000, 100), 10_000_000_000, 61, &constants()));
    assert_eq!(sample.cpu_percent, None);
    assert_eq!(sample.memory_rss, 4096);
    assert_eq!(sample.memory_vms, 8192);
    assert_eq!(sample.state, "S");
    assert_eq!(sample.run_time, 60);
    assert_eq!(sample.start_time, 1_000_000_001);
    assert_eq!(sampler.len(), 1);
}

#[test]
fn the_second_sample_reads_the_delta_over_its_own_interval() {
    let mut sampler = ProcessSampler::new();
    let _ = sampler.fold(7, live(1_000, 100), 10_000_000_000, 10, &constants());
    // 50 ticks (0.5 s of CPU at 100 Hz) over 1 s: half a core.
    let sample = sample_of(sampler.fold(7, live(1_050, 100), 11_000_000_000, 11, &constants()));
    assert_eq!(sample.cpu_percent, Some(50.0));
    // The baseline moved: the next delta is taken from this sample.
    let sample = sample_of(sampler.fold(7, live(1_050, 100), 12_000_000_000, 12, &constants()));
    assert_eq!(sample.cpu_percent, Some(0.0));
}

#[test]
fn a_reused_pid_starts_over() {
    let mut sampler = ProcessSampler::new();
    let _ = sampler.fold(7, live(9_000, 100), 10_000_000_000, 90, &constants());
    // Same PID, a later start time, a counter far below the old one: a new
    // process. No delta against the old baseline, and the new start time is
    // reported so the cache can drop the old row.
    let sample = sample_of(sampler.fold(7, live(50, 200), 11_000_000_000, 91, &constants()));
    assert_eq!(sample.cpu_percent, None);
    assert_eq!(sample.start_time, 1_000_000_002);
    assert_eq!(sample.run_time, 89);
    // From here on the new process has a baseline of its own: 100 ticks
    // (1 s of CPU at 100 Hz) over 1 s reads one core.
    let sample = sample_of(sampler.fold(7, live(150, 200), 12_000_000_000, 92, &constants()));
    assert_eq!(sample.cpu_percent, Some(100.0));
}

#[test]
fn an_exited_pid_is_gone_and_forgotten() {
    let mut sampler = ProcessSampler::new();
    let _ = sampler.fold(7, live(1_000, 100), 10_000_000_000, 10, &constants());
    assert_eq!(
        sampler.fold(7, gone(), 11_000_000_000, 11, &constants()),
        Sampled::Gone
    );
    assert!(sampler.is_empty());
    // Seen again under the same PID later: a first sighting.
    let sample = sample_of(sampler.fold(7, live(2_000, 100), 12_000_000_000, 12, &constants()));
    assert_eq!(sample.cpu_percent, None);
}

#[test]
fn an_unreadable_pid_keeps_its_baseline() {
    let mut sampler = ProcessSampler::new();
    let _ = sampler.fold(7, live(1_000, 100), 10_000_000_000, 10, &constants());
    assert_eq!(
        sampler.fold(7, unreadable(), 11_000_000_000, 11, &constants()),
        Sampled::Unreadable
    );
    assert_eq!(sampler.len(), 1);
    assert!(sampler.contains(7));
    // When it becomes readable again the delta spans the whole gap: 400
    // ticks (4 s of CPU at 100 Hz) over the 2 s since the last good sample.
    let sample = sample_of(sampler.fold(7, live(1_400, 100), 12_000_000_000, 12, &constants()));
    assert_eq!(sample.cpu_percent, Some(200.0));
}

#[test]
fn a_zombie_stays_live_with_state_z() {
    // sysinfo keeps a zombie in its map for as long as /proc lists it, so
    // the sampler keeps reporting it too and the row leaves on the full
    // tick's Gone rather than disappearing mid-tick.
    let mut sampler = ProcessSampler::new();
    let _ = sampler.fold(
        7,
        with_state(live(1_000, 100), 'Z'),
        10_000_000_000,
        10,
        &constants(),
    );
    let sample = sample_of(sampler.fold(
        7,
        with_state(live(1_050, 100), 'Z'),
        11_000_000_000,
        11,
        &constants(),
    ));
    assert_eq!(sample.state, "Z");
    assert_eq!(sample.cpu_percent, Some(50.0));
}

#[test]
fn retain_drops_baselines_that_left_the_cache() {
    let mut sampler = ProcessSampler::new();
    let _ = sampler.fold(1, live(1, 1), 1, 0, &constants());
    let _ = sampler.fold(2, live(1, 1), 1, 0, &constants());
    sampler.retain(|pid| pid == 2);
    assert_eq!(sampler.len(), 1);
}

// ---- the regression -------------------------------------------------------

/// Issue #428: a PID refreshed on one tick in five must read the same as one
/// refreshed on all five. Both burn one core throughout at 100 Hz, so both
/// read 100 percent; sysinfo read the one-in-five PID at about five times
/// its real share, because its five-tick task-time delta was divided by a
/// one-tick global interval.
#[test]
fn a_pid_refreshed_on_one_tick_in_five_reads_the_same_as_one_refreshed_on_all_five() {
    let constants = constants();
    let ticks_ns = |tick: u64| tick * 1_000_000_000;

    // Refreshed every tick, as a tracked PID is.
    let mut tracked = ProcessSampler::new();
    let mut tracked_readings = Vec::new();
    for tick in 0..=5u64 {
        let sampled = tracked.fold(1, live(tick * 100, 0), ticks_ns(tick), tick, &constants);
        tracked_readings.push(sample_of(sampled).cpu_percent);
    }

    // Refreshed only on the full ticks (0 and 5), as a process outside the
    // tracked set is; the ticks in between sample other PIDs and this one's
    // baseline stands.
    let mut untracked = ProcessSampler::new();
    let mut untracked_readings = Vec::new();
    for tick in [0u64, 5] {
        let sampled = untracked.fold(2, live(tick * 100, 0), ticks_ns(tick), tick, &constants);
        untracked_readings.push(sample_of(sampled).cpu_percent);
    }

    // First sightings carry no reading.
    assert_eq!(tracked_readings[0], None);
    assert_eq!(untracked_readings[0], None);
    // Every one-second delta of the tracked PID reads one core.
    for tick in 1..=5u64 {
        assert_eq!(tracked_readings[tick as usize], Some(100.0), "tick {tick}");
    }
    // The five-second delta of the untracked PID reads the same one core,
    // not five: the divisor is the PID's own elapsed time.
    assert_eq!(untracked_readings[1], Some(100.0));
}

// ---- against the kernel ---------------------------------------------------

/// Our own process is always readable: the first sample has no CPU reading,
/// the second one does, and both carry non-zero memory. The state is only a
/// membership check: the main thread's letter at the sampling instant is
/// running, waiting, or briefly in uninterruptible I/O when the parallel
/// test run loads the disk.
#[test]
fn sampling_this_process_twice_produces_a_reading() {
    let pid = std::process::id();
    let mut sampler = ProcessSampler::new();
    let first = sample_of(sampler.sample([pid]).remove(&pid).expect("sampled"));
    assert_eq!(first.cpu_percent, None);
    assert!(first.memory_rss > 0 && first.memory_vms > 0);
    assert!(
        matches!(first.state, "R" | "S" | "D"),
        "state of a live process, read {}",
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

/// The sampler's static fields must come out identical to sysinfo's for the
/// same process: the start time (the same `btime + starttime / clk_tck`) and
/// run time within the second the two uptime reads may differ. The state
/// letter is only a membership check: the main thread's letter at the two
/// reads can legitimately differ (running, waiting, or briefly in
/// uninterruptible I/O).
#[test]
fn static_fields_match_sysinfo_for_this_process() {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};

    let own_pid = std::process::id();
    let pid = sysinfo::Pid::from_u32(own_pid);
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing()
            .with_cpu()
            .with_memory()
            .with_user(sysinfo::UpdateKind::OnlyIfNotSet),
    );
    let process = system.process(pid).expect("sysinfo holds this process");

    let mut sampler = ProcessSampler::new();
    let sample = sample_of(sampler.sample([own_pid]).remove(&own_pid).expect("sampled"));
    assert!(
        matches!(sample.state, "R" | "S" | "D"),
        "state of a live process, read {}, sysinfo read {}",
        sample.state,
        crate::device::process_list::convert_process_state(process.status())
    );
    assert_eq!(sample.start_time, process.start_time());
    assert!(
        sample.run_time.abs_diff(process.run_time()) <= 1,
        "run time {} vs sysinfo {}",
        sample.run_time,
        process.run_time()
    );
}

/// An exited child is `Gone`, never `Unreadable`, so it can be pruned.
#[test]
fn an_exited_child_is_gone() {
    let Ok(mut child) = crate::utils::command::new_command("/bin/true").spawn() else {
        return;
    };
    let pid = child.id();
    let _ = child.wait();
    let mut sampler = ProcessSampler::new();
    assert_eq!(sampler.sample([pid]).remove(&pid), Some(Sampled::Gone));
}
