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

//! ASCII graph rendering for the topology tab.
//!
//! Produces NUMA boxes, one per NUMA zone, with the GPUs placed inside
//! and NvLink/NvSwitch edges rendered between them. The layout strategy
//! is computed in [`super::layout`]; here we just draw.
//!
//! The output is **plain text** (no ANSI escapes) so it composes cleanly
//! with the `BufferWriter` pipeline. Colour highlighting happens at the
//! orchestrator level by wrapping specific fragments in ANSI codes where
//! the layout allows.

use super::classify_edge::EdgeClass;
use super::layout::{BoxStacking, GraphLayout, NumaGroup, numa_row_count};
use super::{TopologyGpu, TopologyModel};

/// Render the graph view as a plain-text string (one-or-more newline-
/// terminated lines). Caller is responsible for width checks — see
/// [`super::GRAPH_MIN_WIDTH`].
pub fn render_graph(model: &TopologyModel, available_width: u16) -> String {
    if model.gpus.is_empty() {
        return "  (no GPUs on this host)\n".to_string();
    }

    let layout = GraphLayout::plan(model, available_width);
    let mut out = String::new();

    match layout.stacking {
        BoxStacking::Horizontal => render_horizontal(&mut out, model, &layout),
        BoxStacking::Vertical => render_vertical(&mut out, model, &layout),
    }

    render_footer(&mut out, model);
    out
}

/// Draw all NUMA boxes side-by-side.
fn render_horizontal(out: &mut String, model: &TopologyModel, layout: &GraphLayout) {
    // Compose each NUMA box into a list of lines, then zip them column-
    // by-column so the boxes sit at the same row offsets.
    let box_lines: Vec<Vec<String>> = layout
        .numa_groups
        .iter()
        .map(|grp| render_numa_box(model, grp))
        .collect();
    let max_lines = box_lines.iter().map(|v| v.len()).max().unwrap_or(0);

    for line_idx in 0..max_lines {
        let mut composite = String::new();
        for (i, lines) in box_lines.iter().enumerate() {
            if i > 0 {
                composite.push_str("   ");
            }
            let line = lines.get(line_idx).map(|s| s.as_str()).unwrap_or("");
            composite.push_str(line);
        }
        composite.push('\n');
        out.push_str(&composite);
    }
}

/// Draw each NUMA box on its own row.
fn render_vertical(out: &mut String, model: &TopologyModel, layout: &GraphLayout) {
    for (i, group) in layout.numa_groups.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let lines = render_numa_box(model, group);
        for line in lines {
            out.push_str(&line);
            out.push('\n');
        }
    }
}

/// Render a single NUMA zone as a box with its GPUs inside.
fn render_numa_box(model: &TopologyModel, group: &NumaGroup) -> Vec<String> {
    let numa_label = match group.numa_node {
        Some(n) => format!("NUMA {n}"),
        None => "NUMA ?".to_string(),
    };
    let gpu_count = group.gpu_slots.len() as u32;
    let columns = group.columns.max(1);
    let rows = numa_row_count(gpu_count);
    let cell_width: usize = 13;
    let inner_width: usize = columns as usize * cell_width;
    let total_width = inner_width + 2;

    let mut lines: Vec<String> = Vec::new();

    // Top border with inline NUMA label.
    let label_padded = format!(" {numa_label} ");
    let dashes = total_width.saturating_sub(label_padded.len() + 2);
    let left_dashes = dashes / 2;
    let right_dashes = dashes - left_dashes;
    let left_dash = "─".repeat(left_dashes);
    let right_dash = "─".repeat(right_dashes);
    lines.push(format!("┌{left_dash}{label_padded}{right_dash}┐"));

    for row in 0..rows {
        // GPU row: labels like "[GPU 0]" centred in each cell.
        let mut gpu_line = String::from("│");
        for col in 0..columns {
            let slot_idx = (row * columns + col) as usize;
            let cell = if let Some(gpu_idx) = group.gpu_slots.get(slot_idx) {
                let gpu = &model.gpus[*gpu_idx];
                center(&format!("[GPU {idx}]", idx = gpu.index), cell_width)
            } else {
                " ".repeat(cell_width)
            };
            gpu_line.push_str(&cell);
        }
        gpu_line.push('│');
        lines.push(gpu_line);

        // Edge row: NvLink / fallback labels connecting horizontally
        // adjacent GPUs in this row (when there are at least two
        // columns).
        let mut edge_line = String::from("│");
        for col in 0..columns {
            let slot_idx = (row * columns + col) as usize;
            let cell = if col + 1 < columns {
                let left_slot_idx = slot_idx;
                let right_slot_idx = slot_idx + 1;
                let left_gpu = group.gpu_slots.get(left_slot_idx).map(|i| &model.gpus[*i]);
                let right_gpu = group.gpu_slots.get(right_slot_idx).map(|i| &model.gpus[*i]);
                match (left_gpu, right_gpu) {
                    (Some(a), Some(b)) => center(&edge_label(model, a, b), cell_width),
                    _ => " ".repeat(cell_width),
                }
            } else {
                " ".repeat(cell_width)
            };
            edge_line.push_str(&cell);
        }
        edge_line.push('│');
        // Only push the edge row when at least one adjacent cell exists.
        if columns > 1 {
            lines.push(edge_line);
        }

        // Divider between GPU rows inside the same NUMA box (keeps the
        // 2xN grid legible).
        if row + 1 < rows {
            let divider = format!("│{}│", " ".repeat(inner_width),);
            lines.push(divider);
        }
    }

    // If this NUMA is NvSwitch-mediated, add a "nvsw" annotation row
    // between the GPU rows.
    if model.has_nvswitch && rows >= 2 {
        let annotation = center("nvsw ── nvsw", inner_width);
        // Insert BEFORE the last divider-or-row; simpler to append when
        // rendered vertically without divider tracking.
        lines.push(format!("│{annotation}│"));
    }

    // Bottom border.
    lines.push(format!("└{}┘", "─".repeat(total_width.saturating_sub(2))));

    lines
}

/// Label placed between two horizontally-adjacent GPUs.
fn edge_label(model: &TopologyModel, a: &TopologyGpu, b: &TopologyGpu) -> String {
    // Build pseudo-GpuInfo views for the classifier.
    let total = model.gpu_count();
    let class =
        super::classify_edge::classify(a.index, b.index, &pseudo_info(a), &pseudo_info(b), total);
    match class {
        EdgeClass::SelfCell => "──".to_string(),
        EdgeClass::NvLink {
            count: _,
            generation,
        } => match generation {
            Some(g) => format!("── NV{g} ──"),
            None => "── NV ──".to_string(),
        },
        EdgeClass::NvSwitch { .. } => "── NSW ──".to_string(),
        EdgeClass::NvSwitchMesh => "── NV ──".to_string(),
        EdgeClass::PcieSameRoot => "── PXB ──".to_string(),
        EdgeClass::PcieSameNuma => "── NODE ──".to_string(),
        EdgeClass::SysInterconnect => "── SYS ──".to_string(),
        EdgeClass::Unknown => "── ? ──".to_string(),
    }
}

/// Build a minimal GpuInfo for the classifier.
fn pseudo_info(gpu: &TopologyGpu) -> crate::device::GpuInfo {
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

/// Footer line with summary + legend.
fn render_footer(out: &mut String, model: &TopologyModel) {
    out.push('\n');
    if !model.has_nvlink {
        out.push_str("  (no active NvLinks — PCIe-only topology)\n");
    } else {
        let summary = model.summary();
        out.push_str(&format!("  {summary}\n"));
    }
    // Legend: keep short to fit narrow terminals.
    if model.is_nvidia {
        out.push_str(
            "  Legend:  NVn=NvLink Gen-n  NSW=NvSwitch  PXB=PCIe bridge  \
             NODE=same NUMA  SYS=across NUMA\n",
        );
    } else {
        out.push_str("  Legend:  NODE=same NUMA  SYS=across NUMA\n");
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::{GpuInfo, NvLinkRemoteDevice, NvLinkRemoteType};
    use std::collections::HashMap;

    fn mk_gpu(index: u32, numa: Option<i32>, link_count: u32, switch: bool) -> GpuInfo {
        let mut detail = HashMap::new();
        detail.insert("index".to_string(), index.to_string());
        let mut links: Vec<NvLinkRemoteDevice> = (0..link_count)
            .map(|i| NvLinkRemoteDevice {
                link_index: i,
                remote_type: NvLinkRemoteType::Gpu,
                bandwidth_mb_s: Some(50_000),
            })
            .collect();
        if switch {
            links.push(NvLinkRemoteDevice {
                link_index: link_count,
                remote_type: NvLinkRemoteType::Switch,
                bandwidth_mb_s: None,
            });
        }
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
    fn two_gpu_one_numa_draws_box_and_edge() {
        let gpus = vec![mk_gpu(0, Some(0), 1, false), mk_gpu(1, Some(0), 1, false)];
        let model = TopologyModel::from_host("h", &gpus);
        let out = render_graph(&model, 200);
        assert!(out.contains("NUMA 0"), "{out}");
        assert!(out.contains("[GPU 0]"), "{out}");
        assert!(out.contains("[GPU 1]"), "{out}");
        assert!(out.contains("Legend"), "{out}");
    }

    #[test]
    fn eight_gpu_two_numa_wide_terminal_renders_horizontally() {
        let gpus: Vec<_> = (0..8)
            .map(|i| mk_gpu(i, Some(i as i32 / 4), 7, true))
            .collect();
        let model = TopologyModel::from_host("h", &gpus);
        let out = render_graph(&model, 200);
        assert!(out.contains("NUMA 0"), "{out}");
        assert!(out.contains("NUMA 1"), "{out}");
        assert!(out.contains("nvsw"), "{out}");
    }

    #[test]
    fn no_nvlink_shows_placeholder_and_still_draws_numa() {
        let gpus = vec![mk_gpu(0, Some(0), 0, false), mk_gpu(1, Some(0), 0, false)];
        let model = TopologyModel::from_host("h", &gpus);
        let out = render_graph(&model, 200);
        assert!(out.contains("NUMA 0"), "{out}");
        assert!(out.contains("no active NvLinks"), "{out}");
    }

    #[test]
    fn narrow_terminal_falls_back_to_vertical_stacking() {
        // See `eight_gpus_two_numa_falls_back_to_vertical_on_narrow_terminal`
        // in `layout.rs` for the cell-width arithmetic that places the
        // horizontal-to-vertical threshold around 50 columns.
        let gpus: Vec<_> = (0..8)
            .map(|i| mk_gpu(i, Some(i as i32 / 4), 7, true))
            .collect();
        let model = TopologyModel::from_host("h", &gpus);
        let out = render_graph(&model, 50);
        // Vertical stacking produces two box segments separated by blank.
        let numa0_pos = out.find("NUMA 0").unwrap();
        let numa1_pos = out.find("NUMA 1").unwrap();
        assert!(numa1_pos > numa0_pos, "{out}");
        // Ensure the two NUMAs are on different visual rows (a newline
        // between them proves vertical stacking).
        let between = &out[numa0_pos..numa1_pos];
        assert!(between.contains('\n'), "{out}");
    }

    #[test]
    fn empty_model_renders_placeholder() {
        let model = TopologyModel::default();
        let out = render_graph(&model, 200);
        assert!(out.contains("no GPUs"), "{out}");
    }

    #[test]
    fn unknown_numa_renders_as_question_mark() {
        let gpus = vec![mk_gpu(0, None, 0, false), mk_gpu(1, None, 0, false)];
        let model = TopologyModel::from_host("h", &gpus);
        let out = render_graph(&model, 200);
        assert!(out.contains("NUMA ?"), "{out}");
    }

    #[test]
    fn graph_renders_switch_annotation_when_nvswitch_present() {
        let gpus = vec![
            mk_gpu(0, Some(0), 1, true),
            mk_gpu(1, Some(0), 1, true),
            mk_gpu(2, Some(0), 1, true),
            mk_gpu(3, Some(0), 1, true),
        ];
        let model = TopologyModel::from_host("h", &gpus);
        let out = render_graph(&model, 200);
        assert!(out.contains("nvsw"), "{out}");
    }
}
