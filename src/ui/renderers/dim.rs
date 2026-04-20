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

/// Rewrite `input` so that every SGR sequence (`\x1b[...m`) is replaced
/// with [`DARK_GREY_FG`]. The reset (`\x1b[0m`) sequences are preserved
/// so background/reset boundaries stay intact.
///
/// Anything outside of an SGR sequence is copied verbatim.
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
                // Preserve `\x1b[0m` (reset) so that paddings after
                // colored text keep their semantics. Everything else
                // becomes `\x1b[90m`.
                let body = &bytes[i + 2..j];
                if body == b"0" || body.is_empty() {
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
    fn background_color_also_replaced() {
        let input = "\x1b[42mgreen bg\x1b[0m";
        let out = dim_ansi(input);
        assert!(out.starts_with("\x1b[90m"));
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
}
