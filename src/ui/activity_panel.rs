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

//! Always-on CPU per-core Activity panel for local mode.
//!
//! Renders per-core CPU utilization bars as the left half of a full-row
//! Activity panel. When core count is high, bars are automatically collapsed
//! into P/E cluster groups (Apple Silicon) or NUMA/socket groups (x86).
//!
//! When terminal width is below 80 columns, the panel is omitted entirely
//! and the caller falls back to the summary-bar-only layout.

use std::io::Write;

use crossterm::{queue, style::Color, style::Print};

use crate::device::{CoreType, CoreUtilization, CpuInfo};
use crate::ui::renderers::widgets::gauges::get_utilization_block;
use crate::ui::text::print_colored_text;
use crate::ui::widgets::draw_bar;

/// Minimum terminal width required to show the Activity panel.
/// Below this threshold the panel is omitted entirely.
const MIN_PANEL_WIDTH: u16 = 80;

/// Strategy for how to display CPU cores in the Activity panel.
#[derive(Debug, Clone, PartialEq)]
pub enum CollapseStrategy {
    /// Show individual bars for every core.
    Individual,
    /// Group cores by P/E cluster (Apple Silicon).
    PECluster,
    /// Group cores by socket / NUMA node (x86 / other).
    SocketGroup,
}

/// Determine how to display per-core CPU bars based on core count and width.
///
/// The heuristic is:
/// - If `core_count <= width / 3` (and <= 16), show individual bars.
/// - Otherwise, collapse into groups based on platform type.
pub fn core_collapse_strategy(cpu_info: &CpuInfo, width: usize) -> CollapseStrategy {
    let core_count = cpu_info.per_core_utilization.len();
    let collapse_threshold = (width / 3).min(16);

    if core_count <= collapse_threshold {
        return CollapseStrategy::Individual;
    }

    // High core count: group by platform type
    if cpu_info.apple_silicon_info.is_some() {
        CollapseStrategy::PECluster
    } else {
        CollapseStrategy::SocketGroup
    }
}

/// Returns `true` if the Activity panel should be shown at the given width.
pub fn should_show_panel(cols: u16) -> bool {
    cols > MIN_PANEL_WIDTH
}

/// Calculate the number of terminal rows the Activity panel will consume.
///
/// Returns 0 when the panel should be hidden (narrow terminal or no data).
pub fn panel_height(cpu_info: &[CpuInfo], cols: u16) -> u16 {
    if !should_show_panel(cols) || cpu_info.is_empty() {
        return 0;
    }

    let info = &cpu_info[0];
    if info.per_core_utilization.is_empty() {
        return 0;
    }

    let width = cols as usize;
    let strategy = core_collapse_strategy(info, width);

    // 1 line for the header/border top
    // N lines for the core bars (depends on strategy and core count)
    // 1 line for the border bottom
    let bar_lines = match strategy {
        CollapseStrategy::Individual => {
            let half_width = width / 2;
            let cores_per_line = calculate_cores_per_line(half_width);
            let core_count = info.per_core_utilization.len();
            core_count.div_ceil(cores_per_line)
        }
        CollapseStrategy::PECluster => {
            // P-cluster bar + E-cluster bar = 2 lines
            2
        }
        CollapseStrategy::SocketGroup => info.socket_count.max(1) as usize,
    };

    // top border + content lines + bottom border
    (1 + bar_lines + 1) as u16
}

/// Render the CPU Activity panel into the given writer.
///
/// This draws per-core CPU utilization bars using the left half of the
/// terminal width. The panel is self-contained: it draws its own borders
/// and handles all layout internally.
///
/// # Arguments
/// * `stdout` - Writer to render into
/// * `cpu_info` - CPU information (first entry used for per-core data)
/// * `width` - Full terminal width in columns
pub fn render_activity_panel<W: Write>(stdout: &mut W, cpu_info: &[CpuInfo], width: usize) {
    if cpu_info.is_empty() {
        return;
    }

    let info = &cpu_info[0];
    if info.per_core_utilization.is_empty() {
        return;
    }

    let strategy = core_collapse_strategy(info, width);

    // Use the left half of the terminal for the Activity panel
    let panel_width = width / 2;

    // Draw the panel
    draw_panel_top_border(stdout, panel_width, &strategy, info);

    match strategy {
        CollapseStrategy::Individual => {
            draw_individual_cores(stdout, &info.per_core_utilization, panel_width, width);
        }
        CollapseStrategy::PECluster => {
            draw_pe_cluster_bars(stdout, info, panel_width, width);
        }
        CollapseStrategy::SocketGroup => {
            draw_socket_group_bars(stdout, info, panel_width, width);
        }
    }

    draw_panel_bottom_border(stdout, panel_width, width);
}

// ---------------------------------------------------------------------------
// Panel chrome (borders)
// ---------------------------------------------------------------------------

fn draw_panel_top_border<W: Write>(
    stdout: &mut W,
    panel_width: usize,
    strategy: &CollapseStrategy,
    info: &CpuInfo,
) {
    let title = match strategy {
        CollapseStrategy::Individual => {
            let core_count = info.per_core_utilization.len();
            let avg_util = average_utilization(&info.per_core_utilization);
            format!("CPU Cores ({core_count} cores, {avg_util:.1}% avg)")
        }
        CollapseStrategy::PECluster => {
            if let Some(apple) = info.apple_silicon_info.as_ref() {
                let avg = average_utilization(&info.per_core_utilization);
                if apple.s_core_count > 0 {
                    format!(
                        "CPU Cores ({}S+{}P, {avg:.1}% avg)",
                        apple.s_core_count, apple.p_core_count,
                    )
                } else {
                    format!(
                        "CPU Cores ({}P+{}E, {avg:.1}% avg)",
                        apple.p_core_count, apple.e_core_count,
                    )
                }
            } else {
                let core_count = info.per_core_utilization.len();
                let avg_util = average_utilization(&info.per_core_utilization);
                format!("CPU Cores ({core_count} cores, {avg_util:.1}% avg)")
            }
        }
        CollapseStrategy::SocketGroup => {
            let socket_count = info.socket_count.max(1);
            let core_count = info.per_core_utilization.len();
            let avg_util = average_utilization(&info.per_core_utilization);
            format!("CPU Cores ({core_count} cores, {socket_count} sockets, {avg_util:.1}% avg)")
        }
    };

    // "  " + "+-" + " title " + "---..." + "-+"
    let inner_width = panel_width.saturating_sub(4); // 2 margin + 2 corners
    let title_space = 1 + title.len() + 1; // space + title + space
    let dashes = inner_width.saturating_sub(title_space + 1); // +1 for the initial dash

    print_colored_text(stdout, "  ", Color::White, None, None);
    print_colored_text(stdout, "\u{256d}\u{2500}", Color::Cyan, None, None);
    print_colored_text(stdout, " ", Color::White, None, None);
    print_colored_text(stdout, &title, Color::Cyan, None, None);
    print_colored_text(stdout, " ", Color::White, None, None);
    for _ in 0..dashes {
        print_colored_text(stdout, "\u{2500}", Color::Cyan, None, None);
    }
    print_colored_text(stdout, "\u{256e}", Color::Cyan, None, None);

    queue!(stdout, Print("\r\n")).unwrap();
}

fn draw_panel_bottom_border<W: Write>(stdout: &mut W, panel_width: usize, _full_width: usize) {
    let inner_width = panel_width.saturating_sub(4); // 2 margin + 2 corners
    print_colored_text(stdout, "  ", Color::White, None, None);
    print_colored_text(stdout, "\u{2570}", Color::Cyan, None, None);
    for _ in 0..inner_width {
        print_colored_text(stdout, "\u{2500}", Color::Cyan, None, None);
    }
    print_colored_text(stdout, "\u{256f}", Color::Cyan, None, None);
    queue!(stdout, Print("\r\n")).unwrap();
}

// ---------------------------------------------------------------------------
// Individual per-core bars
// ---------------------------------------------------------------------------

fn calculate_cores_per_line(panel_width: usize) -> usize {
    // Each core needs: label (3 chars) + ": [" + bar + "]" + spacing
    // For compact display, use utilization blocks (1 char per core) with grouping
    // When we have enough width, show progress bars (4 per line for <=16 cores)
    let content_width = panel_width.saturating_sub(6); // 4 margin + 2 border
    let spacing = 2;
    // Minimum bar width per core: label(3) + ": [" + bar(8) + "]" = ~15 chars
    let min_core_width = 15;
    let cores = content_width / (min_core_width + spacing);
    cores.clamp(1, 4)
}

fn draw_individual_cores<W: Write>(
    stdout: &mut W,
    per_core: &[CoreUtilization],
    panel_width: usize,
    _full_width: usize,
) {
    let content_width = panel_width.saturating_sub(6); // 2 margin + 2 border chars + 2 inner padding
    let cores_per_line = calculate_cores_per_line(panel_width);
    let spacing = 2;
    let core_bar_width =
        content_width.saturating_sub((cores_per_line - 1) * spacing) / cores_per_line;

    // Separate cores by type
    let mut s_cores: Vec<&CoreUtilization> = Vec::new();
    let mut e_cores: Vec<&CoreUtilization> = Vec::new();
    let mut p_cores: Vec<&CoreUtilization> = Vec::new();
    let mut standard_cores: Vec<&CoreUtilization> = Vec::new();

    for core in per_core {
        match core.core_type {
            CoreType::Super => s_cores.push(core),
            CoreType::Efficiency => e_cores.push(core),
            CoreType::Performance => p_cores.push(core),
            CoreType::Standard => standard_cores.push(core),
        }
    }

    // Render in order: S-cores, E-cores, P-cores, Standard cores
    let ordered_cores: Vec<(&CoreUtilization, &str)> = s_cores
        .iter()
        .map(|c| (*c, "S"))
        .chain(e_cores.iter().map(|c| (*c, "E")))
        .chain(p_cores.iter().map(|c| (*c, "P")))
        .chain(standard_cores.iter().map(|c| (*c, "C")))
        .collect();

    let mut s_idx = 0usize;
    let mut e_idx = 0usize;
    let mut p_idx = 0usize;
    let mut c_idx = 0usize;
    let mut cores_on_line = 0;

    for (core, prefix) in &ordered_cores {
        if cores_on_line == 0 {
            print_colored_text(stdout, "  ", Color::White, None, None);
            print_colored_text(stdout, "\u{2502} ", Color::Cyan, None, None);
        }

        let idx = match *prefix {
            "S" => {
                s_idx += 1;
                s_idx
            }
            "E" => {
                e_idx += 1;
                e_idx
            }
            "P" => {
                p_idx += 1;
                p_idx
            }
            _ => {
                c_idx += 1;
                c_idx
            }
        };
        let label = format!("{prefix}{idx}");
        draw_bar(
            stdout,
            &label,
            core.utilization,
            100.0,
            core_bar_width,
            None,
        );

        cores_on_line += 1;

        if cores_on_line >= cores_per_line {
            // Pad to panel width and close border
            let used = 4 + cores_on_line * core_bar_width + (cores_on_line - 1) * spacing;
            let pad = panel_width.saturating_sub(used + 2); // 2 for " |"
            if pad > 0 {
                print_colored_text(stdout, &" ".repeat(pad), Color::White, None, None);
            }
            print_colored_text(stdout, " \u{2502}", Color::Cyan, None, None);
            queue!(stdout, Print("\r\n")).unwrap();
            cores_on_line = 0;
        } else {
            print_colored_text(stdout, "  ", Color::White, None, None);
        }
    }

    // Handle last partial line
    if cores_on_line > 0 {
        let remaining = cores_per_line - cores_on_line;
        let remaining_width = remaining * core_bar_width + remaining * spacing;
        if remaining_width > 0 {
            print_colored_text(
                stdout,
                &" ".repeat(remaining_width),
                Color::White,
                None,
                None,
            );
        }
        let used = 4 + cores_per_line * core_bar_width + (cores_per_line - 1) * spacing;
        let pad = panel_width.saturating_sub(used + 2);
        if pad > 0 {
            print_colored_text(stdout, &" ".repeat(pad), Color::White, None, None);
        }
        print_colored_text(stdout, " \u{2502}", Color::Cyan, None, None);
        queue!(stdout, Print("\r\n")).unwrap();
    }
}

// ---------------------------------------------------------------------------
// P/E cluster grouped bars (Apple Silicon)
// ---------------------------------------------------------------------------

fn draw_pe_cluster_bars<W: Write>(
    stdout: &mut W,
    info: &CpuInfo,
    panel_width: usize,
    _full_width: usize,
) {
    let apple = match &info.apple_silicon_info {
        Some(a) => a,
        None => return,
    };

    let content_width = panel_width.saturating_sub(6);

    // Collect per-core utilization blocks for each cluster
    let s_cores: Vec<&CoreUtilization> = info
        .per_core_utilization
        .iter()
        .filter(|c| c.core_type == CoreType::Super)
        .collect();
    let p_cores: Vec<&CoreUtilization> = info
        .per_core_utilization
        .iter()
        .filter(|c| c.core_type == CoreType::Performance)
        .collect();
    let e_cores: Vec<&CoreUtilization> = info
        .per_core_utilization
        .iter()
        .filter(|c| c.core_type == CoreType::Efficiency)
        .collect();

    if apple.s_core_count > 0 {
        // M5 Pro/Max: S-CPU + P-CPU gauges
        let s_block_width = s_cores.len() + (s_cores.len() / 4);
        let p_block_width = p_cores.len() + (p_cores.len() / 4);
        let shared_bar_width = content_width.saturating_sub(s_block_width.max(p_block_width) + 2);

        // S-cluster line: bar + utilization blocks
        draw_cluster_line(
            stdout,
            "S-CPU",
            apple.s_core_utilization,
            &s_cores,
            shared_bar_width,
            panel_width,
        );

        // P-cluster line: bar + utilization blocks
        draw_cluster_line(
            stdout,
            "P-CPU",
            apple.p_core_utilization,
            &p_cores,
            shared_bar_width,
            panel_width,
        );
    } else {
        // M1-M4: P-CPU + E-CPU gauges
        // Compute one shared bar_width using the larger block section so that
        // P-CPU and E-CPU gauges end at the same column.
        let p_block_width = p_cores.len() + (p_cores.len() / 4);
        let e_block_width = e_cores.len() + (e_cores.len() / 4);
        let shared_bar_width = content_width.saturating_sub(p_block_width.max(e_block_width) + 2);

        // P-cluster line: bar + utilization blocks
        draw_cluster_line(
            stdout,
            "P-CPU",
            apple.p_core_utilization,
            &p_cores,
            shared_bar_width,
            panel_width,
        );

        // E-cluster line: bar + utilization blocks
        draw_cluster_line(
            stdout,
            "E-CPU",
            apple.e_core_utilization,
            &e_cores,
            shared_bar_width,
            panel_width,
        );
    }
}

fn draw_cluster_line<W: Write>(
    stdout: &mut W,
    label: &str,
    utilization: f64,
    cores: &[&CoreUtilization],
    bar_width: usize,
    panel_width: usize,
) {
    print_colored_text(stdout, "  ", Color::White, None, None);
    print_colored_text(stdout, "\u{2502} ", Color::Cyan, None, None);

    // Draw the progress bar using the pre-computed shared bar_width
    draw_bar(stdout, label, utilization, 100.0, bar_width, None);
    print_colored_text(stdout, " ", Color::White, None, None);

    // Draw per-core utilization blocks
    for (i, core) in cores.iter().enumerate() {
        let (block, color) = get_utilization_block(core.utilization);
        print_colored_text(stdout, block, color, None, None);
        if (i + 1) % 4 == 0 && i + 1 < cores.len() {
            print_colored_text(stdout, " ", Color::White, None, None);
        }
    }

    // Pad to panel width
    let blocks_printed = cores.len() + cores.len() / 4
        - if cores.len().is_multiple_of(4) && !cores.is_empty() {
            1
        } else {
            0
        };
    let used = 4 + bar_width + 1 + blocks_printed;
    let pad = panel_width.saturating_sub(used + 2);
    if pad > 0 {
        print_colored_text(stdout, &" ".repeat(pad), Color::White, None, None);
    }
    print_colored_text(stdout, " \u{2502}", Color::Cyan, None, None);
    queue!(stdout, Print("\r\n")).unwrap();
}

// ---------------------------------------------------------------------------
// Socket/NUMA grouped bars (x86 / other)
// ---------------------------------------------------------------------------

fn draw_socket_group_bars<W: Write>(
    stdout: &mut W,
    info: &CpuInfo,
    panel_width: usize,
    _full_width: usize,
) {
    let content_width = panel_width.saturating_sub(6);
    let socket_count = info.socket_count.max(1) as usize;
    let cores_per_socket = info.per_core_utilization.len() / socket_count;

    for socket_id in 0..socket_count {
        let start = socket_id * cores_per_socket;
        let end = if socket_id == socket_count - 1 {
            info.per_core_utilization.len()
        } else {
            (socket_id + 1) * cores_per_socket
        };

        let socket_cores = &info.per_core_utilization[start..end];
        let avg_util = average_utilization(socket_cores);

        // Label: "S0", "S1", etc.
        let label = format!("S{socket_id}");

        // Calculate block section width
        let block_count = socket_cores.len();
        let group_separators = if block_count > 4 {
            (block_count - 1) / 4
        } else {
            0
        };
        let block_section_width = block_count + group_separators;
        let bar_width = content_width.saturating_sub(block_section_width + 2);

        print_colored_text(stdout, "  ", Color::White, None, None);
        print_colored_text(stdout, "\u{2502} ", Color::Cyan, None, None);

        draw_bar(stdout, &label, avg_util, 100.0, bar_width, None);
        print_colored_text(stdout, " ", Color::White, None, None);

        // Draw per-core blocks within this socket
        for (i, core) in socket_cores.iter().enumerate() {
            let (block, color) = get_utilization_block(core.utilization);
            print_colored_text(stdout, block, color, None, None);
            if (i + 1) % 4 == 0 && i + 1 < block_count {
                print_colored_text(stdout, " ", Color::White, None, None);
            }
        }

        // Pad to panel width
        let blocks_printed = block_count + group_separators;
        let used = 4 + bar_width + 1 + blocks_printed;
        let pad = panel_width.saturating_sub(used + 2);
        if pad > 0 {
            print_colored_text(stdout, &" ".repeat(pad), Color::White, None, None);
        }
        print_colored_text(stdout, " \u{2502}", Color::Cyan, None, None);
        queue!(stdout, Print("\r\n")).unwrap();
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn average_utilization(cores: &[CoreUtilization]) -> f64 {
    if cores.is_empty() {
        return 0.0;
    }
    cores.iter().map(|c| c.utilization).sum::<f64>() / cores.len() as f64
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::{AppleSiliconCpuInfo, CpuPlatformType};

    fn make_standard_cpu(core_count: usize) -> CpuInfo {
        let per_core: Vec<CoreUtilization> = (0..core_count)
            .map(|i| CoreUtilization {
                core_id: i as u32,
                core_type: CoreType::Standard,
                utilization: (i as f64 * 10.0) % 100.0,
            })
            .collect();

        CpuInfo {
            host_id: "localhost".to_string(),
            hostname: "testhost".to_string(),
            instance: "".to_string(),
            cpu_model: "Test CPU".to_string(),
            architecture: "x86_64".to_string(),
            platform_type: CpuPlatformType::Intel,
            socket_count: 1,
            total_cores: core_count as u32,
            total_threads: core_count as u32 * 2,
            base_frequency_mhz: 3000,
            max_frequency_mhz: 4000,
            cache_size_mb: 16,
            utilization: 50.0,
            temperature: Some(65),
            power_consumption: Some(95.0),
            per_socket_info: Vec::new(),
            apple_silicon_info: None,
            per_core_utilization: per_core,
            time: String::new(),
        }
    }

    fn make_apple_silicon_cpu(p_count: usize, e_count: usize) -> CpuInfo {
        let mut per_core = Vec::new();
        for i in 0..e_count {
            per_core.push(CoreUtilization {
                core_id: i as u32,
                core_type: CoreType::Efficiency,
                utilization: 20.0 + i as f64 * 5.0,
            });
        }
        for i in 0..p_count {
            per_core.push(CoreUtilization {
                core_id: (e_count + i) as u32,
                core_type: CoreType::Performance,
                utilization: 40.0 + i as f64 * 5.0,
            });
        }

        CpuInfo {
            host_id: "localhost".to_string(),
            hostname: "testhost".to_string(),
            instance: "".to_string(),
            cpu_model: "Apple M2 Pro".to_string(),
            architecture: "arm64".to_string(),
            platform_type: CpuPlatformType::AppleSilicon,
            socket_count: 1,
            total_cores: (p_count + e_count) as u32,
            total_threads: (p_count + e_count) as u32,
            base_frequency_mhz: 3490,
            max_frequency_mhz: 3490,
            cache_size_mb: 16,
            utilization: 35.0,
            temperature: None,
            power_consumption: None,
            per_socket_info: Vec::new(),
            apple_silicon_info: Some(AppleSiliconCpuInfo {
                s_core_count: 0,
                p_core_count: p_count as u32,
                e_core_count: e_count as u32,
                gpu_core_count: 16,
                s_core_utilization: 0.0,
                p_core_utilization: 55.0,
                e_core_utilization: 25.0,
                ane_ops_per_second: None,
                s_cluster_frequency_mhz: None,
                p_cluster_frequency_mhz: Some(3490),
                e_cluster_frequency_mhz: Some(2420),
                s_core_l2_cache_mb: None,
                p_core_l2_cache_mb: Some(16),
                e_core_l2_cache_mb: Some(4),
            }),
            per_core_utilization: per_core,
            time: String::new(),
        }
    }

    #[test]
    fn test_strategy_individual_small_core_count() {
        let cpu = make_standard_cpu(8);
        let strategy = core_collapse_strategy(&cpu, 120);
        assert_eq!(strategy, CollapseStrategy::Individual);
    }

    #[test]
    fn test_strategy_socket_group_high_core_count() {
        let mut cpu = make_standard_cpu(64);
        cpu.socket_count = 2;
        let strategy = core_collapse_strategy(&cpu, 120);
        assert_eq!(strategy, CollapseStrategy::SocketGroup);
    }

    #[test]
    fn test_strategy_pe_cluster_apple_silicon() {
        let cpu = make_apple_silicon_cpu(8, 4);
        // 12 cores with width 30 -> threshold = 10, 12 > 10 -> collapse
        let strategy = core_collapse_strategy(&cpu, 30);
        assert_eq!(strategy, CollapseStrategy::PECluster);
    }

    #[test]
    fn test_strategy_individual_apple_silicon_wide() {
        let cpu = make_apple_silicon_cpu(6, 4);
        // 10 cores with width 120 -> threshold = min(40, 16) = 16, 10 <= 16 -> individual
        let strategy = core_collapse_strategy(&cpu, 120);
        assert_eq!(strategy, CollapseStrategy::Individual);
    }

    #[test]
    fn test_should_show_panel() {
        assert!(!should_show_panel(79));
        assert!(!should_show_panel(80));
        assert!(should_show_panel(81));
        assert!(should_show_panel(120));
    }

    #[test]
    fn test_panel_height_narrow_terminal() {
        let cpu = vec![make_standard_cpu(8)];
        assert_eq!(panel_height(&cpu, 79), 0);
    }

    #[test]
    fn test_panel_height_empty_cpu() {
        let cpu: Vec<CpuInfo> = Vec::new();
        assert_eq!(panel_height(&cpu, 120), 0);
    }

    #[test]
    fn test_panel_height_standard_cores() {
        let cpu = vec![make_standard_cpu(8)];
        let height = panel_height(&cpu, 120);
        // Should be > 0 (top border + at least 1 bar line + bottom border)
        assert!(height >= 3, "Expected height >= 3, got {height}");
    }

    #[test]
    fn test_render_activity_panel_does_not_panic_empty() {
        let cpu: Vec<CpuInfo> = Vec::new();
        let mut buf: Vec<u8> = Vec::new();
        render_activity_panel(&mut buf, &cpu, 120);
    }

    #[test]
    fn test_render_activity_panel_individual() {
        let cpu = vec![make_standard_cpu(8)];
        let mut buf: Vec<u8> = Vec::new();
        render_activity_panel(&mut buf, &cpu, 120);
        assert!(!buf.is_empty());
    }

    #[test]
    fn test_render_activity_panel_pe_cluster() {
        let cpu = vec![make_apple_silicon_cpu(8, 4)];
        let mut buf: Vec<u8> = Vec::new();
        // Width 30 triggers PECluster strategy for 12 cores
        render_activity_panel(&mut buf, &cpu, 80);
        assert!(!buf.is_empty());
    }

    #[test]
    fn test_render_activity_panel_socket_group() {
        let mut cpu = make_standard_cpu(64);
        cpu.socket_count = 2;
        let cpu_vec = vec![cpu];
        let mut buf: Vec<u8> = Vec::new();
        render_activity_panel(&mut buf, &cpu_vec, 120);
        assert!(!buf.is_empty());
    }

    #[test]
    fn test_average_utilization() {
        let cores = vec![
            CoreUtilization {
                core_id: 0,
                core_type: CoreType::Standard,
                utilization: 20.0,
            },
            CoreUtilization {
                core_id: 1,
                core_type: CoreType::Standard,
                utilization: 80.0,
            },
        ];
        let avg = average_utilization(&cores);
        assert!((avg - 50.0).abs() < 0.001);
    }

    #[test]
    fn test_average_utilization_empty() {
        let cores: Vec<CoreUtilization> = Vec::new();
        assert_eq!(average_utilization(&cores), 0.0);
    }
}
