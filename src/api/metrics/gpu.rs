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
use crate::device::readers::detail_keys;
use crate::device::readers::detail_keys::FAN_SPEED_DETAIL_KEY;
use crate::device::types::MAX_GPU_FAN_RPM;
use crate::parsing::common::{sanitize_label_name, sanitize_label_value};

/// Recover an RPM reading from the legacy `Fan Speed` detail string.
///
/// Shared with `network::metrics_parser` so the exporter and the remote
/// parser cannot disagree about the format, the same way
/// `ProcessMetricExporter::parse_start_time_seconds_public` is shared with
/// `ParsedProcessRow::from_local_process`.
///
/// Readers spell the value `"1450 RPM"`, and the Level Zero reader appends a
/// duty cycle when it has one (`"1600 RPM (40%)"`), so the number is parsed
/// from the text before the ` RPM` marker rather than by stripping a suffix.
/// A duty-cycle-only value (`"40%"`) carries no tachometer reading and
/// returns `None`, as does anything non-numeric or negative.
///
/// The result is also bounded by [`MAX_GPU_FAN_RPM`] and rejected when
/// fractional, the same checks `network::metrics_parser` applies to the
/// structured `all_smi_gpu_fan_speed_rpm` metric. Enforcing them here,
/// rather than leaving each caller to repeat them, is what keeps this
/// function's two call sites (the exporter's own detail fallback below and
/// the parser's legacy-node recovery) from disagreeing about what counts as
/// a valid reading: a fractional or out-of-range detail string (a garbled
/// reader value, or a hostile snapshot) can no longer be exported here only
/// for every downstream parser to silently drop it.
pub(crate) fn parse_fan_speed_detail(value: &str) -> Option<f64> {
    let (rpm, _) = value.split_once(" RPM")?;
    let rpm = rpm.trim().parse::<f64>().ok()?;
    (rpm.is_finite() && rpm.fract() == 0.0 && (0.0..=f64::from(MAX_GPU_FAN_RPM)).contains(&rpm))
        .then_some(rpm)
}

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

        // GPU utilization.
        //
        // Omitted when the reader had no source for it, following the same
        // Prometheus "absence means no data" convention this exporter already
        // applies to `all_smi_gpu_performance_state` and the thermal
        // thresholds below. Emitting `0` instead would be indistinguishable
        // from a genuinely idle GPU (issue #325); `all_smi_gpu_info` is still
        // emitted for the device, so a consumer can tell "device present but
        // not reporting" from "device gone" and, on Apple Silicon, read the
        // reason off the `native_metrics` label.
        if let Some(utilization) = info.utilization_reading() {
            builder
                .help(
                    "all_smi_gpu_utilization",
                    "GPU utilization percentage (omitted when the device reports no utilization)",
                )
                .type_("all_smi_gpu_utilization", "gauge")
                .metric("all_smi_gpu_utilization", &base_labels, utilization);
        }

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

        // Temperature. Omitted when no sensor answered — a powered die never
        // reads 0 °C, so the old unconditional `0` made a missing SMC/NVML
        // key look like a cryogenic GPU.
        if let Some(temperature) = info.temperature_reading() {
            builder
                .help(
                    "all_smi_gpu_temperature_celsius",
                    "GPU temperature in celsius (omitted when no sensor reports one)",
                )
                .type_("all_smi_gpu_temperature_celsius", "gauge")
                .metric("all_smi_gpu_temperature_celsius", &base_labels, temperature);
        }

        // Power consumption. Omitted when the device exposes no power rail.
        if let Some(power) = info.power_consumption_reading() {
            builder
                .help(
                    "all_smi_gpu_power_consumption_watts",
                    "GPU power consumption in watts (omitted when the device reports no power)",
                )
                .type_("all_smi_gpu_power_consumption_watts", "gauge")
                .metric("all_smi_gpu_power_consumption_watts", &base_labels, power);
        }

        // Board power, repeated identically on every device of a multi-device
        // board. Carried in `detail` by the readers whose tool reports one
        // power figure per board (the four dies of a Rebellions ATOM Max
        // card), and read back through the same validator the TUI uses, so an
        // absent or unparsable value publishes neither a label nor a sample.
        //
        // It is a separate family rather than a label on `all_smi_gpu_info`
        // because it is a live reading: as a label it gave every die a new
        // label set, and therefore a new series, on nearly every scrape. The
        // `gpu_` prefix is load-bearing on the way back in, since the remote
        // parser routes only `gpu_`, `npu_`, `nvlink_` and `ane_utilization`
        // into the device accumulator.
        if let Some(card_watts) = detail_keys::card_power_watts(&info.detail) {
            builder
                .help(
                    "all_smi_gpu_card_power_watts",
                    "Power of the physical board this device sits on, in watts, for display only. The same board value is repeated on every device of the board, so summing this metric multiplies a board's real draw by its device count (issue #418); sum all_smi_gpu_power_consumption_watts instead, which counts each board exactly once.",
                )
                .type_("all_smi_gpu_card_power_watts", "gauge")
                .metric("all_smi_gpu_card_power_watts", &base_labels, card_watts);
        }

        // Frequency. Omitted when the platform has no clock probe. This also
        // covers the readers that have always reported a static `0` to mean
        // "no probe" (Rebellions, Intel Gaudi, AMD via WMI), which the TUI
        // already rendered as N/A; the exposition now agrees with it instead
        // of publishing a flat 0 MHz line.
        if let Some(frequency) = info.frequency_reading() {
            builder
                .help(
                    "all_smi_gpu_frequency_mhz",
                    "GPU frequency in MHz (omitted when the device exposes no clock probe)",
                )
                .type_("all_smi_gpu_frequency_mhz", "gauge")
                .metric("all_smi_gpu_frequency_mhz", &base_labels, frequency);
        }

        // ANE utilization (Apple Silicon). Non-Apple readers set this to a
        // literal 0.0 meaning "not applicable" and keep emitting it, so this
        // gate only fires for an Apple Silicon row with no live sample.
        if let Some(ane) = info.ane_utilization_reading() {
            builder
                .help(
                    "all_smi_ane_utilization",
                    "ANE utilization in mW (omitted when the native metrics source is unavailable)",
                )
                .type_("all_smi_ane_utilization", "gauge")
                .metric("all_smi_ane_utilization", &base_labels, ane);
        }

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

        // ANE power in watts. Same source as `all_smi_ane_utilization`, so
        // the two families appear and disappear together.
        if let Some(ane_mw) = info.ane_utilization_reading() {
            builder
                .help(
                    "all_smi_ane_power_watts",
                    "ANE power consumption in watts (omitted when the native metrics source is unavailable)",
                )
                .type_("all_smi_ane_power_watts", "gauge")
                .metric("all_smi_ane_power_watts", &base_labels, ane_mw / 1000.0);
        }

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
        //
        // Keys registered in `detail_keys::VOLATILE_DETAIL_KEYS` are dropped
        // first, before sanitizing, so this series carries device identity
        // only. A label whose value moves between polls would give the device
        // a new label set, and therefore a new Prometheus series, on nearly
        // every scrape; each registered key's reading has a series of its own
        // instead. This filters the *label set* and never `detail` itself, so
        // the TUI, the snapshot writer and every exporter that reads a
        // registered key out of `detail` (notably `all_smi_combined_power_watts`
        // above) are unaffected.
        let mut detail_labels: Vec<(String, String)> = info
            .detail
            .iter()
            .filter(|(key, _)| !detail_keys::is_volatile_detail_key(key))
            .map(|(k, v)| (sanitize_label_name(k), sanitize_label_value(v)))
            .collect();

        // `detail` is a `HashMap`, so its iteration order differs between the
        // maps two consecutive polls build. Prometheus reads a label set
        // rather than a line, so the order never affected series identity,
        // but it does make the exposition text differ from scrape to scrape,
        // which hides exactly the churn this filter removes: diffing two
        // scrapes could not tell a reordered line from a new series. Sorting
        // makes an unchanged device render byte-identically, so that diff is
        // a usable check.
        detail_labels.sort();

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

        // PCIe metrics. The keys are the shared writer's constants
        // (`detail_keys::insert_pcie_details` writes exactly these), so the
        // lookup cannot drift from what the NVIDIA and Linux AMD readers
        // write.
        if let Some(pcie_gen) = info.detail.get(detail_keys::PCIE_GEN_CURRENT_DETAIL_KEY)
            && let Ok(pcie_gen_value) = pcie_gen.parse::<f64>()
        {
            builder
                .help("all_smi_gpu_pcie_gen_current", "Current PCIe generation")
                .type_("all_smi_gpu_pcie_gen_current", "gauge")
                .metric("all_smi_gpu_pcie_gen_current", &base_labels, pcie_gen_value);
        }

        if let Some(pcie_width) = info.detail.get(detail_keys::PCIE_WIDTH_CURRENT_DETAIL_KEY)
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

        // Current memory clock, same shape as the maximum above: the Linux
        // AMD plugin writes the live reading under
        // `detail_keys::CLOCK_MEMORY_CURRENT_DETAIL_KEY` on every poll, and
        // it travels as this gauge (registered as a volatile detail key, so
        // it never becomes an `all_smi_gpu_info` label). Omitted when the
        // key is absent or unparsable, matching the "absence means no data"
        // convention above.
        if let Some(clock_current) = info
            .detail
            .get(detail_keys::CLOCK_MEMORY_CURRENT_DETAIL_KEY)
            && let Ok(clock) = clock_current.parse::<f64>()
        {
            builder
                .help(
                    "all_smi_gpu_clock_memory_current_mhz",
                    "Current memory clock in MHz",
                )
                .type_("all_smi_gpu_clock_memory_current_mhz", "gauge")
                .metric("all_smi_gpu_clock_memory_current_mhz", &base_labels, clock);
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

        // Fan speed, same shape as the P-state block above: prefer the
        // structured `fan_speed_rpm` field the AMD / Intel readers now
        // populate, fall back to the legacy `Fan Speed` detail string. The
        // fallback covers any `GpuInfo` that carries the legacy string
        // without the typed field, such as one deserialized from a snapshot
        // recorded before the field existed. A remote node running an older
        // build is handled upstream in `network::metrics_parser` instead:
        // this exporter only ever runs over locally read `GpuInfo`, and the
        // remote node's reading arrives as the `fan_speed` label on
        // `all_smi_gpu_info`, so it never reaches this branch. Omitted
        // entirely when neither is available, so a passively cooled card is
        // distinguishable from a stalled fan by absence rather than by a 0
        // reading.
        if let Some(rpm) = info.fan_speed_rpm {
            builder
                .help(
                    "all_smi_gpu_fan_speed_rpm",
                    "GPU fan speed in revolutions per minute (metric is omitted when the device reports no tachometer)",
                )
                .type_("all_smi_gpu_fan_speed_rpm", "gauge")
                .metric("all_smi_gpu_fan_speed_rpm", &base_labels, rpm);
        } else if let Some(fan) = info.detail.get(FAN_SPEED_DETAIL_KEY)
            && let Some(rpm) = parse_fan_speed_detail(fan)
        {
            builder
                .help(
                    "all_smi_gpu_fan_speed_rpm",
                    "GPU fan speed in revolutions per minute (metric is omitted when the device reports no tachometer)",
                )
                .type_("all_smi_gpu_fan_speed_rpm", "gauge")
                .metric("all_smi_gpu_fan_speed_rpm", &base_labels, rpm);
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
                info.device_type == "GPU" || info.device_type == "NPU" || info.device_type == "TPU"
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
            fan_speed_rpm: None,
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
    fn exporter_sanitizes_dynamic_detail_label_names() {
        let mut gpu = make_nvidia_gpu();
        gpu.detail
            .insert("Source: Fan".to_string(), "hwmon".to_string());
        gpu.detail
            .insert("3D Engine".to_string(), "busy".to_string());
        gpu.detail
            .insert("GPU.Temp/Limit".to_string(), "90".to_string());
        let gpus = vec![gpu];
        let output = GpuMetricExporter::new(&gpus).export_metrics();
        let info_line = output
            .lines()
            .find(|line| line.starts_with("all_smi_gpu_info{"))
            .expect("GPU info metric");

        assert!(info_line.contains("source__fan=\"hwmon\""));
        assert!(info_line.contains("_3d_engine=\"busy\""));
        assert!(info_line.contains("gpu_temp_limit=\"90\""));
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
    fn fan_speed_detail_parser_accepts_every_reader_value_shape() {
        // `amd.rs`, `intel_gpu_linux`, and `amd_adl` all write "<rpm> RPM";
        // the Level Zero reader appends the duty cycle when it has one.
        assert_eq!(parse_fan_speed_detail("1450 RPM"), Some(1450.0));
        assert_eq!(parse_fan_speed_detail("1600 RPM (40%)"), Some(1600.0));
        // Duty cycle only: no tachometer reading to publish.
        assert_eq!(parse_fan_speed_detail("40%"), None);
        // Garbage and out-of-domain values must not reach the exposition.
        assert_eq!(parse_fan_speed_detail("unknown RPM"), None);
        assert_eq!(parse_fan_speed_detail("-1 RPM"), None);
        assert_eq!(parse_fan_speed_detail(""), None);
    }

    #[test]
    fn fan_speed_detail_parser_rejects_fractional_and_out_of_range_values() {
        // A fractional value would be exported as e.g. "1450.5" on the
        // wire, which every all-smi parser then silently drops because it
        // requires an integer. Rejecting it here, at the one function both
        // the exporter and `network::metrics_parser` call, means a garbled
        // detail string can no longer be published only for downstream
        // consumers to lose it. An out-of-range value is the reader-side
        // clamp's failure mode (a corrupted sysfs/PMLog/Sysman read) and
        // must be rejected the same way.
        assert_eq!(parse_fan_speed_detail("1450.5 RPM"), None);
        assert_eq!(parse_fan_speed_detail("1600.25 RPM (40%)"), None);
        assert_eq!(parse_fan_speed_detail("4294967295 RPM"), None);
        assert_eq!(
            parse_fan_speed_detail(&format!("{} RPM", MAX_GPU_FAN_RPM as u64 + 1)),
            None
        );
        // The bound is inclusive.
        assert_eq!(
            parse_fan_speed_detail(&format!("{MAX_GPU_FAN_RPM} RPM")),
            Some(f64::from(MAX_GPU_FAN_RPM))
        );
    }

    #[test]
    fn exporter_emits_fan_speed_from_structured_field() {
        let mut gpu = make_nvidia_gpu();
        gpu.fan_speed_rpm = Some(1450);
        let output = GpuMetricExporter::new(&[gpu]).export_metrics();

        let line = output
            .lines()
            .find(|l| l.starts_with("all_smi_gpu_fan_speed_rpm{"))
            .unwrap_or_else(|| panic!("fan speed metric missing:\n{output}"));
        assert!(line.ends_with(" 1450"), "expected 1450 RPM, got {line}");
        // Same label set as the other per-device gauges so dashboards can
        // join on it.
        assert!(line.contains("gpu=\"NVIDIA A100\""));
        assert!(line.contains("instance=\"node-1\""));
        assert!(line.contains("gpu_uuid=\"GPU-ABC\""));
        assert!(line.contains("gpu_index=\"0\""));
    }

    #[test]
    fn exporter_omits_fan_speed_when_device_reports_none() {
        // A passively cooled card has no fan at all. Absence, not 0, is how
        // that is expressed on the wire.
        let gpu = make_nvidia_gpu();
        assert!(gpu.fan_speed_rpm.is_none());
        let output = GpuMetricExporter::new(&[gpu]).export_metrics();
        assert!(
            !output.contains("all_smi_gpu_fan_speed_rpm"),
            "fan speed must be omitted without data:\n{output}"
        );
    }

    #[test]
    fn exporter_emits_fan_speed_from_detail_fallback() {
        // Mock servers and remote nodes running a build that predates the
        // typed field only carry the legacy detail string.
        let mut gpu = make_nvidia_gpu();
        gpu.fan_speed_rpm = None;
        gpu.detail
            .insert("Fan Speed".to_string(), "1450 RPM".to_string());
        let output = GpuMetricExporter::new(&[gpu]).export_metrics();

        let line = output
            .lines()
            .find(|l| l.starts_with("all_smi_gpu_fan_speed_rpm{"))
            .unwrap_or_else(|| panic!("fan speed fallback missing:\n{output}"));
        assert!(line.ends_with(" 1450"), "expected 1450 RPM, got {line}");
    }

    #[test]
    fn exporter_prefers_structured_fan_speed_over_the_detail_string() {
        let mut gpu = make_nvidia_gpu();
        gpu.fan_speed_rpm = Some(1450);
        gpu.detail
            .insert("Fan Speed".to_string(), "9999 RPM".to_string());
        let output = GpuMetricExporter::new(&[gpu]).export_metrics();

        let lines: Vec<&str> = output
            .lines()
            .filter(|l| l.starts_with("all_smi_gpu_fan_speed_rpm{"))
            .collect();
        assert_eq!(lines.len(), 1, "exactly one sample per device: {lines:?}");
        assert!(lines[0].ends_with(" 1450"), "{}", lines[0]);
    }

    #[test]
    fn exporter_omits_fan_speed_for_a_duty_cycle_only_detail() {
        // The Level Zero reader writes a bare percentage when the driver
        // exposes no tachometer. That is not an RPM and must not be
        // published as one.
        let mut gpu = make_nvidia_gpu();
        gpu.fan_speed_rpm = None;
        gpu.detail
            .insert("Fan Speed".to_string(), "40%".to_string());
        let output = GpuMetricExporter::new(&[gpu]).export_metrics();
        assert!(
            !output.contains("all_smi_gpu_fan_speed_rpm"),
            "a duty cycle is not an RPM reading:\n{output}"
        );
    }

    #[test]
    fn exporter_omits_fan_speed_for_a_fractional_detail_value() {
        // A snapshot recorded before the typed field existed, or a
        // hand-edited one, can carry a fractional `Fan Speed` string. Every
        // all-smi parser rejects a fractional
        // `all_smi_gpu_fan_speed_rpm` reading, so the exporter must not
        // publish one from the fallback either. Otherwise the two paths
        // would disagree, with this side happily emitting a value the
        // other side always drops.
        let mut gpu = make_nvidia_gpu();
        gpu.fan_speed_rpm = None;
        gpu.detail
            .insert("Fan Speed".to_string(), "1450.5 RPM".to_string());
        let output = GpuMetricExporter::new(&[gpu]).export_metrics();
        assert!(
            !output.contains("all_smi_gpu_fan_speed_rpm"),
            "a fractional detail value must not reach the exposition:\n{output}"
        );
    }

    #[test]
    fn exporter_omits_fan_speed_for_an_out_of_range_detail_value() {
        // Mirrors the reader-side clamp's failure mode: a corrupted sensor
        // read that still parses as a huge integer must not be published,
        // the same way `network::metrics_parser` rejects it on the way
        // back in.
        let mut gpu = make_nvidia_gpu();
        gpu.fan_speed_rpm = None;
        gpu.detail
            .insert("Fan Speed".to_string(), "4294967295 RPM".to_string());
        let output = GpuMetricExporter::new(&[gpu]).export_metrics();
        assert!(
            !output.contains("all_smi_gpu_fan_speed_rpm"),
            "an out-of-range detail value must not reach the exposition:\n{output}"
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

    /// Issue #325: an Apple Silicon row whose native metrics manager never
    /// initialized must omit the value series rather than publish zeros.
    #[test]
    fn exporter_omits_series_with_no_reading() {
        use crate::device::types::GPU_METRIC_UNAVAILABLE;

        let mut gpu = make_nvidia_gpu();
        gpu.name = "Apple M2 Max GPU".to_string();
        gpu.utilization = GPU_METRIC_UNAVAILABLE;
        gpu.power_consumption = GPU_METRIC_UNAVAILABLE;
        gpu.ane_utilization = GPU_METRIC_UNAVAILABLE;
        gpu.temperature = 0;
        gpu.frequency = 0;
        gpu.detail
            .insert("native_metrics".to_string(), "unavailable".to_string());

        let output = GpuMetricExporter::new(&[gpu]).export_metrics();

        for family in [
            "all_smi_gpu_utilization{",
            "all_smi_gpu_power_consumption_watts{",
            "all_smi_gpu_temperature_celsius{",
            "all_smi_gpu_frequency_mhz{",
            "all_smi_ane_utilization{",
            "all_smi_ane_power_watts{",
        ] {
            assert!(
                !output.contains(family),
                "{family} must be omitted, not emitted as 0:\n{output}"
            );
        }

        // The sentinel must never appear on the wire in any form.
        assert!(
            !output.contains(" -1"),
            "the in-band unavailable encoding leaked into the exposition:\n{output}"
        );

        // The device must still be discoverable, and must say why.
        let info_line = output
            .lines()
            .find(|l| l.starts_with("all_smi_gpu_info{"))
            .expect("identity series must survive");
        assert!(
            info_line.contains("native_metrics=\"unavailable\""),
            "{info_line}"
        );
        // Memory comes from sysinfo, not IOReport, so it keeps reporting.
        assert!(output.contains("all_smi_gpu_memory_total_bytes{"));
    }

    /// Issue #378: a Windows WMI baseline must keep an unsourced field
    /// absent through Prometheus instead of turning it into a zero sample.
    #[test]
    fn exporter_omits_unsourced_windows_utilization_and_power() {
        use crate::device::types::GPU_METRIC_UNAVAILABLE;

        let mut gpu = make_nvidia_gpu();
        gpu.name = "AMD Radeon Graphics".to_string();
        gpu.utilization = GPU_METRIC_UNAVAILABLE;
        gpu.power_consumption = GPU_METRIC_UNAVAILABLE;
        gpu.detail
            .insert("Source: Utilization".to_string(), "unavailable".to_string());
        gpu.detail
            .insert("Source: Power".to_string(), "unavailable".to_string());

        let output = GpuMetricExporter::new(&[gpu]).export_metrics();

        assert!(!output.contains("all_smi_gpu_utilization{"), "{output}");
        assert!(
            !output.contains("all_smi_gpu_power_consumption_watts{"),
            "{output}"
        );
        assert!(output.contains("all_smi_gpu_info{"), "{output}");
        assert!(output.contains("source__utilization=\"unavailable\""));
        assert!(output.contains("source__power=\"unavailable\""));
    }

    /// The other half of the contract: a real zero is still published, so
    /// omission unambiguously means "no data".
    #[test]
    fn exporter_emits_genuine_zero_readings() {
        let mut gpu = make_nvidia_gpu();
        gpu.utilization = 0.0;
        gpu.power_consumption = 0.0;
        gpu.temperature = 42;
        gpu.frequency = 300;

        let output = GpuMetricExporter::new(&[gpu]).export_metrics();

        let util = output
            .lines()
            .find(|l| l.starts_with("all_smi_gpu_utilization{"))
            .expect("idle GPU must still report 0% utilization");
        assert!(util.ends_with(" 0"), "expected a zero reading, got {util}");

        let power = output
            .lines()
            .find(|l| l.starts_with("all_smi_gpu_power_consumption_watts{"))
            .expect("a 0 W rail is a reading");
        assert!(power.ends_with(" 0"), "{power}");
    }

    /// Non-Apple readers set `ane_utilization` to a literal 0.0 meaning "not
    /// applicable". That has always been published and must keep being
    /// published, so this change does not silently alter NVIDIA scrapes.
    #[test]
    fn exporter_keeps_emitting_zero_ane_for_non_apple_gpus() {
        let output = GpuMetricExporter::new(&[make_nvidia_gpu()]).export_metrics();
        assert!(output.contains("all_smi_ane_utilization{"));
        // `all_smi_ane_power_watts` stays Apple-only, as before.
        assert!(!output.contains("all_smi_ane_power_watts{"));
    }

    /// The `all_smi_gpu_info` line for a device whose `detail` holds
    /// `entries`, rendered through the real exporter.
    fn identity_line(name: &str, entries: &[(&str, &str)]) -> String {
        let mut gpu = make_nvidia_gpu();
        gpu.name = name.to_string();
        for (key, value) in entries {
            gpu.detail.insert((*key).to_string(), (*value).to_string());
        }
        GpuMetricExporter::new(&[gpu])
            .export_metrics()
            .lines()
            .find(|line| line.starts_with("all_smi_gpu_info{"))
            .unwrap_or_else(|| panic!("identity series missing for {name}"))
            .to_string()
    }

    /// The same line for a device whose `detail` is a map, as the shared
    /// writers build them. Sorting keeps the rendering independent of the
    /// map's iteration order, as the exporter itself does.
    fn identity_line_map(name: &str, detail: &HashMap<String, String>) -> String {
        let mut entries: Vec<(String, String)> =
            detail.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        entries.sort();
        let refs: Vec<(&str, &str)> = entries
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        identity_line(name, &refs)
    }

    /// Issue #425: `all_smi_gpu_info` identifies a device, so its label set
    /// must not move when a reading does. Every registered key gets its own
    /// assertion, naming the key, so reverting the filter for one key fails
    /// on that key instead of hiding behind a neighbour.
    #[test]
    fn no_registered_volatile_key_reaches_the_identity_label_set() {
        for key in detail_keys::VOLATILE_DETAIL_KEYS {
            let first = identity_line("NVIDIA A100", &[(key, "1")]);
            let second = identity_line("NVIDIA A100", &[(key, "2")]);
            assert_eq!(
                first, second,
                "{key} changed the identity label set between polls"
            );
            let label = format!("{}=\"", sanitize_label_name(key));
            assert!(
                !first.contains(&label),
                "{key} reached the identity series as {label}: {first}"
            );
        }
    }

    /// The Tenstorrent half of the label-stability criterion, hand-built so
    /// it runs on every platform: the reader itself is Linux-only, but the
    /// export path it feeds is not.
    #[test]
    fn a_tenstorrent_shaped_device_keeps_one_identity_series_across_polls() {
        let stable: &[(&str, &str)] = &[
            ("board_type", "n300"),
            ("arc_fw_version", "2.28.0.0"),
            ("lib_name", "Luwen"),
        ];
        let poll = |voltage, current, asic, vreg, inlet, ai, arc, axi| {
            let mut entries = stable.to_vec();
            entries.extend_from_slice(&[
                ("voltage", voltage),
                ("current", current),
                ("asic_temperature", asic),
                ("vreg_temperature", vreg),
                ("inlet_temperature", inlet),
                ("aiclk_mhz", ai),
                ("arcclk_mhz", arc),
                ("axiclk_mhz", axi),
            ]);
            identity_line("Tenstorrent Wormhole n300", &entries)
        };

        let first = poll(
            "0.800", "12.34", "45.0", "45.0", "32.0", "800", "540", "900",
        );
        let second = poll(
            "0.812", "13.01", "47.5", "46.2", "33.0", "1000", "540", "900",
        );
        assert_eq!(first, second, "a Tenstorrent poll moved the label set");

        // The identity labels are still there: this filters readings, not
        // the series.
        assert!(first.contains("board_type=\"n300\""), "{first}");
        assert!(first.contains("arc_fw_version=\"2.28.0.0\""), "{first}");
        assert!(first.contains("lib_name=\"Luwen\""), "{first}");
    }

    /// Issue #433: the two PCIe current gauges read exactly the keys the
    /// shared `detail_keys::insert_pcie_details` writer produces, so the
    /// map is sourced from the helper and the test fails when either end
    /// drifts. A test that inserted the keys by hand would pass on current
    /// `main` and prove nothing.
    #[test]
    fn exporter_emits_pcie_current_gauges_from_the_shared_writer() {
        let mut gpu = make_nvidia_gpu();
        gpu.name = "NVIDIA H100 80GB HBM3".to_string();
        detail_keys::insert_pcie_details(&mut gpu.detail, Some(4), Some(16), Some(5), Some(16));

        let output = GpuMetricExporter::new(&[gpu]).export_metrics();

        let gen_gauge = output
            .lines()
            .find(|l| l.starts_with("all_smi_gpu_pcie_gen_current{"))
            .unwrap_or_else(|| panic!("gen gauge missing:\n{output}"));
        assert!(
            gen_gauge.ends_with(" 4"),
            "expected a bare 4 sample, got {gen_gauge}"
        );
        let width_gauge = output
            .lines()
            .find(|l| l.starts_with("all_smi_gpu_pcie_width_current{"))
            .unwrap_or_else(|| panic!("width gauge missing:\n{output}"));
        assert!(
            width_gauge.ends_with(" 16"),
            "expected a bare 16 sample, got {width_gauge}"
        );
        for gauge in [gen_gauge, width_gauge] {
            for label in ["gpu=\"", "instance=\"", "gpu_uuid=\"", "gpu_index=\""] {
                assert!(gauge.contains(label), "{label} missing from {gauge}");
            }
        }

        // The gauges carry the current readings; the identity series keeps
        // the maximums and carries none of the current pair, which moved to
        // the gauges.
        let info_line = output
            .lines()
            .find(|l| l.starts_with("all_smi_gpu_info{"))
            .unwrap_or_else(|| panic!("identity series missing:\n{output}"));
        for stale in [
            "pcie_generation=\"",
            "pcie_width=\"",
            "pcie_gen_current=\"",
            "pcie_width_current=\"",
        ] {
            assert!(
                !info_line.contains(stale),
                "{stale} reached the identity series: {info_line}"
            );
        }
        assert!(info_line.contains("pcie_gen_max=\"5\""), "{info_line}");
        assert!(info_line.contains("pcie_width_max=\"16\""), "{info_line}");
    }

    /// The AMD half of the label-stability criterion, hand-built so it runs
    /// on every platform: the plugin itself is Linux-only, but the export
    /// path it feeds is not. Each poll's map is built through the shared
    /// writers, so the test fails when either the reader's keys or the
    /// registry drifts.
    #[test]
    fn an_amd_shaped_device_keeps_one_identity_series_across_polls() {
        let poll = |link_gen: u32, fan_rpm: u32, mclk: &str| {
            let mut detail = HashMap::new();
            detail.insert("lib_name".to_string(), "ROCm".to_string());
            detail.insert("Max GPU Link".to_string(), "Gen4 x16".to_string());
            detail_keys::insert_pcie_details(&mut detail, Some(link_gen), Some(16), None, None);
            detail.insert("Fan Speed".to_string(), format!("{fan_rpm} RPM"));
            detail.insert(
                detail_keys::CLOCK_MEMORY_CURRENT_DETAIL_KEY.to_string(),
                mclk.to_string(),
            );
            identity_line_map("AMD Radeon RX 7900 XTX", &detail)
        };

        let first = poll(1, 0, "96");
        let second = poll(4, 1450, "1249");
        assert_eq!(first, second, "an AMD poll moved the label set");

        // The identity labels are still there.
        assert!(first.contains("lib_name=\"ROCm\""), "{first}");
        assert!(first.contains("max_gpu_link=\"Gen4 x16\""), "{first}");
        // And none of the per-poll readings reached the identity series.
        for stale in [
            "pcie_gen_current=\"",
            "pcie_width_current=\"",
            "fan_speed=\"",
            "clock_memory_current=\"",
            "current_link=\"",
            "memory_clock=\"",
        ] {
            assert!(
                !first.contains(stale),
                "{stale} reached the identity series: {first}"
            );
        }
    }

    /// The same stability contract for the keys this change newly registers,
    /// one assertion per key so a partial revert names the key it broke.
    #[test]
    fn apple_furiosa_gaudi_and_tpu_readings_stay_off_the_identity_series() {
        let polls: &[(&str, &str, &str)] = &[
            ("combined_power_mw", "12345.6", "9876.5"),
            ("cpu_temperature", "48.6", "51.2"),
            ("gpu_temperature", "46.2", "49.8"),
            ("frequency", "1500MHz", "1800MHz"),
            ("Current Power", "142.5 W", "301.0 W"),
            ("Used Memory", "1024 MiB", "2048 MiB"),
            ("HLO Queue Size", "3", "7"),
            ("HLO Exec Mean", "125.5 µs", "210.2 µs"),
            ("HLO Exec P50", "100.0 µs", "180.4 µs"),
            ("HLO Exec P90", "150.0 µs", "260.1 µs"),
            ("HLO Exec P95", "175.0 µs", "300.9 µs"),
            ("HLO Exec P99.9", "220.0 µs", "410.7 µs"),
        ];

        for (key, before, after) in polls {
            let first = identity_line("Apple M2 Max GPU", &[(key, before)]);
            let second = identity_line("Apple M2 Max GPU", &[(key, after)]);
            assert_eq!(first, second, "{key} moved the identity label set");
        }

        // And together. No single device carries all of these, but a real
        // one carries several at once (three on Apple Silicon, eight on a
        // Google TPU), which is what used to make a scrape start a new
        // series every few seconds. Filtering has to hold for the whole set,
        // not just one key at a time.
        let together = |values: [&str; 12]| {
            let entries: Vec<(&str, &str)> = polls
                .iter()
                .zip(values)
                .map(|((key, _, _), value)| (*key, value))
                .collect();
            identity_line("Apple M2 Max GPU", &entries)
        };
        assert_eq!(
            together([
                "12345.6",
                "48.6",
                "46.2",
                "1500MHz",
                "142.5 W",
                "1024 MiB",
                "3",
                "125.5 µs",
                "100.0 µs",
                "150.0 µs",
                "175.0 µs",
                "220.0 µs",
            ]),
            together([
                "9876.5",
                "51.2",
                "49.8",
                "1800MHz",
                "301.0 W",
                "2048 MiB",
                "7",
                "210.2 µs",
                "180.4 µs",
                "260.1 µs",
                "300.9 µs",
                "410.7 µs",
            ]),
        );
    }

    /// The regression that would otherwise break Apple Silicon silently:
    /// `all_smi_combined_power_watts` reads `combined_power_mw` straight out
    /// of `detail`, so the filter has to drop the *label* and leave `detail`
    /// alone.
    #[test]
    fn filtering_removes_labels_only_and_leaves_detail_readable() {
        let mut gpu = make_nvidia_gpu();
        gpu.name = "Apple M2 Max GPU".to_string();
        gpu.detail
            .insert("combined_power_mw".to_string(), "12345.6".to_string());

        let output = GpuMetricExporter::new(&[gpu]).export_metrics();

        let combined = output
            .lines()
            .find(|line| line.starts_with("all_smi_combined_power_watts{"))
            .unwrap_or_else(|| panic!("combined power lost to the filter:\n{output}"));
        let watts: f64 = combined
            .rsplit(' ')
            .next()
            .and_then(|value| value.parse().ok())
            .unwrap_or_else(|| panic!("combined power sample is not a number: {combined}"));
        assert!(
            (watts - 12.3456).abs() < 1e-9,
            "combined power must still be the detail reading, got {combined}"
        );

        let info = output
            .lines()
            .find(|line| line.starts_with("all_smi_gpu_info{"))
            .expect("identity series");
        assert!(
            !info.contains("combined_power_mw=\""),
            "the reading is still a label: {info}"
        );
    }

    /// Issue #425: the board power reaches Prometheus as its own family, with
    /// the same base label set as the power series it sits beside.
    #[test]
    fn card_power_is_published_as_its_own_gauge_not_as_a_label() {
        let mut gpu = make_nvidia_gpu();
        gpu.name = "RBLN-CA25".to_string();
        gpu.device_type = "NPU".to_string();
        gpu.detail.insert(
            detail_keys::CARD_POWER_WATTS_DETAIL_KEY.to_string(),
            "42.80".to_string(),
        );

        let output = GpuMetricExporter::new(&[gpu]).export_metrics();

        let line = output
            .lines()
            .find(|l| l.starts_with("all_smi_gpu_card_power_watts{"))
            .unwrap_or_else(|| panic!("card power gauge missing:\n{output}"));
        assert!(line.ends_with(" 42.8"), "{line}");
        assert!(line.contains("gpu=\"RBLN-CA25\""), "{line}");
        assert!(line.contains("instance=\"node-1\""), "{line}");
        assert!(line.contains("gpu_uuid=\"GPU-ABC\""), "{line}");
        assert!(line.contains("gpu_index=\"0\""), "{line}");

        // The HELP line has to carry the do-not-sum warning: summing this
        // family recreates the 4x overcount issue #418 fixed.
        let help = output
            .lines()
            .find(|l| l.starts_with("# HELP all_smi_gpu_card_power_watts"))
            .expect("HELP line");
        assert!(help.contains("sum"), "{help}");

        let info = output
            .lines()
            .find(|l| l.starts_with("all_smi_gpu_info{"))
            .expect("identity series");
        assert!(!info.contains("card_power_watts=\""), "{info}");
    }

    /// No new failure path: a device without the key, or with one that does
    /// not hold a finite non-negative number, publishes neither a label nor a
    /// sample rather than a fabricated 0 W board.
    #[test]
    fn card_power_gauge_is_omitted_without_a_usable_reading() {
        assert!(
            !GpuMetricExporter::new(&[make_nvidia_gpu()])
                .export_metrics()
                .contains("all_smi_gpu_card_power_watts"),
            "a device with no board value must publish no sample"
        );

        for rejected in ["-1", "NaN", "inf", "abc", ""] {
            let mut gpu = make_nvidia_gpu();
            gpu.detail.insert(
                detail_keys::CARD_POWER_WATTS_DETAIL_KEY.to_string(),
                rejected.to_string(),
            );
            let output = GpuMetricExporter::new(&[gpu]).export_metrics();
            assert!(
                !output.contains("all_smi_gpu_card_power_watts"),
                "{rejected:?} must not reach the exposition:\n{output}"
            );
        }
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
