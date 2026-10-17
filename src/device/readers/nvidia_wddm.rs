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

//! Per-process GPU memory for NVIDIA GPUs under WDDM (issue #459).
//!
//! NVML cannot see per-process memory under the Windows display driver
//! model: every process row comes back as `UsedGpuMemory::Unavailable`
//! ("Not available in WDDM driver model" in `nvidia-smi -q`). Windows
//! itself tracks the figure in the PDH `\GPU Process Memory(*)` counters,
//! which the vendor-neutral [`windows_gpu_perf`] layer already samples for
//! the AMD and Intel readers. This module pairs each NVML device with its
//! DXGI adapter LUID and fills the rows NVML left empty from that sample.
//!
//! Like `windows_gpu_perf`, it is compiled under `test` on every platform
//! so the pairing and the fill reach the Linux test runner.

use std::collections::{HashMap, HashSet};

use crate::device::readers::windows_gpu_perf::ProcessGpuMemory;
use crate::device::readers::windows_gpu_perf::ids::{AdapterIdentity, AdapterLuid, match_adapter};
use crate::device::types::ProcessInfo;

/// What NVML says about a device's PCI identity, enough to find its DXGI
/// adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NvmlPciIdentity {
    pub uuid: String,
    pub name: String,
    /// NVML's `pciDeviceId`: device id in the high 16 bits, vendor id in
    /// the low 16 bits (`0x2E0310DE` for N1X).
    pub pci_device_id: u32,
}

/// Pair NVML devices with DXGI adapters, returning `uuid -> LUID`.
///
/// Reuses [`match_adapter`] by phrasing the NVML PCI ids as the
/// `PCI\VEN_xxxx&DEV_xxxx` string that function parses, so NVIDIA devices
/// are paired by exactly the rules the AMD and Intel readers use. Devices
/// without a PCI identity (the N1X NPU) are skipped.
///
/// `match_adapter` breaks a tie between identical cards with an ordinal
/// counted inside the group sharing one PCI vendor/device id, so the
/// ordinal passed here counts only earlier NVML devices with the same
/// `pci_device_id`, not the global NVML index.
pub(crate) fn map_devices_to_luids(
    devices: &[NvmlPciIdentity],
    adapters: &[AdapterIdentity],
) -> HashMap<String, AdapterLuid> {
    let mut seen_per_model: HashMap<u32, usize> = HashMap::new();
    devices
        .iter()
        .filter(|device| device.pci_device_id != 0)
        .filter_map(|device| {
            let seen = seen_per_model.entry(device.pci_device_id).or_insert(0);
            let model_ordinal = *seen;
            *seen += 1;
            let vendor = device.pci_device_id & 0xFFFF;
            let device_id = device.pci_device_id >> 16;
            let pnp = format!("PCI\\VEN_{vendor:04X}&DEV_{device_id:04X}");
            let adapter = match_adapter(adapters, Some(&pnp), &device.name, model_ordinal)?;
            Some((device.uuid.clone(), adapter.luid))
        })
        .collect()
}

/// Fill the memory of process rows NVML reported as unavailable.
///
/// Only rows listed in `unavailable` (keyed `(pid, device uuid)`) are
/// touched, so a figure NVML did report is never overwritten. A row whose
/// device has no LUID, or whose pid has no PDH sample on that adapter,
/// keeps its zero.
pub(crate) fn supplement_process_memory(
    rows: &mut [ProcessInfo],
    unavailable: &HashSet<(u32, String)>,
    luids: &HashMap<String, AdapterLuid>,
    samples: &[ProcessGpuMemory],
) {
    if unavailable.is_empty() {
        return;
    }
    let by_pid_luid: HashMap<(u32, AdapterLuid), u64> = samples
        .iter()
        .map(|sample| ((sample.pid, sample.luid), sample.used_bytes))
        .collect();
    for row in rows.iter_mut() {
        // A zero row is the only shape NVML leaves behind for an
        // unavailable reading; a non-zero one came from the other process
        // list (compute vs graphics) and is kept.
        if row.used_memory != 0 || !unavailable.contains(&(row.pid, row.device_uuid.clone())) {
            continue;
        }
        let Some(luid) = luids.get(&row.device_uuid) else {
            continue;
        };
        if let Some(bytes) = by_pid_luid.get(&(row.pid, *luid)) {
            row.used_memory = *bytes;
        }
    }
}

/// Fill unavailable NVML process memory from a fresh PDH sample.
///
/// Does nothing (and takes no sample) when NVML reported every row.
#[cfg(target_os = "windows")]
pub(crate) fn supplement_from_pdh(
    rows: &mut [ProcessInfo],
    unavailable: &HashSet<(u32, String)>,
    devices: &[NvmlPciIdentity],
) {
    use crate::device::readers::windows_gpu_perf;

    if unavailable.is_empty() {
        return;
    }
    let snapshot = windows_gpu_perf::snapshot();
    let luids = map_devices_to_luids(devices, &snapshot.identities());
    supplement_process_memory(rows, unavailable, &luids, &snapshot.processes);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::readers::windows_gpu_perf::gpu_process_row;

    const N1X_UUID: &str = "GPU-1e1f295a-4e4f-b9ef-ff91-2226a89c45aa";
    const N1X_LUID: AdapterLuid = AdapterLuid {
        high: 0,
        low: 0xEEB1,
    };

    fn n1x() -> NvmlPciIdentity {
        NvmlPciIdentity {
            uuid: N1X_UUID.to_string(),
            name: "NVIDIA RTX Spark N1X (6144-core Blackwell RTX GPU)".to_string(),
            pci_device_id: 0x2E03_10DE,
        }
    }

    fn adapters() -> Vec<AdapterIdentity> {
        vec![
            AdapterIdentity {
                luid: AdapterLuid {
                    high: 0,
                    low: 0x1234,
                },
                vendor_id: 0x1414,
                device_id: 0x008C,
                description: "Microsoft Basic Render Driver".to_string(),
            },
            AdapterIdentity {
                luid: N1X_LUID,
                vendor_id: 0x10DE,
                device_id: 0x2E03,
                description: "NVIDIA RTX Spark N1X".to_string(),
            },
        ]
    }

    #[test]
    fn nvml_pci_ids_pair_with_the_dxgi_adapter() {
        let luids = map_devices_to_luids(&[n1x()], &adapters());
        assert_eq!(luids.get(N1X_UUID), Some(&N1X_LUID));
    }

    fn nvidia_adapter(low: u32, device_id: u32, description: &str) -> AdapterIdentity {
        AdapterIdentity {
            luid: AdapterLuid { high: 0, low },
            vendor_id: 0x10DE,
            device_id,
            description: description.to_string(),
        }
    }

    fn nvml_device(uuid: &str, name: &str, device_id: u32) -> NvmlPciIdentity {
        NvmlPciIdentity {
            uuid: uuid.to_string(),
            name: name.to_string(),
            pci_device_id: (device_id << 16) | 0x10DE,
        }
    }

    #[test]
    fn identical_cards_pair_by_ordinal_within_their_model() {
        // NVML order: a different model first, then two identical cards.
        // With the global NVML index the second 4090 would get ordinal 2,
        // past the end of the two-entry 4090 group, and fall back onto the
        // first card's adapter.
        let devices = [
            nvml_device("GPU-a4000", "NVIDIA RTX A4000", 0x24B0),
            nvml_device("GPU-4090-0", "NVIDIA GeForce RTX 4090", 0x2684),
            nvml_device("GPU-4090-1", "NVIDIA GeForce RTX 4090", 0x2684),
        ];
        let adapters = [
            nvidia_adapter(0x4090, 0x2684, "NVIDIA GeForce RTX 4090"),
            nvidia_adapter(0xA400, 0x24B0, "NVIDIA RTX A4000"),
            nvidia_adapter(0x4091, 0x2684, "NVIDIA GeForce RTX 4090"),
        ];

        let luids = map_devices_to_luids(&devices, &adapters);

        assert_eq!(luids["GPU-a4000"].low, 0xA400);
        assert_eq!(luids["GPU-4090-0"].low, 0x4090);
        assert_eq!(luids["GPU-4090-1"].low, 0x4091);
    }

    #[test]
    fn devices_without_pci_identity_are_skipped() {
        let npu = NvmlPciIdentity {
            uuid: "DLA-28469e90".to_string(),
            name: "NVIDIA NPU".to_string(),
            pci_device_id: 0,
        };
        assert!(map_devices_to_luids(&[npu], &adapters()).is_empty());
    }

    #[test]
    fn unavailable_rows_are_filled_from_pdh() {
        let mut rows = vec![
            gpu_process_row(0, N1X_UUID, 100, 0),
            gpu_process_row(0, N1X_UUID, 200, 4096),
            gpu_process_row(0, N1X_UUID, 300, 0),
        ];
        let unavailable: HashSet<(u32, String)> =
            [(100, N1X_UUID.to_string()), (300, N1X_UUID.to_string())]
                .into_iter()
                .collect();
        let luids: HashMap<String, AdapterLuid> =
            [(N1X_UUID.to_string(), N1X_LUID)].into_iter().collect();
        let samples = vec![
            ProcessGpuMemory {
                pid: 100,
                luid: N1X_LUID,
                used_bytes: 512 * 1024 * 1024,
            },
            // Reported by NVML: must not be overwritten.
            ProcessGpuMemory {
                pid: 200,
                luid: N1X_LUID,
                used_bytes: 1,
            },
            // Same pid on another adapter: must not leak across.
            ProcessGpuMemory {
                pid: 300,
                luid: AdapterLuid {
                    high: 0,
                    low: 0x10EFA,
                },
                used_bytes: 7,
            },
        ];

        supplement_process_memory(&mut rows, &unavailable, &luids, &samples);

        assert_eq!(rows[0].used_memory, 512 * 1024 * 1024);
        assert_eq!(rows[1].used_memory, 4096);
        assert_eq!(rows[2].used_memory, 0);
    }
}
