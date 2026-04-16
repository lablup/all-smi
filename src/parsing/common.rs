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

// Common parsing utilities for number extraction, unit conversion, and string sanitization.

use std::str::FromStr;

/// Parse a number from a string after sanitizing by removing commas, underscores, and trimming.
/// Returns None if parsing fails.
#[allow(dead_code)]
pub fn parse_number<T: FromStr>(s: &str) -> Option<T> {
    let cleaned = s.trim().replace([',', '_'], "");
    cleaned.parse::<T>().ok()
}

/// Convert a floating-point quantity with a unit into bytes.
/// Supported units (case-insensitive): B, KB, KiB, MB, MiB, GB, GiB, TB, TiB
#[allow(dead_code)]
pub fn to_bytes(value: f64, unit: &str) -> Option<u64> {
    let mul = match unit.trim().to_ascii_uppercase().as_str() {
        "B" => 1.0,
        "KB" => 1_000.0,
        "KIB" => 1024.0,
        "MB" => 1_000_000.0,
        "MIB" => 1024.0_f64.powi(2),
        "GB" => 1_000_000_000.0,
        "GIB" => 1024.0_f64.powi(3),
        "TB" => 1_000_000_000_000.0,
        "TIB" => 1024.0_f64.powi(4),
        _ => return None,
    };
    let bytes = value * mul;
    if bytes.is_finite() && bytes >= 0.0 {
        Some(bytes as u64)
    } else {
        None
    }
}

/// Strip control characters (including ANSI escape sequences like `\x1b[2J`)
/// from a string. Prevents TUI escape injection when label values from remote
/// metrics are rendered in terminal output.
pub fn strip_control_chars(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).collect()
}

/// Sanitize a quoted label/value by trimming whitespace, removing surrounding
/// double quotes, and stripping control characters to defend against ANSI
/// escape injection from compromised remote endpoints.
pub fn sanitize_label_value(s: &str) -> String {
    const MAX_LABEL_VALUE_LENGTH: usize = 1024;

    let trimmed = s.trim();
    let cleaned = trimmed.trim_matches('"');

    // Strip control characters before length check so we don't count
    // characters that will be removed anyway.
    let stripped = strip_control_chars(cleaned);

    // Truncate excessively long values to prevent memory exhaustion.
    // Walk back to a char boundary so UTF-8 input whose byte
    // `[MAX_LABEL_VALUE_LENGTH]` lands in the middle of a multi-byte codepoint
    // does not panic via the slice operation.
    if stripped.len() > MAX_LABEL_VALUE_LENGTH {
        let mut end = MAX_LABEL_VALUE_LENGTH;
        while !stripped.is_char_boundary(end) {
            end -= 1;
        }
        stripped[..end].to_string()
    } else {
        stripped
    }
}

/// Sanitize a label name for Prometheus compatibility.
/// Converts spaces to underscores and makes lowercase.
/// Prometheus label names must match: [a-zA-Z_][a-zA-Z0-9_]*
pub fn sanitize_label_name(s: &str) -> String {
    s.replace([' ', '-'], "_").to_lowercase()
}

/// Extract the substring that appears after the first ':' character, trimmed.
/// Returns None if ':' is not present.
#[allow(dead_code)]
pub fn after_colon_trimmed(line: &str) -> Option<&str> {
    line.split_once(':').map(|x| x.1).map(|s| s.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_number_util() {
        assert_eq!(parse_number::<u32>("1_234"), Some(1234));
        assert_eq!(parse_number::<u64>("1,234,567"), Some(1_234_567));
        assert_eq!(parse_number::<f64>("  3.1234 "), Some(3.1234));
        assert_eq!(parse_number::<i32>("abc"), None);
    }

    #[test]
    fn test_to_bytes() {
        assert_eq!(to_bytes(1.0, "B"), Some(1));
        assert_eq!(to_bytes(1.0, "KB"), Some(1_000));
        assert_eq!(to_bytes(1.0, "KiB"), Some(1024));
        assert_eq!(to_bytes(1.5, "MiB"), Some((1.5 * 1024.0 * 1024.0) as u64));
        assert_eq!(to_bytes(2.0, "GB"), Some(2_000_000_000));
        assert_eq!(to_bytes(1.0, "unknown"), None);
    }

    #[test]
    fn test_sanitize_label_value() {
        assert_eq!(sanitize_label_value(r#" "hello" "#), "hello".to_string());
        assert_eq!(sanitize_label_value("world"), "world".to_string());
    }

    #[test]
    fn test_strip_control_chars() {
        assert_eq!(strip_control_chars("hello"), "hello");
        assert_eq!(strip_control_chars(""), "");
        assert_eq!(strip_control_chars("\x1b[2J"), "");
        assert_eq!(strip_control_chars("abc\x1b[2Jdef"), "abc[2Jdef");
        assert_eq!(strip_control_chars("\x00\x07\x0b"), "");
    }

    #[test]
    fn test_sanitize_label_value_strips_ansi_escape() {
        // A clear-screen ANSI escape sequence must be stripped entirely.
        assert_eq!(sanitize_label_value("\x1b[2Jmalicious"), "[2Jmalicious");
        // Control chars within a quoted value are also stripped.
        assert_eq!(
            sanitize_label_value("\"\x1b[2JEvil GPU\""),
            "[2JEvil GPU"
        );
    }

    #[test]
    fn test_sanitize_label_value_utf8_boundary_truncation() {
        // Construct an input where byte index 1024 falls inside a multi-byte
        // codepoint. We prepend 1023 ASCII bytes, then a 3-byte UTF-8 char.
        // Without the char-boundary walk this call would panic.
        const MAX_LABEL_VALUE_LENGTH: usize = 1024;
        let mut s = String::with_capacity(MAX_LABEL_VALUE_LENGTH + 8);
        s.push_str(&"a".repeat(MAX_LABEL_VALUE_LENGTH - 1));
        s.push('가'); // 3 bytes in UTF-8
        let out = sanitize_label_value(&s);
        assert!(out.len() <= MAX_LABEL_VALUE_LENGTH);
        // The multi-byte char must be dropped entirely, not sliced.
        assert!(!out.contains('가'));
        assert_eq!(out.len(), MAX_LABEL_VALUE_LENGTH - 1);
    }

    #[test]
    fn test_sanitize_label_name() {
        assert_eq!(sanitize_label_name("Driver Version"), "driver_version");
        assert_eq!(sanitize_label_name("GPU Type"), "gpu_type");
        assert_eq!(sanitize_label_name("Thermal Pressure"), "thermal_pressure");
        assert_eq!(sanitize_label_name("Architecture"), "architecture");
        assert_eq!(sanitize_label_name("pcie-gen-current"), "pcie_gen_current");
        assert_eq!(sanitize_label_name("UPPER_CASE"), "upper_case");
        assert_eq!(sanitize_label_name("already_valid"), "already_valid");
    }

    #[test]
    fn test_after_colon_trimmed() {
        assert_eq!(after_colon_trimmed("Key: Value"), Some("Value"));
        assert_eq!(after_colon_trimmed("NoColon"), None);
    }
}
