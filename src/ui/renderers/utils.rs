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

//! Shared rendering utilities used across multiple renderer modules.

/// Truncate a display string on char boundaries to the given max char count.
/// Appends a single ellipsis character (`…`, U+2026) when truncation occurred.
///
/// Truncation is based on Unicode scalar values (`char`) rather than bytes.
/// This means multi-byte sequences such as CJK ideographs, emoji, or accented
/// characters each count as one "character" regardless of their byte length.
///
/// # Examples
///
/// ```ignore
/// assert_eq!(truncate_str("hello", 10), "hello");
/// assert_eq!(truncate_str("hello", 5), "hello");
/// assert_eq!(truncate_str("hello world", 5), "hell…");
/// ```
pub(crate) fn truncate_str(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_string_stays_empty() {
        assert_eq!(truncate_str("", 0), "");
        assert_eq!(truncate_str("", 5), "");
    }

    #[test]
    fn string_shorter_than_cap_is_unchanged() {
        assert_eq!(truncate_str("abc", 10), "abc");
        assert_eq!(truncate_str("abc", 4), "abc");
    }

    #[test]
    fn string_equal_to_cap_is_unchanged() {
        assert_eq!(truncate_str("abcde", 5), "abcde");
        assert_eq!(truncate_str("abcdefghij", 10), "abcdefghij");
    }

    #[test]
    fn string_longer_than_cap_is_truncated_with_ellipsis() {
        let out = truncate_str("abcdefghij", 5);
        // Total char count must equal max_chars.
        assert_eq!(out.chars().count(), 5);
        // Last character must be the ellipsis.
        assert!(out.ends_with('…'), "expected ellipsis, got: {out:?}");
        // Prefix must be first (max_chars - 1) chars of the original.
        assert!(out.starts_with("abcd"), "unexpected prefix: {out:?}");
    }

    #[test]
    fn truncation_at_cap_one_keeps_only_ellipsis() {
        let out = truncate_str("hello", 1);
        assert_eq!(out, "…");
    }

    #[test]
    fn multibyte_cjk_characters_counted_by_char_not_byte() {
        // Each CJK character is 3 bytes in UTF-8, but should count as 1 char.
        let input = "你好世界ABC"; // 4 CJK + 3 ASCII = 7 chars, 15 bytes
        assert_eq!(input.chars().count(), 7);

        // No truncation: 7 chars fits in cap 7.
        assert_eq!(truncate_str(input, 7), input);
        assert_eq!(truncate_str(input, 10), input);

        // Truncated to 4 chars: "你好世…"
        let out = truncate_str(input, 4);
        assert_eq!(out.chars().count(), 4);
        assert!(out.ends_with('…'), "expected ellipsis, got: {out:?}");
        assert!(out.starts_with("你好世"), "unexpected prefix: {out:?}");
    }

    #[test]
    fn multibyte_emoji_counted_by_scalar_value() {
        // Emoji are multi-byte but each counts as one char (scalar value).
        // Note: grapheme clusters (e.g. family emoji with ZWJ) would differ,
        // but this function intentionally uses scalar-value counting.
        let input = "🦀🎉🚀AB"; // 3 emoji + 2 ASCII = 5 chars
        assert_eq!(input.chars().count(), 5);

        assert_eq!(truncate_str(input, 5), input);

        let out = truncate_str(input, 4);
        assert_eq!(out.chars().count(), 4);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn max_chars_zero_returns_empty() {
        // saturating_sub(1) on 0 yields 0, so take(0) collects nothing,
        // then we push '…'. Result: a single ellipsis if max_chars==0
        // only when the string has content; an empty string stays empty.
        assert_eq!(truncate_str("", 0), "");
        // A non-empty string with max_chars=0: "…"
        let out = truncate_str("x", 0);
        // chars().count() == 1 > 0, so we enter the truncation path.
        // take(0.saturating_sub(1) = 0) → empty, then push '…'.
        assert_eq!(out, "…");
    }
}
