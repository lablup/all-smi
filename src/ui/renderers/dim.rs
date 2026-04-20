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

//! Post-processing helper that rewrites ANSI Select-Graphic-Rendition
//! sequences in a buffer so that every emitted foreground colour is
//! replaced by `DarkGrey`.
//!
//! Used by the frame renderer to render rows that do not match the active
//! filter in a visually muted state without having to pass a `dim: bool`
//! parameter through every renderer function.

/// ANSI SGR sequence that selects `DarkGrey` (bright-black) as the
/// foreground color. This is byte-for-byte equivalent to
/// `crossterm::style::SetForegroundColor(Color::DarkGrey)`.
#[allow(dead_code)] // Used by the `dim_ansi` binary-side path which is gated by `view/`.
const DARK_GREY_FG: &[u8] = b"\x1b[90m";

/// Rewrite `input` so that every SGR foreground color is replaced with
/// [`DARK_GREY_FG`] while background colors and the reset (`\x1b[0m`) are
/// preserved. Non-SGR CSI sequences (cursor movement, erase line, etc.)
/// are copied verbatim.
///
/// The classification is conservative: any SGR body that contains a
/// background-color parameter (`40`–`47`, `100`–`107`, or the 256/RGB
/// forms prefixed with `48;`) is passed through unchanged. Anything else
/// — foreground colors, intensity modifiers, reset — is collapsed into a
/// single `\x1b[90m` so the rendered output visibly loses saturation.
#[allow(dead_code)] // Called from `view/frame_renderer.rs` which is binary-side only.
pub fn dim_ansi(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
            // Find the terminating ASCII letter (the final byte in a CSI
            // sequence per ECMA-48). We only care about `m` (SGR) here;
            // non-SGR sequences are preserved so cursor moves still work.
            let start = i;
            let mut j = i + 2;
            while j < bytes.len() && !bytes[j].is_ascii_alphabetic() {
                j += 1;
            }
            if j >= bytes.len() {
                // Unterminated escape — emit verbatim and stop scanning.
                out.extend_from_slice(&bytes[start..]);
                return String::from_utf8(out).unwrap_or_else(|_| input.to_string());
            }
            let final_byte = bytes[j];
            if final_byte == b'm' {
                // Preserve `\x1b[0m` (reset) and any SGR carrying a
                // background color so we don't drop the highlight that
                // a renderer deliberately painted (e.g. selected row).
                let body = &bytes[i + 2..j];
                if body == b"0" || body.is_empty() || sgr_has_background(body) {
                    out.extend_from_slice(&bytes[start..=j]);
                } else {
                    out.extend_from_slice(DARK_GREY_FG);
                }
                i = j + 1;
            } else {
                // Cursor moves etc. — copy verbatim.
                out.extend_from_slice(&bytes[start..=j]);
                i = j + 1;
            }
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| input.to_string())
}

/// Classify an SGR body as containing a background-color parameter.
///
/// Covers:
/// - Standard backgrounds: `40`..=`47`
/// - Bright backgrounds: `100`..=`107`
/// - 256-color background: `48;5;<n>`
/// - Truecolor background: `48;2;<r>;<g>;<b>`
///
/// Combined fg+bg sequences like `\x1b[1;37;44m` are also matched via the
/// split-by-`;` loop because any single parameter in the bg range is
/// enough to preserve the whole sequence.
fn sgr_has_background(body: &[u8]) -> bool {
    let Ok(s) = std::str::from_utf8(body) else {
        return false;
    };
    for p in s.split(';') {
        if p == "48" {
            return true;
        }
        if let Ok(n) = p.parse::<u16>()
            && ((40..=47).contains(&n) || (100..=107).contains(&n))
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_passes_through() {
        assert_eq!(dim_ansi("hello"), "hello");
    }

    #[test]
    fn sgr_replaced_with_dark_grey() {
        let input = "\x1b[31mRED\x1b[0m tail";
        let out = dim_ansi(input);
        assert!(out.starts_with("\x1b[90mRED\x1b[0m"));
        assert!(out.ends_with(" tail"));
    }

    #[test]
    fn multiple_sgr_all_replaced() {
        let input = "\x1b[31mA\x1b[32mB\x1b[34mC\x1b[0m";
        let out = dim_ansi(input);
        let expected = "\x1b[90mA\x1b[90mB\x1b[90mC\x1b[0m";
        assert_eq!(out, expected);
    }

    #[test]
    fn non_sgr_sequence_preserved() {
        // Cursor move: should not be changed.
        let input = "\x1b[2;3Htext";
        let out = dim_ansi(input);
        assert_eq!(out, input);
    }

    #[test]
    fn background_color_is_preserved() {
        // Background colors are applied intentionally by renderers
        // (highlighted row, alert flash). Dropping them on dim would
        // cancel out the renderer's signal, so we preserve the SGR
        // verbatim when a bg parameter is present.
        let input = "\x1b[42mgreen bg\x1b[0m";
        let out = dim_ansi(input);
        assert_eq!(out, input);
    }

    #[test]
    fn combined_fg_and_bg_preserves_the_sgr() {
        // Combined fg+bg (`\x1b[31;42m`) must also be preserved so the
        // background survives; otherwise we'd silently strip the
        // intentional highlight.
        let input = "\x1b[31;42mcolored\x1b[0m";
        let out = dim_ansi(input);
        assert_eq!(out, input);
    }

    #[test]
    fn extended_bg_256_color_is_preserved() {
        // `\x1b[48;5;226m` selects a 256-color bg (bright yellow).
        let input = "\x1b[48;5;226mhighlight\x1b[0m";
        let out = dim_ansi(input);
        assert_eq!(out, input);
    }

    #[test]
    fn bright_bg_is_preserved() {
        // 100..=107 are the bright-background range.
        let input = "\x1b[105mbright pink bg\x1b[0m";
        let out = dim_ansi(input);
        assert_eq!(out, input);
    }

    #[test]
    fn reset_preserved() {
        let input = "\x1b[0m";
        let out = dim_ansi(input);
        assert_eq!(out, input);
    }

    #[test]
    fn empty_input() {
        assert_eq!(dim_ansi(""), "");
    }

    #[test]
    fn unterminated_escape_copied_verbatim() {
        let input = "a\x1b[31";
        let out = dim_ansi(input);
        assert_eq!(out, input);
    }

    #[test]
    fn truecolor_bg_is_preserved() {
        // `\x1b[48;2;255;128;0m` is an RGB background (orange).
        // The `48` parameter signals background; the full sequence must be
        // preserved so an alert-flash highlight is not silently discarded.
        let input = "\x1b[48;2;255;128;0mhighlight\x1b[0m";
        let out = dim_ansi(input);
        assert_eq!(out, input);
    }

    #[test]
    fn fg_only_before_truecolor_bg_dims_correctly() {
        // A foreground followed by a truecolor background: the fg should be
        // replaced, the bg should be kept intact.
        let fg = "\x1b[33m"; // yellow fg
        let bg = "\x1b[48;2;0;0;128m"; // dark-blue RGB bg
        let input = format!("{fg}{bg}text\x1b[0m");
        let out = dim_ansi(&input);
        // fg becomes dark-grey; bg is verbatim; reset is verbatim.
        assert!(out.contains("\x1b[90m"), "fg not dimmed");
        assert!(out.contains(bg), "truecolor bg was stripped");
        assert!(out.contains("\x1b[0m"), "reset was stripped");
    }
}
