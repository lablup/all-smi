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
//! CPU <pct>%<t> ⣿⣷⣶…  GPU <pct>%<t> ⣿⣷…  RAM <used>/<total>GB<t> ⣿…  Pwr <W>W<t> ⣿…  Tmp <°C>°C<t> ⣿…
//! ```
//!
//! Each metric carries a one-cell trend glyph `<t>` (`↑ ↗ → ↘ ↓`) immediately
//! after its latest value, coloured in the metric's theme colour, derived from
//! the recent slope of that metric's history (see [`trend_glyph`]).
//!
//! Each sparkline uses a per-metric [`soft_range`](crate::ui::scale::soft_range)
//! auto-axis (zoomed into the visible window with a minimum span, coarse-grid
//! hysteresis, and hard-domain clamping) so small variations stay visible.
//!
//! Colors come from `ThemeConfig` — none are hardcoded.
//! Sparklines are rendered via [`sparkline_braille`] from the braille utility module.

use std::io::Write;

use crossterm::{queue, style::Color, style::Print};

use crate::app_state::AppState;
use crate::common::config::ThemeConfig;
use crate::ui::braille::sparkline_braille;
use crate::ui::scale::{
    PERCENT_DOMAIN, PERCENT_SOFT_GRID, PERCENT_SOFT_MIN_SPAN, TEMP_SOFT_GRID, TEMP_SOFT_MIN_SPAN,
    power_range, power_soft_grid, power_soft_min_span, soft_range, temp_range,
};
use crate::ui::text::print_colored_text;

/// Width in braille cells for each metric sparkline.
const SPARKLINE_WIDTH: usize = 8;

/// Trend-classification thresholds `(flat, steep)` for percentage metrics
/// (CPU / GPU / RAM), in percentage points. A recent change below `flat` reads
/// as level (`→`), below `steep` as a gentle slope (`↗`/`↘`), otherwise steep
/// (`↑`/`↓`).
const TREND_PERCENT: (f64, f64) = (1.0, 5.0);

/// Trend-classification thresholds `(flat, steep)` for temperature, in °C.
const TREND_TEMP: (f64, f64) = (0.5, 2.0);

/// Trend-classification thresholds `(flat, steep)` for package power, in W.
const TREND_POWER: (f64, f64) = (0.2, 1.0);

/// How many samples back the trend slope is measured over.
const TREND_LOOKBACK: usize = 5;

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
    let cpu_history: Vec<f64> = state.cpu_utilization_history.iter().copied().collect();
    draw_metric_sparkline(
        stdout,
        "CPU",
        &cpu_history,
        format_pct(state.cpu_utilization_history.back().copied()),
        ThemeConfig::cpu_color(),
        Some(soft_range(
            &cpu_history,
            PERCENT_SOFT_MIN_SPAN,
            PERCENT_SOFT_GRID,
            PERCENT_DOMAIN,
        )),
        TREND_PERCENT,
    );

    print_colored_text(stdout, "  ", Color::White, None, None);

    // GPU% — theme color Blue
    let gpu_history: Vec<f64> = state.utilization_history.iter().copied().collect();
    draw_metric_sparkline(
        stdout,
        "GPU",
        &gpu_history,
        format_pct(state.utilization_history.back().copied()),
        ThemeConfig::gpu_color(),
        Some(soft_range(
            &gpu_history,
            PERCENT_SOFT_MIN_SPAN,
            PERCENT_SOFT_GRID,
            PERCENT_DOMAIN,
        )),
        TREND_PERCENT,
    );

    print_colored_text(stdout, "  ", Color::White, None, None);

    // RAM used/total — theme color Green
    draw_ram_sparkline(stdout, state);

    print_colored_text(stdout, "  ", Color::White, None, None);

    // Package power — theme color Red
    draw_power_sparkline(stdout, state);

    print_colored_text(stdout, "  ", Color::White, None, None);

    // Temperature — theme color Magenta
    let temp_history: Vec<f64> = state.cpu_temperature_history.iter().copied().collect();
    // Soft axis clamped to (0, temp ceiling): CPU sensors report no thermal
    // threshold, so the ceiling is the 100°C fallback while the floor is 0 so a
    // cool sensor can zoom below 30°C. The window then tracks small changes.
    let temp_ceiling = temp_range(None).1;
    draw_metric_sparkline(
        stdout,
        "Tmp",
        &temp_history,
        format_temp(state.cpu_temperature_history.back().copied()),
        ThemeConfig::thermal_color(),
        Some(soft_range(
            &temp_history,
            TEMP_SOFT_MIN_SPAN,
            TEMP_SOFT_GRID,
            (0.0, temp_ceiling),
        )),
        TREND_TEMP,
    );

    queue!(stdout, Print("\r\n")).unwrap();
}

/// Draw a single labelled metric with a braille sparkline.
///
/// Format: `<label> <value><trend> <sparkline>`, where `<trend>` is a one-cell
/// glyph (`↑ ↗ → ↘ ↓`) coloured in the metric's theme colour.
fn draw_metric_sparkline<W: Write>(
    stdout: &mut W,
    label: &str,
    history: &[f64],
    value_str: String,
    color: Color,
    range: Option<(f64, f64)>,
    trend: (f64, f64),
) {
    let sparkline = sparkline_braille(history, SPARKLINE_WIDTH, range);
    let glyph = trend_glyph(history, trend.0, trend.1);

    print_colored_text(stdout, label, color, None, None);
    print_colored_text(stdout, " ", Color::White, None, None);
    print_colored_text(stdout, &value_str, Color::White, None, None);
    print_colored_text(stdout, glyph, color, None, None);
    print_colored_text(stdout, " ", Color::DarkGrey, None, None);
    print_colored_text(stdout, &sparkline, color, None, None);
}

/// Classify the recent slope of `history` into one of five trend glyphs.
///
/// The slope is the delta between the latest sample and the sample
/// [`TREND_LOOKBACK`] positions back (or the oldest available when the history
/// is shorter). Classification, by the delta `d` against `(flat, steep)`:
/// - `|d| < flat` → `→` (level)
/// - `flat ≤ d < steep` → `↗`, `steep ≤ d` → `↑`
/// - `flat ≤ -d < steep` → `↘`, `steep ≤ -d` → `↓`
///
/// Fewer than two samples yields a single space so the metric column stays
/// aligned. A non-finite latest or reference sample reads as level (`→`).
#[must_use]
fn trend_glyph(history: &[f64], flat: f64, steep: f64) -> &'static str {
    if history.len() < 2 {
        return " ";
    }
    let latest = history[history.len() - 1];
    let reference = history[history.len().saturating_sub(TREND_LOOKBACK + 1)];
    if !latest.is_finite() || !reference.is_finite() {
        return "\u{2192}"; // →
    }
    let delta = latest - reference;
    if delta.abs() < flat {
        "\u{2192}" // →
    } else if delta > 0.0 {
        if delta >= steep {
            "\u{2191}" // ↑
        } else {
            "\u{2197}" // ↗
        }
    } else if delta <= -steep {
        "\u{2193}" // ↓
    } else {
        "\u{2198}" // ↘
    }
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

    // Soft axis over the memory-utilization (%) window.
    let range = soft_range(
        &history,
        PERCENT_SOFT_MIN_SPAN,
        PERCENT_SOFT_GRID,
        PERCENT_DOMAIN,
    );
    let sparkline = sparkline_braille(&history, SPARKLINE_WIDTH, Some(range));
    let glyph = trend_glyph(&history, TREND_PERCENT.0, TREND_PERCENT.1);

    print_colored_text(stdout, "RAM", ThemeConfig::memory_color(), None, None);
    print_colored_text(stdout, " ", Color::White, None, None);
    print_colored_text(stdout, &value_str, Color::White, None, None);
    print_colored_text(stdout, glyph, ThemeConfig::memory_color(), None, None);
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
    // Soft axis clamped to (0, power ceiling), where the ceiling is the summed
    // enforced power limits (or a nice-rounded peak when no limit is reported).
    // The soft-axis min span and grid step scale with that ceiling, so the
    // height zooms into the visible window without leaving the power budget.
    let ceiling = power_range(&state.gpu_info, &history).1;
    let range = soft_range(
        &history,
        power_soft_min_span(ceiling),
        power_soft_grid(ceiling),
        (0.0, ceiling),
    );
    let sparkline = sparkline_braille(&history, SPARKLINE_WIDTH, Some(range));
    let glyph = trend_glyph(&history, TREND_POWER.0, TREND_POWER.1);

    print_colored_text(stdout, "Pwr", ThemeConfig::power_color(), None, None);
    print_colored_text(stdout, " ", Color::White, None, None);
    print_colored_text(stdout, &value_str, Color::White, None, None);
    print_colored_text(stdout, glyph, ThemeConfig::power_color(), None, None);
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
            let zero = 0.0_f64;
            let len_0 = format!("{zero:>w$.0}/{total_str}GB").len();
            let len_total = format!("{total:>w$.0}/{total_str}GB").len();
            assert_eq!(
                len_0, len_total,
                "RAM format width should be stable for total={total}"
            );
        }
    }

    #[test]
    fn test_trend_glyph_insufficient_history() {
        // Fewer than two samples -> a single space to preserve column alignment.
        assert_eq!(trend_glyph(&[], 1.0, 5.0), " ");
        assert_eq!(trend_glyph(&[42.0], 1.0, 5.0), " ");
    }

    #[test]
    fn test_trend_glyph_flat() {
        // |delta| < flat threshold -> level arrow.
        assert_eq!(trend_glyph(&[50.0, 50.5], 1.0, 5.0), "\u{2192}"); // →
        assert_eq!(trend_glyph(&[50.0, 49.5], 1.0, 5.0), "\u{2192}"); // →
    }

    #[test]
    fn test_trend_glyph_gentle_slopes() {
        // flat <= |delta| < steep -> diagonal arrows.
        assert_eq!(trend_glyph(&[50.0, 53.0], 1.0, 5.0), "\u{2197}"); // ↗
        assert_eq!(trend_glyph(&[53.0, 50.0], 1.0, 5.0), "\u{2198}"); // ↘
    }

    #[test]
    fn test_trend_glyph_steep_slopes() {
        // |delta| >= steep -> vertical arrows.
        assert_eq!(trend_glyph(&[50.0, 60.0], 1.0, 5.0), "\u{2191}"); // ↑
        assert_eq!(trend_glyph(&[60.0, 50.0], 1.0, 5.0), "\u{2193}"); // ↓
    }

    #[test]
    fn test_trend_glyph_uses_sample_lookback_not_oldest() {
        // With more than TREND_LOOKBACK+1 samples the reference is the sample
        // ~5 back, not the oldest. Here the oldest (0.0) would read as a steep
        // rise, but the last 5 steps are flat, so the glyph must be level.
        let h = [0.0, 50.0, 50.0, 50.0, 50.0, 50.0, 50.2];
        assert_eq!(trend_glyph(&h, 1.0, 5.0), "\u{2192}"); // →
    }

    #[test]
    fn test_trend_glyph_short_history_uses_oldest() {
        // Between 2 and TREND_LOOKBACK+1 samples: reference is the oldest.
        let h = [10.0, 20.0, 30.0];
        assert_eq!(trend_glyph(&h, 1.0, 5.0), "\u{2191}"); // delta 20 -> ↑
    }

    #[test]
    fn test_trend_glyph_non_finite_reads_level() {
        assert_eq!(trend_glyph(&[f64::NAN, 50.0], 1.0, 5.0), "\u{2192}");
        assert_eq!(trend_glyph(&[50.0, f64::NAN], 1.0, 5.0), "\u{2192}");
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

    /// [`trend_glyph`] is covered in isolation above, but nothing previously
    /// checked that its result actually reaches the rendered byte stream in
    /// the right column. Six-sample histories are used so `TREND_LOOKBACK`
    /// (5) always compares against the oldest sample, and each metric is
    /// engineered to land in a distinct classification bucket so the test can
    /// pin down exactly which glyph is expected in which metric's segment of
    /// the output.
    #[test]
    fn test_draw_local_header_bar_renders_trend_glyphs() {
        use crate::app_state::AppState;
        let mut state = AppState::new();
        for v in [10.0, 10.0, 10.0, 10.0, 10.0, 60.0] {
            state.cpu_utilization_history.push_back(v); // delta 50 -> steep rise
        }
        for v in [80.0, 80.0, 80.0, 80.0, 80.0, 20.0] {
            state.utilization_history.push_back(v); // delta -60 -> steep fall
        }
        for v in [50.0, 50.0, 50.0, 50.0, 50.0, 50.5] {
            state.system_memory_history.push_back(v); // delta 0.5 -> flat
        }
        for v in [10.0, 10.0, 10.0, 10.0, 10.0, 10.5] {
            state.package_power_history.push_back(v); // delta 0.5 -> gentle rise
        }
        for v in [50.0, 50.0, 50.0, 50.0, 50.0, 49.0] {
            state.cpu_temperature_history.push_back(v); // delta -1.0 -> gentle fall
        }

        let mut buf: Vec<u8> = Vec::new();
        draw_local_header_bar(&mut buf, &state, 120);
        let out = String::from_utf8_lossy(&buf);

        // Locate each metric's label so the glyph search can be scoped to
        // that metric's segment of line 2, confirming both the glyph value
        // and that it lands in the correct column.
        let cpu = out.find("CPU").expect("CPU label rendered");
        let gpu = out.find("GPU").expect("GPU label rendered");
        let ram = out.find("RAM").expect("RAM label rendered");
        let pwr = out.find("Pwr").expect("Pwr label rendered");
        let tmp = out.find("Tmp").expect("Tmp label rendered");
        assert!(
            cpu < gpu && gpu < ram && ram < pwr && pwr < tmp,
            "metric labels rendered out of order: {out:?}"
        );

        assert!(
            out[cpu..gpu].contains('\u{2191}'), // ↑
            "CPU segment should contain the steep-rise glyph: {:?}",
            &out[cpu..gpu]
        );
        assert!(
            out[gpu..ram].contains('\u{2193}'), // ↓
            "GPU segment should contain the steep-fall glyph: {:?}",
            &out[gpu..ram]
        );
        assert!(
            out[ram..pwr].contains('\u{2192}'), // →
            "RAM segment should contain the level glyph: {:?}",
            &out[ram..pwr]
        );
        assert!(
            out[pwr..tmp].contains('\u{2197}'), // ↗
            "Pwr segment should contain the gentle-rise glyph: {:?}",
            &out[pwr..tmp]
        );
        assert!(
            out[tmp..].contains('\u{2198}'), // ↘
            "Tmp segment should contain the gentle-fall glyph: {:?}",
            &out[tmp..]
        );
    }
}
