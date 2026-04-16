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

/// UI layout calculation utilities
use crate::app_state::AppState;
use crate::cli::ViewArgs;
use crate::device::{GpuInfo, MigGpuInfo, VgpuHostInfo};
use crate::ui::activity_panel;
use crate::ui::gpu_sparkline_panel;
use crate::ui::renderers::gpu_renderer::{
    build_mig_uuid_lookup, build_vgpu_uuid_lookup, gpu_render_line_count,
    gpu_render_line_count_with_lookup,
};

pub struct LayoutCalculator;

impl LayoutCalculator {
    /// Calculate the number of header lines for dynamic layout.
    ///
    /// In local mode (`state.is_local_mode == true`) the Cluster Overview card,
    /// dashboard items, and the tabs row are all suppressed by `render_main()`,
    /// so their line contributions are excluded here.  This keeps the content
    /// area (process list, GPU rows) from being unnecessarily clipped and
    /// preserves the "reserved space for function keys" regression fix.
    pub fn calculate_header_lines(state: &AppState) -> u16 {
        let mut lines = 0u16;

        // Basic header (title line)
        lines += 1;

        if state.is_local_mode {
            // Local mode shows the two-line host summary bar (identity + sparklines)
            lines += 2;
        } else {
            // "Cluster Overview" label line
            lines += 1;

            // System overview dashboard card (2 rows) + label separator row
            lines += 4;

            // Live statistics section (remote sparkline panel)
            if !state.utilization_history.is_empty() || !state.cpu_utilization_history.is_empty() {
                lines += 5; // Separator + header + 3 sparkline rows
            }

            // Tabs section
            lines += 2; // Tabs line + separator
        }

        lines
    }

    /// Calculate available content area
    pub fn calculate_content_area(state: &AppState, cols: u16, rows: u16) -> ContentArea {
        let header_lines = Self::calculate_header_lines(state);
        let function_keys_lines = 1; // Reserve space for function keys

        // In local mode, the Activity panel (CPU left + GPU right) consumes
        // additional rows.  Both halves render on the same terminal rows, so
        // we take the maximum height of the two.
        let activity_panel_lines = if state.is_local_mode {
            let cpu_lines = activity_panel::panel_height(&state.cpu_info, cols);
            let gpu_content = gpu_sparkline_panel::gpu_content_rows(state) as u16;
            // GPU panel has top+bottom borders (+2) when it has content
            let gpu_lines = if gpu_content > 0 { gpu_content + 2 } else { 0 };
            cpu_lines.max(gpu_lines)
        } else {
            0
        };

        let available_rows = rows
            .saturating_sub(header_lines)
            .saturating_sub(activity_panel_lines)
            .saturating_sub(function_keys_lines);

        ContentArea {
            x: 0,
            y: header_lines + activity_panel_lines,
            width: cols,
            height: available_rows,
            available_rows: available_rows as usize,
        }
    }

    /// Calculate GPU display parameters
    pub fn calculate_gpu_display_params(
        state: &AppState,
        args: &ViewArgs,
        content_area: &ContentArea,
    ) -> GpuDisplayParams {
        let is_remote = args.hosts.is_some() || args.hostfile.is_some();

        // Calculate storage space requirements
        let storage_items_count = Self::calculate_storage_items_count(state, args);
        let storage_display_rows = if storage_items_count > 0 {
            storage_items_count + 2 // Header + items
        } else {
            0
        };

        // Calculate GPU display area
        let gpu_display_rows = if is_remote {
            if state.current_tab < state.tabs.len() && state.tabs[state.current_tab] == "All" {
                content_area.available_rows // Full space for "All" tab
            } else {
                content_area
                    .available_rows
                    .saturating_sub(storage_display_rows)
            }
        } else if state.process_info.is_empty() {
            content_area
                .available_rows
                .saturating_sub(storage_display_rows)
        } else {
            content_area
                .available_rows
                .saturating_sub(storage_display_rows)
                / 2
        };

        // Each GPU may render 2, 3, or more lines depending on whether it
        // populates the optional thermal/P-state row (NVML 0.12+ data) and
        // whether a vGPU section nests beneath it. Use the maximum line
        // count over the GPUs visible in the current tab so PgUp/PgDn
        // never overshoots the rendered area. A pessimistic max means
        // mixed pages may under-fill by one row, which is acceptable —
        // overshooting would clip the gauges off-screen and corrupt the
        // scroll math.
        let lines_per_gpu = max_gpu_lines_for_tab(state).max(2);
        let max_gpu_items = gpu_display_rows / lines_per_gpu;

        GpuDisplayParams {
            display_rows: gpu_display_rows,
            lines_per_gpu,
            max_items: max_gpu_items,
            start_index: state.gpu_scroll_offset,
            storage_rows: storage_display_rows,
        }
    }

    /// Calculate progress bar layout
    #[allow(dead_code)] // Future progress bar layout
    pub fn calculate_progress_bar_layout(
        width: usize,
        num_bars: usize,
        padding: usize,
    ) -> ProgressBarLayout {
        let total_padding = padding * 2; // Left and right padding
        let separators = if num_bars > 1 { (num_bars - 1) * 2 } else { 0 }; // 2 spaces between bars

        let available_width = width.saturating_sub(total_padding + separators);
        let bar_width = if num_bars > 0 {
            available_width / num_bars
        } else {
            available_width
        };

        ProgressBarLayout {
            bar_width,
            left_padding: padding,
            right_padding: padding,
            separator_width: if num_bars > 1 { 2 } else { 0 },
            total_bars: num_bars,
        }
    }

    /// Calculate dynamic column widths for tables
    #[allow(dead_code)] // Future table layout
    pub fn calculate_table_columns(
        available_width: usize,
        column_specs: &[ColumnSpec],
    ) -> Vec<usize> {
        let min_total: usize = column_specs.iter().map(|c| c.min_width).sum();
        let separator_width = column_specs.len() - 1; // 1 space between columns

        if available_width <= min_total + separator_width {
            // Use minimum widths if not enough space
            return column_specs.iter().map(|c| c.min_width).collect();
        }

        let extra_space = available_width - min_total - separator_width;
        let total_weight: f32 = column_specs.iter().map(|c| c.weight).sum();

        let mut widths = Vec::new();
        for spec in column_specs {
            let extra = (extra_space as f32 * spec.weight / total_weight) as usize;
            widths.push(spec.min_width + extra);
        }

        widths
    }

    /// Public entry point used by `view::event_handler` so PgUp / PgDn page
    /// sizes stay consistent with the rendered layout.
    pub fn max_gpu_lines_for_tab(state: &AppState) -> usize {
        max_gpu_lines_for_tab(state)
    }

    fn calculate_storage_items_count(state: &AppState, args: &ViewArgs) -> usize {
        let is_remote = args.hosts.is_some() || args.hostfile.is_some();

        if state.storage_info.is_empty() {
            return 0;
        }

        if is_remote {
            if state.current_tab < state.tabs.len() && state.tabs[state.current_tab] != "All" {
                let current_hostname = &state.tabs[state.current_tab];
                state
                    .storage_info
                    .iter()
                    .filter(|info| info.host_id == *current_hostname)
                    .count()
            } else {
                0
            }
        } else {
            state.storage_info.len()
        }
    }
}

/// Content area dimensions
#[derive(Debug, Clone)]
pub struct ContentArea {
    #[allow(dead_code)] // Future layout calculations
    pub x: u16,
    #[allow(dead_code)] // Future layout calculations
    pub y: u16,
    #[allow(dead_code)] // Future layout calculations
    pub width: u16,
    #[allow(dead_code)] // Future layout calculations
    pub height: u16,
    pub available_rows: usize,
}

/// GPU display parameters
#[derive(Debug, Clone)]
pub struct GpuDisplayParams {
    #[allow(dead_code)] // Future layout calculations
    pub display_rows: usize,
    #[allow(dead_code)] // Future layout calculations
    pub lines_per_gpu: usize,
    pub max_items: usize, // Used in ui_loop.rs
    #[allow(dead_code)] // Future layout calculations
    pub start_index: usize,
    #[allow(dead_code)] // Future layout calculations
    pub storage_rows: usize,
}

/// Progress bar layout configuration
#[derive(Debug, Clone)]
#[allow(dead_code)] // Future progress bar layout architecture
pub struct ProgressBarLayout {
    pub bar_width: usize,
    pub left_padding: usize,
    pub right_padding: usize,
    pub separator_width: usize,
    pub total_bars: usize,
}

/// Table column specification
#[derive(Debug, Clone)]
#[allow(dead_code)] // Future table layout architecture
pub struct ColumnSpec {
    pub name: &'static str,
    pub min_width: usize,
    pub weight: f32, // Relative weight for extra space distribution
}

#[allow(dead_code)] // Future table layout architecture
impl ColumnSpec {
    pub fn new(name: &'static str, min_width: usize, weight: f32) -> Self {
        Self {
            name,
            min_width,
            weight,
        }
    }
}

/// Predefined column specifications for common tables
#[allow(dead_code)] // Future table layout architecture
pub struct StandardColumns;

#[allow(dead_code)] // Future table layout architecture
impl StandardColumns {
    pub fn process_table() -> Vec<ColumnSpec> {
        vec![
            ColumnSpec::new("PID", 6, 0.5),
            ColumnSpec::new("User", 12, 1.0),
            ColumnSpec::new("Name", 8, 2.0),
            ColumnSpec::new("CPU%", 6, 0.5),
            ColumnSpec::new("Mem%", 8, 0.5),
            ColumnSpec::new("GPU Mem", 8, 1.0),
            ColumnSpec::new("State", 8, 0.5),
            ColumnSpec::new("Command", 10, 3.0),
        ]
    }

    pub fn device_table() -> Vec<ColumnSpec> {
        vec![
            ColumnSpec::new("Device", 15, 2.0),
            ColumnSpec::new("Host", 12, 1.0),
            ColumnSpec::new("Utilization", 12, 1.0),
            ColumnSpec::new("Memory", 15, 1.5),
            ColumnSpec::new("Temperature", 12, 1.0),
            ColumnSpec::new("Power", 10, 1.0),
        ]
    }
}

/// Filter `gpu_info` to the subset visible under the current tab. "All"
/// returns every GPU; a host tab returns only GPUs whose `host_id`
/// matches the tab name.
fn visible_gpus_for_tab<'a>(state: &'a AppState) -> Box<dyn Iterator<Item = &'a GpuInfo> + 'a> {
    if let Some(tab_name) = state.tabs.get(state.current_tab) {
        if tab_name == "All" {
            Box::new(state.gpu_info.iter())
        } else {
            let owned = tab_name.clone();
            Box::new(state.gpu_info.iter().filter(move |g| g.host_id == owned))
        }
    } else {
        Box::new(state.gpu_info.iter())
    }
}

/// Compute the maximum line count any visible GPU would render given the
/// current `state.gpu_info`, `state.vgpu_info`, and `state.mig_info`. Falls
/// back to the historical 2-line baseline when no GPUs are visible (empty
/// cluster, loading state) so layout math never returns 0.
pub(crate) fn max_gpu_lines_for_tab(state: &AppState) -> usize {
    max_gpu_lines_over(
        visible_gpus_for_tab(state),
        &state.vgpu_info,
        &state.mig_info,
    )
}

/// Pure helper used by [`max_gpu_lines_for_tab`] and the unit tests. Iterates
/// any GPU iterator and returns the largest [`gpu_render_line_count`] value,
/// or 2 for an empty iterator.
///
/// Builds UUID lookup maps once and uses
/// [`gpu_render_line_count_with_lookup`] so the total cost is O(G + V + M)
/// instead of the previous O(G * (V + M)) per frame.
pub(crate) fn max_gpu_lines_over<'a>(
    gpus: impl Iterator<Item = &'a GpuInfo>,
    vgpu_info: &[VgpuHostInfo],
    mig_info: &[MigGpuInfo],
) -> usize {
    let vgpu_lookup = build_vgpu_uuid_lookup(vgpu_info);
    let mig_lookup = build_mig_uuid_lookup(mig_info);
    gpus.map(|g| {
        gpu_render_line_count_with_lookup(g, vgpu_info, mig_info, &vgpu_lookup, &mig_lookup)
    })
    .max()
    .unwrap_or(2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calculate_progress_bar_layout() {
        let layout = LayoutCalculator::calculate_progress_bar_layout(40, 3, 5);

        assert_eq!(layout.bar_width, 8); // (40 - 10 padding - 4 separators) / 3 = 26 / 3 = 8
        assert_eq!(layout.left_padding, 5);
        assert_eq!(layout.right_padding, 5);
        assert_eq!(layout.separator_width, 2);
        assert_eq!(layout.total_bars, 3);
    }

    #[test]
    fn test_calculate_table_columns() {
        let specs = vec![
            ColumnSpec::new("A", 10, 1.0),
            ColumnSpec::new("B", 15, 2.0),
            ColumnSpec::new("C", 5, 0.5),
        ];

        let widths = LayoutCalculator::calculate_table_columns(50, &specs);

        // Min total: 30, separators: 2, extra: 18
        // Weight distribution: A=18*1/3.5=5, B=18*2/3.5=10, C=18*0.5/3.5=2
        assert_eq!(widths[0], 15); // 10 + 5
        assert_eq!(widths[1], 25); // 15 + 10
        assert_eq!(widths[2], 7); // 5 + 2
    }

    #[test]
    fn test_calculate_header_lines_local_vs_remote() {
        use crate::app_state::AppState;

        // Local mode: only the basic title line, no cluster/tab widgets.
        let local_state = AppState {
            is_local_mode: true,
            ..AppState::default()
        };
        let local_lines = LayoutCalculator::calculate_header_lines(&local_state);

        // Remote mode without history: title + "Cluster Overview" label +
        // dashboard card rows + tabs row.
        let mut remote_state = AppState {
            is_local_mode: false,
            ..AppState::default()
        };
        let remote_lines_no_history = LayoutCalculator::calculate_header_lines(&remote_state);

        assert!(
            local_lines < remote_lines_no_history,
            "local mode ({local_lines}) should use fewer header lines than remote mode ({remote_lines_no_history})"
        );

        // Remote mode with non-empty utilization history adds more lines.
        remote_state.utilization_history.push_back(42.0);
        let remote_lines_with_history = LayoutCalculator::calculate_header_lines(&remote_state);

        assert!(
            remote_lines_no_history < remote_lines_with_history,
            "remote mode with history ({remote_lines_with_history}) should use more header lines than without ({remote_lines_no_history})"
        );
    }

    // --- per-GPU line-count math ---

    fn make_minimal_gpu(host_id: &str, name: &str) -> GpuInfo {
        GpuInfo {
            uuid: format!("{host_id}/{name}"),
            time: String::new(),
            name: name.to_string(),
            device_type: "GPU".to_string(),
            host_id: host_id.to_string(),
            hostname: host_id.to_string(),
            instance: host_id.to_string(),
            utilization: 0.0,
            ane_utilization: 0.0,
            dla_utilization: None,
            tensorcore_utilization: None,
            temperature: 50,
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
            numa_node_id: None,
            gsp_firmware_mode: None,
            gsp_firmware_version: None,
            nvlink_remote_devices: Vec::new(),
            gpm_metrics: None,
            detail: std::collections::HashMap::new(),
        }
    }

    #[test]
    fn max_gpu_lines_over_returns_two_for_empty_iterator() {
        // No GPUs visible (e.g. cluster still loading) → fall back to the
        // historical baseline so layout math never returns 0.
        let lines = max_gpu_lines_over(std::iter::empty(), &[], &[]);
        assert_eq!(lines, 2);
    }

    #[test]
    fn max_gpu_lines_over_picks_largest_visible_gpu() {
        // A 2-line GPU and a 3-line NVIDIA GPU: the layout must size for
        // the 3-line worst case to avoid clipping the gauges.
        let plain = make_minimal_gpu("h1", "Apple M2 Pro");
        let mut nvidia = make_minimal_gpu("h2", "NVIDIA A100");
        nvidia.performance_state = Some(2);
        let gpus = [plain, nvidia];
        let lines = max_gpu_lines_over(gpus.iter(), &[], &[]);
        assert_eq!(lines, 3);
    }

    #[test]
    fn max_gpu_lines_for_tab_filters_by_host_id_in_per_host_tab() {
        use crate::app_state::AppState;
        // Tab "h2" is selected; only h2's GPU contributes to the max,
        // even though h1 has a vGPU section that would otherwise inflate
        // the count.
        let plain_h2 = make_minimal_gpu("h2", "Apple M2 Pro");
        let mut nvidia_h1 = make_minimal_gpu("h1", "NVIDIA A100");
        nvidia_h1.performance_state = Some(0);
        let state = AppState {
            tabs: vec!["All".into(), "h2".into()],
            current_tab: 1,
            gpu_info: vec![plain_h2, nvidia_h1],
            ..AppState::default()
        };
        let lines = LayoutCalculator::max_gpu_lines_for_tab(&state);
        assert_eq!(lines, 2, "h1's NVIDIA row must not affect h2's tab");
    }

    #[test]
    fn max_gpu_lines_for_tab_uses_all_gpus_under_all_tab() {
        use crate::app_state::AppState;
        let plain_h2 = make_minimal_gpu("h2", "Apple M2 Pro");
        let mut nvidia_h1 = make_minimal_gpu("h1", "NVIDIA A100");
        nvidia_h1.performance_state = Some(0);
        let state = AppState {
            tabs: vec!["All".into(), "h2".into()],
            current_tab: 0,
            gpu_info: vec![plain_h2, nvidia_h1],
            ..AppState::default()
        };
        let lines = LayoutCalculator::max_gpu_lines_for_tab(&state);
        assert_eq!(lines, 3, "All tab must surface the worst-case GPU height");
    }
}
