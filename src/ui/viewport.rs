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

//! Terminal geometry: how a reported size becomes a size we will render at.
//!
//! Every dimension the TUI uses enters through [`Viewport`], which separates
//! two degenerate cases that look identical in the raw numbers but mean
//! opposite things (issue #326).
//!
//! # Zero is "no geometry", not "a terminal of size zero"
//!
//! A pty allocated without a `TIOCSWINSZ` has no window size, so `TIOCGWINSZ`
//! reports zero rows and zero columns and `crossterm::terminal::size` returns
//! `Ok((0, 0))`. `script -q /dev/null all-smi local` produces exactly this, as
//! do some CI harnesses and terminal multiplexer edge cases. Zero there means
//! "nobody ever told the kernel how big this window is", and there is very
//! likely a real terminal of ordinary size on the far end of the pty. Refusing
//! to draw would be the wrong reading of the signal, so an absent dimension is
//! replaced by `$COLUMNS` / `$LINES` when the environment supplies one and by
//! the conventional 80x24 otherwise. This is the same fallback order ncurses
//! uses, and it is what makes the TUI drivable under `script` at all.
//!
//! # A tiny terminal is real and must be believed
//!
//! 12x2 is not missing geometry, it is an operator who dragged the window
//! that small. Substituting a size there would be a lie and would scribble
//! outside the visible area. Below [`MIN_COLS`] x [`MIN_ROWS`] the TUI renders
//! a single-line notice instead of a frame and recovers on the next resize.

use crossterm::terminal;

use crate::ui::text::{display_width, truncate_to_width};

/// Narrowest terminal the TUI will compose a frame for.
///
/// This is not a taste judgement, it is the width above every unchecked
/// width subtraction in the renderer set. The binding constraint is the
/// three-gauge GPU row in `renderers/gpu_renderer.rs`, which computes
/// `available_width - (num_gauges - 1) * 2` over `width.saturating_sub(10)`
/// and therefore needs `width >= 14`; the Apple Silicon CPU row needs 12, and
/// the chassis, storage and single-gauge CPU rows need 5. Twenty clears all of
/// them with room to spare and is also the point where the shortest status bar
/// text (`h:Help q:Exit`, 13 columns) stops being the whole line.
pub const MIN_COLS: u16 = 20;

/// Shortest terminal the TUI will compose a frame for: one header row, one
/// row of actual content, and the status bar that always occupies the last
/// row. Two rows would be pure chrome with nothing between it.
pub const MIN_ROWS: u16 = 3;

/// Column count assumed when the terminal reports none.
pub const FALLBACK_COLS: u16 = 80;

/// Row count assumed when the terminal reports none.
pub const FALLBACK_ROWS: u16 = 24;

/// A terminal geometry that has already been through [`Viewport::resolve`],
/// so neither dimension is zero.
///
/// The fields are public and there is deliberately no bare constructor:
/// building one literally is legal but visibly bypasses resolution, which is
/// what the tests below want and what production code never wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Viewport {
    pub cols: u16,
    pub rows: u16,
}

impl Viewport {
    /// Resolve a raw size as reported by the terminal, substituting a
    /// fallback for any dimension the terminal could not supply.
    ///
    /// Substitution is per-dimension: a terminal that knows its width but not
    /// its height keeps the width it reported.
    #[must_use]
    pub fn resolve(raw_cols: u16, raw_rows: u16) -> Self {
        Self {
            cols: resolve_dimension(raw_cols, "COLUMNS", FALLBACK_COLS),
            rows: resolve_dimension(raw_rows, "LINES", FALLBACK_ROWS),
        }
    }

    /// Read the current terminal geometry and resolve it.
    ///
    /// `terminal::size` failing outright is treated the same as it reporting
    /// zeros: geometry is unavailable, so fall back rather than abandon the
    /// frame. A monitoring TUI that quits because one `ioctl` blipped is
    /// worse than one that keeps drawing at an assumed size.
    #[must_use]
    pub fn current() -> Self {
        match terminal::size() {
            Ok((cols, rows)) => Self::resolve(cols, rows),
            Err(_) => Self::resolve(0, 0),
        }
    }

    /// Whether a normal frame is worth composing at this size.
    #[must_use]
    pub fn is_renderable(self) -> bool {
        self.cols >= MIN_COLS && self.rows >= MIN_ROWS
    }

    /// The single line shown in place of a frame when the terminal is below
    /// the minimum. Always fits within `cols`, down to and including zero.
    ///
    /// Deliberately plain text with no color escapes: it has to survive being
    /// squeezed into a handful of columns, and staying plain makes it
    /// greppable in a captured session transcript.
    #[must_use]
    pub fn too_small_notice(self) -> String {
        let Self { cols, rows } = self;
        let width = cols as usize;

        let long =
            format!("all-smi needs at least {MIN_COLS}x{MIN_ROWS}, this terminal is {cols}x{rows}");
        if display_width(&long) <= width {
            return long;
        }

        let short = format!("{cols}x{rows} < {MIN_COLS}x{MIN_ROWS}");
        if display_width(&short) <= width {
            return short;
        }

        truncate_to_width(&short, width).into_owned()
    }
}

/// Resolve one dimension: keep what the terminal reported when it reported
/// anything, otherwise consult the environment, otherwise use the fallback.
///
/// The environment lookup only happens on the zero path, so the common case
/// costs nothing.
fn resolve_dimension(raw: u16, env_key: &str, fallback: u16) -> u16 {
    if raw > 0 {
        return raw;
    }
    dimension_from_env(std::env::var(env_key).ok().as_deref(), fallback)
}

/// The environment half of [`resolve_dimension`], split out so it can be
/// tested without mutating the process environment (which would race against
/// every other test in the binary).
fn dimension_from_env(value: Option<&str>, fallback: u16) -> u16 {
    value
        .and_then(|value| value.trim().parse::<u16>().ok())
        .filter(|&parsed| parsed > 0)
        .unwrap_or(fallback)
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Zero means "geometry unavailable" and is replaced, not believed.
    // -----------------------------------------------------------------------

    #[test]
    fn zero_size_resolves_to_the_fallback_geometry() {
        // The `script -q /dev/null all-smi local` case: the pty never had a
        // window size set, so both dimensions come back as zero.
        let viewport = Viewport::resolve(0, 0);
        assert_eq!(viewport.cols, FALLBACK_COLS);
        assert_eq!(viewport.rows, FALLBACK_ROWS);
        assert!(
            viewport.is_renderable(),
            "a pty with no geometry must still render a normal frame"
        );
    }

    #[test]
    fn substitution_is_per_dimension() {
        // A terminal that reported one real dimension keeps it; only the
        // missing one is invented.
        assert_eq!(
            Viewport::resolve(0, 30),
            Viewport {
                cols: FALLBACK_COLS,
                rows: 30
            }
        );
        assert_eq!(
            Viewport::resolve(100, 0),
            Viewport {
                cols: 100,
                rows: FALLBACK_ROWS
            }
        );
    }

    #[test]
    fn a_real_size_passes_through_untouched() {
        assert_eq!(
            Viewport::resolve(100, 30),
            Viewport {
                cols: 100,
                rows: 30
            }
        );
        // Including a real size below the minimum: resolve must not "fix" it,
        // because believing the operator is the whole point of the split.
        assert_eq!(Viewport::resolve(12, 2), Viewport { cols: 12, rows: 2 });
    }

    #[test]
    fn environment_supplies_a_missing_dimension_when_it_is_usable() {
        // `$COLUMNS` / `$LINES` are the operator's way to drive the TUI at a
        // chosen size under a pty that has none, which is what makes an
        // automated session capture reproducible.
        assert_eq!(dimension_from_env(Some("120"), FALLBACK_COLS), 120);
        assert_eq!(dimension_from_env(Some("  40  "), FALLBACK_COLS), 40);
    }

    #[test]
    fn unusable_environment_values_fall_through_to_the_fallback() {
        for value in [
            None,
            Some(""),
            Some("0"),
            Some("abc"),
            Some("-5"),
            Some("99999"),
        ] {
            assert_eq!(
                dimension_from_env(value, FALLBACK_COLS),
                FALLBACK_COLS,
                "{value:?} must not be trusted as a dimension"
            );
        }
    }

    // -----------------------------------------------------------------------
    // The renderable floor.
    // -----------------------------------------------------------------------

    #[test]
    fn renderable_floor_is_inclusive_and_rejects_just_below() {
        assert!(
            Viewport {
                cols: MIN_COLS,
                rows: MIN_ROWS
            }
            .is_renderable()
        );
        assert!(
            !Viewport {
                cols: MIN_COLS - 1,
                rows: MIN_ROWS
            }
            .is_renderable()
        );
        assert!(
            !Viewport {
                cols: MIN_COLS,
                rows: MIN_ROWS - 1
            }
            .is_renderable()
        );
    }

    #[test]
    fn degenerate_and_tiny_sizes_are_not_renderable() {
        // These never reach the renderers in production, because ui_loop
        // gates on `is_renderable`. The test pins that contract.
        for (cols, rows) in [
            (0, 0),
            (1, 1),
            (0, 30),
            (200, 0),
            (200, 1),
            (20, 2),
            (19, 3),
        ] {
            assert!(
                !Viewport { cols, rows }.is_renderable(),
                "{cols}x{rows} must not be treated as renderable"
            );
        }
    }

    // -----------------------------------------------------------------------
    // The notice always fits the width it is given.
    // -----------------------------------------------------------------------

    #[test]
    fn notice_never_exceeds_the_available_width() {
        // Bounded sweep over every width the notice could face, including
        // zero and one.
        for cols in 0u16..=90 {
            let notice = Viewport { cols, rows: 2 }.too_small_notice();
            assert!(
                display_width(&notice) <= cols as usize,
                "notice {notice:?} overflows {cols} columns"
            );
        }
    }

    #[test]
    fn notice_states_both_the_requirement_and_the_actual_size() {
        let notice = Viewport { cols: 80, rows: 2 }.too_small_notice();
        assert!(notice.contains("20x3"), "notice must state the minimum");
        assert!(notice.contains("80x2"), "notice must state what it got");
    }

    #[test]
    fn notice_degrades_to_a_short_form_when_the_long_one_will_not_fit() {
        // 24 columns cannot hold the sentence but can hold "24x2 < 20x3".
        let notice = Viewport { cols: 24, rows: 2 }.too_small_notice();
        assert_eq!(notice, "24x2 < 20x3");
    }

    #[test]
    fn notice_is_plain_text_with_no_escape_sequences() {
        let notice = Viewport { cols: 80, rows: 2 }.too_small_notice();
        assert!(
            !notice.contains('\u{1b}'),
            "notice must stay greppable in a captured transcript"
        );
    }
}
