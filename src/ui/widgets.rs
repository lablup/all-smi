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

use crossterm::style::Color;

use crate::common::config::ThemeConfig;
use crate::ui::text::print_colored_text;

pub struct BarSegment {
    pub value: f64,
    pub color: Color,
    pub label: Option<String>,
}

impl BarSegment {
    pub fn new(value: f64, color: Color) -> Self {
        Self {
            value,
            color,
            label: None,
        }
    }

    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }
}

pub fn draw_bar<W: Write>(
    stdout: &mut W,
    label: &str,
    value: f64,
    max_value: f64,
    width: usize,
    show_text: Option<String>,
) {
    // Format label to exactly 5 characters for consistent alignment
    let formatted_label = if label.len() > 5 {
        // Trim to 5 characters if too long
        label[..5].to_string()
    } else {
        // Pad with spaces if too short
        format!("{label:<5}")
    };
    let available_bar_width = width.saturating_sub(9); // 9 for "LABEL: [" and "] " (5 + 4)

    // Calculate the filled portion
    let fill_ratio = (value / max_value).min(1.0);
    let filled_width = (available_bar_width as f64 * fill_ratio) as usize;

    // Choose color based on usage using ThemeConfig
    let color = ThemeConfig::progress_bar_color(fill_ratio);

    // Prepare text to display inside the bar with fixed width
    let display_text = if let Some(text) = show_text {
        // Ensure consistent width for value text (8 characters)
        if text.len() > 8 {
            text[..8].to_string()
        } else {
            format!("{text:>8}") // Right-align in 8-character field
        }
    } else {
        format!("{:>7.1}%", fill_ratio * 100.0) // Right-align percentage in 8-character field
    };

    // Print label
    print_colored_text(stdout, &formatted_label, Color::White, None, None);
    print_colored_text(stdout, ": [", Color::White, None, None);

    // Calculate positioning for right-aligned text
    let text_len = display_text.len();
    let text_pos = available_bar_width.saturating_sub(text_len);

    // Build the bar content in batches to reduce terminal escape sequences.
    // Instead of calling print_colored_text per character, we accumulate
    // consecutive runs of the same type and emit them as a single call.

    // Phase 1: filled segment before text overlay (if any)
    let filled_before_text = filled_width.min(text_pos);
    if filled_before_text > 0 {
        print_colored_text(stdout, &"▬".repeat(filled_before_text), color, None, None);
    }

    // Phase 2: empty segment between filled area and text overlay (if any)
    let empty_before_text = text_pos.saturating_sub(filled_width);
    if empty_before_text > 0 {
        print_colored_text(
            stdout,
            &"─".repeat(empty_before_text),
            Color::DarkGrey,
            None,
            None,
        );
    }

    // Phase 3: text overlay region
    if text_len > 0 {
        print_colored_text(stdout, &display_text, Color::Grey, None, None);
    }

    // Phase 4: filled segment after text overlay (if any)
    let after_text_start = text_pos + text_len;
    let filled_after_text = filled_width.saturating_sub(after_text_start);
    if filled_after_text > 0 {
        print_colored_text(stdout, &"▬".repeat(filled_after_text), color, None, None);
    }

    // Phase 5: empty segment after everything
    let total_used = after_text_start + filled_after_text;
    let empty_after = available_bar_width.saturating_sub(total_used);
    if empty_after > 0 {
        print_colored_text(
            stdout,
            &"─".repeat(empty_after),
            Color::DarkGrey,
            None,
            None,
        );
    }

    print_colored_text(stdout, "]", Color::White, None, None);
}

pub fn draw_bar_multi<W: Write>(
    stdout: &mut W,
    label: &str,
    segments: &[BarSegment],
    max_value: f64,
    width: usize,
    show_text: Option<String>,
) {
    // Format label to exactly 5 characters for consistent alignment
    let formatted_label = if label.len() > 5 {
        label[..5].to_string()
    } else {
        format!("{label:<5}")
    };
    let available_bar_width = width.saturating_sub(9); // 9 for "LABEL: [" and "] " (5 + 4)

    // Calculate total value
    let total_value: f64 = segments.iter().map(|s| s.value).sum();
    let total_ratio = (total_value / max_value).min(1.0);

    // Prepare text to display inside the bar
    let display_text = if let Some(text) = show_text {
        // Ensure consistent width for value text (8 characters)
        if text.len() > 8 {
            text[..8].to_string()
        } else {
            format!("{text:>8}")
        }
    } else {
        format!("{:>7.1}%", total_ratio * 100.0)
    };

    // Print label
    print_colored_text(stdout, &formatted_label, Color::White, None, None);
    print_colored_text(stdout, ": [", Color::White, None, None);

    // Calculate positioning for right-aligned text
    let text_len = display_text.len();
    let text_pos = available_bar_width.saturating_sub(text_len);

    // Calculate segment positions
    let mut segment_positions = Vec::new();
    let mut current_pos = 0;

    for segment in segments {
        let segment_ratio = segment.value / max_value;
        let segment_width = (available_bar_width as f64 * segment_ratio).round() as usize;
        segment_positions.push((current_pos, current_pos + segment_width, segment.color));
        current_pos += segment_width;
    }

    // Ensure we don't exceed the total filled width
    let total_filled_width = (available_bar_width as f64 * total_ratio).round() as usize;
    if current_pos > total_filled_width {
        // Adjust the last segment to fit
        if let Some(last) = segment_positions.last_mut() {
            last.1 = total_filled_width;
        }
    }

    // Build the bar content in batches to reduce terminal escape sequences.
    // We emit consecutive runs of the same segment/empty type as single calls.

    // Classify each position into a region type, then batch consecutive same-type runs.
    // Region types: Segment(color), Empty, Text
    let text_end = text_pos + text_len;
    let mut pos = 0;

    while pos < available_bar_width {
        if pos >= text_pos && pos < text_end {
            // Text overlay region -- emit all text chars at once
            print_colored_text(stdout, &display_text, Color::Grey, None, None);
            pos = text_end;
            continue;
        }

        // Find which segment this position belongs to
        let seg_match = segment_positions
            .iter()
            .find(|seg| pos >= seg.0 && pos < seg.1);

        if let Some(&(_, end, color)) = seg_match {
            // Batch the entire segment run up to text_pos or segment end.
            // When text_pos is behind or at pos (text already emitted),
            // use the segment end directly instead.
            let run_end = if text_pos > pos {
                end.min(text_pos).min(available_bar_width)
            } else {
                end.min(available_bar_width)
            };
            let run_len = run_end.saturating_sub(pos);
            if run_len > 0 {
                print_colored_text(stdout, &"▬".repeat(run_len), color, None, None);
                pos += run_len;
            } else {
                // Segment ends at or before this position; advance past it
                // to guarantee forward progress.
                pos = end.max(pos + 1);
            }
        } else {
            // Empty region -- batch until the next segment, text, or end
            let next_boundary = segment_positions
                .iter()
                .filter_map(|seg| if seg.0 > pos { Some(seg.0) } else { None })
                .min()
                .unwrap_or(available_bar_width)
                .min(text_pos)
                .min(available_bar_width);
            let run_len = next_boundary.saturating_sub(pos).max(1);
            print_colored_text(stdout, &"─".repeat(run_len), Color::DarkGrey, None, None);
            pos += run_len;
        }
    }

    print_colored_text(stdout, "]", Color::White, None, None);
}

// Helper functions for common use cases
impl BarSegment {
    // CPU usage helpers (reserved for future use)
    #[allow(dead_code)]
    pub fn cpu_low_priority(value: f64) -> Self {
        // nice
        Self::new(value, Color::Blue).with_label("low")
    }

    #[allow(dead_code)]
    pub fn cpu_normal(value: f64) -> Self {
        // user
        Self::new(value, Color::Green).with_label("normal")
    }

    #[allow(dead_code)]
    pub fn cpu_kernel(value: f64) -> Self {
        // system
        Self::new(value, Color::Red).with_label("kernel")
    }

    #[allow(dead_code)]
    pub fn cpu_virtualized(value: f64) -> Self {
        // steal + guest
        Self::new(value, Color::DarkBlue).with_label("virtual")
    }

    // Memory usage helpers
    pub fn memory_used(value: f64) -> Self {
        Self::new(value, Color::Green).with_label("used")
    }

    pub fn memory_buffers(value: f64) -> Self {
        Self::new(value, Color::Blue).with_label("buffers")
    }

    pub fn memory_cache(value: f64) -> Self {
        Self::new(value, Color::Yellow).with_label("cache")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::buffer::BufferWriter;

    // Helper: render a bar into a BufferWriter and strip ANSI escape sequences
    // so we can inspect the visible character content.
    fn strip_ansi(s: &str) -> String {
        // Very simple stripper: remove ESC [ ... m sequences
        let mut out = String::with_capacity(s.len());
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                // consume until 'm' or end
                for nc in chars.by_ref() {
                    if nc == 'm' {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    // -----------------------------------------------------------------------
    // draw_bar: basic rendering properties
    // -----------------------------------------------------------------------

    #[test]
    fn test_draw_bar_zero_value_produces_output() {
        let mut bw = BufferWriter::new();
        draw_bar(&mut bw, "CPU", 0.0, 100.0, 40, None);
        let raw = bw.get_buffer();
        assert!(!raw.is_empty(), "draw_bar should produce non-empty output");
    }

    #[test]
    fn test_draw_bar_full_value_produces_output() {
        let mut bw = BufferWriter::new();
        draw_bar(&mut bw, "MEM", 100.0, 100.0, 40, None);
        let raw = bw.get_buffer();
        assert!(!raw.is_empty());
    }

    #[test]
    fn test_draw_bar_label_appears_in_output() {
        let mut bw = BufferWriter::new();
        draw_bar(&mut bw, "GPU", 50.0, 100.0, 40, None);
        let visible = strip_ansi(bw.get_buffer());
        assert!(
            visible.contains("GPU"),
            "Label 'GPU' should appear in bar output; got: {visible:?}"
        );
    }

    #[test]
    fn test_draw_bar_brackets_present() {
        let mut bw = BufferWriter::new();
        draw_bar(&mut bw, "CPU", 25.0, 100.0, 40, None);
        let visible = strip_ansi(bw.get_buffer());
        assert!(visible.contains('['), "Opening bracket missing");
        assert!(visible.contains(']'), "Closing bracket missing");
    }

    #[test]
    fn test_draw_bar_percentage_text_shown() {
        // When no show_text is provided the bar renders a percentage.
        let mut bw = BufferWriter::new();
        draw_bar(&mut bw, "CPU", 50.0, 100.0, 40, None);
        let visible = strip_ansi(bw.get_buffer());
        // 50.0 / 100.0 * 100 = 50.0 -> should contain "50.0%"
        assert!(
            visible.contains("50.0%"),
            "Percentage text missing; got: {visible:?}"
        );
    }

    #[test]
    fn test_draw_bar_custom_text_overrides_percent() {
        let mut bw = BufferWriter::new();
        draw_bar(&mut bw, "CPU", 50.0, 100.0, 40, Some("8.0GB".to_string()));
        let visible = strip_ansi(bw.get_buffer());
        assert!(
            visible.contains("8.0GB"),
            "Custom text missing; got: {visible:?}"
        );
        assert!(
            !visible.contains('%'),
            "Percentage should not appear when custom text is set"
        );
    }

    #[test]
    fn test_draw_bar_long_label_trimmed() {
        // Labels longer than 5 chars should be trimmed to 5
        let mut bw = BufferWriter::new();
        draw_bar(&mut bw, "TOOLONG", 0.0, 100.0, 40, None);
        let visible = strip_ansi(bw.get_buffer());
        // Only first 5 chars "TOOLO" should appear
        assert!(
            visible.contains("TOOLO"),
            "Trimmed label missing; got: {visible:?}"
        );
        assert!(
            !visible.contains("TOOLONG"),
            "Full untrimmed label should not appear; got: {visible:?}"
        );
    }

    // -----------------------------------------------------------------------
    // draw_bar: batching reduces escape sequence count
    // -----------------------------------------------------------------------

    #[test]
    fn test_draw_bar_batches_segments() {
        // For a long bar, the batched implementation should produce significantly
        // fewer escape sequences than one-per-character.
        // We count ESC occurrences in the raw output.
        let mut bw = BufferWriter::new();
        draw_bar(&mut bw, "CPU", 50.0, 100.0, 120, None);
        let raw = bw.get_buffer();

        let esc_count = raw.chars().filter(|&c| c == '\x1b').count();
        // Available bar width = 120 - 9 = 111 chars.
        // An unbatched implementation would emit 1 escape per char ≈ 111 + overhead.
        // The batched version should emit far fewer. We use 20 as a conservative upper bound.
        assert!(
            esc_count <= 20,
            "Too many escape sequences ({esc_count}); batching may not be working"
        );
    }

    // -----------------------------------------------------------------------
    // draw_bar_multi: basic rendering properties
    // -----------------------------------------------------------------------

    #[test]
    fn test_draw_bar_multi_empty_segments_produces_output() {
        let mut bw = BufferWriter::new();
        draw_bar_multi(&mut bw, "MEM", &[], 100.0, 40, None);
        let raw = bw.get_buffer();
        assert!(!raw.is_empty());
    }

    #[test]
    fn test_draw_bar_multi_single_segment() {
        let mut bw = BufferWriter::new();
        let segments = vec![BarSegment::memory_used(40.0)];
        draw_bar_multi(&mut bw, "MEM", &segments, 100.0, 40, None);
        let visible = strip_ansi(bw.get_buffer());
        assert!(visible.contains("MEM"), "Label missing");
        assert!(visible.contains('['), "Opening bracket missing");
        assert!(visible.contains(']'), "Closing bracket missing");
    }

    #[test]
    fn test_draw_bar_multi_multiple_segments() {
        let mut bw = BufferWriter::new();
        let segments = vec![
            BarSegment::memory_used(30.0),
            BarSegment::memory_buffers(10.0),
            BarSegment::memory_cache(20.0),
        ];
        draw_bar_multi(&mut bw, "MEM", &segments, 100.0, 40, None);
        let raw = bw.get_buffer();
        assert!(!raw.is_empty());
        let visible = strip_ansi(raw);
        assert!(visible.contains("MEM"));
    }

    #[test]
    fn test_draw_bar_multi_batches_segments() {
        // Same escape-sequence batching test for draw_bar_multi.
        let mut bw = BufferWriter::new();
        let segments = vec![
            BarSegment::memory_used(25.0),
            BarSegment::memory_buffers(10.0),
            BarSegment::memory_cache(15.0),
        ];
        draw_bar_multi(&mut bw, "MEM", &segments, 100.0, 120, None);
        let raw = bw.get_buffer();
        let esc_count = raw.chars().filter(|&c| c == '\x1b').count();
        // Available bar width = 111 chars, 3 segments: expect far fewer than 111 escapes.
        assert!(
            esc_count <= 30,
            "Too many escape sequences ({esc_count}) in draw_bar_multi; batching may not be working"
        );
    }

    // -----------------------------------------------------------------------
    // draw_bar throughput: hot-path measurement
    // -----------------------------------------------------------------------

    #[test]
    fn test_draw_bar_throughput() {
        let mut bw = BufferWriter::new();
        let start = std::time::Instant::now();
        for i in 0..1000 {
            bw.reset();
            draw_bar(&mut bw, "CPU", (i % 101) as f64, 100.0, 80, None);
        }
        let elapsed = start.elapsed();
        assert!(
            elapsed.as_millis() < 1000,
            "draw_bar throughput too slow: {elapsed:?}"
        );
    }

    #[test]
    fn test_draw_bar_multi_throughput() {
        let mut bw = BufferWriter::new();
        let segments = vec![
            BarSegment::memory_used(30.0),
            BarSegment::memory_buffers(10.0),
            BarSegment::memory_cache(20.0),
        ];
        let start = std::time::Instant::now();
        for _ in 0..1000 {
            bw.reset();
            draw_bar_multi(&mut bw, "MEM", &segments, 100.0, 80, None);
        }
        let elapsed = start.elapsed();
        assert!(
            elapsed.as_millis() < 1000,
            "draw_bar_multi throughput too slow: {elapsed:?}"
        );
    }
}
