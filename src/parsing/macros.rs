//! Parsing macros for repeated text parsing patterns.

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

/// Parse a numeric metric value from a "Key: Value <SUFFIX>" line.
/// - Extracts substring after the first ':'.
/// - Takes the first whitespace-separated token.
/// - Strips the provided suffix (e.g., "MHz", "mW") if present.
/// - Additionally strips a trailing '%' if present (common in residency values).
/// - Parses the remainder into the requested numeric type.
///
/// Returns Option<T> (None if parsing fails).
///
/// # Safety
/// This macro does not panic. Returns None for invalid input.
#[macro_export]
macro_rules! parse_metric {
    ($line:expr_2021, $suffix:expr_2021, $ty:ty) => {{
        let opt = $crate::parsing::common::after_colon_trimmed($line)
            .and_then(|rest| rest.split_whitespace().next())
            .map(|tok| {
                let no_suffix = tok.trim_end_matches($suffix);
                // Common pattern: percentages like "64.29%"
                no_suffix.trim_end_matches('%').to_string()
            })
            .and_then(|num| $crate::parsing::common::parse_number::<$ty>(&num));
        opt
    }};
}

/// Parse a Prometheus-formatted metric line using a regex with 3 capture groups:
/// 1) metric name without the `all_smi_` prefix
/// 2) labels content inside braces `{}`
/// 3) numeric value
///
/// Example regex: r"^all_smi_([^\{]+)\{([^}]+)\} ([\d\.]+)$"
/// Returns Option<(String, String, f64)>
///
/// # Safety
/// This macro does not panic. Returns None for invalid input or regex mismatches.
#[macro_export]
macro_rules! parse_prometheus {
    ($line:expr_2021, $re:expr_2021) => {{
        if let Some(cap) = $re.captures($line.trim()) {
            let name = cap.get(1).map(|m| m.as_str().to_string());
            let labels = cap.get(2).map(|m| m.as_str().to_string());
            let value = cap
                .get(3)
                .and_then(|m| m.as_str().parse::<f64>().ok())
                .unwrap_or(0.0);
            if let (Some(name), Some(labels)) = (name, labels) {
                // Add length validation that was previously enforced by bounded quantifiers
                if name.len() > 256 || labels.len() > 1024 {
                    None
                } else {
                    Some((name, labels, value))
                }
            } else {
                None
            }
        } else {
            None
        }
    }};
}

/// Extract a label value from a HashMap and insert it into a detail HashMap with a given key.
/// Useful for processing Prometheus label data.
///
/// Example usage:
/// ```ignore
/// extract_label_to_detail!(labels, "cuda_version", gpu_info.detail, "cuda_version");
/// ```
///
/// # Safety
/// This macro does not panic. Silently skips if the label is not found.
#[macro_export]
macro_rules! extract_label_to_detail {
    ($labels:expr_2021, $label_key:expr_2021, $detail_map:expr_2021, $detail_key:expr_2021) => {
        if let Some(value) = $labels.get($label_key) {
            $detail_map.insert($detail_key.to_string(), value.clone());
        }
    };
    // Variant that uses the same key for both label and detail
    ($labels:expr_2021, $key:expr_2021, $detail_map:expr_2021) => {
        extract_label_to_detail!($labels, $key, $detail_map, $key);
    };
}

/// Process multiple label extractions in a batch.
/// Takes a list of label keys and inserts them into the detail map.
/// Optimized to perform single HashMap lookup per key.
///
/// Example usage:
/// ```ignore
/// extract_labels_batch!(
///     labels, gpu_info.detail,
///     ["cuda_version", "driver_version", "architecture", "compute_capability"]
/// );
/// ```
#[macro_export]
macro_rules! extract_labels_batch {
    ($labels:expr_2021, $detail_map:expr_2021, [$($key:expr_2021),* $(,)?]) => {
        $(
            if let Some(value) = $labels.get($key) {
                $detail_map.insert($key.to_string(), value.clone());
            }
        )*
    };
}

/// Update a struct field based on a metric name match.
/// Reduces repetitive match arms to single macro calls.
/// Uses saturating casts to prevent overflow/underflow.
///
/// Example usage:
/// ```ignore
/// update_metric_field!(metric_name, value, gpu_info, {
///     "gpu_utilization" => utilization as f64,
///     "gpu_memory_used_bytes" => used_memory as u64,
///     "gpu_temperature_celsius" => temperature as u32
/// });
/// ```
#[macro_export]
macro_rules! update_metric_field {
    ($metric_name:expr_2021, $value:expr_2021, $target:expr_2021, {
        $($name:expr_2021 => $field:ident as $type:ty),* $(,)?
    }) => {
        match $metric_name {
            $(
                $name => {
                    // Use saturating conversions for integer types to prevent overflow
                    #[allow(unused_comparisons)]
                    let safe_value = if $value < 0.0 {
                        0 as $type
                    } else if $value > (<$type>::MAX as f64) {
                        <$type>::MAX
                    } else {
                        $value as $type
                    };
                    $target.$field = safe_value;
                },
            )*
            _ => {}
        }
    };
}

/// Extract a label value from a HashMap with a default if not present.
/// Returns the value or a default. Uses efficient borrowing when possible.
///
/// Example usage:
/// ```ignore
/// let gpu_name = get_label_or_default!(labels, "gpu");
/// let gpu_index = get_label_or_default!(labels, "index", "0");
/// ```
#[macro_export]
macro_rules! get_label_or_default {
    ($labels:expr_2021, $key:expr_2021) => {
        $labels
            .get($key)
            .map(|s| s.as_str())
            .unwrap_or("")
            .to_string()
    };
    ($labels:expr_2021, $key:expr_2021, $default:expr_2021) => {
        $labels
            .get($key)
            .map(|s| s.to_string())
            .unwrap_or_else(|| $default.to_string())
    };
}

/// Update a field within an optional struct field.
/// Useful for updating fields in optional nested structures like apple_silicon_info.
///
/// Example usage:
/// ```ignore
/// update_optional_field!(cpu_info, apple_silicon_info, p_core_count, value as u32);
/// ```
#[macro_export]
macro_rules! update_optional_field {
    ($parent:expr_2021, $optional_field:ident, $field:ident, $value:expr_2021) => {
        if let Some(ref mut inner) = $parent.$optional_field {
            inner.$field = $value;
        }
    };
}

/// Extract fields from a struct and insert them into a HashMap.
/// Useful for populating detail HashMaps from device structs.
/// Optimized to avoid redundant allocations for static strings.
///
/// Example usage:
/// ```ignore
/// extract_struct_fields!(detail, device, {
///     "serial_number" => device_sn,
///     "firmware_version" => firmware,
///     "pci_bdf" => pci_bdf
/// });
/// ```
#[macro_export]
macro_rules! extract_struct_fields {
    ($detail:expr_2021, $source:expr_2021, {
        $($key:literal => $field:ident),* $(,)?
    }) => {
        $(
            $detail.insert($key.into(), $source.$field.clone());
        )*
    };
}

/// Insert optional fields from a struct into a HashMap if they exist.
/// Skips None values automatically.
/// Optimized to avoid redundant allocations for static strings.
///
/// Example usage:
/// ```ignore
/// insert_optional_fields!(detail, static_info, {
///     "PCIe Address" => pcie_address,
///     "PCIe Vendor ID" => pcie_vendor_id,
///     "PCIe Device ID" => pcie_device_id
/// });
/// ```
#[macro_export]
macro_rules! insert_optional_fields {
    ($detail:expr_2021, $source:expr_2021, {
        $($key:literal => $field:ident),* $(,)?
    }) => {
        $(
            if let Some(ref value) = $source.$field {
                $detail.insert($key.into(), value.clone());
            }
        )*
    };
}

/// Parse a value after a colon with optional type conversion.
/// Simple utility for "Key: Value" parsing patterns.
///
/// Example usage:
/// ```ignore
/// let frequency = parse_colon_value!(line, u32);
/// let temperature = parse_colon_value!(line, f64);
/// ```
#[macro_export]
macro_rules! parse_colon_value {
    ($line:expr_2021, $type:ty) => {
        $line
            .split(':')
            .nth(1)
            .and_then(|s| s.split_whitespace().next())
            .and_then(|s| s.parse::<$type>().ok())
    };
}

/// Parse a line starting with a specific prefix and extract the value.
/// Useful for consistent prefix-based parsing.
///
/// Example usage:
/// ```ignore
/// if line.starts_with("CPU Temperature:") {
///     let temp = parse_prefixed_line!(line, "CPU Temperature:", f64);
/// }
/// ```
#[macro_export]
macro_rules! parse_prefixed_line {
    ($line:expr_2021, $prefix:expr_2021, $type:ty) => {
        if $line.starts_with($prefix) {
            $line
                .strip_prefix($prefix)
                .and_then(|s| s.trim().split_whitespace().next())
                .and_then(|s| s.parse::<$type>().ok())
        } else {
            None
        }
    };
}

#[cfg(test)]
mod tests {
    use regex::Regex;

    #[test]
    fn test_parse_metric_frequency() {
        let line = "GPU HW active frequency: 444 MHz";
        let v = parse_metric!(line, "MHz", u32);
        assert_eq!(v, Some(444u32));
    }

    #[test]
    fn test_parse_metric_percentage() {
        let line = "E-Cluster HW active residency:  64.29% (details omitted)";
        let v = parse_metric!(line, "%", f64);
        assert!(v.is_some());
        assert!((v.unwrap() - 64.29).abs() < 1e-6);
    }

    #[test]
    fn test_parse_metric_power() {
        let line = "CPU Power: 475 mW";
        let v = parse_metric!(line, "mW", f64);
        assert_eq!(v, Some(475.0));
    }

    #[test]
    fn test_parse_metric_invalid() {
        let line = "Invalid Line";
        let v = parse_metric!(line, "MHz", u32);
        assert!(v.is_none());
    }

    #[test]
    fn test_parse_prometheus_success() {
        let re = Regex::new(r"^all_smi_([^\{]+)\{([^}]+)\} ([\d\.]+)$").unwrap();
        let line = r#"all_smi_gpu_utilization{gpu="RTX", uuid="GPU-1"} 25.5"#;
        let parsed = parse_prometheus!(line, re);
        assert!(parsed.is_some());
        let (name, labels, value) = parsed.unwrap();
        assert_eq!(name, "gpu_utilization");
        assert!(labels.contains(r#"gpu="RTX""#));
        assert_eq!(value, 25.5);
    }

    #[test]
    fn test_parse_prometheus_invalid() {
        let re = Regex::new(r"^all_smi_([^\{]+)\{([^}]+)\} ([\d\.]+)$").unwrap();
        let line = "bad format";
        let parsed = parse_prometheus!(line, re);
        assert!(parsed.is_none());
    }

    #[test]
    fn test_extract_label_to_detail() {
        use std::collections::HashMap;

        let mut labels = HashMap::new();
        labels.insert("cuda_version".to_string(), "11.8".to_string());
        labels.insert("driver_version".to_string(), "525.60.13".to_string());

        let mut detail = HashMap::new();

        extract_label_to_detail!(labels, "cuda_version", detail, "cuda_version");
        assert_eq!(detail.get("cuda_version"), Some(&"11.8".to_string()));

        extract_label_to_detail!(labels, "driver_version", detail);
        assert_eq!(detail.get("driver_version"), Some(&"525.60.13".to_string()));

        // Test non-existent label
        extract_label_to_detail!(labels, "non_existent", detail);
        assert_eq!(detail.get("non_existent"), None);
    }

    #[test]
    fn test_extract_labels_batch() {
        use std::collections::HashMap;

        let mut labels = HashMap::new();
        labels.insert("cuda_version".to_string(), "11.8".to_string());
        labels.insert("driver_version".to_string(), "525.60.13".to_string());
        labels.insert("architecture".to_string(), "Ampere".to_string());

        let mut detail = HashMap::new();

        extract_labels_batch!(
            labels,
            detail,
            [
                "cuda_version",
                "driver_version",
                "architecture",
                "non_existent"
            ]
        );

        assert_eq!(detail.get("cuda_version"), Some(&"11.8".to_string()));
        assert_eq!(detail.get("driver_version"), Some(&"525.60.13".to_string()));
        assert_eq!(detail.get("architecture"), Some(&"Ampere".to_string()));
        assert_eq!(detail.get("non_existent"), None);
    }

    #[test]
    fn test_update_metric_field() {
        struct TestStruct {
            utilization: f64,
            memory: u64,
            temperature: u32,
        }

        let mut test = TestStruct {
            utilization: 0.0,
            memory: 0,
            temperature: 0,
        };

        let metric_name = "gpu_utilization";
        let value = 75.5;

        update_metric_field!(metric_name, value, test, {
            "gpu_utilization" => utilization as f64,
            "gpu_memory_used_bytes" => memory as u64,
            "gpu_temperature_celsius" => temperature as u32
        });

        assert_eq!(test.utilization, 75.5);

        let metric_name = "gpu_memory_used_bytes";
        let value = 1024.0;

        update_metric_field!(metric_name, value, test, {
            "gpu_utilization" => utilization as f64,
            "gpu_memory_used_bytes" => memory as u64,
            "gpu_temperature_celsius" => temperature as u32
        });

        assert_eq!(test.memory, 1024);
    }

    #[test]
    fn test_get_label_or_default() {
        use std::collections::HashMap;

        let mut labels = HashMap::new();
        labels.insert("gpu".to_string(), "RTX 4090".to_string());
        labels.insert("index".to_string(), "2".to_string());

        let gpu_name = get_label_or_default!(labels, "gpu");
        assert_eq!(gpu_name, "RTX 4090");

        let non_existent = get_label_or_default!(labels, "non_existent");
        assert_eq!(non_existent, "");

        let custom_default = get_label_or_default!(labels, "non_existent", "N/A");
        assert_eq!(custom_default, "N/A");

        let index = get_label_or_default!(labels, "index", "0");
        assert_eq!(index, "2");
    }
}
