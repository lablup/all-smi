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

use std::io::Write;

use crossterm::{queue, style::Color, style::Print};

use crate::app_state::AppState;
use crate::ui::buffer::BufferWriter;
use crate::ui::led_grid;
use crate::ui::remote_sparkline_panel;
use crate::ui::text::{format_ram_value, print_colored_text};

pub fn draw_system_view<W: Write>(stdout: &mut W, state: &AppState, cols: u16) {
    let box_width = (cols as usize).min(80);

    // Calculate cluster statistics
    let is_local_mode = state.is_local_mode;
    let total_nodes = if is_local_mode {
        1 // Local mode has 1 node
    } else {
        state.tabs.len().saturating_sub(1) // Exclude "All" tab in remote mode
    };
    let live_nodes = if is_local_mode {
        1 // Local node is always considered live
    } else {
        state
            .connection_status
            .values()
            .filter(|status| status.is_connected)
            .count()
    };
    let total_gpus = state.gpu_info.len();

    // Check if we're on Apple Silicon. The native reader writes the
    // architecture detail under the lowercase "architecture" key
    // (see device/readers/apple_silicon_native.rs); previously this lookup
    // used "Architecture" and never matched, so the special-case path
    // (thermal pressure display, unified memory totals, etc.) was dead.
    let is_apple_silicon = state.gpu_info.iter().any(|gpu| {
        gpu.detail
            .get("architecture")
            .map(|arch| arch == "Apple Silicon")
            .unwrap_or(false)
    });

    // Calculate GPU cores/count based on mode
    // - Remote mode: show number of GPUs in the cluster
    // - Local Apple Silicon: show actual GPU core count
    // - Local non-Apple Silicon: show number of GPUs
    let gpu_cores_display = if !is_local_mode {
        // Remote mode: show total number of GPUs
        total_gpus
    } else if is_apple_silicon {
        // Local Apple Silicon: show actual GPU core count
        state
            .gpu_info
            .iter()
            .map(|gpu| gpu.gpu_core_count.unwrap_or(0) as usize)
            .sum::<usize>()
    } else {
        // Local non-Apple Silicon: show number of GPUs
        total_gpus
    };

    let total_memory_gb = if is_apple_silicon {
        // Use system RAM for Apple Silicon
        state
            .memory_info
            .iter()
            .map(|memory| memory.total_bytes)
            .sum::<u64>() as f64
            / (1024.0 * 1024.0 * 1024.0)
    } else {
        // Use GPU memory for other platforms
        state
            .gpu_info
            .iter()
            .map(|gpu| gpu.total_memory)
            .sum::<u64>() as f64
            / (1024.0 * 1024.0 * 1024.0)
    };

    // Calculate total power
    // For Apple Silicon: use combined power (CPU + GPU + ANE) from native metrics
    // For other platforms: sum GPU power consumption
    let total_power_watts = if is_apple_silicon {
        // Try to get combined power from GPU detail (set by native metrics manager)
        state
            .gpu_info
            .iter()
            .filter_map(|gpu| {
                gpu.detail
                    .get("combined_power_mw")
                    .and_then(|s| s.parse::<f64>().ok())
                    .map(|mw| mw / 1000.0) // Convert mW to W
            })
            .next() // Only one GPU entry for Apple Silicon
            .unwrap_or_else(|| {
                // Fallback to GPU power if combined power not available
                state
                    .gpu_info
                    .iter()
                    .map(|gpu| gpu.power_consumption)
                    .sum::<f64>()
            })
    } else {
        state
            .gpu_info
            .iter()
            .map(|gpu| gpu.power_consumption)
            .sum::<f64>()
    };

    // Calculate total CPU cores
    let total_cpu_cores = state
        .cpu_info
        .iter()
        .map(|cpu| {
            if let Some(apple_info) = &cpu.apple_silicon_info {
                apple_info.p_core_count + apple_info.e_core_count
            } else {
                cpu.total_cores
            }
        })
        .sum::<u32>();

    // Calculate total system memory
    let total_system_memory_gb = state
        .memory_info
        .iter()
        .map(|memory| memory.total_bytes)
        .sum::<u64>() as f64
        / (1024.0 * 1024.0 * 1024.0);

    let used_system_memory_gb = state
        .memory_info
        .iter()
        .map(|memory| memory.used_bytes)
        .sum::<u64>() as f64
        / (1024.0 * 1024.0 * 1024.0);

    // Calculate averages
    let avg_utilization = if total_gpus > 0 {
        state
            .gpu_info
            .iter()
            .map(|gpu| gpu.utilization)
            .sum::<f64>()
            / total_gpus as f64
    } else {
        0.0
    };

    // Average GPU temperature in °C — shown identically on every platform.
    // On Apple Silicon, gpu.temperature is sourced from the SMC CPU die sensor
    // (CPU/GPU share the same SoC die), so this is a real die temperature
    // rather than the qualitative thermal-pressure text we used to show.
    let avg_temperature = if total_gpus > 0 {
        state
            .gpu_info
            .iter()
            .map(|gpu| gpu.temperature as f64)
            .sum::<f64>()
            / total_gpus as f64
    } else {
        0.0
    };
    let avg_temperature_display = format!("{avg_temperature:.0}°C");

    // The second-row temperature cell is platform-dependent:
    // - On Apple Silicon (single SoC die) standard deviation is meaningless,
    //   so we surface NSProcessInfo's thermal pressure level instead — that
    //   qualitative reading is the only OS-blessed thermal hint Apple exposes.
    // - On multi-GPU platforms we keep the cross-GPU temperature spread.
    let (temp_secondary_label, temp_secondary_display) = if is_apple_silicon && total_gpus > 0 {
        let thermal_pressure = state
            .gpu_info
            .first()
            .and_then(|gpu| gpu.detail.get("thermal_pressure"))
            .cloned()
            .unwrap_or_else(|| "Unknown".to_string());
        ("Thermal", thermal_pressure)
    } else {
        let temp_std_dev = if total_gpus > 1 {
            let temp_variance = state
                .gpu_info
                .iter()
                .map(|gpu| {
                    let diff = gpu.temperature as f64 - avg_temperature;
                    diff * diff
                })
                .sum::<f64>()
                / (total_gpus - 1) as f64;
            temp_variance.sqrt()
        } else {
            0.0
        };
        ("Temp. Stdev", format!("±{temp_std_dev:.1}°C"))
    };

    let avg_power = if total_gpus > 0 {
        total_power_watts / total_gpus as f64
    } else {
        0.0
    };

    // Calculate used GPU memory in GB
    let used_gpu_memory_gb = if is_apple_silicon {
        // Use system RAM for Apple Silicon
        state
            .memory_info
            .iter()
            .map(|memory| memory.used_bytes)
            .sum::<u64>() as f64
            / (1024.0 * 1024.0 * 1024.0)
    } else {
        // Use GPU memory for other platforms
        state
            .gpu_info
            .iter()
            .map(|gpu| gpu.used_memory)
            .sum::<u64>() as f64
            / (1024.0 * 1024.0 * 1024.0)
    };

    // Render dashboard card rows into a buffer so we can interleave with
    // the LED grid in remote mode.
    let card_lines = {
        let mut buf = BufferWriter::new();

        // First row: | Nodes | Total RAM | GPU Cores | Total GPU RAM | Avg. Temp | Total Power |
        print_dashboard_row(
            &mut buf,
            &[
                (
                    "Nodes",
                    format!("{live_nodes}/{total_nodes}"),
                    Color::Yellow,
                ),
                (
                    "Total RAM",
                    format_ram_value(total_system_memory_gb),
                    Color::Green,
                ),
                ("GPU Cores", format!("{gpu_cores_display}"), Color::Cyan),
                ("Total VRAM", format_ram_value(total_memory_gb), Color::Blue),
                ("Avg. Temp", avg_temperature_display, Color::Magenta),
                (
                    "Total Power",
                    format!("{:.1}kW", total_power_watts / 1000.0),
                    Color::Red,
                ),
            ],
            box_width,
        );

        // Second row: | CPU Cores | Used RAM | GPU Util | Used GPU RAM | Temp Stdev | Avg. Power |
        print_dashboard_row(
            &mut buf,
            &[
                ("CPU Cores", format!("{total_cpu_cores}"), Color::Cyan),
                (
                    "Used RAM",
                    format_ram_value(used_system_memory_gb),
                    Color::Green,
                ),
                ("GPU Util", format!("{avg_utilization:.1}%"), Color::Blue),
                (
                    "Used VRAM",
                    format_ram_value(used_gpu_memory_gb),
                    Color::Blue,
                ),
                (temp_secondary_label, temp_secondary_display, Color::Magenta),
                ("Avg. Power", format!("{avg_power:.1}W"), Color::Red),
            ],
            box_width,
        );

        let raw = buf.get_buffer().to_string();
        raw.split("\r\n")
            .filter(|line| !line.is_empty())
            .map(String::from)
            .collect::<Vec<_>>()
    };

    // In remote mode, render LED grid beside the dashboard cards
    if !is_local_mode {
        // Dashboard cards occupy ~85 columns (1 border + 6 * 14 chars).
        // LED grid fills the remaining width.
        const CARD_COLUMNS: usize = 85;
        let grid_gap = 2; // space between cards and grid
        let grid_width = (cols as usize).saturating_sub(CARD_COLUMNS + grid_gap);
        let grid_lines = led_grid::render_led_grid_lines(state, grid_width, card_lines.len());

        for (i, card_line) in card_lines.iter().enumerate() {
            stdout.write_all(card_line.as_bytes()).unwrap();
            // Gap between cards and grid
            print_colored_text(stdout, &" ".repeat(grid_gap), Color::White, None, None);
            led_grid::write_led_row(stdout, &grid_lines, i, grid_width);
            queue!(stdout, Print("\r\n")).unwrap();
        }
    } else {
        // Local mode: just output the card lines as-is
        for card_line in &card_lines {
            stdout.write_all(card_line.as_bytes()).unwrap();
            queue!(stdout, Print("\r\n")).unwrap();
        }
    }
}

pub fn draw_dashboard_items<W: Write>(stdout: &mut W, state: &AppState, cols: u16) {
    // Print separator
    let separator = "\u{2500}".repeat(cols as usize);
    print_colored_text(stdout, &separator, Color::DarkGrey, None, None);
    queue!(stdout, Print("\r\n")).unwrap();

    // Remote mode: full-width braille sparkline panel replaces the old
    // split node-list + bar-chart layout.
    remote_sparkline_panel::draw_remote_sparkline_panel(stdout, state, cols);
}

fn print_dashboard_row<W: Write>(
    stdout: &mut W,
    items: &[(&str, String, Color)],
    _total_width: usize,
) {
    const ITEM_WIDTH: usize = 15; // Fixed width for each dashboard item

    // Print labels row
    print_colored_text(stdout, "│", Color::DarkGrey, None, None);
    for (label, _, color) in items {
        // Truncate label if too long, ensuring it fits in 15 characters minus padding and separator
        let max_label_len = ITEM_WIDTH.saturating_sub(3);
        let truncated_label = if label.len() > max_label_len {
            &label[..max_label_len]
        } else {
            label
        };
        let formatted_label = format!(" {truncated_label:<max_label_len$}");
        print_colored_text(stdout, &formatted_label, *color, None, None);
        print_colored_text(stdout, "│", Color::DarkGrey, None, None);
    }
    queue!(stdout, Print("\r\n")).unwrap();

    // Print values row
    print_colored_text(stdout, "│", Color::DarkGrey, None, None);
    for (_, value, _) in items {
        // Truncate value if too long, ensuring it fits in 15 characters minus padding and separator
        let max_value_len = ITEM_WIDTH.saturating_sub(3);
        let truncated_value = if value.len() > max_value_len {
            &value[..max_value_len]
        } else {
            value
        };
        let formatted_value = format!(" {truncated_value:<max_value_len$}");
        print_colored_text(stdout, &formatted_value, Color::White, None, None);
        print_colored_text(stdout, "│", Color::DarkGrey, None, None);
    }
    queue!(stdout, Print("\r\n")).unwrap();
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

    fn make_local_state() -> AppState {
        let mut state = AppState::new();
        state.is_local_mode = true;
        state
    }

    fn make_remote_state(node_count: usize) -> AppState {
        let mut state = AppState::new();
        state.is_local_mode = false;
        state.tabs = vec!["All".to_string()];
        for i in 0..node_count {
            let host = format!("host-{i}");
            state.tabs.push(host.clone());
            let mut cs = ConnectionStatus::new(host.clone(), format!("http://{host}:9090"));
            cs.mark_success();
            state.connection_status.insert(host.clone(), cs);
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
                temperature: 60,
                used_memory: 2048,
                total_memory: 8192,
                frequency: 1500,
                power_consumption: 200.0,
                gpu_core_count: None,
                temperature_threshold_slowdown: None,
                temperature_threshold_shutdown: None,
                temperature_threshold_max_operating: None,
                temperature_threshold_acoustic: None,
                performance_state: None,
                detail: HashMap::new(),
            });
        }
        state.current_tab = 0;
        state
    }

    #[test]
    fn test_draw_system_view_local_does_not_panic() {
        let state = make_local_state();
        let mut buf: Vec<u8> = Vec::new();
        draw_system_view(&mut buf, &state, 160);
        assert!(!buf.is_empty());
    }

    #[test]
    fn test_draw_system_view_remote_does_not_panic() {
        let state = make_remote_state(8);
        let mut buf: Vec<u8> = Vec::new();
        draw_system_view(&mut buf, &state, 160);
        assert!(!buf.is_empty());
    }

    #[test]
    fn test_draw_system_view_remote_narrow_terminal() {
        // Narrow terminal: grid_width will be zero or negative — no panic.
        let state = make_remote_state(4);
        let mut buf: Vec<u8> = Vec::new();
        draw_system_view(&mut buf, &state, 80);
        assert!(!buf.is_empty());
    }

    #[test]
    fn test_draw_dashboard_items_does_not_panic() {
        let state = make_remote_state(2);
        let mut buf: Vec<u8> = Vec::new();
        draw_dashboard_items(&mut buf, &state, 160);
        // Should at least write the separator
        assert!(!buf.is_empty());
    }
}
