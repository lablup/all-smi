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

/// Row bit masks for the left sub-column, ordered bottom→top.
/// level=0 fills only the bottom row; level=3 fills all four rows.
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
/// # Arguments
/// - `data`: time-series samples, most-recent sample last.
/// - `width`: desired output width in terminal columns (each cell = 2 sub-columns).
/// - `range`: optional fixed `(min, max)`. When `None`, the range is derived
///   from the data automatically.
///
/// # Behaviour
/// - Empty `data` → returns `" ".repeat(width)` (ASCII spaces, preserves layout).
/// - `width == 0` → returns an empty string.
/// - Constant input with auto-range → renders the bottom-most row filled across
///   all columns (`⣀` U+28C0, i.e. both bottom dots set), so callers can still
///   see that data is present.
/// - NaN / non-finite values are clamped to the minimum of the range.
/// - Degenerate explicit range `(lo, hi)` where `hi <= lo` → treated as constant;
///   all cells rendered at the bottom row.
#[must_use]
pub fn sparkline_braille(data: &[f64], width: usize, range: Option<(f64, f64)>) -> String {
    if data.is_empty() {
        return " ".repeat(width);
    }
    if width == 0 {
        return String::new();
    }

    // Determine effective min/max.
    let (min, max) = match range {
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

    // Resample data into n_sub sub-columns using nearest-neighbour interpolation.
    // Maps sub-column index i → data index j.
    let resample = |i: usize| -> f64 {
        let j = if data.len() == 1 {
            0
        } else {
            // Map [0, n_sub-1] → [0, data.len()-1] linearly (nearest neighbour).
            let j_frac = i as f64 * (data.len() - 1) as f64 / (n_sub - 1).max(1) as f64;
            j_frac.round() as usize
        };
        let v = data[j.min(data.len() - 1)];
        if v.is_finite() { v } else { min }
    };

    // Compute vertical level (0=bottom, 3=top) for a value.
    // When max == min (constant / degenerate range) always returns 0 (bottom row).
    let level_of = |v: f64| -> usize {
        if max <= min {
            return 0;
        }
        let clamped = v.clamp(min, max);
        let norm = (clamped - min) / (max - min);
        // norm ∈ [0.0, 1.0]; multiply by 4 and floor, clamped to [0, 3].
        ((norm * 4.0).floor() as usize).min(3)
    };

    // Build output string: one braille character per pair of sub-columns.
    let mut out = String::with_capacity(width * 3); // braille chars are 3 bytes in UTF-8
    for cell in 0..width {
        let left_val = resample(cell * 2);
        let right_val = resample(cell * 2 + 1);

        let left_level = level_of(left_val);
        let right_level = level_of(right_val);

        // Bar-fill: fill all rows from bottom up to the computed level.
        let mut bits: u32 = 0;
        for &b in LEFT_BITS.iter().take(left_level + 1) {
            bits |= b;
        }
        for &b in RIGHT_BITS.iter().take(right_level + 1) {
            bits |= b;
        }

        let ch = char::from_u32(0x2800 + bits).unwrap_or('⠀');
        out.push(ch);
    }
    out
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
}
