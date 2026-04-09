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
// use crate::common::config::AppConfig;

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

        if !state.is_local_mode {
            // "Cluster Overview" label line
            lines += 1;

            // System overview dashboard card (2 rows) + label separator row
            lines += 4;

            // Live statistics section
            if !state.utilization_history.is_empty() {
                lines += 5; // Header + 3 history lines + separator
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

        let available_rows = rows
            .saturating_sub(header_lines)
            .saturating_sub(function_keys_lines);

        ContentArea {
            x: 0,
            y: header_lines,
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

        let lines_per_gpu = 2; // Each GPU takes 2 lines
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
}
