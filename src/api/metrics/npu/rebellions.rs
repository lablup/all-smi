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

/// Value the Rebellions reader stamps into `detail["lib_name"]` for every
/// device it produces, regardless of SKU.
const RBLN_LIB_NAME: &str = "RBLN-SDK";

/// Does this device belong to the Rebellions exporter?
///
/// Routing prefers the vendor tag the reader writes into `detail["lib_name"]`,
/// which does not depend on how a particular card spells its marketing name.
/// The name check is the fallback for devices that arrive without that tag
/// (remote nodes parsed from Prometheus text, the mock server) and matches the
/// `RBLN` product prefix rather than one SKU: real hardware reports
/// `name = "RBLN-CA22"`, never the literal "Rebellions", so the old
/// `contains("Rebellions")` test routed no real device at all.
pub fn is_rebellions_device(info: &GpuInfo) -> bool {
    if info
        .detail
        .get("lib_name")
        .is_some_and(|lib| lib == RBLN_LIB_NAME)
    {
        return true;
    }

    let name = &info.name;
    name.contains("RBLN") || name.contains("rbln") || name.contains("Rebellions")
}

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
        // Rebellions firmware info. Detail keys are the Title Case ones the
        // Rebellions reader actually writes (see `DetailBuilder` usage there);
        // the snake_case keys this used to look up are never present, so the
        // metrics were empty even once routing reached this exporter.
        if let Some(fw_version) = info.detail.get("Firmware Version") {
            let fw_labels = [
                ("npu", info.name.as_str()),
                ("instance", info.instance.as_str()),
                ("npu_uuid", info.uuid.as_str()),
                ("npu_index", &index.to_string()),
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
        if let Some(kmd_version) = info.detail.get("KMD Version") {
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
        if let Some(sid) = info.detail.get("Serial ID") {
            let model_type = if info.name.contains("ATOM Max") {
                "ATOM-Max"
            } else if info.name.contains("ATOM+") {
                "ATOM-Plus"
            } else {
                "ATOM"
            };

            // Real slot index from the driver; "unknown" rather than a made-up
            // constant when the device did not report one.
            let location = info
                .detail
                .get("Location")
                .map_or("unknown", |value| value.as_str());

            let device_labels = [
                ("npu", info.name.as_str()),
                ("instance", info.instance.as_str()),
                ("npu_uuid", info.uuid.as_str()),
                ("npu_index", &index.to_string()),
                ("model", model_type),
                ("sid", sid.as_str()),
                ("location", location),
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
        if let Some(pstate) = info.detail.get("Performance State") {
            let pstate_labels = [
                ("npu", info.name.as_str()),
                ("instance", info.instance.as_str()),
                ("npu_uuid", info.uuid.as_str()),
                ("npu_index", &index.to_string()),
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
            "Status",
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
        is_rebellions_device(info)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// A `GpuInfo` shaped exactly as `readers::rebellions` produces it for a
    /// live RBLN-CA22 (ATOM Plus) card: the name is the SKU, never the vendor
    /// string, and the detail keys are the reader's Title Case ones.
    fn atom_plus_device() -> GpuInfo {
        let detail = [
            ("Serial ID", "0000000022513338"),
            ("Firmware Version", "3.0.0"),
            ("KMD Version", "3.0.0"),
            ("Device Path", "rbln0"),
            ("Board Info", "0005000c"),
            ("Location", "5"),
            ("Status", "normal"),
            ("Performance State", "P14"),
            ("lib_name", "RBLN-SDK"),
            ("lib_version", "3.0.0"),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect::<HashMap<_, _>>();

        GpuInfo {
            uuid: "4126c167-c7a6-4d0a-80dd-ffbf3641d1b0".to_string(),
            time: "2025-09-12 11:18:00".to_string(),
            name: "RBLN-CA22".to_string(),
            device_type: "NPU".to_string(),
            host_id: "atom-plus-01".to_string(),
            hostname: "atom-plus-01".to_string(),
            instance: "atom-plus-01".to_string(),
            utilization: 0.0,
            ane_utilization: 0.0,
            dla_utilization: None,
            tensorcore_utilization: None,
            temperature: 31,
            used_memory: 0,
            total_memory: 16_877_879_296,
            frequency: 0,
            power_consumption: 17.5218,
            gpu_core_count: None,
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

    fn named(name: &str) -> GpuInfo {
        GpuInfo {
            name: name.to_string(),
            detail: HashMap::new(),
            ..atom_plus_device()
        }
    }

    /// Regression: `can_handle` tested `name.contains("Rebellions")`, but no
    /// Rebellions card ever reports that name — the real one is "RBLN-CA22".
    #[test]
    fn real_hardware_is_recognised() {
        assert!(RebellionsExporter::new().can_handle(&atom_plus_device()));
        assert!(is_rebellions_device(&atom_plus_device()));

        // Name alone is enough when the reader's vendor tag is absent
        // (remote nodes, mock server), for any RBLN-prefixed SKU.
        assert!(is_rebellions_device(&named("RBLN-CA22")));
        assert!(is_rebellions_device(&named("RBLN-CA25")));
        assert!(is_rebellions_device(&named("Rebellions ATOM")));
    }

    #[test]
    fn other_vendors_are_not_claimed() {
        for name in ["HL-325L", "Intel Gaudi 3", "FuriosaAI RNGD", "TPU v5e"] {
            assert!(
                !is_rebellions_device(&named(name)),
                "{name} must not route to the Rebellions exporter"
            );
        }
    }

    /// Regression: the exporter looked up snake_case detail keys the reader
    /// never writes, so even a correctly routed device emitted nothing.
    #[test]
    fn vendor_metrics_are_emitted_for_a_real_device() {
        let mut builder = MetricBuilder::new();
        RebellionsExporter::new().export_vendor_metrics(&mut builder, &atom_plus_device(), 0, "0");
        let output = builder.build();

        for metric in [
            "all_smi_rebellions_firmware_info",
            "all_smi_rebellions_kmd_info",
            "all_smi_rebellions_device_info",
            "all_smi_rebellions_pstate_info",
            "all_smi_rebellions_status",
        ] {
            assert!(output.contains(metric), "missing {metric} in:\n{output}");
        }

        assert!(output.contains("firmware=\"3.0.0\""));
        assert!(output.contains("pstate=\"P14\""));
        assert!(output.contains("sid=\"0000000022513338\""));
        // Reported by the driver, not the constant the label used to carry.
        assert!(output.contains("location=\"5\""));
        assert!(output.contains("status=\"normal\""));
    }

    #[test]
    fn nothing_is_emitted_for_a_foreign_device() {
        let mut builder = MetricBuilder::new();
        RebellionsExporter::new().export_vendor_metrics(&mut builder, &named("HL-325L"), 0, "0");
        assert!(builder.build().is_empty());
    }
}
