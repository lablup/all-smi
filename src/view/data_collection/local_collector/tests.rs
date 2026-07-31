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

/// Serializes the tests in this module against each other.
///
/// All of them touch the process-global `GLOBAL_SYSTEM` (via
/// `with_global_system`), and `first_iteration_collection_reports_startup_status`
/// additionally builds a second full reader set through `initialize_readers`
/// whose `Drop` impls reach into the shared platform metrics manager (on
/// macOS, IOReport shutdown). Run concurrently under the default test
/// harness, those interact and make the tests flaky; held for the whole test
/// body, this lock makes them deterministic instead. `tokio::sync::Mutex`
/// rather than `std::sync::Mutex` on purpose: the guard is held across
/// `.await` points, and unlike `std::sync::Mutex` it does not poison when a
/// task panics while holding it, so one failing test cannot wedge the rest.
static TEST_LOCK: Mutex<()> = Mutex::const_new(());

/// Build a collector with real platform readers already installed, skipping
/// `initialize_readers` so no `AppState` is needed.
async fn initialized_collector() -> LocalCollector {
    let collector = LocalCollector::new();
    *collector.gpu_readers.write().await = get_gpu_readers();
    *collector.cpu_readers.write().await = get_cpu_readers();
    *collector.memory_readers.write().await = get_memory_readers();
    *collector.chassis_reader.write().await = Some(create_chassis_reader());
    *collector.initialized.lock().await = true;
    collector
}

fn summarize(label: &str, samples: &[Duration]) -> Duration {
    let mut sorted = samples.to_vec();
    sorted.sort();
    let total: Duration = sorted.iter().sum();
    let mean = total / sorted.len() as u32;
    println!(
        "{label:<24} mean {:>9.3}ms  min {:>9.3}ms  median {:>9.3}ms  max {:>9.3}ms",
        mean.as_secs_f64() * 1000.0,
        sorted[0].as_secs_f64() * 1000.0,
        sorted[sorted.len() / 2].as_secs_f64() * 1000.0,
        sorted[sorted.len() - 1].as_secs_f64() * 1000.0,
    );
    mean
}

/// Total CPU time (user + system, all threads) charged to this process so
/// far. Used to check that moving work onto the blocking pool relocates
/// cycles instead of adding them.
#[cfg(unix)]
fn process_cpu_time() -> Duration {
    // SAFETY: `getrusage` writes into a fully owned, zero-initialized
    // `rusage` and reads nothing from it. `RUSAGE_SELF` is always a valid
    // target for the calling process.
    let usage = unsafe {
        let mut usage: libc::rusage = std::mem::zeroed();
        if libc::getrusage(libc::RUSAGE_SELF, &mut usage) != 0 {
            return Duration::ZERO;
        }
        usage
    };
    let to_duration = |tv: libc::timeval| {
        Duration::new(tv.tv_sec as u64, (tv.tv_usec as u32).saturating_mul(1000))
    };
    to_duration(usage.ru_utime) + to_duration(usage.ru_stime)
}

#[cfg(not(unix))]
fn process_cpu_time() -> Duration {
    Duration::ZERO
}

/// Measurement harness for the local collection pipeline (issue #287).
///
/// Times every synchronous reader arm in isolation, sums them to show the
/// serialized critical path, then times a full `collect_steady_state` cycle
/// so the wall clock can be compared before and after a parallelization
/// change. Ignored by default because it drives real hardware readers and
/// its numbers are only meaningful on a release build.
///
/// Run with:
/// `cargo test --release --bin all-smi view::data_collection::local_collector::tests::measure_collection_arms -- --ignored --nocapture`
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "measurement harness: drives real hardware readers, run manually on a release build"]
async fn measure_collection_arms() {
    let _serialize = TEST_LOCK.lock().await;
    const SAMPLES: usize = 12;

    let collector = initialized_collector().await;

    // Warm up so delta-based samplers (IOReport, sysinfo CPU) have a
    // previous sample to compare against and are not paying first-call cost.
    for _ in 0..3 {
        let _ = collector.collect_steady_state().await;
    }

    let mut gpu = Vec::new();
    let mut cpu = Vec::new();
    let mut memory = Vec::new();
    let mut gpu_procs = Vec::new();
    let mut vgpu = Vec::new();
    let mut mig = Vec::new();
    let mut storage = Vec::new();
    let mut chassis = Vec::new();
    let mut processes = Vec::new();
    let mut cycle = Vec::new();

    for _ in 0..SAMPLES {
        {
            let readers = collector.gpu_readers.read().await;
            let t = std::time::Instant::now();
            let _: Vec<GpuInfo> = readers.iter().flat_map(|r| r.get_gpu_info()).collect();
            gpu.push(t.elapsed());

            let t = std::time::Instant::now();
            for reader in readers.iter() {
                let _ = reader.get_gpu_processes();
            }
            gpu_procs.push(t.elapsed());

            let t = std::time::Instant::now();
            let _: Vec<VgpuHostInfo> = readers.iter().flat_map(|r| r.get_vgpu_info()).collect();
            vgpu.push(t.elapsed());

            let t = std::time::Instant::now();
            let _: Vec<MigGpuInfo> = readers.iter().flat_map(|r| r.get_mig_info()).collect();
            mig.push(t.elapsed());
        }
        {
            let readers = collector.cpu_readers.read().await;
            let t = std::time::Instant::now();
            let _: Vec<CpuInfo> = readers.iter().flat_map(|r| r.get_cpu_info()).collect();
            cpu.push(t.elapsed());
        }
        {
            let readers = collector.memory_readers.read().await;
            let t = std::time::Instant::now();
            let _: Vec<MemoryInfo> = readers.iter().flat_map(|r| r.get_memory_info()).collect();
            memory.push(t.elapsed());
        }
        {
            let reader = collector.chassis_reader.read().await;
            let t = std::time::Instant::now();
            let _: Vec<ChassisInfo> = reader
                .as_ref()
                .and_then(|r| r.get_chassis_info())
                .into_iter()
                .collect();
            chassis.push(t.elapsed());
        }
        {
            let t = std::time::Instant::now();
            let _ = LocalCollector::collect_storage_info();
            storage.push(t.elapsed());
        }
        {
            let cache = Arc::clone(&collector.process_cache);
            let t = std::time::Instant::now();
            let _ = with_global_system(|system| {
                use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, UpdateKind};
                let refresh_kind = ProcessRefreshKind::nothing()
                    .with_cpu()
                    .with_memory()
                    .with_user(UpdateKind::OnlyIfNotSet);
                system.refresh_processes_specifics(ProcessesToUpdate::All, true, refresh_kind);
                system.refresh_memory();
                let gpu_pids: HashSet<u32> = HashSet::new();
                let mut cache = cache.write().unwrap();
                update_process_cache(system, &gpu_pids, &mut cache)
            });
            processes.push(t.elapsed());
        }

        let t = std::time::Instant::now();
        let _ = collector.collect_steady_state().await;
        cycle.push(t.elapsed());
    }

    println!("--- per-arm synchronous cost ({SAMPLES} samples) ---");
    let arm_means = [
        summarize("gpu_info", &gpu),
        summarize("gpu_processes", &gpu_procs),
        summarize("vgpu_info", &vgpu),
        summarize("mig_info", &mig),
        summarize("cpu_info", &cpu),
        summarize("memory_info", &memory),
        summarize("chassis_info", &chassis),
        summarize("storage_info", &storage),
        summarize("processes(full)", &processes),
    ];
    let serialized: Duration = arm_means.iter().sum();
    println!(
        "{:<24} {:>9.3}ms",
        "SUM (serialized)",
        serialized.as_secs_f64() * 1000.0
    );
    println!("--- full collection cycle ---");
    summarize("collect_steady_state", &cycle);

    // Wall clock and process CPU time over a longer run of back to back
    // cycles. CPU time covers every thread, so blocking-pool work counts
    // exactly as much as work done inline on an async worker.
    const CYCLES: u32 = 100;
    let cpu_before = process_cpu_time();
    let wall_before = std::time::Instant::now();
    for _ in 0..CYCLES {
        let _ = collector.collect_steady_state().await;
    }
    let wall = wall_before.elapsed();
    let cpu = process_cpu_time().saturating_sub(cpu_before);
    println!("--- {CYCLES} back-to-back cycles ---");
    println!(
        "{:<24} {:>9.3}ms/cycle",
        "wall clock",
        wall.as_secs_f64() * 1000.0 / f64::from(CYCLES)
    );
    println!(
        "{:<24} {:>9.3}ms/cycle",
        "process CPU time",
        cpu.as_secs_f64() * 1000.0 / f64::from(CYCLES)
    );
}

/// Collect the same data the pre-#287 pipeline did, straight-line on the
/// calling task, as a reference to compare the parallel pipeline against.
async fn collect_reference(collector: &LocalCollector) -> CollectionData {
    let gpu_readers = collector.gpu_readers.read().await;
    let all_gpu_info: Vec<GpuInfo> = gpu_readers
        .iter()
        .flat_map(|reader| reader.get_gpu_info())
        .collect();
    let mut gpu_processes = Vec::new();
    let mut gpu_pids = HashSet::new();
    for reader in gpu_readers.iter() {
        let (procs, pids) = reader.get_gpu_processes();
        gpu_processes.extend(procs);
        gpu_pids.extend(pids);
    }
    let all_vgpu_info: Vec<VgpuHostInfo> = gpu_readers
        .iter()
        .flat_map(|reader| reader.get_vgpu_info())
        .collect();
    let all_mig_info: Vec<MigGpuInfo> = gpu_readers
        .iter()
        .flat_map(|reader| reader.get_mig_info())
        .collect();

    let all_cpu_info: Vec<CpuInfo> = collector
        .cpu_readers
        .read()
        .await
        .iter()
        .flat_map(|reader| reader.get_cpu_info())
        .collect();
    let all_memory_info: Vec<MemoryInfo> = collector
        .memory_readers
        .read()
        .await
        .iter()
        .flat_map(|reader| reader.get_memory_info())
        .collect();

    let process_cache = Arc::clone(&collector.process_cache);
    let all_processes = with_global_system(|system| {
        use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, UpdateKind};
        let refresh_kind = ProcessRefreshKind::nothing()
            .with_cpu()
            .with_memory()
            .with_user(UpdateKind::OnlyIfNotSet);
        system.refresh_processes_specifics(ProcessesToUpdate::All, true, refresh_kind);
        system.refresh_memory();
        let mut cache = process_cache.write().unwrap();
        update_process_cache(system, &gpu_pids, &mut cache)
    });
    let mut all_processes = merge_gpu_processes(all_processes, gpu_processes);
    all_processes.sort_by(|a, b| {
        b.cpu_percent
            .partial_cmp(&a.cpu_percent)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    all_processes.truncate(MAX_DISPLAY_PROCESSES);

    let all_storage_info = LocalCollector::collect_storage_info();
    let all_chassis_info: Vec<ChassisInfo> = collector
        .chassis_reader
        .read()
        .await
        .as_ref()
        .and_then(|r| r.get_chassis_info())
        .into_iter()
        .collect();
    let all_chassis_info = inject_gpu_power(all_chassis_info, &all_gpu_info);

    CollectionData {
        gpu_info: all_gpu_info,
        cpu_info: all_cpu_info,
        memory_info: all_memory_info,
        process_info: all_processes,
        storage_info: all_storage_info,
        chassis_info: all_chassis_info,
        vgpu_info: all_vgpu_info,
        mig_info: all_mig_info,
        connection_statuses: Vec::new(),
        remote_process_info: Vec::new(),
    }
}

/// The parallel pipeline must produce the same shape of data as the
/// straight-line reference: same devices, same mount points, same chassis
/// rows, same ordering and truncation of the process list. Absolute metric
/// values are sampled at different instants and cannot be compared for
/// equality, so this asserts on everything that is not time varying.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn parallel_collection_matches_serial_reference() {
    let _serialize = TEST_LOCK.lock().await;
    let collector = initialized_collector().await;

    // Prime the readers and the process cache so neither run pays
    // first-call initialization cost or sees an empty cache.
    let _ = collector.collect_steady_state().await;

    let reference = collect_reference(&collector).await;
    let parallel = collector.collect_steady_state().await;

    fn uuids(data: &CollectionData) -> Vec<&str> {
        let mut v: Vec<&str> = data.gpu_info.iter().map(|g| g.uuid.as_str()).collect();
        v.sort_unstable();
        v
    }
    assert_eq!(
        uuids(&reference),
        uuids(&parallel),
        "GPU device set differs"
    );
    assert_eq!(
        reference.cpu_info.len(),
        parallel.cpu_info.len(),
        "CPU row count differs"
    );
    assert_eq!(
        reference.memory_info.len(),
        parallel.memory_info.len(),
        "memory row count differs"
    );
    assert_eq!(
        reference.vgpu_info.len(),
        parallel.vgpu_info.len(),
        "vGPU row count differs"
    );
    assert_eq!(
        reference.mig_info.len(),
        parallel.mig_info.len(),
        "MIG row count differs"
    );
    assert_eq!(
        reference.chassis_info.len(),
        parallel.chassis_info.len(),
        "chassis row count differs"
    );

    fn mounts(data: &CollectionData) -> Vec<&str> {
        let mut v: Vec<&str> = data
            .storage_info
            .iter()
            .map(|s| s.mount_point.as_str())
            .collect();
        v.sort_unstable();
        v
    }
    assert_eq!(
        mounts(&reference),
        mounts(&parallel),
        "mount point set differs"
    );

    assert!(
        !parallel.process_info.is_empty(),
        "process list should never be empty"
    );
    assert!(parallel.process_info.len() <= MAX_DISPLAY_PROCESSES);
    assert!(
        parallel
            .process_info
            .windows(2)
            .all(|w| w[0].cpu_percent >= w[1].cpu_percent),
        "process list must stay sorted by CPU usage descending"
    );
    assert!(
        parallel.remote_process_info.is_empty() && parallel.connection_statuses.is_empty(),
        "local mode must not populate remote fields"
    );
}

/// Every group now runs on the blocking pool while holding an owned read
/// guard. Guard against the failure mode that introduces: a collection that
/// never returns because a lock or the blocking pool is starved. Repeated
/// cycles also cover the selective/full process refresh alternation.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn repeated_collections_complete_without_deadlock() {
    let _serialize = TEST_LOCK.lock().await;
    let collector = initialized_collector().await;

    for _ in 0..(FULL_REFRESH_INTERVAL + 2) {
        let data = timeout(Duration::from_secs(20), collector.collect_steady_state())
            .await
            .expect("collection cycle timed out, likely a lock or blocking-pool deadlock");
        assert!(!data.process_info.is_empty());
    }
}

/// `join_or_log_default` is the seam that keeps a panicking reader from
/// taking the whole collection cycle down. Exercise it directly: a task that
/// panics must not propagate that panic to the caller, and must produce the
/// group's `Default` instead of hanging or bubbling up the panic.
#[tokio::test]
async fn join_or_log_default_falls_back_on_panicking_task() {
    let handle: JoinHandle<Vec<GpuInfo>> =
        tokio::task::spawn_blocking(|| panic!("simulated reader panic"));
    let result = join_or_log_default(handle, "GPU").await;
    assert!(
        result.is_empty(),
        "a panicking reader task must degrade to Default, not propagate"
    );
}

/// Finding: a panic while holding `process_cache`'s write lock (mirroring a
/// panicking reader mid-cycle) used to poison the lock permanently, so every
/// later cycle panicked in turn on `.write().unwrap()` and produced silent,
/// permanently empty data. The lock acquisition now recovers via
/// `PoisonError::into_inner`; this test poisons the lock the same way a
/// panicking reader would and asserts the next cycle still completes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn steady_state_collection_recovers_from_poisoned_process_cache() {
    let _serialize = TEST_LOCK.lock().await;
    let collector = initialized_collector().await;

    let cache = Arc::clone(&collector.process_cache);
    let poisoned = std::panic::catch_unwind(move || {
        let _guard = cache.write().unwrap();
        panic!("simulated panic while holding process_cache");
    });
    assert!(poisoned.is_err(), "the simulated panic should have unwound");
    assert!(
        collector.process_cache.is_poisoned(),
        "the write lock should be poisoned after a panic while held"
    );

    let data = timeout(Duration::from_secs(20), collector.collect_steady_state())
        .await
        .expect("collection cycle timed out, likely a lock deadlock instead of recovery");
    assert!(
        !data.process_info.is_empty(),
        "a recovered cache should still produce process data"
    );
}

/// The first-iteration path additionally pushes startup status lines from
/// inside blocking tasks via `blocking_send`. Exercise it end to end so a
/// misuse of that API (which panics when called from an async context)
/// cannot reach a release build.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn first_iteration_collection_reports_startup_status() {
    let _serialize = TEST_LOCK.lock().await;
    let collector = LocalCollector::new();
    let app_state = Arc::new(Mutex::new(AppState::new()));
    collector.initialize_readers(Arc::clone(&app_state)).await;

    let data = timeout(
        Duration::from_secs(30),
        collector.collect_parallel_first_iteration(Arc::clone(&app_state)),
    )
    .await
    .expect("first-iteration collection timed out");

    assert!(!data.process_info.is_empty());
    assert!(data.process_info.len() <= MAX_DISPLAY_PROCESSES);

    let state = app_state.lock().await;
    let collected_markers = state
        .startup_status_lines
        .iter()
        .filter(|line| line.starts_with('✓') && line.contains("collected"))
        .count();
    assert_eq!(
        collected_markers, 5,
        "expected all five collection arms to report completion, got: {:?}",
        state.startup_status_lines
    );

    // Regression check for the `3 + index` startup-status offset (finding 3):
    // `initialize_readers` must always push exactly three preamble lines, in
    // order, so each "○ Collecting ..." placeholder gets overwritten by the
    // matching "✓ ... collected" line at the right slot rather than a
    // neighboring one (or being silently dropped by the bounds check).
    assert_eq!(
        state.startup_status_lines,
        vec![
            "✓ Initializing GPU readers...",
            "✓ Initializing CPU readers...",
            "✓ Initializing memory readers...",
            "✓ GPU information collected",
            "✓ CPU information collected",
            "✓ Memory information collected",
            "✓ Process information collected",
            "✓ Storage information collected",
        ],
        "startup status lines landed in the wrong slots: {:?}",
        state.startup_status_lines
    );
}
