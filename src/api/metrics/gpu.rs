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

use super::{MetricBuilder, MetricExporter};
use crate::device::GpuInfo;
use crate::parsing::common::sanitize_label_name;

pub struct GpuMetricExporter<'a> {
    pub gpu_info: &'a [GpuInfo],
}

impl<'a> GpuMetricExporter<'a> {
    pub fn new(gpu_info: &'a [GpuInfo]) -> Self {
        Self { gpu_info }
    }

    fn export_basic_metrics(&self, builder: &mut MetricBuilder, info: &GpuInfo, index: usize) {
        let base_labels = [
            ("gpu", info.name.as_str()),
            ("instance", info.instance.as_str()),
            ("uuid", info.uuid.as_str()),
            ("index", &index.to_string()),
        ];

        // GPU utilization
        builder
            .help("all_smi_gpu_utilization", "GPU utilization percentage")
            .type_("all_smi_gpu_utilization", "gauge")
            .metric("all_smi_gpu_utilization", &base_labels, info.utilization);

        // Memory metrics
        builder
            .help("all_smi_gpu_memory_used_bytes", "GPU memory used in bytes")
            .type_("all_smi_gpu_memory_used_bytes", "gauge")
            .metric(
                "all_smi_gpu_memory_used_bytes",
                &base_labels,
                info.used_memory,
            );

        builder
            .help(
                "all_smi_gpu_memory_total_bytes",
                "GPU memory total in bytes",
            )
            .type_("all_smi_gpu_memory_total_bytes", "gauge")
            .metric(
                "all_smi_gpu_memory_total_bytes",
                &base_labels,
                info.total_memory,
            );

        // Temperature
        builder
            .help(
                "all_smi_gpu_temperature_celsius",
                "GPU temperature in celsius",
            )
            .type_("all_smi_gpu_temperature_celsius", "gauge")
            .metric(
                "all_smi_gpu_temperature_celsius",
                &base_labels,
                info.temperature,
            );

        // Power consumption
        builder
            .help(
                "all_smi_gpu_power_consumption_watts",
                "GPU power consumption in watts",
            )
            .type_("all_smi_gpu_power_consumption_watts", "gauge")
            .metric(
                "all_smi_gpu_power_consumption_watts",
                &base_labels,
                info.power_consumption,
            );

        // Frequency
        builder
            .help("all_smi_gpu_frequency_mhz", "GPU frequency in MHz")
            .type_("all_smi_gpu_frequency_mhz", "gauge")
            .metric("all_smi_gpu_frequency_mhz", &base_labels, info.frequency);

        // ANE utilization (Apple Silicon)
        builder
            .help("all_smi_ane_utilization", "ANE utilization in mW")
            .type_("all_smi_ane_utilization", "gauge")
            .metric(
                "all_smi_ane_utilization",
                &base_labels,
                info.ane_utilization,
            );

        // DLA utilization (if available)
        if let Some(dla_util) = info.dla_utilization {
            builder
                .help("all_smi_dla_utilization", "DLA utilization percentage")
                .type_("all_smi_dla_utilization", "gauge")
                .metric("all_smi_dla_utilization", &base_labels, dla_util);
        }
    }

    fn export_apple_silicon_metrics(
        &self,
        builder: &mut MetricBuilder,
        info: &GpuInfo,
        index: usize,
    ) {
        if !info.name.contains("Apple") && !info.name.contains("Metal") {
            return;
        }

        let base_labels = [
            ("gpu", info.name.as_str()),
            ("instance", info.instance.as_str()),
            ("uuid", info.uuid.as_str()),
            ("index", &index.to_string()),
        ];

        // ANE power in watts
        builder
            .help("all_smi_ane_power_watts", "ANE power consumption in watts")
            .type_("all_smi_ane_power_watts", "gauge")
            .metric(
                "all_smi_ane_power_watts",
                &base_labels,
                info.ane_utilization / 1000.0,
            );

        // Thermal pressure level
        if let Some(thermal_level) = info.detail.get("thermal_pressure") {
            let thermal_labels = [
                ("gpu", info.name.as_str()),
                ("instance", info.instance.as_str()),
                ("uuid", info.uuid.as_str()),
                ("index", &index.to_string()),
                ("level", thermal_level.as_str()),
            ];
            builder
                .help("all_smi_thermal_pressure_info", "Thermal pressure level")
                .type_("all_smi_thermal_pressure_info", "gauge")
                .metric("all_smi_thermal_pressure_info", &thermal_labels, 1);
        }

        // Combined power (CPU + GPU + ANE) for Apple Silicon
        if let Some(combined_power_str) = info.detail.get("combined_power_mw") {
            if let Ok(combined_power_mw) = combined_power_str.parse::<f64>() {
                let combined_power_watts = combined_power_mw / 1000.0;
                builder
                    .help(
                        "all_smi_combined_power_watts",
                        "Combined power consumption (CPU + GPU + ANE) in watts",
                    )
                    .type_("all_smi_combined_power_watts", "gauge")
                    .metric(
                        "all_smi_combined_power_watts",
                        &base_labels,
                        combined_power_watts,
                    );
            }
        }
    }

    fn export_device_info(&self, builder: &mut MetricBuilder, info: &GpuInfo, index: usize) {
        let index_str = index.to_string();

        // Build label string with all detail fields
        let labels = [
            ("gpu", info.name.as_str()),
            ("instance", info.instance.as_str()),
            ("uuid", info.uuid.as_str()),
            ("index", index_str.as_str()),
            ("type", info.device_type.as_str()),
        ];

        // Convert detail HashMap to label pairs with sanitized names
        let detail_labels: Vec<(String, String)> = info
            .detail
            .iter()
            .map(|(k, v)| (sanitize_label_name(k), v.clone()))
            .collect();

        builder
            .help("all_smi_gpu_info", "GPU/NPU device information")
            .type_("all_smi_gpu_info", "gauge");

        // Build dynamic labels by combining base and detail labels
        let mut all_labels = Vec::new();

        // Add base labels
        for (key, value) in labels.iter() {
            all_labels.push((*key, *value));
        }

        // Add detail labels
        for (key, value) in &detail_labels {
            all_labels.push((key.as_str(), value.as_str()));
        }

        // Use the metric method with all labels
        builder.metric("all_smi_gpu_info", &all_labels, 1);
    }

    fn export_cuda_metrics(&self, builder: &mut MetricBuilder, info: &GpuInfo, index: usize) {
        let base_labels = [
            ("gpu", info.name.as_str()),
            ("instance", info.instance.as_str()),
            ("uuid", info.uuid.as_str()),
            ("index", &index.to_string()),
        ];

        // PCIe metrics
        if let Some(pcie_gen) = info.detail.get("pcie_gen_current") {
            if let Ok(gen) = pcie_gen.parse::<f64>() {
                builder
                    .help("all_smi_gpu_pcie_gen_current", "Current PCIe generation")
                    .type_("all_smi_gpu_pcie_gen_current", "gauge")
                    .metric("all_smi_gpu_pcie_gen_current", &base_labels, gen);
            }
        }

        if let Some(pcie_width) = info.detail.get("pcie_width_current") {
            if let Ok(width) = pcie_width.parse::<f64>() {
                builder
                    .help("all_smi_gpu_pcie_width_current", "Current PCIe link width")
                    .type_("all_smi_gpu_pcie_width_current", "gauge")
                    .metric("all_smi_gpu_pcie_width_current", &base_labels, width);
            }
        }

        // Clock metrics
        if let Some(clock_max) = info.detail.get("clock_graphics_max") {
            if let Ok(clock) = clock_max.parse::<f64>() {
                builder
                    .help(
                        "all_smi_gpu_clock_graphics_max_mhz",
                        "Maximum graphics clock in MHz",
                    )
                    .type_("all_smi_gpu_clock_graphics_max_mhz", "gauge")
                    .metric("all_smi_gpu_clock_graphics_max_mhz", &base_labels, clock);
            }
        }

        if let Some(clock_max) = info.detail.get("clock_memory_max") {
            if let Ok(clock) = clock_max.parse::<f64>() {
                builder
                    .help(
                        "all_smi_gpu_clock_memory_max_mhz",
                        "Maximum memory clock in MHz",
                    )
                    .type_("all_smi_gpu_clock_memory_max_mhz", "gauge")
                    .metric("all_smi_gpu_clock_memory_max_mhz", &base_labels, clock);
            }
        }

        // Power limit metrics
        if let Some(power_limit) = info.detail.get("power_limit_current") {
            if let Ok(power) = power_limit.parse::<f64>() {
                builder
                    .help(
                        "all_smi_gpu_power_limit_current_watts",
                        "Current power limit in watts",
                    )
                    .type_("all_smi_gpu_power_limit_current_watts", "gauge")
                    .metric("all_smi_gpu_power_limit_current_watts", &base_labels, power);
            }
        }

        if let Some(power_limit) = info.detail.get("power_limit_max") {
            if let Ok(power) = power_limit.parse::<f64>() {
                builder
                    .help(
                        "all_smi_gpu_power_limit_max_watts",
                        "Maximum power limit in watts",
                    )
                    .type_("all_smi_gpu_power_limit_max_watts", "gauge")
                    .metric("all_smi_gpu_power_limit_max_watts", &base_labels, power);
            }
        }

        // Performance state
        if let Some(pstate) = info.detail.get("performance_state") {
            if let Some(state_str) = pstate.strip_prefix('P') {
                if let Ok(state_num) = state_str.parse::<f64>() {
                    builder
                        .help(
                            "all_smi_gpu_performance_state",
                            "GPU performance state (P0=0, P1=1, ...)",
                        )
                        .type_("all_smi_gpu_performance_state", "gauge")
                        .metric("all_smi_gpu_performance_state", &base_labels, state_num);
                }
            }
        }
    }
}

// Allow for Rust 1.85 compatibility (Launchpad)
#[allow(clippy::needless_lifetimes)]
impl<'a> MetricExporter for GpuMetricExporter<'a> {
    fn export_metrics(&self) -> String {
        let mut builder = MetricBuilder::new();

        for (i, info) in self.gpu_info.iter().enumerate() {
            // Export metrics for GPU, NPU, and TPU devices
            if info.device_type == "GPU" || info.device_type == "NPU" || info.device_type == "TPU" {
                self.export_basic_metrics(&mut builder, info, i);
                self.export_apple_silicon_metrics(&mut builder, info, i);
                self.export_device_info(&mut builder, info, i);
                self.export_cuda_metrics(&mut builder, info, i);
            }
        }

        builder.build()
    }
}
