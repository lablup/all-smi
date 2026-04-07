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

use super::exporter_trait::CommonNpuMetrics;
use crate::api::metrics::MetricBuilder;
use crate::device::GpuInfo;
use tracing::{debug, warn};

/// Maximum allowed length for device names and UUIDs in metrics
const MAX_LABEL_LENGTH: usize = 128;

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

/// Common NPU metrics implementation
/// Contains shared functionality and patterns used across all NPU vendors
pub struct CommonNpuExporter;

impl CommonNpuExporter {
    pub fn new() -> Self {
        Self
    }

    /// Sanitize and validate a label value for safe use in metrics
    /// Prevents injection attacks and ensures valid metric labels
    pub fn sanitize_label(value: &str) -> String {
        // Truncate to max length
        let truncated = if value.len() > MAX_LABEL_LENGTH {
            warn!(
                "Label value too long ({}), truncating to {} chars",
                value.len(),
                MAX_LABEL_LENGTH
            );
            &value[..MAX_LABEL_LENGTH]
        } else {
            value
        };

        // Replace invalid characters with underscore
        // Prometheus labels must match [a-zA-Z_][a-zA-Z0-9_]*
        truncated
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.' {
                    c
                } else {
                    '_'
                }
            })
            .collect()
    }

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
                ("uuid", info.uuid.as_str()),
                ("index", &index.to_string()),
                ("status", status.as_str()),
            ];
            builder
                .help(metric_name, metric_help)
                .type_(metric_name, "gauge")
                .metric(metric_name, &status_labels, status_value);
        }
    }
}

impl Default for CommonNpuExporter {
    fn default() -> Self {
        Self::new()
    }
}

impl CommonNpuMetrics for CommonNpuExporter {
    fn export_generic_npu_metrics(
        &self,
        builder: &mut MetricBuilder,
        info: &GpuInfo,
        index: usize,
    ) {
        let index_str = index.to_string();
        self.export_generic_npu_metrics_str(builder, info, &index_str);
    }

    fn export_generic_npu_metrics_str(
        &self,
        builder: &mut MetricBuilder,
        info: &GpuInfo,
        index_str: &str,
    ) {
        // Device type check removed - caller ensures NPU-only devices
        // Generic NPU firmware version
        if let Some(firmware) = info.detail.get("firmware") {
            // Sanitize labels to prevent injection
            let safe_name = Self::sanitize_label(&info.name);
            let safe_instance = Self::sanitize_label(&info.instance);
            let safe_uuid = Self::sanitize_label(&info.uuid);
            let safe_firmware = Self::sanitize_label(firmware);

            let fw_labels = [
                ("npu", safe_name.as_str()),
                ("instance", safe_instance.as_str()),
                ("uuid", safe_uuid.as_str()),
                ("index", index_str),
                ("firmware", safe_firmware.as_str()),
            ];
            builder
                .help("all_smi_npu_firmware_info", "NPU firmware version")
                .type_("all_smi_npu_firmware_info", "gauge")
                .metric("all_smi_npu_firmware_info", &fw_labels, 1);
        }
    }

    fn export_device_info(&self, builder: &mut MetricBuilder, info: &GpuInfo, index: usize) {
        // Export basic device information
        let device_labels = [
            ("npu", info.name.as_str()),
            ("instance", info.instance.as_str()),
            ("uuid", info.uuid.as_str()),
            ("index", &index.to_string()),
            ("device_type", info.device_type.as_str()),
        ];

        builder
            .help("all_smi_npu_device_info", "NPU device information")
            .type_("all_smi_npu_device_info", "gauge")
            .metric("all_smi_npu_device_info", &device_labels, 1);
    }

    fn export_firmware_info(&self, builder: &mut MetricBuilder, info: &GpuInfo, index: usize) {
        // Generic firmware version export (this is called by the generic method above)
        self.export_generic_npu_metrics(builder, info, index);
    }

    fn export_temperature_metrics(
        &self,
        builder: &mut MetricBuilder,
        info: &GpuInfo,
        index: usize,
    ) {
        let base_labels = [
            ("npu", info.name.as_str()),
            ("instance", info.instance.as_str()),
            ("uuid", info.uuid.as_str()),
            ("index", &index.to_string()),
        ];

        // Generic temperature metric if available
        if let Some(temp_str) = info.detail.get("temperature")
            && let Some(temp) = Self::parse_numeric_value(temp_str)
        {
            builder
                .help(
                    "all_smi_npu_temperature_celsius",
                    "NPU temperature in celsius",
                )
                .type_("all_smi_npu_temperature_celsius", "gauge")
                .metric("all_smi_npu_temperature_celsius", &base_labels, temp);
        }
    }

    fn export_power_metrics(&self, builder: &mut MetricBuilder, info: &GpuInfo, index: usize) {
        let base_labels = [
            ("npu", info.name.as_str()),
            ("instance", info.instance.as_str()),
            ("uuid", info.uuid.as_str()),
            ("index", &index.to_string()),
        ];

        // Generic power metric if available
        if let Some(power_str) = info.detail.get("power")
            && let Some(power) = Self::parse_numeric_value(power_str)
        {
            builder
                .help("all_smi_npu_power_watts", "NPU power consumption in watts")
                .type_("all_smi_npu_power_watts", "gauge")
                .metric("all_smi_npu_power_watts", &base_labels, power);
        }

        // Generic power draw (common field name)
        if let Some(power_str) = info.detail.get("power_draw")
            && let Some(power) = Self::parse_numeric_value(power_str)
        {
            builder
                .help("all_smi_npu_power_draw_watts", "NPU power draw in watts")
                .type_("all_smi_npu_power_draw_watts", "gauge")
                .metric("all_smi_npu_power_draw_watts", &base_labels, power);
        }
    }
}
