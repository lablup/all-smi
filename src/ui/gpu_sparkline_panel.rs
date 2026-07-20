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

//! GPU / ANE / Pkg Power sparkline stack for the right half of the local
//! Activity panel.
//!
//! Renders a compact stack of braille sparkline rows, each formatted as:
//!
//! ```text
//! <label>  <braille sparkline>  <latest>  <scale badge>
//! ```
//!
//! The scale badge shows the row's soft-zoom Y-axis range actually in use
//! (e.g. `40-60`), computed by [`soft_range`](crate::ui::scale::soft_range)
//! from the visible history window with a per-metric minimum span, coarse-grid
//! hysteresis, and hard-domain clamping. It keeps the axis honest: the badge
//! always reports the exact bounds the sparkline is drawn against.
//!
//! Rows rendered (platform-dependent):
//!
//! | Row       | Source                         | Color   |
//! |-----------|--------------------------------|---------|
//! | GPU Util  | `utilization_history`          | Blue    |
//! | GPU Mem   | `memory_history` (%)           | Green   |
//! | GPU Temp  | `temperature_history`          | Magenta |
//! | ANE       | `gpu.ane_utilization` (mW)     | Yellow  |
//! | Pkg Power | `combined_power_mw` / board pwr| Red     |
//!
//! The ANE row is shown on Apple Silicon regardless of current ANE power.
//!
//! ## Rendering model
//!
//! Because the Activity panel renders left and right halves on the same
//! terminal rows, both halves emit their lines into intermediate `Vec`
//! buffers.  The public [`render_combined_activity_panel`] function
//! interleaves the two halves and writes the combined output.

use std::io::Write;

use crossterm::{queue, style::Color, style::Print};

use crate::app_state::AppState;
use crate::common::config::ThemeConfig;
use crate::device::CpuInfo;
use crate::ui::activity_panel::{self, GRAPH_ROWS, use_multirow_graphs};
use crate::ui::braille::sparkline_braille;
use crate::ui::buffer::BufferWriter;
use crate::ui::scale::{
    ANE_SOFT_GRID, ANE_SOFT_MIN_SPAN, PERCENT_DOMAIN, PERCENT_SOFT_GRID, PERCENT_SOFT_MIN_SPAN,
    TEMP_SOFT_GRID, TEMP_SOFT_MIN_SPAN, ane_range, power_range, power_soft_grid,
    power_soft_min_span, scale_badge, soft_range, temp_range,
};
use crate::ui::text::print_colored_text;

/// Width reserved for the label column (e.g. "GPU Util").
const LABEL_WIDTH: usize = 9;

/// Width reserved for the latest-value column (e.g. "100.0%").
const VALUE_WIDTH: usize = 7;

/// Width reserved for the min-max badge (e.g. "0-100").
const MINMAX_WIDTH: usize = 9;

/// Fixed spacing characters between columns.
const SPACING: usize = 5; // 1+1 border padding + 3 inter-column spaces

/// Calculate the number of content rows for the GPU sparkline panel.
///
/// Returns the content row count (excluding borders). `terminal_rows` drives
/// the same mode decision as the renderer ([`use_multirow_graphs`]): in
/// multi-row mode GPU Util becomes a [`GRAPH_ROWS`]-tall graph and the
/// remaining metrics collapse to compact rows of two metrics each.
pub fn gpu_content_rows(state: &AppState, terminal_rows: u16) -> usize {
    if state.gpu_info.is_empty() {
        return 0;
    }
    let ane = show_ane_row(state);
    let npu = show_npu_row(state);
    // build_rows always emits: GPU Util + GPU Mem + GPU Temp + (ANE?) + (NPU?) + Pkg Power
    let metric_rows = 3 + usize::from(ane) + usize::from(npu) + 1;

    if use_multirow_graphs(terminal_rows) {
        // GPU Util is drawn as a GRAPH_ROWS-tall history graph; the remaining
        // metrics are paired two-per-line.
        let secondary = metric_rows - 1;
        GRAPH_ROWS + secondary.div_ceil(2)
    } else {
        metric_rows
    }
}

/// Render the combined Activity panel (CPU left half + GPU right half).
///
/// Both halves are rendered into intermediate line buffers and then
/// interleaved so they appear on the same terminal rows.
///
/// When there is no GPU data, only the CPU left half is rendered.
/// `terminal_rows` is threaded to both halves so the multi-row-graph mode
/// decision (and therefore the emitted line count) matches the height
/// functions used by [`LayoutCalculator`](crate::ui::layout::LayoutCalculator).
pub fn render_combined_activity_panel<W: Write>(
    stdout: &mut W,
    state: &AppState,
    cpu_info: &[CpuInfo],
    width: usize,
    terminal_rows: u16,
) {
    if cpu_info.is_empty() || cpu_info[0].per_core_utilization.is_empty() {
        return;
    }

    let left_width = width / 2;
    let right_width = width - left_width;

    // Render left half (CPU) into line buffer. The CPU total-utilization
    // history is handed down as a slice so `activity_panel` stays decoupled
    // from `AppState`.
    let cpu_history: Vec<f64> = state.cpu_utilization_history.iter().copied().collect();
    let left_lines = render_cpu_lines(cpu_info, &cpu_history, width, terminal_rows);

    // Render right half (GPU) into line buffer
    let right_lines = render_gpu_lines(state, right_width, terminal_rows);

    // Determine total lines needed (max of both halves)
    let total_lines = left_lines.len().max(right_lines.len());

    // Interleave and output
    for i in 0..total_lines {
        if i < left_lines.len() {
            // Write pre-formatted left line (contains ANSI escapes)
            stdout.write_all(left_lines[i].as_bytes()).unwrap();
        } else {
            // Pad with spaces for absent left half
            print_colored_text(stdout, &" ".repeat(left_width), Color::White, None, None);
        }

        if i < right_lines.len() {
            stdout.write_all(right_lines[i].as_bytes()).unwrap();
        }
        // else: right half is absent, line ends at left boundary

        queue!(stdout, Print("\r\n")).unwrap();
    }
}

// ---------------------------------------------------------------------------
// Left-half (CPU) line buffer rendering
// ---------------------------------------------------------------------------

/// Render the CPU activity panel into a vector of pre-formatted lines.
///
/// Each line is an ANSI-escaped string WITHOUT a trailing `\r\n`.
fn render_cpu_lines(
    cpu_info: &[CpuInfo],
    cpu_history: &[f64],
    width: usize,
    terminal_rows: u16,
) -> Vec<String> {
    // Render the full CPU panel into a buffer
    let mut buf = BufferWriter::new();
    activity_panel::render_activity_panel(&mut buf, cpu_info, cpu_history, width, terminal_rows);
    let raw = buf.get_buffer().to_string();

    // Split on "\r\n" and strip trailing empty line
    raw.split("\r\n")
        .filter(|line| !line.is_empty())
        .map(String::from)
        .collect()
}

// ---------------------------------------------------------------------------
// Right-half (GPU) line buffer rendering
// ---------------------------------------------------------------------------

/// Render the GPU sparkline panel into a vector of pre-formatted lines.
///
/// Each line is an ANSI-escaped string WITHOUT a trailing `\r\n`. In multi-row
/// mode ([`use_multirow_graphs`]) GPU Util is drawn as a [`GRAPH_ROWS`]-tall
/// history graph and the remaining metrics collapse to compact rows of two
/// metrics each; otherwise every metric keeps its own single-row sparkline.
fn render_gpu_lines(state: &AppState, panel_width: usize, terminal_rows: u16) -> Vec<String> {
    if state.gpu_info.is_empty() {
        return Vec::new();
    }

    let is_apple = detect_apple_silicon(state);
    let ane = show_ane_row(state);
    let npu = show_npu_row(state);
    let rows = build_rows(state, is_apple, ane, npu);

    let mut lines: Vec<String> = Vec::with_capacity(rows.len() + 2);

    // Top border
    lines.push(render_line_to_string(|w| {
        draw_top_border(w, panel_width);
    }));

    if use_multirow_graphs(terminal_rows) {
        // GPU Util (row 0) becomes a multi-row history graph.
        if let Some(util) = rows.first() {
            lines.extend(draw_gpu_util_graph(util, panel_width));
        }
        // Remaining metrics: two compact half-cells per line.
        let secondary = if rows.is_empty() { &[][..] } else { &rows[1..] };
        for pair in secondary.chunks(2) {
            lines.push(render_line_to_string(|w| {
                draw_compact_pair(w, pair, panel_width);
            }));
        }
    } else {
        // Content rows (one single-row sparkline per metric).
        for row in &rows {
            lines.push(render_line_to_string(|w| {
                draw_sparkline_row(w, row, panel_width);
            }));
        }
    }

    // Bottom border
    lines.push(render_line_to_string(|w| {
        draw_bottom_border(w, panel_width);
    }));

    lines
}

/// Helper: render a drawing function into a String (no trailing newline).
fn render_line_to_string<F>(f: F) -> String
where
    F: FnOnce(&mut BufferWriter),
{
    let mut buf = BufferWriter::new();
    f(&mut buf);
    buf.get_buffer().to_string()
}

// ---------------------------------------------------------------------------
// Row data model
// ---------------------------------------------------------------------------

struct SparklineRow {
    label: &'static str,
    /// Short label used by the compact 2-per-line multi-row layout (e.g.
    /// `Mem`, `Temp`, `Pkg`) where the full `label` will not fit a half-cell.
    short_label: &'static str,
    color: Color,
    history: Vec<f64>,
    latest_str: String,
    min_max_str: String,
    range: Option<(f64, f64)>,
    /// Optional badge appended after the min-max (e.g. thermal pressure).
    badge: Option<(String, Color)>,
}

// ---------------------------------------------------------------------------
// Row construction
// ---------------------------------------------------------------------------

fn build_rows(state: &AppState, is_apple: bool, has_ane: bool, has_npu: bool) -> Vec<SparklineRow> {
    let mut rows = Vec::with_capacity(6);
    let gpu = state.gpu_info.first();

    // 1. GPU Utilization — soft axis zoomed into the visible window (clamped to
    //    0..100) so small variations around a typical load stay visible.
    let gpu_util: Vec<f64> = state.utilization_history.iter().copied().collect();
    let latest_util = gpu_util.last().copied().unwrap_or(0.0);
    let util_range = soft_range(
        &gpu_util,
        PERCENT_SOFT_MIN_SPAN,
        PERCENT_SOFT_GRID,
        PERCENT_DOMAIN,
    );
    rows.push(SparklineRow {
        label: "GPU Util",
        short_label: "GPU",
        color: ThemeConfig::gpu_color(),
        latest_str: format!("{latest_util:.1}%"),
        min_max_str: scale_badge(util_range.0, util_range.1),
        history: gpu_util,
        range: Some(util_range),
        badge: None,
    });

    // 2. GPU Memory — soft axis over the memory-utilization (%) window.
    let gpu_mem: Vec<f64> = state.memory_history.iter().copied().collect();
    let latest_mem = gpu_mem.last().copied().unwrap_or(0.0);
    let mem_range = soft_range(
        &gpu_mem,
        PERCENT_SOFT_MIN_SPAN,
        PERCENT_SOFT_GRID,
        PERCENT_DOMAIN,
    );
    rows.push(SparklineRow {
        label: "GPU Mem",
        short_label: "Mem",
        color: ThemeConfig::memory_color(),
        latest_str: format!("{latest_mem:.1}%"),
        min_max_str: scale_badge(mem_range.0, mem_range.1),
        history: gpu_mem,
        range: Some(mem_range),
        badge: None,
    });

    // 3. GPU Temperature — soft axis clamped to (0, thermal-threshold ceiling)
    //    (100°C fallback). The floor is 0 so a cool GPU can zoom below 30°C
    //    while the window still tracks small changes.
    let gpu_temp: Vec<f64> = state.temperature_history.iter().copied().collect();
    let latest_temp = gpu_temp.last().copied().unwrap_or(0.0);
    let temp_ceiling = temp_range(gpu).1;
    let temp_rng = soft_range(
        &gpu_temp,
        TEMP_SOFT_MIN_SPAN,
        TEMP_SOFT_GRID,
        (0.0, temp_ceiling),
    );
    rows.push(SparklineRow {
        label: "GPU Temp",
        short_label: "Temp",
        color: ThemeConfig::thermal_color(),
        latest_str: format!("{latest_temp:.0}\u{00B0}C"),
        min_max_str: scale_badge(temp_rng.0, temp_rng.1),
        history: gpu_temp,
        range: Some(temp_rng),
        badge: None,
    });

    // 4. ANE (Apple Silicon -- always shown regardless of current power)
    if has_ane {
        let ane_w = state.ane_power_history.back().copied().unwrap_or_else(|| {
            state
                .gpu_info
                .first()
                .map(|g| g.ane_utilization / 1000.0)
                .unwrap_or(0.0)
        });
        let ane_history: Vec<f64> = if state.ane_power_history.is_empty() {
            vec![ane_w]
        } else {
            state.ane_power_history.iter().copied().collect()
        };
        // Soft axis clamped to (0, ane ceiling); the ceiling still comes from
        // ane_range so a busy Neural Engine keeps a comparable top.
        let ane_ceiling = ane_range(&ane_history).1;
        let ane_rng = soft_range(
            &ane_history,
            ANE_SOFT_MIN_SPAN,
            ANE_SOFT_GRID,
            (0.0, ane_ceiling),
        );
        rows.push(SparklineRow {
            label: "ANE",
            short_label: "ANE",
            color: ThemeConfig::accelerator_color(),
            latest_str: format!("{ane_w:.1}W"),
            min_max_str: scale_badge(ane_rng.0, ane_rng.1),
            history: ane_history,
            range: Some(ane_rng),
            badge: None,
        });
    }

    // 4b. NPU (Intel/Windows -- scaffolding for future NPU reader)
    if has_npu {
        rows.push(SparklineRow {
            label: "NPU",
            short_label: "NPU",
            color: ThemeConfig::accelerator_color(),
            latest_str: "0.0W".to_string(),
            min_max_str: String::new(),
            history: vec![0.0],
            range: None,
            badge: None,
        });
    }

    // 5. Pkg Power — soft axis clamped to (0, power ceiling), where the ceiling
    //    is the summed enforced per-GPU limits (package power is summed across
    //    all GPUs), else a nice-rounded peak. The soft min span and grid step
    //    scale with that ceiling.
    let power_w = package_power(state, is_apple);
    let power_history: Vec<f64> = if state.package_power_history.is_empty() {
        vec![power_w]
    } else {
        state.package_power_history.iter().copied().collect()
    };
    let power_ceiling = power_range(&state.gpu_info, &power_history).1;
    let power_rng = soft_range(
        &power_history,
        power_soft_min_span(power_ceiling),
        power_soft_grid(power_ceiling),
        (0.0, power_ceiling),
    );
    rows.push(SparklineRow {
        label: "Pkg Power",
        short_label: "Pkg",
        color: ThemeConfig::power_color(),
        latest_str: format!("{power_w:.1}W"),
        min_max_str: scale_badge(power_rng.0, power_rng.1),
        history: power_history,
        range: Some(power_rng),
        badge: None,
    });

    rows
}

// ---------------------------------------------------------------------------
// Drawing helpers (write to buffer, no trailing \r\n)
// ---------------------------------------------------------------------------

fn draw_top_border<W: Write>(stdout: &mut W, panel_width: usize) {
    let title = "GPU Metrics";
    let inner_width = panel_width.saturating_sub(2); // 2 corner chars (no left margin unlike CPU panel)
    let title_space = 1 + title.len() + 1;
    let dashes = inner_width.saturating_sub(title_space + 1);

    print_colored_text(
        stdout,
        "\u{256d}\u{2500}",
        ThemeConfig::accent_color(),
        None,
        None,
    );
    print_colored_text(stdout, " ", Color::White, None, None);
    print_colored_text(stdout, title, ThemeConfig::accent_color(), None, None);
    print_colored_text(stdout, " ", Color::White, None, None);
    for _ in 0..dashes {
        print_colored_text(stdout, "\u{2500}", ThemeConfig::accent_color(), None, None);
    }
    print_colored_text(stdout, "\u{256e}", ThemeConfig::accent_color(), None, None);
}

fn draw_bottom_border<W: Write>(stdout: &mut W, panel_width: usize) {
    let inner_width = panel_width.saturating_sub(2);
    print_colored_text(stdout, "\u{2570}", ThemeConfig::accent_color(), None, None);
    for _ in 0..inner_width {
        print_colored_text(stdout, "\u{2500}", ThemeConfig::accent_color(), None, None);
    }
    print_colored_text(stdout, "\u{256f}", ThemeConfig::accent_color(), None, None);
}

fn draw_sparkline_row<W: Write>(stdout: &mut W, row: &SparklineRow, panel_width: usize) {
    // Layout: "| " + label + " " + sparkline + " " + value + " " + minmax + badge + pad + " |"
    let content_width = panel_width.saturating_sub(4); // border chars + inner padding

    // Calculate sparkline width from available space
    let badge_len = row.badge.as_ref().map(|(s, _)| s.len() + 1).unwrap_or(0);
    let fixed = LABEL_WIDTH + VALUE_WIDTH + MINMAX_WIDTH + SPACING + badge_len;
    let sparkline_width = content_width.saturating_sub(fixed).max(4);

    let sparkline = sparkline_braille(&row.history, sparkline_width, row.range);

    // Left border
    print_colored_text(stdout, "\u{2502} ", ThemeConfig::accent_color(), None, None);

    // Label (right-padded to LABEL_WIDTH)
    let label_display = format!("{:<LABEL_WIDTH$}", row.label);
    print_colored_text(stdout, &label_display, row.color, None, None);
    print_colored_text(stdout, " ", Color::White, None, None);

    // Sparkline
    print_colored_text(stdout, &sparkline, row.color, None, None);
    print_colored_text(stdout, " ", Color::White, None, None);

    // Latest value (right-padded)
    let value_display = format!("{:<VALUE_WIDTH$}", row.latest_str);
    print_colored_text(stdout, &value_display, Color::White, None, None);

    // Min-max badge
    let minmax_display = format!("{:<MINMAX_WIDTH$}", row.min_max_str);
    print_colored_text(stdout, &minmax_display, Color::DarkGrey, None, None);

    // Optional badge (thermal pressure etc.)
    if let Some((ref text, color)) = row.badge {
        print_colored_text(stdout, " ", Color::White, None, None);
        print_colored_text(stdout, text, color, None, None);
    }

    // Pad to fill panel, then right border
    let used = 2 + LABEL_WIDTH + 1 + sparkline_width + 1 + VALUE_WIDTH + MINMAX_WIDTH + badge_len;
    let pad = panel_width.saturating_sub(used + 2);
    if pad > 0 {
        print_colored_text(stdout, &" ".repeat(pad), Color::White, None, None);
    }
    print_colored_text(stdout, " \u{2502}", ThemeConfig::accent_color(), None, None);
}

// ---------------------------------------------------------------------------
// Multi-row (btop-style) rendering
// ---------------------------------------------------------------------------

/// Short label column width in a compact half-cell (fits `Temp`, `Pkg`, ...).
const COMPACT_LABEL_WIDTH: usize = 4;
/// Latest-value column width in a compact half-cell (fits `1400.0W`, `100.0%`).
const COMPACT_VALUE_WIDTH: usize = 7;
/// Scale-badge column width in a compact half-cell (fits `0-100`, `0-350`).
const COMPACT_BADGE_WIDTH: usize = 5;

/// Render the GPU Util metric as a stacked multi-row history graph on a fixed
/// `0..100` axis. Returns [`GRAPH_ROWS`] line strings (no trailing newline),
/// matching the format of the panel's other rows.
fn draw_gpu_util_graph(util: &SparklineRow, panel_width: usize) -> Vec<String> {
    // Content width between the "│ " and " │" borders (matches draw_sparkline_row).
    let content_width = panel_width.saturating_sub(4);
    activity_panel::multirow_graph_lines(
        &util.history,
        (0.0, 100.0),
        util.color,
        ThemeConfig::accent_color(),
        "",
        content_width,
        &util.latest_str,
        "0-100",
    )
}

/// Draw one compact line holding up to two metric half-cells side by side.
///
/// The content area (`panel_width - 4`) is split into two halves; a `None`
/// second half is filled with spaces so the right border stays aligned. All
/// width math is saturating and stays panic-free at narrow widths.
fn draw_compact_pair<W: Write>(stdout: &mut W, pair: &[SparklineRow], panel_width: usize) {
    let content_width = panel_width.saturating_sub(4);
    let first_budget = content_width / 2;
    let second_budget = content_width - first_budget;

    print_colored_text(stdout, "\u{2502} ", ThemeConfig::accent_color(), None, None);

    let mut used = 0usize;
    used += draw_compact_cell(stdout, pair.first(), first_budget);
    used += draw_compact_cell(stdout, pair.get(1), second_budget);

    let pad = content_width.saturating_sub(used);
    if pad > 0 {
        print_colored_text(stdout, &" ".repeat(pad), Color::White, None, None);
    }
    print_colored_text(stdout, " \u{2502}", ThemeConfig::accent_color(), None, None);
}

/// Render a single compact half-cell into `budget` columns and return the
/// number of columns emitted. `None` renders an empty (all-spaces) cell.
///
/// Cell layout: `<label> <sparkline> <value><badge>`, keeping the #273 soft
/// range and scale badge. The sparkline absorbs whatever columns the fixed
/// label/value/badge fields leave, with a 1-column minimum floor.
fn draw_compact_cell<W: Write>(stdout: &mut W, row: Option<&SparklineRow>, budget: usize) -> usize {
    let Some(row) = row else {
        if budget > 0 {
            print_colored_text(stdout, &" ".repeat(budget), Color::White, None, None);
        }
        return budget;
    };

    // label + " " + spark + " " + value + badge
    let fixed = COMPACT_LABEL_WIDTH + 1 + 1 + COMPACT_VALUE_WIDTH + COMPACT_BADGE_WIDTH;
    let spark_width = budget.saturating_sub(fixed).max(1);
    let spark = sparkline_braille(&row.history, spark_width, row.range);

    let label = fit_field(row.short_label, COMPACT_LABEL_WIDTH);
    print_colored_text(stdout, &label, row.color, None, None);
    print_colored_text(stdout, " ", Color::White, None, None);
    print_colored_text(stdout, &spark, row.color, None, None);
    print_colored_text(stdout, " ", Color::White, None, None);
    let value = fit_field(&row.latest_str, COMPACT_VALUE_WIDTH);
    print_colored_text(stdout, &value, Color::White, None, None);
    let badge = fit_field(&row.min_max_str, COMPACT_BADGE_WIDTH);
    print_colored_text(stdout, &badge, Color::DarkGrey, None, None);

    COMPACT_LABEL_WIDTH + 1 + spark_width + 1 + COMPACT_VALUE_WIDTH + COMPACT_BADGE_WIDTH
}

/// Truncate or right-pad `s` to exactly `width` characters (char-based, so the
/// `°` in a temperature value counts as a single display column).
fn fit_field(s: &str, width: usize) -> String {
    let count = s.chars().count();
    if count > width {
        s.chars().take(width).collect()
    } else {
        format!("{s:<width$}")
    }
}

// ---------------------------------------------------------------------------
// Platform detection helpers
// ---------------------------------------------------------------------------

fn detect_apple_silicon(state: &AppState) -> bool {
    state.gpu_info.iter().any(|gpu| {
        gpu.detail
            .get("architecture")
            .map(|arch| arch == "Apple Silicon")
            .unwrap_or(false)
    })
}

/// Whether the ANE row should be shown in the GPU Metrics panel.
///
/// Returns `true` on Apple Silicon regardless of current ANE power.
/// An ANE at 0 W is a meaningful "idle" reading and the row is
/// load-bearing for platform identity even when the Neural Engine
/// is completely idle.
fn show_ane_row(state: &AppState) -> bool {
    detect_apple_silicon(state)
}

/// Whether an NPU row should be shown in the GPU Metrics panel.
///
/// Currently returns `false` -- no Intel/Windows NPU reader exists yet.
/// When an NPU telemetry reader is added (Meteor Lake / Core Ultra),
/// flip this to check for NPU presence via `src/api/metrics/npu/common.rs`.
fn show_npu_row(_state: &AppState) -> bool {
    false
}

fn package_power(state: &AppState, is_apple: bool) -> f64 {
    if is_apple {
        // Apple Silicon: combined CPU+GPU+ANE power from native metrics
        let power_mw = state
            .gpu_info
            .iter()
            .filter_map(|gpu| {
                gpu.detail
                    .get("combined_power_mw")
                    .and_then(|s| s.parse::<f64>().ok())
            })
            .next()
            .unwrap_or(0.0);
        power_mw / 1000.0
    } else {
        // NVIDIA / other: sum GPU board power
        state.gpu_info.iter().map(|g| g.power_consumption).sum()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_state::AppState;
    use crate::device::{
        AppleSiliconCpuInfo, CoreType, CoreUtilization, CpuInfo, CpuPlatformType, GpuInfo,
    };
    use std::collections::HashMap;

    fn make_nvidia_state() -> AppState {
        let mut state = AppState::new();
        state.is_local_mode = true;

        let mut detail = HashMap::new();
        detail.insert("architecture".to_string(), "NVIDIA".to_string());

        state.gpu_info.push(GpuInfo {
            uuid: "gpu-0".to_string(),
            time: String::new(),
            name: "RTX 4090".to_string(),
            device_type: "GPU".to_string(),
            host_id: "localhost".to_string(),
            hostname: "localhost".to_string(),
            instance: "localhost".to_string(),
            utilization: 75.0,
            ane_utilization: 0.0,
            dla_utilization: None,
            tensorcore_utilization: None,
            temperature: 72,
            used_memory: 8 * 1024 * 1024 * 1024,
            total_memory: 24 * 1024 * 1024 * 1024,
            frequency: 2100,
            power_consumption: 320.0,
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
            detail,
        });

        // Populate histories
        for i in 0..20 {
            state.utilization_history.push_back(i as f64 * 5.0);
            state.memory_history.push_back(i as f64 * 3.0);
            state.temperature_history.push_back(50.0 + i as f64);
            state
                .package_power_history
                .push_back(120.0 + i as f64 * 2.0);
        }
        state
    }

    fn make_apple_silicon_state() -> AppState {
        let mut state = AppState::new();
        state.is_local_mode = true;

        let mut detail = HashMap::new();
        detail.insert("architecture".to_string(), "Apple Silicon".to_string());
        detail.insert("combined_power_mw".to_string(), "12500".to_string());

        state.gpu_info.push(GpuInfo {
            uuid: "apple-gpu".to_string(),
            time: String::new(),
            name: "Apple M2 Pro".to_string(),
            device_type: "GPU".to_string(),
            host_id: "localhost".to_string(),
            hostname: "localhost".to_string(),
            instance: "localhost".to_string(),
            utilization: 45.0,
            ane_utilization: 3500.0, // 3500 mW = 3.5 W
            dla_utilization: None,
            tensorcore_utilization: None,
            temperature: 55,
            used_memory: 4 * 1024 * 1024 * 1024,
            total_memory: 16 * 1024 * 1024 * 1024,
            frequency: 1398,
            power_consumption: 8.0,
            gpu_core_count: Some(16),
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
            detail,
        });

        for i in 0..20 {
            state.utilization_history.push_back(i as f64 * 4.0);
            state.memory_history.push_back(i as f64 * 2.5);
            state.temperature_history.push_back(40.0 + i as f64);
            state.package_power_history.push_back(8.0 + i as f64 * 0.5);
            state.ane_power_history.push_back(i as f64 * 0.2);
        }
        state
    }

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

    fn make_apple_cpu() -> CpuInfo {
        let mut per_core = Vec::new();
        for i in 0..4 {
            per_core.push(CoreUtilization {
                core_id: i as u32,
                core_type: CoreType::Efficiency,
                utilization: 20.0 + i as f64 * 5.0,
            });
        }
        for i in 0..8 {
            per_core.push(CoreUtilization {
                core_id: (4 + i) as u32,
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
            total_cores: 12,
            total_threads: 12,
            base_frequency_mhz: 3490,
            max_frequency_mhz: 3490,
            cache_size_mb: 16,
            utilization: 35.0,
            temperature: None,
            power_consumption: None,
            per_socket_info: Vec::new(),
            apple_silicon_info: Some(AppleSiliconCpuInfo {
                s_core_count: 0,
                p_core_count: 8,
                e_core_count: 4,
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

    /// A 24-core Apple Silicon CPU (16P + 8E). At width 120 the core count
    /// exceeds the collapse threshold (16), so the panel uses the PECluster
    /// strategy -- the scenario the multirow "no growth" target is written for.
    fn make_apple_cpu_many_cores() -> CpuInfo {
        let mut per_core = Vec::new();
        for i in 0..8 {
            per_core.push(CoreUtilization {
                core_id: i as u32,
                core_type: CoreType::Efficiency,
                utilization: 20.0 + i as f64 * 3.0,
            });
        }
        for i in 0..16 {
            per_core.push(CoreUtilization {
                core_id: (8 + i) as u32,
                core_type: CoreType::Performance,
                utilization: 40.0 + i as f64 * 2.0,
            });
        }
        let mut cpu = make_apple_cpu();
        cpu.total_cores = 24;
        cpu.total_threads = 24;
        if let Some(a) = cpu.apple_silicon_info.as_mut() {
            a.p_core_count = 16;
            a.e_core_count = 8;
        }
        cpu.per_core_utilization = per_core;
        cpu
    }

    /// Terminal heights that select each mode.
    const SHORT_ROWS: u16 = 24;
    const TALL_ROWS: u16 = 50;

    #[test]
    fn test_gpu_content_rows_empty() {
        let state = AppState::new();
        assert_eq!(gpu_content_rows(&state, SHORT_ROWS), 0);
        assert_eq!(gpu_content_rows(&state, TALL_ROWS), 0);
    }

    #[test]
    fn test_gpu_content_rows_nvidia() {
        let state = make_nvidia_state();
        // Fallback: GPU Util + GPU Mem + GPU Temp + Pkg Power = 4 (unchanged).
        assert_eq!(gpu_content_rows(&state, SHORT_ROWS), 4);
        // Multirow: 3 graph rows + ceil(3 secondary / 2) = 3 + 2 = 5.
        assert_eq!(gpu_content_rows(&state, TALL_ROWS), 5);
    }

    #[test]
    fn test_gpu_content_rows_apple_with_ane() {
        let state = make_apple_silicon_state();
        // Fallback: GPU Util + GPU Mem + GPU Temp + ANE + Pkg Power = 5.
        assert_eq!(gpu_content_rows(&state, SHORT_ROWS), 5);
        // Multirow: 3 graph rows + ceil(4 secondary / 2) = 3 + 2 = 5.
        // With +2 borders this lands the GPU half at the 7-row target.
        assert_eq!(gpu_content_rows(&state, TALL_ROWS), 5);
    }

    #[test]
    fn test_gpu_content_rows_nvidia_with_npu_multirow() {
        let state = make_nvidia_state();
        // Secondary = Mem + Temp + NPU + Pkg = 4 -> ceil(4/2) = 2 compact lines.
        assert_eq!(build_rows(&state, false, false, true).len(), 5);
        // Height helper counts NPU only when show_npu_row is true (currently
        // false), so exercise the count via the compact-line arithmetic here:
        // 3 graph + 2 = 5, matching the Apple/NVIDIA multirow height.
        assert_eq!(gpu_content_rows(&state, TALL_ROWS), 5);
    }

    #[test]
    fn test_detect_apple_silicon() {
        assert!(!detect_apple_silicon(&make_nvidia_state()));
        assert!(detect_apple_silicon(&make_apple_silicon_state()));
    }

    #[test]
    fn test_show_ane_row() {
        assert!(!show_ane_row(&make_nvidia_state()));
        assert!(show_ane_row(&make_apple_silicon_state()));
    }

    #[test]
    fn test_show_ane_row_even_when_ane_idle() {
        // ANE row should be shown even when ane_utilization is 0
        let mut state = make_apple_silicon_state();
        state.gpu_info[0].ane_utilization = 0.0;
        assert!(show_ane_row(&state));
    }

    #[test]
    fn test_show_npu_row_returns_false() {
        assert!(!show_npu_row(&make_nvidia_state()));
        assert!(!show_npu_row(&make_apple_silicon_state()));
    }

    #[test]
    fn test_gpu_content_rows_apple_with_zero_ane_still_shows_row() {
        let mut state = make_apple_silicon_state();
        state.gpu_info[0].ane_utilization = 0.0;
        // GPU Util + GPU Mem + GPU Temp + ANE (always-on) + Pkg Power = 5
        assert_eq!(gpu_content_rows(&state, SHORT_ROWS), 5);
    }

    #[test]
    fn test_package_power_apple_silicon() {
        let state = make_apple_silicon_state();
        let watts = package_power(&state, true);
        assert!((watts - 12.5).abs() < 0.01);
    }

    #[test]
    fn test_package_power_nvidia() {
        let state = make_nvidia_state();
        let watts = package_power(&state, false);
        assert!((watts - 320.0).abs() < 0.01);
    }

    #[test]
    fn test_build_rows_nvidia() {
        let state = make_nvidia_state();
        let rows = build_rows(&state, false, false, false);
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0].label, "GPU Util");
        assert_eq!(rows[1].label, "GPU Mem");
        assert_eq!(rows[2].label, "GPU Temp");
        assert_eq!(rows[3].label, "Pkg Power");
        assert!(rows[2].badge.is_none());
    }

    #[test]
    fn test_build_rows_apple_silicon() {
        let state = make_apple_silicon_state();
        let rows = build_rows(&state, true, true, false);
        assert_eq!(rows.len(), 5);
        assert_eq!(rows[0].label, "GPU Util");
        assert_eq!(rows[3].label, "ANE");
        assert_eq!(rows[4].label, "Pkg Power");
        assert!(rows[2].badge.is_none());
        // Scale badges now show the soft-zoom axis actually in use, computed by
        // hand from the fixture histories (see make_apple_silicon_state):
        //   GPU Util  data [0..76]   -> grid [0, 80]
        //   GPU Mem   data [0..47.5] -> grid [0, 50]
        //   GPU Temp  data [40..59]  -> grid [40, 60]  (domain 0..100)
        //   ANE       data [0..3.8]  -> grid [0, 4]    (ceiling 10, min span 2)
        //   Pkg Power data [8..17.5] -> grid [8, 18]   (ceiling 20, min span 4, grid 1)
        assert_eq!(rows[0].min_max_str, "0-80");
        assert_eq!(rows[1].min_max_str, "0-50");
        assert_eq!(rows[2].min_max_str, "40-60");
        assert_eq!(rows[3].min_max_str, "0-4");
        assert_eq!(rows[4].min_max_str, "8-18");
    }

    #[test]
    fn test_build_rows_with_npu_scaffolding() {
        let state = make_nvidia_state();
        let rows = build_rows(&state, false, false, true);
        assert_eq!(rows.len(), 5);
        assert_eq!(rows[3].label, "NPU");
        assert_eq!(rows[4].label, "Pkg Power");
    }

    #[test]
    fn test_render_gpu_lines_nvidia() {
        let state = make_nvidia_state();
        let lines = render_gpu_lines(&state, 60, SHORT_ROWS);
        // Fallback: top border + 4 content rows + bottom border = 6.
        assert_eq!(lines.len(), 6);
        assert!(!lines[0].is_empty()); // top border
    }

    #[test]
    fn test_render_gpu_lines_apple_silicon() {
        let state = make_apple_silicon_state();
        let lines = render_gpu_lines(&state, 60, SHORT_ROWS);
        // Fallback: top border + 5 content rows + bottom border = 7.
        assert_eq!(lines.len(), 7);
    }

    #[test]
    fn test_render_gpu_lines_empty() {
        let state = AppState::new();
        let lines = render_gpu_lines(&state, 60, SHORT_ROWS);
        assert!(lines.is_empty());
        assert!(render_gpu_lines(&state, 60, TALL_ROWS).is_empty());
    }

    // --- multirow: rendered line count == gpu_content_rows + 2 borders ------

    #[test]
    fn test_render_gpu_lines_multirow_matches_content_rows() {
        for state in [make_nvidia_state(), make_apple_silicon_state()] {
            for &rows in &[SHORT_ROWS, TALL_ROWS] {
                let expected = gpu_content_rows(&state, rows) + 2; // + borders
                let lines = render_gpu_lines(&state, 60, rows);
                assert_eq!(
                    lines.len(),
                    expected,
                    "line count must match gpu_content_rows at rows={rows}"
                );
            }
        }
    }

    #[test]
    fn test_render_gpu_lines_apple_multirow_is_seven() {
        let state = make_apple_silicon_state();
        // top border + 3 graph rows + 2 compact rows + bottom border = 7.
        let lines = render_gpu_lines(&state, 60, TALL_ROWS);
        assert_eq!(lines.len(), 7);
    }

    #[test]
    fn test_render_gpu_lines_multirow_no_panic_narrow() {
        // Narrow right-half widths must not panic in multirow mode.
        for state in [make_nvidia_state(), make_apple_silicon_state()] {
            for &w in &[40usize, 41, 30, 12] {
                let _ = render_gpu_lines(&state, w, TALL_ROWS);
            }
        }
    }

    /// Count the interleaved terminal lines emitted by the combined panel.
    fn combined_line_count(state: &AppState, cpu: &[CpuInfo], width: usize, rows: u16) -> usize {
        let mut buf: Vec<u8> = Vec::new();
        render_combined_activity_panel(&mut buf, state, cpu, width, rows);
        String::from_utf8(buf)
            .unwrap()
            .split("\r\n")
            .filter(|l| !l.is_empty())
            .count()
    }

    #[test]
    fn test_render_combined_does_not_panic_nvidia() {
        let mut state = make_nvidia_state();
        let cpu = vec![make_standard_cpu(8)];
        state.cpu_info = cpu.clone();
        for &rows in &[SHORT_ROWS, TALL_ROWS] {
            let mut buf: Vec<u8> = Vec::new();
            render_combined_activity_panel(&mut buf, &state, &cpu, 120, rows);
            assert!(!buf.is_empty());
        }
    }

    #[test]
    fn test_render_combined_does_not_panic_apple() {
        let mut state = make_apple_silicon_state();
        let cpu = vec![make_apple_cpu()];
        state.cpu_info = cpu.clone();
        for &rows in &[SHORT_ROWS, TALL_ROWS] {
            let mut buf: Vec<u8> = Vec::new();
            render_combined_activity_panel(&mut buf, &state, &cpu, 120, rows);
            assert!(!buf.is_empty());
        }
    }

    #[test]
    fn test_render_combined_no_gpu() {
        let state = AppState::new();
        let cpu = vec![make_standard_cpu(4)];
        let mut buf: Vec<u8> = Vec::new();
        render_combined_activity_panel(&mut buf, &state, &cpu, 120, TALL_ROWS);
        // Should still render - CPU only
        assert!(!buf.is_empty());
    }

    #[test]
    fn test_render_combined_no_cpu() {
        let state = make_nvidia_state();
        let cpu: Vec<CpuInfo> = Vec::new();
        let mut buf: Vec<u8> = Vec::new();
        render_combined_activity_panel(&mut buf, &state, &cpu, 120, TALL_ROWS);
        // No CPU info -> no output
        assert!(buf.is_empty());
    }

    #[test]
    fn test_render_combined_line_count_matches_layout_math() {
        // The combined panel emits max(cpu_height, gpu_height) lines, exactly
        // what LayoutCalculator reserves. Verify for both platforms/modes.
        let mut nvidia = make_nvidia_state();
        let nvidia_cpu = vec![make_standard_cpu(8)];
        nvidia.cpu_info = nvidia_cpu.clone();

        let mut apple = make_apple_silicon_state();
        let apple_cpu = vec![make_apple_cpu()];
        apple.cpu_info = apple_cpu.clone();

        for &rows in &[SHORT_ROWS, TALL_ROWS] {
            for (state, cpu) in [(&nvidia, &nvidia_cpu), (&apple, &apple_cpu)] {
                let cpu_h = activity_panel::panel_height(cpu, 120, rows) as usize;
                let gpu_content = gpu_content_rows(state, rows);
                let gpu_h = if gpu_content > 0 { gpu_content + 2 } else { 0 };
                let expected = cpu_h.max(gpu_h);
                let actual = combined_line_count(state, cpu, 120, rows);
                assert_eq!(actual, expected, "combined mismatch at rows={rows}");
            }
        }
    }

    #[test]
    fn test_render_combined_apple_multirow_no_growth() {
        // Apple Silicon: the combined panel stays 7 rows in both modes (the
        // multirow graphs consume the space the CPU half left blank, so there
        // is no net growth versus the fallback layout).
        let mut apple = make_apple_silicon_state();
        // 24-core Apple Silicon -> PECluster at width 120.
        let cpu = vec![make_apple_cpu_many_cores()];
        apple.cpu_info = cpu.clone();
        assert_eq!(combined_line_count(&apple, &cpu, 120, SHORT_ROWS), 7);
        assert_eq!(combined_line_count(&apple, &cpu, 120, TALL_ROWS), 7);
    }

    #[test]
    fn test_render_combined_no_panic_narrow_short_tall() {
        // No-panic at narrow (81 cols), short (24 rows) and tall (50 rows),
        // including empty histories and a missing GPU.
        let mut apple = make_apple_silicon_state();
        apple.utilization_history.clear();
        apple.memory_history.clear();
        apple.temperature_history.clear();
        apple.ane_power_history.clear();
        apple.package_power_history.clear();
        apple.cpu_utilization_history.clear();
        let cpu = vec![make_apple_cpu()];
        let no_gpu = AppState::new();
        for &(w, r) in &[(81usize, SHORT_ROWS), (81, TALL_ROWS), (200, TALL_ROWS)] {
            let mut buf: Vec<u8> = Vec::new();
            render_combined_activity_panel(&mut buf, &apple, &cpu, w, r);
            let mut buf2: Vec<u8> = Vec::new();
            render_combined_activity_panel(&mut buf2, &no_gpu, &cpu, w, r);
        }
    }
}
