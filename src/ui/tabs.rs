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
    queue,
    style::{Color, Print},
};
use std::io::Write;

use crate::app_state::AppState;
use crate::ui::text::print_colored_text;

pub fn draw_tabs<W: Write>(stdout: &mut W, state: &AppState, cols: u16) {
    // Print tabs
    let mut labels: Vec<(String, Color)> = Vec::new();

    // Calculate available width for tabs
    // Reserve space for "Tabs: " prefix (6 chars) plus some padding
    let mut available_width = cols.saturating_sub(8);

    // Always show "All" tab first (index 0)
    if !state.tabs.is_empty() {
        let all_tab = &state.tabs[0];
        let tab_width = all_tab.len() as u16 + 2; // Tab name + 2 spaces padding

        if available_width >= tab_width {
            if state.current_tab == 0 {
                labels.push((format!(" {all_tab} "), Color::Black));
            } else {
                labels.push((format!(" {all_tab} "), Color::White));
            }
            available_width -= tab_width;
        }
    }

    // Show node tabs starting from scroll offset (skip "All" tab at index 0)
    let node_tabs: Vec<_> = state
        .tabs
        .iter()
        .enumerate()
        .skip(1) // Skip "All" tab
        .skip(state.tab_scroll_offset)
        .collect();

    for (i, tab) in node_tabs {
        // Get display name (instance name) while keeping tab as the key
        let display_name = if tab == "All" {
            tab.to_string()
        } else if let Some(connection_status) = state.connection_status.get(tab) {
            connection_status
                .actual_hostname
                .as_ref()
                .unwrap_or(tab)
                .clone()
        } else {
            tab.to_string()
        };

        let tab_width = display_name.len() as u16 + 2; // Display name + 2 spaces padding
        if available_width < tab_width {
            break; // No more space
        }

        // Determine color based on connection status and selection
        let color = if state.current_tab == i {
            Color::Black // Selected tab (will get blue background)
        } else {
            // Check if this tab represents a disconnected node
            let is_connected = if tab != "All" {
                state
                    .connection_status
                    .get(tab)
                    .map(|status| status.is_connected)
                    .unwrap_or(true) // Default to connected for local mode
            } else {
                true // "All" tab is always "connected"
            };

            if is_connected {
                Color::White // Connected: normal white text
            } else {
                Color::DarkGrey // Disconnected: dimmed grey text
            }
        };

        labels.push((format!(" {display_name} "), color));

        available_width -= tab_width;
    }

    // Render tabs
    render_tab_labels(stdout, labels);
    render_tab_separator(stdout, cols);
}

fn render_tab_labels<W: Write>(stdout: &mut W, labels: Vec<(String, Color)>) {
    queue!(stdout, Print("Tabs: ")).unwrap();
    for (text, color) in labels {
        if color == Color::Black {
            // Selected tab: white text on blue background for good visibility
            print_colored_text(stdout, &text, Color::White, Some(Color::Blue), None);
        } else {
            print_colored_text(stdout, &text, color, None, None);
        }
    }
    queue!(stdout, Print("\r\n")).unwrap();
}

fn render_tab_separator<W: Write>(stdout: &mut W, cols: u16) {
    // Print separator
    let separator = "─".repeat(cols as usize);
    print_colored_text(stdout, &separator, Color::DarkGrey, None, None);
    queue!(stdout, Print("\r\n")).unwrap();
}

#[allow(dead_code)]
pub fn calculate_tab_visibility(state: &AppState, cols: u16) -> TabVisibility {
    let mut available_width = cols.saturating_sub(8);

    // Reserve space for "All" tab (always visible)
    if !state.tabs.is_empty() {
        let all_tab_width = state.tabs[0].len() as u16 + 2;
        available_width = available_width.saturating_sub(all_tab_width);
    }

    // Calculate visible node tabs (skip "All" tab)
    let mut last_visible_node_tab = state.tab_scroll_offset;

    for (node_index, tab) in state
        .tabs
        .iter()
        .enumerate()
        .skip(1)
        .skip(state.tab_scroll_offset)
    {
        // Get display name for width calculation
        let display_name = if let Some(connection_status) = state.connection_status.get(tab) {
            connection_status
                .actual_hostname
                .as_ref()
                .unwrap_or(tab)
                .clone()
        } else {
            tab.to_string()
        };
        let tab_width = display_name.len() as u16 + 2;
        if available_width < tab_width {
            break;
        }
        available_width -= tab_width;
        last_visible_node_tab = node_index - 1; // Convert to node tab index
    }

    TabVisibility {
        first_visible: state.tab_scroll_offset,
        last_visible: last_visible_node_tab + 1, // Convert back to absolute tab index
        has_more_left: state.tab_scroll_offset > 0,
        has_more_right: last_visible_node_tab + 1 < state.tabs.len() - 1,
    }
}

#[allow(dead_code)]
pub struct TabVisibility {
    pub first_visible: usize,
    pub last_visible: usize,
    pub has_more_left: bool,
    pub has_more_right: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, VecDeque};

    fn create_test_state() -> AppState {
        AppState {
            gpu_info: Vec::new(),
            cpu_info: Vec::new(),
            memory_info: Vec::new(),
            process_info: Vec::new(),
            chassis_info: Vec::new(),
            selected_process_index: 0,
            start_index: 0,
            sort_criteria: crate::app_state::SortCriteria::Default,
            sort_direction: crate::app_state::SortDirection::Descending,
            loading: false,
            startup_status_lines: Vec::new(),
            tabs: vec![
                "All".to_string(),
                "host1".to_string(),
                "host2".to_string(),
                "host3".to_string(),
            ],
            current_tab: 0,
            gpu_scroll_offset: 0,
            storage_scroll_offset: 0,
            tab_scroll_offset: 0,
            process_horizontal_scroll_offset: 0,
            device_name_scroll_offsets: HashMap::new(),
            host_id_scroll_offsets: HashMap::new(),
            cpu_name_scroll_offsets: HashMap::new(),
            frame_counter: 0,
            storage_info: Vec::new(),
            show_help: false,
            show_per_core_cpu: false,
            utilization_history: VecDeque::new(),
            memory_history: VecDeque::new(),
            temperature_history: VecDeque::new(),
            cpu_utilization_history: VecDeque::new(),
            system_memory_history: VecDeque::new(),
            cpu_temperature_history: VecDeque::new(),
            notifications: crate::ui::notification::NotificationManager::new(),
            nvml_notification_shown: false,
            #[cfg(target_os = "linux")]
            tenstorrent_notification_shown: false,
            #[cfg(target_os = "linux")]
            tpu_notification_shown: false,
            connection_status: HashMap::new(),
            known_hosts: Vec::new(),
            hostname_to_host_id: HashMap::new(),
            is_local_mode: false, // Test state assumes remote mode
            runtime_environment: crate::utils::RuntimeEnvironment::detect(),
            data_version: 0,
            gpu_filter_enabled: false,
        }
    }

    #[test]
    fn test_tab_visibility_calculation() {
        let state = create_test_state();
        let visibility = calculate_tab_visibility(&state, 80);

        assert_eq!(visibility.first_visible, 0);
        assert!(!visibility.has_more_left);
        assert!(!visibility.has_more_right || state.tabs.len() > 4);
    }

    #[test]
    fn test_tab_visibility_with_scroll() {
        let mut state = create_test_state();
        state.tab_scroll_offset = 1;
        let visibility = calculate_tab_visibility(&state, 80);

        assert_eq!(visibility.first_visible, 1);
        assert!(visibility.has_more_left);
    }
}
