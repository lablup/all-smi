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

/// Rebellions NPU-specific metric exporter
pub struct RebellionsExporter {
    common: CommonNpuExporter,
}

impl RebellionsExporter {
    pub fn new() -> Self {
        Self {
            common: CommonNpuExporter::new(),
        }
    }

    fn export_firmware_info(&self, builder: &mut MetricBuilder, info: &GpuInfo, index: usize) {
        // Rebellions firmware info
        if let Some(fw_version) = info.detail.get("firmware_version") {
            let fw_labels = [
                ("npu", info.name.as_str()),
                ("instance", info.instance.as_str()),
                ("uuid", info.uuid.as_str()),
                ("index", &index.to_string()),
                ("firmware", fw_version.as_str()),
            ];
            builder
                .help(
                    "all_smi_rebellions_firmware_info",
                    "Rebellions NPU firmware version",
                )
                .type_("all_smi_rebellions_firmware_info", "gauge")
                .metric("all_smi_rebellions_firmware_info", &fw_labels, 1);
        }

        // KMD version
        if let Some(kmd_version) = info.detail.get("kmd_version") {
            let kmd_labels = [
                ("instance", info.instance.as_str()),
                ("version", kmd_version.as_str()),
            ];
            builder
                .help("all_smi_rebellions_kmd_info", "Rebellions KMD version")
                .type_("all_smi_rebellions_kmd_info", "gauge")
                .metric("all_smi_rebellions_kmd_info", &kmd_labels, 1);
        }
    }

    fn export_device_info(&self, builder: &mut MetricBuilder, info: &GpuInfo, index: usize) {
        if let Some(_device_name) = info.detail.get("device_name")
            && let Some(sid) = info.detail.get("serial_id")
        {
            let model_type = if info.name.contains("ATOM Max") {
                "ATOM-Max"
            } else if info.name.contains("ATOM+") {
                "ATOM-Plus"
            } else {
                "ATOM"
            };

            let device_labels = [
                ("npu", info.name.as_str()),
                ("instance", info.instance.as_str()),
                ("uuid", info.uuid.as_str()),
                ("index", &index.to_string()),
                ("model", model_type),
                ("sid", sid.as_str()),
                ("location", "5"), // Default location from mock server
            ];
            builder
                .help(
                    "all_smi_rebellions_device_info",
                    "Rebellions device information",
                )
                .type_("all_smi_rebellions_device_info", "gauge")
                .metric("all_smi_rebellions_device_info", &device_labels, 1);
        }
    }

    fn export_performance_state(&self, builder: &mut MetricBuilder, info: &GpuInfo, index: usize) {
        if let Some(pstate) = info.detail.get("performance_state") {
            let pstate_labels = [
                ("npu", info.name.as_str()),
                ("instance", info.instance.as_str()),
                ("uuid", info.uuid.as_str()),
                ("index", &index.to_string()),
                ("pstate", pstate.as_str()),
            ];
            builder
                .help(
                    "all_smi_rebellions_pstate_info",
                    "Current performance state",
                )
                .type_("all_smi_rebellions_pstate_info", "gauge")
                .metric("all_smi_rebellions_pstate_info", &pstate_labels, 1);
        }
    }

    fn export_device_status(&self, builder: &mut MetricBuilder, info: &GpuInfo, index: usize) {
        use super::common::status_values;

        CommonNpuExporter::export_status_metric(
            builder,
            info,
            index,
            "all_smi_rebellions_status",
            "Device operational status",
            "status",
            status_values::NORMAL,
        );
    }
}

impl Default for RebellionsExporter {
    fn default() -> Self {
        Self::new()
    }
}

impl NpuExporter for RebellionsExporter {
    fn can_handle(&self, info: &GpuInfo) -> bool {
        info.name.contains("Rebellions")
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

        // Export all Rebellions-specific metrics
        self.export_firmware_info(builder, info, index);
        self.export_device_info(builder, info, index);
        self.export_performance_state(builder, info, index);
        self.export_device_status(builder, info, index);
    }

    fn vendor_name(&self) -> &'static str {
        "Rebellions"
    }
}

impl CommonNpuMetrics for RebellionsExporter {
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
        // Use vendor-specific device info instead of common one for Rebellions
        self.export_device_info(builder, info, index);
    }

    fn export_firmware_info(&self, builder: &mut MetricBuilder, info: &GpuInfo, index: usize) {
        // Use vendor-specific firmware info for Rebellions
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
