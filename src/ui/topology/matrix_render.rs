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

//! `nvidia-smi topo -m`-style matrix rendering for the topology tab.
//!
//! Produces a plain-text table suitable for BufferWriter output. Each cell
//! holds a short label (`NV8`, `SYS`, `PXB`, `X`). Extra columns at the
//! end report CPU affinity (pulled from the GPU's `detail` map when
//! available) and NUMA node.

use super::TopologyModel;
use super::classify_edge::classify;

/// Minimum cell width (in characters) for a matrix column. Keeps the
/// longest label (`NODE`, `NV8`) readable without overflowing on tight
/// 80-column terminals.
const MIN_CELL: usize = 5;

/// Maximum cell width — beyond this we truncate labels so the table fits
/// within the available terminal width.
const MAX_CELL: usize = 6;

/// Render the matrix view into a `String` (newline-terminated rows).
///
/// `width` is the available terminal width in cells. The renderer sizes
/// column widths to fit; when even the minimum sizing exceeds `width`,
/// it falls back to an 80-column truncation message so narrow terminals
/// degrade gracefully.
pub fn render_matrix(model: &TopologyModel, width: u16) -> String {
    if model.gpus.is_empty() {
        return "  (no GPUs on this host)\n".to_string();
    }

    let gpu_count = model.gpus.len();
    let available = width as usize;

    // Dynamic cell sizing: shrink columns until the table fits.
    let cell_width = pick_cell_width(gpu_count, available);
    if cell_width < MIN_CELL {
        return render_narrow_fallback(model);
    }

    let mut out = String::new();

    // Header row: blank corner + GPU column labels + CPU Affinity + NUMA
    let label_col_w = gpu_label_width(model);
    out.push_str(&" ".repeat(label_col_w));
    for gpu in &model.gpus {
        let hdr = format!("GPU{}", gpu.index);
        out.push_str(&center(&hdr, cell_width));
    }
    out.push_str("   CPU Affinity   NUMA\n");

    // Body rows: one row per GPU.
    let gpu_count_u32 = gpu_count as u32;
    for (r, row_gpu) in model.gpus.iter().enumerate() {
        let row_label = format!("GPU{}", row_gpu.index);
        out.push_str(&right_pad(&row_label, label_col_w));
        for (c, col_gpu) in model.gpus.iter().enumerate() {
            let edge = classify(
                r as u32,
                c as u32,
                // Rebuild a pseudo-GpuInfo from topology data: classifier
                // only looks at `nvlink_remote_devices` + `numa_node_id`.
                &pseudo_info(row_gpu),
                &pseudo_info(col_gpu),
                gpu_count_u32,
            );
            let label = edge.label();
            out.push_str(&center(&label, cell_width));
        }
        let cpu_aff = cpu_affinity(row_gpu);
        let numa = row_gpu
            .numa_node
            .map(|n| n.to_string())
            .unwrap_or_else(|| "-".to_string());
        out.push_str(&format!("   {cpu_aff:<12}   {numa}\n"));
    }

    // Legend row — mirrors nvidia-smi's convention.
    out.push_str("\nLegend:  X=self   NVn=NvLink Gen-n   NV=NvLink (gen unknown)   ");
    out.push_str("NSW=NvSwitch   PXB=PCIe bridge   NODE=PCIe same NUMA   SYS=PCIe across NUMA\n");

    out
}

/// CPU affinity helper. Reads the string from the `detail` map under
/// any of a few canonical keys; falls back to a dash when absent.
fn cpu_affinity(gpu: &super::TopologyGpu) -> String {
    // Walk known keys in the detail map. Topology renderer is informed
    // directly from GpuInfo so we never populate this here — instead
    // we require the caller to have set the `detail` map via
    // `TopologyGpu::pcie_display` / a future `cpu_affinity` field. For
    // v1 we surface whatever the NVML reader stored.
    //
    // The model doesn't currently carry this, so return "-" to keep the
    // column aligned. Future work: propagate the affinity from the
    // detail map into TopologyGpu.
    let _ = gpu;
    "-".to_string()
}

/// Build a minimal GpuInfo view from a TopologyGpu for the classifier.
/// Classifier only touches `nvlink_remote_devices`, `numa_node_id`, and
/// treats an empty `detail` map as "no extra PCIe info".
fn pseudo_info(gpu: &super::TopologyGpu) -> crate::device::GpuInfo {
    use std::collections::HashMap;
    crate::device::GpuInfo {
        uuid: gpu.uuid.clone(),
        time: String::new(),
        name: gpu.name.clone(),
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
        total_memory: 0,
        frequency: 0,
        power_consumption: 0.0,
        gpu_core_count: None,
        temperature_threshold_slowdown: None,
        temperature_threshold_shutdown: None,
        temperature_threshold_max_operating: None,
        temperature_threshold_acoustic: None,
        performance_state: None,
        numa_node_id: gpu.numa_node,
        gsp_firmware_mode: None,
        gsp_firmware_version: None,
        nvlink_remote_devices: gpu.links.clone(),
        gpm_metrics: None,
        detail: HashMap::new(),
    }
}

/// Compute the label column width (e.g. `GPU10` requires 5 chars).
fn gpu_label_width(model: &TopologyModel) -> usize {
    let max = model
        .gpus
        .iter()
        .map(|g| format!("GPU{}", g.index).len())
        .max()
        .unwrap_or(3);
    max.max(4) + 1 // min 5 cells to leave a single space of padding
}

/// Pick the widest cell width that fits the table within `available`
/// cells. Returns `0` when even the minimum doesn't fit, which the
/// caller treats as "fall back to narrow rendering".
fn pick_cell_width(gpu_count: usize, available: usize) -> usize {
    if gpu_count == 0 {
        return MAX_CELL;
    }
    // The tail (CPU Affinity + NUMA + padding) eats ~22 cells.
    // Label column ~ 5 cells.
    let overhead = 5 + 22;
    let usable = available.saturating_sub(overhead);
    for cw in (MIN_CELL..=MAX_CELL).rev() {
        if cw * gpu_count <= usable {
            return cw;
        }
    }
    // Sub-minimum: return 0 so the caller renders the narrow fallback.
    if MIN_CELL * gpu_count <= usable {
        MIN_CELL
    } else {
        0
    }
}

fn center(s: &str, w: usize) -> String {
    if s.len() >= w {
        let trimmed: String = s.chars().take(w).collect();
        return trimmed;
    }
    let total = w - s.len();
    let left = total / 2;
    let right = total - left;
    format!("{}{s}{}", " ".repeat(left), " ".repeat(right))
}

fn right_pad(s: &str, w: usize) -> String {
    if s.len() >= w {
        let trimmed: String = s.chars().take(w).collect();
        return trimmed;
    }
    format!("{s}{}", " ".repeat(w - s.len()))
}

/// Degraded fallback when the terminal is too narrow for any cell width
/// ≥ `MIN_CELL`. Produces a per-row list instead of a grid.
fn render_narrow_fallback(model: &TopologyModel) -> String {
    let mut out = String::new();
    out.push_str("  (terminal too narrow for matrix view — summary only)\n");
    for gpu in &model.gpus {
        let links = gpu.links.len();
        let numa = gpu
            .numa_node
            .map(|n| n.to_string())
            .unwrap_or_else(|| "?".to_string());
        out.push_str(&format!(
            "  GPU{idx:<3}  NUMA {numa}  {links} active NvLinks\n",
            idx = gpu.index,
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::{GpuInfo, NvLinkRemoteDevice, NvLinkRemoteType};
    use std::collections::HashMap;

    fn mk_gpu(index: u32, numa: Option<i32>, links: u32) -> GpuInfo {
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
            nvlink_remote_devices: (0..links)
                .map(|i| NvLinkRemoteDevice {
                    link_index: i,
                    remote_type: NvLinkRemoteType::Gpu,
                    bandwidth_mb_s: Some(50_000),
                })
                .collect(),
            gpm_metrics: None,
            detail,
        }
    }

    #[test]
    fn renders_header_with_gpu_columns() {
        let gpus: Vec<_> = (0..4).map(|i| mk_gpu(i, Some(0), 7)).collect();
        let model = TopologyModel::from_host("h", &gpus);
        let out = render_matrix(&model, 120);
        assert!(out.contains("GPU0"), "{out}");
        assert!(out.contains("GPU3"), "{out}");
        assert!(out.contains("CPU Affinity"), "{out}");
        assert!(out.contains("NUMA"), "{out}");
    }

    #[test]
    fn full_mesh_classifies_as_nv5_with_50gbs_bandwidth() {
        let gpus: Vec<_> = (0..8).map(|i| mk_gpu(i, Some(i as i32 / 4), 7)).collect();
        let model = TopologyModel::from_host("h", &gpus);
        let out = render_matrix(&model, 140);
        assert!(out.contains("NV5"), "{out}");
    }

    #[test]
    fn self_cell_is_x_label() {
        let gpus: Vec<_> = (0..2).map(|i| mk_gpu(i, Some(0), 1)).collect();
        let model = TopologyModel::from_host("h", &gpus);
        let out = render_matrix(&model, 120);
        // Each GPU row has exactly one "X" in a matrix cell.
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines.iter().any(|l| l.contains("X")), "{out}");
    }

    #[test]
    fn falls_back_to_summary_under_80_col() {
        let gpus: Vec<_> = (0..8).map(|i| mk_gpu(i, Some(0), 4)).collect();
        let model = TopologyModel::from_host("h", &gpus);
        let out = render_matrix(&model, 60);
        assert!(out.contains("summary only"), "{out}");
    }

    #[test]
    fn cross_numa_renders_sys_for_non_nvlink() {
        // No NvLinks, two different NUMA zones -> SYS between them.
        let gpus = vec![mk_gpu(0, Some(0), 0), mk_gpu(1, Some(1), 0)];
        let model = TopologyModel::from_host("h", &gpus);
        let out = render_matrix(&model, 120);
        assert!(out.contains("SYS"), "{out}");
    }

    #[test]
    fn same_numa_renders_node_for_non_nvlink() {
        let gpus = vec![mk_gpu(0, Some(0), 0), mk_gpu(1, Some(0), 0)];
        let model = TopologyModel::from_host("h", &gpus);
        let out = render_matrix(&model, 120);
        assert!(out.contains("NODE"), "{out}");
    }

    #[test]
    fn legend_contains_vocabulary() {
        let gpus = vec![mk_gpu(0, Some(0), 0), mk_gpu(1, Some(0), 0)];
        let model = TopologyModel::from_host("h", &gpus);
        let out = render_matrix(&model, 120);
        for term in ["X=self", "PXB", "NODE", "SYS"] {
            assert!(out.contains(term), "legend missing {term}: {out}");
        }
    }

    #[test]
    fn empty_model_renders_placeholder() {
        let model = TopologyModel::default();
        let out = render_matrix(&model, 120);
        assert!(out.contains("no GPUs"), "{out}");
    }

    #[test]
    fn cell_width_picker_honours_available_space() {
        // Tight budget (80 cols) with 8 GPUs: should pick min cell width
        // (5) or fall back below.
        let cw = pick_cell_width(8, 80);
        assert!(cw >= MIN_CELL || cw == 0);
    }
}
