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
use crate::parsing::common::{sanitize_label_name, sanitize_label_value};

pub struct GpuMetricExporter<'a> {
    pub gpu_info: &'a [GpuInfo],
}

impl<'a> GpuMetricExporter<'a> {
    pub fn new(gpu_info: &'a [GpuInfo]) -> Self {
        Self { gpu_info }
    }

    fn export_basic_metrics(&self, builder: &mut MetricBuilder, row: &GpuRow<'a>) {
        let info = row.gpu;
        let base_labels = [
            ("gpu", info.name.as_str()),
            ("instance", info.instance.as_str()),
            ("gpu_uuid", info.uuid.as_str()),
            ("gpu_index", row.index_str.as_str()),
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

    fn export_apple_silicon_metrics(&self, builder: &mut MetricBuilder, row: &GpuRow<'a>) {
        let info = row.gpu;
        if !info.name.contains("Apple") && !info.name.contains("Metal") {
            return;
        }

        let base_labels = [
            ("gpu", info.name.as_str()),
            ("instance", info.instance.as_str()),
            ("gpu_uuid", info.uuid.as_str()),
            ("gpu_index", row.index_str.as_str()),
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
                ("gpu_uuid", info.uuid.as_str()),
                ("gpu_index", row.index_str.as_str()),
                ("level", thermal_level.as_str()),
            ];
            builder
                .help("all_smi_thermal_pressure_info", "Thermal pressure level")
                .type_("all_smi_thermal_pressure_info", "gauge")
                .metric("all_smi_thermal_pressure_info", &thermal_labels, 1);
        }

        // Combined power (CPU + GPU + ANE) for Apple Silicon
        if let Some(combined_power_str) = info.detail.get("combined_power_mw")
            && let Ok(combined_power_mw) = combined_power_str.parse::<f64>()
        {
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

    fn export_device_info(&self, builder: &mut MetricBuilder, row: &GpuRow<'a>) {
        let info = row.gpu;

        // Build label string with all detail fields
        let labels = [
            ("gpu", info.name.as_str()),
            ("instance", info.instance.as_str()),
            ("gpu_uuid", info.uuid.as_str()),
            ("gpu_index", row.index_str.as_str()),
            ("type", info.device_type.as_str()),
        ];

        // Convert detail HashMap to label pairs with sanitized names and values.
        // Values are sanitized to strip control characters and prevent
        // injection of ANSI escape sequences from NVML.
        let detail_labels: Vec<(String, String)> = info
            .detail
            .iter()
            .map(|(k, v)| (sanitize_label_name(k), sanitize_label_value(v)))
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

    fn export_cuda_metrics(&self, builder: &mut MetricBuilder, row: &GpuRow<'a>) {
        let info = row.gpu;
        let base_labels = [
            ("gpu", info.name.as_str()),
            ("instance", info.instance.as_str()),
            ("gpu_uuid", info.uuid.as_str()),
            ("gpu_index", row.index_str.as_str()),
        ];

        // PCIe metrics
        if let Some(pcie_gen) = info.detail.get("pcie_gen_current")
            && let Ok(pcie_gen_value) = pcie_gen.parse::<f64>()
        {
            builder
                .help("all_smi_gpu_pcie_gen_current", "Current PCIe generation")
                .type_("all_smi_gpu_pcie_gen_current", "gauge")
                .metric("all_smi_gpu_pcie_gen_current", &base_labels, pcie_gen_value);
        }

        if let Some(pcie_width) = info.detail.get("pcie_width_current")
            && let Ok(width) = pcie_width.parse::<f64>()
        {
            builder
                .help("all_smi_gpu_pcie_width_current", "Current PCIe link width")
                .type_("all_smi_gpu_pcie_width_current", "gauge")
                .metric("all_smi_gpu_pcie_width_current", &base_labels, width);
        }

        // Clock metrics
        if let Some(clock_max) = info.detail.get("clock_graphics_max")
            && let Ok(clock) = clock_max.parse::<f64>()
        {
            builder
                .help(
                    "all_smi_gpu_clock_graphics_max_mhz",
                    "Maximum graphics clock in MHz",
                )
                .type_("all_smi_gpu_clock_graphics_max_mhz", "gauge")
                .metric("all_smi_gpu_clock_graphics_max_mhz", &base_labels, clock);
        }

        if let Some(clock_max) = info.detail.get("clock_memory_max")
            && let Ok(clock) = clock_max.parse::<f64>()
        {
            builder
                .help(
                    "all_smi_gpu_clock_memory_max_mhz",
                    "Maximum memory clock in MHz",
                )
                .type_("all_smi_gpu_clock_memory_max_mhz", "gauge")
                .metric("all_smi_gpu_clock_memory_max_mhz", &base_labels, clock);
        }

        // Power limit metrics
        if let Some(power_limit) = info.detail.get("power_limit_current")
            && let Ok(power) = power_limit.parse::<f64>()
        {
            builder
                .help(
                    "all_smi_gpu_power_limit_current_watts",
                    "Current power limit in watts",
                )
                .type_("all_smi_gpu_power_limit_current_watts", "gauge")
                .metric("all_smi_gpu_power_limit_current_watts", &base_labels, power);
        }

        if let Some(power_limit) = info.detail.get("power_limit_max")
            && let Ok(power) = power_limit.parse::<f64>()
        {
            builder
                .help(
                    "all_smi_gpu_power_limit_max_watts",
                    "Maximum power limit in watts",
                )
                .type_("all_smi_gpu_power_limit_max_watts", "gauge")
                .metric("all_smi_gpu_power_limit_max_watts", &base_labels, power);
        }

        // Performance state — first prefer the structured per-device field
        // populated by the NVIDIA reader, fall back to the legacy
        // `detail.performance_state` string so mock servers and older
        // collectors keep working. When neither is available, the metric
        // is omitted entirely (Prometheus convention for "no data") so
        // dashboards can distinguish "unsupported" from P0 by absence
        // rather than relying on a sentinel value.
        if let Some(pstate) = info.performance_state {
            builder
                .help(
                    "all_smi_gpu_performance_state",
                    "GPU performance state (0=P0 fastest, 15=P15 idlest; metric is omitted when the device does not report a P-state)",
                )
                .type_("all_smi_gpu_performance_state", "gauge")
                .metric("all_smi_gpu_performance_state", &base_labels, pstate as f64);
        } else if let Some(pstate_str) = info.detail.get("performance_state")
            && let Some(state_str) = pstate_str.strip_prefix('P')
            && let Ok(state_num) = state_str.parse::<f64>()
        {
            builder
                .help(
                    "all_smi_gpu_performance_state",
                    "GPU performance state (0=P0 fastest, 15=P15 idlest; metric is omitted when the device does not report a P-state)",
                )
                .type_("all_smi_gpu_performance_state", "gauge")
                .metric("all_smi_gpu_performance_state", &base_labels, state_num);
        }
    }

    /// Export extended NVML temperature thresholds and the P-state gauge.
    ///
    /// Emitted only when the GPU populated any of the new fields — so
    /// Apple Silicon / AMD / Jetson rows produce no output and the metrics
    /// surface stays unchanged for them.
    ///
    /// Each metric carries the standard GPU label set (`gpu`, `instance`,
    /// `uuid`, `index`) so dashboards can correlate with the existing
    /// `all_smi_gpu_temperature_celsius` series by the same labels.
    fn export_thermal_thresholds(&self, builder: &mut MetricBuilder, row: &GpuRow<'a>) {
        let info = row.gpu;
        let base_labels = [
            ("gpu", info.name.as_str()),
            ("instance", info.instance.as_str()),
            ("gpu_uuid", info.uuid.as_str()),
            ("gpu_index", row.index_str.as_str()),
        ];

        if let Some(slowdown) = info.temperature_threshold_slowdown {
            builder
                .help(
                    "all_smi_gpu_temperature_threshold_slowdown_celsius",
                    "GPU slowdown temperature threshold in Celsius",
                )
                .type_(
                    "all_smi_gpu_temperature_threshold_slowdown_celsius",
                    "gauge",
                )
                .metric(
                    "all_smi_gpu_temperature_threshold_slowdown_celsius",
                    &base_labels,
                    slowdown,
                );
        }

        if let Some(shutdown) = info.temperature_threshold_shutdown {
            builder
                .help(
                    "all_smi_gpu_temperature_threshold_shutdown_celsius",
                    "GPU shutdown temperature threshold in Celsius",
                )
                .type_(
                    "all_smi_gpu_temperature_threshold_shutdown_celsius",
                    "gauge",
                )
                .metric(
                    "all_smi_gpu_temperature_threshold_shutdown_celsius",
                    &base_labels,
                    shutdown,
                );
        }

        if let Some(gpu_max) = info.temperature_threshold_max_operating {
            builder
                .help(
                    "all_smi_gpu_temperature_threshold_max_operating_celsius",
                    "GPU maximum operating temperature threshold in Celsius",
                )
                .type_(
                    "all_smi_gpu_temperature_threshold_max_operating_celsius",
                    "gauge",
                )
                .metric(
                    "all_smi_gpu_temperature_threshold_max_operating_celsius",
                    &base_labels,
                    gpu_max,
                );
        }

        if let Some(acoustic) = info.temperature_threshold_acoustic {
            builder
                .help(
                    "all_smi_gpu_temperature_threshold_acoustic_celsius",
                    "GPU acoustic (noise) temperature threshold in Celsius",
                )
                .type_(
                    "all_smi_gpu_temperature_threshold_acoustic_celsius",
                    "gauge",
                )
                .metric(
                    "all_smi_gpu_temperature_threshold_acoustic_celsius",
                    &base_labels,
                    acoustic,
                );
        }
    }

    /// Pre-compute the stringified `gpu_index` label for each eligible GPU.
    /// Allocates once per scrape regardless of how many metric families are
    /// later emitted, reducing per-family string allocations from 5*N to N.
    fn collect_rows(&self) -> Vec<GpuRow<'a>> {
        self.gpu_info
            .iter()
            .enumerate()
            .filter(|(_, info)| {
                info.device_type == "GPU"
                    || info.device_type == "NPU"
                    || info.device_type == "TPU"
            })
            .map(|(idx, gpu)| GpuRow {
                gpu,
                index_str: idx.to_string(),
            })
            .collect()
    }
}

/// Borrowed view of a single GPU row with its stringified index cached.
/// Exists purely to amortise the `.to_string()` on the index label across
/// the five export methods.
struct GpuRow<'a> {
    gpu: &'a GpuInfo,
    index_str: String,
}

impl<'a> MetricExporter for GpuMetricExporter<'a> {
    fn export_metrics(&self) -> String {
        let rows = self.collect_rows();
        let mut builder = MetricBuilder::new();

        for row in &rows {
            self.export_basic_metrics(&mut builder, row);
            self.export_apple_silicon_metrics(&mut builder, row);
            self.export_device_info(&mut builder, row);
            self.export_cuda_metrics(&mut builder, row);
            self.export_thermal_thresholds(&mut builder, row);
        }

        builder.build()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn make_nvidia_gpu() -> GpuInfo {
        GpuInfo {
            uuid: "GPU-ABC".to_string(),
            time: String::new(),
            name: "NVIDIA A100".to_string(),
            device_type: "GPU".to_string(),
            host_id: "node-1".to_string(),
            hostname: "node-1".to_string(),
            instance: "node-1".to_string(),
            utilization: 50.0,
            ane_utilization: 0.0,
            dla_utilization: None,
            tensorcore_utilization: None,
            temperature: 70,
            used_memory: 1024,
            total_memory: 8192,
            frequency: 1500,
            power_consumption: 200.0,
            gpu_core_count: None,
            temperature_threshold_slowdown: Some(90),
            temperature_threshold_shutdown: Some(95),
            temperature_threshold_max_operating: Some(85),
            temperature_threshold_acoustic: Some(77),
            performance_state: Some(2),
            numa_node_id: None,
            gsp_firmware_mode: None,
            gsp_firmware_version: None,
            nvlink_remote_devices: Vec::new(),
            gpm_metrics: None,
            detail: HashMap::new(),
        }
    }

    #[test]
    fn exporter_emits_all_new_threshold_metrics() {
        let gpu = make_nvidia_gpu();
        let gpus = vec![gpu];
        let exporter = GpuMetricExporter::new(&gpus);
        let output = exporter.export_metrics();

        assert!(
            output.contains("all_smi_gpu_temperature_threshold_slowdown_celsius{"),
            "slowdown metric missing:\n{output}"
        );
        assert!(
            output.contains("all_smi_gpu_temperature_threshold_shutdown_celsius{"),
            "shutdown metric missing:\n{output}"
        );
        assert!(
            output.contains("all_smi_gpu_temperature_threshold_max_operating_celsius{"),
            "max_operating metric missing:\n{output}"
        );
        assert!(
            output.contains("all_smi_gpu_temperature_threshold_acoustic_celsius{"),
            "acoustic metric missing:\n{output}"
        );
    }

    #[test]
    fn exporter_emits_pstate_from_structured_field() {
        let gpu = make_nvidia_gpu();
        let gpus = vec![gpu];
        let output = GpuMetricExporter::new(&gpus).export_metrics();
        // Structured field wins over the legacy detail-map path.
        assert!(
            output.contains("all_smi_gpu_performance_state{"),
            "pstate metric missing:\n{output}"
        );
        // Make sure the value is the structured `2`, not a truncated value.
        let pstate_line = output
            .lines()
            .find(|l| l.starts_with("all_smi_gpu_performance_state{"))
            .expect("pstate line");
        assert!(
            pstate_line.ends_with(" 2"),
            "expected P2, got {pstate_line}"
        );
    }

    #[test]
    fn exporter_skips_thresholds_when_none_present() {
        let mut gpu = make_nvidia_gpu();
        gpu.temperature_threshold_slowdown = None;
        gpu.temperature_threshold_shutdown = None;
        gpu.temperature_threshold_max_operating = None;
        gpu.temperature_threshold_acoustic = None;
        gpu.performance_state = None;
        let gpus = vec![gpu];
        let output = GpuMetricExporter::new(&gpus).export_metrics();
        assert!(
            !output.contains("all_smi_gpu_temperature_threshold_"),
            "should not emit threshold metrics without data:\n{output}"
        );
        assert!(
            !output.contains("all_smi_gpu_performance_state{"),
            "should not emit pstate metric without data:\n{output}"
        );
    }

    #[test]
    fn exporter_emits_only_available_thresholds() {
        // Older drivers: slowdown + shutdown known, others absent.
        let mut gpu = make_nvidia_gpu();
        gpu.temperature_threshold_max_operating = None;
        gpu.temperature_threshold_acoustic = None;
        let gpus = vec![gpu];
        let output = GpuMetricExporter::new(&gpus).export_metrics();
        assert!(output.contains("all_smi_gpu_temperature_threshold_slowdown_celsius"));
        assert!(output.contains("all_smi_gpu_temperature_threshold_shutdown_celsius"));
        assert!(!output.contains("all_smi_gpu_temperature_threshold_max_operating_celsius"));
        assert!(!output.contains("all_smi_gpu_temperature_threshold_acoustic_celsius"));
    }

    #[test]
    fn exporter_preserves_standard_gpu_labels_on_new_metrics() {
        let gpu = make_nvidia_gpu();
        let gpus = vec![gpu];
        let output = GpuMetricExporter::new(&gpus).export_metrics();
        // Sanity-check label set on the new metrics — should match the
        // legacy `all_smi_gpu_temperature_celsius` labels exactly.
        let slowdown_line = output
            .lines()
            .find(|l| l.starts_with("all_smi_gpu_temperature_threshold_slowdown_celsius{"))
            .expect("slowdown line");
        assert!(slowdown_line.contains("gpu=\"NVIDIA A100\""));
        assert!(slowdown_line.contains("instance=\"node-1\""));
        assert!(slowdown_line.contains("gpu_uuid=\"GPU-ABC\""));
        assert!(slowdown_line.contains("gpu_index=\"0\""));
    }
}
