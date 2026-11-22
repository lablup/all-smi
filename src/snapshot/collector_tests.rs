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

//! Tests for [`collect_once_gated`] (issue #503): a hung reader holds one
//! blocking task, sections run concurrently, and the ungated wrapper keeps
//! its behaviour.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use super::{SectionGate, SnapshotCollector, collect_once, collect_once_gated};
use crate::cli::SnapshotIncludes;
use crate::device::{ChassisInfo, CpuInfo, GpuInfo, MemoryInfo, ProcessInfo};
use crate::storage::info::StorageInfo;

/// Collector whose GPU reader blocks until released, and whose CPU,
/// memory and chassis readers sleep for a configurable time.
#[derive(Default)]
struct FakeCollector {
    gpu_entries: AtomicUsize,
    gpu_blocked: Mutex<bool>,
    gpu_released: Condvar,
    cpu_sleep: Duration,
    memory_sleep: Duration,
    chassis_sleep: Duration,
}

impl FakeCollector {
    fn with_blocked_gpu() -> Self {
        Self {
            gpu_blocked: Mutex::new(true),
            ..Self::default()
        }
    }

    fn release_gpu(&self) {
        *self.gpu_blocked.lock().unwrap() = false;
        self.gpu_released.notify_all();
    }
}

impl SnapshotCollector for FakeCollector {
    fn hostname(&self) -> String {
        "fake".to_string()
    }

    fn collect_gpus(&self) -> Vec<GpuInfo> {
        self.gpu_entries.fetch_add(1, Ordering::SeqCst);
        let mut blocked = self.gpu_blocked.lock().unwrap();
        while *blocked {
            blocked = self.gpu_released.wait(blocked).unwrap();
        }
        Vec::new()
    }

    fn collect_cpus(&self) -> Vec<CpuInfo> {
        std::thread::sleep(self.cpu_sleep);
        Vec::new()
    }

    fn collect_memory(&self) -> Vec<MemoryInfo> {
        std::thread::sleep(self.memory_sleep);
        Vec::new()
    }

    fn collect_chassis(&self) -> Vec<ChassisInfo> {
        std::thread::sleep(self.chassis_sleep);
        Vec::new()
    }

    fn collect_processes(&self) -> Vec<ProcessInfo> {
        Vec::new()
    }

    fn collect_storage(&self) -> Vec<StorageInfo> {
        Vec::new()
    }
}

fn gpu_and_cpu() -> SnapshotIncludes {
    SnapshotIncludes {
        gpu: true,
        cpu: true,
        ..SnapshotIncludes::default()
    }
}

fn error_kinds(snap: &crate::snapshot::Snapshot) -> Vec<(String, String)> {
    snap.errors
        .iter()
        .map(|e| (e.section.clone(), e.kind.clone()))
        .collect()
}

/// With a reader that blocks forever, repeated gated collects enter it
/// once. Without the gate each collect spawned another blocking task into
/// the hung reader (three entries here).
#[tokio::test]
async fn a_hung_reader_is_entered_once_and_reported_busy_afterwards() {
    let collector = Arc::new(FakeCollector::with_blocked_gpu());
    let gate = SectionGate::new();
    let timeout = Duration::from_millis(50);

    let first = collect_once_gated(collector.clone(), &gpu_and_cpu(), timeout, &gate).await;
    assert_eq!(
        error_kinds(&first),
        vec![("gpu".to_string(), "timeout".to_string())]
    );
    assert!(first.gpus.is_none());
    assert!(first.cpus.is_some(), "healthy sections are unaffected");

    for _ in 0..2 {
        let snap = collect_once_gated(collector.clone(), &gpu_and_cpu(), timeout, &gate).await;
        assert_eq!(
            error_kinds(&snap),
            vec![("gpu".to_string(), "busy".to_string())]
        );
        assert_eq!(snap.errors[0].message, "previous call still running");
        assert!(snap.gpus.is_none());
        assert!(snap.cpus.is_some());
    }
    assert_eq!(
        collector.gpu_entries.load(Ordering::SeqCst),
        1,
        "the hung reader must hold exactly one blocking task"
    );

    // Once the reader returns, the next collect takes the section again.
    collector.release_gpu();
    let deadline = Instant::now() + Duration::from_secs(5);
    let recovered = loop {
        let snap = collect_once_gated(collector.clone(), &gpu_and_cpu(), timeout, &gate).await;
        if snap.gpus.is_some() || Instant::now() > deadline {
            break snap;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    };
    assert!(
        recovered.gpus.is_some(),
        "the section recovers after release"
    );
    assert!(recovered.errors.is_empty());
    assert_eq!(collector.gpu_entries.load(Ordering::SeqCst), 2);
}

/// A panicking reader clears its flag through the drop guard, so the
/// section is not stuck `busy` forever.
#[tokio::test]
async fn a_panicking_reader_does_not_leave_the_section_busy() {
    struct Panicky;
    impl SnapshotCollector for Panicky {
        fn hostname(&self) -> String {
            "panicky".to_string()
        }
        fn collect_gpus(&self) -> Vec<GpuInfo> {
            panic!("reader exploded (expected by this test)");
        }
        fn collect_cpus(&self) -> Vec<CpuInfo> {
            Vec::new()
        }
        fn collect_memory(&self) -> Vec<MemoryInfo> {
            Vec::new()
        }
        fn collect_chassis(&self) -> Vec<ChassisInfo> {
            Vec::new()
        }
        fn collect_processes(&self) -> Vec<ProcessInfo> {
            Vec::new()
        }
        fn collect_storage(&self) -> Vec<StorageInfo> {
            Vec::new()
        }
    }

    let collector = Arc::new(Panicky);
    let gate = SectionGate::new();
    for _ in 0..2 {
        let snap = collect_once_gated(
            collector.clone(),
            &gpu_and_cpu(),
            Duration::from_secs(5),
            &gate,
        )
        .await;
        assert_eq!(
            error_kinds(&snap),
            vec![("gpu".to_string(), "panic".to_string())]
        );
    }
}

/// Sections run concurrently: 100, 200 and 300 ms readers finish in about
/// 300 ms, not the 600 ms the old sequential await took.
#[tokio::test]
async fn sections_are_collected_concurrently() {
    let collector = Arc::new(FakeCollector {
        cpu_sleep: Duration::from_millis(100),
        memory_sleep: Duration::from_millis(200),
        chassis_sleep: Duration::from_millis(300),
        ..FakeCollector::default()
    });
    let includes = SnapshotIncludes {
        cpu: true,
        memory: true,
        chassis: true,
        ..SnapshotIncludes::default()
    };

    let started = Instant::now();
    let snap = collect_once(collector, &includes, Duration::from_secs(5)).await;
    let elapsed = started.elapsed();

    assert!(snap.errors.is_empty());
    assert!(snap.cpus.is_some() && snap.memory.is_some() && snap.chassis.is_some());
    assert!(
        elapsed >= Duration::from_millis(290) && elapsed < Duration::from_millis(500),
        "expected about 300 ms (the slowest section), got {elapsed:?}"
    );
}

/// The ungated wrapper keeps its contract: requested sections are
/// populated, unrequested ones are absent, and errors come out in the
/// fixed section order.
#[tokio::test]
async fn collect_once_output_is_unchanged_for_healthy_and_timed_out_readers() {
    let healthy = Arc::new(FakeCollector::default());
    let all = SnapshotIncludes {
        gpu: true,
        cpu: true,
        memory: true,
        chassis: true,
        process: true,
        storage: true,
    };
    let snap = collect_once(healthy.clone(), &all, Duration::from_secs(5)).await;
    assert_eq!(snap.hostname, "fake");
    assert!(snap.errors.is_empty());
    assert!(
        snap.gpus.is_some()
            && snap.cpus.is_some()
            && snap.memory.is_some()
            && snap.chassis.is_some()
            && snap.processes.is_some()
            && snap.storage.is_some()
    );

    let only_memory = SnapshotIncludes {
        memory: true,
        ..SnapshotIncludes::default()
    };
    let snap = collect_once(healthy, &only_memory, Duration::from_secs(5)).await;
    assert!(snap.memory.is_some());
    assert!(snap.gpus.is_none() && snap.cpus.is_none() && snap.storage.is_none());

    // Two slow sections time out; their errors follow section order
    // (gpu before chassis) even though chassis is the first to time out
    // or finish in wall-clock terms. A fresh gate per call means a second
    // call is never `busy`.
    let slow = Arc::new(FakeCollector {
        gpu_blocked: Mutex::new(true),
        chassis_sleep: Duration::from_millis(300),
        ..FakeCollector::default()
    });
    let includes = SnapshotIncludes {
        gpu: true,
        chassis: true,
        ..SnapshotIncludes::default()
    };
    for _ in 0..2 {
        let snap = collect_once(slow.clone(), &includes, Duration::from_millis(30)).await;
        assert_eq!(
            error_kinds(&snap),
            vec![
                ("gpu".to_string(), "timeout".to_string()),
                ("chassis".to_string(), "timeout".to_string()),
            ]
        );
    }
    slow.release_gpu();
}
