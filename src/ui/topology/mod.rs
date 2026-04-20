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

//! Topology tab (issue #190).
//!
//! Renders the intra-node GPU interconnect as either an ASCII NUMA-grouped
//! graph (default) or a `nvidia-smi topo -m`-style matrix. See the issue
//! body for the motivation and visual mockups.
//!
//! Submodules:
//!
//! * [`classify_edge`] — derives NVn / SYS / PXB / NODE labels from
//!   `GpuInfo.nvlink_remote_devices` + `numa_node_id` + `detail` PCIe
//!   keys.
//! * [`layout`] — groups GPUs by NUMA zone and picks a horizontal /
//!   vertical stacking strategy based on terminal width.
//! * [`graph_render`] — renders the NUMA-boxed ASCII graph.
//! * [`matrix_render`] — renders the tabular matrix fallback.
//!
//! The orchestration entry point lives in
//! [`crate::ui::renderers::topology_renderer`].

use crate::device::{GpuInfo, NvLinkRemoteDevice};

pub mod classify_edge;
pub mod graph_render;
pub mod layout;
pub mod matrix_render;

/// Minimum terminal width (in cells) at which the graph renderer remains
/// legible. Below this threshold the orchestrator falls back to matrix
/// mode even when the operator has selected graph mode.
///
/// Chosen so two 4-GPU NUMA boxes fit side-by-side at roughly 50 cells
/// each; dropping below 100 columns means either the graph would overflow
/// or the NUMA labels would collide.
pub const GRAPH_MIN_WIDTH: u16 = 100;

/// Render mode selected by the in-tab `M` toggle.
///
/// Default is `Graph`. When the terminal is narrower than
/// [`GRAPH_MIN_WIDTH`], the renderer falls back to `Matrix` regardless of
/// this selection so the content never overflows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TopologyViewMode {
    #[default]
    Graph,
    Matrix,
}

impl TopologyViewMode {
    /// Flip between graph and matrix. Invoked from the event handler on
    /// `M`. Caller is responsible for bumping `mark_data_changed`.
    pub fn toggled(self) -> Self {
        match self {
            Self::Graph => Self::Matrix,
            Self::Matrix => Self::Graph,
        }
    }

    pub fn as_label(self) -> &'static str {
        match self {
            Self::Graph => "graph",
            Self::Matrix => "matrix",
        }
    }
}

/// GPU rolled into the shape the topology renderers work with.
#[derive(Debug, Clone)]
pub struct TopologyGpu {
    /// Ordinal index inside the host, derived from the `detail["index"]`
    /// metric label when available and otherwise the positional index.
    /// Drives the column order in the matrix and the label in the graph.
    pub index: u32,
    /// NVML UUID. Used for the graph label tooltip and dedup.
    pub uuid: String,
    /// Product name (e.g. `NVIDIA H100 80GB HBM3`). Shown in the header.
    pub name: String,
    /// NUMA zone the GPU sits in, or `None` on non-Linux hosts / drivers
    /// that do not report NUMA. `None` lands in the synthetic "NUMA ?"
    /// zone so layout still has a place to put the card.
    pub numa_node: Option<i32>,
    /// Active NvLinks as reported by the parent GPU.
    pub links: Vec<NvLinkRemoteDevice>,
    /// PCIe generation/width best-effort. Empty string when absent.
    /// Reserved for a future iteration of the graph header that
    /// surfaces PCIe speed next to each GPU label; kept in the model
    /// today so the reader-side contract is stable.
    #[allow(dead_code)]
    pub pcie_display: String,
}

/// Topology snapshot for a single host.
///
/// Built once per frame from the GPU slice for the currently-selected
/// host. Both the graph and matrix renderers consume the same model so
/// the two views stay consistent.
#[derive(Debug, Clone, Default)]
pub struct TopologyModel {
    /// Hostname / host_id for the header line.
    pub host_label: String,
    /// Indexed list of GPUs ordered by [`TopologyGpu::index`].
    pub gpus: Vec<TopologyGpu>,
    /// Has-any-NvLink shortcut. When false the graph drops NvLink edges
    /// and shows only NUMA + PCIe annotations.
    pub has_nvlink: bool,
    /// Has-any-NvSwitch shortcut. Drives whether the graph draws the
    /// switch-mesh overlay.
    pub has_nvswitch: bool,
    /// NvSwitch node count derived from the union of switch-typed remote
    /// endpoints. Used only by the graph legend.
    pub switch_count: u32,
    /// Total active link count across all GPUs.
    pub active_link_count: u32,
    /// Whether the host belongs to the NVIDIA device family. Non-NVIDIA
    /// paths omit the NVn/SYS legend entries.
    pub is_nvidia: bool,
}

impl TopologyModel {
    /// Build a topology model from the GPUs on a host.
    ///
    /// `host_label` is rendered verbatim in the header; pass the
    /// hostname when known, otherwise the host_id.
    pub fn from_host(host_label: impl Into<String>, gpus: &[GpuInfo]) -> Self {
        let mut out = Self {
            host_label: host_label.into(),
            ..Self::default()
        };
        if gpus.is_empty() {
            return out;
        }

        out.is_nvidia = gpus
            .iter()
            .any(|g| g.device_type == "GPU" && looks_nvidia(g));

        let mut entries: Vec<TopologyGpu> = gpus
            .iter()
            .enumerate()
            .map(|(positional, gpu)| TopologyGpu {
                index: gpu
                    .detail
                    .get("index")
                    .and_then(|s| s.parse::<u32>().ok())
                    .unwrap_or(positional as u32),
                uuid: gpu.uuid.clone(),
                name: gpu.name.clone(),
                numa_node: gpu.numa_node_id,
                links: gpu.nvlink_remote_devices.clone(),
                pcie_display: format_pcie(gpu),
            })
            .collect();
        entries.sort_by_key(|g| g.index);

        out.active_link_count = entries.iter().map(|g| g.links.len() as u32).sum();
        out.has_nvlink = out.active_link_count > 0;

        let switch_total: u32 = entries
            .iter()
            .map(|g| {
                g.links
                    .iter()
                    .filter(|d| matches!(d.remote_type, crate::device::NvLinkRemoteType::Switch))
                    .count() as u32
            })
            .sum();
        out.switch_count = switch_total;
        out.has_nvswitch = switch_total > 0;
        out.gpus = entries;
        out
    }

    /// Number of GPUs in the model (convenience accessor for renderers).
    pub fn gpu_count(&self) -> u32 {
        self.gpus.len() as u32
    }

    /// Ordered distinct NUMA nodes present in the snapshot, with `None`
    /// collapsed to a single synthetic bucket at the end. Used by the
    /// layout to decide column stacking.
    pub fn numa_nodes(&self) -> Vec<Option<i32>> {
        let mut seen: Vec<Option<i32>> = Vec::new();
        for gpu in &self.gpus {
            if !seen.contains(&gpu.numa_node) {
                seen.push(gpu.numa_node);
            }
        }
        // Keep `None` at the end so the graph draws the "unknown NUMA"
        // box last.
        seen.sort_by(|a, b| match (a, b) {
            (Some(x), Some(y)) => x.cmp(y),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        });
        seen
    }

    /// Short summary emitted under the graph / matrix (e.g. "8 GPUs · 2
    /// NUMA · 56 NvLinks"). Pure utility for the orchestrator renderer.
    pub fn summary(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        let gpu_count = self.gpu_count();
        parts.push(format!(
            "{gpu_count} GPU{}",
            if gpu_count == 1 { "" } else { "s" }
        ));
        let numa_count = self.numa_nodes().len();
        if numa_count > 0 {
            parts.push(format!(
                "{numa_count} NUMA{}",
                if numa_count == 1 { "" } else { "s" }
            ));
        }
        if self.active_link_count > 0 {
            parts.push(format!("{} NvLinks", self.active_link_count));
        }
        if self.switch_count > 0 {
            parts.push(format!("{} NvSwitch", self.switch_count));
        }
        parts.join(" · ")
    }
}

/// Heuristic for "is this device NVIDIA?" — required because `GpuInfo`
/// does not carry a vendor enum and non-NVIDIA readers set the name to
/// various things ("Apple M-series", "AMD Radeon …"). NVIDIA names
/// always start with `NVIDIA`, `GeForce`, or `Tesla`.
fn looks_nvidia(gpu: &GpuInfo) -> bool {
    let n = &gpu.name;
    n.starts_with("NVIDIA")
        || n.starts_with("GeForce")
        || n.starts_with("Tesla")
        || n.starts_with("Quadro")
}

/// Format PCIe display from the detail map. Falls back to the empty
/// string when the reader did not populate any of the keys.
fn format_pcie(gpu: &GpuInfo) -> String {
    let gen_str = gpu
        .detail
        .get("PCIe Generation")
        .or_else(|| gpu.detail.get("pcie_gen_current"));
    let width = gpu
        .detail
        .get("PCIe Link Width")
        .or_else(|| gpu.detail.get("pcie_width_current"));
    match (gen_str, width) {
        (Some(g), Some(w)) => format!("Gen{g} x{w}"),
        (Some(g), None) => format!("Gen{g}"),
        (None, Some(w)) => format!("x{w}"),
        (None, None) => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::{GpuInfo, NvLinkRemoteDevice, NvLinkRemoteType};
    use std::collections::HashMap;

    fn mk_gpu(index: u32, numa: Option<i32>, links: Vec<NvLinkRemoteDevice>) -> GpuInfo {
        let mut detail = HashMap::new();
        detail.insert("index".to_string(), index.to_string());
        GpuInfo {
            uuid: format!("GPU-{index}"),
            time: String::new(),
            name: "NVIDIA H100 80GB HBM3".to_string(),
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
            nvlink_remote_devices: links,
            gpm_metrics: None,
            detail,
        }
    }

    #[test]
    fn empty_model_is_empty() {
        let model = TopologyModel::from_host("h", &[]);
        assert!(model.gpus.is_empty());
        assert!(!model.has_nvlink);
        assert_eq!(model.gpu_count(), 0);
    }

    #[test]
    fn model_tallies_links_and_switches() {
        let g0 = mk_gpu(
            0,
            Some(0),
            vec![
                NvLinkRemoteDevice {
                    link_index: 0,
                    remote_type: NvLinkRemoteType::Gpu,
                    bandwidth_mb_s: None,
                },
                NvLinkRemoteDevice {
                    link_index: 1,
                    remote_type: NvLinkRemoteType::Switch,
                    bandwidth_mb_s: None,
                },
            ],
        );
        let g1 = mk_gpu(
            1,
            Some(1),
            vec![NvLinkRemoteDevice {
                link_index: 0,
                remote_type: NvLinkRemoteType::Gpu,
                bandwidth_mb_s: None,
            }],
        );
        let model = TopologyModel::from_host("h", &[g0, g1]);
        assert_eq!(model.gpu_count(), 2);
        assert_eq!(model.active_link_count, 3);
        assert_eq!(model.switch_count, 1);
        assert!(model.has_nvlink);
        assert!(model.has_nvswitch);
        assert!(model.is_nvidia);
        assert_eq!(model.numa_nodes(), vec![Some(0), Some(1)]);
    }

    #[test]
    fn gpus_are_sorted_by_index_label() {
        // Out-of-order insertion: label index 2 first, then 0, then 1.
        let g2 = mk_gpu(2, Some(0), vec![]);
        let g0 = mk_gpu(0, Some(0), vec![]);
        let g1 = mk_gpu(1, Some(0), vec![]);
        let model = TopologyModel::from_host("h", &[g2, g0, g1]);
        assert_eq!(
            model.gpus.iter().map(|g| g.index).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
    }

    #[test]
    fn missing_numa_collapses_to_single_bucket() {
        let g0 = mk_gpu(0, None, vec![]);
        let g1 = mk_gpu(1, None, vec![]);
        let model = TopologyModel::from_host("h", &[g0, g1]);
        assert_eq!(model.numa_nodes(), vec![None]);
    }

    #[test]
    fn non_nvidia_is_not_flagged() {
        let mut gpu = mk_gpu(0, Some(0), vec![]);
        gpu.name = "Apple M-series".to_string();
        let model = TopologyModel::from_host("h", &[gpu]);
        assert!(!model.is_nvidia);
    }

    #[test]
    fn pcie_formatting_prefers_capitalised_detail_keys() {
        let mut gpu = mk_gpu(0, Some(0), vec![]);
        gpu.detail
            .insert("PCIe Generation".to_string(), "5".to_string());
        gpu.detail
            .insert("PCIe Link Width".to_string(), "16".to_string());
        let model = TopologyModel::from_host("h", &[gpu]);
        assert_eq!(model.gpus[0].pcie_display, "Gen5 x16");
    }

    #[test]
    fn summary_reports_gpu_numa_and_link_counts() {
        let g0 = mk_gpu(
            0,
            Some(0),
            vec![NvLinkRemoteDevice {
                link_index: 0,
                remote_type: NvLinkRemoteType::Gpu,
                bandwidth_mb_s: None,
            }],
        );
        let g1 = mk_gpu(
            1,
            Some(1),
            vec![NvLinkRemoteDevice {
                link_index: 0,
                remote_type: NvLinkRemoteType::Switch,
                bandwidth_mb_s: None,
            }],
        );
        let model = TopologyModel::from_host("h", &[g0, g1]);
        let s = model.summary();
        assert!(s.contains("2 GPUs"), "{s}");
        assert!(s.contains("2 NUMAs"), "{s}");
        assert!(s.contains("2 NvLinks"), "{s}");
        assert!(s.contains("NvSwitch"), "{s}");
    }

    #[test]
    fn toggling_view_mode_round_trips() {
        let mode = TopologyViewMode::default();
        assert_eq!(mode, TopologyViewMode::Graph);
        assert_eq!(mode.toggled(), TopologyViewMode::Matrix);
        assert_eq!(mode.toggled().toggled(), TopologyViewMode::Graph);
    }
}
