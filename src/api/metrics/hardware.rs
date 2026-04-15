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

//! Prometheus exporter for NVIDIA hardware-detail metrics (issue #132).
//!
//! Metrics produced by this exporter:
//!
//! * `all_smi_gpu_numa_node_id` (gauge): NUMA node the GPU is attached to.
//!   Emitted only when the reader returned a concrete value — absence
//!   signals "unavailable" (no NUMA topology, driver too old, non-Linux
//!   platform). Dashboards MUST treat `absent` as "unknown" rather than
//!   defaulting to 0.
//! * `all_smi_gpu_gsp_firmware_mode` (gauge): `0=disabled`, `1=enabled`,
//!   `2=default`. Absent when unsupported.
//! * `all_smi_gpu_gsp_firmware_version_info` (gauge, label-only): info-style
//!   metric carrying the firmware version string in a `version` label with
//!   a constant value of 1. Absent when unsupported.
//! * `all_smi_nvlink_remote_device_type` (gauge, label-only): one row per
//!   active NvLink carrying `link_index` and `remote_type` labels with a
//!   constant value of 1. Empty when no NvLinks are active.
//! * `all_smi_gpu_sm_occupancy` (gauge, 0-1): fractional SM occupancy from
//!   the GPM API. Absent when GPM is not supported.
//! * `all_smi_gpu_memory_bandwidth_utilization` (gauge, 0-1): fractional
//!   DRAM bandwidth utilization from the GPM API. Absent when unsupported.
//!
//! All metrics carry the standard GPU label set (`gpu`, `instance`, `uuid`,
//! `index`), matching the core `all_smi_gpu_temperature_celsius` series so
//! dashboards can correlate by label.
//!
//! The exporter emits nothing when no GPUs populated any of the new fields
//! — non-NVIDIA paths, older NVIDIA drivers without the relevant APIs, and
//! hosts with zero NvLinks stay silent in the `/metrics` output.

use super::{MetricBuilder, MetricExporter};
use crate::device::GpuInfo;

/// Hardware-detail exporter.
///
/// Takes a borrowed slice of [`GpuInfo`] and emits the issue-#132 metric
/// families. Row caching (stringifying the device index once per GPU) is
/// done in [`HardwareMetricExporter::collect_rows`] so the four metric
/// families iterate without re-allocating labels.
pub struct HardwareMetricExporter<'a> {
    pub gpu_info: &'a [GpuInfo],
}

impl<'a> HardwareMetricExporter<'a> {
    pub fn new(gpu_info: &'a [GpuInfo]) -> Self {
        Self { gpu_info }
    }

    /// Pre-compute the stringified `index` label and only keep GPUs that
    /// contribute at least one hardware-detail row. Allocates once per
    /// scrape regardless of how many metric families are later emitted.
    fn collect_rows(&self) -> Vec<Row<'a>> {
        self.gpu_info
            .iter()
            .enumerate()
            .filter(|(_, gpu)| has_any_hw_detail(gpu))
            .map(|(idx, gpu)| Row {
                gpu,
                index_str: idx.to_string(),
            })
            .collect()
    }

    fn base_labels<'b>(row: &'b Row<'a>) -> [(&'b str, &'b str); 4] {
        [
            ("gpu", row.gpu.name.as_str()),
            ("instance", row.gpu.instance.as_str()),
            ("uuid", row.gpu.uuid.as_str()),
            ("index", row.index_str.as_str()),
        ]
    }

    fn export_numa_node_id(&self, builder: &mut MetricBuilder, rows: &[Row<'a>]) {
        // Emit HELP/TYPE unconditionally so the exposition format is well-
        // formed even when no GPU has NUMA data. This matches the
        // per-metric-family pattern used by the vGPU / MIG exporters.
        let mut emitted_any = false;
        for row in rows {
            if let Some(node_id) = row.gpu.numa_node_id {
                if !emitted_any {
                    builder
                        .help(
                            "all_smi_gpu_numa_node_id",
                            "NUMA node the GPU is attached to (metric is omitted when the host \
                             has no NUMA topology or the driver does not report one)",
                        )
                        .type_("all_smi_gpu_numa_node_id", "gauge");
                    emitted_any = true;
                }
                let labels = Self::base_labels(row);
                builder.metric("all_smi_gpu_numa_node_id", &labels, node_id);
            }
        }
    }

    fn export_gsp_firmware_mode(&self, builder: &mut MetricBuilder, rows: &[Row<'a>]) {
        let mut emitted_any = false;
        for row in rows {
            if let Some(mode) = row.gpu.gsp_firmware_mode {
                if !emitted_any {
                    builder
                        .help(
                            "all_smi_gpu_gsp_firmware_mode",
                            "GSP firmware mode (0=disabled, 1=enabled, 2=default); omitted when \
                             the driver does not expose the GSP firmware API",
                        )
                        .type_("all_smi_gpu_gsp_firmware_mode", "gauge");
                    emitted_any = true;
                }
                let labels = Self::base_labels(row);
                builder.metric("all_smi_gpu_gsp_firmware_mode", &labels, mode);
            }
        }
    }

    fn export_gsp_firmware_version(&self, builder: &mut MetricBuilder, rows: &[Row<'a>]) {
        let mut emitted_any = false;
        for row in rows {
            if let Some(ref version) = row.gpu.gsp_firmware_version {
                if !emitted_any {
                    builder
                        .help(
                            "all_smi_gpu_gsp_firmware_version_info",
                            "GSP firmware version, encoded as a constant 1 with the version in a \
                             `version` label; omitted when unsupported",
                        )
                        .type_("all_smi_gpu_gsp_firmware_version_info", "gauge");
                    emitted_any = true;
                }
                let base = Self::base_labels(row);
                let labels = [
                    base[0],
                    base[1],
                    base[2],
                    base[3],
                    ("version", version.as_str()),
                ];
                builder.metric("all_smi_gpu_gsp_firmware_version_info", &labels, 1);
            }
        }
    }

    fn export_nvlink_remote_device_type(&self, builder: &mut MetricBuilder, rows: &[Row<'a>]) {
        let mut emitted_any = false;
        for row in rows {
            for link in &row.gpu.nvlink_remote_devices {
                if !emitted_any {
                    builder
                        .help(
                            "all_smi_nvlink_remote_device_type",
                            "NvLink remote endpoint classification per active link. Value is \
                             always 1; classification is carried in the `remote_type` label \
                             (gpu / switch / ibmnpu / unknown).",
                        )
                        .type_("all_smi_nvlink_remote_device_type", "gauge");
                    emitted_any = true;
                }
                let link_idx_str = link.link_index.to_string();
                let base = Self::base_labels(row);
                let labels = [
                    base[0],
                    base[1],
                    base[2],
                    base[3],
                    ("link_index", link_idx_str.as_str()),
                    ("remote_type", link.remote_type.as_label()),
                ];
                builder.metric("all_smi_nvlink_remote_device_type", &labels, 1);
            }
        }
    }

    fn export_gpm_metrics(&self, builder: &mut MetricBuilder, rows: &[Row<'a>]) {
        // Only emit the gauges when the underlying reader surfaced a
        // numeric value. Presence of a `GpmMetrics` struct without any
        // populated field (the reader's "GPM-capable, not yet sampled"
        // state) should produce no output so dashboards can't confuse it
        // with a real zero reading.
        let mut emitted_sm_header = false;
        let mut emitted_mem_header = false;
        for row in rows {
            let Some(ref metrics) = row.gpu.gpm_metrics else {
                continue;
            };
            if let Some(sm) = metrics.sm_occupancy {
                if !emitted_sm_header {
                    builder
                        .help(
                            "all_smi_gpu_sm_occupancy",
                            "GPM-reported SM occupancy fraction (0.0-1.0); omitted on devices \
                             that do not support GPM (pre-Hopper)",
                        )
                        .type_("all_smi_gpu_sm_occupancy", "gauge");
                    emitted_sm_header = true;
                }
                let labels = Self::base_labels(row);
                builder.metric("all_smi_gpu_sm_occupancy", &labels, sm);
            }
            if let Some(mem) = metrics.memory_bandwidth_utilization {
                if !emitted_mem_header {
                    builder
                        .help(
                            "all_smi_gpu_memory_bandwidth_utilization",
                            "GPM-reported memory bandwidth utilization fraction (0.0-1.0); \
                             omitted on devices that do not support GPM (pre-Hopper)",
                        )
                        .type_("all_smi_gpu_memory_bandwidth_utilization", "gauge");
                    emitted_mem_header = true;
                }
                let labels = Self::base_labels(row);
                builder.metric("all_smi_gpu_memory_bandwidth_utilization", &labels, mem);
            }
        }
    }
}

/// Returns true when a GPU has at least one hardware-detail field
/// populated. Used by [`HardwareMetricExporter::collect_rows`] so we skip
/// non-NVIDIA rows entirely without iterating them in every metric family.
fn has_any_hw_detail(gpu: &GpuInfo) -> bool {
    gpu.numa_node_id.is_some()
        || gpu.gsp_firmware_mode.is_some()
        || gpu.gsp_firmware_version.is_some()
        || !gpu.nvlink_remote_devices.is_empty()
        || gpu.gpm_metrics.is_some()
}

/// Borrowed view of a single GPU row with its stringified index cached.
/// Exists purely to amortise the `.to_string()` on the index label across
/// the five metric families.
struct Row<'a> {
    gpu: &'a GpuInfo,
    index_str: String,
}

impl<'a> MetricExporter for HardwareMetricExporter<'a> {
    fn export_metrics(&self) -> String {
        let rows = self.collect_rows();
        if rows.is_empty() {
            return String::new();
        }

        let mut builder = MetricBuilder::new();
        self.export_numa_node_id(&mut builder, &rows);
        self.export_gsp_firmware_mode(&mut builder, &rows);
        self.export_gsp_firmware_version(&mut builder, &rows);
        self.export_nvlink_remote_device_type(&mut builder, &rows);
        self.export_gpm_metrics(&mut builder, &rows);
        builder.build()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::{GpmMetrics, NvLinkRemoteDevice, NvLinkRemoteType};
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
            utilization: 0.0,
            ane_utilization: 0.0,
            dla_utilization: None,
            tensorcore_utilization: None,
            temperature: 0,
            used_memory: 0,
            total_memory: 0,
            frequency: 0,
            power_consumption: 0.0,
            gpu_core_count: None,
            temperature_threshold_slowdown: None,
            temperature_threshold_shutdown: None,
            temperature_threshold_max_operating: None,
            temperature_threshold_acoustic: None,
            performance_state: None,
            numa_node_id: Some(0),
            gsp_firmware_mode: Some(1),
            gsp_firmware_version: Some("550.54.15".to_string()),
            nvlink_remote_devices: vec![
                NvLinkRemoteDevice {
                    link_index: 0,
                    remote_type: NvLinkRemoteType::Gpu,
                },
                NvLinkRemoteDevice {
                    link_index: 1,
                    remote_type: NvLinkRemoteType::Switch,
                },
            ],
            gpm_metrics: Some(GpmMetrics {
                sm_occupancy: Some(0.67),
                memory_bandwidth_utilization: Some(0.42),
            }),
            detail: HashMap::new(),
        }
    }

    fn make_non_nvidia_gpu() -> GpuInfo {
        let mut gpu = make_nvidia_gpu();
        gpu.uuid = "GPU-AMD".to_string();
        gpu.name = "Radeon RX".to_string();
        gpu.numa_node_id = None;
        gpu.gsp_firmware_mode = None;
        gpu.gsp_firmware_version = None;
        gpu.nvlink_remote_devices = Vec::new();
        gpu.gpm_metrics = None;
        gpu
    }

    #[test]
    fn empty_when_no_hardware_details_anywhere() {
        let gpus = vec![make_non_nvidia_gpu()];
        let exporter = HardwareMetricExporter::new(&gpus);
        assert_eq!(exporter.export_metrics(), "");
    }

    #[test]
    fn emits_numa_node_id_when_populated() {
        let gpus = vec![make_nvidia_gpu()];
        let out = HardwareMetricExporter::new(&gpus).export_metrics();
        assert!(out.contains("# HELP all_smi_gpu_numa_node_id"));
        assert!(out.contains("# TYPE all_smi_gpu_numa_node_id gauge"));
        // Pattern: `all_smi_gpu_numa_node_id{...} 0`
        assert!(
            out.lines()
                .any(|l| l.starts_with("all_smi_gpu_numa_node_id{") && l.ends_with(" 0")),
            "missing numa node id line in:\n{out}"
        );
    }

    #[test]
    fn omits_numa_node_id_when_none() {
        let mut gpu = make_nvidia_gpu();
        gpu.numa_node_id = None;
        let gpus = vec![gpu];
        let out = HardwareMetricExporter::new(&gpus).export_metrics();
        assert!(
            !out.contains("all_smi_gpu_numa_node_id"),
            "should omit numa metric entirely:\n{out}"
        );
    }

    #[test]
    fn emits_gsp_firmware_mode_and_version() {
        let gpus = vec![make_nvidia_gpu()];
        let out = HardwareMetricExporter::new(&gpus).export_metrics();
        assert!(out.contains("all_smi_gpu_gsp_firmware_mode{"));
        assert!(out.contains("all_smi_gpu_gsp_firmware_version_info{"));
        assert!(out.contains(r#"version="550.54.15""#));
    }

    #[test]
    fn emits_one_nvlink_row_per_active_link() {
        let gpus = vec![make_nvidia_gpu()];
        let out = HardwareMetricExporter::new(&gpus).export_metrics();
        let nvlink_lines: Vec<_> = out
            .lines()
            .filter(|l| l.starts_with("all_smi_nvlink_remote_device_type{"))
            .collect();
        assert_eq!(nvlink_lines.len(), 2, "expected 2 NvLink rows:\n{out}");
        assert!(
            nvlink_lines
                .iter()
                .any(|l| l.contains(r#"remote_type="gpu""#))
        );
        assert!(
            nvlink_lines
                .iter()
                .any(|l| l.contains(r#"remote_type="switch""#))
        );
        assert!(nvlink_lines.iter().any(|l| l.contains(r#"link_index="0""#)));
        assert!(nvlink_lines.iter().any(|l| l.contains(r#"link_index="1""#)));
    }

    #[test]
    fn emits_no_nvlink_metric_when_vector_empty() {
        let mut gpu = make_nvidia_gpu();
        gpu.nvlink_remote_devices.clear();
        // But keep NUMA so the row survives collect_rows filter.
        let gpus = vec![gpu];
        let out = HardwareMetricExporter::new(&gpus).export_metrics();
        assert!(
            !out.contains("all_smi_nvlink_remote_device_type"),
            "should omit NvLink metric entirely when no active links:\n{out}"
        );
    }

    #[test]
    fn emits_gpm_metrics_when_populated() {
        let gpus = vec![make_nvidia_gpu()];
        let out = HardwareMetricExporter::new(&gpus).export_metrics();
        assert!(out.contains("all_smi_gpu_sm_occupancy{"));
        assert!(out.contains("all_smi_gpu_memory_bandwidth_utilization{"));
    }

    #[test]
    fn omits_gpm_metrics_when_unsupported_device() {
        let mut gpu = make_nvidia_gpu();
        gpu.gpm_metrics = None;
        let gpus = vec![gpu];
        let out = HardwareMetricExporter::new(&gpus).export_metrics();
        assert!(!out.contains("all_smi_gpu_sm_occupancy"));
        assert!(!out.contains("all_smi_gpu_memory_bandwidth_utilization"));
    }

    #[test]
    fn gpm_supported_but_unsampled_emits_nothing_for_gpm_values() {
        // Reader emits `Some(GpmMetrics::default())` on Hopper+ until the
        // two-sample handshake lands. The exporter MUST NOT publish that
        // as zeros — presence without fields should be silent.
        let mut gpu = make_nvidia_gpu();
        gpu.gpm_metrics = Some(GpmMetrics::default());
        let gpus = vec![gpu];
        let out = HardwareMetricExporter::new(&gpus).export_metrics();
        assert!(
            !out.contains("all_smi_gpu_sm_occupancy"),
            "unsampled GPM must not emit zero:\n{out}"
        );
        assert!(
            !out.contains("all_smi_gpu_memory_bandwidth_utilization"),
            "unsampled GPM must not emit zero:\n{out}"
        );
    }

    #[test]
    fn preserves_standard_gpu_labels_on_all_metrics() {
        let gpus = vec![make_nvidia_gpu()];
        let out = HardwareMetricExporter::new(&gpus).export_metrics();
        // Every emitted line must carry the canonical GPU label set so
        // operators can correlate hardware-detail rows with basic util /
        // temperature series by identical labels.
        let expected_fragments = [
            r#"gpu="NVIDIA A100""#,
            r#"instance="node-1""#,
            r#"uuid="GPU-ABC""#,
            r#"index="0""#,
        ];
        for line in out
            .lines()
            .filter(|l| l.starts_with("all_smi_") && l.contains('{'))
        {
            for frag in &expected_fragments {
                assert!(line.contains(frag), "label '{frag}' missing from {line}");
            }
        }
    }

    #[test]
    fn skips_non_nvidia_rows_when_nvidia_row_also_present() {
        let gpus = vec![make_non_nvidia_gpu(), make_nvidia_gpu()];
        let out = HardwareMetricExporter::new(&gpus).export_metrics();
        // Non-NVIDIA GPU has UUID "GPU-AMD" — it must not appear in the
        // exposition since it populated no hardware-detail fields.
        assert!(
            !out.contains("GPU-AMD"),
            "non-NVIDIA GPU leaked into hardware exporter:\n{out}"
        );
        // NVIDIA GPU must remain observable.
        assert!(out.contains("GPU-ABC"));
    }
}
