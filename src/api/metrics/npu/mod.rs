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

pub mod common;
pub mod exporter_trait;
pub mod furiosa;
pub mod gaudi;
pub mod google_tpu;
pub mod rebellions;
#[cfg(target_os = "linux")]
pub mod tenstorrent;

use crate::api::metrics::{MetricBuilder, MetricExporter};
use crate::device::GpuInfo;
use exporter_trait::{CommonNpuMetrics, NpuExporter};
use std::sync::OnceLock;

/// Static pool of vendor exporters to avoid repeated allocations
static EXPORTER_POOL: OnceLock<Vec<Box<dyn NpuExporter + Send + Sync>>> = OnceLock::new();

/// Main NPU metric exporter that coordinates between different vendor-specific exporters
pub struct NpuMetricExporter<'a> {
    pub npu_info: &'a [GpuInfo],
    common: common::CommonNpuExporter,
}

impl<'a> NpuMetricExporter<'a> {
    pub fn new(npu_info: &'a [GpuInfo]) -> Self {
        // Initialize the exporter pool once
        EXPORTER_POOL.get_or_init(|| {
            #[allow(unused_mut)]
            let mut exporters: Vec<Box<dyn NpuExporter + Send + Sync>> = vec![
                Box::new(gaudi::GaudiExporter::new()),
                Box::new(rebellions::RebellionsExporter::new()),
                Box::new(furiosa::FuriosaExporter::new()),
                Box::new(google_tpu::GoogleTpuExporter::new()),
            ];
            #[cfg(target_os = "linux")]
            exporters.insert(0, Box::new(tenstorrent::TenstorrentExporter::new()));
            exporters
        });

        Self {
            npu_info,
            common: common::CommonNpuExporter::new(),
        }
    }

    /// Find the appropriate exporter for a given NPU device
    /// Optimized with early pattern matching to avoid linear search
    fn find_exporter(&self, info: &GpuInfo) -> Option<&(dyn NpuExporter + Send + Sync)> {
        EXPORTER_POOL.get().and_then(|exporters| {
            // Fast path: match common vendor patterns first
            let name = &info.name;

            // Direct index access for known vendors (most common first)
            #[cfg(target_os = "linux")]
            if name.contains("Tenstorrent") {
                return Some(exporters[0].as_ref());
            }

            // Index mapping based on platform
            // Linux: [Tenstorrent, Gaudi, Rebellions, Furiosa, Google TPU]
            // Other: [Gaudi, Rebellions, Furiosa, Google TPU]
            #[cfg(target_os = "linux")]
            let (gaudi_idx, rebellions_idx, furiosa_idx, tpu_idx) = (1, 2, 3, 4);
            #[cfg(not(target_os = "linux"))]
            let (gaudi_idx, rebellions_idx, furiosa_idx, tpu_idx) = (0, 1, 2, 3);

            if name.contains("Gaudi") || name.contains("HL-") {
                return Some(exporters[gaudi_idx].as_ref());
            } else if rebellions::is_rebellions_device(info) {
                return Some(exporters[rebellions_idx].as_ref());
            } else if name.contains("Furiosa") || name.contains("RNGD") || name.contains("Warboy") {
                return Some(exporters[furiosa_idx].as_ref());
            } else if name.contains("TPU") || name.contains("Google") {
                return Some(exporters[tpu_idx].as_ref());
            }

            // Fallback to dynamic check for unknown patterns
            exporters
                .iter()
                .find(|exporter| exporter.can_handle(info))
                .map(|b| b.as_ref())
        })
    }

    /// Export generic NPU metrics that are common across all vendors
    #[allow(dead_code)]
    fn export_generic_npu_metrics(
        &self,
        builder: &mut MetricBuilder,
        info: &GpuInfo,
        index: usize,
    ) {
        // Device type check removed - caller already filters NPU devices
        // Always export common metrics first
        self.common.export_generic_npu_metrics(builder, info, index);
    }

    /// Export vendor-specific metrics using the appropriate exporter
    fn export_vendor_metrics(
        &self,
        builder: &mut MetricBuilder,
        info: &GpuInfo,
        index: usize,
        index_str: &str,
    ) {
        if let Some(exporter) = self.find_exporter(info) {
            exporter.export_vendor_metrics(builder, info, index, index_str);
        }
    }

    /// Export all NPU metrics for a single device
    fn export_device_metrics(&self, builder: &mut MetricBuilder, info: &GpuInfo, index: usize) {
        // Pre-allocate index string once per device
        let index_str = index.to_string();

        // Export generic metrics first
        self.export_generic_npu_metrics_with_str(builder, info, &index_str);

        // Then export vendor-specific metrics
        self.export_vendor_metrics(builder, info, index, &index_str);
    }

    /// Export generic NPU metrics with pre-allocated index string
    fn export_generic_npu_metrics_with_str(
        &self,
        builder: &mut MetricBuilder,
        info: &GpuInfo,
        index_str: &str,
    ) {
        // Device type check removed - caller already filters NPU devices
        // Always export common metrics first
        self.common
            .export_generic_npu_metrics_str(builder, info, index_str);
    }
}

impl<'a> MetricExporter for NpuMetricExporter<'a> {
    fn export_metrics(&self) -> String {
        let mut builder = MetricBuilder::new();

        // Filter NPU devices and export metrics
        for (i, info) in self.npu_info.iter().enumerate() {
            // Only process NPU or TPU devices
            if info.device_type == "NPU" || info.device_type == "TPU" {
                self.export_device_metrics(&mut builder, info, i);
            }
        }

        builder.build()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn npu_named(name: &str, detail: &[(&str, &str)]) -> GpuInfo {
        GpuInfo {
            uuid: "4126c167-c7a6-4d0a-80dd-ffbf3641d1b0".to_string(),
            time: "2025-09-12 11:18:00".to_string(),
            name: name.to_string(),
            device_type: "NPU".to_string(),
            host_id: "node01".to_string(),
            hostname: "node01".to_string(),
            instance: "node01".to_string(),
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
            detail: detail
                .iter()
                .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
                .collect::<HashMap<_, _>>(),
        }
    }

    fn vendor_for(info: &GpuInfo) -> Option<&'static str> {
        let devices: [GpuInfo; 0] = [];
        NpuMetricExporter::new(&devices)
            .find_exporter(info)
            .map(|exporter| exporter.vendor_name())
    }

    /// Regression: the fast path matched `name.contains("Rebellions")`, but a
    /// real card reports `RBLN-CA22`, so no device reached the Rebellions
    /// exporter and no `all_smi_rebellions_*` series was ever scraped.
    #[test]
    fn a_real_rebellions_card_routes_to_the_rebellions_exporter() {
        let tagged = npu_named("RBLN-CA22", &[("lib_name", "RBLN-SDK")]);
        assert_eq!(vendor_for(&tagged), Some("Rebellions"));

        // Untagged (remote node / mock server), and a hypothetical later SKU.
        assert_eq!(vendor_for(&npu_named("RBLN-CA22", &[])), Some("Rebellions"));
        assert_eq!(vendor_for(&npu_named("RBLN-CA25", &[])), Some("Rebellions"));
        assert_eq!(
            vendor_for(&npu_named("Rebellions ATOM", &[])),
            Some("Rebellions")
        );
    }

    /// The exporter pool is indexed by hardcoded positions, so a change to the
    /// `vec!` order silently mis-routes vendors. Pin every position.
    #[test]
    fn the_other_vendors_still_route_to_their_own_exporters() {
        assert_eq!(vendor_for(&npu_named("HL-325L", &[])), Some("Intel Gaudi"));
        assert_eq!(
            vendor_for(&npu_named("Intel Gaudi 3", &[])),
            Some("Intel Gaudi")
        );
        assert_eq!(
            vendor_for(&npu_named("FuriosaAI RNGD", &[])),
            Some("Furiosa")
        );
        assert_eq!(vendor_for(&npu_named("Warboy", &[])), Some("Furiosa"));
        assert_eq!(vendor_for(&npu_named("TPU v5e", &[])), Some("Google TPU"));

        #[cfg(target_os = "linux")]
        assert_eq!(
            vendor_for(&npu_named("Tenstorrent Wormhole", &[])),
            Some("Tenstorrent")
        );
    }

    #[test]
    fn an_unknown_accelerator_routes_nowhere() {
        assert_eq!(vendor_for(&npu_named("Some Unknown NPU", &[])), None);
    }

    /// End to end: a scrape of a real Rebellions node carries both the generic
    /// NPU series and the vendor-specific ones.
    #[test]
    fn a_rebellions_scrape_contains_vendor_metrics() {
        let devices = [npu_named(
            "RBLN-CA22",
            &[
                ("lib_name", "RBLN-SDK"),
                ("Serial ID", "0000000022513338"),
                ("Firmware Version", "3.0.0"),
                ("KMD Version", "3.0.0"),
                ("Status", "normal"),
                ("Performance State", "P14"),
                ("Location", "5"),
            ],
        )];
        let output = NpuMetricExporter::new(&devices).export_metrics();

        assert!(output.contains("all_smi_rebellions_device_info"));
        assert!(output.contains("all_smi_rebellions_firmware_info"));
        assert!(output.contains("all_smi_rebellions_status"));
    }
}
