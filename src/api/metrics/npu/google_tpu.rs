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

use super::common::CommonNpuExporter;
use super::exporter_trait::{CommonNpuMetrics, NpuExporter};
use crate::api::metrics::MetricBuilder;
use crate::device::GpuInfo;

/// Google TPU-specific metric exporter
pub struct GoogleTpuExporter {
    common: CommonNpuExporter,
}

impl GoogleTpuExporter {
    pub fn new() -> Self {
        Self {
            common: CommonNpuExporter::new(),
        }
    }
}

impl Default for GoogleTpuExporter {
    fn default() -> Self {
        Self::new()
    }
}

impl NpuExporter for GoogleTpuExporter {
    fn can_handle(&self, info: &GpuInfo) -> bool {
        // Handle both "Google TPU" and generic "TPU" (e.g. from mock)
        info.name.contains("Google TPU") || info.device_type.contains("TPU")
    }

    fn export_vendor_metrics(
        &self,
        builder: &mut MetricBuilder,
        info: &GpuInfo,
        index: usize,
        _index_str: &str,
    ) {
        if !self.can_handle(info) {
            return;
        }

        let index_str = index.to_string();
        let base_labels = [
            ("npu", info.name.as_str()),
            ("instance", info.instance.as_str()),
            ("npu_uuid", info.uuid.as_str()),
            ("npu_index", index_str.as_str()),
        ];

        // TPU Utilization (duty cycle or tensorcore utilization)
        builder
            .help(
                "all_smi_tpu_utilization_percent",
                "TPU utilization percentage",
            )
            .type_("all_smi_tpu_utilization_percent", "gauge")
            .metric(
                "all_smi_tpu_utilization_percent",
                &base_labels,
                info.utilization,
            );

        // TPU HBM Memory Usage
        builder
            .help(
                "all_smi_tpu_memory_used_bytes",
                "TPU HBM memory used in bytes",
            )
            .type_("all_smi_tpu_memory_used_bytes", "gauge")
            .metric(
                "all_smi_tpu_memory_used_bytes",
                &base_labels,
                info.used_memory as f64,
            );

        builder
            .help(
                "all_smi_tpu_memory_total_bytes",
                "TPU HBM memory total in bytes",
            )
            .type_("all_smi_tpu_memory_total_bytes", "gauge")
            .metric(
                "all_smi_tpu_memory_total_bytes",
                &base_labels,
                info.total_memory as f64,
            );

        // Memory utilization percentage
        let memory_util = if info.total_memory > 0 {
            (info.used_memory as f64 / info.total_memory as f64) * 100.0
        } else {
            0.0
        };
        builder
            .help(
                "all_smi_tpu_memory_utilization_percent",
                "TPU HBM memory utilization percentage",
            )
            .type_("all_smi_tpu_memory_utilization_percent", "gauge")
            .metric(
                "all_smi_tpu_memory_utilization_percent",
                &base_labels,
                memory_util,
            );

        // 1. Chip Version / Accelerator Type
        if let Some(chip_version) = info.detail.get("Chip Version") {
            let labels = [
                ("npu", info.name.as_str()),
                ("instance", info.instance.as_str()),
                ("npu_uuid", info.uuid.as_str()),
                ("npu_index", index_str.as_str()),
                ("version", chip_version.as_str()),
            ];
            builder
                .help(
                    "all_smi_tpu_chip_version_info",
                    "TPU chip version information",
                )
                .type_("all_smi_tpu_chip_version_info", "gauge")
                .metric("all_smi_tpu_chip_version_info", &labels, 1);
        }

        if let Some(accel_type) = info.detail.get("Accelerator Type") {
            let labels = [
                ("npu", info.name.as_str()),
                ("instance", info.instance.as_str()),
                ("npu_uuid", info.uuid.as_str()),
                ("npu_index", index_str.as_str()),
                ("type", accel_type.as_str()),
            ];
            builder
                .help(
                    "all_smi_tpu_accelerator_type_info",
                    "TPU accelerator type information",
                )
                .type_("all_smi_tpu_accelerator_type_info", "gauge")
                .metric("all_smi_tpu_accelerator_type_info", &labels, 1);
        }

        // 2. Core Counts
        if let Some(core_count) = info.detail.get("Core Count")
            && let Ok(count) = core_count.parse::<f64>()
        {
            builder
                .help("all_smi_tpu_core_count", "Number of TPU cores")
                .type_("all_smi_tpu_core_count", "gauge")
                .metric("all_smi_tpu_core_count", &base_labels, count);
        }

        if let Some(tc_count) = info.detail.get("TensorCore Count")
            && let Ok(count) = tc_count.parse::<f64>()
        {
            builder
                .help(
                    "all_smi_tpu_tensorcore_count",
                    "Number of TensorCores per chip",
                )
                .type_("all_smi_tpu_tensorcore_count", "gauge")
                .metric("all_smi_tpu_tensorcore_count", &base_labels, count);
        }

        // 3. Memory Type
        if let Some(mem_type) = info.detail.get("Memory Type") {
            let labels = [
                ("npu", info.name.as_str()),
                ("instance", info.instance.as_str()),
                ("npu_uuid", info.uuid.as_str()),
                ("npu_index", index_str.as_str()),
                ("type", mem_type.as_str()),
            ];
            builder
                .help(
                    "all_smi_tpu_memory_type_info",
                    "TPU memory type information",
                )
                .type_("all_smi_tpu_memory_type_info", "gauge")
                .metric("all_smi_tpu_memory_type_info", &labels, 1);
        }

        // 4. Runtime / Library Version
        if let Some(lib_ver) = info.detail.get("lib_version") {
            let labels = [
                ("npu", info.name.as_str()),
                ("instance", info.instance.as_str()),
                ("npu_uuid", info.uuid.as_str()),
                ("npu_index", index_str.as_str()),
                ("version", lib_ver.as_str()),
            ];
            builder
                .help(
                    "all_smi_tpu_runtime_version_info",
                    "TPU runtime/library version",
                )
                .type_("all_smi_tpu_runtime_version_info", "gauge")
                .metric("all_smi_tpu_runtime_version_info", &labels, 1);
        }

        // 5. Max Power Limit
        if let Some(max_power_str) = info.detail.get("Max Power") {
            // Format is usually "XXX W"
            if let Some(val_str) = max_power_str.split_whitespace().next()
                && let Ok(val) = val_str.parse::<f64>()
            {
                builder
                    .help(
                        "all_smi_tpu_power_max_watts",
                        "TPU maximum power limit in watts",
                    )
                    .type_("all_smi_tpu_power_max_watts", "gauge")
                    .metric("all_smi_tpu_power_max_watts", &base_labels, val);
            }
        }

        // 6. HLO Metrics (Queue Size and Execution Timing)
        if let Some(q_size_str) = info.detail.get("HLO Queue Size")
            && let Ok(val) = q_size_str.parse::<f64>()
        {
            builder
                .help(
                    "all_smi_tpu_hlo_queue_size",
                    "Number of pending HLO programs in the queue",
                )
                .type_("all_smi_tpu_hlo_queue_size", "gauge")
                .metric("all_smi_tpu_hlo_queue_size", &base_labels, val);
        }

        let hlo_metrics = [
            (
                "HLO Exec Mean",
                "all_smi_tpu_hlo_exec_mean_microseconds",
                "HLO execution timing mean in microseconds",
            ),
            (
                "HLO Exec P50",
                "all_smi_tpu_hlo_exec_p50_microseconds",
                "HLO execution timing 50th percentile in microseconds",
            ),
            (
                "HLO Exec P90",
                "all_smi_tpu_hlo_exec_p90_microseconds",
                "HLO execution timing 90th percentile in microseconds",
            ),
            (
                "HLO Exec P95",
                "all_smi_tpu_hlo_exec_p95_microseconds",
                "HLO execution timing 95th percentile in microseconds",
            ),
            (
                "HLO Exec P99.9",
                "all_smi_tpu_hlo_exec_p999_microseconds",
                "HLO execution timing 99.9th percentile in microseconds",
            ),
        ];

        for (detail_key, metric_name, help_text) in hlo_metrics {
            if let Some(val_str) = info.detail.get(detail_key) {
                // Format is "XXX.X µs"
                if let Some(v_str) = val_str.split_whitespace().next()
                    && let Ok(val) = v_str.parse::<f64>()
                {
                    builder
                        .help(metric_name, help_text)
                        .type_(metric_name, "gauge")
                        .metric(metric_name, &base_labels, val);
                }
            }
        }
    }

    fn vendor_name(&self) -> &'static str {
        "Google TPU"
    }
}

impl CommonNpuMetrics for GoogleTpuExporter {
    fn export_generic_npu_metrics(
        &self,
        builder: &mut MetricBuilder,
        info: &GpuInfo,
        index: usize,
    ) {
        self.common.export_generic_npu_metrics(builder, info, index);
    }

    fn export_generic_npu_metrics_str(
        &self,
        builder: &mut MetricBuilder,
        info: &GpuInfo,
        index_str: &str,
    ) {
        self.common
            .export_generic_npu_metrics_str(builder, info, index_str);
    }

    fn export_device_info(&self, builder: &mut MetricBuilder, info: &GpuInfo, index: usize) {
        self.common.export_device_info(builder, info, index);
    }

    fn export_firmware_info(&self, builder: &mut MetricBuilder, info: &GpuInfo, index: usize) {
        self.common.export_firmware_info(builder, info, index);
    }

    fn export_temperature_metrics(
        &self,
        builder: &mut MetricBuilder,
        info: &GpuInfo,
        index: usize,
    ) {
        self.common.export_temperature_metrics(builder, info, index);
    }

    fn export_power_metrics(&self, builder: &mut MetricBuilder, info: &GpuInfo, index: usize) {
        self.common.export_power_metrics(builder, info, index);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// A `GpuInfo` shaped like `readers::google_tpu` produces it for a v4
    /// slice with gRPC HLO telemetry available: the six queue/latency
    /// readings sit in `detail` under the Title Case keys the reader writes.
    fn tpu_with_hlo(entries: &[(&str, &str)]) -> GpuInfo {
        let mut detail: HashMap<String, String> = entries
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect();
        detail.insert("lib_name".to_string(), "libtpu".to_string());

        GpuInfo {
            uuid: "TPU-0".to_string(),
            time: "2026-09-21 00:00:00".to_string(),
            name: "Google TPU v4".to_string(),
            device_type: "TPU".to_string(),
            host_id: "tpu-node".to_string(),
            hostname: "tpu-node".to_string(),
            instance: "tpu-node".to_string(),
            utilization: 75.5,
            ane_utilization: 0.0,
            dla_utilization: None,
            tensorcore_utilization: Some(50.0),
            temperature: 65,
            used_memory: 16 * 1024 * 1024 * 1024,
            total_memory: 32 * 1024 * 1024 * 1024,
            frequency: 0,
            power_consumption: 150.0,
            gpu_core_count: Some(2),
            temperature_threshold_slowdown: None,
            temperature_threshold_shutdown: None,
            temperature_threshold_max_operating: None,
            temperature_threshold_acoustic: None,
            performance_state: None,
            fan_speed_rpm: None,
            numa_node_id: None,
            gsp_firmware_mode: None,
            gsp_firmware_version: None,
            nvlink_remote_devices: Vec::new(),
            gpm_metrics: None,
            detail,
        }
    }

    /// Issue #425 registered these six readings in `VOLATILE_DETAIL_KEYS` on
    /// the strength of this exporter already parsing them out of `detail`
    /// into their own `all_smi_tpu_hlo_*` gauges, so dropping them from the
    /// `all_smi_gpu_info` label set would not orphan the reading. Nothing
    /// exercised that parse end to end: a stale detail key or a broken
    /// `split_whitespace` unit strip would have silently dropped a gauge
    /// with every other test in the repo still green.
    #[test]
    fn hlo_readings_reach_their_own_gauges() {
        let device = tpu_with_hlo(&[
            ("HLO Queue Size", "3"),
            ("HLO Exec Mean", "125.5 µs"),
            ("HLO Exec P50", "100.0 µs"),
            ("HLO Exec P90", "150.0 µs"),
            ("HLO Exec P95", "200.0 µs"),
            ("HLO Exec P99.9", "400.0 µs"),
        ]);

        let mut builder = MetricBuilder::new();
        GoogleTpuExporter::new().export_vendor_metrics(&mut builder, &device, 0, "0");
        let output = builder.build();

        for (family, expected) in [
            ("all_smi_tpu_hlo_queue_size", 3.0),
            ("all_smi_tpu_hlo_exec_mean_microseconds", 125.5),
            ("all_smi_tpu_hlo_exec_p50_microseconds", 100.0),
            ("all_smi_tpu_hlo_exec_p90_microseconds", 150.0),
            ("all_smi_tpu_hlo_exec_p95_microseconds", 200.0),
            ("all_smi_tpu_hlo_exec_p999_microseconds", 400.0),
        ] {
            let prefix = format!("{family}{{");
            let line = output
                .lines()
                .find(|line| line.starts_with(&prefix))
                .unwrap_or_else(|| panic!("{family} missing from:\n{output}"));
            let value: f64 = line
                .rsplit(' ')
                .next()
                .and_then(|v| v.parse().ok())
                .unwrap_or_else(|| panic!("{family} sample is not a number: {line}"));
            assert!(
                (value - expected).abs() < 1e-9,
                "{family} reads {value}, expected {expected}"
            );
        }
    }

    /// A device with none of the six readings (gRPC unreachable, or a
    /// generation the collector does not query) publishes no HLO gauge at
    /// all, rather than a fabricated zero.
    #[test]
    fn hlo_gauges_are_omitted_without_a_reading() {
        let mut builder = MetricBuilder::new();
        GoogleTpuExporter::new().export_vendor_metrics(&mut builder, &tpu_with_hlo(&[]), 0, "0");
        let output = builder.build();

        for family in [
            "all_smi_tpu_hlo_queue_size",
            "all_smi_tpu_hlo_exec_mean_microseconds",
            "all_smi_tpu_hlo_exec_p50_microseconds",
            "all_smi_tpu_hlo_exec_p90_microseconds",
            "all_smi_tpu_hlo_exec_p95_microseconds",
            "all_smi_tpu_hlo_exec_p999_microseconds",
        ] {
            assert!(
                !output.contains(family),
                "{family} must not appear without a reading:\n{output}"
            );
        }
    }
}
