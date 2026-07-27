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

//! Braille-dot sparkline rendering utility.
//!
//! Each Unicode braille cell (U+2800–U+28FF) encodes a 2×4 sub-pixel grid,
//! giving 4× horizontal and 4× vertical resolution compared to half-block
//! sparklines. The dot layout per cell is:
//!
//! ```text
//! dot1(0x01)  dot4(0x08)
//! dot2(0x02)  dot5(0x10)
//! dot3(0x04)  dot6(0x20)
//! dot7(0x40)  dot8(0x80)
//! ```
//!
//! Left sub-column uses dots 1,2,3,7 (bits 0x01,0x02,0x04,0x40).
//! Right sub-column uses dots 4,5,6,8 (bits 0x08,0x10,0x20,0x80).
//! Rows fill bottom-up (bar-chart style) for maximum legibility at 4px height.
//!
//! ## Multi-row model
//!
//! [`sparkline_braille_rows`] is the shared rendering core: it stacks `rows`
//! terminal rows on top of each other, giving `rows * 4` vertical dot levels
//! instead of the 4 levels a single row provides. The returned `Vec<String>`
//! holds one string per terminal row, top row first. Fill order is bottom-up
//! across the *entire* stack (btop-style): a value fills the bottom terminal
//! row completely before any dot lights up in the row above it, and so on up
//! the stack. [`sparkline_braille`] is a thin `rows == 1` wrapper over this
//! shared core, so single-row callers are unaffected by the multi-row API.
//!
//! ## Horizontal mapping: scrolling window
//!
//! The horizontal axis is time, and one sub-column is always exactly one
//! sample. The rendered window is the most recent `width * 2` samples,
//! right-anchored: the newest sample owns the rightmost sub-column and each
//! new sample shifts the whole plot one sub-column to the left, dropping the
//! oldest sample off the left edge. The time scale is therefore constant, and
//! a feature keeps its on-screen width for as long as it stays in the window.
//!
//! When the series is shorter than the window (right after startup, before
//! the history buffer has filled), the leading sub-columns carry no sample and
//! are left blank, so the plot grows in from the right edge at its final
//! scale. Resampling the whole series to fill the width instead would make
//! every feature drift left *and* shrink as history accumulated, which reads
//! as the graph zooming out rather than scrolling.
//!
//! Callers are responsible for keeping enough history to fill the widest
//! graph they render; see [`AppConfig::HISTORY_MAX_ENTRIES`].
//!
//! [`AppConfig::HISTORY_MAX_ENTRIES`]: crate::common::config::AppConfig::HISTORY_MAX_ENTRIES

/// Row bit masks for the left sub-column, ordered bottom→top.
/// dots=1 fills only the bottom row; dots=4 fills all four rows.
const LEFT_BITS: [u32; 4] = [
    0x40, // dot7 – bottom row
    0x04, // dot3 – lower-mid row
    0x02, // dot2 – upper-mid row
    0x01, // dot1 – top row
];

/// Row bit masks for the right sub-column, ordered bottom→top.
const RIGHT_BITS: [u32; 4] = [
    0x80, // dot8 – bottom row
    0x20, // dot6 – lower-mid row
    0x10, // dot5 – upper-mid row
    0x08, // dot4 – top row
];

/// Render `data` as a braille-dot sparkline `width` columns wide.
///
/// This is a thin wrapper over [`sparkline_braille_rows`] with `rows == 1`;
/// see that function for the full behaviour contract (range handling,
/// the scrolling window, and edge cases).
///
/// # Arguments
/// - `data`: time-series samples, most-recent sample last.
/// - `width`: desired output width in terminal columns (each cell = 2 sub-columns).
/// - `range`: optional fixed `(min, max)`. When `None`, the range is derived
///   from the data automatically.
#[must_use]
pub fn sparkline_braille(data: &[f64], width: usize, range: Option<(f64, f64)>) -> String {
    let mut rows = sparkline_braille_rows(data, width, 1, range);
    rows.pop().unwrap_or_default()
}

/// Render `data` as a multi-row braille-dot sparkline occupying `rows`
/// stacked terminal rows.
///
/// # Arguments
/// - `data`: time-series samples, most-recent sample last.
/// - `width`: desired output width in terminal columns (each cell = 2 sub-columns).
/// - `rows`: number of terminal rows to render. Vertical resolution is
///   `rows * 4` dot levels, bar-filled bottom-up across the whole stack.
/// - `range`: optional fixed `(min, max)`. When `None`, the range is derived
///   from the data automatically.
///
/// # Returns
///
/// A `Vec<String>` with exactly `rows` entries, the first one being the
/// *top* terminal row and the last one the *bottom* terminal row. Each
/// string is exactly `width` characters long.
///
/// # Behaviour
/// - `rows == 0` → returns an empty `Vec`.
/// - Empty `data` → returns `rows` copies of `" ".repeat(width)` (ASCII
///   spaces, preserves layout).
/// - `width == 0` → returns `rows` empty strings.
/// - Only the most recent `width * 2` samples are drawn, one per sub-column,
///   right-anchored (see the module docs). A shorter series leaves the
///   leading sub-columns blank; cells with no sample in either sub-column
///   render as an ASCII space, so the layout width is unaffected.
/// - With `range == None` the auto-range is derived from the samples actually
///   drawn, not from the whole series, so a spike that has scrolled off the
///   left edge no longer compresses the visible scale.
/// - Constant input with auto-range → only the single bottom-most dot row of
///   the bottom terminal row is filled (`⣀` U+28C0 when `rows == 1`); all
///   rows above stay blank, so callers can still see that data is present.
/// - NaN / non-finite values are clamped to the minimum of the range.
/// - Degenerate explicit range `(lo, hi)` where `hi <= lo` → treated as
///   constant; only the bottom-most dot row is filled.
#[must_use]
pub fn sparkline_braille_rows(
    data: &[f64],
    width: usize,
    rows: usize,
    range: Option<(f64, f64)>,
) -> Vec<String> {
    if rows == 0 {
        return Vec::new();
    }
    if data.is_empty() {
        return vec![" ".repeat(width); rows];
    }
    if width == 0 {
        return vec![String::new(); rows];
    }

    // Total sub-columns = width * 2 (each braille cell has 2 horizontal sub-pixels).
    let n_sub = width * 2;
    let len = data.len();

    // Scrolling window: the plot shows the most recent `n_sub` samples, one
    // per sub-column, with the newest sample pinned to the rightmost
    // sub-column. `window` is that tail; when the series is shorter than the
    // window it is the whole series, and the leading `n_sub - len`
    // sub-columns stay blank so the plot grows in from the right instead of
    // being stretched across the full width.
    let window = &data[len.saturating_sub(n_sub)..];
    let blank_subs = n_sub - window.len();

    // Determine effective min/max. The auto-range covers only the window, so
    // a spike that has already scrolled off no longer flattens the plot.
    let (min, max) = match range {
        Some((lo, hi)) if !lo.is_finite() || !hi.is_finite() => {
            // Non-finite range bounds are treated as a degenerate (constant) range.
            (0.0_f64, 0.0_f64)
        }
        Some((lo, hi)) => (lo, hi),
        None => {
            let mut lo = f64::INFINITY;
            let mut hi = f64::NEG_INFINITY;
            for &v in window {
                if v.is_finite() {
                    if v < lo {
                        lo = v;
                    }
                    if v > hi {
                        hi = v;
                    }
                }
            }
            // All-NaN / all-infinite data: fall back to [0, 1] degenerate.
            if !lo.is_finite() {
                lo = 0.0;
            }
            if !hi.is_finite() {
                hi = lo;
            }
            (lo, hi)
        }
    };

    // Compute vertical level (0..rows*4, bottom→top) for a value.
    // When max <= min (constant / degenerate range) always returns 0.
    let total_levels = rows * 4;
    let level_of = |v: f64| -> usize {
        if max <= min {
            return 0;
        }
        // Non-finite samples clamp to the bottom of the range.
        let clamped = if v.is_finite() {
            v.clamp(min, max)
        } else {
            min
        };
        let norm = (clamped - min) / (max - min);
        // norm ∈ [0.0, 1.0]; multiply by total_levels and floor, clamped to
        // [0, total_levels - 1].
        ((norm * total_levels as f64).floor() as usize).min(total_levels - 1)
    };

    // Dots filled (1..=total_levels), bottom-up, per sub-column. `None` marks
    // a sub-column that predates the series and carries no sample.
    let dots_filled: Vec<Option<usize>> = (0..n_sub)
        .map(|i| i.checked_sub(blank_subs).map(|w| level_of(window[w]) + 1))
        .collect();

    // Build one output string per terminal row, top row first. Each row's
    // fill is derived by slicing the per-sub-column dot count into this
    // row's 4-dot window.
    let mut out_rows: Vec<String> = Vec::with_capacity(rows);
    for r in 0..rows {
        let row_from_bottom = rows - 1 - r;
        let row_base = row_from_bottom * 4;
        let mut row = String::with_capacity(width * 3); // braille chars are 3 bytes in UTF-8
        for cell in 0..width {
            let left = dots_filled[cell * 2];
            let right = dots_filled[cell * 2 + 1];

            // A cell with no sample on either side renders as a space, so the
            // not-yet-filled part of the window stays visually empty while
            // still occupying its column.
            if left.is_none() && right.is_none() {
                row.push(' ');
                continue;
            }

            let left_dots = left.map_or(0, |d| d.saturating_sub(row_base).min(4));
            let right_dots = right.map_or(0, |d| d.saturating_sub(row_base).min(4));

            // Bar-fill: fill this row's dots from bottom up to the computed count.
            let mut bits: u32 = 0;
            for &b in LEFT_BITS.iter().take(left_dots) {
                bits |= b;
            }
            for &b in RIGHT_BITS.iter().take(right_dots) {
                bits |= b;
            }

            let ch = char::from_u32(0x2800 + bits).unwrap_or('⠀');
            row.push(ch);
        }
        out_rows.push(row);
    }
    out_rows
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: count Unicode scalar values (chars) in a string.
    fn char_count(s: &str) -> usize {
        s.chars().count()
    }

    /// True if every char is a braille codepoint (U+2800..=U+28FF).
    fn all_braille(s: &str) -> bool {
        s.chars().all(|c| ('\u{2800}'..='\u{28FF}').contains(&c))
    }

    // 1. Empty input returns `width` ASCII spaces.
    #[test]
    fn empty_input_returns_spaces() {
        let result = sparkline_braille(&[], 8, None);
        assert_eq!(result.len(), 8, "should be 8 ASCII space bytes");
        assert_eq!(char_count(&result), 8);
        assert!(result.chars().all(|c| c == ' '));
    }

    // 2. width == 0 returns empty string.
    #[test]
    fn zero_width_returns_empty() {
        let result = sparkline_braille(&[1.0, 2.0, 3.0], 0, None);
        assert!(result.is_empty());
    }

    // 3. Single-point input does not panic and has length `width` in chars.
    #[test]
    fn single_point_no_panic() {
        let result = sparkline_braille(&[42.0], 5, None);
        assert_eq!(char_count(&result), 5);
    }

    // 4. Constant input with auto-range → bottom-row-filled braille cells only.
    //    Bottom row filled = both LEFT_BITS[0]=0x40 and RIGHT_BITS[0]=0x80 set
    //    → 0x2800 + 0x40 + 0x80 = 0x28C0 = '⣀'.
    #[test]
    fn constant_input_renders_bottom_row() {
        let data = vec![7.0; 10];
        let result = sparkline_braille(&data, 4, None);
        assert_eq!(char_count(&result), 4);
        // Every cell should be '⣀' (U+28C0).
        for ch in result.chars() {
            assert_eq!(
                ch, '\u{28C0}',
                "expected bottom-row-filled cell ⣀, got {ch:?}"
            );
        }
    }

    // 5. Monotonic ramp at width=2 → 2 chars, all valid braille.
    #[test]
    fn monotonic_ramp_valid_braille() {
        let data = [0.0, 1.0, 2.0, 3.0];
        let result = sparkline_braille(&data, 2, None);
        assert_eq!(char_count(&result), 2);
        assert!(
            all_braille(&result),
            "all chars should be braille codepoints"
        );
    }

    // 6. Explicit range clamps correctly: different ranges → different outputs,
    //    both of correct character length.
    #[test]
    fn explicit_range_different_outputs() {
        let data = [5.0, 10.0, 15.0];
        let wide = sparkline_braille(&data, 3, Some((0.0, 20.0)));
        let tight = sparkline_braille(&data, 3, Some((5.0, 15.0)));
        assert_eq!(char_count(&wide), 3);
        assert_eq!(char_count(&tight), 3);
        // The two outputs should differ because the scale is different.
        assert_ne!(
            wide, tight,
            "different ranges should produce different sparklines"
        );
    }

    // 7. Degenerate explicit range (lo == hi) does not panic.
    #[test]
    fn degenerate_range_no_panic() {
        let result = sparkline_braille(&[5.0, 5.0, 5.0], 4, Some((5.0, 5.0)));
        assert_eq!(char_count(&result), 4);
    }

    // 8. NaN / infinity in data does not panic; returns correct length.
    #[test]
    fn nan_and_infinity_no_panic() {
        let data = [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 1.0, 2.0];
        let result = sparkline_braille(&data, 5, None);
        assert_eq!(char_count(&result), 5);
    }

    // 9. Non-finite range bounds do not panic; output has correct char length.
    //    This validates the guard against NaN/infinite explicit range arguments.
    #[test]
    fn non_finite_range_bounds_no_panic() {
        let result = sparkline_braille(&[1.0], 4, Some((f64::NAN, 1.0)));
        assert_eq!(
            char_count(&result),
            4,
            "should return 4 chars even with NaN range bound"
        );
        let result2 = sparkline_braille(&[1.0], 4, Some((0.0, f64::INFINITY)));
        assert_eq!(
            char_count(&result2),
            4,
            "should return 4 chars even with infinite range bound"
        );
    }

    // 10. Multi-row dimensions: Vec length equals `rows`, each row's char
    //     count equals `width`.
    #[test]
    fn multirow_dimensions() {
        let data: Vec<f64> = (0..50).map(|i| (i as f64).sin() * 10.0 + 20.0).collect();
        let rows = sparkline_braille_rows(&data, 6, 3, None);
        assert_eq!(rows.len(), 3, "should return one string per row");
        for row in &rows {
            assert_eq!(char_count(row), 6, "each row should be `width` chars");
            assert!(all_braille(row), "all chars should be braille codepoints");
        }
    }

    // 11. Level continuity across row boundaries: a value at exactly 50% of
    //     the range with rows=2 fills the entire bottom terminal row.
    #[test]
    fn level_continuity_fills_bottom_row_at_half_range() {
        let data = vec![50.0; 8];
        let rows = sparkline_braille_rows(&data, 4, 2, Some((0.0, 100.0)));
        assert_eq!(rows.len(), 2);
        let bottom_row = &rows[1]; // last element is the bottom terminal row
        for ch in bottom_row.chars() {
            assert_eq!(
                ch, '\u{28FF}',
                "bottom terminal row should be fully filled at exactly 50% of range, got {ch:?}"
            );
        }
    }

    // 12. Spike preservation: 99 zeros plus one 100.0 at width 8 renders at
    //     least one top-level dot (bit 0x01 or 0x08).
    #[test]
    fn spike_preservation_width_8() {
        let mut data = vec![0.0; 99];
        data.push(100.0);
        let result = sparkline_braille(&data, 8, None);
        assert_eq!(char_count(&result), 8);
        let has_top_dot = result.chars().any(|c| {
            let bits = c as u32 - 0x2800;
            bits & (0x01 | 0x08) != 0
        });
        assert!(
            has_top_dot,
            "a single-sample spike should render at least one top-level dot, got {result:?}"
        );
    }

    // 13. rows == 1 parity: `sparkline_braille` must equal
    //     `sparkline_braille_rows(..., rows = 1, ...)[0]` for representative
    //     inputs, since the former is a thin wrapper over the latter.
    #[test]
    fn rows_one_matches_wrapper() {
        type Case = (Vec<f64>, usize, Option<(f64, f64)>);
        let cases: Vec<Case> = vec![
            (vec![], 8, None),
            (vec![1.0, 2.0, 3.0], 0, None),
            (vec![42.0], 5, None),
            (vec![7.0; 10], 4, None),
            ((0..30).map(|i| i as f64).collect(), 8, None),
            (vec![5.0, 10.0, 15.0], 3, Some((0.0, 20.0))),
            (vec![f64::NAN, f64::INFINITY, 1.0, 2.0], 5, None),
        ];
        for (data, width, range) in cases {
            let direct = sparkline_braille(&data, width, range);
            let via_rows = sparkline_braille_rows(&data, width, 1, range);
            assert_eq!(via_rows.len(), 1);
            assert_eq!(
                direct, via_rows[0],
                "sparkline_braille should match sparkline_braille_rows(..., 1, ...)[0] for {data:?}"
            );
        }
    }

    // 14. Windowing: samples older than the window are dropped off the left
    //     edge, and the newest sample owns the rightmost sub-column.
    #[test]
    fn window_keeps_the_most_recent_samples() {
        // 4 samples, width=1 -> 2 sub-columns, so only the last 2 samples
        // ({5.0, 0.0}) are drawn: left sub-column = 5.0 (full), right
        // sub-column = 0.0 (bottom dot only). The leading zeros scrolled off.
        let data = [0.0, 0.0, 5.0, 0.0];
        let result = sparkline_braille(&data, 1, Some((0.0, 5.0)));
        assert_eq!(char_count(&result), 1);
        let ch = result.chars().next().expect("single char");
        let bits = ch as u32 - 0x2800;
        assert_eq!(
            bits & (0x40 | 0x04 | 0x02 | 0x01),
            0x40 | 0x04 | 0x02 | 0x01,
            "left sub-column should be fully filled by data[2] = 5.0"
        );
        assert_eq!(bits & 0x80, 0x80, "right bottom dot should be set");
        assert_eq!(
            bits & 0x08,
            0,
            "right top dot should be clear (data[3] = 0.0 is the newest sample)"
        );
    }

    // 15. Multi-row API: empty data returns `rows` copies of `width` spaces.
    #[test]
    fn multirow_empty_data() {
        let rows = sparkline_braille_rows(&[], 6, 3, None);
        assert_eq!(rows.len(), 3);
        for row in &rows {
            assert_eq!(char_count(row), 6);
            assert!(row.chars().all(|c| c == ' '));
        }
    }

    // 16. Multi-row API: zero width returns `rows` empty strings.
    #[test]
    fn multirow_zero_width() {
        let rows = sparkline_braille_rows(&[1.0, 2.0, 3.0], 0, 3, None);
        assert_eq!(rows.len(), 3);
        for row in &rows {
            assert!(row.is_empty());
        }
    }

    // 17. Multi-row API: zero rows returns an empty Vec.
    #[test]
    fn multirow_zero_rows() {
        let rows = sparkline_braille_rows(&[1.0, 2.0, 3.0], 8, 0, None);
        assert!(rows.is_empty());

        // Also true for empty data / zero width combined with zero rows.
        assert!(sparkline_braille_rows(&[], 8, 0, None).is_empty());
        assert!(sparkline_braille_rows(&[1.0], 0, 0, None).is_empty());
    }

    // 18. Right-anchored short history: when `data.len() < width * 2`, the
    //     plot grows in from the right edge instead of stretching across the
    //     full width. The leading sub-columns stay blank.
    #[test]
    fn short_history_is_right_anchored() {
        // 2 samples, width = 2 -> 4 sub-columns. Sub-columns 0,1 have no
        // sample (cell 0 is a space); sub-column 2 = data[0] = 0.0 (bottom
        // dot only) and sub-column 3 = data[1] = 10.0 (fully filled, the
        // most recent sample) share cell 1.
        let data = [0.0, 10.0];
        let result = sparkline_braille(&data, 2, Some((0.0, 10.0)));
        // cell 1 bits: left bottom (0x40) + right column filled (0xB8).
        assert_eq!(
            result, " \u{28F8}",
            "a 2-sample series at width 2 should leave the first cell blank and \
             render both samples right-anchored in the second cell"
        );
    }

    // 19. Scrolling: a fixed feature keeps its on-screen width and shifts left
    //     by exactly one sub-column per new sample. This is the regression
    //     guard for the "graph zooms out instead of scrolling" bug — with the
    //     old whole-series resampling the burst narrowed as history grew.
    #[test]
    fn window_scrolls_left_without_rescaling() {
        const WIDTH: usize = 8; // 16 sub-columns
        let burst_at = |offset: usize| -> Vec<f64> {
            // A 4-sample burst whose newest sample sits `offset` samples back
            // from the end, on a full (>= 16 sample) idle series.
            let mut v = vec![0.0; 40];
            let end = v.len() - offset;
            for x in v[end - 4..end].iter_mut() {
                *x = 100.0;
            }
            v
        };

        // Count sub-columns whose top dot is lit, i.e. the burst's width.
        let burst_width = |s: &str| -> usize {
            s.chars()
                .map(|c| {
                    let bits = c as u32 - 0x2800;
                    usize::from(bits & 0x01 != 0) + usize::from(bits & 0x08 != 0)
                })
                .sum()
        };
        // Index of the leftmost lit sub-column.
        let burst_start = |s: &str| -> Option<usize> {
            s.chars().enumerate().find_map(|(cell, c)| {
                let bits = c as u32 - 0x2800;
                if bits & 0x01 != 0 {
                    Some(cell * 2)
                } else if bits & 0x08 != 0 {
                    Some(cell * 2 + 1)
                } else {
                    None
                }
            })
        };

        let mut prev_start = None;
        for offset in 0..8 {
            let s = sparkline_braille(&burst_at(offset), WIDTH, Some((0.0, 100.0)));
            assert_eq!(
                burst_width(&s),
                4,
                "the burst must keep its width as it scrolls (offset={offset}): {s:?}"
            );
            let start = burst_start(&s).expect("burst must be visible");
            if let Some(prev) = prev_start {
                assert_eq!(
                    start + 1,
                    prev,
                    "the burst must shift left by exactly one sub-column per sample \
                     (offset={offset}): {s:?}"
                );
            }
            prev_start = Some(start);
        }
    }

    // 20. Auto-range follows the window: a spike that has scrolled out of the
    //     window must not keep compressing the visible scale.
    #[test]
    fn auto_range_uses_only_the_visible_window() {
        // 1000.0 sits far outside the last 4 sub-columns (width 2), so the
        // remaining constant tail must render as a constant series rather
        // than being flattened to the bottom of a 0..1000 scale.
        let mut data = vec![1000.0];
        data.extend(std::iter::repeat_n(5.0, 10));
        let result = sparkline_braille(&data, 2, None);
        assert_eq!(
            result, "\u{28C0}\u{28C0}",
            "the out-of-window spike must not affect the auto-range: {result:?}"
        );
    }
}
