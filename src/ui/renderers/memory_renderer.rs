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

use crate::device::MemoryInfo;
use crate::ui::text::print_colored_text;
use crate::ui::widgets::{BarSegment, draw_bar_multi};

/// Memory renderer struct implementing the DeviceRenderer trait
#[allow(dead_code)]
pub struct MemoryRenderer;

impl Default for MemoryRenderer {
    fn default() -> Self {
        Self::new()
    }
}

#[allow(dead_code)]
impl MemoryRenderer {
    pub fn new() -> Self {
        Self
    }
}

/// Helper function to format hostname with scrolling
fn format_hostname_with_scroll(hostname: &str, scroll_offset: usize) -> String {
    if hostname.len() > 9 {
        let scroll_len = hostname.len() + 3;
        let start_pos = scroll_offset % scroll_len;
        let extended_hostname = format!("{hostname}   {hostname}");
        extended_hostname
            .chars()
            .skip(start_pos)
            .take(9)
            .collect::<String>()
    } else {
        // Always return 9 characters, left-aligned with space padding
        format!("{hostname:<9}")
    }
}

/// Render memory information including total, used, available, and utilization.
///
/// When `info.swap_total_bytes > 0`, a second bar (`Swap`) is rendered
/// directly below the `Mem` bar. The swap segment color flips from
/// magenta (idle: swap space exists but is unused) to red (active:
/// `swap_used_bytes > 0`) so users can immediately spot swap pressure,
/// which is the primary signal requested in issue #220 — particularly
/// important on Apple Silicon where unified-memory spillover into swap
/// silently degrades AI inference throughput.
///
/// Hosts with no swap configured (`swap_total_bytes == 0`) skip the
/// swap row entirely, matching the API exporter guard at
/// `src/api/metrics/memory.rs:76`.
pub fn print_memory_info<W: Write>(
    stdout: &mut W,
    _index: usize,
    info: &MemoryInfo,
    width: usize,
    hostname_scroll_offset: usize,
    show_hostname: bool,
) {
    // Convert bytes to GB for display
    let total_gb = info.total_bytes as f64 / (1024.0 * 1024.0 * 1024.0);
    let used_gb = info.used_bytes as f64 / (1024.0 * 1024.0 * 1024.0);
    let available_gb = info.available_bytes as f64 / (1024.0 * 1024.0 * 1024.0);

    // Print Memory info line
    print_colored_text(stdout, "Host Memory         ", Color::Cyan, None, None);
    if show_hostname {
        let hostname_display = format_hostname_with_scroll(&info.hostname, hostname_scroll_offset);
        print_colored_text(stdout, " @ ", Color::DarkGreen, None, None);
        print_colored_text(stdout, &hostname_display, Color::White, None, None);
    }
    print_colored_text(stdout, " Total:", Color::Green, None, None);
    print_colored_text(
        stdout,
        &format!("{total_gb:>6.0}GB"),
        Color::White,
        None,
        None,
    );
    print_colored_text(stdout, " Used:", Color::Red, None, None);
    print_colored_text(
        stdout,
        &format!("{used_gb:>6.1}GB"),
        Color::White,
        None,
        None,
    );
    print_colored_text(stdout, " Avail:", Color::Green, None, None);
    print_colored_text(
        stdout,
        &format!("{available_gb:>6.1}GB"),
        Color::White,
        None,
        None,
    );
    print_colored_text(stdout, " Util:", Color::Magenta, None, None);
    print_colored_text(
        stdout,
        &format!("{:>5.1}%", info.utilization),
        Color::White,
        None,
        None,
    );
    queue!(stdout, Print("\r\n")).unwrap();

    // Calculate gauge widths with 5 char padding on each side
    let available_width = width.saturating_sub(10); // 5 padding each side
    let gauge_width = available_width;

    // Calculate actual space used and dynamic right padding
    let total_gauge_width = gauge_width;
    let left_padding = 5;
    let right_padding = width
        .saturating_sub(left_padding)
        .saturating_sub(total_gauge_width);

    print_colored_text(stdout, "     ", Color::White, None, None); // 5 char left padding

    // Create segments for multi-bar display
    let mut segments = Vec::new();

    // Calculate memory values in bytes
    let actual_used_bytes = info
        .used_bytes
        .saturating_sub(info.buffers_bytes + info.cached_bytes);
    let actual_used_gb = actual_used_bytes as f64 / (1024.0 * 1024.0 * 1024.0);
    let buffers_gb = info.buffers_bytes as f64 / (1024.0 * 1024.0 * 1024.0);
    let cached_gb = info.cached_bytes as f64 / (1024.0 * 1024.0 * 1024.0);

    // Add used memory segment (actual used without buffers/cache)
    if actual_used_bytes > 0 {
        segments.push(BarSegment::memory_used(actual_used_gb));
    }

    // Add buffers segment
    if info.buffers_bytes > 0 {
        segments.push(BarSegment::memory_buffers(buffers_gb));
    }

    // Add cache segment
    if info.cached_bytes > 0 {
        segments.push(BarSegment::memory_cache(cached_gb));
    }

    // Calculate total used memory for display text
    let total_used_gb = actual_used_gb + buffers_gb + cached_gb;
    let display_text = format!("{total_used_gb:.1}GB");

    // Draw the multi-segment bar
    draw_bar_multi(
        stdout,
        "Mem",
        &segments,
        total_gb,
        gauge_width,
        Some(display_text),
    );

    print_colored_text(stdout, &" ".repeat(right_padding), Color::White, None, None);
    queue!(stdout, Print("\r\n")).unwrap();

    // Render the Swap row only when swap space exists on the host. Mirrors
    // the API exporter guard at `src/api/metrics/memory.rs:76` so non-swap
    // hosts (e.g., Apple Silicon before `dynamic_pager` allocates a swap
    // file) stay visually uncluttered. The row appears the moment macOS
    // creates a swap file, which is exactly when the user wants to see it.
    if info.swap_total_bytes > 0 {
        print_swap_bar(stdout, info, width, gauge_width, left_padding);
    }
}

/// Render the swap bar row that sits directly below the memory bar.
///
/// Layout mirrors the `Mem` row: 5-char left pad, full-width
/// `draw_bar_multi` gauge, trailing pad to the right edge. The bar
/// label is `Swap` and the overlay text shows `<used>GB` to match
/// the memory bar's `<total_used>GB` convention.
fn print_swap_bar<W: Write>(
    stdout: &mut W,
    info: &MemoryInfo,
    width: usize,
    gauge_width: usize,
    left_padding: usize,
) {
    let swap_total_gb = info.swap_total_bytes as f64 / (1024.0 * 1024.0 * 1024.0);
    let swap_used_gb = info.swap_used_bytes as f64 / (1024.0 * 1024.0 * 1024.0);

    // Right padding mirrors the Mem row so the trailing edges line up.
    let right_padding = width
        .saturating_sub(left_padding)
        .saturating_sub(gauge_width);

    print_colored_text(stdout, &" ".repeat(left_padding), Color::White, None, None);

    let mut segments = Vec::new();
    if info.swap_used_bytes > 0 {
        // Emphasize active swapping in red — the core signal from #220.
        segments.push(BarSegment::swap_used_active(swap_used_gb));
    }

    // Overlay text is always `<used>GB` so the column reads at a glance
    // alongside the Mem row's `<total_used>GB`. When idle (used == 0)
    // this naturally prints `0.0GB` over an empty bar, signaling
    // "swap is available but unused."
    let display_text = format!("{swap_used_gb:.1}GB");

    draw_bar_multi(
        stdout,
        "Swap",
        &segments,
        swap_total_gb,
        gauge_width,
        Some(display_text),
    );

    print_colored_text(stdout, &" ".repeat(right_padding), Color::White, None, None);
    queue!(stdout, Print("\r\n")).unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::MemoryInfo;

    /// Strip ANSI CSI sequences (ESC [ ... letter) so tests can inspect
    /// the visible characters that end up on the terminal.
    fn strip_ansi(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                // Consume "[" and then everything up to the final byte
                // (any ASCII letter terminates a CSI sequence).
                if chars.next() == Some('[') {
                    for nc in chars.by_ref() {
                        if nc.is_ascii_alphabetic() {
                            break;
                        }
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    fn foreground_sgr(color: Color) -> String {
        let mut buf: Vec<u8> = Vec::new();
        queue!(buf, crossterm::style::SetForegroundColor(color)).unwrap();
        String::from_utf8(buf).unwrap()
    }

    fn make_memory_info(hostname: &str) -> MemoryInfo {
        MemoryInfo {
            index: 0,
            host_id: "localhost".to_string(),
            hostname: hostname.to_string(),
            instance: String::new(),
            total_bytes: 16 * 1024 * 1024 * 1024,
            used_bytes: 8 * 1024 * 1024 * 1024,
            available_bytes: 8 * 1024 * 1024 * 1024,
            free_bytes: 4 * 1024 * 1024 * 1024,
            buffers_bytes: 1024 * 1024 * 1024,
            cached_bytes: 3 * 1024 * 1024 * 1024,
            swap_total_bytes: 4 * 1024 * 1024 * 1024,
            swap_used_bytes: 512 * 1024 * 1024,
            swap_free_bytes: 3584 * 1024 * 1024,
            utilization: 50.0,
            time: String::new(),
        }
    }

    #[test]
    fn test_print_memory_info_with_hostname() {
        let info = make_memory_info("myhost");
        let mut buf: Vec<u8> = Vec::new();
        print_memory_info(&mut buf, 0, &info, 120, 0, true);
        let output = String::from_utf8_lossy(&buf);
        assert!(output.contains("myhost"));
        assert!(!buf.is_empty());
    }

    #[test]
    fn test_print_memory_info_without_hostname() {
        let info = make_memory_info("myhost");
        let mut buf: Vec<u8> = Vec::new();
        print_memory_info(&mut buf, 0, &info, 120, 0, false);
        let output = String::from_utf8_lossy(&buf);
        // hostname is suppressed in local mode
        assert!(!output.contains("@ myhost"));
        assert!(!buf.is_empty());
    }

    #[test]
    fn test_print_memory_info_long_hostname_scrolls() {
        let info = make_memory_info("very-long-hostname-value");
        let mut buf: Vec<u8> = Vec::new();
        print_memory_info(&mut buf, 0, &info, 120, 3, true);
        assert!(!buf.is_empty());
    }

    #[test]
    fn test_print_memory_info_narrow_width() {
        let info = make_memory_info("host");
        let mut buf: Vec<u8> = Vec::new();
        // Should not panic with a narrow terminal width
        print_memory_info(&mut buf, 0, &info, 30, 0, false);
        assert!(!buf.is_empty());
    }

    // -----------------------------------------------------------------------
    // Swap row rendering (issue #220)
    // -----------------------------------------------------------------------

    #[test]
    fn test_swap_row_renders_when_swap_total_nonzero() {
        // Default fixture has swap_total_bytes > 0 and swap_used_bytes > 0,
        // so the Swap row must appear.
        let info = make_memory_info("host");
        let mut buf: Vec<u8> = Vec::new();
        print_memory_info(&mut buf, 0, &info, 120, 0, false);
        let visible = strip_ansi(&String::from_utf8_lossy(&buf));
        assert!(
            visible.contains("Swap"),
            "Swap row should appear when swap_total_bytes > 0; got: {visible:?}"
        );
        // The label has its own line (newline separates Mem and Swap rows).
        let row_count = visible.matches("\r\n").count();
        assert!(
            row_count >= 3,
            "Expected at least 3 newlines (info + Mem + Swap), got {row_count} in: {visible:?}"
        );
    }

    #[test]
    fn test_swap_row_hidden_when_swap_total_zero() {
        let mut info = make_memory_info("host");
        info.swap_total_bytes = 0;
        info.swap_used_bytes = 0;
        info.swap_free_bytes = 0;

        let mut buf: Vec<u8> = Vec::new();
        print_memory_info(&mut buf, 0, &info, 120, 0, false);
        let visible = strip_ansi(&String::from_utf8_lossy(&buf));
        assert!(
            !visible.contains("Swap"),
            "Swap row should NOT appear when swap_total_bytes == 0; got: {visible:?}"
        );
    }

    #[test]
    fn test_swap_row_renders_when_total_present_but_unused() {
        // Apple Silicon's `dynamic_pager` can create a swap file with
        // zero usage; the row should still appear so the user knows
        // swap exists, but in the idle (non-emphasized) state.
        let mut info = make_memory_info("host");
        info.swap_used_bytes = 0;
        info.swap_free_bytes = info.swap_total_bytes;

        let mut buf: Vec<u8> = Vec::new();
        print_memory_info(&mut buf, 0, &info, 120, 0, false);
        let visible = strip_ansi(&String::from_utf8_lossy(&buf));
        assert!(
            visible.contains("Swap"),
            "Swap row should still appear when swap_used_bytes == 0 but swap_total_bytes > 0; got: {visible:?}"
        );
        // Used = 0 bytes -> overlay text "0.0GB" must appear.
        assert!(
            visible.contains("0.0GB"),
            "Idle swap row should show '0.0GB' overlay text; got: {visible:?}"
        );
    }

    #[test]
    fn test_swap_row_active_uses_red_color() {
        // When swap is actively in use, the rendered output should
        // contain the red ANSI color SGR for the active swap segment.
        // crossterm Color::Red maps to SGR "31".
        let info = make_memory_info("host");
        assert!(info.swap_used_bytes > 0, "fixture precondition");

        let mut buf: Vec<u8> = Vec::new();
        print_memory_info(&mut buf, 0, &info, 120, 0, false);
        let raw = String::from_utf8_lossy(&buf);
        let red_sgr = foreground_sgr(Color::Red);
        assert!(
            raw.contains(&red_sgr),
            "Active swap row should be colored red; raw output did not contain {red_sgr:?}"
        );
    }

    #[test]
    fn test_swap_row_renders_at_narrow_width() {
        // Narrow widths should not panic and the Swap row should still
        // render. The bar's left padding and gauge_width are derived
        // via saturating arithmetic so this exercises the underflow
        // guard added alongside the swap row.
        let info = make_memory_info("host");
        let mut buf: Vec<u8> = Vec::new();
        print_memory_info(&mut buf, 0, &info, 30, 0, false);
        let visible = strip_ansi(&String::from_utf8_lossy(&buf));
        assert!(
            visible.contains("Swap"),
            "Swap row should appear even at narrow width; got: {visible:?}"
        );
    }
}
