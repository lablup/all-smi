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

use crate::common::config::ThemeConfig;
use crate::device::{CoreType, CoreUtilization, CpuInfo};
use crate::ui::braille::sparkline_braille_rows;
use crate::ui::buffer::BufferWriter;
use crate::ui::renderers::widgets::gauges::{
    GAUGE_HIGH_COLOR, GAUGE_MEDIUM_COLOR, get_utilization_block,
};
use crate::ui::text::print_colored_text;
use crate::ui::widgets::{MIN_BAR_WIDTH, draw_bar};

/// Minimum terminal width required to show the Activity panel.
/// Below this threshold the panel is omitted entirely.
const MIN_PANEL_WIDTH: u16 = 80;

/// Terminal height (rows) at or above which the Activity panel switches to the
/// btop-style multi-row history graphs. Below it, both halves fall back to the
/// compact single-row layout so the process list is not starved.
pub const MULTIROW_MIN_TERMINAL_ROWS: u16 = 35;

/// Number of terminal rows a multi-row history graph occupies. Three rows give
/// `3 * 4 = 12` braille dot levels of vertical resolution.
pub const GRAPH_ROWS: usize = 3;

/// Whether the multi-row history graphs should be used at the given terminal
/// height.
///
/// This is the single source of truth for the mode decision. Both the height
/// functions ([`panel_height`], [`gpu_content_rows`](crate::ui::gpu_sparkline_panel::gpu_content_rows))
/// and the rendering paths ([`render_activity_panel`],
/// [`render_combined_activity_panel`](crate::ui::gpu_sparkline_panel::render_combined_activity_panel))
/// call this with the same `terminal_rows`, so the reported height can never
/// disagree with the rendered line count.
#[must_use]
pub fn use_multirow_graphs(terminal_rows: u16) -> bool {
    terminal_rows >= MULTIROW_MIN_TERMINAL_ROWS
}

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

/// Number of core-bar content lines the CPU panel renders for the given
/// strategy (excludes the optional history graph and the borders).
///
/// Used by [`panel_height`] only; the rendering path computes the same line
/// count independently (see `calculate_cores_per_line` and friends) rather
/// than calling this function. The two are kept in sync by construction
/// (identical math on both sides) and tests assert that `panel_height`'s
/// result matches the actual emitted line count.
fn bar_line_count(info: &CpuInfo, strategy: &CollapseStrategy, width: usize) -> usize {
    match strategy {
        CollapseStrategy::Individual => {
            let half_width = width / 2;
            let cores_per_line = calculate_cores_per_line(half_width);
            let core_count = info.per_core_utilization.len();
            core_count.div_ceil(cores_per_line)
        }
        // P-cluster bar + E-cluster bar (or S+P on M-series Pro/Max) = 2 lines.
        CollapseStrategy::PECluster => 2,
        CollapseStrategy::SocketGroup => info.socket_count.max(1) as usize,
    }
}

/// Calculate the number of terminal rows the Activity panel will consume.
///
/// Returns 0 when the panel should be hidden (narrow terminal or no data).
///
/// `rows` is the full terminal height; when it is tall enough
/// ([`use_multirow_graphs`]) the panel reserves [`GRAPH_ROWS`] extra rows for
/// the CPU total-utilization history graph above the core bars.
pub fn panel_height(cpu_info: &[CpuInfo], cols: u16, rows: u16) -> u16 {
    if !should_show_panel(cols) || cpu_info.is_empty() {
        return 0;
    }

    let info = &cpu_info[0];
    if info.per_core_utilization.is_empty() {
        return 0;
    }

    let width = cols as usize;
    let strategy = core_collapse_strategy(info, width);
    let bar_lines = bar_line_count(info, &strategy, width);
    let graph_lines = if use_multirow_graphs(rows) {
        GRAPH_ROWS
    } else {
        0
    };

    // top border + optional history graph + core bars + bottom border
    (1 + graph_lines + bar_lines + 1) as u16
}

/// Render the CPU Activity panel into the given writer.
///
/// This draws per-core CPU utilization bars using the left half of the
/// terminal width. The panel is self-contained: it draws its own borders
/// and handles all layout internally.
///
/// When `terminal_rows` is tall enough ([`use_multirow_graphs`]) a 3-row CPU
/// total-utilization history graph (fixed 0..100 axis, fed from `cpu_history`)
/// is inserted above the core bars, btop-style.
///
/// # Arguments
/// * `stdout` - Writer to render into
/// * `cpu_info` - CPU information (first entry used for per-core data)
/// * `cpu_history` - CPU total-utilization history (most recent last); passed
///   in as a slice to keep this module decoupled from `AppState`
/// * `width` - Full terminal width in columns
/// * `terminal_rows` - Full terminal height in rows (drives the mode decision)
pub fn render_activity_panel<W: Write>(
    stdout: &mut W,
    cpu_info: &[CpuInfo],
    cpu_history: &[f64],
    width: usize,
    terminal_rows: u16,
) {
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

    if use_multirow_graphs(terminal_rows) {
        draw_cpu_history_graph(stdout, cpu_history, panel_width);
    }

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
// Multi-row history graph (shared by the CPU and GPU halves)
// ---------------------------------------------------------------------------

/// Draw the 3-row CPU total-utilization history graph (fixed 0..100 axis) into
/// the panel, emitting exactly [`GRAPH_ROWS`] lines each terminated by "\r\n".
fn draw_cpu_history_graph<W: Write>(stdout: &mut W, cpu_history: &[f64], panel_width: usize) {
    let content_width = panel_width.saturating_sub(4);
    let latest = cpu_history.last().copied().unwrap_or(0.0);
    let value_str = format!("{latest:.1}%");
    let lines = multirow_graph_lines(
        cpu_history,
        (0.0, 100.0),
        ThemeConfig::cpu_color(),
        Color::Cyan,
        content_width,
        &value_str,
        "0-100",
    );
    for line in lines {
        stdout.write_all(line.as_bytes()).unwrap();
        queue!(stdout, Print("\r\n")).unwrap();
    }
}

/// Compose the lines of a stacked multi-row braille history graph with
/// btop-style right-aligned annotations. Returns exactly [`GRAPH_ROWS`]
/// strings, top row first, each WITHOUT a trailing newline.
///
/// Layout of every row: `│ <sparkline><annotation> │`. The sparkline
/// occupies the same width on all rows (so the stacked dots stay vertically
/// aligned); the annotation column shows `value_str` right-aligned on the
/// top row and `axis_str` right-aligned on the bottom row. `content_width`
/// is the width available between the `│ ` and ` │` borders, so the total row
/// width is always `4 + content_width`.
///
/// Each row's sparkline segment is colored by height, btop-style: the bottom
/// row keeps `spark_color` (the metric's base theme color) and rows above it
/// shift toward warmer colors, computed per row by [`graph_row_color`]. Only
/// the sparkline segment is affected; the annotations and borders keep their
/// existing colors regardless of row.
///
/// All width math uses saturating arithmetic and stays panic-free down to
/// pathologically narrow panels.
#[allow(clippy::too_many_arguments)]
pub(crate) fn multirow_graph_lines(
    history: &[f64],
    range: (f64, f64),
    spark_color: Color,
    border_color: Color,
    content_width: usize,
    value_str: &str,
    axis_str: &str,
) -> Vec<String> {
    let annot_width = value_str.chars().count().max(axis_str.chars().count());
    let spark_width = content_width.saturating_sub(annot_width + 1).max(1);
    // The annotation region absorbs whatever columns the sparkline did not use,
    // so `left_margin + "│ " + spark + annot_region + " │"` fills the panel.
    let annot_region = content_width.saturating_sub(spark_width);
    let sparks = sparkline_braille_rows(history, spark_width, GRAPH_ROWS, Some(range));
    let rows = sparks.len();

    sparks
        .iter()
        .enumerate()
        .map(|(i, spark)| {
            let (annot, annot_color): (&str, Color) = if i == 0 {
                (value_str, Color::White)
            } else if i + 1 == GRAPH_ROWS {
                (axis_str, Color::DarkGrey)
            } else {
                ("", Color::White)
            };
            // `sparks` is top row first (see `sparkline_braille_rows`), so
            // convert the top-first index `i` to a bottom-based row index
            // before looking up the height-gradient color.
            let row_from_bottom = rows - 1 - i;
            let row_color = graph_row_color(row_from_bottom, rows, spark_color);

            let mut buf = BufferWriter::new();
            print_colored_text(&mut buf, "\u{2502} ", border_color, None, None);
            print_colored_text(&mut buf, spark, row_color, None, None);

            // Right-align the annotation within its reserved region.
            let annot: String = if annot.chars().count() > annot_region {
                annot.chars().take(annot_region).collect()
            } else {
                annot.to_string()
            };
            let pad = annot_region.saturating_sub(annot.chars().count());
            if pad > 0 {
                print_colored_text(&mut buf, &" ".repeat(pad), Color::White, None, None);
            }
            if !annot.is_empty() {
                print_colored_text(&mut buf, &annot, annot_color, None, None);
            }
            print_colored_text(&mut buf, " \u{2502}", border_color, None, None);
            buf.get_buffer().to_string()
        })
        .collect()
}

/// btop-style height gradient: pick the color for one terminal row of a
/// multi-row sparkline based on its position, bottom to top.
///
/// `row_from_bottom` is 0 for the bottom-most row and `rows - 1` for the
/// top-most row. The mapping uses a 3-anchor palette `[base,
/// GAUGE_MEDIUM_COLOR, GAUGE_HIGH_COLOR]` (Yellow and Red): row
/// `row_from_bottom` of `rows` picks `palette[row_from_bottom * 3 / rows]`.
/// For the current [`GRAPH_ROWS`]`== 3` graphs this yields exactly
/// base / Yellow / Red bottom-to-top; a single-row graph (`rows == 1`) stays
/// entirely `base`; a two-row graph (`rows == 2`) yields base / Yellow.
/// `rows == 0` returns `base` rather than dividing by zero.
#[must_use]
pub(crate) fn graph_row_color(row_from_bottom: usize, rows: usize, base: Color) -> Color {
    if rows == 0 {
        return base;
    }
    let palette = [base, GAUGE_MEDIUM_COLOR, GAUGE_HIGH_COLOR];
    let idx = (row_from_bottom * palette.len() / rows).min(palette.len() - 1);
    palette[idx]
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

    // Keep even the longest strategy-specific title inside the panel. At the
    // minimum supported terminal width, socket metadata can otherwise push
    // the right corner several columns past the GPU panel boundary.
    let max_title_width = panel_width.saturating_sub(5);
    let title = truncate_with_ellipsis(&title, max_title_width);

    // "+-" + " title " + "---..." + "-+"
    let inner_width = panel_width.saturating_sub(2); // 2 corners
    let title_space = 1 + title.chars().count() + 1; // space + title + space
    let dashes = inner_width.saturating_sub(title_space + 1); // +1 for the initial dash

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
    let inner_width = panel_width.saturating_sub(2); // 2 corners
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
    let content_width = panel_width.saturating_sub(4); // 2 border chars + 2 inner padding
    let spacing = 2;
    // `draw_bar` needs MIN_BAR_WIDTH columns for its full representation.
    // Add one spacing allowance before dividing so the final core does not
    // pay for a separator that is only rendered between adjacent cores.
    let cores = content_width.saturating_add(spacing) / (MIN_BAR_WIDTH + spacing);
    cores.clamp(1, 4)
}

fn draw_individual_cores<W: Write>(
    stdout: &mut W,
    per_core: &[CoreUtilization],
    panel_width: usize,
    _full_width: usize,
) {
    let content_width = panel_width.saturating_sub(4); // 2 border chars + 2 inner padding
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
            let used = 2 + cores_on_line * core_bar_width + (cores_on_line - 1) * spacing;
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
        // The loop has already emitted one separator after each core on a
        // partial line, including the last rendered core. Account for those
        // actual columns directly; padding as though all core slots had been
        // rendered used to add one extra separator and overflow by 2 columns.
        let used = 2 + cores_on_line * core_bar_width + cores_on_line * spacing;
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

    let content_width = panel_width.saturating_sub(4);

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
        let shared_block_width =
            bounded_block_width(s_cores.len().max(p_cores.len()), content_width);
        let shared_bar_width = content_width.saturating_sub(shared_block_width + 1);

        // S-cluster line: bar + utilization blocks
        draw_cluster_line(
            stdout,
            "S-CPU",
            apple.s_core_utilization,
            &s_cores,
            shared_bar_width,
            shared_block_width,
            panel_width,
        );

        // P-cluster line: bar + utilization blocks
        draw_cluster_line(
            stdout,
            "P-CPU",
            apple.p_core_utilization,
            &p_cores,
            shared_bar_width,
            shared_block_width,
            panel_width,
        );
    } else {
        // M1-M4: P-CPU + E-CPU gauges
        // Compute one shared bar_width using the larger block section so that
        // P-CPU and E-CPU gauges end at the same column.
        let shared_block_width =
            bounded_block_width(p_cores.len().max(e_cores.len()), content_width);
        let shared_bar_width = content_width.saturating_sub(shared_block_width + 1);

        // P-cluster line: bar + utilization blocks
        draw_cluster_line(
            stdout,
            "P-CPU",
            apple.p_core_utilization,
            &p_cores,
            shared_bar_width,
            shared_block_width,
            panel_width,
        );

        // E-cluster line: bar + utilization blocks
        draw_cluster_line(
            stdout,
            "E-CPU",
            apple.e_core_utilization,
            &e_cores,
            shared_bar_width,
            shared_block_width,
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
    block_width: usize,
    panel_width: usize,
) {
    print_colored_text(stdout, "\u{2502} ", Color::Cyan, None, None);

    // Draw the progress bar using the pre-computed shared bar_width
    draw_bar(stdout, label, utilization, 100.0, bar_width, None);
    print_colored_text(stdout, " ", Color::White, None, None);

    // Draw as many per-core utilization blocks as fit after preserving the
    // full gauge. High-core-count CPUs previously pushed the right border out.
    let blocks_printed = draw_utilization_blocks(stdout, cores.iter().copied(), block_width);

    // Pad to panel width
    let used = 2 + bar_width + 1 + blocks_printed;
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
    let content_width = panel_width.saturating_sub(4);
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
        let block_width = bounded_block_width(block_count, content_width);
        let bar_width = content_width.saturating_sub(block_width + 1);

        print_colored_text(stdout, "\u{2502} ", Color::Cyan, None, None);

        draw_bar(stdout, &label, avg_util, 100.0, bar_width, None);
        print_colored_text(stdout, " ", Color::White, None, None);

        // Draw per-core blocks within this socket without exceeding the
        // columns left after the gauge.
        let blocks_printed = draw_utilization_blocks(stdout, socket_cores.iter(), block_width);

        // Pad to panel width
        let used = 2 + bar_width + 1 + blocks_printed;
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

/// Width of a grouped per-core block section, capped so the gauge retains its
/// full representation plus the one-column gap between gauge and blocks.
fn bounded_block_width(core_count: usize, content_width: usize) -> usize {
    let natural_width = core_count + core_count.saturating_sub(1) / 4;
    let available = content_width.saturating_sub(MIN_BAR_WIDTH + 1);
    natural_width.min(available)
}

/// Render grouped utilization blocks into at most `max_width` columns and
/// return the number of columns emitted. Groups are separated every four
/// cores; a separator is emitted only when another block also fits.
fn draw_utilization_blocks<'a, W, I>(stdout: &mut W, cores: I, max_width: usize) -> usize
where
    W: Write,
    I: IntoIterator<Item = &'a CoreUtilization>,
{
    let mut used = 0usize;
    for (i, core) in cores.into_iter().enumerate() {
        let needs_separator = i > 0 && i.is_multiple_of(4);
        let needed = 1 + usize::from(needs_separator);
        if used.saturating_add(needed) > max_width {
            break;
        }
        if needs_separator {
            print_colored_text(stdout, " ", Color::White, None, None);
            used += 1;
        }
        let (block, color) = get_utilization_block(core.utilization);
        print_colored_text(stdout, block, color, None, None);
        used += 1;
    }
    used
}

/// Truncate `text` to `max_width` visible characters, using an ellipsis when
/// there is room, while leaving shorter strings unchanged.
fn truncate_with_ellipsis(text: &str, max_width: usize) -> String {
    if text.chars().count() <= max_width {
        return text.to_string();
    }
    match max_width {
        0 => String::new(),
        1 => "…".to_string(),
        _ => {
            let mut truncated: String = text.chars().take(max_width - 1).collect();
            truncated.push('…');
            truncated
        }
    }
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
            index: 0,
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
            index: 0,
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

    /// Fallback (short-terminal) height and a ramp history for the multirow
    /// tests below.
    const SHORT_ROWS: u16 = 24;
    const TALL_ROWS: u16 = 50;

    fn ramp_history(n: usize) -> Vec<f64> {
        (0..n).map(|i| (i as f64 * 3.0) % 100.0).collect()
    }

    /// Count the terminal lines actually rendered by `render_activity_panel`,
    /// mirroring the `\r\n`-split logic used by the combined panel renderer.
    fn cpu_rendered_lines(cpu: &[CpuInfo], history: &[f64], width: usize, rows: u16) -> usize {
        let mut buf: Vec<u8> = Vec::new();
        render_activity_panel(&mut buf, cpu, history, width, rows);
        String::from_utf8(buf)
            .unwrap()
            .split("\r\n")
            .filter(|l| !l.is_empty())
            .count()
    }

    #[test]
    fn test_panel_height_narrow_terminal() {
        let cpu = vec![make_standard_cpu(8)];
        assert_eq!(panel_height(&cpu, 79, TALL_ROWS), 0);
    }

    #[test]
    fn test_panel_height_empty_cpu() {
        let cpu: Vec<CpuInfo> = Vec::new();
        assert_eq!(panel_height(&cpu, 120, TALL_ROWS), 0);
    }

    #[test]
    fn test_panel_height_standard_cores() {
        let cpu = vec![make_standard_cpu(8)];
        let height = panel_height(&cpu, 120, SHORT_ROWS);
        // Should be > 0 (top border + at least 1 bar line + bottom border)
        assert!(height >= 3, "Expected height >= 3, got {height}");
    }

    #[test]
    fn test_use_multirow_graphs_threshold() {
        assert!(!use_multirow_graphs(24));
        assert!(!use_multirow_graphs(MULTIROW_MIN_TERMINAL_ROWS - 1));
        assert!(use_multirow_graphs(MULTIROW_MIN_TERMINAL_ROWS));
        assert!(use_multirow_graphs(50));
    }

    // --- height: fallback (today's values) vs multirow across strategies ---

    #[test]
    fn test_panel_height_individual_both_modes() {
        // 8 cores at width 120 -> Individual strategy, 3 bar lines.
        let cpu = vec![make_standard_cpu(8)];
        // Fallback: top + 3 bars + bottom = 5 (unchanged from today).
        assert_eq!(panel_height(&cpu, 120, SHORT_ROWS), 5);
        // Multirow: top + 3 graph + 3 bars + bottom = 8.
        assert_eq!(panel_height(&cpu, 120, TALL_ROWS), 8);
    }

    #[test]
    fn test_panel_height_pe_cluster_both_modes() {
        // 24-core Apple Silicon at width 120 -> PECluster (24 > threshold 16).
        let cpu = vec![make_apple_silicon_cpu(16, 8)];
        assert_eq!(
            core_collapse_strategy(&cpu[0], 120),
            CollapseStrategy::PECluster
        );
        // Fallback: top + 2 cluster bars + bottom = 4 (unchanged from today).
        assert_eq!(panel_height(&cpu, 120, SHORT_ROWS), 4);
        // Multirow target: top + 3 graph + 2 cluster bars + bottom = 7.
        assert_eq!(panel_height(&cpu, 120, TALL_ROWS), 7);
    }

    #[test]
    fn test_panel_height_socket_group_both_modes() {
        // 64 cores, 2 sockets at width 120 -> SocketGroup, 2 bar lines.
        let mut cpu = make_standard_cpu(64);
        cpu.socket_count = 2;
        let cpu = vec![cpu];
        assert_eq!(
            core_collapse_strategy(&cpu[0], 120),
            CollapseStrategy::SocketGroup
        );
        // Fallback: top + 2 socket bars + bottom = 4 (unchanged from today).
        assert_eq!(panel_height(&cpu, 120, SHORT_ROWS), 4);
        // Multirow: top + 3 graph + 2 socket bars + bottom = 7.
        assert_eq!(panel_height(&cpu, 120, TALL_ROWS), 7);
    }

    // --- rendered line count == panel_height, both modes, all strategies ---

    #[test]
    fn test_render_line_count_matches_height_individual() {
        let cpu = vec![make_standard_cpu(8)];
        let history = ramp_history(40);
        for &rows in &[SHORT_ROWS, TALL_ROWS] {
            let expected = panel_height(&cpu, 120, rows) as usize;
            let actual = cpu_rendered_lines(&cpu, &history, 120, rows);
            assert_eq!(actual, expected, "individual mismatch at rows={rows}");
        }
    }

    #[test]
    fn test_render_line_count_matches_height_pe_cluster() {
        let cpu = vec![make_apple_silicon_cpu(16, 8)];
        let history = ramp_history(40);
        for &rows in &[SHORT_ROWS, TALL_ROWS] {
            let expected = panel_height(&cpu, 120, rows) as usize;
            let actual = cpu_rendered_lines(&cpu, &history, 120, rows);
            assert_eq!(actual, expected, "pe-cluster mismatch at rows={rows}");
        }
    }

    #[test]
    fn test_render_line_count_matches_height_socket_group() {
        let mut cpu = make_standard_cpu(64);
        cpu.socket_count = 2;
        let cpu = vec![cpu];
        let history = ramp_history(40);
        for &rows in &[SHORT_ROWS, TALL_ROWS] {
            let expected = panel_height(&cpu, 120, rows) as usize;
            let actual = cpu_rendered_lines(&cpu, &history, 120, rows);
            assert_eq!(actual, expected, "socket-group mismatch at rows={rows}");
        }
    }

    #[test]
    fn test_render_line_count_matches_height_empty_history() {
        // Empty CPU history still renders the fixed-height graph rows.
        let cpu = vec![make_apple_silicon_cpu(16, 8)];
        let expected = panel_height(&cpu, 120, TALL_ROWS) as usize;
        let actual = cpu_rendered_lines(&cpu, &[], 120, TALL_ROWS);
        assert_eq!(actual, expected);
    }

    #[test]
    fn test_render_activity_panel_does_not_panic_empty() {
        let cpu: Vec<CpuInfo> = Vec::new();
        let mut buf: Vec<u8> = Vec::new();
        render_activity_panel(&mut buf, &cpu, &ramp_history(20), 120, TALL_ROWS);
    }

    #[test]
    fn test_render_activity_panel_individual() {
        let cpu = vec![make_standard_cpu(8)];
        let mut buf: Vec<u8> = Vec::new();
        render_activity_panel(&mut buf, &cpu, &ramp_history(20), 120, TALL_ROWS);
        assert!(!buf.is_empty());
    }

    #[test]
    fn test_render_activity_panel_pe_cluster() {
        let cpu = vec![make_apple_silicon_cpu(16, 8)];
        let mut buf: Vec<u8> = Vec::new();
        render_activity_panel(&mut buf, &cpu, &ramp_history(20), 120, TALL_ROWS);
        assert!(!buf.is_empty());
    }

    #[test]
    fn test_render_activity_panel_socket_group() {
        let mut cpu = make_standard_cpu(64);
        cpu.socket_count = 2;
        let cpu_vec = vec![cpu];
        let mut buf: Vec<u8> = Vec::new();
        render_activity_panel(&mut buf, &cpu_vec, &ramp_history(20), 120, TALL_ROWS);
        assert!(!buf.is_empty());
    }

    #[test]
    fn test_render_activity_panel_no_panic_narrow_and_sizes() {
        // Narrow (81 cols), short (24 rows), tall (50 rows), empty history.
        let cpu = vec![make_apple_silicon_cpu(16, 8)];
        for &(w, r) in &[(81usize, SHORT_ROWS), (81, TALL_ROWS), (200, TALL_ROWS)] {
            let mut buf: Vec<u8> = Vec::new();
            render_activity_panel(&mut buf, &cpu, &[], w, r);
            render_activity_panel(&mut buf, &cpu, &ramp_history(60), w, r);
        }
    }

    // --- regression: no left margin (#280) ---

    /// Display width of a rendered line with ANSI color escapes stripped,
    /// mirroring the equivalent helper in `gpu_sparkline_panel`'s tests
    /// (every payload char is one terminal column).
    fn visible_width(line: &str) -> usize {
        let mut count = 0usize;
        let mut in_escape = false;
        for c in line.chars() {
            if in_escape {
                if c == 'm' {
                    in_escape = false;
                }
            } else if c == '\u{1b}' {
                in_escape = true;
            } else {
                count += 1;
            }
        }
        count
    }

    /// First non-escape (visible) character of a rendered line, or `None` if
    /// the line is empty or entirely escape sequences.
    fn first_visible_char(line: &str) -> Option<char> {
        let mut in_escape = false;
        for c in line.chars() {
            if in_escape {
                if c == 'm' {
                    in_escape = false;
                }
            } else if c == '\u{1b}' {
                in_escape = true;
            } else {
                return Some(c);
            }
        }
        None
    }

    #[test]
    fn test_cpu_panel_lines_start_at_column_zero_and_fit_panel_width() {
        // Regression for #280: the CPU panel border must start at column 0
        // (no leading margin spaces) and every rendered line must be exactly
        // `panel_width` (= width / 2) display columns wide, in both the
        // fallback and multirow rendering modes, across all three collapse
        // strategies. The sweep deliberately includes odd widths, every
        // individual-core packing transition, trailing partial lines, long
        // titles, and grouped block sections too large to fit without
        // truncation.
        let individual_one = vec![make_standard_cpu(1)];
        let individual_partial = vec![make_standard_cpu(5)];
        let individual_max = vec![make_standard_cpu(16)];
        let pe_cluster_cpu = vec![make_apple_silicon_cpu(18, 18)];
        let mut socket_24 = make_standard_cpu(24);
        socket_24.socket_count = 2;
        let socket_24 = vec![socket_24];
        let mut socket_64 = make_standard_cpu(64);
        socket_64.socket_count = 2;
        let socket_64 = vec![socket_64];
        let mut socket_128 = make_standard_cpu(128);
        socket_128.socket_count = 4;
        let socket_128 = vec![socket_128];
        let cases = [
            &individual_one,
            &individual_partial,
            &individual_max,
            &pe_cluster_cpu,
            &socket_24,
            &socket_64,
            &socket_128,
        ];

        let history = ramp_history(40);
        let border_chars = ['\u{256d}', '\u{2502}', '\u{2570}']; // ╭ │ ╰

        for cpu in cases {
            for width in 81usize..=200 {
                let panel_width = width / 2;
                for &rows in &[SHORT_ROWS, TALL_ROWS] {
                    let mut buf: Vec<u8> = Vec::new();
                    render_activity_panel(&mut buf, cpu, &history, width, rows);
                    let text = String::from_utf8(buf).unwrap();
                    let lines: Vec<_> = text.split("\r\n").filter(|l| !l.is_empty()).collect();
                    assert_eq!(
                        lines.len(),
                        panel_height(cpu, width as u16, rows) as usize,
                        "rendered height must match reserved height at width={width} rows={rows}"
                    );
                    for line in lines {
                        let first = first_visible_char(line);
                        assert!(
                            first.is_some_and(|c| border_chars.contains(&c)),
                            "line must start with a border char, not a margin, at \
                             width={width} rows={rows}: {line:?}"
                        );
                        assert_eq!(
                            visible_width(line),
                            panel_width,
                            "line must be exactly panel_width={panel_width} columns \
                             at width={width} rows={rows}: {line:?}"
                        );
                    }
                }
            }
        }
    }

    // --- graph_row_color: btop-style height gradient ---

    #[test]
    fn test_graph_row_color_rows_zero_does_not_panic() {
        let base = Color::Green;
        assert_eq!(graph_row_color(0, 0, base), base);
        assert_eq!(graph_row_color(5, 0, base), base);
    }

    #[test]
    fn test_graph_row_color_rows_1_always_base() {
        let base = Color::Green;
        assert_eq!(graph_row_color(0, 1, base), base);
    }

    #[test]
    fn test_graph_row_color_rows_2() {
        let base = Color::Green;
        assert_eq!(graph_row_color(0, 2, base), base);
        assert_eq!(graph_row_color(1, 2, base), GAUGE_MEDIUM_COLOR);
    }

    #[test]
    fn test_graph_row_color_rows_3() {
        let base = Color::Green;
        assert_eq!(graph_row_color(0, 3, base), base);
        assert_eq!(graph_row_color(1, 3, base), GAUGE_MEDIUM_COLOR);
        assert_eq!(graph_row_color(2, 3, base), GAUGE_HIGH_COLOR);
    }

    #[test]
    fn test_graph_row_color_rows_4() {
        let base = Color::Green;
        // Bottom row always base, top row always red once rows >= 3.
        assert_eq!(graph_row_color(0, 4, base), base);
        assert_eq!(graph_row_color(3, 4, base), GAUGE_HIGH_COLOR);
    }

    #[test]
    fn test_graph_row_color_rows_6() {
        let base = Color::Green;
        assert_eq!(graph_row_color(0, 6, base), base);
        assert_eq!(graph_row_color(5, 6, base), GAUGE_HIGH_COLOR);
    }

    #[test]
    fn test_graph_row_color_bottom_always_base_top_always_red_for_3_plus() {
        let base = Color::Green;
        for rows in [1usize, 2, 3, 4, 5, 6, 10, 32] {
            assert_eq!(
                graph_row_color(0, rows, base),
                base,
                "bottom row must stay the base color at rows={rows}"
            );
            if rows >= 3 {
                assert_eq!(
                    graph_row_color(rows - 1, rows, base),
                    GAUGE_HIGH_COLOR,
                    "top row must be red once rows >= 3 (rows={rows})"
                );
            }
        }
    }

    #[test]
    fn test_graph_row_color_monotonic_band_order() {
        // Rank a color by how "hot" its band is; the band must never cool
        // down as `row_from_bottom` increases (bottom-to-top).
        let base = Color::Green;
        let band = |c: Color| -> u8 {
            if c == base {
                0
            } else if c == GAUGE_MEDIUM_COLOR {
                1
            } else if c == GAUGE_HIGH_COLOR {
                2
            } else {
                panic!("unexpected color {c:?} outside the 3-anchor palette")
            }
        };

        for rows in [1usize, 2, 3, 4, 5, 6, 10, 32] {
            let mut prev_band = 0u8;
            for row_from_bottom in 0..rows {
                let cur_band = band(graph_row_color(row_from_bottom, rows, base));
                assert!(
                    cur_band >= prev_band,
                    "band cooled going up at rows={rows}, row_from_bottom={row_from_bottom}"
                );
                prev_band = cur_band;
            }
        }
    }

    // --- multirow_graph_lines: rendered rows carry the height gradient ---

    /// Render the exact SGR (Select Graphic Rendition) escape sequence
    /// crossterm emits for a foreground color, so tests can assert on the
    /// literal bytes that end up in a rendered line.
    fn foreground_sgr(color: Color) -> String {
        let mut buf: Vec<u8> = Vec::new();
        queue!(buf, crossterm::style::SetForegroundColor(color)).unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn test_multirow_graph_lines_rows_carry_distinct_gradient_colors() {
        let base = Color::Green;
        let history = ramp_history(40);
        let lines = multirow_graph_lines(
            &history,
            (0.0, 100.0),
            base,
            Color::Magenta,
            40,
            "50.0%",
            "0-100",
        );
        assert_eq!(lines.len(), GRAPH_ROWS);

        let base_sgr = foreground_sgr(base);
        let medium_sgr = foreground_sgr(GAUGE_MEDIUM_COLOR);
        let high_sgr = foreground_sgr(GAUGE_HIGH_COLOR);

        // Top row (index 0, top-most terminal row) is farthest from the
        // bottom -> highest band -> red.
        assert!(
            lines[0].contains(&high_sgr),
            "top row must carry the red gradient escape: {:?}",
            lines[0]
        );
        // Middle row -> yellow.
        assert!(
            lines[1].contains(&medium_sgr),
            "middle row must carry the yellow gradient escape: {:?}",
            lines[1]
        );
        // Bottom row (last index) -> base theme color, unchanged from today.
        assert!(
            lines[GRAPH_ROWS - 1].contains(&base_sgr),
            "bottom row must carry the base theme color escape: {:?}",
            lines[GRAPH_ROWS - 1]
        );

        // The three rows must carry mutually distinct color escapes.
        assert_ne!(lines[0], lines[1]);
        assert_ne!(lines[1], lines[GRAPH_ROWS - 1]);
        assert_ne!(lines[0], lines[GRAPH_ROWS - 1]);
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
