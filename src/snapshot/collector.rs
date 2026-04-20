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

//! Collection pipeline for a single snapshot sample.
//!
//! Defines [`SnapshotCollector`] (the trait that real hardware readers and
//! test doubles both implement) and [`DefaultSnapshotCollector`] (the
//! production implementation that wraps the existing reader factories).
//!
//! The runtime guarantee enforced by this module is that *every* reader
//! call is wrapped in `spawn_blocking` + `tokio::time::timeout`, so a
//! misbehaving reader cannot stall the whole snapshot.

use std::sync::Arc;
use std::time::Duration;

use tokio::time::timeout;

use crate::cli::SnapshotIncludes;
use crate::device::{
    ChassisInfo, ChassisReader, CpuInfo, CpuReader, GpuInfo, GpuReader, MemoryInfo, MemoryReader,
    ProcessInfo, create_chassis_reader, get_cpu_readers, get_gpu_readers, get_memory_readers,
};
use crate::snapshot::options::{Snapshot, SnapshotError};
use crate::storage::info::StorageInfo;
use crate::storage::reader::{LocalStorageReader, StorageReader};

/// Trait that lets the snapshot collector pull from either real hardware
/// readers or test doubles. Defined here (rather than reusing the existing
/// platform-specific traits) so every section has a uniform signature that
/// can be timeout-wrapped the same way.
pub trait SnapshotCollector: Send + Sync {
    fn hostname(&self) -> String;
    fn collect_gpus(&self) -> Vec<GpuInfo>;
    fn collect_cpus(&self) -> Vec<CpuInfo>;
    fn collect_memory(&self) -> Vec<MemoryInfo>;
    fn collect_chassis(&self) -> Vec<ChassisInfo>;
    fn collect_processes(&self) -> Vec<ProcessInfo>;
    fn collect_storage(&self) -> Vec<StorageInfo>;
}

/// Default collector that wraps the existing device-reader factory
/// functions. Panics-in-readers propagate up through the awaiting task and
/// are converted to [`SnapshotError`] by the collection driver.
pub struct DefaultSnapshotCollector {
    gpu_readers: Vec<Box<dyn GpuReader>>,
    cpu_readers: Vec<Box<dyn CpuReader>>,
    memory_readers: Vec<Box<dyn MemoryReader>>,
    chassis_reader: Box<dyn ChassisReader>,
    storage_reader: Box<dyn StorageReader>,
    hostname: String,
}

impl DefaultSnapshotCollector {
    pub fn new() -> Self {
        Self {
            gpu_readers: get_gpu_readers(),
            cpu_readers: get_cpu_readers(),
            memory_readers: get_memory_readers(),
            chassis_reader: create_chassis_reader(),
            storage_reader: Box::new(LocalStorageReader::new()),
            hostname: crate::utils::get_hostname(),
        }
    }
}

impl Default for DefaultSnapshotCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl SnapshotCollector for DefaultSnapshotCollector {
    fn hostname(&self) -> String {
        self.hostname.clone()
    }

    fn collect_gpus(&self) -> Vec<GpuInfo> {
        self.gpu_readers
            .iter()
            .flat_map(|r| r.get_gpu_info())
            .collect()
    }

    fn collect_cpus(&self) -> Vec<CpuInfo> {
        self.cpu_readers
            .iter()
            .flat_map(|r| r.get_cpu_info())
            .collect()
    }

    fn collect_memory(&self) -> Vec<MemoryInfo> {
        self.memory_readers
            .iter()
            .flat_map(|r| r.get_memory_info())
            .collect()
    }

    fn collect_chassis(&self) -> Vec<ChassisInfo> {
        self.chassis_reader.get_chassis_info().into_iter().collect()
    }

    fn collect_processes(&self) -> Vec<ProcessInfo> {
        self.gpu_readers
            .iter()
            .flat_map(|r| r.get_process_info())
            .collect()
    }

    fn collect_storage(&self) -> Vec<StorageInfo> {
        self.storage_reader.get_storage_info()
    }
}

/// Run a single collection pass using the provided collector.
///
/// Each requested section is wrapped in [`tokio::task::spawn_blocking`] and
/// governed by a [`tokio::time::timeout`] so that a single hung reader
/// cannot block the whole snapshot.
pub async fn collect_once<C: SnapshotCollector + 'static>(
    collector: Arc<C>,
    includes: &SnapshotIncludes,
    per_reader_timeout: Duration,
) -> Snapshot {
    let mut snap = Snapshot::new(collector.hostname());
    let sections: &[(&str, bool)] = &[
        ("gpu", includes.gpu),
        ("cpu", includes.cpu),
        ("memory", includes.memory),
        ("chassis", includes.chassis),
        ("process", includes.process),
        ("storage", includes.storage),
    ];

    for (name, enabled) in sections {
        if !*enabled {
            continue;
        }
        let c = collector.clone();
        let section = name.to_string();
        let join_handle = tokio::task::spawn_blocking(move || match section.as_str() {
            "gpu" => SectionResult::Gpus(c.collect_gpus()),
            "cpu" => SectionResult::Cpus(c.collect_cpus()),
            "memory" => SectionResult::Memory(c.collect_memory()),
            "chassis" => SectionResult::Chassis(c.collect_chassis()),
            "process" => SectionResult::Processes(c.collect_processes()),
            "storage" => SectionResult::Storage(c.collect_storage()),
            _ => SectionResult::Empty,
        });

        match timeout(per_reader_timeout, join_handle).await {
            Ok(Ok(result)) => match result {
                SectionResult::Gpus(v) => snap.gpus = Some(v),
                SectionResult::Cpus(v) => snap.cpus = Some(v),
                SectionResult::Memory(v) => snap.memory = Some(v),
                SectionResult::Chassis(v) => snap.chassis = Some(v),
                SectionResult::Processes(v) => snap.processes = Some(v),
                SectionResult::Storage(v) => snap.storage = Some(v),
                SectionResult::Empty => {}
            },
            Ok(Err(join_err)) => {
                snap.errors.push(SnapshotError {
                    section: (*name).to_string(),
                    kind: if join_err.is_panic() {
                        "panic".to_string()
                    } else {
                        "error".to_string()
                    },
                    message: join_err.to_string(),
                });
            }
            Err(_elapsed) => {
                // The `JoinHandle` is dropped when `timeout` returns
                // `Elapsed`, but `spawn_blocking` does NOT cancel the
                // underlying OS thread — it keeps running until the
                // reader returns. A hung driver call therefore leaks a
                // Tokio blocking-pool worker for the rest of the
                // process's lifetime. Emit a warning so operators have a
                // diagnostic trail. The CLI entrypoint in `src/main.rs`
                // bounds the blast radius by using a dedicated runtime
                // with `max_blocking_threads(32)` for the snapshot
                // invocation.
                tracing::warn!(
                    section = *name,
                    timeout_ms = per_reader_timeout.as_millis() as u64,
                    "snapshot reader timed out; the blocking worker cannot be cancelled \
                     and will continue until the reader returns"
                );
                snap.errors.push(SnapshotError {
                    section: (*name).to_string(),
                    kind: "timeout".to_string(),
                    message: format!(
                        "reader exceeded timeout of {} ms",
                        per_reader_timeout.as_millis()
                    ),
                });
            }
        }
    }

    // Mirror `api::server::run_api_mode`: when the chassis reader did not
    // report a total power value but GPUs are reporting power, use the GPU
    // sum as the chassis total so `snapshot --format prometheus` stays
    // consistent with a `/metrics` scrape. This is a data-preparation step
    // specific to the Prometheus path on the server side, but it is
    // equally meaningful to JSON consumers.
    if let (Some(gpus), Some(chassis)) = (snap.gpus.as_ref(), snap.chassis.as_mut()) {
        let total_gpu_power: f64 = gpus.iter().map(|g| g.power_consumption).sum();
        if total_gpu_power > 0.0 {
            for ci in chassis.iter_mut() {
                if ci.total_power_watts.is_none() {
                    ci.total_power_watts = Some(total_gpu_power);
                }
            }
        }
    }

    snap
}

/// Internal tagged union used to shuttle the result of a single
/// `spawn_blocking` reader call back to the driver loop without reserving
/// a separate channel per section. Stays private because the tag it carries
/// is already encoded by the source-section string.
enum SectionResult {
    Gpus(Vec<GpuInfo>),
    Cpus(Vec<CpuInfo>),
    Memory(Vec<MemoryInfo>),
    Chassis(Vec<ChassisInfo>),
    Processes(Vec<ProcessInfo>),
    Storage(Vec<StorageInfo>),
    Empty,
}
