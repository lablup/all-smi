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

/// Intel Gaudi NPU-specific metric exporter
pub struct GaudiExporter {
    common: CommonNpuExporter,
}

impl GaudiExporter {
    pub fn new() -> Self {
        Self {
            common: CommonNpuExporter::new(),
        }
    }

    fn export_device_info(&self, builder: &mut MetricBuilder, info: &GpuInfo, index: usize) {
        let index_str = index.to_string();

        // Export Gaudi device information
        let device_labels = [
            ("npu", info.name.as_str()),
            ("instance", info.instance.as_str()),
            ("npu_uuid", info.uuid.as_str()),
            ("npu_index", &index_str),
        ];

        builder
            .help(
                "all_smi_gaudi_device_info",
                "Intel Gaudi device information",
            )
            .type_("all_smi_gaudi_device_info", "gauge")
            .metric("all_smi_gaudi_device_info", &device_labels, 1);

        // Export internal name if available (e.g., HL-325L)
        if let Some(internal_name) = info.detail.get("Internal Name") {
            let internal_labels = [
                ("npu", info.name.as_str()),
                ("instance", info.instance.as_str()),
                ("npu_uuid", info.uuid.as_str()),
                ("npu_index", &index_str),
                ("internal_name", internal_name.as_str()),
            ];
            builder
                .help(
                    "all_smi_gaudi_internal_name_info",
                    "Intel Gaudi internal device name",
                )
                .type_("all_smi_gaudi_internal_name_info", "gauge")
                .metric("all_smi_gaudi_internal_name_info", &internal_labels, 1);
        }
    }

    fn export_driver_info(&self, builder: &mut MetricBuilder, info: &GpuInfo, index: usize) {
        let index_str = index.to_string();

        // Export Habana driver version if available
        if let Some(driver_version) = info.detail.get("lib_version") {
            let driver_labels = [
                ("npu", info.name.as_str()),
                ("instance", info.instance.as_str()),
                ("npu_uuid", info.uuid.as_str()),
                ("npu_index", &index_str),
                ("version", driver_version.as_str()),
            ];
            builder
                .help(
                    "all_smi_gaudi_driver_info",
                    "Intel Gaudi (Habana) driver version",
                )
                .type_("all_smi_gaudi_driver_info", "gauge")
                .metric("all_smi_gaudi_driver_info", &driver_labels, 1);
        }
    }

    fn export_aip_metrics(&self, builder: &mut MetricBuilder, info: &GpuInfo, index: usize) {
        let index_str = index.to_string();
        let base_labels = [
            ("npu", info.name.as_str()),
            ("instance", info.instance.as_str()),
            ("npu_uuid", info.uuid.as_str()),
            ("npu_index", &index_str),
        ];

        // AIP (AI Processor) utilization - this is the main utilization metric for Gaudi
        builder
            .help(
                "all_smi_gaudi_aip_utilization_percent",
                "Gaudi AIP (AI Processor) utilization percentage",
            )
            .type_("all_smi_gaudi_aip_utilization_percent", "gauge")
            .metric(
                "all_smi_gaudi_aip_utilization_percent",
                &base_labels,
                info.utilization,
            );
    }

    fn export_memory_metrics(&self, builder: &mut MetricBuilder, info: &GpuInfo, index: usize) {
        let index_str = index.to_string();
        let base_labels = [
            ("npu", info.name.as_str()),
            ("instance", info.instance.as_str()),
            ("npu_uuid", info.uuid.as_str()),
            ("npu_index", &index_str),
        ];

        // Memory used in bytes
        builder
            .help(
                "all_smi_gaudi_memory_used_bytes",
                "Gaudi HBM memory used in bytes",
            )
            .type_("all_smi_gaudi_memory_used_bytes", "gauge")
            .metric(
                "all_smi_gaudi_memory_used_bytes",
                &base_labels,
                info.used_memory,
            );

        // Memory total in bytes
        builder
            .help(
                "all_smi_gaudi_memory_total_bytes",
                "Gaudi HBM total memory in bytes",
            )
            .type_("all_smi_gaudi_memory_total_bytes", "gauge")
            .metric(
                "all_smi_gaudi_memory_total_bytes",
                &base_labels,
                info.total_memory,
            );

        // Memory utilization percentage
        let memory_util = if info.total_memory > 0 {
            (info.used_memory as f64 / info.total_memory as f64) * 100.0
        } else {
            0.0
        };
        builder
            .help(
                "all_smi_gaudi_memory_utilization_percent",
                "Gaudi HBM memory utilization percentage",
            )
            .type_("all_smi_gaudi_memory_utilization_percent", "gauge")
            .metric(
                "all_smi_gaudi_memory_utilization_percent",
                &base_labels,
                memory_util,
            );
    }

    fn export_power_metrics(&self, builder: &mut MetricBuilder, info: &GpuInfo, index: usize) {
        let index_str = index.to_string();
        let base_labels = [
            ("npu", info.name.as_str()),
            ("instance", info.instance.as_str()),
            ("npu_uuid", info.uuid.as_str()),
            ("npu_index", &index_str),
        ];

        // Current power draw
        builder
            .help(
                "all_smi_gaudi_power_draw_watts",
                "Gaudi current power consumption in watts",
            )
            .type_("all_smi_gaudi_power_draw_watts", "gauge")
            .metric(
                "all_smi_gaudi_power_draw_watts",
                &base_labels,
                info.power_consumption,
            );

        // Power limit max if available
        if let Some(power_max_str) = info.detail.get("power_limit_max")
            && let Ok(power_max) = power_max_str.parse::<f64>()
        {
            builder
                .help(
                    "all_smi_gaudi_power_max_watts",
                    "Gaudi maximum power limit in watts",
                )
                .type_("all_smi_gaudi_power_max_watts", "gauge")
                .metric("all_smi_gaudi_power_max_watts", &base_labels, power_max);

            // Power utilization percentage
            let power_util = if power_max > 0.0 {
                (info.power_consumption / power_max) * 100.0
            } else {
                0.0
            };
            builder
                .help(
                    "all_smi_gaudi_power_utilization_percent",
                    "Gaudi power utilization percentage",
                )
                .type_("all_smi_gaudi_power_utilization_percent", "gauge")
                .metric(
                    "all_smi_gaudi_power_utilization_percent",
                    &base_labels,
                    power_util,
                );
        }
    }

    fn export_temperature_metrics(
        &self,
        builder: &mut MetricBuilder,
        info: &GpuInfo,
        index: usize,
    ) {
        let index_str = index.to_string();
        let base_labels = [
            ("npu", info.name.as_str()),
            ("instance", info.instance.as_str()),
            ("npu_uuid", info.uuid.as_str()),
            ("npu_index", &index_str),
        ];

        // AIP temperature
        builder
            .help(
                "all_smi_gaudi_temperature_celsius",
                "Gaudi AIP temperature in Celsius",
            )
            .type_("all_smi_gaudi_temperature_celsius", "gauge")
            .metric(
                "all_smi_gaudi_temperature_celsius",
                &base_labels,
                info.temperature,
            );
    }
}

impl Default for GaudiExporter {
    fn default() -> Self {
        Self::new()
    }
}

impl NpuExporter for GaudiExporter {
    fn can_handle(&self, info: &GpuInfo) -> bool {
        info.name.contains("Gaudi") || info.name.contains("Intel Gaudi")
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

        // Export all Gaudi-specific metrics
        self.export_device_info(builder, info, index);
        self.export_driver_info(builder, info, index);
        self.export_aip_metrics(builder, info, index);
        self.export_memory_metrics(builder, info, index);
        self.export_power_metrics(builder, info, index);
        self.export_temperature_metrics(builder, info, index);
    }

    fn vendor_name(&self) -> &'static str {
        "Intel Gaudi"
    }
}

impl CommonNpuMetrics for GaudiExporter {
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
        // Use vendor-specific device info for Gaudi
        self.export_device_info(builder, info, index);
    }

    fn export_firmware_info(&self, builder: &mut MetricBuilder, info: &GpuInfo, index: usize) {
        // Use vendor-specific driver info for Gaudi
        self.export_driver_info(builder, info, index);
    }

    fn export_temperature_metrics(
        &self,
        builder: &mut MetricBuilder,
        info: &GpuInfo,
        index: usize,
    ) {
        self.export_temperature_metrics(builder, info, index);
    }

    fn export_power_metrics(&self, builder: &mut MetricBuilder, info: &GpuInfo, index: usize) {
        self.export_power_metrics(builder, info, index);
    }
}
