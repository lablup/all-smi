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

use std::borrow::Cow;
use std::io::Write;

use crossterm::{
    queue,
    style::{Color, Print, ResetColor, SetBackgroundColor, SetForegroundColor},
};

// Helper function to get display width of a single character
pub fn char_display_width(c: char) -> usize {
    match c {
        // Arrow characters that display as 1 character width
        '←' | '→' | '↑' | '↓' => 1,
        // Most other characters display as their char count
        _ => 1,
    }
}

// Helper function to calculate display width of a string, accounting for Unicode characters
pub fn display_width(s: &str) -> usize {
    s.chars().map(char_display_width).sum()
}

/// Calculate the visible width of text containing ANSI escape sequences.
///
/// Crossterm emits CSI sequences for colors and styles. Those bytes do not
/// occupy terminal cells and therefore must not participate in responsive
/// layout calculations.
pub fn ansi_display_width(s: &str) -> usize {
    let mut width = 0;
    let mut chars = s.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '\x1b' {
            skip_ansi_sequence(&mut chars);
        } else if c != '\r' && c != '\n' {
            width += char_display_width(c);
        }
    }

    width
}

/// Truncate ANSI-styled text to at most `max_width` visible terminal cells.
///
/// Escape sequences are copied atomically and do not consume width. A reset
/// sequence is appended only on the truncating path so a clipped colored span
/// cannot leak its style into the next terminal row.
pub fn truncate_ansi_to_width(s: &str, max_width: usize) -> Cow<'_, str> {
    if ansi_display_width(s) <= max_width {
        return Cow::Borrowed(s);
    }

    let mut output = String::with_capacity(s.len().min(max_width.saturating_mul(2)) + 4);
    let mut visible_width = 0;
    let mut chars = s.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '\x1b' {
            output.push(c);
            copy_ansi_sequence(&mut chars, &mut output);
            continue;
        }

        if c == '\r' || c == '\n' {
            break;
        }

        let char_width = char_display_width(c);
        if visible_width + char_width > max_width {
            break;
        }
        output.push(c);
        visible_width += char_width;
    }

    output.push_str("\x1b[0m");
    Cow::Owned(output)
}

fn skip_ansi_sequence(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    let Some(kind) = chars.next() else {
        return;
    };

    match kind {
        '[' => {
            for c in chars.by_ref() {
                if ('@'..='~').contains(&c) {
                    break;
                }
            }
        }
        ']' => {
            let mut previous_was_escape = false;
            for c in chars.by_ref() {
                if c == '\u{7}' || (previous_was_escape && c == '\\') {
                    break;
                }
                previous_was_escape = c == '\x1b';
            }
        }
        _ => {}
    }
}

fn copy_ansi_sequence(chars: &mut std::iter::Peekable<std::str::Chars<'_>>, output: &mut String) {
    let Some(kind) = chars.next() else {
        return;
    };
    output.push(kind);

    match kind {
        '[' => {
            for c in chars.by_ref() {
                output.push(c);
                if ('@'..='~').contains(&c) {
                    break;
                }
            }
        }
        ']' => {
            let mut previous_was_escape = false;
            for c in chars.by_ref() {
                output.push(c);
                if c == '\u{7}' || (previous_was_escape && c == '\\') {
                    break;
                }
                previous_was_escape = c == '\x1b';
            }
        }
        _ => {}
    }
}

// Helper function to truncate a string to fit within a given display width.
//
// Returns `Cow::Borrowed` when the string already fits, avoiding allocation.
pub fn truncate_to_width(s: &str, max_width: usize) -> Cow<'_, str> {
    // Fast path: ASCII-only strings where byte length == display width
    if s.len() <= max_width {
        return Cow::Borrowed(s);
    }

    // The string is longer than max_width. For ASCII-only content we can
    // simply slice at the byte boundary.
    if s.is_ascii() {
        return Cow::Borrowed(&s[..max_width]);
    }

    // Slow path: walk char-by-char for non-ASCII content.
    let mut current_width = 0;
    let mut byte_end = 0;
    for c in s.chars() {
        let char_width = char_display_width(c);
        if current_width + char_width > max_width {
            break;
        }
        current_width += char_width;
        byte_end += c.len_utf8();
    }

    Cow::Borrowed(&s[..byte_end])
}

// Helper function to format RAM values with appropriate units
pub fn format_ram_value(gb_value: f64) -> String {
    if gb_value >= 1024.0 {
        format!("{:.2}TB", gb_value / 1024.0)
    } else if gb_value < 1.0 {
        // For sub-GB values (like 512MB = 0.5GB), show with 1 decimal place
        format!("{gb_value:.1}GB")
    } else {
        format!("{gb_value:.0}GB")
    }
}

/// Write colored text to a terminal buffer.
///
/// When `width` is `None` (the common hot-path case), the text is printed
/// directly without any intermediate `String` allocation. When `width` is
/// `Some(w)`, the text is padded or truncated to exactly `w` characters.
pub fn print_colored_text<W: Write>(
    stdout: &mut W,
    text: &str,
    fg_color: Color,
    bg_color: Option<Color>,
    width: Option<usize>,
) {
    match width {
        Some(w) => {
            // Width-adjusted path: only allocate when padding/truncation is needed
            let adjusted: Cow<'_, str> = if text.len() > w {
                truncate_to_width(text, w)
            } else if text.len() < w {
                Cow::Owned(format!("{text:<w$}"))
            } else {
                Cow::Borrowed(text)
            };

            if let Some(bg) = bg_color {
                queue!(
                    stdout,
                    SetForegroundColor(fg_color),
                    SetBackgroundColor(bg),
                    Print(adjusted.as_ref()),
                    ResetColor
                )
                .unwrap();
            } else {
                queue!(
                    stdout,
                    SetForegroundColor(fg_color),
                    Print(adjusted.as_ref()),
                    ResetColor
                )
                .unwrap();
            }
        }
        None => {
            // Hot path: no width adjustment, print text directly without allocation
            if let Some(bg) = bg_color {
                queue!(
                    stdout,
                    SetForegroundColor(fg_color),
                    SetBackgroundColor(bg),
                    Print(text),
                    ResetColor
                )
                .unwrap();
            } else {
                queue!(
                    stdout,
                    SetForegroundColor(fg_color),
                    Print(text),
                    ResetColor
                )
                .unwrap();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // truncate_to_width: correctness and allocation behavior
    // -----------------------------------------------------------------------

    #[test]
    fn test_truncate_to_width_short_string_borrows() {
        let s = "hello";
        let result = truncate_to_width(s, 10);
        // Should borrow, not allocate
        assert!(matches!(result, Cow::Borrowed(_)));
        assert_eq!(&*result, "hello");
    }

    #[test]
    fn test_truncate_to_width_exact_fit_borrows() {
        let s = "hello";
        let result = truncate_to_width(s, 5);
        assert!(matches!(result, Cow::Borrowed(_)));
        assert_eq!(&*result, "hello");
    }

    #[test]
    fn test_truncate_to_width_truncates_ascii() {
        let s = "hello world";
        let result = truncate_to_width(s, 5);
        // ASCII fast path: should borrow a slice
        assert!(matches!(result, Cow::Borrowed(_)));
        assert_eq!(&*result, "hello");
    }

    #[test]
    fn test_truncate_to_width_empty_string() {
        let result = truncate_to_width("", 5);
        assert!(matches!(result, Cow::Borrowed(_)));
        assert_eq!(&*result, "");
    }

    #[test]
    fn test_truncate_to_width_zero_width() {
        let result = truncate_to_width("hello", 0);
        assert_eq!(&*result, "");
    }

    #[test]
    fn test_truncate_to_width_unicode_arrows() {
        // Arrows have display width 1 each
        let s = "↑↓←→abc";
        let result = truncate_to_width(s, 4);
        assert_eq!(&*result, "↑↓←→");
    }

    // -----------------------------------------------------------------------
    // display_width
    // -----------------------------------------------------------------------

    #[test]
    fn test_display_width_ascii() {
        assert_eq!(display_width("hello"), 5);
    }

    #[test]
    fn test_display_width_arrows() {
        assert_eq!(display_width("↑↓"), 2);
    }

    #[test]
    fn test_display_width_empty() {
        assert_eq!(display_width(""), 0);
    }

    #[test]
    fn test_ansi_display_width_ignores_style_sequences() {
        assert_eq!(ansi_display_width("\x1b[31mGPU 42%\x1b[0m"), 7);
        assert_eq!(ansi_display_width("\x1b]0;title\u{7}CPU"), 3);
    }

    #[test]
    fn test_truncate_ansi_to_width_preserves_complete_sequences() {
        let input = "\x1b[31mGPU 42%\x1b[0m trailing";
        let truncated = truncate_ansi_to_width(input, 7);

        assert_eq!(ansi_display_width(&truncated), 7);
        assert!(truncated.starts_with("\x1b[31mGPU 42%"));
        assert!(truncated.ends_with("\x1b[0m"));
        assert!(!truncated.contains("trailing"));
    }

    #[test]
    fn test_truncate_ansi_to_width_borrows_when_text_fits() {
        let input = "\x1b[32mok\x1b[0m";
        assert!(matches!(truncate_ansi_to_width(input, 2), Cow::Borrowed(_)));
    }

    // -----------------------------------------------------------------------
    // format_ram_value
    // -----------------------------------------------------------------------

    #[test]
    fn test_format_ram_value_gb() {
        assert_eq!(format_ram_value(16.0), "16GB");
    }

    #[test]
    fn test_format_ram_value_tb() {
        assert_eq!(format_ram_value(2048.0), "2.00TB");
    }

    #[test]
    fn test_format_ram_value_sub_gb() {
        assert_eq!(format_ram_value(0.5), "0.5GB");
    }

    // -----------------------------------------------------------------------
    // Throughput measurement
    // -----------------------------------------------------------------------

    #[test]
    fn test_truncate_to_width_throughput() {
        let long_string = "a".repeat(200);
        let start = std::time::Instant::now();
        for _ in 0..100_000 {
            let _ = truncate_to_width(&long_string, 80);
        }
        let elapsed = start.elapsed();
        // 100k truncations should complete well under 1 second
        assert!(
            elapsed.as_millis() < 1000,
            "truncate_to_width throughput too slow: {elapsed:?}"
        );
    }
}
