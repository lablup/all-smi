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

/// Furiosa AI NPU-specific metric exporter
/// Currently uses common NPU metrics as Furiosa-specific metrics are not yet implemented
pub struct FuriosaExporter {
    common: CommonNpuExporter,
}

impl FuriosaExporter {
    pub fn new() -> Self {
        Self {
            common: CommonNpuExporter::new(),
        }
    }

    fn export_device_info(&self, builder: &mut MetricBuilder, info: &GpuInfo, index: usize) {
        // Export Furiosa-specific device information
        if let Some(device_name) = info.detail.get("device_name") {
            let device_labels = [
                ("npu", info.name.as_str()),
                ("instance", info.instance.as_str()),
                ("npu_uuid", info.uuid.as_str()),
                ("npu_index", &index.to_string()),
                ("device_name", device_name.as_str()),
            ];
            builder
                .help("all_smi_furiosa_device_info", "Furiosa device information")
                .type_("all_smi_furiosa_device_info", "gauge")
                .metric("all_smi_furiosa_device_info", &device_labels, 1);
        }

        // Export chip information if available
        if let Some(chip_name) = info.detail.get("chip_name") {
            let chip_labels = [
                ("npu", info.name.as_str()),
                ("instance", info.instance.as_str()),
                ("npu_uuid", info.uuid.as_str()),
                ("npu_index", &index.to_string()),
                ("chip", chip_name.as_str()),
            ];
            builder
                .help("all_smi_furiosa_chip_info", "Furiosa chip information")
                .type_("all_smi_furiosa_chip_info", "gauge")
                .metric("all_smi_furiosa_chip_info", &chip_labels, 1);
        }
    }

    fn export_firmware_info(&self, builder: &mut MetricBuilder, info: &GpuInfo, index: usize) {
        // Export Furiosa driver version if available
        if let Some(driver_version) = info.detail.get("driver_version") {
            let driver_labels = [
                ("npu", info.name.as_str()),
                ("instance", info.instance.as_str()),
                ("npu_uuid", info.uuid.as_str()),
                ("npu_index", &index.to_string()),
                ("version", driver_version.as_str()),
            ];
            builder
                .help("all_smi_furiosa_driver_info", "Furiosa driver version")
                .type_("all_smi_furiosa_driver_info", "gauge")
                .metric("all_smi_furiosa_driver_info", &driver_labels, 1);
        }

        // Export firmware version if available
        if let Some(firmware_version) = info.detail.get("firmware_version") {
            let fw_labels = [
                ("npu", info.name.as_str()),
                ("instance", info.instance.as_str()),
                ("npu_uuid", info.uuid.as_str()),
                ("npu_index", &index.to_string()),
                ("version", firmware_version.as_str()),
            ];
            builder
                .help("all_smi_furiosa_firmware_info", "Furiosa firmware version")
                .type_("all_smi_furiosa_firmware_info", "gauge")
                .metric("all_smi_furiosa_firmware_info", &fw_labels, 1);
        }
    }

    fn export_utilization_metrics(
        &self,
        builder: &mut MetricBuilder,
        info: &GpuInfo,
        index: usize,
    ) {
        let base_labels = [
            ("npu", info.name.as_str()),
            ("instance", info.instance.as_str()),
            ("npu_uuid", info.uuid.as_str()),
            ("npu_index", &index.to_string()),
        ];

        // Export NPU utilization if available
        if let Some(util_str) = info.detail.get("utilization")
            && let Some(util) = CommonNpuExporter::parse_numeric_value(util_str)
        {
            builder
                .help(
                    "all_smi_furiosa_utilization_percent",
                    "NPU utilization percentage",
                )
                .type_("all_smi_furiosa_utilization_percent", "gauge")
                .metric("all_smi_furiosa_utilization_percent", &base_labels, util);
        }

        // Export compute utilization if available
        if let Some(compute_util_str) = info.detail.get("compute_utilization")
            && let Some(compute_util) = CommonNpuExporter::parse_numeric_value(compute_util_str)
        {
            builder
                .help(
                    "all_smi_furiosa_compute_utilization_percent",
                    "NPU compute utilization percentage",
                )
                .type_("all_smi_furiosa_compute_utilization_percent", "gauge")
                .metric(
                    "all_smi_furiosa_compute_utilization_percent",
                    &base_labels,
                    compute_util,
                );
        }
    }

    fn export_memory_metrics(&self, builder: &mut MetricBuilder, info: &GpuInfo, index: usize) {
        let base_labels = [
            ("npu", info.name.as_str()),
            ("instance", info.instance.as_str()),
            ("npu_uuid", info.uuid.as_str()),
            ("npu_index", &index.to_string()),
        ];

        // Export memory usage if available
        if let Some(mem_used_str) = info.detail.get("memory_used")
            && let Some(mem_used) = CommonNpuExporter::parse_numeric_value(mem_used_str)
        {
            builder
                .help(
                    "all_smi_furiosa_memory_used_bytes",
                    "NPU memory used in bytes",
                )
                .type_("all_smi_furiosa_memory_used_bytes", "gauge")
                .metric("all_smi_furiosa_memory_used_bytes", &base_labels, mem_used);
        }

        // Export memory total if available
        if let Some(mem_total_str) = info.detail.get("memory_total")
            && let Some(mem_total) = CommonNpuExporter::parse_numeric_value(mem_total_str)
        {
            builder
                .help(
                    "all_smi_furiosa_memory_total_bytes",
                    "NPU total memory in bytes",
                )
                .type_("all_smi_furiosa_memory_total_bytes", "gauge")
                .metric(
                    "all_smi_furiosa_memory_total_bytes",
                    &base_labels,
                    mem_total,
                );
        }
    }

    fn export_clock_metrics(&self, builder: &mut MetricBuilder, info: &GpuInfo, index: usize) {
        let base_labels = [
            ("npu", info.name.as_str()),
            ("instance", info.instance.as_str()),
            ("npu_uuid", info.uuid.as_str()),
            ("npu_index", &index.to_string()),
        ];

        // Export clock frequencies if available
        if let Some(clock_str) = info.detail.get("clock_mhz")
            && let Some(clock) = CommonNpuExporter::parse_numeric_value(clock_str)
        {
            builder
                .help("all_smi_furiosa_clock_mhz", "NPU clock frequency in MHz")
                .type_("all_smi_furiosa_clock_mhz", "gauge")
                .metric("all_smi_furiosa_clock_mhz", &base_labels, clock);
        }

        // Export memory clock if available
        if let Some(mem_clock_str) = info.detail.get("memory_clock_mhz")
            && let Some(mem_clock) = CommonNpuExporter::parse_numeric_value(mem_clock_str)
        {
            builder
                .help(
                    "all_smi_furiosa_memory_clock_mhz",
                    "NPU memory clock frequency in MHz",
                )
                .type_("all_smi_furiosa_memory_clock_mhz", "gauge")
                .metric("all_smi_furiosa_memory_clock_mhz", &base_labels, mem_clock);
        }
    }

    fn export_device_status(&self, builder: &mut MetricBuilder, info: &GpuInfo, index: usize) {
        use super::common::status_values;

        // Export device status using common helper
        CommonNpuExporter::export_status_metric(
            builder,
            info,
            index,
            "all_smi_furiosa_status",
            "Device operational status",
            "status",
            status_values::NORMAL,
        );

        // Export ready state if available
        CommonNpuExporter::export_status_metric(
            builder,
            info,
            index,
            "all_smi_furiosa_ready",
            "Device ready state",
            "ready",
            status_values::READY,
        );
    }
}

impl Default for FuriosaExporter {
    fn default() -> Self {
        Self::new()
    }
}

impl NpuExporter for FuriosaExporter {
    fn can_handle(&self, info: &GpuInfo) -> bool {
        info.name.contains("Furiosa") || info.name.contains("RNGD") || info.name.contains("Warboy")
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

        // Export all Furiosa-specific metrics
        self.export_device_info(builder, info, index);
        self.export_firmware_info(builder, info, index);
        self.export_utilization_metrics(builder, info, index);
        self.export_memory_metrics(builder, info, index);
        self.export_clock_metrics(builder, info, index);
        self.export_device_status(builder, info, index);
    }

    fn vendor_name(&self) -> &'static str {
        "Furiosa"
    }
}

impl CommonNpuMetrics for FuriosaExporter {
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
        // Use vendor-specific device info for Furiosa
        self.export_device_info(builder, info, index);
    }

    fn export_firmware_info(&self, builder: &mut MetricBuilder, info: &GpuInfo, index: usize) {
        // Use vendor-specific firmware info for Furiosa
        self.export_firmware_info(builder, info, index);
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
