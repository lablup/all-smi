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

//! Edge classification for the topology tab (issue #190).
//!
//! `nvidia-smi topo -m` renders the relationship between every pair of GPUs
//! as a short label (`NV4`, `NV8`, `SYS`, `PXB`, `NODE`, `X`). The topology
//! view reproduces that vocabulary for the matrix mode and re-uses the same
//! classifier for graph-mode edge labels.
//!
//! The `NVn` family where `n` is the bandwidth hint is the trickiest part.
//! NVML can report per-link bandwidth on a narrow subset of boards; when
//! the hint is missing we fall back to a generic `"NV"` so we never
//! misreport a generation.

use crate::device::{GpuInfo, NvLinkRemoteDevice, NvLinkRemoteType};

/// Short topology label for a pair of endpoints, mirroring the
/// `nvidia-smi topo -m` legend plus one ad-hoc variant (`NSW`) for
/// NvSwitch mesh members.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeClass {
    /// Self-cell along the matrix diagonal.
    SelfCell,
    /// Direct NvLink connection. `count` is the number of active parallel
    /// links between the endpoints; `generation` is the `NVn` bandwidth
    /// hint (None when unknown).
    NvLink { count: u32, generation: Option<u8> },
    /// Remote is an NvSwitch (GPU → Switch on this row, or Switch → GPU).
    ///
    /// Reserved for a future classifier extension that materialises the
    /// switch node as a first-class edge target; the current matrix
    /// renderer still treats switch-mediated pairs as `NvSwitchMesh`
    /// because NVML does not expose the remote GPU UUID.
    #[allow(dead_code)]
    NvSwitch { count: u32 },
    /// Two GPUs that share an NvSwitch mesh but do not have a direct link.
    /// nvidia-smi labels this `NV#` too; we keep a distinct variant so the
    /// graph renderer can draw a dashed switch-mediated edge.
    NvSwitchMesh,
    /// Same NUMA node, same PCIe root complex (nvidia-smi `PXB`).
    ///
    /// Reserved for a future classifier extension that reads PCIe root
    /// complex info from `detail` and distinguishes PXB from NODE.
    #[allow(dead_code)]
    PcieSameRoot,
    /// Same NUMA node but different PCIe root complexes (nvidia-smi `NODE`).
    PcieSameNuma,
    /// Different NUMA nodes — traversal goes through system fabric
    /// (nvidia-smi `SYS`).
    SysInterconnect,
    /// No information available for this pair (non-NVIDIA path or
    /// partial data). Rendered as a dim `--`.
    Unknown,
}

impl EdgeClass {
    /// Canonical matrix-cell label. `NVn` is collapsed to `NV` when the
    /// generation hint is missing so the operator is never misled by a
    /// hallucinated generation.
    pub fn label(&self) -> String {
        match self {
            Self::SelfCell => "X".to_string(),
            Self::NvLink {
                count: _,
                generation: Some(g),
            } => format!("NV{g}"),
            Self::NvLink {
                count: _,
                generation: None,
            } => "NV".to_string(),
            Self::NvSwitch { .. } => "NSW".to_string(),
            Self::NvSwitchMesh => "NV".to_string(),
            Self::PcieSameRoot => "PXB".to_string(),
            Self::PcieSameNuma => "NODE".to_string(),
            Self::SysInterconnect => "SYS".to_string(),
            Self::Unknown => "--".to_string(),
        }
    }
}

/// Bandwidth-to-generation hint table. The boundaries follow NVIDIA's
/// public NvLink generation ceilings per link direction (rounded down to
/// 2 decimals):
///
/// | Gen | Per-link BW (GB/s) | Approx MB/s   |
/// |-----|--------------------|---------------|
/// | 1   | 20                 |  20_000       |
/// | 2   | 25                 |  25_000       |
/// | 3   | 25                 |  25_000       |
/// | 4   | 25                 |  25_000       |
/// | 5   | 50                 |  50_000       |
/// | 6   | 100                | 100_000       |
///
/// We use widened floors so the classifier is forgiving of slight
/// under-reporting (e.g. a 24 GB/s sample still resolves to Gen-3 territory
/// rather than degrading to Gen-1).
pub fn bandwidth_to_generation(bandwidth_mb_s: u32) -> Option<u8> {
    match bandwidth_mb_s {
        0 => None,
        1..=22_000 => Some(1),
        22_001..=40_000 => Some(4),
        40_001..=80_000 => Some(5),
        _ => Some(6),
    }
}

/// Is this endpoint a GPU (not a switch / ibmnpu / unknown)?
fn remote_is_gpu(d: &NvLinkRemoteDevice) -> bool {
    matches!(d.remote_type, NvLinkRemoteType::Gpu)
}

/// Is this endpoint an NvSwitch?
fn remote_is_switch(d: &NvLinkRemoteDevice) -> bool {
    matches!(d.remote_type, NvLinkRemoteType::Switch)
}

/// Derive the edge classification between two GPU rows.
///
/// `a` and `b` are the row / column GPUs respectively; `a.nvlink_remote_devices`
/// is the authoritative source of active links. We cannot tell which
/// **remote GPU** a specific link connects to (NVML does not expose remote
/// UUIDs in this reader), so the best heuristic for direct links is:
///
/// * If `a` has `k` active NvLink links of type `Gpu` and `k >= GPU_COUNT-1`,
///   the mesh is complete and every other GPU row gets an NvLink class.
/// * Otherwise we fall back to PCIe / NUMA classification.
///
/// This heuristic is intentionally optimistic: on DGX-like topologies with
/// full mesh + switch overlay it matches reality; on partial topologies the
/// matrix may show an `NVn` label for a pair that is actually switch-only,
/// which is the same ambiguity `nvidia-smi topo -m` exhibits.
pub fn classify(
    a_index: u32,
    b_index: u32,
    a: &GpuInfo,
    b: &GpuInfo,
    total_gpu_count: u32,
) -> EdgeClass {
    if a_index == b_index {
        return EdgeClass::SelfCell;
    }

    // Counts of each remote kind on the row GPU. `a` is the authoritative
    // source because `nvlink_remote_devices` is populated per parent GPU.
    let gpu_remote_count = a
        .nvlink_remote_devices
        .iter()
        .filter(|d| remote_is_gpu(d))
        .count() as u32;
    let switch_remote_count = a
        .nvlink_remote_devices
        .iter()
        .filter(|d| remote_is_switch(d))
        .count() as u32;

    // If the row has no NvLink info, classify strictly by PCIe / NUMA so
    // non-NVIDIA hosts still get a readable matrix.
    if gpu_remote_count == 0 && switch_remote_count == 0 {
        return pcie_class(a, b);
    }

    // Direct-link heuristic: when the row GPU has enough GPU-type links
    // to reach every peer, the pair is likely a direct NvLink.
    let peer_count = total_gpu_count.saturating_sub(1);
    if peer_count > 0 && gpu_remote_count >= peer_count {
        let generation = dominant_generation(&a.nvlink_remote_devices);
        return EdgeClass::NvLink {
            count: (gpu_remote_count / peer_count.max(1)).max(1),
            generation,
        };
    }

    // Switch-mediated heuristic: either row has switch links but no direct
    // GPU link to this peer, so classify the pair as switch-mesh.
    if switch_remote_count > 0 {
        return EdgeClass::NvSwitchMesh;
    }

    // Partial NvLink mesh: classify as generic NvLink with the count
    // divided among peers. Slightly optimistic but closer to what the
    // operator expects.
    if gpu_remote_count > 0 {
        let generation = dominant_generation(&a.nvlink_remote_devices);
        let count = gpu_remote_count
            .checked_div(peer_count.max(1))
            .unwrap_or(0)
            .max(1);
        return EdgeClass::NvLink { count, generation };
    }

    pcie_class(a, b)
}

/// Classify edge based purely on NUMA / PCIe info. Used as a fallback when
/// NvLink data is absent and for non-NVIDIA paths.
fn pcie_class(a: &GpuInfo, b: &GpuInfo) -> EdgeClass {
    match (a.numa_node_id, b.numa_node_id) {
        (Some(na), Some(nb)) if na == nb => EdgeClass::PcieSameNuma,
        (Some(_), Some(_)) => EdgeClass::SysInterconnect,
        _ => EdgeClass::Unknown,
    }
}

/// Pick the most common bandwidth hint among active links. When a single
/// GPU has links of multiple generations we prefer the majority; ties fall
/// back to `None` (ambiguous, render as generic `NV`).
fn dominant_generation(links: &[NvLinkRemoteDevice]) -> Option<u8> {
    // Tally generations across all links that report a bandwidth.
    let mut counts = [0u32; 7]; // index = generation (1..=6)
    let mut any = false;
    for link in links {
        if let Some(bw) = link.bandwidth_mb_s
            && let Some(generation) = bandwidth_to_generation(bw)
            && (generation as usize) < counts.len()
        {
            counts[generation as usize] += 1;
            any = true;
        }
    }
    if !any {
        return None;
    }
    let (mut best_gen, mut best_count, mut tied) = (0u8, 0u32, false);
    for (generation, &cnt) in counts.iter().enumerate().skip(1) {
        if cnt > best_count {
            best_gen = generation as u8;
            best_count = cnt;
            tied = false;
        } else if cnt == best_count && cnt > 0 {
            tied = true;
        }
    }
    if tied { None } else { Some(best_gen) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::NvLinkRemoteType;
    use std::collections::HashMap;

    fn gpu_with_numa(numa: Option<i32>) -> GpuInfo {
        GpuInfo {
            uuid: "GPU-X".to_string(),
            time: String::new(),
            name: "Test".to_string(),
            device_type: "GPU".to_string(),
            host_id: "h".to_string(),
            hostname: "h".to_string(),
            instance: "h".to_string(),
            utilization: 0.0,
            ane_utilization: 0.0,
            dla_utilization: None,
            tensorcore_utilization: None,
            temperature: 0,
            used_memory: 0,
            total_memory: 0,
            frequency: 0,
            power_consumption: 0.0,
            gpu_core_count: None,
            temperature_threshold_slowdown: None,
            temperature_threshold_shutdown: None,
            temperature_threshold_max_operating: None,
            temperature_threshold_acoustic: None,
            performance_state: None,
            numa_node_id: numa,
            gsp_firmware_mode: None,
            gsp_firmware_version: None,
            nvlink_remote_devices: Vec::new(),
            gpm_metrics: None,
            detail: HashMap::new(),
        }
    }

    fn link(remote: NvLinkRemoteType, bw: Option<u32>) -> NvLinkRemoteDevice {
        NvLinkRemoteDevice {
            link_index: 0,
            remote_type: remote,
            bandwidth_mb_s: bw,
        }
    }

    #[test]
    fn self_cell_is_identity() {
        let a = gpu_with_numa(Some(0));
        assert_eq!(classify(0, 0, &a, &a, 8), EdgeClass::SelfCell);
    }

    #[test]
    fn no_nvlink_same_numa_is_node() {
        let a = gpu_with_numa(Some(0));
        let b = gpu_with_numa(Some(0));
        assert_eq!(classify(0, 1, &a, &b, 8), EdgeClass::PcieSameNuma);
    }

    #[test]
    fn no_nvlink_cross_numa_is_sys() {
        let a = gpu_with_numa(Some(0));
        let b = gpu_with_numa(Some(1));
        assert_eq!(classify(0, 1, &a, &b, 8), EdgeClass::SysInterconnect);
    }

    #[test]
    fn missing_numa_is_unknown() {
        let a = gpu_with_numa(None);
        let b = gpu_with_numa(None);
        assert_eq!(classify(0, 1, &a, &b, 8), EdgeClass::Unknown);
    }

    #[test]
    fn full_mesh_gpu_links_classify_as_nvlink() {
        let mut a = gpu_with_numa(Some(0));
        // 7 GPU links: full mesh across 8 GPUs.
        a.nvlink_remote_devices = (0..7)
            .map(|i| NvLinkRemoteDevice {
                link_index: i,
                remote_type: NvLinkRemoteType::Gpu,
                bandwidth_mb_s: Some(50_000),
            })
            .collect();
        let b = gpu_with_numa(Some(0));
        let edge = classify(0, 1, &a, &b, 8);
        match edge {
            EdgeClass::NvLink { generation, .. } => assert_eq!(generation, Some(5)),
            other => panic!("expected NvLink, got {other:?}"),
        }
    }

    #[test]
    fn switch_remotes_without_full_gpu_mesh_classify_as_mesh() {
        let mut a = gpu_with_numa(Some(0));
        // 5 GPU links + 1 switch link — sub-mesh + switch overlay.
        a.nvlink_remote_devices = (0..5)
            .map(|_| link(NvLinkRemoteType::Gpu, Some(50_000)))
            .enumerate()
            .map(|(i, mut d)| {
                d.link_index = i as u32;
                d
            })
            .collect();
        a.nvlink_remote_devices.push(NvLinkRemoteDevice {
            link_index: 5,
            remote_type: NvLinkRemoteType::Switch,
            bandwidth_mb_s: None,
        });
        // With 8 total GPUs, peer_count=7 and GPU remotes=5 < 7, so we
        // should fall to the switch-mesh branch rather than mis-classify.
        let b = gpu_with_numa(Some(0));
        assert_eq!(classify(0, 1, &a, &b, 8), EdgeClass::NvSwitchMesh);
    }

    #[test]
    fn bandwidth_to_generation_thresholds() {
        assert_eq!(bandwidth_to_generation(0), None);
        assert_eq!(bandwidth_to_generation(20_000), Some(1));
        assert_eq!(bandwidth_to_generation(25_000), Some(4));
        assert_eq!(bandwidth_to_generation(50_000), Some(5));
        assert_eq!(bandwidth_to_generation(90_000), Some(6));
        assert_eq!(bandwidth_to_generation(150_000), Some(6));
    }

    #[test]
    fn edge_label_falls_back_to_nv_when_generation_unknown() {
        assert_eq!(
            EdgeClass::NvLink {
                count: 4,
                generation: None,
            }
            .label(),
            "NV"
        );
        assert_eq!(
            EdgeClass::NvLink {
                count: 4,
                generation: Some(5),
            }
            .label(),
            "NV5"
        );
    }

    #[test]
    fn dominant_generation_requires_populated_hints() {
        let links = vec![
            link(NvLinkRemoteType::Gpu, None),
            link(NvLinkRemoteType::Gpu, None),
        ];
        assert_eq!(dominant_generation(&links), None);
    }

    #[test]
    fn dominant_generation_picks_majority() {
        let links = vec![
            link(NvLinkRemoteType::Gpu, Some(50_000)),
            link(NvLinkRemoteType::Gpu, Some(50_000)),
            link(NvLinkRemoteType::Gpu, Some(25_000)),
        ];
        assert_eq!(dominant_generation(&links), Some(5));
    }

    #[test]
    fn dominant_generation_ties_resolve_to_none() {
        let links = vec![
            link(NvLinkRemoteType::Gpu, Some(50_000)),
            link(NvLinkRemoteType::Gpu, Some(25_000)),
        ];
        assert_eq!(dominant_generation(&links), None);
    }
}
