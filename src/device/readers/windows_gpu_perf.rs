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
    /// True dedicated VRAM size in bytes, from DXGI.
    pub total_memory: Option<u64>,
    /// System-wide dedicated VRAM in use, in bytes, from the PDH
    /// `GPU Adapter Memory` counter.
    pub used_memory: Option<u64>,
    /// Device utilization, 0..=100, from the PDH `GPU Engine` counters.
    pub utilization: Option<f64>,
    /// Process-scoped DXGI budget, in bytes. Diagnostics only.
    pub process_budget: Option<u64>,
    /// Process-scoped DXGI current usage, in bytes. Diagnostics only.
    pub process_current_usage: Option<u64>,
}

/// Per-process dedicated GPU memory, keyed by adapter.
#[derive(Clone, Debug)]
pub struct ProcessGpuMemory {
    pub pid: u32,
    pub luid: AdapterLuid,
    pub dedicated_bytes: u64,
}

/// One poll's worth of vendor-neutral GPU data.
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub adapters: Vec<AdapterMetrics>,
    pub processes: Vec<ProcessGpuMemory>,
}

impl Snapshot {
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

/// Take a fresh sample and cache it for [`latest`].
///
/// Cheap to call once per poll; do not call it twice in one poll, as
/// each call consumes one PDH collection and the utilization rate is
/// computed between consecutive collections.
#[cfg(target_os = "windows")]
pub fn snapshot() -> Snapshot {
    let dxgi_adapters = dxgi::enumerate();
    let sample = pdh::sample();

    let adapters = dxgi_adapters
        .into_iter()
        .map(|adapter| {
            let luid = adapter.identity.luid;
            AdapterMetrics {
                identity: adapter.identity,
                total_memory: (adapter.dedicated_video_memory > 0)
                    .then_some(adapter.dedicated_video_memory),
                used_memory: sample.adapter_memory.get(&luid).copied(),
                utilization: sample.utilization.get(&luid).copied(),
                process_budget: adapter.process_budget,
                process_current_usage: adapter.process_current_usage,
            }
        })
        .collect();

    let processes = sample
        .process_memory
        .into_iter()
        .filter(|(_, bytes)| *bytes > 0)
        .map(|(instance, bytes)| ProcessGpuMemory {
            pid: instance.pid,
            luid: instance.luid,
            dedicated_bytes: bytes,
        })
        .collect();

    let snapshot = Snapshot {
        adapters,
        processes,
    };
    cache_snapshot(&snapshot);
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
static LAST_SNAPSHOT: once_cell::sync::OnceCell<std::sync::Mutex<Snapshot>> =
    once_cell::sync::OnceCell::new();

#[cfg(target_os = "windows")]
fn cache_snapshot(snapshot: &Snapshot) {
    let cell = LAST_SNAPSHOT.get_or_init(|| std::sync::Mutex::new(Snapshot::default()));
    let mut guard = match cell.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    *guard = snapshot.clone();
}

/// The most recent [`snapshot`], without consuming a PDH collection.
///
/// `get_process_info` uses this so that a poll which already called
/// `snapshot` from `get_gpu_info` does not disturb the utilization rate
/// by collecting twice.
#[cfg(target_os = "windows")]
pub fn latest() -> Snapshot {
    let Some(cell) = LAST_SNAPSHOT.get() else {
        return Snapshot::default();
    };
    match cell.lock() {
        Ok(guard) => guard.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    }
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

/// Record that `source` contributed to this GPU's metrics.
///
/// The Intel reader established `Metrics Source` as a human-readable
/// composition ("WMI", then "WMI + Level Zero" once Level Zero produced
/// a readout). This keeps that shape while letting several backends
/// append, and is idempotent so repeated polls do not grow the string.
pub fn note_metrics_source(detail: &mut HashMap<String, String>, source: &str) {
    let entry = detail.entry("Metrics Source".to_string()).or_default();
    if entry.is_empty() {
        *entry = source.to_string();
        return;
    }
    if entry.split(" + ").any(|part| part == source) {
        return;
    }
    entry.push_str(" + ");
    entry.push_str(source);
}

/// Layer this adapter's metrics onto a WMI-derived [`GpuInfo`].
///
/// Only fields that carry real data are written, so a partially
/// available adapter (DXGI present, PDH counters absent, which is the
/// shape of a GitHub-hosted Windows runner) upgrades VRAM and leaves
/// utilization at the baseline rather than zeroing anything that was
/// already known.
pub fn apply_to_gpu_info(gpu: &mut GpuInfo, metrics: &AdapterMetrics) {
    let mut touched_dxgi = false;
    let mut touched_pdh = false;

    if let Some(total) = metrics.total_memory {
        gpu.total_memory = total;
        gpu.detail
            .insert("Source: Memory".to_string(), "DXGI".to_string());
        touched_dxgi = true;
    }

    // Both DXGI video-memory figures are scoped to the calling process.
    // They are labelled as such and kept out of `used_memory`, which
    // must stay system-wide; reading either as a device-level number
    // would understate a busy GPU by whatever other processes hold.
    if let Some(budget) = metrics.process_budget {
        gpu.detail
            .insert("VRAM Budget (this process)".to_string(), budget.to_string());
        touched_dxgi = true;
    }
    if let Some(usage) = metrics.process_current_usage {
        gpu.detail
            .insert("VRAM Usage (this process)".to_string(), usage.to_string());
        touched_dxgi = true;
    }

    if let Some(used) = metrics.used_memory {
        gpu.used_memory = used;
        gpu.detail
            .insert("Source: Memory Used".to_string(), "PDH".to_string());
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

/// Build per-process GPU memory rows from the most recent snapshot.
///
/// Uses [`latest`] rather than sampling again: a second collection in
/// the same poll would halve the interval the utilization rate is
/// computed over.
pub fn process_rows(adapter_index: &AdapterIndex) -> Vec<ProcessInfo> {
    process_rows_from(&latest(), adapter_index)
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
                process.dedicated_bytes,
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
    use crate::device::types::GpuInfo;

    fn blank_gpu() -> GpuInfo {
        let mut detail = HashMap::new();
        detail.insert("Metrics Source".to_string(), "WMI".to_string());
        detail.insert("Source: Utilization".to_string(), "unavailable".to_string());
        detail.insert("Source: Memory".to_string(), "WMI".to_string());
        GpuInfo {
            uuid: "PCI\\VEN_1002&DEV_744C".to_string(),
            time: String::new(),
            name: "AMD Radeon RX 7900 XTX".to_string(),
            device_type: "GPU".to_string(),
            host_id: String::new(),
            hostname: String::new(),
            instance: String::new(),
            utilization: 0.0,
            ane_utilization: 0.0,
            dla_utilization: None,
            tensorcore_utilization: None,
            temperature: 0,
            used_memory: 0,
            total_memory: 4_294_967_295,
            frequency: 0,
            power_consumption: 0.0,
            gpu_core_count: None,
            temperature_threshold_slowdown: None,
            temperature_threshold_shutdown: None,
            temperature_threshold_max_operating: None,
            temperature_threshold_acoustic: None,
            performance_state: None,
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
            used_memory: used,
            utilization,
            process_budget: None,
            process_current_usage: None,
        }
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
        gpu.utilization = 0.0;
        apply_to_gpu_info(&mut gpu, &metrics(Some(1_073_741_824), None, None));
        assert_eq!(gpu.total_memory, 1_073_741_824);
        assert_eq!(gpu.utilization, 0.0);
        assert_eq!(gpu.detail["Source: Utilization"], "unavailable");
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
                    dedicated_bytes: 536_870_912,
                },
                // A card this reader never paired, for example an NVIDIA
                // GPU sitting alongside the AMD one. Its processes must
                // be dropped, not attributed to the wrong device.
                ProcessGpuMemory {
                    pid: 99,
                    luid: AdapterLuid::new(0, 0xFFFF),
                    dedicated_bytes: 1,
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
        assert_eq!(gpu.detail["VRAM Budget (this process)"], "7000000000");
        assert_eq!(gpu.detail["VRAM Usage (this process)"], "123456");
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
        let _ = process_rows(&index);
        let _ = pdh_query_available();
        let _ = latest();

        #[cfg(not(target_os = "windows"))]
        {
            assert!(snapshot().is_empty());
            assert!(latest().is_empty());
            assert!(!pdh_query_available());
            assert!(index.is_empty());
            assert!(process_rows(&index).is_empty());
            // An inert layer must leave the WMI baseline exactly as it
            // found it.
            assert_eq!(gpus[0].total_memory, 4_294_967_295);
            assert_eq!(gpus[0].detail["Metrics Source"], "WMI");
        }
    }

    #[test]
    fn snapshot_helpers_behave_on_an_empty_snapshot() {
        let snapshot = Snapshot::default();
        assert!(snapshot.is_empty());
        assert!(snapshot.identities().is_empty());
        assert!(snapshot.adapter(AdapterLuid::new(0, 1)).is_none());
    }
}
