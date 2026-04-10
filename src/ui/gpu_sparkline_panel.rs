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
//! <label>  <braille sparkline>  <latest>  <min-max badge>
//! ```
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
//! The ANE row is only shown on Apple Silicon when `ane_utilization > 0`.
//! The thermal pressure badge is appended to the GPU Temp row on Apple
//! Silicon when the `thermal_pressure` detail key is present.
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
use crate::device::CpuInfo;
use crate::ui::activity_panel;
use crate::ui::braille::sparkline_braille;
use crate::ui::buffer::BufferWriter;
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
/// Returns the content row count (excluding borders).
pub fn gpu_content_rows(state: &AppState) -> usize {
    if state.gpu_info.is_empty() {
        return 0;
    }
    let is_apple = detect_apple_silicon(state);
    let has_ane = is_apple && has_ane_data(state);
    // GPU Util + GPU Mem + GPU Temp + (ANE?) + Pkg Power
    3 + usize::from(has_ane) + 1
}

/// Render the combined Activity panel (CPU left half + GPU right half).
///
/// Both halves are rendered into intermediate line buffers and then
/// interleaved so they appear on the same terminal rows.
///
/// When there is no GPU data, only the CPU left half is rendered.
pub fn render_combined_activity_panel<W: Write>(
    stdout: &mut W,
    state: &AppState,
    cpu_info: &[CpuInfo],
    width: usize,
) {
    if cpu_info.is_empty() || cpu_info[0].per_core_utilization.is_empty() {
        return;
    }

    let left_width = width / 2;
    let right_width = width - left_width;

    // Render left half (CPU) into line buffer
    let left_lines = render_cpu_lines(cpu_info, width);

    // Render right half (GPU) into line buffer
    let right_lines = render_gpu_lines(state, right_width);

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
fn render_cpu_lines(cpu_info: &[CpuInfo], width: usize) -> Vec<String> {
    // Render the full CPU panel into a buffer
    let mut buf = BufferWriter::new();
    activity_panel::render_activity_panel(&mut buf, cpu_info, width);
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
/// Each line is an ANSI-escaped string WITHOUT a trailing `\r\n`.
fn render_gpu_lines(state: &AppState, panel_width: usize) -> Vec<String> {
    if state.gpu_info.is_empty() {
        return Vec::new();
    }

    let is_apple = detect_apple_silicon(state);
    let has_ane = is_apple && has_ane_data(state);
    let rows = build_rows(state, is_apple, has_ane);

    let mut lines: Vec<String> = Vec::with_capacity(rows.len() + 2);

    // Top border
    lines.push(render_line_to_string(|w| {
        draw_top_border(w, panel_width);
    }));

    // Content rows
    for row in &rows {
        lines.push(render_line_to_string(|w| {
            draw_sparkline_row(w, row, panel_width);
        }));
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

fn build_rows(state: &AppState, is_apple: bool, has_ane: bool) -> Vec<SparklineRow> {
    let mut rows = Vec::with_capacity(5);

    // 1. GPU Utilization
    // Collect once; clone for power-proxy row to avoid a second VecDeque iteration.
    let gpu_util: Vec<f64> = state.utilization_history.iter().copied().collect();
    let power_proxy = gpu_util.clone();
    let latest_util = gpu_util.last().copied().unwrap_or(0.0);
    rows.push(SparklineRow {
        label: "GPU Util",
        color: Color::Blue,
        latest_str: format!("{latest_util:.1}%"),
        min_max_str: min_max_badge(&gpu_util),
        history: gpu_util,
        range: Some((0.0, 100.0)),
        badge: None,
    });

    // 2. GPU Memory
    let gpu_mem: Vec<f64> = state.memory_history.iter().copied().collect();
    let latest_mem = gpu_mem.last().copied().unwrap_or(0.0);
    rows.push(SparklineRow {
        label: "GPU Mem",
        color: Color::Green,
        latest_str: format!("{latest_mem:.1}%"),
        min_max_str: min_max_badge(&gpu_mem),
        history: gpu_mem,
        range: Some((0.0, 100.0)),
        badge: None,
    });

    // 3. GPU Temperature
    let gpu_temp: Vec<f64> = state.temperature_history.iter().copied().collect();
    let latest_temp = gpu_temp.last().copied().unwrap_or(0.0);
    let thermal_badge = if is_apple {
        thermal_pressure_badge(state)
    } else {
        None
    };
    rows.push(SparklineRow {
        label: "GPU Temp",
        color: Color::Magenta,
        latest_str: format!("{latest_temp:.0}\u{00B0}C"),
        min_max_str: min_max_badge(&gpu_temp),
        history: gpu_temp,
        range: None, // auto-range: temperature varies
        badge: thermal_badge,
    });

    // 4. ANE (Apple Silicon only, when data is present)
    if has_ane {
        let ane_mw = state
            .gpu_info
            .first()
            .map(|g| g.ane_utilization)
            .unwrap_or(0.0);
        let ane_w = ane_mw / 1000.0;
        // ANE doesn't have its own history queue; show single-point sparkline
        let ane_history = vec![ane_w];
        rows.push(SparklineRow {
            label: "ANE",
            color: Color::Yellow,
            latest_str: format!("{ane_w:.1}W"),
            min_max_str: String::new(),
            history: ane_history,
            range: None,
            badge: None,
        });
    }

    // 5. Pkg Power
    let (power_w, power_label) = package_power(state, is_apple);
    // Reuse the cloned utilization history as a proxy sparkline for power
    // variation (power closely tracks GPU utilisation on all platforms).
    rows.push(SparklineRow {
        label: power_label,
        color: Color::Red,
        latest_str: format!("{power_w:.1}W"),
        min_max_str: String::new(), // no min-max for proxy sparkline
        history: power_proxy,
        range: Some((0.0, 100.0)),
        badge: None,
    });

    rows
}

// ---------------------------------------------------------------------------
// Drawing helpers (write to buffer, no trailing \r\n)
// ---------------------------------------------------------------------------

fn draw_top_border<W: Write>(stdout: &mut W, panel_width: usize) {
    let title = "GPU Metrics";
    let inner_width = panel_width.saturating_sub(4); // 2 corners + 2 spacing
    let title_space = 1 + title.len() + 1;
    let dashes = inner_width.saturating_sub(title_space + 1);

    print_colored_text(stdout, "\u{256d}\u{2500}", Color::Cyan, None, None);
    print_colored_text(stdout, " ", Color::White, None, None);
    print_colored_text(stdout, title, Color::Cyan, None, None);
    print_colored_text(stdout, " ", Color::White, None, None);
    for _ in 0..dashes {
        print_colored_text(stdout, "\u{2500}", Color::Cyan, None, None);
    }
    print_colored_text(stdout, "\u{256e}", Color::Cyan, None, None);
}

fn draw_bottom_border<W: Write>(stdout: &mut W, panel_width: usize) {
    let inner_width = panel_width.saturating_sub(4);
    print_colored_text(stdout, "\u{2570}", Color::Cyan, None, None);
    for _ in 0..(inner_width + 1) {
        print_colored_text(stdout, "\u{2500}", Color::Cyan, None, None);
    }
    print_colored_text(stdout, "\u{256f}", Color::Cyan, None, None);
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
    print_colored_text(stdout, "\u{2502} ", Color::Cyan, None, None);

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
    print_colored_text(stdout, " \u{2502}", Color::Cyan, None, None);
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

fn has_ane_data(state: &AppState) -> bool {
    state
        .gpu_info
        .first()
        .map(|g| g.ane_utilization > 0.0)
        .unwrap_or(false)
}

fn thermal_pressure_badge(state: &AppState) -> Option<(String, Color)> {
    state
        .gpu_info
        .first()
        .and_then(|gpu| gpu.detail.get("thermal_pressure"))
        .map(|level| {
            let color = match level.as_str() {
                "Nominal" => Color::Green,
                "Fair" => Color::Yellow,
                "Serious" => Color::Red,
                "Critical" => Color::DarkRed,
                _ => Color::DarkGrey,
            };
            (level.clone(), color)
        })
}

fn package_power(state: &AppState, is_apple: bool) -> (f64, &'static str) {
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
        (power_mw / 1000.0, "Pkg Power")
    } else {
        // NVIDIA / other: sum GPU board power
        let total: f64 = state.gpu_info.iter().map(|g| g.power_consumption).sum();
        (total, "GPU Power")
    }
}

// ---------------------------------------------------------------------------
// Formatting helpers
// ---------------------------------------------------------------------------

fn min_max_badge(data: &[f64]) -> String {
    if data.is_empty() {
        return String::new();
    }
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    for &v in data {
        if v.is_finite() {
            if v < min {
                min = v;
            }
            if v > max {
                max = v;
            }
        }
    }
    if !min.is_finite() || !max.is_finite() {
        return String::new();
    }
    format!("{min:.0}-{max:.0}")
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
            detail,
        });

        // Populate histories
        for i in 0..20 {
            state.utilization_history.push_back(i as f64 * 5.0);
            state.memory_history.push_back(i as f64 * 3.0);
            state.temperature_history.push_back(50.0 + i as f64);
        }
        state
    }

    fn make_apple_silicon_state() -> AppState {
        let mut state = AppState::new();
        state.is_local_mode = true;

        let mut detail = HashMap::new();
        detail.insert("architecture".to_string(), "Apple Silicon".to_string());
        detail.insert("thermal_pressure".to_string(), "Nominal".to_string());
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
            detail,
        });

        for i in 0..20 {
            state.utilization_history.push_back(i as f64 * 4.0);
            state.memory_history.push_back(i as f64 * 2.5);
            state.temperature_history.push_back(40.0 + i as f64);
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
                p_core_count: 8,
                e_core_count: 4,
                gpu_core_count: 16,
                p_core_utilization: 55.0,
                e_core_utilization: 25.0,
                ane_ops_per_second: None,
                p_cluster_frequency_mhz: Some(3490),
                e_cluster_frequency_mhz: Some(2420),
                p_core_l2_cache_mb: Some(16),
                e_core_l2_cache_mb: Some(4),
            }),
            per_core_utilization: per_core,
            time: String::new(),
        }
    }

    #[test]
    fn test_gpu_content_rows_empty() {
        let state = AppState::new();
        assert_eq!(gpu_content_rows(&state), 0);
    }

    #[test]
    fn test_gpu_content_rows_nvidia() {
        let state = make_nvidia_state();
        // GPU Util + GPU Mem + GPU Temp + Pkg Power = 4
        assert_eq!(gpu_content_rows(&state), 4);
    }

    #[test]
    fn test_gpu_content_rows_apple_with_ane() {
        let state = make_apple_silicon_state();
        // GPU Util + GPU Mem + GPU Temp + ANE + Pkg Power = 5
        assert_eq!(gpu_content_rows(&state), 5);
    }

    #[test]
    fn test_detect_apple_silicon() {
        assert!(!detect_apple_silicon(&make_nvidia_state()));
        assert!(detect_apple_silicon(&make_apple_silicon_state()));
    }

    #[test]
    fn test_has_ane_data() {
        assert!(!has_ane_data(&make_nvidia_state()));
        assert!(has_ane_data(&make_apple_silicon_state()));
    }

    #[test]
    fn test_thermal_pressure_badge_nominal() {
        let state = make_apple_silicon_state();
        let badge = thermal_pressure_badge(&state);
        assert!(badge.is_some());
        let (text, color) = badge.unwrap();
        assert_eq!(text, "Nominal");
        assert_eq!(color, Color::Green);
    }

    #[test]
    fn test_thermal_pressure_badge_none_on_nvidia() {
        assert!(thermal_pressure_badge(&make_nvidia_state()).is_none());
    }

    #[test]
    fn test_package_power_apple_silicon() {
        let state = make_apple_silicon_state();
        let (watts, label) = package_power(&state, true);
        assert_eq!(label, "Pkg Power");
        assert!((watts - 12.5).abs() < 0.01);
    }

    #[test]
    fn test_package_power_nvidia() {
        let state = make_nvidia_state();
        let (watts, label) = package_power(&state, false);
        assert_eq!(label, "GPU Power");
        assert!((watts - 320.0).abs() < 0.01);
    }

    #[test]
    fn test_min_max_badge() {
        assert_eq!(min_max_badge(&[10.0, 50.0, 90.0]), "10-90");
        assert_eq!(min_max_badge(&[42.0]), "42-42");
        assert_eq!(min_max_badge(&[]), "");
    }

    #[test]
    fn test_min_max_badge_handles_nan() {
        assert_eq!(min_max_badge(&[f64::NAN, f64::NAN]), "");
    }

    #[test]
    fn test_build_rows_nvidia() {
        let state = make_nvidia_state();
        let rows = build_rows(&state, false, false);
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0].label, "GPU Util");
        assert_eq!(rows[1].label, "GPU Mem");
        assert_eq!(rows[2].label, "GPU Temp");
        assert_eq!(rows[3].label, "GPU Power");
        assert!(rows[2].badge.is_none());
    }

    #[test]
    fn test_build_rows_apple_silicon() {
        let state = make_apple_silicon_state();
        let rows = build_rows(&state, true, true);
        assert_eq!(rows.len(), 5);
        assert_eq!(rows[0].label, "GPU Util");
        assert_eq!(rows[3].label, "ANE");
        assert_eq!(rows[4].label, "Pkg Power");
        assert!(rows[2].badge.is_some());
    }

    #[test]
    fn test_render_gpu_lines_nvidia() {
        let state = make_nvidia_state();
        let lines = render_gpu_lines(&state, 60);
        // top border + 4 content rows + bottom border = 6
        assert_eq!(lines.len(), 6);
        assert!(!lines[0].is_empty()); // top border
    }

    #[test]
    fn test_render_gpu_lines_apple_silicon() {
        let state = make_apple_silicon_state();
        let lines = render_gpu_lines(&state, 60);
        // top border + 5 content rows + bottom border = 7
        assert_eq!(lines.len(), 7);
    }

    #[test]
    fn test_render_gpu_lines_empty() {
        let state = AppState::new();
        let lines = render_gpu_lines(&state, 60);
        assert!(lines.is_empty());
    }

    #[test]
    fn test_render_combined_does_not_panic_nvidia() {
        let mut state = make_nvidia_state();
        let cpu = vec![make_standard_cpu(8)];
        state.cpu_info = cpu.clone();
        let mut buf: Vec<u8> = Vec::new();
        render_combined_activity_panel(&mut buf, &state, &cpu, 120);
        assert!(!buf.is_empty());
    }

    #[test]
    fn test_render_combined_does_not_panic_apple() {
        let mut state = make_apple_silicon_state();
        let cpu = vec![make_apple_cpu()];
        state.cpu_info = cpu.clone();
        let mut buf: Vec<u8> = Vec::new();
        render_combined_activity_panel(&mut buf, &state, &cpu, 120);
        assert!(!buf.is_empty());
    }

    #[test]
    fn test_render_combined_no_gpu() {
        let state = AppState::new();
        let cpu = vec![make_standard_cpu(4)];
        let mut buf: Vec<u8> = Vec::new();
        render_combined_activity_panel(&mut buf, &state, &cpu, 120);
        // Should still render - CPU only
        assert!(!buf.is_empty());
    }

    #[test]
    fn test_render_combined_no_cpu() {
        let state = make_nvidia_state();
        let cpu: Vec<CpuInfo> = Vec::new();
        let mut buf: Vec<u8> = Vec::new();
        render_combined_activity_panel(&mut buf, &state, &cpu, 120);
        // No CPU info -> no output
        assert!(buf.is_empty());
    }
}
