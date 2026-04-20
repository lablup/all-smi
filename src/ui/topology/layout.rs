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

//! Layout computation for the topology graph view.
//!
//! The renderer groups GPUs by NUMA zone and stacks the resulting boxes
//! either horizontally (side-by-side) or vertically depending on the
//! available terminal width. This module keeps the stacking decision
//! purely data-driven so the logic is unit-testable without touching the
//! terminal.

use super::TopologyModel;

/// Minimum horizontal cells required to render a NUMA box with `g` GPUs
/// laid out in a 2xN grid. We reserve 13 cells per GPU column (label +
/// padding + inter-column connector) and 4 cells for the box borders.
pub fn numa_box_width_cells(gpus_in_numa: u32) -> u16 {
    let columns = numa_column_count(gpus_in_numa);
    (columns as u16 * 13) + 6
}

/// How many columns the renderer uses inside a single NUMA box. Keeps
/// 2xN rectangular layouts for 2/4/8 GPUs (the most common topologies)
/// and falls back to a single row for odd counts.
pub fn numa_column_count(gpus_in_numa: u32) -> u32 {
    match gpus_in_numa {
        0 | 1 => 1,
        2 => 2,
        3 => 3,
        4 => 2, // 2x2
        5 | 6 => 3,
        7 | 8 => 4, // 2x4
        _ => 4,     // 2xN with N rows
    }
}

/// How many rows the renderer uses inside a single NUMA box.
pub fn numa_row_count(gpus_in_numa: u32) -> u32 {
    match gpus_in_numa {
        0 => 0,
        1 => 1,
        2 | 3 => 1,
        4 => 2,
        5 | 6 => 2,
        7 | 8 => 2,
        n => n.div_ceil(4),
    }
}

/// Stacking strategy for NUMA boxes in the graph view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoxStacking {
    /// All NUMA boxes side-by-side on the same row.
    Horizontal,
    /// NUMA boxes stacked top-to-bottom. Used when horizontal would
    /// overflow the available width.
    Vertical,
}

/// Resolved graph layout plan.
#[derive(Debug, Clone)]
pub struct GraphLayout {
    pub stacking: BoxStacking,
    /// GPU indices grouped by NUMA node in the same order the renderer
    /// should walk them. Outer Vec is the NUMA zone, inner Vec is the
    /// GPU slots inside that zone (row-major, `numa_column_count` wide).
    pub numa_groups: Vec<NumaGroup>,
    /// Width cells the graph will actually use. Reserved for a future
    /// iteration that centres the summary footer under the drawn boxes
    /// or right-aligns the mode indicator; kept in the plan today so
    /// the layout-to-render contract is already in place.
    #[allow(dead_code)]
    pub used_width: u16,
}

/// GPUs assigned to a single NUMA zone.
#[derive(Debug, Clone)]
pub struct NumaGroup {
    pub numa_node: Option<i32>,
    /// Indices into `TopologyModel::gpus`.
    pub gpu_slots: Vec<usize>,
    /// Cached column count for this zone (== `numa_column_count(len)`).
    pub columns: u32,
}

impl GraphLayout {
    /// Compute the layout for `model` inside `available_width` cells.
    ///
    /// Returns a plan the graph renderer can execute without having to
    /// redo the width arithmetic.
    pub fn plan(model: &TopologyModel, available_width: u16) -> Self {
        let numa_nodes = model.numa_nodes();
        let mut numa_groups: Vec<NumaGroup> = numa_nodes
            .iter()
            .map(|numa| {
                let gpu_slots: Vec<usize> = model
                    .gpus
                    .iter()
                    .enumerate()
                    .filter(|(_, g)| g.numa_node == *numa)
                    .map(|(i, _)| i)
                    .collect();
                let columns = numa_column_count(gpu_slots.len() as u32);
                NumaGroup {
                    numa_node: *numa,
                    gpu_slots,
                    columns,
                }
            })
            .collect();

        // Horizontal stacking: sum the box widths plus inter-box gaps.
        // Guard `sum` against an empty list so `used_width` stays at 0.
        let horizontal_width: u16 = numa_groups
            .iter()
            .map(|grp| numa_box_width_cells(grp.gpu_slots.len() as u32))
            .sum::<u16>()
            + numa_groups.len().saturating_sub(1) as u16 * 3;

        let (stacking, used_width) =
            if numa_groups.len() <= 1 || horizontal_width <= available_width {
                (
                    BoxStacking::Horizontal,
                    horizontal_width.min(available_width),
                )
            } else {
                // Vertical stacking: width is the widest single NUMA box.
                let max_width = numa_groups
                    .iter()
                    .map(|grp| numa_box_width_cells(grp.gpu_slots.len() as u32))
                    .max()
                    .unwrap_or(0)
                    .min(available_width);
                (BoxStacking::Vertical, max_width)
            };

        // Deterministic ordering for repeatable rendering and tests:
        // by numeric NUMA id, with `None` last.
        numa_groups.sort_by(|a, b| match (a.numa_node, b.numa_node) {
            (Some(x), Some(y)) => x.cmp(&y),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        });

        Self {
            stacking,
            numa_groups,
            used_width,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::{GpuInfo, NvLinkRemoteDevice, NvLinkRemoteType};
    use std::collections::HashMap;

    fn mk_gpu(index: u32, numa: Option<i32>) -> GpuInfo {
        let mut detail = HashMap::new();
        detail.insert("index".to_string(), index.to_string());
        GpuInfo {
            uuid: format!("GPU-{index}"),
            time: String::new(),
            name: "NVIDIA H100".to_string(),
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
            nvlink_remote_devices: vec![NvLinkRemoteDevice {
                link_index: 0,
                remote_type: NvLinkRemoteType::Gpu,
                bandwidth_mb_s: None,
            }],
            gpm_metrics: None,
            detail,
        }
    }

    #[test]
    fn two_gpus_one_numa_picks_horizontal() {
        let model = TopologyModel::from_host("h", &[mk_gpu(0, Some(0)), mk_gpu(1, Some(0))]);
        let layout = GraphLayout::plan(&model, 120);
        assert_eq!(layout.stacking, BoxStacking::Horizontal);
        assert_eq!(layout.numa_groups.len(), 1);
        assert_eq!(layout.numa_groups[0].gpu_slots.len(), 2);
    }

    #[test]
    fn four_gpus_single_numa_uses_2x2_grid() {
        let gpus: Vec<_> = (0..4).map(|i| mk_gpu(i, Some(0))).collect();
        let model = TopologyModel::from_host("h", &gpus);
        let layout = GraphLayout::plan(&model, 200);
        assert_eq!(layout.numa_groups.len(), 1);
        assert_eq!(layout.numa_groups[0].columns, 2);
        assert_eq!(numa_row_count(4), 2);
    }

    #[test]
    fn eight_gpus_two_numa_fits_horizontally_on_wide_terminal() {
        let mut gpus = Vec::new();
        for i in 0..8 {
            gpus.push(mk_gpu(i, Some((i as i32) / 4)));
        }
        let model = TopologyModel::from_host("h", &gpus);
        let layout = GraphLayout::plan(&model, 200);
        assert_eq!(layout.stacking, BoxStacking::Horizontal);
        assert_eq!(layout.numa_groups.len(), 2);
        assert_eq!(layout.numa_groups[0].gpu_slots.len(), 4);
        assert_eq!(layout.numa_groups[1].gpu_slots.len(), 4);
    }

    #[test]
    fn eight_gpus_two_numa_falls_back_to_vertical_on_narrow_terminal() {
        // 4-GPU NUMA box width = columns(2) * 13 + 6 = 32 cells; two
        // boxes side-by-side + 3-cell gap = 67 cells. 50 columns must
        // force vertical stacking.
        let mut gpus = Vec::new();
        for i in 0..8 {
            gpus.push(mk_gpu(i, Some((i as i32) / 4)));
        }
        let model = TopologyModel::from_host("h", &gpus);
        let layout = GraphLayout::plan(&model, 50);
        assert_eq!(layout.stacking, BoxStacking::Vertical);
        assert_eq!(layout.numa_groups.len(), 2);
    }

    #[test]
    fn unknown_numa_sorts_last() {
        let gpus = vec![mk_gpu(0, None), mk_gpu(1, Some(1)), mk_gpu(2, Some(0))];
        let model = TopologyModel::from_host("h", &gpus);
        let layout = GraphLayout::plan(&model, 200);
        let numa_order: Vec<_> = layout.numa_groups.iter().map(|g| g.numa_node).collect();
        assert_eq!(numa_order, vec![Some(0), Some(1), None]);
    }

    #[test]
    fn used_width_never_exceeds_available_width() {
        let gpus: Vec<_> = (0..8).map(|i| mk_gpu(i, Some((i as i32) / 4))).collect();
        let model = TopologyModel::from_host("h", &gpus);
        let layout = GraphLayout::plan(&model, 50);
        assert!(layout.used_width <= 50, "{}", layout.used_width);
    }

    #[test]
    fn single_gpu_has_one_column_one_row() {
        assert_eq!(numa_column_count(1), 1);
        assert_eq!(numa_row_count(1), 1);
    }

    #[test]
    fn column_counts_match_2x_n_convention() {
        assert_eq!(numa_column_count(4), 2);
        assert_eq!(numa_column_count(8), 4);
        assert_eq!(numa_row_count(8), 2);
    }
}
