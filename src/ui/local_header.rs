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

//! One-line host summary bar for local mode.
//!
//! Renders two compact lines that replace the remote-mode Cluster Overview:
//!
//! **Line 1** — identity row:
//! ```text
//! Host <hostname> · <cpu_model> · arch <arch> · up <uptime>    ● Live
//! ```
//!
//! **Line 2** — metrics sparkline row (8-cell braille sparklines):
//! ```text
//! CPU <pct>% ⣿⣷⣶…  GPU <pct>% ⣿⣷…  RAM <used>/<total>GB ⣿…  Pwr <W>W ⣿…  Tmp <°C>°C ⣿…
//! ```
//!
//! Colors come from `ThemeConfig` — none are hardcoded.
//! Sparklines are rendered via [`sparkline_braille`] from the braille utility module.

use std::io::Write;

use crossterm::{queue, style::Color, style::Print};

use crate::app_state::AppState;
use crate::common::config::ThemeConfig;
use crate::ui::braille::sparkline_braille;
use crate::ui::text::print_colored_text;

/// Width in braille cells for each metric sparkline.
const SPARKLINE_WIDTH: usize = 8;

/// Render the two-line local-mode host summary bar.
///
/// This function is called from `render_main()` in `frame_renderer.rs` when
/// `view_state.is_local_mode` is `true`, in place of the Cluster Overview.
pub fn draw_local_header_bar<W: Write>(stdout: &mut W, state: &AppState, _cols: u16) {
    draw_identity_line(stdout, state);
    draw_metrics_line(stdout, state);
}

// ─── Line 1: identity ────────────────────────────────────────────────────────

/// Render the identity line:
/// `Host <hostname> · <cpu_model> · arch <arch> · up <uptime>    ● Live`
fn draw_identity_line<W: Write>(stdout: &mut W, state: &AppState) {
    // Hostname — use the first CPU entry's hostname (always available in local mode)
    let hostname = state
        .cpu_info
        .first()
        .map(|c| c.hostname.as_str())
        .unwrap_or("localhost");

    // CPU model — first CPU entry
    let cpu_model = state
        .cpu_info
        .first()
        .map(|c| c.cpu_model.as_str())
        .unwrap_or("unknown");

    // Architecture — first CPU entry
    let arch = state
        .cpu_info
        .first()
        .map(|c| c.architecture.as_str())
        .unwrap_or("unknown");

    // Uptime — read from sysinfo (cheap: sysinfo re-reads /proc/uptime on each call on Linux,
    // uses sysctl kern.boottime on macOS; both are lightweight system calls)
    let uptime_secs = sysinfo::System::uptime();
    let uptime_str = format_uptime(uptime_secs);

    // Print: "Host <hostname>"
    print_colored_text(stdout, "Host ", Color::DarkGrey, None, None);
    print_colored_text(stdout, hostname, Color::White, None, None);

    // " · <cpu_model>"
    print_colored_text(stdout, " · ", Color::DarkGrey, None, None);
    print_colored_text(stdout, cpu_model, Color::White, None, None);

    // " · arch <arch>"
    print_colored_text(stdout, " · arch ", Color::DarkGrey, None, None);
    print_colored_text(stdout, arch, ThemeConfig::accent_color(), None, None);

    // " · up <uptime>"
    print_colored_text(stdout, " · up ", Color::DarkGrey, None, None);
    print_colored_text(stdout, &uptime_str, ThemeConfig::memory_color(), None, None);

    // Right-side "● Live" indicator — blinks on even frame counts
    // `frame_counter` is incremented on every render tick by the UI loop
    let live_color = if state.frame_counter.is_multiple_of(2) {
        Color::Green
    } else {
        Color::DarkGreen
    };
    print_colored_text(stdout, "    ", Color::White, None, None);
    print_colored_text(stdout, "●", live_color, None, None);
    print_colored_text(stdout, " Live", Color::DarkGrey, None, None);

    queue!(stdout, Print("\r\n")).unwrap();
}

// ─── Line 2: metrics sparklines ──────────────────────────────────────────────

/// Render the metrics sparkline row.
fn draw_metrics_line<W: Write>(stdout: &mut W, state: &AppState) {
    // CPU% — theme color Cyan
    draw_metric_sparkline(
        stdout,
        "CPU",
        &state
            .cpu_utilization_history
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        format_pct(state.cpu_utilization_history.back().copied()),
        ThemeConfig::cpu_color(),
        Some((0.0, 100.0)),
    );

    print_colored_text(stdout, "  ", Color::White, None, None);

    // GPU% — theme color Blue
    draw_metric_sparkline(
        stdout,
        "GPU",
        &state
            .utilization_history
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        format_pct(state.utilization_history.back().copied()),
        ThemeConfig::gpu_color(),
        Some((0.0, 100.0)),
    );

    print_colored_text(stdout, "  ", Color::White, None, None);

    // RAM used/total — theme color Green
    draw_ram_sparkline(stdout, state);

    print_colored_text(stdout, "  ", Color::White, None, None);

    // Package power — theme color Red
    draw_power_sparkline(stdout, state);

    print_colored_text(stdout, "  ", Color::White, None, None);

    // Temperature — theme color Magenta
    draw_metric_sparkline(
        stdout,
        "Tmp",
        &state
            .cpu_temperature_history
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        format_temp(state.cpu_temperature_history.back().copied()),
        ThemeConfig::thermal_color(),
        None, // auto-range: temperature varies between platforms
    );

    queue!(stdout, Print("\r\n")).unwrap();
}

/// Draw a single labelled metric with a braille sparkline.
///
/// Format: `<label> <value> <sparkline>`
fn draw_metric_sparkline<W: Write>(
    stdout: &mut W,
    label: &str,
    history: &[f64],
    value_str: String,
    color: Color,
    range: Option<(f64, f64)>,
) {
    let sparkline = sparkline_braille(history, SPARKLINE_WIDTH, range);

    print_colored_text(stdout, label, color, None, None);
    print_colored_text(stdout, " ", Color::White, None, None);
    print_colored_text(stdout, &value_str, Color::White, None, None);
    print_colored_text(stdout, " ", Color::DarkGrey, None, None);
    print_colored_text(stdout, &sparkline, color, None, None);
}

/// Draw the RAM metric: `RAM <used>/<total>GB <sparkline>`.
///
/// The sparkline tracks `system_memory_history` (memory utilization %).
fn draw_ram_sparkline<W: Write>(stdout: &mut W, state: &AppState) {
    let total_gb = state.memory_info.iter().map(|m| m.total_bytes).sum::<u64>() as f64
        / (1024.0 * 1024.0 * 1024.0);

    let used_gb = state.memory_info.iter().map(|m| m.used_bytes).sum::<u64>() as f64
        / (1024.0 * 1024.0 * 1024.0);

    let total_str = format!("{total_gb:.0}");
    let value_str = format!("{used_gb:>width$.0}/{total_str}GB", width = total_str.len());

    let history: Vec<f64> = state.system_memory_history.iter().copied().collect();

    let sparkline = sparkline_braille(&history, SPARKLINE_WIDTH, Some((0.0, 100.0)));

    print_colored_text(stdout, "RAM", ThemeConfig::memory_color(), None, None);
    print_colored_text(stdout, " ", Color::White, None, None);
    print_colored_text(stdout, &value_str, Color::White, None, None);
    print_colored_text(stdout, " ", Color::DarkGrey, None, None);
    print_colored_text(stdout, &sparkline, ThemeConfig::memory_color(), None, None);
}

/// Draw the power metric: `Pwr <W>W <sparkline>`.
///
/// For Apple Silicon: reads `combined_power_mw` from `gpu.detail`.
/// For Linux/NVIDIA: sums `gpu.power_consumption` across all GPUs.
///
/// The sparkline tracks the dedicated package-power history maintained by the
/// data aggregator.
fn draw_power_sparkline<W: Write>(stdout: &mut W, state: &AppState) {
    let is_apple_silicon = state.gpu_info.iter().any(|gpu| {
        gpu.detail
            .get("architecture")
            .map(|arch| arch == "Apple Silicon")
            .unwrap_or(false)
    });

    let power_watts = if is_apple_silicon {
        // Apple Silicon: combined CPU+GPU+ANE power from the native metrics manager
        state
            .gpu_info
            .iter()
            .filter_map(|gpu| {
                gpu.detail
                    .get("combined_power_mw")
                    .and_then(|s| s.parse::<f64>().ok())
                    .map(|mw| mw / 1000.0)
            })
            .next()
            .unwrap_or_else(|| state.gpu_info.iter().map(|g| g.power_consumption).sum())
    } else {
        // Linux/NVIDIA: aggregate GPU power
        state.gpu_info.iter().map(|g| g.power_consumption).sum()
    };

    let value_str = format!("{power_watts:>5.1}W");

    let history: Vec<f64> = state.package_power_history.iter().copied().collect();
    let range = if history.is_empty() {
        None
    } else {
        Some((
            0.0,
            history.iter().copied().fold(0.0, f64::max).max(power_watts),
        ))
    };
    let sparkline = sparkline_braille(&history, SPARKLINE_WIDTH, range);

    print_colored_text(stdout, "Pwr", ThemeConfig::power_color(), None, None);
    print_colored_text(stdout, " ", Color::White, None, None);
    print_colored_text(stdout, &value_str, Color::White, None, None);
    print_colored_text(stdout, " ", Color::DarkGrey, None, None);
    print_colored_text(stdout, &sparkline, ThemeConfig::power_color(), None, None);
}

// ─── Formatting helpers ───────────────────────────────────────────────────────

/// Format a `%` value as `"<val>%"` or `"N/A"` when missing.
///
/// The numeric part is right-aligned in a 5-char field, producing a
/// consistent 6-display-column string: `"  0.0%"` through `"100.0%"`.
fn format_pct(value: Option<f64>) -> String {
    match value {
        Some(v) => format!("{v:>5.1}%"),
        None => format!("{:>6}", "N/A"),
    }
}

/// Format a temperature value as `"<val>°C"` or `"N/A"`.
///
/// The numeric part is right-aligned in a 3-char field, producing a
/// consistent 5-display-column string: `"  0°C"` through `"999°C"`.
fn format_temp(value: Option<f64>) -> String {
    match value {
        Some(v) => format!("{v:>3.0}°C"),
        None => format!("{:>5}", "N/A"),
    }
}

/// Convert uptime seconds into a human-readable string.
///
/// Format: `"Xd Xh Xm"` for multi-day, `"Xh Xm"` for multi-hour, `"Xm Xs"` otherwise.
fn format_uptime(secs: u64) -> String {
    let days = secs / 86400;
    let hours = (secs % 86400) / 3600;
    let mins = (secs % 3600) / 60;
    let remaining_secs = secs % 60;

    if days > 0 {
        format!("{days}d {hours}h {mins}m")
    } else if hours > 0 {
        format!("{hours}h {mins}m")
    } else {
        format!("{mins}m {remaining_secs}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_uptime_seconds_only() {
        assert_eq!(format_uptime(45), "0m 45s");
        assert_eq!(format_uptime(0), "0m 0s");
    }

    #[test]
    fn test_format_uptime_minutes() {
        assert_eq!(format_uptime(90), "1m 30s");
        assert_eq!(format_uptime(3599), "59m 59s");
    }

    #[test]
    fn test_format_uptime_hours() {
        assert_eq!(format_uptime(3600), "1h 0m");
        assert_eq!(format_uptime(7384), "2h 3m");
        assert_eq!(format_uptime(86399), "23h 59m");
    }

    #[test]
    fn test_format_uptime_days() {
        assert_eq!(format_uptime(86400), "1d 0h 0m");
        assert_eq!(format_uptime(172861), "2d 0h 1m");
        assert_eq!(format_uptime(263845), "3d 1h 17m");
    }

    #[test]
    fn test_format_pct_some() {
        assert_eq!(format_pct(Some(0.0)), "  0.0%");
        assert_eq!(format_pct(Some(75.5)), " 75.5%");
        assert_eq!(format_pct(Some(100.0)), "100.0%");
    }

    #[test]
    fn test_format_pct_none() {
        assert_eq!(format_pct(None), "   N/A");
    }

    #[test]
    fn test_format_temp_some() {
        assert_eq!(format_temp(Some(72.0)), " 72°C");
        assert_eq!(format_temp(Some(72.9)), " 73°C"); // rounds
    }

    #[test]
    fn test_format_temp_none() {
        assert_eq!(format_temp(None), "  N/A");
    }

    #[test]
    fn test_format_pct_fixed_width() {
        // All formatted percentages must have the same display width (6 chars)
        let values = [0.0, 9.9, 10.0, 50.0, 99.9, 100.0];
        let widths: Vec<usize> = values.iter().map(|&v| format_pct(Some(v)).len()).collect();
        assert!(
            widths.windows(2).all(|w| w[0] == w[1]),
            "all pct widths should be equal: {widths:?}"
        );
    }

    #[test]
    fn test_format_temp_fixed_display_width() {
        // Verify digit boundaries don't change width
        // "°" is multi-byte UTF-8, so check specific expected values
        assert_eq!(format_temp(Some(9.0)), "  9°C");
        assert_eq!(format_temp(Some(10.0)), " 10°C");
        assert_eq!(format_temp(Some(99.0)), " 99°C");
        assert_eq!(format_temp(Some(100.0)), "100°C");
    }

    /// Replicate the inline power formatting from [`draw_power_sparkline`] and
    /// assert that all values produce a string of exactly 6 characters.
    #[test]
    fn test_format_power_fixed_width() {
        // The inline formula is: format!("{power_watts:>5.1}W")
        // 5-char numeric field + "W" = 6 chars total for 0.0 through 999.9 W.
        let values = [0.0_f64, 9.9, 10.0, 99.9, 100.0, 999.9];
        for &w in &values {
            let s = format!("{w:>5.1}W");
            assert_eq!(
                s.len(),
                6,
                "power format for {w} should be 6 chars, got {s:?}"
            );
        }
        // Spot-check specific expected strings
        assert_eq!(format!("{:>5.1}W", 0.0_f64), "  0.0W");
        assert_eq!(format!("{:>5.1}W", 10.5_f64), " 10.5W");
        assert_eq!(format!("{:>5.1}W", 999.9_f64), "999.9W");
    }

    /// Replicate the inline RAM formatting from [`draw_ram_sparkline`] and
    /// assert that the `used` field is always padded to the same width as
    /// `total`, keeping the `/` separator in a fixed column.
    #[test]
    fn test_format_ram_fixed_separator_position() {
        // The inline formula:
        //   let total_str = format!("{total_gb:.0}");
        //   format!("{used_gb:>width$.0}/{total_str}GB", width = total_str.len())
        let cases: &[(f64, f64, &str)] = &[
            // total=16 → 2-digit field → used is padded to width 2
            (0.0, 16.0, " 0/16GB"),
            (8.0, 16.0, " 8/16GB"),
            (16.0, 16.0, "16/16GB"),
            // total=128 → 3-digit field → used is padded to width 3
            (0.0, 128.0, "  0/128GB"),
            (64.0, 128.0, " 64/128GB"),
            (128.0, 128.0, "128/128GB"),
        ];
        for &(used, total, expected) in cases {
            let total_str = format!("{total:.0}");
            let value_str = format!("{used:>width$.0}/{total_str}GB", width = total_str.len());
            assert_eq!(
                value_str, expected,
                "RAM format for {used}/{total} GB should be {expected:?}, got {value_str:?}"
            );
        }
        // All strings for a given total_gb must have the same byte length
        let totals = [16.0_f64, 128.0];
        for total in totals {
            let total_str = format!("{total:.0}");
            let w = total_str.len();
            let len_0 = format!("{:>w$.0}/{total_str}GB", 0.0_f64).len();
            let len_total = format!("{:>w$.0}/{total_str}GB", total).len();
            assert_eq!(
                len_0, len_total,
                "RAM format width should be stable for total={total}"
            );
        }
    }

    #[test]
    fn test_draw_local_header_bar_does_not_panic_empty_state() {
        use crate::app_state::AppState;
        let state = AppState::new();
        let mut buf: Vec<u8> = Vec::new();
        // Should complete without panic even when all history is empty
        draw_local_header_bar(&mut buf, &state, 80);
    }

    #[test]
    fn test_draw_local_header_bar_with_history() {
        use crate::app_state::AppState;
        let mut state = AppState::new();
        // Populate some history to exercise the sparkline path
        for i in 0..10 {
            state.cpu_utilization_history.push_back(i as f64 * 10.0);
            state.utilization_history.push_back(i as f64 * 8.0);
            state.system_memory_history.push_back(i as f64 * 5.0);
            state
                .cpu_temperature_history
                .push_back(40.0 + i as f64 * 3.0);
        }
        let mut buf: Vec<u8> = Vec::new();
        draw_local_header_bar(&mut buf, &state, 120);
        // Buffer must be non-empty
        assert!(!buf.is_empty());
    }
}
