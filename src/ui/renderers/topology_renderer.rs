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

//! Topology tab renderer (issue #190).
//!
//! Orchestrates the topology view for a single host:
//! 1. Pull the GPUs for the target host out of the render snapshot.
//! 2. Build a [`TopologyModel`].
//! 3. Pick graph vs. matrix based on the in-tab mode **and** the
//!    available terminal width.
//! 4. Write the rendered block to the caller's `BufferWriter`.
//!
//! Keeps the business logic thin so the heavy rendering lives in
//! [`crate::ui::topology::graph_render`] and
//! [`crate::ui::topology::matrix_render`].

use std::io::Write;

use crossterm::{queue, style::Color, style::Print};

use crate::device::GpuInfo;
use crate::ui::buffer::BufferWriter;
use crate::ui::text::print_colored_text;
use crate::ui::topology::{
    GRAPH_MIN_WIDTH, TopologyModel, TopologyViewMode, graph_render, matrix_render,
};

/// Entry point invoked by the frame renderer when the Topology tab is
/// active for the currently-selected host.
///
/// * `host_id` — the host selected in the tab strip (local hostname or
///   remote host_id). Used to filter the GPU slice and label the header.
/// * `mode` — the in-tab graph/matrix mode the operator has toggled.
/// * `cols`/`rows` — current terminal dimensions.
pub fn render_topology_tab(
    buffer: &mut BufferWriter,
    gpu_info: &[GpuInfo],
    host_id: &str,
    mode: TopologyViewMode,
    cols: u16,
    _rows: u16,
) {
    let host_gpus = filter_host_gpus(gpu_info, host_id);
    let model = TopologyModel::from_host(host_id, &host_gpus);

    render_header(buffer, &model, mode, cols);

    // Width-adaptive fallback: even in graph mode, drop to matrix on
    // narrow terminals so the content never overflows 80 columns.
    let effective_mode = if mode == TopologyViewMode::Graph && cols < GRAPH_MIN_WIDTH {
        TopologyViewMode::Matrix
    } else {
        mode
    };

    let dropped_to_matrix =
        mode == TopologyViewMode::Graph && effective_mode == TopologyViewMode::Matrix;

    match effective_mode {
        TopologyViewMode::Graph => {
            let rendered = graph_render::render_graph(&model, cols);
            write_plain(buffer, &rendered);
        }
        TopologyViewMode::Matrix => {
            if dropped_to_matrix {
                // Dropped from graph — surface a hint above the matrix
                // so the operator knows why they're seeing it.
                print_colored_text(
                    buffer,
                    "  (terminal narrower than 100 columns — showing matrix fallback)",
                    Color::DarkGrey,
                    None,
                    None,
                );
                queue!(buffer, Print("\r\n")).unwrap();
            }
            let rendered = matrix_render::render_matrix(&model, cols);
            write_plain(buffer, &rendered);
        }
    }
}

/// Collect the GPUs that belong to the currently-selected host. In local
/// mode this is every GPU (host filtering is a no-op); in remote mode we
/// filter on `host_id`.
fn filter_host_gpus(gpu_info: &[GpuInfo], host_id: &str) -> Vec<GpuInfo> {
    if host_id.is_empty() || host_id == "All" {
        return gpu_info.to_vec();
    }
    gpu_info
        .iter()
        .filter(|g| g.host_id == host_id || g.hostname == host_id)
        .cloned()
        .collect()
}

/// Write the top-of-panel header with host + mode indicator + hotkey hint.
fn render_header(
    buffer: &mut BufferWriter,
    model: &TopologyModel,
    mode: TopologyViewMode,
    cols: u16,
) {
    let host = if model.host_label.is_empty() {
        "(local)"
    } else {
        &model.host_label
    };
    let mode_label = mode.as_label();
    let gpu_count = model.gpu_count();
    let title =
        format!(" Topology ─ {host} ─ {gpu_count} GPUs ─ mode: {mode_label} (press M to toggle) ");
    let truncated = truncate_line(&title, cols as usize);
    print_colored_text(buffer, &truncated, Color::Black, Some(Color::Cyan), None);
    queue!(buffer, Print("\r\n")).unwrap();
    // Summary line (e.g. "8 GPUs · 2 NUMA · 56 NvLinks"). Empty on
    // non-NVIDIA hosts with no topology data.
    let summary = model.summary();
    if !summary.is_empty() {
        print_colored_text(buffer, &format!("  {summary}"), Color::DarkGrey, None, None);
        queue!(buffer, Print("\r\n")).unwrap();
    }
}

/// Write a multi-line plain-text block to the buffer, preserving line
/// endings so `BufferWriter` accounts for them correctly.
fn write_plain(buffer: &mut BufferWriter, text: &str) {
    for line in text.lines() {
        buffer.write_all(line.as_bytes()).ok();
        queue!(buffer, Print("\r\n")).ok();
    }
}

/// Truncate a single-line header string to fit in `cells` characters.
fn truncate_line(s: &str, cells: usize) -> String {
    if s.chars().count() <= cells {
        return s.to_string();
    }
    s.chars().take(cells).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::{GpuInfo, NvLinkRemoteDevice, NvLinkRemoteType};
    use std::collections::HashMap;

    fn mk_gpu(index: u32, host: &str, numa: Option<i32>) -> GpuInfo {
        let mut detail = HashMap::new();
        detail.insert("index".to_string(), index.to_string());
        GpuInfo {
            uuid: format!("GPU-{index}"),
            time: String::new(),
            name: "NVIDIA H100".to_string(),
            device_type: "GPU".to_string(),
            host_id: host.to_string(),
            hostname: host.to_string(),
            instance: host.to_string(),
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
            nvlink_remote_devices: vec![NvLinkRemoteDevice {
                link_index: 0,
                remote_type: NvLinkRemoteType::Gpu,
                bandwidth_mb_s: Some(50_000),
            }],
            gpm_metrics: None,
            detail,
        }
    }

    #[test]
    fn host_filter_preserves_all_in_local() {
        let gpus = vec![mk_gpu(0, "h1", Some(0)), mk_gpu(1, "h2", Some(0))];
        assert_eq!(filter_host_gpus(&gpus, "").len(), 2);
        assert_eq!(filter_host_gpus(&gpus, "All").len(), 2);
    }

    #[test]
    fn host_filter_narrows_to_single_host_in_remote() {
        let gpus = vec![mk_gpu(0, "h1", Some(0)), mk_gpu(1, "h2", Some(0))];
        let filtered = filter_host_gpus(&gpus, "h2");
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].host_id, "h2");
    }

    #[test]
    fn renders_topology_tab_without_panic() {
        let gpus: Vec<_> = (0..4).map(|i| mk_gpu(i, "h1", Some(0))).collect();
        let mut buf = BufferWriter::new();
        render_topology_tab(&mut buf, &gpus, "h1", TopologyViewMode::Graph, 180, 40);
        let out = buf.get_buffer().to_string();
        assert!(out.contains("Topology"), "{out}");
        assert!(out.contains("GPU"), "{out}");
    }

    #[test]
    fn graph_mode_falls_back_to_matrix_on_narrow_terminal() {
        let gpus: Vec<_> = (0..4).map(|i| mk_gpu(i, "h1", Some(0))).collect();
        let mut buf = BufferWriter::new();
        render_topology_tab(&mut buf, &gpus, "h1", TopologyViewMode::Graph, 80, 40);
        let out = buf.get_buffer().to_string();
        // Matrix output contains the "Legend: X=self" string; graph
        // output never does.
        assert!(out.contains("X=self"), "{out}");
        assert!(out.contains("matrix fallback"), "{out}");
    }

    #[test]
    fn matrix_mode_emits_legend_vocabulary() {
        let gpus: Vec<_> = (0..2).map(|i| mk_gpu(i, "h1", Some(0))).collect();
        let mut buf = BufferWriter::new();
        render_topology_tab(&mut buf, &gpus, "h1", TopologyViewMode::Matrix, 200, 40);
        let out = buf.get_buffer().to_string();
        assert!(out.contains("CPU Affinity"), "{out}");
    }

    #[test]
    fn empty_host_renders_placeholder() {
        let mut buf = BufferWriter::new();
        render_topology_tab(&mut buf, &[], "h1", TopologyViewMode::Graph, 200, 40);
        let out = buf.get_buffer().to_string();
        assert!(out.contains("no GPUs"), "{out}");
    }
}
