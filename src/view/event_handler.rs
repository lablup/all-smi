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

use crossterm::{
    event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind},
    terminal::size,
};

use crate::app_state::{AppState, SortCriteria};
use crate::cli::ViewArgs;

pub async fn handle_key_event(key_event: KeyEvent, state: &mut AppState, args: &ViewArgs) -> bool {
    match key_event.code {
        KeyCode::Esc => {
            if state.show_help {
                state.show_help = false;
                false
            } else {
                true // Exit
            }
        }
        KeyCode::Char('q') => true, // Exit
        KeyCode::Char('1') | KeyCode::Char('h') => {
            state.show_help = !state.show_help;
            false
        }
        KeyCode::Left => {
            if !state.show_help {
                handle_left_arrow(state);
            }
            false
        }
        KeyCode::Right => {
            if !state.show_help {
                handle_right_arrow(state);
            }
            false
        }
        _ if !state.loading && !state.show_help => {
            handle_navigation_keys(key_event.code, state, args);
            false
        }
        _ => false,
    }
}

fn handle_left_arrow(state: &mut AppState) {
    // Check if we're in local mode ("All" tab + local hostname)
    if state.is_local_mode {
        // Local mode - handle horizontal scrolling for process list
        if state.process_horizontal_scroll_offset > 0 {
            state.process_horizontal_scroll_offset =
                state.process_horizontal_scroll_offset.saturating_sub(10);
        }
    } else {
        // Remote mode - handle tab switching
        if state.current_tab > 0 {
            state.current_tab -= 1;

            // If we're moving to a node tab (not "All" tab), adjust scroll if needed
            if state.current_tab > 0 {
                // Calculate which node tab index this is (subtract 1 for "All" tab)
                let node_tab_index = state.current_tab - 1;
                if node_tab_index < state.tab_scroll_offset {
                    state.tab_scroll_offset = node_tab_index;
                }
            }
            // If moving to "All" tab (index 0), no scroll adjustment needed since it's always visible
        }
        state.gpu_scroll_offset = 0;
        state.storage_scroll_offset = 0;
    }
}

fn handle_right_arrow(state: &mut AppState) {
    // Check if we're in local mode ("All" tab + local hostname)
    if state.is_local_mode {
        // Local mode - handle horizontal scrolling for process list
        state.process_horizontal_scroll_offset += 10;
    } else {
        // Remote mode - handle tab switching
        if state.current_tab < state.tabs.len() - 1 {
            state.current_tab += 1;

            // If we're moving to a node tab (not "All" tab), check if we need to scroll
            if state.current_tab > 0 {
                let (cols, _) = size().unwrap();
                let mut available_width = cols.saturating_sub(8); // Space for "Tabs: " prefix

                // Reserve space for "All" tab (always visible)
                if !state.tabs.is_empty() {
                    let all_tab_width = state.tabs[0].len() as u16 + 2;
                    available_width = available_width.saturating_sub(all_tab_width);
                }

                // Calculate which node tabs are visible starting from scroll offset
                let mut last_visible_node_tab_index = state.tab_scroll_offset;

                for (node_index, tab) in state
                    .tabs
                    .iter()
                    .enumerate()
                    .skip(1)
                    .skip(state.tab_scroll_offset)
                {
                    let tab_width = tab.len() as u16 + 2;
                    if available_width < tab_width {
                        break;
                    }
                    available_width -= tab_width;
                    last_visible_node_tab_index = node_index - 1; // Convert to node tab index (subtract 1 for "All")
                }

                // Check if current tab is a node tab and not visible
                let current_node_tab_index = state.current_tab - 1; // Convert to node tab index
                if current_node_tab_index > last_visible_node_tab_index {
                    state.tab_scroll_offset += 1;
                }
            }
            // If moving to "All" tab, no scroll adjustment needed since it's always visible
        }
        state.gpu_scroll_offset = 0;
        state.storage_scroll_offset = 0;
    }
}

fn handle_navigation_keys(key_code: KeyCode, state: &mut AppState, args: &ViewArgs) {
    match key_code {
        KeyCode::Up => handle_up_arrow(state, args),
        KeyCode::Down => handle_down_arrow(state, args),
        KeyCode::PageUp => handle_page_up(state, args),
        KeyCode::PageDown => handle_page_down(state, args),
        KeyCode::Char('p') => state.sort_criteria = SortCriteria::Pid,
        KeyCode::Char('m') => state.sort_criteria = SortCriteria::MemoryPercent,
        KeyCode::Char('u') => state.sort_criteria = SortCriteria::Utilization,
        KeyCode::Char('g') => state.sort_criteria = SortCriteria::GpuMemory,
        KeyCode::Char('d') => state.sort_criteria = SortCriteria::Default,
        KeyCode::Char('c') => state.show_per_core_cpu = !state.show_per_core_cpu,
        KeyCode::Char('f') => state.gpu_filter_enabled = !state.gpu_filter_enabled,
        _ => {}
    }
}

fn handle_up_arrow(state: &mut AppState, args: &ViewArgs) {
    let is_remote = args.hosts.is_some() || args.hostfile.is_some();
    if is_remote {
        // Unified scrolling for remote mode
        if state.gpu_scroll_offset > 0 {
            state.gpu_scroll_offset -= 1;
            state.storage_scroll_offset = 0; // Reset storage scroll when in GPU area
        } else if state.storage_scroll_offset > 0 {
            state.storage_scroll_offset -= 1;
        }
    } else {
        // Local mode - process list scrolling
        if state.selected_process_index > 0 {
            state.selected_process_index -= 1;
        }
        if state.selected_process_index < state.start_index {
            state.start_index = state.selected_process_index;
        }
    }
}

fn handle_down_arrow(state: &mut AppState, args: &ViewArgs) {
    let is_remote = args.hosts.is_some() || args.hostfile.is_some();
    if is_remote {
        // Unified scrolling for remote mode
        let gpu_count = if state.current_tab == 0 {
            state.gpu_info.len()
        } else {
            state
                .gpu_info
                .iter()
                .filter(|info| info.host_id == state.tabs[state.current_tab])
                .count()
        };

        let storage_count = if state.current_tab == 0 {
            // No storage on 'All' tab
            0
        } else {
            state
                .storage_info
                .iter()
                .filter(|info| info.host_id == state.tabs[state.current_tab])
                .count()
        };

        if state.gpu_scroll_offset < gpu_count.saturating_sub(1) {
            state.gpu_scroll_offset += 1;
            state.storage_scroll_offset = 0; // Reset storage scroll when in GPU area
        } else if state.storage_scroll_offset < storage_count.saturating_sub(1) {
            state.storage_scroll_offset += 1;
        }
    } else {
        // Local mode - process list scrolling
        if !state.process_info.is_empty()
            && state.selected_process_index < state.process_info.len() - 1
        {
            state.selected_process_index += 1;
        }
        let (_cols, rows) = size().unwrap();
        let half_rows = rows / 2;
        let visible_process_rows = half_rows.saturating_sub(1) as usize;
        if state.selected_process_index >= state.start_index + visible_process_rows {
            state.start_index = state.selected_process_index - visible_process_rows + 1;
        }
    }
}

fn handle_page_up(state: &mut AppState, args: &ViewArgs) {
    let is_remote = args.hosts.is_some() || args.hostfile.is_some();
    if is_remote {
        // Remote mode - page up through GPU list
        let (_cols, rows) = size().unwrap();
        let content_start_row = 19;
        let available_rows = rows.saturating_sub(content_start_row).saturating_sub(1) as usize;

        // Calculate storage display space for current tab
        let storage_items_count = if state.current_tab > 0 && !state.storage_info.is_empty() {
            let current_hostname = &state.tabs[state.current_tab];
            state
                .storage_info
                .iter()
                .filter(|info| info.host_id == *current_hostname)
                .count()
        } else {
            0
        };
        let storage_display_rows = if storage_items_count > 0 {
            storage_items_count + 2 // Each storage item takes 1 line (labels + bar on same line)
        } else {
            0
        };

        let gpu_display_rows = available_rows.saturating_sub(storage_display_rows);
        let lines_per_gpu = 2; // Each GPU takes 2 lines (labels + progress bars on same line)
        let max_gpu_items = gpu_display_rows / lines_per_gpu;
        let page_size = max_gpu_items.max(1); // At least 1 item per page

        state.gpu_scroll_offset = state.gpu_scroll_offset.saturating_sub(page_size);
        state.storage_scroll_offset = 0; // Reset storage scroll when paging GPU list
    } else {
        // Local mode - page up through process list
        let (_cols, rows) = size().unwrap();
        let half_rows = rows / 2;
        let page_size = half_rows.saturating_sub(1) as usize;
        state.selected_process_index = state.selected_process_index.saturating_sub(page_size);
        if state.selected_process_index < state.start_index {
            state.start_index = state.selected_process_index;
        }
    }
}

fn handle_page_down(state: &mut AppState, args: &ViewArgs) {
    let is_remote = args.hosts.is_some() || args.hostfile.is_some();
    if is_remote {
        // Remote mode - page down through GPU list
        let (_cols, rows) = size().unwrap();
        let content_start_row = 19;
        let available_rows = rows.saturating_sub(content_start_row).saturating_sub(1) as usize;

        // Calculate storage display space for current tab
        let storage_items_count = if state.current_tab > 0 && !state.storage_info.is_empty() {
            let current_hostname = &state.tabs[state.current_tab];
            state
                .storage_info
                .iter()
                .filter(|info| info.host_id == *current_hostname)
                .count()
        } else {
            0
        };
        let storage_display_rows = if storage_items_count > 0 {
            storage_items_count + 2 // Each storage item takes 1 line (labels + bar on same line)
        } else {
            0
        };

        let gpu_display_rows = available_rows.saturating_sub(storage_display_rows);
        let lines_per_gpu = 2; // Each GPU takes 2 lines (labels + progress bars on same line)
        let max_gpu_items = gpu_display_rows / lines_per_gpu;
        let page_size = max_gpu_items.max(1); // At least 1 item per page

        // Calculate total GPUs for current tab
        let total_gpus = if state.current_tab == 0 {
            state.gpu_info.len()
        } else {
            state
                .gpu_info
                .iter()
                .filter(|info| info.host_id == state.tabs[state.current_tab])
                .count()
        };

        if total_gpus > 0 {
            let max_offset = total_gpus.saturating_sub(max_gpu_items);
            state.gpu_scroll_offset = (state.gpu_scroll_offset + page_size).min(max_offset);
            state.storage_scroll_offset = 0; // Reset storage scroll when paging GPU list
        }
    } else {
        // Local mode - page down through process list
        if !state.process_info.is_empty() {
            let (_cols, rows) = size().unwrap();
            let half_rows = rows / 2;
            let page_size = half_rows.saturating_sub(1) as usize;
            state.selected_process_index =
                (state.selected_process_index + page_size).min(state.process_info.len() - 1);
            let visible_process_rows = half_rows.saturating_sub(1) as usize;
            if state.selected_process_index >= state.start_index + visible_process_rows {
                state.start_index = state.selected_process_index - visible_process_rows + 1;
            }
        }
    }
}

pub async fn handle_mouse_event(
    mouse_event: MouseEvent,
    state: &mut AppState,
    _args: &ViewArgs,
) -> bool {
    match mouse_event.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            // Only handle clicks when not in help mode and not loading
            if !state.show_help && !state.loading {
                handle_process_header_click(mouse_event.column, mouse_event.row, state);
            }
            false
        }
        _ => false,
    }
}

fn handle_process_header_click(x: u16, y: u16, state: &mut AppState) {
    // Check if we're in local mode with process list visible
    if !state.is_local_mode {
        return;
    }

    // Get terminal size to calculate process list position
    let (_cols, rows) = match size() {
        Ok((c, r)) => (c, r),
        Err(_) => return,
    };

    // Calculate where the process header should be
    // The header is at half_rows - 1 based on testing
    let half_rows = rows / 2;
    let process_header_row = half_rows - 1;

    // Check if click is on the process header row
    if y != process_header_row {
        return;
    }

    // Calculate column positions based on fixed widths
    let fixed_widths = [7, 12, 3, 3, 6, 6, 1, 5, 5, 5, 7, 8];
    let mut column_start: usize = 0;
    let mut column_index = None;

    // Account for horizontal scrolling
    let scroll_offset = state.process_horizontal_scroll_offset;

    // Find which column was clicked
    for (i, &width) in fixed_widths.iter().enumerate() {
        let column_end = column_start + width;

        // Adjust for scroll offset
        let visible_start = column_start.saturating_sub(scroll_offset) as u16;
        let visible_end = column_end.saturating_sub(scroll_offset) as u16;

        if x >= visible_start && x < visible_end {
            column_index = Some(i);
            break;
        }
        column_start = column_end + 1; // +1 for space between columns
    }

    // Map column index to sort criteria
    if let Some(idx) = column_index {
        let new_criteria = match idx {
            0 => SortCriteria::Pid,
            1 => SortCriteria::User,
            2 => SortCriteria::Priority,
            3 => SortCriteria::Nice,
            4 => SortCriteria::VirtualMemory,
            5 => SortCriteria::ResidentMemory,
            6 => SortCriteria::State,
            7 => SortCriteria::CpuPercent,
            8 => SortCriteria::MemoryPercent,
            9 => SortCriteria::GpuPercent,
            10 => SortCriteria::GpuMemoryUsage,
            11 => SortCriteria::CpuTime,
            _ => return, // Command column or beyond
        };

        // Toggle sort direction if clicking the same column
        if state.sort_criteria == new_criteria {
            state.sort_direction = match state.sort_direction {
                crate::app_state::SortDirection::Ascending => {
                    crate::app_state::SortDirection::Descending
                }
                crate::app_state::SortDirection::Descending => {
                    crate::app_state::SortDirection::Ascending
                }
            };
        } else {
            // New column, default to descending for most columns
            state.sort_criteria = new_criteria;
            state.sort_direction = match new_criteria {
                SortCriteria::User | SortCriteria::State | SortCriteria::Command => {
                    crate::app_state::SortDirection::Ascending
                }
                _ => crate::app_state::SortDirection::Descending,
            };
        }
    }
}
