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

use std::collections::HashMap;
use std::io::Write;

use crossterm::{queue, style::Color, style::Print};

use crate::app_state::AppState;
use crate::common::config::ThemeConfig;
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

    // Check if we're on Apple Silicon
    let is_apple_silicon = state.gpu_info.iter().any(|gpu| {
        gpu.detail
            .get("Architecture")
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

    // For Apple Silicon, get thermal pressure text; for others, calculate numeric temperature
    let (avg_temperature_display, temp_std_dev_display) = if is_apple_silicon && total_gpus > 0 {
        // Get the thermal pressure text from the first GPU (they should all be the same on a single machine)
        let thermal_pressure = state
            .gpu_info
            .first()
            .and_then(|gpu| gpu.detail.get("thermal_pressure"))
            .cloned()
            .unwrap_or_else(|| "Unknown".to_string());
        (thermal_pressure, "N/A".to_string())
    } else {
        // Calculate numeric temperature for non-Apple Silicon
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

        // Calculate temperature standard deviation
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

        (
            format!("{avg_temperature:.0}°C"),
            format!("±{temp_std_dev:.1}°C"),
        )
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

    // First row: | Nodes | Total RAM | GPU Cores | Total GPU RAM | Avg. Temp | Total Power |
    print_dashboard_row(
        stdout,
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

    // Second row: | CPU Cores | Used RAM | Avg. GPU Util | Used GPU RAM | Temp. Stdev | Avg. Power |
    print_dashboard_row(
        stdout,
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
            ("Temp. Stdev", temp_std_dev_display, Color::Magenta),
            ("Avg. Power", format!("{avg_power:.1}W"), Color::Red),
        ],
        box_width,
    );
}

pub fn draw_dashboard_items<W: Write>(stdout: &mut W, state: &AppState, cols: u16) {
    // Print separator
    let separator = "─".repeat(cols as usize);
    print_colored_text(stdout, &separator, Color::DarkGrey, None, None);
    queue!(stdout, Print("\r\n")).unwrap();

    // Node utilization history box
    draw_utilization_history(stdout, state, cols);
    queue!(stdout, Print("\r\n")).unwrap();
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

pub fn draw_utilization_history<W: Write>(stdout: &mut W, state: &AppState, cols: u16) {
    let box_width = (cols as usize).min(120); // Increased width to accommodate more columns

    if state.utilization_history.is_empty() && state.cpu_utilization_history.is_empty() {
        return;
    }

    // Calculate GPU averages for display
    let avg_util = if !state.utilization_history.is_empty() {
        state.utilization_history.iter().sum::<f64>() / state.utilization_history.len() as f64
    } else {
        0.0
    };
    let avg_mem = if !state.memory_history.is_empty() {
        state.memory_history.iter().sum::<f64>() / state.memory_history.len() as f64
    } else {
        0.0
    };
    let avg_temp = if !state.temperature_history.is_empty() {
        state.temperature_history.iter().sum::<f64>() / state.temperature_history.len() as f64
    } else {
        0.0
    };

    // Calculate CPU averages for display
    let avg_cpu_util = if !state.cpu_utilization_history.is_empty() {
        state.cpu_utilization_history.iter().sum::<f64>()
            / state.cpu_utilization_history.len() as f64
    } else {
        0.0
    };
    let avg_sys_mem = if !state.system_memory_history.is_empty() {
        state.system_memory_history.iter().sum::<f64>() / state.system_memory_history.len() as f64
    } else {
        0.0
    };
    let avg_cpu_temp = if !state.cpu_temperature_history.is_empty() {
        state.cpu_temperature_history.iter().sum::<f64>()
            / state.cpu_temperature_history.len() as f64
    } else {
        0.0
    };

    // Print header
    print_colored_text(stdout, "Live Statistics", Color::Cyan, None, None);
    queue!(stdout, Print("\r\n")).unwrap();

    // Layout: left half for node view, right half split between GPU and CPU stats
    let node_width = box_width / 2;
    let stats_width = box_width - node_width;

    // Determine if we have GPU data
    let has_gpu = !state.gpu_info.is_empty();

    // Always split stats_width between GPU and CPU for consistent layout
    let (gpu_width, cpu_width) = (stats_width / 2, stats_width / 2);

    let gpu_history_width = if gpu_width > 10 {
        gpu_width.saturating_sub(10)
    } else {
        0
    };
    let cpu_history_width = cpu_width.saturating_sub(10);

    // Print node view and history gauges side by side
    print_node_view_and_history(
        stdout,
        state,
        NodeViewParams {
            node_width,
            gpu_width,
            cpu_width,
            gpu_history_width,
            cpu_history_width,
            avg_util,
            avg_mem,
            avg_temp,
            avg_cpu_util,
            avg_sys_mem,
            avg_cpu_temp,
            has_gpu,
        },
    );
}

struct NodeViewParams {
    node_width: usize,
    #[allow(dead_code)]
    gpu_width: usize,
    #[allow(dead_code)]
    cpu_width: usize,
    gpu_history_width: usize,
    cpu_history_width: usize,
    avg_util: f64,
    avg_mem: f64,
    avg_temp: f64,
    avg_cpu_util: f64,
    avg_sys_mem: f64,
    avg_cpu_temp: f64,
    has_gpu: bool,
}

fn print_node_view_and_history<W: Write>(stdout: &mut W, state: &AppState, params: NodeViewParams) {
    // Get nodes (excluding "All" tab) - these are host addresses
    let nodes: Vec<&String> = state.tabs.iter().skip(1).collect();

    // Calculate per-node utilization using host addresses as keys
    let mut node_utils: HashMap<String, f64> = HashMap::new();
    for node in &nodes {
        let node_gpus: Vec<_> = state
            .gpu_info
            .iter()
            .filter(|gpu| &gpu.host_id == *node)
            .collect();
        if !node_gpus.is_empty() {
            let node_util =
                node_gpus.iter().map(|gpu| gpu.utilization).sum::<f64>() / node_gpus.len() as f64;
            node_utils.insert(node.to_string(), node_util);
        }
    }

    // Calculate node grid layout
    let nodes_per_row = params.node_width.saturating_sub(2).max(1);
    let num_rows = if nodes.is_empty() {
        1
    } else {
        ((nodes.len() - 1) / nodes_per_row) + 1
    };
    let num_rows = num_rows.min(3); // Limit to 3 rows max

    // Print each row of the combined view
    for row in 0..3 {
        if row < num_rows {
            // Print node view for this row
            print_node_view_row(
                stdout,
                NodeViewRowParams {
                    nodes: &nodes,
                    node_utils: &node_utils,
                    connection_status: &state.connection_status,
                    current_tab: state.current_tab,
                    left_width: params.node_width,
                    row,
                    nodes_per_row,
                },
            );
        } else {
            // Print empty space for this row
            print_colored_text(
                stdout,
                &" ".repeat(params.node_width),
                Color::White,
                None,
                None,
            );
        }

        // Print GPU statistics (or CPU stats as GPU if no GPU present)
        if params.has_gpu {
            match row {
                0 => {
                    print_colored_text(stdout, "GPU Util.", Color::Yellow, None, None);
                    print_history_bar_with_value(
                        stdout,
                        &state.utilization_history,
                        params.gpu_history_width,
                        100.0,
                        format!("{:3.1}%", params.avg_util),
                    );
                }
                1 => {
                    print_colored_text(stdout, "GPU Mem. ", Color::Yellow, None, None);
                    print_history_bar_with_value(
                        stdout,
                        &state.memory_history,
                        params.gpu_history_width,
                        100.0,
                        format!("{:3.1}%", params.avg_mem),
                    );
                }
                2 => {
                    print_colored_text(stdout, "GPU Temp.", Color::Yellow, None, None);
                    print_history_bar_with_value(
                        stdout,
                        &state.temperature_history,
                        params.gpu_history_width + 1, // +1 to fit the temperature value
                        100.0,
                        format!("{:3.0}°C", params.avg_temp),
                    );
                }
                _ => {}
            }
        } else {
            // No GPU, show labels with N/A values
            match row {
                0 => {
                    print_colored_text(stdout, "GPU Util.", Color::Yellow, None, None);
                    // Fill the history bar area with spaces, then show N/A
                    let padding = params.gpu_history_width.saturating_sub(5); // Reserve space for "  N/A" (5 chars like "xx.x%")
                    print_colored_text(stdout, &" ".repeat(padding), Color::DarkGrey, None, None);
                    print_colored_text(stdout, "  N/A", Color::DarkGrey, None, None);
                }
                1 => {
                    print_colored_text(stdout, "GPU Mem. ", Color::Yellow, None, None);
                    let padding = params.gpu_history_width.saturating_sub(5);
                    print_colored_text(stdout, &" ".repeat(padding), Color::DarkGrey, None, None);
                    print_colored_text(stdout, "  N/A", Color::DarkGrey, None, None);
                }
                2 => {
                    print_colored_text(stdout, "GPU Temp.", Color::Yellow, None, None);
                    let padding = params.gpu_history_width.saturating_sub(5);
                    print_colored_text(stdout, &" ".repeat(padding), Color::DarkGrey, None, None);
                    print_colored_text(stdout, "  N/A", Color::DarkGrey, None, None);
                }
                _ => {}
            }
        }

        // Add spacing between GPU and CPU stats
        print_colored_text(stdout, "  ", Color::White, None, None);

        // Print CPU statistics
        match row {
            0 => {
                print_colored_text(stdout, "CPU Util.", Color::Cyan, None, None);
                print_history_bar_with_value(
                    stdout,
                    &state.cpu_utilization_history,
                    params.cpu_history_width,
                    100.0,
                    format!("{:3.1}%", params.avg_cpu_util),
                );
            }
            1 => {
                print_colored_text(stdout, "Host Mem.", Color::Cyan, None, None);
                print_history_bar_with_value(
                    stdout,
                    &state.system_memory_history,
                    params.cpu_history_width,
                    100.0,
                    format!("{:3.1}%", params.avg_sys_mem),
                );
            }
            2 => {
                print_colored_text(stdout, "CPU Temp.", Color::Cyan, None, None);
                print_history_bar_with_value(
                    stdout,
                    &state.cpu_temperature_history,
                    params.cpu_history_width + 1, // +1 to fit the temperature value
                    100.0,
                    format!("{:3.0}°C", params.avg_cpu_temp),
                );
            }
            _ => {}
        }
        queue!(stdout, Print("\r\n")).unwrap();
    }
}

struct NodeViewRowParams<'a> {
    nodes: &'a [&'a String],
    node_utils: &'a HashMap<String, f64>,
    connection_status: &'a HashMap<String, crate::app_state::ConnectionStatus>,
    current_tab: usize,
    left_width: usize,
    row: usize,
    nodes_per_row: usize,
}

fn print_node_view_row<W: Write>(stdout: &mut W, params: NodeViewRowParams) {
    let start_idx = params.row * params.nodes_per_row;
    let row_nodes: Vec<&String> = params
        .nodes
        .iter()
        .skip(start_idx)
        .take(params.nodes_per_row)
        .copied()
        .collect();

    let mut row_chars = Vec::new();
    for (col, node) in row_nodes.iter().enumerate() {
        let util = params.node_utils.get(*node).unwrap_or(&0.0);

        // Look up connection status using the host address (node is a host address)
        let is_connected = params
            .connection_status
            .get(*node)
            .map(|status| status.is_connected)
            .unwrap_or(false);

        let (char, color) = get_node_char_and_color(
            *util,
            params.current_tab == col + 1 + start_idx,
            is_connected,
        );
        row_chars.push((char, color));
    }

    // Calculate actual display width (Unicode chars like ● take 2 display columns)
    let actual_width = row_chars.len(); // Each node character takes 1 position, regardless of Unicode width
    let padding_needed = params.left_width.saturating_sub(actual_width);

    // Print each character with its color
    for (char, color) in row_chars {
        print_colored_text(stdout, &char.to_string(), color, None, None);
    }

    // Add padding spaces
    if padding_needed > 0 {
        print_colored_text(
            stdout,
            &" ".repeat(padding_needed),
            Color::White,
            None,
            None,
        );
    }
}

fn get_node_char_and_color(
    utilization: f64,
    is_selected: bool,
    is_connected: bool,
) -> (char, Color) {
    if !is_connected {
        // Disconnected node - show circle with X in gray
        return ('⊗', Color::DarkGrey);
    }

    let base_color = ThemeConfig::utilization_color(utilization);
    let char = if is_selected { '●' } else { '○' };
    (char, base_color)
}

fn print_history_bar_with_value<W: Write>(
    stdout: &mut W,
    history: &std::collections::VecDeque<f64>,
    width: usize,
    max_value: f64,
    value_text: String,
) {
    let available_width = width.saturating_sub(value_text.len() + 1);
    if history.is_empty() || available_width == 0 {
        return;
    }
    let step = if history.len() > available_width {
        history.len() / available_width
    } else {
        1
    };

    // Sample the history based on available width and reverse for right-to-left display
    let sampled_data: Vec<f64> = history
        .iter()
        .step_by(step.max(1))
        .take(available_width)
        .copied()
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();

    // Print the bar
    for &value in &sampled_data {
        let normalized = (value / max_value).min(1.0);
        let color = ThemeConfig::progress_bar_color(normalized);

        let bar_char = if normalized > 0.875 {
            '⣿' // All 8 dots filled
        } else if normalized > 0.75 {
            '⣾' // 7 dots filled
        } else if normalized > 0.625 {
            '⣶' // 6 dots filled
        } else if normalized > 0.5 {
            '⣦' // 5 dots filled
        } else if normalized > 0.375 {
            '⣤' // 4 dots filled
        } else if normalized > 0.25 {
            '⣠' // 3 dots filled
        } else if normalized > 0.125 {
            '⣀' // 2 dots filled (bottom)
        } else if normalized > 0.0 {
            '⡀' // 1 dot filled (bottom)
        } else {
            '⠀' // Empty Braille character for zero values
        };

        print_colored_text(stdout, &bar_char.to_string(), color, None, None);
    }

    // Print remaining space as empty Braille characters (but limit to reasonable amount)
    let remaining = available_width.saturating_sub(sampled_data.len());
    if remaining > 0 && remaining < 50 {
        // Limit to prevent long horizontal lines
        print_colored_text(stdout, &"⠀".repeat(remaining), Color::DarkGrey, None, None);
    }

    // Print value
    print_colored_text(stdout, " ", Color::White, None, None);
    print_colored_text(stdout, &value_text, Color::White, None, None);
}
