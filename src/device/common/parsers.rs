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
        .or_else(|| cleaned.parse::<f64>().ok().map(|f| f as u32))
}

/// Parse a power string (e.g., "150W" or "150.5W") into f64
/// Returns None if parsing fails
pub fn parse_power(power_str: &str) -> Option<f64> {
    power_str
        .trim_end_matches(['W', ' '].as_ref())
        .split('/')
        .next()?
        .trim()
        .parse::<f64>()
        .ok()
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
        assert_eq!(parse_temperature("37.99°C"), Some(37));
        assert_eq!(parse_temperature("invalid"), None);
    }

    #[test]
    fn test_parse_power() {
        assert_eq!(parse_power("150W"), Some(150.0));
        assert_eq!(parse_power("150.5W"), Some(150.5));
        assert_eq!(parse_power("150/250W"), Some(150.0));
        assert_eq!(parse_power("invalid"), None);
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
