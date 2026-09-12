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

// Common parsing utilities with proper error handling

/// Parse a temperature string (e.g., "45C", "45°C", or "41.33°C") into u32
/// Handles both integer and decimal temperature values
/// Returns None if parsing fails
pub fn parse_temperature(temp_str: &str) -> Option<u32> {
    let cleaned = temp_str
        .trim_end_matches(['C', '°', ' '].as_ref())
        .split('/')
        .next()?
        .trim();

    // Try integer first, then float (for decimal temperatures like "41.33")
    cleaned
        .parse::<u32>()
        .ok()
        .or_else(|| cleaned.parse::<f64>().ok().map(|f| f.round() as u32))
}

/// Parse a power string (e.g., "150W" or "150.5W") into watts
///
/// Also accepts the SI-prefixed forms some vendor tools emit. Rebellions
/// `rbln-stat` reports `card_power` in microwatts ("17521800uW" = 17.52 W),
/// which used to fail to parse and silently read as 0 W. Recognised prefixes
/// are `u`/`µ`/`μ` (micro), `m` (milli) and `k`/`K` (kilo); a value with no
/// prefix is watts, exactly as before.
///
/// Returns None if parsing fails
pub fn parse_power(power_str: &str) -> Option<f64> {
    // Take the current value out of a "current/limit" pair before stripping
    // the unit, so "150W/250W" works as well as "150/250W".
    let value = power_str
        .split('/')
        .next()?
        .trim()
        .trim_end_matches(['W', ' '].as_ref())
        .trim();

    // An SI prefix can only sit immediately before the `W`. No f64 literal
    // ends in one of these characters, so recognising them cannot change the
    // result for any input that already parsed successfully.
    const SI_PREFIXES: &[(&str, f64)] = &[
        ("u", 1e-6),
        ("µ", 1e-6), // U+00B5 MICRO SIGN
        ("μ", 1e-6), // U+03BC GREEK SMALL LETTER MU
        ("m", 1e-3),
        ("k", 1e3),
        ("K", 1e3),
    ];
    for &(prefix, multiplier) in SI_PREFIXES {
        if let Some(number) = value.strip_suffix(prefix) {
            return number.trim().parse::<f64>().ok().map(|v| v * multiplier);
        }
    }

    value.parse::<f64>().ok()
}

/// Parse a utilization percentage string (e.g., "85%" or "85.5%") into f64
/// Returns None if parsing fails
pub fn parse_utilization(util_str: &str) -> Option<f64> {
    util_str
        .trim_end_matches(['%', ' '].as_ref())
        .trim()
        .parse::<f64>()
        .ok()
}

/// Parse a memory value string (e.g., "1024MB" or "1024MiB") into bytes
/// Returns None if parsing fails
pub fn parse_memory_mb_to_bytes(mem_str: &str) -> Option<u64> {
    let cleaned = mem_str
        .trim()
        .trim_end_matches("MB")
        .trim_end_matches("MiB")
        .trim();

    cleaned.parse::<u64>().ok().map(|mb| mb * 1024 * 1024)
}

/// Parse a frequency string (e.g., "1000MHz") into u32
/// Returns None if parsing fails
pub fn parse_frequency_mhz(freq_str: &str) -> Option<u32> {
    freq_str.trim_end_matches("MHz").trim().parse::<u32>().ok()
}

/// Parse a string with a default value if parsing fails
/// Logs the parse error for debugging
#[allow(dead_code)]
pub fn parse_with_default<T, E>(value_str: &str, default: T, context: &str) -> T
where
    T: std::str::FromStr<Err = E>,
    E: std::fmt::Display,
{
    match value_str.parse::<T>() {
        Ok(val) => val,
        Err(e) => {
            eprintln!("Parse error in {context}: {e} (input: '{value_str}')");
            default
        }
    }
}

/// Parse a device ID from a string like "npu0" or "gpu1"
/// Returns None if parsing fails
pub fn parse_device_id(device_str: &str) -> Option<usize> {
    device_str
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .collect::<String>()
        .parse::<usize>()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_temperature() {
        assert_eq!(parse_temperature("45C"), Some(45));
        assert_eq!(parse_temperature("45°C"), Some(45));
        assert_eq!(parse_temperature("45/90C"), Some(45));
        assert_eq!(parse_temperature("41.33°C"), Some(41));
        assert_eq!(parse_temperature("37.99°C"), Some(38));
        assert_eq!(parse_temperature("invalid"), None);
    }

    /// f64 comparison for values that are not exactly representable after the
    /// SI-prefix multiply (17521800 * 1e-6 is not bit-identical to 17.5218).
    fn assert_close(actual: Option<f64>, expected: f64) {
        let actual = actual.expect("value should parse");
        assert!(
            (actual - expected).abs() < 1e-9,
            "expected ~{expected}, got {actual}"
        );
    }

    #[test]
    fn test_parse_power() {
        assert_eq!(parse_power("150W"), Some(150.0));
        assert_eq!(parse_power("150.5W"), Some(150.5));
        assert_eq!(parse_power("150 W"), Some(150.0));
        assert_eq!(parse_power("150"), Some(150.0));
        assert_eq!(parse_power("150/250W"), Some(150.0));
        assert_eq!(parse_power("invalid"), None);
        assert_eq!(parse_power(""), None);
        assert_eq!(parse_power("W"), None);
    }

    /// Regression: Rebellions `rbln-stat` reports `card_power` in microwatts.
    /// `trim_end_matches(['W', ' '])` left the `u` behind, the parse failed and
    /// every Rebellions NPU reported 0.0 W. Value captured from a live
    /// RBLN-CA22 (ATOM Plus) card idling at ~17.5 W.
    #[test]
    fn test_parse_power_si_prefixes() {
        assert_close(parse_power("17521800uW"), 17.5218);
        assert_close(parse_power("17521800µW"), 17.5218); // U+00B5
        assert_close(parse_power("17521800μW"), 17.5218); // U+03BC
        assert_close(parse_power("1500mW"), 1.5);
        assert_close(parse_power("1.5kW"), 1500.0);
        assert_close(parse_power("1.5KW"), 1500.0);
        assert_close(parse_power("17521800 uW"), 17.5218);

        // A prefix with no number is still a parse failure, not 0.
        assert_eq!(parse_power("uW"), None);
        assert_eq!(parse_power("mW"), None);
    }

    /// A "current/limit" pair where both halves carry the unit used to yield
    /// None ("150W" was left after the split); it now reads the current value.
    #[test]
    fn test_parse_power_unit_on_both_halves() {
        assert_eq!(parse_power("150W/250W"), Some(150.0));
    }

    #[test]
    fn test_parse_utilization() {
        assert_eq!(parse_utilization("85%"), Some(85.0));
        assert_eq!(parse_utilization("85.5%"), Some(85.5));
        assert_eq!(parse_utilization("invalid"), None);
    }

    #[test]
    fn test_parse_memory_mb_to_bytes() {
        assert_eq!(parse_memory_mb_to_bytes("1024MB"), Some(1073741824));
        assert_eq!(parse_memory_mb_to_bytes("1024MiB"), Some(1073741824));
        assert_eq!(parse_memory_mb_to_bytes("invalid"), None);
    }

    #[test]
    fn test_parse_device_id() {
        assert_eq!(parse_device_id("npu0"), Some(0));
        assert_eq!(parse_device_id("gpu1"), Some(1));
        assert_eq!(parse_device_id("device123"), Some(123));
        assert_eq!(parse_device_id("invalid"), None);
    }
}
