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

use crate::api::metrics::MetricBuilder;
use crate::device::GpuInfo;
use tracing::{debug, warn};

/// Standard status values for NPU devices
pub mod status_values {
    pub const NORMAL: &str = "normal";
    pub const READY: &str = "true";
    // Reserved for future error handling
    #[allow(dead_code)]
    pub const ERROR: &str = "error";
    #[allow(dead_code)]
    pub const UNKNOWN: &str = "unknown";
}

/// Shared NPU exporter helpers
/// Parsing helpers used across all NPU vendors
pub struct CommonNpuExporter;

impl CommonNpuExporter {
    /// Helper function to parse hex register values commonly found in NPU metrics
    /// Safely handles overflow by using checked parsing and reasonable bounds
    #[cfg(target_os = "linux")]
    pub fn parse_hex_register(value: &str) -> Option<f64> {
        let trimmed = value.trim_start_matches("0x").trim();

        // Validate input: max 8 hex chars for u32 to prevent overflow
        if trimmed.len() > 8 || trimmed.is_empty() {
            debug!(
                "Invalid hex value length: {} (value: {})",
                trimmed.len(),
                value
            );
            return None;
        }

        // Validate hex characters
        if !trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
            debug!("Invalid hex characters in value: {}", value);
            return None;
        }

        // Use checked parsing to prevent panic on overflow
        match u32::from_str_radix(trimmed, 16) {
            Ok(reg_val) => Some(reg_val as f64),
            Err(e) => {
                warn!("Failed to parse hex value '{}': {}", value, e);
                None
            }
        }
    }

    /// Helper function to safely parse numeric values from device details
    /// Rejects NaN, infinity, and malformed values
    pub fn parse_numeric_value(value: &str) -> Option<f64> {
        let trimmed = value.trim();
        match trimmed.parse::<f64>() {
            Ok(v) if v.is_finite() => Some(v),
            Ok(v) => {
                warn!(
                    "Rejected non-finite numeric value: {} (parsed as: {})",
                    trimmed, v
                );
                None
            }
            Err(e) => {
                debug!("Failed to parse numeric value '{}': {}", trimmed, e);
                None
            }
        }
    }

    /// Export status metrics with predefined status values
    pub fn export_status_metric(
        builder: &mut MetricBuilder,
        info: &GpuInfo,
        index: usize,
        metric_name: &str,
        metric_help: &str,
        status_key: &str,
        normal_status: &str,
    ) {
        if let Some(status) = info.detail.get(status_key) {
            let status_value = if status == normal_status { 1.0 } else { 0.0 };
            let status_labels = [
                ("npu", info.name.as_str()),
                ("instance", info.instance.as_str()),
                ("npu_uuid", info.uuid.as_str()),
                ("npu_index", &index.to_string()),
                ("status", status.as_str()),
            ];
            builder
                .help(metric_name, metric_help)
                .type_(metric_name, "gauge")
                .metric(metric_name, &status_labels, status_value);
        }
    }
}
