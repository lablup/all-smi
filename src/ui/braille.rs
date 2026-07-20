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
//! ## Resampling: bucket max-pooling
//!
//! Horizontal resampling maps `width * 2` braille sub-columns onto the input
//! time series using bucket max-pooling rather than nearest-neighbour
//! sampling. Each sub-column owns a contiguous bucket of input samples, and
//! the rendered level is derived from the bucket's maximum finite value.
//! Every input sample belongs to at least one bucket, every bucket is
//! non-empty, and the rightmost sub-column always covers the most recent
//! sample. When there are fewer samples than sub-columns, buckets are
//! stretched (a sample is repeated across multiple sub-columns) so every
//! sub-column still owns at least one sample. An all-non-finite bucket
//! clamps to the range minimum. This keeps transient spikes visible at any
//! output width, unlike nearest-neighbour resampling, which can skip a spike
//! entirely if it does not land on a sampled index.

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
/// bucket max-pooling resampling, and edge cases).
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
/// - Constant input with auto-range → only the single bottom-most dot row of
///   the bottom terminal row is filled (`⣀` U+28C0 when `rows == 1`); all
///   rows above stay blank, so callers can still see that data is present.
/// - NaN / non-finite values are clamped to the minimum of the range.
/// - Degenerate explicit range `(lo, hi)` where `hi <= lo` → treated as
///   constant; only the bottom-most dot row is filled.
///
/// Resampling uses bucket max-pooling: see the module documentation for
/// details. In short, each of the `width * 2` sub-columns owns a contiguous,
/// non-empty bucket of `data`, and its level is derived from the bucket's
/// maximum finite value, so a single-sample spike remains visible at any
/// output width.
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

    // Determine effective min/max.
    let (min, max) = match range {
        Some((lo, hi)) if !lo.is_finite() || !hi.is_finite() => {
            // Non-finite range bounds are treated as a degenerate (constant) range.
            (0.0_f64, 0.0_f64)
        }
        Some((lo, hi)) => (lo, hi),
        None => {
            let mut lo = f64::INFINITY;
            let mut hi = f64::NEG_INFINITY;
            for &v in data {
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

    // Total sub-columns = width * 2 (each braille cell has 2 horizontal sub-pixels).
    let n_sub = width * 2;
    let len = data.len();

    // Bucket max-pooling: sub-column `i` owns `data[start..end)`.
    //
    // `start`/`end` form a standard floor partition of `data` into `n_sub`
    // buckets, which is already non-empty for every bucket when
    // `len >= n_sub`. When `len < n_sub`, the natural `end` can equal
    // `start`, so it is pushed up to at least `start + 1` (stretching,
    // i.e. repeating a sample across multiple sub-columns) to guarantee
    // every sub-column owns at least one sample. The last sub-column's
    // natural `end` is always exactly `len`, so it always covers the most
    // recent sample regardless of stretching.
    let bucket_max = |i: usize| -> f64 {
        let start = i * len / n_sub;
        let end = ((i + 1) * len / n_sub).max(start + 1).min(len);
        let mut m = f64::NEG_INFINITY;
        for &v in &data[start..end] {
            if v.is_finite() && v > m {
                m = v;
            }
        }
        if m.is_finite() { m } else { min }
    };

    // Compute vertical level (0..rows*4, bottom→top) for a value.
    // When max <= min (constant / degenerate range) always returns 0.
    let total_levels = rows * 4;
    let level_of = |v: f64| -> usize {
        if max <= min {
            return 0;
        }
        let clamped = v.clamp(min, max);
        let norm = (clamped - min) / (max - min);
        // norm ∈ [0.0, 1.0]; multiply by total_levels and floor, clamped to
        // [0, total_levels - 1].
        ((norm * total_levels as f64).floor() as usize).min(total_levels - 1)
    };

    // Number of dots filled (1..=total_levels), bottom-up, for each sub-column.
    let dots_filled: Vec<usize> = (0..n_sub).map(|i| level_of(bucket_max(i)) + 1).collect();

    // Build one output string per terminal row, top row first. Each row's
    // fill is derived by slicing the per-sub-column dot count into this
    // row's 4-dot window.
    let mut out_rows: Vec<String> = Vec::with_capacity(rows);
    for r in 0..rows {
        let row_from_bottom = rows - 1 - r;
        let row_base = row_from_bottom * 4;
        let mut row = String::with_capacity(width * 3); // braille chars are 3 bytes in UTF-8
        for cell in 0..width {
            let left_dots = dots_filled[cell * 2].saturating_sub(row_base).min(4);
            let right_dots = dots_filled[cell * 2 + 1].saturating_sub(row_base).min(4);

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

    // 14. Max-pooling correctness: a bucket's maximum, not its last or
    //     nearest sample, determines the rendered level.
    #[test]
    fn bucket_max_pooling_uses_bucket_maximum() {
        // 4 samples, width=1 -> 2 sub-columns, so each bucket holds 2 samples:
        // left sub-column owns data[0..2] = {0.0, 0.0} (max 0.0),
        // right sub-column owns data[2..4] = {5.0, 0.0} (max 5.0, not the
        // trailing 0.0 that nearest-neighbour / last-sample would pick).
        let data = [0.0, 0.0, 5.0, 0.0];
        let result = sparkline_braille(&data, 1, Some((0.0, 5.0)));
        assert_eq!(char_count(&result), 1);
        let ch = result.chars().next().expect("single char");
        let bits = ch as u32 - 0x2800;
        // Left sub-column: bottom dot only (level 0 -> 1 dot, bucket max 0.0).
        assert_eq!(bits & 0x40, 0x40, "left bottom dot should be set");
        assert_eq!(bits & 0x01, 0, "left top dot should be clear");
        // Right sub-column: fully filled (level 3 -> 4 dots, bucket max 5.0).
        assert_eq!(
            bits & (0x80 | 0x20 | 0x10 | 0x08),
            0x80 | 0x20 | 0x10 | 0x08,
            "right sub-column should be fully filled by the bucket maximum"
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
}
