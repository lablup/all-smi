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
#[cfg(target_os = "linux")]
pub mod neuron;
pub mod rebellions;
#[cfg(target_os = "linux")]
pub mod tenstorrent;

use crate::api::metrics::{MetricBuilder, MetricExporter};
use crate::device::GpuInfo;
use exporter_trait::NpuExporter;
use std::sync::OnceLock;

/// Static pool of vendor exporters to avoid repeated allocations
static EXPORTER_POOL: OnceLock<Vec<Box<dyn NpuExporter + Send + Sync>>> = OnceLock::new();

/// Position of the AWS Neuron exporter in [`EXPORTER_POOL`] on Linux.
/// The pool is `[Tenstorrent, Gaudi, Rebellions, Furiosa, Google TPU,
/// AWS Neuron]` there; `exporter_pool_indices_are_pinned` locks it in.
#[cfg(target_os = "linux")]
const NEURON_IDX: usize = 5;

/// Main NPU metric exporter that coordinates between different vendor-specific exporters
pub struct NpuMetricExporter<'a> {
    pub npu_info: &'a [GpuInfo],
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
            // APPEND ONLY. `find_exporter` addresses this pool by
            // hardcoded index, so inserting mid-list silently re-routes
            // another vendor's metrics. AWS Neuron is Linux-only, so it
            // cannot live in the cross-platform `vec!` above and is
            // pushed after the Tenstorrent insert instead — landing at
            // index 5, which `NEURON_IDX` below pins.
            #[cfg(target_os = "linux")]
            exporters.push(Box::new(neuron::NeuronExporter::new()));
            exporters
        });

        Self { npu_info }
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

            // AWS Neuron rows are named "AWS Trainium1" /
            // "AWS Neuron ..." by the reader; no other vendor's name
            // contains these substrings.
            #[cfg(target_os = "linux")]
            if neuron::is_neuron_device(info) {
                return Some(exporters[NEURON_IDX].as_ref());
            }

            // Index mapping based on platform
            // Linux: [Tenstorrent, Gaudi, Rebellions, Furiosa, Google TPU, AWS Neuron]
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

        // Export vendor-specific metrics. The generic `all_smi_npu_*` family
        // that used to run here first never fired for any vendor (issue
        // #431): no reader writes the `detail` keys it gated on, and NPU
        // devices export under the `all_smi_gpu_*` names through the GPU
        // exporter instead.
        self.export_vendor_metrics(builder, info, index, &index_str);
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

    /// Pool indices per platform, mirroring `find_exporter`'s mapping:
    /// Linux `[Tenstorrent, Gaudi, Rebellions, Furiosa, Google TPU, AWS Neuron]`,
    /// other `[Gaudi, Rebellions, Furiosa, Google TPU]`.
    #[cfg(target_os = "linux")]
    const TENSTORRENT_IDX: usize = 0;
    #[cfg(target_os = "linux")]
    const GAUDI_IDX: usize = 1;
    #[cfg(target_os = "linux")]
    const REBELLIONS_IDX: usize = 2;
    #[cfg(target_os = "linux")]
    const FURIOSA_IDX: usize = 3;
    #[cfg(target_os = "linux")]
    const TPU_IDX: usize = 4;
    #[cfg(not(target_os = "linux"))]
    const GAUDI_IDX: usize = 0;
    #[cfg(not(target_os = "linux"))]
    const REBELLIONS_IDX: usize = 1;
    #[cfg(not(target_os = "linux"))]
    const FURIOSA_IDX: usize = 2;
    #[cfg(not(target_os = "linux"))]
    const TPU_IDX: usize = 3;

    /// Pool index of the exporter `find_exporter` routes `info` to.
    fn vendor_for(info: &GpuInfo) -> Option<usize> {
        let devices: [GpuInfo; 0] = [];
        NpuMetricExporter::new(&devices)
            .find_exporter(info)
            .map(|exporter| {
                EXPORTER_POOL
                    .get()
                    .expect("pool initialized")
                    .iter()
                    .position(|e| std::ptr::eq(e.as_ref(), exporter))
                    .expect("the routed exporter is in the pool")
            })
    }

    fn vendor_for_name(name: &str) -> Option<usize> {
        vendor_for(&npu_named(name, &[]))
    }

    /// Regression: the fast path matched `name.contains("Rebellions")`, but a
    /// real card reports `RBLN-CA22`, so no device reached the Rebellions
    /// exporter and no `all_smi_rebellions_*` series was ever scraped.
    #[test]
    fn a_real_rebellions_card_routes_to_the_rebellions_exporter() {
        let tagged = npu_named("RBLN-CA22", &[("lib_name", "RBLN-SDK")]);
        assert_eq!(vendor_for(&tagged), Some(REBELLIONS_IDX));

        // Untagged (remote node / mock server), and a hypothetical later SKU.
        assert_eq!(
            vendor_for(&npu_named("RBLN-CA22", &[])),
            Some(REBELLIONS_IDX)
        );
        assert_eq!(
            vendor_for(&npu_named("RBLN-CA25", &[])),
            Some(REBELLIONS_IDX)
        );
        assert_eq!(
            vendor_for(&npu_named("Rebellions ATOM", &[])),
            Some(REBELLIONS_IDX)
        );
    }

    /// The exporter pool is indexed by hardcoded positions, so a change to the
    /// `vec!` order silently mis-routes vendors. Pin every position.
    #[test]
    fn the_other_vendors_still_route_to_their_own_exporters() {
        assert_eq!(vendor_for(&npu_named("HL-325L", &[])), Some(GAUDI_IDX));
        assert_eq!(
            vendor_for(&npu_named("Intel Gaudi 3", &[])),
            Some(GAUDI_IDX)
        );
        assert_eq!(
            vendor_for(&npu_named("FuriosaAI RNGD", &[])),
            Some(FURIOSA_IDX)
        );
        assert_eq!(vendor_for(&npu_named("Warboy", &[])), Some(FURIOSA_IDX));
        assert_eq!(vendor_for(&npu_named("TPU v5e", &[])), Some(TPU_IDX));

        #[cfg(target_os = "linux")]
        assert_eq!(
            vendor_for(&npu_named("Tenstorrent Wormhole", &[])),
            Some(TENSTORRENT_IDX)
        );
    }

    /// `find_exporter` addresses [`EXPORTER_POOL`] by hardcoded index, so
    /// appending a vendor must not re-route any existing one.
    #[test]
    fn exporter_pool_indices_are_pinned() {
        assert_eq!(vendor_for_name("Intel Gaudi 3"), Some(GAUDI_IDX));
        assert_eq!(vendor_for_name("Rebellions ATOM"), Some(REBELLIONS_IDX));
        assert_eq!(vendor_for_name("Furiosa RNGD"), Some(FURIOSA_IDX));
        assert_eq!(vendor_for_name("Google TPU v5e"), Some(TPU_IDX));

        #[cfg(target_os = "linux")]
        {
            assert_eq!(
                vendor_for_name("Tenstorrent Wormhole n150s"),
                Some(TENSTORRENT_IDX)
            );
            let pool = EXPORTER_POOL.get().expect("pool initialized");
            assert_eq!(pool.len(), 6);
            assert!(pool[NEURON_IDX].can_handle(&npu_named("AWS Trainium1", &[])));
        }
    }

    /// A Neuron row routes to the Neuron exporter, whatever the reader
    /// managed to resolve the device name to.
    #[cfg(target_os = "linux")]
    #[test]
    fn neuron_rows_route_to_the_neuron_exporter() {
        assert_eq!(vendor_for_name("AWS Trainium1"), Some(NEURON_IDX));
        assert_eq!(vendor_for_name("AWS Inferentia2"), Some(NEURON_IDX));
        assert_eq!(vendor_for_name("AWS Neuron Device"), Some(NEURON_IDX));
        assert_eq!(
            vendor_for(&npu_named("AWS Accelerator", &[("lib_name", "Neuron")])),
            Some(NEURON_IDX)
        );
        assert_eq!(vendor_for_name("Neuronal Accelerator"), None);
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

    /// Regression (issue #431): the generic `all_smi_npu_*` family was
    /// removed because no reader writes the `detail` keys it gated on, and
    /// NPU devices export under the `all_smi_gpu_*` names through the GPU
    /// exporter. The rows below still carry every key the removed exporter
    /// used to gate on, so a reintroduced half-wired copy fires here instead
    /// of passing silently.
    #[test]
    fn no_removed_generic_npu_metric_name_appears_in_a_full_exposition() {
        use crate::api::metrics::render::{MetricsRenderInputs, render_prometheus_exposition};
        use crate::utils::RuntimeEnvironment;

        const REMOVED: [&str; 5] = [
            "all_smi_npu_power_watts",
            "all_smi_npu_power_draw_watts",
            "all_smi_npu_temperature_celsius",
            "all_smi_npu_device_info",
            "all_smi_npu_firmware_info",
        ];
        // Keys no reader writes, but that the removed exporter gated on.
        let detail = &[
            ("power", "17.5"),
            ("power_draw", "17.5"),
            ("temperature", "31"),
            ("firmware", "3.0.0"),
        ];
        let mut tpu = npu_named("TPU v5e", detail);
        tpu.device_type = "TPU".to_string();
        let devices = [npu_named("RBLN-CA22", detail), tpu];

        let env = RuntimeEnvironment::default();
        let inputs = MetricsRenderInputs {
            gpu_info: &devices,
            process_info: &[],
            cpu_info: &[],
            memory_info: &[],
            storage_info: &[],
            runtime_environment: &env,
            chassis_info: &[],
            vgpu_info: &[],
            mig_info: &[],
            energy_integrator: None,
            ready: true,
        };
        let output = render_prometheus_exposition(&inputs);

        // The underlying values still reach the exposition through the GPU
        // exporter, fed from the typed fields.
        assert!(output.contains("all_smi_gpu_power_consumption_watts"));
        assert!(output.contains("all_smi_gpu_temperature_celsius"));

        for name in REMOVED {
            assert!(
                !output.lines().any(|line| line.contains(name)),
                "removed generic NPU metric `{name}` must not appear in the exposition: {output}"
            );
        }
    }
}
