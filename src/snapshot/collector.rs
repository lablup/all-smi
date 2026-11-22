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
//! misbehaving reader cannot stall the whole snapshot. Long-lived callers
//! additionally pass a [`SectionGate`] so a hung reader holds one blocking
//! thread instead of one more per collection.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
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
        crate::device::process_list::collect_process_rows(&self.gpu_readers)
    }

    fn collect_storage(&self) -> Vec<StorageInfo> {
        self.storage_reader.get_storage_info()
    }
}

/// Section identifiers, in the fixed order results and errors are applied.
const SECTIONS: [&str; 6] = ["gpu", "cpu", "memory", "chassis", "process", "storage"];

/// In-flight flags, one per section, owned by a long-lived caller of
/// [`collect_once_gated`].
///
/// `spawn_blocking` cannot cancel a reader that hangs: after the timeout
/// the thread keeps running. Without a gate, a caller that collects every
/// interval (`record`, the `/snapshot` fallback) spawns another task into
/// the same hung reader on every tick, each one parking a blocking-pool
/// thread, until the pool's cap is reached and every `spawn_blocking` user
/// in the process queues behind them (issue #503). The gate lets a section
/// have at most one reader call in flight: while its flag is set, later
/// collections skip the section and report it as `busy`.
///
/// Cloning shares the flags, so clones gate the same readers.
#[derive(Clone, Debug, Default)]
pub struct SectionGate {
    in_flight: [Arc<AtomicBool>; 6],
}

impl SectionGate {
    /// A gate with no section in flight.
    pub fn new() -> Self {
        Self::default()
    }
}

/// Clears a section's in-flight flag when the reader call ends, including
/// by panic, or when the blocking task is dropped without ever running.
struct InFlight(Arc<AtomicBool>);

impl Drop for InFlight {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// What one section produced.
enum SectionOutcome {
    Skipped,
    Done(SectionResult),
    Busy,
    Join(tokio::task::JoinError),
    Timeout,
}

/// Run a single collection pass using the provided collector.
///
/// Each requested section is wrapped in [`tokio::task::spawn_blocking`] and
/// governed by a [`tokio::time::timeout`] so that a single hung reader
/// cannot block the whole snapshot.
///
/// One-shot form of [`collect_once_gated`]: it uses a fresh
/// [`SectionGate`], so no section is ever reported `busy`.
pub async fn collect_once<C: SnapshotCollector + ?Sized + 'static>(
    collector: Arc<C>,
    includes: &SnapshotIncludes,
    per_reader_timeout: Duration,
) -> Snapshot {
    collect_once_gated(collector, includes, per_reader_timeout, &SectionGate::new()).await
}

/// Run a single collection pass, skipping any section whose previous
/// reader call (from an earlier pass through the same `gate`) is still
/// running.
///
/// The requested sections run concurrently, each in its own
/// [`tokio::task::spawn_blocking`] under `per_reader_timeout`, so the pass
/// takes as long as its slowest section, not the sum. Results and errors
/// are applied in the fixed section order (`gpu`, `cpu`, `memory`,
/// `chassis`, `process`, `storage`) so `errors` stays deterministic.
///
/// A skipped section is absent from the snapshot and recorded as
/// `SnapshotError { kind: "busy", message: "previous call still running" }`.
/// Once the stuck reader returns, the next pass collects the section
/// normally. `gpu` and `process` share the GPU readers; calling them
/// concurrently is safe (`SnapshotCollector: Send + Sync`) though they may
/// serialize on reader-internal locks.
pub async fn collect_once_gated<C: SnapshotCollector + ?Sized + 'static>(
    collector: Arc<C>,
    includes: &SnapshotIncludes,
    per_reader_timeout: Duration,
    gate: &SectionGate,
) -> Snapshot {
    let mut snap = Snapshot::new(collector.hostname());
    let enabled = [
        includes.gpu,
        includes.cpu,
        includes.memory,
        includes.chassis,
        includes.process,
        includes.storage,
    ];

    let passes = SECTIONS.iter().enumerate().map(|(idx, name)| {
        let collector = collector.clone();
        let flag = gate.in_flight[idx].clone();
        let enabled = enabled[idx];
        async move {
            if !enabled {
                return SectionOutcome::Skipped;
            }
            if flag
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                return SectionOutcome::Busy;
            }
            let in_flight = InFlight(flag);
            let join_handle = tokio::task::spawn_blocking(move || {
                let _in_flight = in_flight;
                match idx {
                    0 => SectionResult::Gpus(collector.collect_gpus()),
                    1 => SectionResult::Cpus(collector.collect_cpus()),
                    2 => SectionResult::Memory(collector.collect_memory()),
                    3 => SectionResult::Chassis(collector.collect_chassis()),
                    // The process list comes from this one call.
                    4 => SectionResult::Processes(collector.collect_processes()),
                    _ => SectionResult::Storage(collector.collect_storage()),
                }
            });
            match timeout(per_reader_timeout, join_handle).await {
                Ok(Ok(result)) => SectionOutcome::Done(result),
                Ok(Err(join_err)) => SectionOutcome::Join(join_err),
                Err(_elapsed) => {
                    // The `JoinHandle` is dropped when `timeout` returns
                    // `Elapsed`, but `spawn_blocking` does NOT cancel the
                    // underlying OS thread — it keeps running until the
                    // reader returns, and the section's gate flag stays
                    // set until then. Emit a warning so operators have a
                    // diagnostic trail. A caller that passes a long-lived
                    // gate leaks at most this one thread per section; the
                    // one-shot CLI entrypoint in `src/main.rs` bounds the
                    // blast radius with a dedicated runtime with
                    // `max_blocking_threads(32)`.
                    tracing::warn!(
                        section = *name,
                        timeout_ms = per_reader_timeout.as_millis() as u64,
                        "snapshot reader timed out; the blocking worker cannot be cancelled \
                         and will continue until the reader returns"
                    );
                    SectionOutcome::Timeout
                }
            }
        }
    });

    let outcomes = futures_util::future::join_all(passes).await;
    for (name, outcome) in SECTIONS.iter().zip(outcomes) {
        let error = |kind: &str, message: String| SnapshotError {
            section: (*name).to_string(),
            kind: kind.to_string(),
            message,
        };
        match outcome {
            SectionOutcome::Skipped => {}
            SectionOutcome::Done(result) => match result {
                SectionResult::Gpus(v) => snap.gpus = Some(v),
                SectionResult::Cpus(v) => snap.cpus = Some(v),
                SectionResult::Memory(v) => snap.memory = Some(v),
                SectionResult::Chassis(v) => snap.chassis = Some(v),
                SectionResult::Processes(v) => snap.processes = Some(v),
                SectionResult::Storage(v) => snap.storage = Some(v),
            },
            SectionOutcome::Busy => snap
                .errors
                .push(error("busy", "previous call still running".to_string())),
            SectionOutcome::Join(join_err) => snap.errors.push(error(
                if join_err.is_panic() {
                    "panic"
                } else {
                    "error"
                },
                join_err.to_string(),
            )),
            SectionOutcome::Timeout => snap.errors.push(error(
                "timeout",
                format!(
                    "reader exceeded timeout of {} ms",
                    per_reader_timeout.as_millis()
                ),
            )),
        }
    }

    // Mirror `api::server::run_api_mode`: when the chassis reader did not
    // report a total power value but GPUs are reporting power, use the GPU
    // sum as the chassis total so `snapshot --format prometheus` stays
    // consistent with a `/metrics` scrape. This is a data-preparation step
    // specific to the Prometheus path on the server side, but it is
    // equally meaningful to JSON consumers.
    if let (Some(gpus), Some(chassis)) = (snap.gpus.as_ref(), snap.chassis.as_mut()) {
        let total_gpu_power = crate::metrics::gpu_readings::total_power_watts(gpus);
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
/// `spawn_blocking` reader call back to the driver without reserving a
/// separate channel per section.
enum SectionResult {
    Gpus(Vec<GpuInfo>),
    Cpus(Vec<CpuInfo>),
    Memory(Vec<MemoryInfo>),
    Chassis(Vec<ChassisInfo>),
    Processes(Vec<ProcessInfo>),
    Storage(Vec<StorageInfo>),
}

#[cfg(test)]
#[path = "collector_tests.rs"]
mod tests;
