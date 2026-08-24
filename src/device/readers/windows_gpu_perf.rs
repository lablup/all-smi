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

//! Vendor-neutral Windows GPU metrics, shared by the AMD and Intel
//! Windows readers.
//!
//! Both readers were WMI-only baselines: they could name a card and
//! little else. `Win32_VideoController` publishes no utilization, no
//! temperature, no per-process data, and its `AdapterRAM` field is a
//! `uint32` that saturates at 4 GB. This module closes the gaps that do
//! not need a vendor SDK, using two facilities every WDDM driver feeds:
//!
//! - **DXGI** for the true dedicated VRAM size and the adapter identity
//!   (LUID, PCI vendor / device).
//! - **PDH** for device utilization, system-wide used VRAM, and
//!   per-process VRAM. This is Task Manager's data source.
//!
//! Temperature, power, and fan speed are deliberately absent: WDDM does
//! not publish them, and they remain the job of the vendor backends
//! (Level Zero for Intel, ADL for AMD).
//!
//! ## Precedence
//!
//! A vendor backend, when it produces a reading, outranks this layer,
//! which in turn outranks the WMI baseline. Callers enforce that by
//! applying this layer first and letting the vendor augmentation
//! overwrite afterwards. Each field records where it came from in the
//! `Source: *` detail keys the Intel reader already established.
//!
//! ## Known limitation: one-shot invocations report no utilization
//!
//! Utilization is a PDH rate counter, so it only exists once two
//! collections can be differenced. Any caller that samples exactly once
//! and exits, `all-smi snapshot` at its default `--samples 1` or a
//! library consumer doing a single `get_gpu_info`, therefore sees the
//! WMI baseline utilization rather than a real figure. The polling
//! paths (`view` and `api`) are unaffected from their second poll
//! onward. Memory figures are gauges and are correct from the first
//! collection either way.
//!
//! ## Platform gating
//!
//! The DXGI and PDH FFI submodules are Windows-only. Everything else
//! here, the identifier parsing in [`ids`], the adapter pairing, and the
//! field application, is compiled under `cfg(any(target_os = "windows",
//! test))` so that a `cargo test` run on any host builds and exercises
//! it.
//!
//! That is deliberate rather than incidental. No CI job builds all-smi
//! for Windows at all, so logic reachable only on Windows ships with no
//! automated coverage whatsoever. Keeping the parsing and the
//! arithmetic testable on the Linux runner is the only coverage this
//! code can actually get; the FFI beneath it is verified by a
//! cross-compile check and by `all-smi doctor` output from real
//! machines.

pub mod ids;

#[cfg(target_os = "windows")]
mod dxgi;
#[cfg(target_os = "windows")]
mod pdh;

use crate::device::types::{GpuInfo, ProcessInfo};
use ids::{AdapterIdentity, AdapterLuid};
use std::collections::HashMap;

/// Everything the shared layer learned about one adapter.
#[derive(Clone, Debug)]
pub struct AdapterMetrics {
    pub identity: AdapterIdentity,
    /// Adapter capacity in bytes, from DXGI.
    pub total_memory: Option<u64>,
    /// Whether [`Self::total_memory`] is a dedicated VRAM pool or the
    /// shared system-memory aperture an integrated GPU uses.
    ///
    /// Recorded so the provenance the reader publishes does not claim
    /// more than the number delivers: "DXGI" and "DXGI (shared)" mean
    /// materially different things to someone reading a memory gauge.
    pub memory_is_shared: bool,
    /// System-wide GPU memory in use, in bytes, from the PDH
    /// `GPU Adapter Memory` counter. Read from `Shared Usage` when
    /// [`Self::memory_is_shared`] is set and `Dedicated Usage` otherwise,
    /// so the figure is always drawn from the same pool
    /// [`Self::total_memory`] measures.
    pub used_memory: Option<u64>,
    /// Device utilization, 0..=100, from the PDH `GPU Engine` counters.
    pub utilization: Option<f64>,
    /// Process-scoped DXGI budget, in bytes. Diagnostics only.
    pub process_budget: Option<u64>,
    /// Process-scoped DXGI current usage, in bytes. Diagnostics only.
    pub process_current_usage: Option<u64>,
}

/// Per-process GPU memory, keyed by adapter.
#[derive(Clone, Debug)]
pub struct ProcessGpuMemory {
    pub pid: u32,
    pub luid: AdapterLuid,
    /// Bytes this process holds on that adapter. Dedicated VRAM for a
    /// card with its own pool, shared-aperture memory for an integrated
    /// part, matching whichever pool [`AdapterMetrics::total_memory`]
    /// reports for the same adapter. Named for what it means rather than
    /// for one of the two counters, because reading the dedicated counter
    /// on an iGPU returns a flat zero.
    pub used_bytes: u64,
}

/// One poll's worth of vendor-neutral GPU data.
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub adapters: Vec<AdapterMetrics>,
    pub processes: Vec<ProcessGpuMemory>,
}

impl Snapshot {
    /// Test-only today. This module is gated `cfg(any(target_os =
    /// "windows", test))`, so off Windows it exists only under `test`,
    /// where this is used and nothing is reported. A non-test Windows
    /// build compiles it with no caller, which is the one configuration
    /// that sees it as dead.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn is_empty(&self) -> bool {
        self.adapters.is_empty() && self.processes.is_empty()
    }

    /// Adapter identities in enumeration order, for
    /// [`ids::match_adapter`].
    pub fn identities(&self) -> Vec<AdapterIdentity> {
        self.adapters
            .iter()
            .map(|adapter| adapter.identity.clone())
            .collect()
    }

    /// Look up the metrics for a specific adapter.
    pub fn adapter(&self, luid: AdapterLuid) -> Option<&AdapterMetrics> {
        self.adapters
            .iter()
            .find(|adapter| adapter.identity.luid == luid)
    }
}

/// How long a snapshot stays fresh enough to be reused instead of
/// taking another PDH collection.
///
/// This is not an optimisation, it is a correctness requirement.
/// `Utilization Percentage` is a rate computed between consecutive
/// collections, so two collections microseconds apart yield a rate over
/// a microsecond, which reads as 0 or as a clamped 100 rather than as
/// the load over the poll interval.
///
/// More than one reader can be registered at once: `get_gpu_readers`
/// tests for AMD and Intel adapters with independent `if`s, and a laptop
/// with an Intel iGPU beside a Radeon dGPU (or an AMD APU beside an Arc
/// card) registers both. The collectors then call `get_gpu_info` on each
/// in turn within the same poll. Without coalescing, whichever reader
/// runs second would always see a near-zero interval, deterministically,
/// and its utilization would be useless. `get_gpu_info_by_uuid`'s
/// default body has the same shape when called in a loop.
///
/// 500 ms is comfortably below all-smi's fastest poll interval (1 s for
/// local monitoring) so consecutive polls still each get a fresh
/// collection, and comfortably above the time it takes to walk a
/// handful of readers.
#[cfg(target_os = "windows")]
const SNAPSHOT_COALESCE_WINDOW: std::time::Duration = std::time::Duration::from_millis(500);

/// Take a sample, reusing the cached one when it is younger than
/// [`SNAPSHOT_COALESCE_WINDOW`].
///
/// Safe to call from every registered reader in the same poll: only the
/// first call collects, and the rest share its result.
#[cfg(target_os = "windows")]
pub fn snapshot() -> Snapshot {
    if let Some(cached) = cached_snapshot(SNAPSHOT_COALESCE_WINDOW) {
        return cached;
    }
    // DXGI runs first, and its answer decides whether the shared-aperture
    // PDH counters are worth sampling at all. A machine with only discrete
    // cards never adds them to the query.
    let dxgi_adapters = dxgi::enumerate();
    let resolved: Vec<_> = dxgi_adapters
        .into_iter()
        .map(|adapter| {
            let (total_memory, memory_is_shared) = resolve_adapter_memory(
                adapter.dedicated_video_memory,
                adapter.shared_system_memory,
            );
            (adapter, total_memory, memory_is_shared)
        })
        .collect();
    let shared_luids: std::collections::HashSet<AdapterLuid> = resolved
        .iter()
        .filter(|(_, _, is_shared)| *is_shared)
        .map(|(adapter, _, _)| adapter.identity.luid)
        .collect();
    let sample = pdh::sample(!shared_luids.is_empty());

    let adapters = resolved
        .into_iter()
        .map(|(adapter, total_memory, memory_is_shared)| {
            let luid = adapter.identity.luid;
            let used_memory = select_adapter_usage(
                memory_is_shared,
                sample.adapter_memory.get(&luid).copied(),
                sample.adapter_shared_memory.get(&luid).copied(),
            );
            AdapterMetrics {
                identity: adapter.identity,
                total_memory,
                memory_is_shared,
                used_memory,
                utilization: sample.utilization.get(&luid).copied(),
                process_budget: adapter.process_budget,
                process_current_usage: adapter.process_current_usage,
            }
        })
        .collect();

    let processes = merge_process_rows(
        sample.process_memory,
        sample.process_shared_memory,
        &shared_luids,
    );

    let snapshot = Snapshot {
        adapters,
        processes,
    };
    store_snapshot(&snapshot);
    snapshot
}

/// Non-Windows builds have nothing to sample. The readers that call this
/// are themselves Windows-gated; the stub exists so the surrounding
/// logic and its tests compile on every platform.
#[cfg(not(target_os = "windows"))]
pub fn snapshot() -> Snapshot {
    Snapshot::default()
}

#[cfg(target_os = "windows")]
type SnapshotCache = std::sync::Mutex<Option<(std::time::Instant, Snapshot)>>;

#[cfg(target_os = "windows")]
static LAST_SNAPSHOT: once_cell::sync::OnceCell<SnapshotCache> = once_cell::sync::OnceCell::new();

#[cfg(target_os = "windows")]
fn snapshot_cache() -> &'static SnapshotCache {
    LAST_SNAPSHOT.get_or_init(|| std::sync::Mutex::new(None))
}

/// The cached snapshot, if it is younger than `max_age`.
#[cfg(target_os = "windows")]
fn cached_snapshot(max_age: std::time::Duration) -> Option<Snapshot> {
    let guard = match snapshot_cache().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard
        .as_ref()
        .and_then(|(taken_at, snapshot)| (taken_at.elapsed() < max_age).then(|| snapshot.clone()))
}

#[cfg(target_os = "windows")]
fn store_snapshot(snapshot: &Snapshot) {
    let mut guard = match snapshot_cache().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    *guard = Some((std::time::Instant::now(), snapshot.clone()));
}

/// The most recent [`snapshot`], without consuming a PDH collection.
///
/// `get_process_info` uses this so a poll that already sampled from
/// `get_gpu_info` does not disturb the utilization rate. Unlike
/// [`snapshot`] this never collects, and returns an empty snapshot when
/// nothing has been sampled yet.
#[cfg(target_os = "windows")]
pub fn latest() -> Snapshot {
    let guard = match snapshot_cache().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard
        .as_ref()
        .map(|(_, snapshot)| snapshot.clone())
        .unwrap_or_default()
}

#[cfg(not(target_os = "windows"))]
pub fn latest() -> Snapshot {
    Snapshot::default()
}

/// Whether the PDH GPU counter query could be opened.
#[cfg(target_os = "windows")]
pub fn pdh_query_available() -> bool {
    pdh::query_available()
}

#[cfg(not(target_os = "windows"))]
pub fn pdh_query_available() -> bool {
    false
}

// `Metrics Source` composition moved to `detail_keys` so the Level Zero
// backend can append to it too; that module is compiled on every target,
// this one is not. Re-exported here because `amd_adl` and downstream
// consumers already reach it through this path.
pub use crate::device::readers::detail_keys::note_metrics_source;

/// Layer this adapter's metrics onto a WMI-derived [`GpuInfo`].
///
/// Only fields that carry real data are written, so a partially
/// available adapter (DXGI present, PDH counters absent, which is the
/// shape of a GitHub-hosted Windows runner) upgrades VRAM and leaves
/// utilization at the baseline rather than zeroing anything that was
/// already known.
/// Smallest dedicated pool that can plausibly be a discrete card's own
/// VRAM.
///
/// The previous rule was "a non-zero dedicated pool means a discrete
/// card", written from the premise that integrated graphics report no
/// dedicated pool at all. That premise is false on current hardware:
/// modern Intel and AMD integrated parts publish a small stolen-memory
/// carve-out through DXGI, and 128 MiB is the classic value. The old rule
/// therefore took the carve-out as the whole capacity, which is what
/// reported an Arc B390 as a 128 MiB card (issue #364).
///
/// 1 GiB separates the two populations with room on both sides. No
/// discrete card has ever shipped with less, and the stock carve-outs are
/// 64/128/256/512 MiB. A machine whose firmware is configured for a large
/// carve-out (some AMD APUs allow 2 GiB or more) lands above the floor and
/// is reported at that size, which is the amount the operator actually
/// set aside.
const MIN_DISCRETE_DEDICATED_BYTES: u64 = 1024 * 1024 * 1024;

/// Choose an adapter's total memory and say whether it is a shared
/// aperture rather than a dedicated pool.
///
/// Split out of [`build_adapter_metrics`] so the decision is reachable by
/// a test runner. It drives every Windows GPU this project reports:
/// `intel_gpu_windows` and `amd_windows` both reach it through
/// [`augment_gpus`], so a change here moves Radeon APUs as well as Intel
/// iGPUs.
fn resolve_adapter_memory(dedicated: u64, shared: u64) -> (Option<u64>, bool) {
    if dedicated >= MIN_DISCRETE_DEDICATED_BYTES {
        return (Some(dedicated), false);
    }
    if shared > 0 {
        // Either a carve-out below the floor or no dedicated pool at all.
        // The shared aperture is the memory the device can actually
        // address, so it is the honest capacity.
        return (Some(shared), true);
    }
    if dedicated > 0 {
        // Below the floor with nothing shared to fall back to. Report
        // what the adapter claims rather than nothing; a small pool is
        // still better than an unknown one.
        return (Some(dedicated), false);
    }
    (None, false)
}

/// Pick the usage figure drawn from the same pool the capacity describes.
///
/// An integrated adapter's `Dedicated Usage` instances read a flat zero,
/// because nothing is allocated out of its small stolen carve-out. Pairing
/// that with a shared-aperture total reported every Intel and AMD iGPU on
/// Windows as using no memory at all.
///
/// There is deliberately no cross-pool fallback. `Source: Memory Used` is
/// labelled from the same `memory_is_shared` flag, so substituting the
/// other pool would publish a number under a label that does not describe
/// it. An absent counter yields `None`, and the caller leaves whatever the
/// WMI baseline held, which is what already happens on a host publishing no
/// GPU memory counters at all.
fn select_adapter_usage(
    memory_is_shared: bool,
    dedicated: Option<u64>,
    shared: Option<u64>,
) -> Option<u64> {
    if memory_is_shared { shared } else { dedicated }
}

/// Combine the two per-process counter families into one row set, taking
/// each pid's figure from the pool its adapter is measured against.
///
/// Zero-byte rows are dropped: a process that has touched the GPU at some
/// point keeps an instance alive at zero, and listing it would fill the
/// process view with entries that hold nothing.
fn merge_process_rows(
    dedicated: Vec<(ids::GpuProcessMemoryInstance, u64)>,
    shared: Vec<(ids::GpuProcessMemoryInstance, u64)>,
    shared_luids: &std::collections::HashSet<AdapterLuid>,
) -> Vec<ProcessGpuMemory> {
    dedicated
        .into_iter()
        .filter(|(instance, _)| !shared_luids.contains(&instance.luid))
        .chain(
            shared
                .into_iter()
                .filter(|(instance, _)| shared_luids.contains(&instance.luid)),
        )
        .filter(|(_, bytes)| *bytes > 0)
        .map(|(instance, bytes)| ProcessGpuMemory {
            pid: instance.pid,
            luid: instance.luid,
            used_bytes: bytes,
        })
        .collect()
}

pub fn apply_to_gpu_info(gpu: &mut GpuInfo, metrics: &AdapterMetrics) {
    let mut touched_dxgi = false;
    let mut touched_pdh = false;

    if let Some(total) = metrics.total_memory {
        gpu.total_memory = total;
        gpu.detail.insert(
            "Source: Memory".to_string(),
            if metrics.memory_is_shared {
                "DXGI (shared)"
            } else {
                "DXGI"
            }
            .to_string(),
        );
        // Fill the discrete/integrated variant when the caller could not
        // decide from the device name. Whether the adapter owns a
        // dedicated pool or addresses a shared aperture is the definition
        // of the distinction, so DXGI answers it directly rather than by
        // inference from a marketing string. `or_insert_with` keeps a
        // name-derived answer that the reader already trusted; only the
        // "unknown numbered part" case reaches this (issue #364).
        gpu.detail.entry("Variant".to_string()).or_insert_with(|| {
            if metrics.memory_is_shared {
                "Integrated".to_string()
            } else {
                "Discrete".to_string()
            }
        });
        touched_dxgi = true;
    }

    // Both DXGI video-memory figures are scoped to the calling process.
    // They are labelled as such and kept out of `used_memory`, which
    // must stay system-wide; reading either as a device-level number
    // would understate a busy GPU by whatever other processes hold.
    if let Some(budget) = metrics.process_budget {
        gpu.detail.insert(
            "VRAM Budget (this process)".to_string(),
            format!("{budget} bytes"),
        );
        touched_dxgi = true;
    }
    if let Some(usage) = metrics.process_current_usage {
        gpu.detail.insert(
            "VRAM Usage (this process)".to_string(),
            format!("{usage} bytes"),
        );
        touched_dxgi = true;
    }

    if let Some(used) = metrics.used_memory {
        gpu.used_memory = used;
        // Integrated adapters are measured against the shared aperture,
        // discrete ones against their dedicated pool. Both are honest
        // usage figures for the capacity reported next to them, but they
        // count different memory, so the label says which.
        gpu.detail.insert(
            "Source: Memory Used".to_string(),
            if metrics.memory_is_shared {
                "PDH (shared)"
            } else {
                "PDH"
            }
            .to_string(),
        );
        touched_pdh = true;
    }

    if let Some(utilization) = metrics.utilization {
        gpu.utilization = utilization;
        gpu.detail
            .insert("Source: Utilization".to_string(), "PDH".to_string());
        touched_pdh = true;
    }

    if touched_dxgi {
        note_metrics_source(&mut gpu.detail, "DXGI");
    }
    if touched_pdh {
        note_metrics_source(&mut gpu.detail, "PDH");
    }
}

/// Map from adapter LUID to the `(index, uuid)` of the GPU it was
/// paired with. Readers keep the most recent one so per-process PDH rows
/// can be attributed to a card.
pub type AdapterIndex = HashMap<AdapterLuid, (usize, String)>;

/// Pair each WMI-derived GPU with a DXGI adapter, apply that adapter's
/// metrics, and return the LUID mapping.
///
/// Split from [`augment_gpus`] so the pairing and application logic can
/// be exercised with a synthetic snapshot on any platform. The Windows
/// FFI is only reachable through `snapshot()`, which the thin wrapper
/// calls.
pub fn pair_and_apply(gpus: &mut [GpuInfo], snapshot: &Snapshot) -> AdapterIndex {
    let mut adapter_index = AdapterIndex::new();
    if snapshot.adapters.is_empty() {
        return adapter_index;
    }
    let identities = snapshot.identities();

    for (ordinal, gpu) in gpus.iter_mut().enumerate() {
        // The reader stores `PNPDeviceID` as the GPU uuid, and that is
        // what carries the PCI vendor / device ids the matcher prefers.
        let Some(identity) =
            ids::match_adapter(&identities, Some(gpu.uuid.as_str()), &gpu.name, ordinal)
        else {
            continue;
        };
        let luid = identity.luid;
        if let Some(metrics) = snapshot.adapter(luid) {
            apply_to_gpu_info(gpu, metrics);
        }
        adapter_index.insert(luid, (ordinal, gpu.uuid.clone()));
    }

    adapter_index
}

/// Take a fresh sample and layer it onto `gpus`.
///
/// Call once per poll from `get_gpu_info`; the returned index feeds
/// [`process_rows`].
pub fn augment_gpus(gpus: &mut [GpuInfo]) -> AdapterIndex {
    let snapshot = snapshot();
    pair_and_apply(gpus, &snapshot)
}

/// Build the merged per-process rows a reader's `get_process_info`
/// should return.
///
/// Two things happen here beyond reading the snapshot:
///
/// 1. If `adapter_index` is empty, nothing has paired adapters yet. That
///    is the case for one-shot entry points such as
///    `all-smi snapshot --include process`, which never call
///    `get_gpu_info`. Rather than reporting an empty list forever, the
///    caller is given a chance to populate the index first via
///    `refresh`.
/// 2. The GPU rows are merged against the system process table, matching
///    what `nvidia`, `nvidia_jetson`, and `tenstorrent` do inside their
///    own `get_process_info`. The API and snapshot collectors consume
///    `get_process_info` directly without merging, so returning bare
///    skeleton rows would export processes with empty names and zeroed
///    CPU and RSS.
pub fn process_rows_with<F>(adapter_index: &AdapterIndex, refresh: F) -> Vec<ProcessInfo>
where
    F: FnOnce() -> AdapterIndex,
{
    let owned;
    let adapter_index = if adapter_index.is_empty() {
        owned = refresh();
        &owned
    } else {
        adapter_index
    };
    if adapter_index.is_empty() {
        return Vec::new();
    }

    let gpu_rows = process_rows_from(&latest(), adapter_index);
    if gpu_rows.is_empty() {
        return Vec::new();
    }

    let gpu_pids: std::collections::HashSet<u32> = gpu_rows.iter().map(|row| row.pid).collect();
    let all_processes = crate::utils::system::with_global_system(|system| {
        system.refresh_processes_specifics(
            sysinfo::ProcessesToUpdate::All,
            true,
            sysinfo::ProcessRefreshKind::everything().with_user(sysinfo::UpdateKind::Always),
        );
        system.refresh_memory();
        crate::device::process_list::get_all_processes(system, &gpu_pids)
    });

    crate::device::process_list::merge_gpu_processes(all_processes, gpu_rows)
}

/// Attribute each per-process sample to the card its LUID names.
///
/// Split from [`process_rows`] for the same reason as
/// [`pair_and_apply`]: it makes the attribution testable without
/// Windows. Rows whose adapter was never paired are dropped rather than
/// guessed at, so a card the WMI vendor filter excluded (an NVIDIA GPU
/// alongside an AMD one, say) does not have its processes reported
/// against the wrong device.
pub fn process_rows_from(snapshot: &Snapshot, adapter_index: &AdapterIndex) -> Vec<ProcessInfo> {
    snapshot
        .processes
        .iter()
        .filter_map(|process| {
            let (device_id, uuid) = adapter_index.get(&process.luid)?;
            Some(gpu_process_row(
                *device_id,
                uuid,
                process.pid,
                process.used_bytes,
            ))
        })
        .collect()
}

/// Build the GPU-attributed process row for a PDH per-process sample.
///
/// Only the GPU-specific fields are populated.
/// [`crate::device::process_list::merge_gpu_processes`] joins these rows
/// against the system process table by pid and supplies the name, user,
/// CPU time, and system-memory figures, so filling them here would be
/// both wasted work and a second source of truth that could disagree.
pub fn gpu_process_row(
    device_id: usize,
    device_uuid: &str,
    pid: u32,
    used_memory: u64,
) -> ProcessInfo {
    ProcessInfo {
        device_id,
        device_uuid: device_uuid.to_string(),
        pid,
        process_name: String::new(),
        used_memory,
        cpu_percent: 0.0,
        memory_percent: 0.0,
        memory_rss: 0,
        memory_vms: 0,
        user: String::new(),
        state: String::new(),
        start_time: String::new(),
        cpu_time: 0,
        command: String::new(),
        ppid: 0,
        threads: 0,
        uses_gpu: true,
        priority: 0,
        nice_value: 0,
        // PDH publishes GPU *memory* per process. Per-process engine
        // utilization would need the GPU Engine counters keyed by pid,
        // which report per-engine shares that do not reduce to a single
        // per-process figure the way memory does. Left at zero rather
        // than guessed.
        gpu_utilization: 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: u64 = 1024 * 1024;
    const GIB: u64 = 1024 * MIB;

    // ---------- issue #364: the dedicated-pool premise ----------

    /// The reported defect. An Arc B390 publishes a 128 MiB DXGI
    /// carve-out, the old rule took any non-zero dedicated pool as the
    /// card's VRAM, and the device reported as a 128 MiB card.
    #[test]
    fn small_carve_out_reports_the_shared_aperture() {
        let (total, shared) = resolve_adapter_memory(128 * MIB, 16 * GIB);
        assert_eq!(total, Some(16 * GIB));
        assert!(shared, "a carve-out below the floor is a shared aperture");
    }

    /// The same premise breaks on AMD: `amd_windows` reaches this through
    /// `augment_gpus`, so every Radeon APU on Windows moves with it. The
    /// issue calls this out as test-plan scope rather than a review
    /// surprise.
    #[test]
    fn amd_apu_carve_out_reports_the_shared_aperture() {
        for carve_out in [64 * MIB, 128 * MIB, 256 * MIB, 512 * MIB] {
            let (total, shared) = resolve_adapter_memory(carve_out, 32 * GIB);
            assert_eq!(total, Some(32 * GIB), "carve-out {carve_out} bytes");
            assert!(shared, "carve-out {carve_out} bytes");
        }
    }

    /// A discrete card keeps its dedicated pool. This is the behaviour the
    /// old rule got right and the fix must not trade away.
    #[test]
    fn discrete_card_keeps_its_dedicated_pool() {
        for vram in [2 * GIB, 8 * GIB, 12 * GIB, 24 * GIB] {
            let (total, shared) = resolve_adapter_memory(vram, 16 * GIB);
            assert_eq!(total, Some(vram), "vram {vram} bytes");
            assert!(!shared, "vram {vram} bytes");
        }
    }

    /// An integrated part that reports no dedicated pool at all: the case
    /// the original rule was written for, unchanged.
    #[test]
    fn zero_dedicated_still_reports_the_shared_aperture() {
        let (total, shared) = resolve_adapter_memory(0, 8 * GIB);
        assert_eq!(total, Some(8 * GIB));
        assert!(shared);
    }

    /// A firmware-configured carve-out at or above the floor is what the
    /// operator set aside, so it is reported as dedicated rather than
    /// silently replaced by the aperture.
    #[test]
    fn large_configured_carve_out_is_taken_at_face_value() {
        let (total, shared) = resolve_adapter_memory(2 * GIB, 32 * GIB);
        assert_eq!(total, Some(2 * GIB));
        assert!(!shared);
    }

    /// Nothing shared to fall back to: report the small pool rather than
    /// nothing at all.
    #[test]
    fn small_pool_with_no_aperture_is_reported_as_is() {
        let (total, shared) = resolve_adapter_memory(128 * MIB, 0);
        assert_eq!(total, Some(128 * MIB));
        assert!(!shared);
    }

    #[test]
    fn no_memory_information_reports_nothing() {
        assert_eq!(resolve_adapter_memory(0, 0), (None, false));
    }

    /// The floor itself is inclusive, so the boundary is pinned rather
    /// than left to drift with a future edit.
    #[test]
    fn the_discrete_floor_is_inclusive() {
        let (_, shared_at) = resolve_adapter_memory(MIN_DISCRETE_DEDICATED_BYTES, 32 * GIB);
        assert!(!shared_at, "exactly at the floor counts as dedicated");
        let (_, shared_below) = resolve_adapter_memory(MIN_DISCRETE_DEDICATED_BYTES - 1, 32 * GIB);
        assert!(shared_below, "one byte below the floor is a carve-out");
    }
    use crate::device::types::{GPU_METRIC_UNAVAILABLE, GpuInfo};

    fn blank_gpu() -> GpuInfo {
        let mut detail = HashMap::new();
        detail.insert("Metrics Source".to_string(), "WMI".to_string());
        detail.insert("Source: Utilization".to_string(), "unavailable".to_string());
        detail.insert("Source: Power".to_string(), "unavailable".to_string());
        detail.insert("Source: Memory".to_string(), "WMI".to_string());
        GpuInfo {
            uuid: "PCI\\VEN_1002&DEV_744C".to_string(),
            time: String::new(),
            name: "AMD Radeon RX 7900 XTX".to_string(),
            device_type: "GPU".to_string(),
            host_id: String::new(),
            hostname: String::new(),
            instance: String::new(),
            utilization: GPU_METRIC_UNAVAILABLE,
            ane_utilization: 0.0,
            dla_utilization: None,
            tensorcore_utilization: None,
            temperature: 0,
            used_memory: 0,
            total_memory: 4_294_967_295,
            frequency: 0,
            power_consumption: GPU_METRIC_UNAVAILABLE,
            gpu_core_count: None,
            temperature_threshold_slowdown: None,
            temperature_threshold_shutdown: None,
            temperature_threshold_max_operating: None,
            temperature_threshold_acoustic: None,
            performance_state: None,
            fan_speed_rpm: None,
            numa_node_id: None,
            gsp_firmware_mode: None,
            gsp_firmware_version: None,
            nvlink_remote_devices: Vec::new(),
            gpm_metrics: None,
            detail,
        }
    }

    fn metrics(total: Option<u64>, used: Option<u64>, utilization: Option<f64>) -> AdapterMetrics {
        AdapterMetrics {
            identity: AdapterIdentity {
                luid: AdapterLuid::new(0, 0xD3F5),
                vendor_id: 0x1002,
                device_id: 0x744C,
                description: "AMD Radeon RX 7900 XTX".to_string(),
            },
            total_memory: total,
            memory_is_shared: false,
            used_memory: used,
            utilization,
            process_budget: None,
            process_current_usage: None,
        }
    }

    /// DXGI settles the discrete/integrated question when the device
    /// name could not. Owning a dedicated pool versus addressing a shared
    /// aperture is the distinction itself, so this is a direct answer
    /// rather than an inference from a marketing string (issue #364).
    #[test]
    fn dxgi_fills_the_variant_the_name_could_not_decide() {
        let mut shared = blank_gpu();
        let mut shared_metrics = metrics(Some(16 * GIB), None, None);
        shared_metrics.memory_is_shared = true;
        apply_to_gpu_info(&mut shared, &shared_metrics);
        assert_eq!(shared.detail["Variant"], "Integrated");

        let mut dedicated = blank_gpu();
        apply_to_gpu_info(&mut dedicated, &metrics(Some(8 * GIB), None, None));
        assert_eq!(dedicated.detail["Variant"], "Discrete");
    }

    /// A variant the reader already decided from a known SKU wins. DXGI
    /// only fills the gap; it does not overrule a curated answer.
    #[test]
    fn an_existing_variant_is_not_overwritten() {
        let mut gpu = blank_gpu();
        gpu.detail
            .insert("Variant".to_string(), "Discrete".to_string());
        let mut shared_metrics = metrics(Some(16 * GIB), None, None);
        shared_metrics.memory_is_shared = true;
        apply_to_gpu_info(&mut gpu, &shared_metrics);
        assert_eq!(gpu.detail["Variant"], "Discrete");
    }

    #[test]
    fn dxgi_total_replaces_the_truncated_wmi_value() {
        let mut gpu = blank_gpu();
        // 24 GB, well beyond what Win32_VideoController.AdapterRAM can
        // represent.
        apply_to_gpu_info(&mut gpu, &metrics(Some(25_769_803_776), None, None));
        assert_eq!(gpu.total_memory, 25_769_803_776);
        assert_eq!(gpu.detail["Source: Memory"], "DXGI");
        assert_eq!(gpu.detail["Metrics Source"], "WMI + DXGI");
    }

    #[test]
    fn pdh_fields_are_recorded_with_their_source() {
        let mut gpu = blank_gpu();
        apply_to_gpu_info(
            &mut gpu,
            &metrics(Some(8_589_934_592), Some(2_147_483_648), Some(42.5)),
        );
        assert_eq!(gpu.utilization, 42.5);
        assert_eq!(gpu.used_memory, 2_147_483_648);
        assert_eq!(gpu.detail["Source: Utilization"], "PDH");
        assert_eq!(gpu.detail["Source: Memory Used"], "PDH");
        assert_eq!(gpu.detail["Metrics Source"], "WMI + DXGI + PDH");
    }

    #[test]
    fn absent_fields_leave_the_baseline_untouched() {
        // The shape of a GitHub-hosted Windows runner: DXGI answers, no
        // GPU counter instances exist. VRAM must upgrade while
        // utilization stays at its baseline and is not falsely
        // attributed to PDH.
        let mut gpu = blank_gpu();
        apply_to_gpu_info(&mut gpu, &metrics(Some(1_073_741_824), None, None));
        assert_eq!(gpu.total_memory, 1_073_741_824);
        assert_eq!(gpu.utilization_reading(), None);
        assert_eq!(gpu.power_consumption_reading(), None);
        assert_eq!(gpu.detail["Source: Utilization"], "unavailable");
        assert_eq!(gpu.detail["Source: Power"], "unavailable");
        assert_eq!(gpu.detail["Metrics Source"], "WMI + DXGI");
    }

    #[test]
    fn applying_twice_does_not_grow_the_source_string() {
        let mut gpu = blank_gpu();
        let m = metrics(Some(1), Some(2), Some(3.0));
        apply_to_gpu_info(&mut gpu, &m);
        apply_to_gpu_info(&mut gpu, &m);
        apply_to_gpu_info(&mut gpu, &m);
        assert_eq!(gpu.detail["Metrics Source"], "WMI + DXGI + PDH");
    }

    #[test]
    fn metrics_source_starts_clean_when_absent() {
        let mut detail = HashMap::new();
        note_metrics_source(&mut detail, "DXGI");
        assert_eq!(detail["Metrics Source"], "DXGI");
        note_metrics_source(&mut detail, "PDH");
        assert_eq!(detail["Metrics Source"], "DXGI + PDH");
    }

    fn snapshot_with(adapters: Vec<AdapterMetrics>, processes: Vec<ProcessGpuMemory>) -> Snapshot {
        Snapshot {
            adapters,
            processes,
        }
    }

    #[test]
    fn pairs_gpus_to_adapters_and_returns_the_luid_index() {
        let mut gpus = vec![blank_gpu()];
        let snapshot = snapshot_with(
            vec![metrics(Some(25_769_803_776), Some(1024), Some(77.0))],
            vec![],
        );

        let index = pair_and_apply(&mut gpus, &snapshot);

        assert_eq!(gpus[0].total_memory, 25_769_803_776);
        assert_eq!(gpus[0].utilization, 77.0);
        assert_eq!(gpus[0].used_memory, 1024);
        // The uuid is the PNPDeviceID, and it is what the per-process
        // attribution keys on.
        assert_eq!(
            index.get(&AdapterLuid::new(0, 0xD3F5)),
            Some(&(0usize, "PCI\\VEN_1002&DEV_744C".to_string()))
        );
    }

    #[test]
    fn pairing_an_empty_snapshot_changes_nothing() {
        let mut gpus = vec![blank_gpu()];
        let before = gpus[0].total_memory;
        let index = pair_and_apply(&mut gpus, &Snapshot::default());
        assert!(index.is_empty());
        assert_eq!(gpus[0].total_memory, before);
        assert_eq!(gpus[0].detail["Metrics Source"], "WMI");
    }

    #[test]
    fn unmatched_gpus_keep_the_wmi_baseline() {
        // A DXGI adapter for a different vendor entirely, and a name
        // that shares no substring, so only the ordinal fallback could
        // pair them.
        let mut gpus = vec![blank_gpu(), blank_gpu()];
        gpus[1].uuid = "PCI\\VEN_10DE&DEV_2684".to_string();
        gpus[1].name = "NVIDIA GeForce RTX 4090".to_string();

        let snapshot = snapshot_with(vec![metrics(Some(8_589_934_592), None, Some(10.0))], vec![]);
        let index = pair_and_apply(&mut gpus, &snapshot);

        // First GPU matches on PCI ids.
        assert_eq!(gpus[0].total_memory, 8_589_934_592);
        // Second has ordinal 1, which is out of range for a one-adapter
        // snapshot, so it is left alone rather than mis-attributed.
        assert_eq!(gpus[1].total_memory, 4_294_967_295);
        assert_eq!(gpus[1].detail["Metrics Source"], "WMI");
        assert_eq!(index.len(), 1);
    }

    #[test]
    fn process_rows_carry_the_gpu_identity_and_leave_the_rest_to_the_merge() {
        let row = gpu_process_row(2, "PCI\\VEN_1002&DEV_744C", 4242, 536_870_912);
        assert_eq!(row.pid, 4242);
        assert_eq!(row.device_id, 2);
        assert_eq!(row.device_uuid, "PCI\\VEN_1002&DEV_744C");
        assert_eq!(row.used_memory, 536_870_912);
        assert!(row.uses_gpu);
        // Deliberately blank: merge_gpu_processes fills these from the
        // system process table.
        assert!(row.process_name.is_empty());
        assert!(row.user.is_empty());
    }

    #[test]
    fn process_rows_are_attributed_to_the_matching_adapter() {
        let known = AdapterLuid::new(0, 0xD3F5);
        let mut adapter_index = AdapterIndex::new();
        adapter_index.insert(known, (0, "PCI\\VEN_1002&DEV_744C".to_string()));

        let snapshot = snapshot_with(
            vec![],
            vec![
                ProcessGpuMemory {
                    pid: 4242,
                    luid: known,
                    used_bytes: 536_870_912,
                },
                // A card this reader never paired, for example an NVIDIA
                // GPU sitting alongside the AMD one. Its processes must
                // be dropped, not attributed to the wrong device.
                ProcessGpuMemory {
                    pid: 99,
                    luid: AdapterLuid::new(0, 0xFFFF),
                    used_bytes: 1,
                },
            ],
        );

        let rows = process_rows_from(&snapshot, &adapter_index);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].pid, 4242);
        assert_eq!(rows[0].used_memory, 536_870_912);
        assert_eq!(rows[0].device_uuid, "PCI\\VEN_1002&DEV_744C");
        assert_eq!(rows[0].device_id, 0);
    }

    #[test]
    fn process_scoped_dxgi_figures_are_labelled_and_kept_out_of_used_memory() {
        let mut gpu = blank_gpu();
        let mut m = metrics(Some(8_589_934_592), None, None);
        m.process_budget = Some(7_000_000_000);
        m.process_current_usage = Some(123_456);

        apply_to_gpu_info(&mut gpu, &m);

        // Neither DXGI figure may leak into the device-level number.
        assert_eq!(gpu.used_memory, 0);
        assert_eq!(gpu.detail["VRAM Budget (this process)"], "7000000000 bytes");
        assert_eq!(gpu.detail["VRAM Usage (this process)"], "123456 bytes");
        assert!(!gpu.detail.contains_key("Source: Memory Used"));
    }

    #[test]
    fn the_platform_entry_points_never_panic() {
        // The readers that call these are Windows-gated, but the entry
        // points compile everywhere. On a non-Windows host each must be
        // an inert no-op so the surrounding logic can be tested; on
        // Windows they touch real hardware, so only assert they return.
        let mut gpus = vec![blank_gpu()];
        let index = augment_gpus(&mut gpus);
        let _ = process_rows_with(&index, AdapterIndex::new);
        let _ = pdh_query_available();
        let _ = latest();

        #[cfg(not(target_os = "windows"))]
        {
            assert!(snapshot().is_empty());
            assert!(latest().is_empty());
            assert!(!pdh_query_available());
            assert!(index.is_empty());
            // An empty index triggers the refresh closure; when that
            // also comes back empty the result must be no rows rather
            // than a panic or a wasted system-process enumeration.
            assert!(process_rows_with(&index, AdapterIndex::new).is_empty());
            // An inert layer must leave the WMI baseline exactly as it
            // found it.
            assert_eq!(gpus[0].total_memory, 4_294_967_295);
            assert_eq!(gpus[0].detail["Metrics Source"], "WMI");
        }
    }

    #[test]
    fn an_empty_index_consults_the_refresh_closure_exactly_once() {
        use std::cell::Cell;

        // The one-shot path (`all-smi snapshot --include process`) never
        // calls `get_gpu_info`, so the index starts empty and the
        // closure is what populates it.
        let calls = Cell::new(0);
        let rows = process_rows_with(&AdapterIndex::new(), || {
            calls.set(calls.get() + 1);
            AdapterIndex::new()
        });
        assert_eq!(calls.get(), 1);
        assert!(rows.is_empty());

        // A populated index must not pay for the refresh at all.
        let calls = Cell::new(0);
        let mut populated = AdapterIndex::new();
        populated.insert(AdapterLuid::new(0, 1), (0, "uuid".to_string()));
        let _ = process_rows_with(&populated, || {
            calls.set(calls.get() + 1);
            AdapterIndex::new()
        });
        assert_eq!(calls.get(), 0);
    }

    #[test]
    fn snapshot_helpers_behave_on_an_empty_snapshot() {
        let snapshot = Snapshot::default();
        assert!(snapshot.is_empty());
        assert!(snapshot.identities().is_empty());
        assert!(snapshot.adapter(AdapterLuid::new(0, 1)).is_none());
    }

    // -----------------------------------------------------------------
    // Which memory pool a figure is drawn from (issue #364)
    // -----------------------------------------------------------------

    /// The defect: an integrated adapter reports its capacity as the shared
    /// aperture, but usage was read from `Dedicated Usage`, whose instances
    /// are a flat zero on such a part. Every Intel and AMD iGPU on Windows
    /// showed 0 bytes in use against a multi-gigabyte total.
    #[test]
    fn an_integrated_adapter_is_measured_against_its_aperture() {
        assert_eq!(
            select_adapter_usage(true, Some(0), Some(3_221_225_472)),
            Some(3_221_225_472)
        );
    }

    #[test]
    fn a_discrete_adapter_is_measured_against_its_dedicated_pool() {
        assert_eq!(
            select_adapter_usage(false, Some(8_589_934_592), Some(1_048_576)),
            Some(8_589_934_592)
        );
    }

    /// A counter can fail to add, and the shared pair is only added once
    /// some adapter needs it. Reporting the other pool's figure under this
    /// one's label would be worse than reporting nothing, so the field goes
    /// unwritten and the baseline stands.
    #[test]
    fn a_missing_pool_reports_nothing_rather_than_the_other_one() {
        assert_eq!(select_adapter_usage(true, Some(134_217_728), None), None);
        assert_eq!(select_adapter_usage(false, None, Some(512)), None);
        assert_eq!(select_adapter_usage(true, None, None), None);
    }

    fn process_instance(pid: u32, luid: AdapterLuid) -> ids::GpuProcessMemoryInstance {
        ids::GpuProcessMemoryInstance { pid, luid, phys: 0 }
    }

    /// Each pid's figure comes from the pool its own adapter is measured
    /// against, so a host with both an iGPU and a discrete card reports both
    /// correctly in the same poll.
    #[test]
    fn process_rows_follow_their_adapter() {
        let igpu = AdapterLuid::new(0, 1);
        let discrete = AdapterLuid::new(0, 2);
        let shared_luids = std::collections::HashSet::from([igpu]);

        let rows = merge_process_rows(
            vec![
                (process_instance(100, igpu), 0),
                (process_instance(200, discrete), 4_294_967_296),
            ],
            vec![
                (process_instance(100, igpu), 1_073_741_824),
                // A shared row for a discrete adapter is noise here: that
                // card is already counted from its dedicated pool, and
                // taking both would double-count the pid.
                (process_instance(200, discrete), 65_536),
            ],
            &shared_luids,
        );

        let mut seen: Vec<(u32, u64)> = rows.iter().map(|r| (r.pid, r.used_bytes)).collect();
        seen.sort_unstable();
        assert_eq!(seen, vec![(100, 1_073_741_824), (200, 4_294_967_296)]);
    }

    /// A process that once touched the GPU keeps a zero-valued instance
    /// alive; listing it would fill the process view with empty rows.
    #[test]
    fn zero_byte_process_rows_are_dropped() {
        let luid = AdapterLuid::new(0, 1);
        let rows = merge_process_rows(
            vec![
                (process_instance(1, luid), 0),
                (process_instance(2, luid), 8),
            ],
            Vec::new(),
            &std::collections::HashSet::new(),
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].pid, 2);
    }

    /// With no shared adapter on the machine the shared families are never
    /// sampled, so the second vector is empty and must change nothing.
    #[test]
    fn a_discrete_only_host_is_unaffected() {
        let luid = AdapterLuid::new(0, 7);
        let rows = merge_process_rows(
            vec![(process_instance(42, luid), 1024)],
            Vec::new(),
            &std::collections::HashSet::new(),
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].used_bytes, 1024);
    }
}
