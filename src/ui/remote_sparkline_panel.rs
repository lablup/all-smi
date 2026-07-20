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

//! Full-width braille sparkline panel for remote-mode Live Statistics.
//!
//! Replaces the old split node-list + bar-chart layout with a single
//! full-width panel showing six sparkline rows (3 GPU + 3 CPU) using
//! `sparkline_braille()` from the braille utility module.
//!
//! Each row is formatted as:
//!
//! ```text
//! <label>  <braille sparkline>  <latest value>
//! ```
//!
//! Each sparkline uses a per-metric [`soft_range`](crate::ui::scale::soft_range)
//! auto-axis (zoomed into the visible window with a minimum span, coarse-grid
//! hysteresis, and hard-domain clamping) so small variations stay visible.

use std::io::Write;

use crossterm::{queue, style::Color, style::Print};

use crate::app_state::AppState;
use crate::ui::braille::sparkline_braille;
use crate::ui::scale::{
    PERCENT_DOMAIN, PERCENT_SOFT_GRID, PERCENT_SOFT_MIN_SPAN, TEMP_SOFT_GRID, TEMP_SOFT_MIN_SPAN,
    soft_range, temp_range,
};
use crate::ui::text::print_colored_text;

/// Width reserved for the label column (e.g. "GPU Util.").
const LABEL_WIDTH: usize = 10;

/// Width reserved for the latest-value column (e.g. "100.0%").
const VALUE_WIDTH: usize = 7;

/// Fixed spacing (separators between columns).
const SPACING: usize = 3;

/// Number of rows the remote sparkline panel occupies (header + 3 stat rows).
#[allow(dead_code)]
pub const PANEL_ROWS: usize = 4;

/// Render the full-width remote Live Statistics sparkline panel.
///
/// The panel occupies 4 terminal rows:
/// - 1 header row ("Live Statistics")
/// - 3 sparkline rows (one each for GPU/CPU Util, Memory, Temp)
///
/// Each sparkline row shows GPU and CPU metrics side by side, each taking
/// half the available width.
pub fn draw_remote_sparkline_panel<W: Write>(stdout: &mut W, state: &AppState, cols: u16) {
    let box_width = (cols as usize).min(200);

    if state.utilization_history.is_empty() && state.cpu_utilization_history.is_empty() {
        return;
    }

    // Header
    print_colored_text(stdout, "Live Statistics", Color::Cyan, None, None);
    queue!(stdout, Print("\r\n")).unwrap();

    // Split into GPU and CPU halves
    let half_width = box_width / 2;

    let has_gpu = !state.gpu_info.is_empty();

    // Row 1: Utilization — soft axis over each window (clamped to 0..100).
    let gpu_util = history_vec(&state.utilization_history);
    let cpu_util = history_vec(&state.cpu_utilization_history);
    draw_sparkline_pair(
        stdout,
        SparklinePairParams {
            left_label: "GPU Util.",
            left_color: Color::Yellow,
            left_history: &gpu_util,
            left_value: avg_str(&state.utilization_history, "%"),
            left_range: Some(percent_soft_range(&gpu_util)),
            left_available: has_gpu,
            right_label: "CPU Util.",
            right_color: Color::Cyan,
            right_history: &cpu_util,
            right_value: avg_str(&state.cpu_utilization_history, "%"),
            right_range: Some(percent_soft_range(&cpu_util)),
            half_width,
        },
    );
    queue!(stdout, Print("\r\n")).unwrap();

    // Row 2: Memory — soft axis over each window (clamped to 0..100).
    let gpu_mem = history_vec(&state.memory_history);
    let host_mem = history_vec(&state.system_memory_history);
    draw_sparkline_pair(
        stdout,
        SparklinePairParams {
            left_label: "GPU Mem. ",
            left_color: Color::Yellow,
            left_history: &gpu_mem,
            left_value: avg_str(&state.memory_history, "%"),
            left_range: Some(percent_soft_range(&gpu_mem)),
            left_available: has_gpu,
            right_label: "Host Mem.",
            right_color: Color::Cyan,
            right_history: &host_mem,
            right_value: avg_str(&state.system_memory_history, "%"),
            right_range: Some(percent_soft_range(&host_mem)),
            half_width,
        },
    );
    queue!(stdout, Print("\r\n")).unwrap();

    // Row 3: Temperature — soft axis clamped to (0, thermal-threshold ceiling);
    // the GPU ceiling comes from its reported threshold, the CPU from the
    // 100°C fallback. The floor is 0 so a cool sensor can zoom below 30°C.
    let gpu_temp = history_vec(&state.temperature_history);
    let cpu_temp = history_vec(&state.cpu_temperature_history);
    let gpu_temp_ceiling = temp_range(state.gpu_info.first()).1;
    let cpu_temp_ceiling = temp_range(None).1;
    draw_sparkline_pair(
        stdout,
        SparklinePairParams {
            left_label: "GPU Temp.",
            left_color: Color::Yellow,
            left_history: &gpu_temp,
            left_value: avg_temp_str(&state.temperature_history),
            left_range: Some(soft_range(
                &gpu_temp,
                TEMP_SOFT_MIN_SPAN,
                TEMP_SOFT_GRID,
                (0.0, gpu_temp_ceiling),
            )),
            left_available: has_gpu,
            right_label: "CPU Temp.",
            right_color: Color::Cyan,
            right_history: &cpu_temp,
            right_value: avg_temp_str(&state.cpu_temperature_history),
            right_range: Some(soft_range(
                &cpu_temp,
                TEMP_SOFT_MIN_SPAN,
                TEMP_SOFT_GRID,
                (0.0, cpu_temp_ceiling),
            )),
            half_width,
        },
    );
    queue!(stdout, Print("\r\n")).unwrap();

    // Spacer row so the Tabs strip below isn't visually glued to the
    // last sparkline row. The matching header-line budget lives in
    // `layout::calculate_header_lines` — keep the two in sync.
    queue!(stdout, Print("\r\n")).unwrap();
}

struct SparklinePairParams<'a> {
    left_label: &'a str,
    left_color: Color,
    left_history: &'a [f64],
    left_value: String,
    left_range: Option<(f64, f64)>,
    left_available: bool,
    right_label: &'a str,
    right_color: Color,
    right_history: &'a [f64],
    right_value: String,
    right_range: Option<(f64, f64)>,
    half_width: usize,
}

/// Draw a side-by-side sparkline pair (GPU left, CPU right).
fn draw_sparkline_pair<W: Write>(stdout: &mut W, params: SparklinePairParams) {
    let fixed = LABEL_WIDTH + VALUE_WIDTH + SPACING;
    let sparkline_width = params.half_width.saturating_sub(fixed).max(4);

    // Left half (GPU)
    draw_single_sparkline(
        stdout,
        &SingleSparklineParams {
            label: params.left_label,
            color: params.left_color,
            history: params.left_history,
            value: &params.left_value,
            range: params.left_range,
            sparkline_width,
            available: params.left_available,
            half_width: params.half_width,
        },
    );

    // Right half (CPU)
    draw_single_sparkline(
        stdout,
        &SingleSparklineParams {
            label: params.right_label,
            color: params.right_color,
            history: params.right_history,
            value: &params.right_value,
            range: params.right_range,
            sparkline_width,
            available: true, // CPU is always available
            half_width: params.half_width,
        },
    );
}

struct SingleSparklineParams<'a> {
    label: &'a str,
    color: Color,
    history: &'a [f64],
    value: &'a str,
    range: Option<(f64, f64)>,
    sparkline_width: usize,
    available: bool,
    half_width: usize,
}

/// Draw a single labeled sparkline within its allotted width.
fn draw_single_sparkline<W: Write>(stdout: &mut W, p: &SingleSparklineParams) {
    // Label
    let label_display = format!("{:<LABEL_WIDTH$}", p.label);
    print_colored_text(stdout, &label_display, p.color, None, None);

    if !p.available || p.history.is_empty() {
        // Pad with spaces for the entire sparkline + value area
        let remaining = p.half_width.saturating_sub(LABEL_WIDTH);
        let na_pad = remaining.saturating_sub(4);
        print_colored_text(stdout, &" ".repeat(na_pad), Color::DarkGrey, None, None);
        print_colored_text(stdout, " N/A", Color::DarkGrey, None, None);
        return;
    }

    // Sparkline
    let sparkline = sparkline_braille(p.history, p.sparkline_width, p.range);
    print_colored_text(stdout, &sparkline, p.color, None, None);
    print_colored_text(stdout, " ", Color::White, None, None);

    // Value (right-padded)
    let value_display = format!("{:<VALUE_WIDTH$}", p.value);
    print_colored_text(stdout, &value_display, Color::White, None, None);
}

// ---------------------------------------------------------------------------
// History helpers
// ---------------------------------------------------------------------------

fn history_vec(history: &std::collections::VecDeque<f64>) -> Vec<f64> {
    history.iter().copied().collect()
}

/// Soft auto-axis for a percentage metric (utilization / memory), clamped to
/// `0..100`.
fn percent_soft_range(history: &[f64]) -> (f64, f64) {
    soft_range(
        history,
        PERCENT_SOFT_MIN_SPAN,
        PERCENT_SOFT_GRID,
        PERCENT_DOMAIN,
    )
}

fn avg_str(history: &std::collections::VecDeque<f64>, suffix: &str) -> String {
    if history.is_empty() {
        return "N/A".to_string();
    }
    let avg = history.iter().sum::<f64>() / history.len() as f64;
    format!("{avg:3.1}{suffix}")
}

fn avg_temp_str(history: &std::collections::VecDeque<f64>) -> String {
    if history.is_empty() {
        return "N/A".to_string();
    }
    let avg = history.iter().sum::<f64>() / history.len() as f64;
    format!("{avg:3.0}\u{00B0}C")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_state::AppState;

    fn make_state_with_history() -> AppState {
        let mut state = AppState::new();
        state.is_local_mode = false;
        for i in 0..20 {
            state.utilization_history.push_back(i as f64 * 5.0);
            state.memory_history.push_back(i as f64 * 3.0);
            state.temperature_history.push_back(50.0 + i as f64);
            state.cpu_utilization_history.push_back(i as f64 * 4.0);
            state.system_memory_history.push_back(i as f64 * 2.5);
            state.cpu_temperature_history.push_back(40.0 + i as f64);
        }
        state
    }

    #[test]
    fn test_panel_does_not_panic_empty_state() {
        let state = AppState::new();
        let mut buf: Vec<u8> = Vec::new();
        draw_remote_sparkline_panel(&mut buf, &state, 120);
        // Empty history -> no output
        assert!(buf.is_empty());
    }

    #[test]
    fn test_panel_renders_with_history() {
        let state = make_state_with_history();
        let mut buf: Vec<u8> = Vec::new();
        draw_remote_sparkline_panel(&mut buf, &state, 120);
        assert!(!buf.is_empty());
    }

    #[test]
    fn test_panel_narrow_terminal() {
        let state = make_state_with_history();
        let mut buf: Vec<u8> = Vec::new();
        draw_remote_sparkline_panel(&mut buf, &state, 40);
        assert!(!buf.is_empty());
    }

    #[test]
    fn test_avg_str_empty() {
        let h = std::collections::VecDeque::new();
        assert_eq!(avg_str(&h, "%"), "N/A");
    }

    #[test]
    fn test_avg_str_nonempty() {
        let mut h = std::collections::VecDeque::new();
        h.push_back(50.0);
        h.push_back(100.0);
        assert_eq!(avg_str(&h, "%"), "75.0%");
    }

    #[test]
    fn test_avg_temp_str() {
        let mut h = std::collections::VecDeque::new();
        h.push_back(60.0);
        h.push_back(70.0);
        assert_eq!(avg_temp_str(&h), " 65\u{00B0}C");
    }

    #[test]
    fn test_percent_soft_range_zooms_and_clamps() {
        // A near-constant window is expanded to the minimum span and rounded
        // outward, while staying inside the 0..100 domain.
        let (lo, hi) = percent_soft_range(&[41.0, 42.0, 41.0]);
        assert!(hi - lo >= PERCENT_SOFT_MIN_SPAN);
        assert!(lo >= 0.0 && hi <= 100.0);
        // center 41.5 -> [31.5, 51.5] -> grid [30, 55].
        assert_eq!((lo, hi), (30.0, 55.0));
    }

    #[test]
    fn test_percent_soft_range_empty_history() {
        // Empty history anchors a min-span window at the domain floor.
        assert_eq!(percent_soft_range(&[]), (0.0, 20.0));
    }
}
