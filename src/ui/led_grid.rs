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

//! Compact per-node LED grid for remote mode.
//!
//! Renders a grid of status dots to the right of the Cluster Overview cards.
//! Each node is represented by a single character colored by health/utilization:
//!
//! | Symbol | Meaning                  |
//! |--------|--------------------------|
//! | `●`    | Selected node (tab)      |
//! | `○`    | Connected node            |
//! | `⊗`    | Disconnected node         |
//!
//! Color follows `ThemeConfig::utilization_color()`: DarkGrey < 20%, Green
//! 20-50%, Yellow 50-80%, Red > 80%.
//!
//! The grid wraps into multiple rows when node count exceeds available columns,
//! capped at the allotted height (typically 4 rows matching the dashboard card
//! height).

use std::collections::HashMap;
use std::io::Write;

use crossterm::style::Color;

use crate::app_state::AppState;
use crate::common::config::ThemeConfig;
use crate::ui::text::print_colored_text;

/// Maximum number of display rows the LED grid may occupy.
const MAX_GRID_ROWS: usize = 4;

/// Minimum width (in columns) to attempt rendering the LED grid.
/// Below this threshold we skip the grid entirely to avoid clutter.
const MIN_GRID_WIDTH: usize = 4;

/// Information about a single node for LED rendering.
struct NodeLed {
    color: Color,
    symbol: char,
}

/// Render the per-node LED grid into the given line buffers.
///
/// Each entry in the returned `Vec` is a pre-formatted ANSI string
/// representing one row of LED dots (no trailing `\r\n`).
///
/// # Arguments
/// - `state`: application state with `tabs`, `connection_status`, `gpu_info`.
/// - `grid_width`: available horizontal character cells for the grid.
/// - `max_rows`: maximum number of rows the grid may occupy.
///
/// Returns an empty `Vec` when there are no remote nodes or the grid
/// width is too narrow.
pub fn render_led_grid_lines(state: &AppState, grid_width: usize, max_rows: usize) -> Vec<String> {
    if state.is_local_mode || grid_width < MIN_GRID_WIDTH {
        return Vec::new();
    }

    // Collect nodes: tabs[1..] are the remote host addresses (skip "All")
    let nodes: Vec<&String> = state.tabs.iter().skip(1).collect();
    if nodes.is_empty() {
        return Vec::new();
    }

    // Calculate per-node GPU utilization keyed by host address
    let node_utils = compute_node_utils(state, &nodes);

    // Build LED data for each node
    let leds: Vec<NodeLed> = nodes
        .iter()
        .enumerate()
        .map(|(idx, node)| {
            let util = node_utils.get(*node).copied().unwrap_or(0.0);
            let is_connected = state
                .connection_status
                .get(*node)
                .map(|s| s.is_connected)
                .unwrap_or(false);
            let is_selected = state.current_tab == idx + 1;
            node_led(util, is_selected, is_connected)
        })
        .collect();

    // Layout: how many nodes per row, capped by max_rows
    let nodes_per_row = grid_width.max(1);
    let effective_max_rows = max_rows.min(MAX_GRID_ROWS);
    let total_rows = leds.len().div_ceil(nodes_per_row).min(effective_max_rows);

    let mut lines = Vec::with_capacity(total_rows);
    for row in 0..total_rows {
        let start = row * nodes_per_row;
        let end = (start + nodes_per_row).min(leds.len());
        if start >= leds.len() {
            // Remaining rows are empty padding
            lines.push(" ".repeat(grid_width));
            continue;
        }
        let row_leds = &leds[start..end];
        let mut buf: Vec<u8> = Vec::with_capacity(grid_width * 4);
        for led in row_leds {
            print_colored_text(&mut buf, &led.symbol.to_string(), led.color, None, None);
        }
        // Pad remaining width with spaces
        let used = row_leds.len();
        if used < grid_width {
            print_colored_text(
                &mut buf,
                &" ".repeat(grid_width - used),
                Color::White,
                None,
                None,
            );
        }
        lines.push(String::from_utf8_lossy(&buf).into_owned());
    }
    lines
}

/// Compute average GPU utilization per node (host address).
///
/// Uses a single O(G) pass over all GPUs instead of O(N*G) nested filtering.
fn compute_node_utils(state: &AppState, _nodes: &[&String]) -> HashMap<String, f64> {
    // Accumulate (sum, count) per host_id in one pass over gpu_info.
    let mut accum: HashMap<&str, (f64, usize)> = HashMap::with_capacity(state.gpu_info.len());
    for gpu in &state.gpu_info {
        let entry = accum.entry(gpu.host_id.as_str()).or_insert((0.0, 0));
        entry.0 += gpu.utilization;
        entry.1 += 1;
    }
    accum
        .into_iter()
        .map(|(host, (sum, count))| (host.to_string(), sum / count as f64))
        .collect()
}

/// Determine LED symbol and color for a single node.
fn node_led(utilization: f64, is_selected: bool, is_connected: bool) -> NodeLed {
    if !is_connected {
        return NodeLed {
            color: Color::DarkGrey,
            symbol: '\u{2297}', // ⊗
        };
    }
    let color = ThemeConfig::utilization_color(utilization);
    let symbol = if is_selected {
        '\u{25CF}' // ●
    } else {
        '\u{25CB}' // ○
    };
    NodeLed { color, symbol }
}

/// Draw LED grid lines directly to a writer, right-padded to `total_width`.
///
/// This is a convenience wrapper used by `draw_system_view()` to interleave
/// LED grid rows with the dashboard card rows.
pub fn write_led_row<W: Write>(
    stdout: &mut W,
    grid_lines: &[String],
    row: usize,
    total_width: usize,
) {
    if row < grid_lines.len() {
        // Grid line already contains ANSI escapes; write directly
        stdout.write_all(grid_lines[row].as_bytes()).unwrap();
    } else {
        // Empty padding for rows beyond the grid
        print_colored_text(stdout, &" ".repeat(total_width), Color::White, None, None);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_state::{AppState, ConnectionStatus};
    use crate::device::GpuInfo;
    use std::collections::HashMap;

    fn make_remote_state(node_count: usize) -> AppState {
        let mut state = AppState::new();
        state.is_local_mode = false;
        // Remote mode: tabs start with "All" followed by host addresses.
        // Replace default local-mode tabs.
        state.tabs = vec!["All".to_string()];

        for i in 0..node_count {
            let host = format!("host-{i}");
            state.tabs.push(host.clone());
            let mut cs = ConnectionStatus::new(host.clone(), format!("http://{host}:9090"));
            cs.mark_success();
            state.connection_status.insert(host.clone(), cs);
            // Add a GPU per node
            state.gpu_info.push(GpuInfo {
                uuid: format!("gpu-{i}"),
                time: String::new(),
                name: "Test GPU".to_string(),
                device_type: "GPU".to_string(),
                host_id: host.clone(),
                hostname: host.clone(),
                instance: host,
                utilization: (i as f64 * 10.0) % 100.0,
                ane_utilization: 0.0,
                dla_utilization: None,
                tensorcore_utilization: None,
                temperature: 50,
                used_memory: 1024,
                total_memory: 8192,
                frequency: 1500,
                power_consumption: 150.0,
                gpu_core_count: None,
                detail: HashMap::new(),
            });
        }
        state.current_tab = 0;
        state
    }

    #[test]
    fn test_led_grid_empty_local_mode() {
        let mut state = AppState::new();
        state.is_local_mode = true;
        let lines = render_led_grid_lines(&state, 20, 4);
        assert!(lines.is_empty());
    }

    #[test]
    fn test_led_grid_empty_no_nodes() {
        let mut state = AppState::new();
        state.is_local_mode = false;
        // Remote mode with only "All" tab, no host nodes
        state.tabs = vec!["All".to_string()];
        let lines = render_led_grid_lines(&state, 20, 4);
        assert!(lines.is_empty());
    }

    #[test]
    fn test_led_grid_narrow_width() {
        let state = make_remote_state(10);
        let lines = render_led_grid_lines(&state, 2, 4);
        assert!(lines.is_empty());
    }

    #[test]
    fn test_led_grid_single_node() {
        let state = make_remote_state(1);
        let lines = render_led_grid_lines(&state, 20, 4);
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn test_led_grid_wraps_into_rows() {
        let state = make_remote_state(30);
        let lines = render_led_grid_lines(&state, 10, 4);
        // 30 nodes / 10 per row = 3 rows, within max 4
        assert_eq!(lines.len(), 3);
    }

    #[test]
    fn test_led_grid_caps_at_max_rows() {
        let state = make_remote_state(200);
        let lines = render_led_grid_lines(&state, 10, 4);
        // 200 nodes / 10 per row = 20 rows, capped to 4
        assert_eq!(lines.len(), 4);
    }

    #[test]
    fn test_led_grid_128_nodes_fits() {
        let state = make_remote_state(128);
        // Wide terminal: 80 columns available for the grid
        let lines = render_led_grid_lines(&state, 80, 4);
        // 128 / 80 = 2 rows
        assert_eq!(lines.len(), 2);
        // Verify no panic
        for line in &lines {
            assert!(!line.is_empty());
        }
    }

    #[test]
    fn test_led_grid_disconnected_node() {
        let mut state = make_remote_state(3);
        // Mark second node as disconnected
        if let Some(cs) = state.connection_status.get_mut("host-1") {
            cs.mark_failure("timeout".to_string());
        }
        let lines = render_led_grid_lines(&state, 20, 4);
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn test_node_led_connected_selected() {
        let led = node_led(50.0, true, true);
        assert_eq!(led.symbol, '\u{25CF}');
    }

    #[test]
    fn test_node_led_connected_unselected() {
        let led = node_led(50.0, false, true);
        assert_eq!(led.symbol, '\u{25CB}');
    }

    #[test]
    fn test_node_led_disconnected() {
        let led = node_led(50.0, false, false);
        assert_eq!(led.symbol, '\u{2297}');
        assert_eq!(led.color, Color::DarkGrey);
    }
}
