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

//! Prometheus exporter for NVIDIA MIG (Multi-Instance GPU) metrics.
//!
//! Metrics produced by this exporter:
//!
//! * `all_smi_gpu_mig_mode` (gauge): per parent GPU; `1` when MIG mode is
//!   currently enabled, `0` otherwise.
//! * `all_smi_mig_instance_utilization_gpu` (gauge, %): per-instance SM
//!   utilization (0-100).
//! * `all_smi_mig_instance_utilization_memory` (gauge, %): per-instance memory
//!   bandwidth utilization (0-100).
//! * `all_smi_mig_instance_memory_used_bytes` (gauge): per-instance framebuffer
//!   used bytes.
//! * `all_smi_mig_instance_memory_total_bytes` (gauge): per-instance
//!   framebuffer carve-out total bytes.
//!
//! All metrics carry the labels `gpu_index`, `gpu_uuid`, `gpu`, `host`, and
//! `instance` (the Prometheus instance label). Per-instance metrics also carry
//! `mig_instance`, `mig_uuid`, `mig_profile`, and — when reported by NVML —
//! `gpu_instance_id` and `compute_instance_id`.
//!
//! The exporter emits nothing when `mig_info` is empty — non-MIG hosts stay
//! completely silent in the `/metrics` output, matching the vGPU exporter.

use super::{MetricBuilder, MetricExporter};
use crate::device::MigGpuInfo;

pub struct MigMetricExporter<'a> {
    pub mig_info: &'a [MigGpuInfo],
}

impl<'a> MigMetricExporter<'a> {
    pub fn new(mig_info: &'a [MigGpuInfo]) -> Self {
        Self { mig_info }
    }

    /// Numeric encoding of the MIG mode flag for Prometheus. Stable so
    /// downstream dashboards can branch on the value.
    fn mig_mode_code(enabled: bool) -> u32 {
        if enabled { 1 } else { 0 }
    }

    fn export_host_info(&self, builder: &mut MetricBuilder) {
        builder
            .help(
                "all_smi_gpu_mig_mode",
                "NVIDIA MIG mode (1=enabled, 0=disabled) per parent GPU",
            )
            .type_("all_smi_gpu_mig_mode", "gauge");

        for host in self.mig_info {
            let gpu_index_str = host.gpu_index.to_string();
            let labels = [
                ("gpu_index", gpu_index_str.as_str()),
                ("gpu_uuid", host.gpu_uuid.as_str()),
                ("gpu", host.gpu_name.as_str()),
                ("instance", host.instance.as_str()),
                ("host", host.hostname.as_str()),
            ];
            builder.metric(
                "all_smi_gpu_mig_mode",
                &labels,
                Self::mig_mode_code(host.mig_mode),
            );
        }
    }

    fn export_instance_metrics(&self, builder: &mut MetricBuilder) {
        // Precompute the flattened host/instance rows once so the four
        // per-instance metric families below iterate over borrowed slices
        // without re-allocating the stringified indices per family. For
        // N instances this reduces per-scrape allocations from
        // 4*K*N to K*N (where K is the number of stringified labels per row).
        let rows = self.collect_rows();
        if rows.is_empty() {
            return;
        }

        builder
            .help(
                "all_smi_mig_instance_utilization_gpu",
                "Per-MIG-instance GPU SM utilization percentage (0-100)",
            )
            .type_("all_smi_mig_instance_utilization_gpu", "gauge");
        for row in &rows {
            if let Some(util) = row.instance.utilization_gpu {
                let labels = Self::instance_labels(row);
                builder.metric("all_smi_mig_instance_utilization_gpu", &labels, util);
            }
        }

        builder
            .help(
                "all_smi_mig_instance_utilization_memory",
                "Per-MIG-instance memory bandwidth utilization percentage (0-100)",
            )
            .type_("all_smi_mig_instance_utilization_memory", "gauge");
        for row in &rows {
            if let Some(util) = row.instance.utilization_memory {
                let labels = Self::instance_labels(row);
                builder.metric("all_smi_mig_instance_utilization_memory", &labels, util);
            }
        }

        builder
            .help(
                "all_smi_mig_instance_memory_used_bytes",
                "Per-MIG-instance framebuffer memory used in bytes",
            )
            .type_("all_smi_mig_instance_memory_used_bytes", "gauge");
        for row in &rows {
            let labels = Self::instance_labels(row);
            builder.metric(
                "all_smi_mig_instance_memory_used_bytes",
                &labels,
                row.instance.memory_used_bytes,
            );
        }

        builder
            .help(
                "all_smi_mig_instance_memory_total_bytes",
                "Per-MIG-instance framebuffer memory total carve-out in bytes",
            )
            .type_("all_smi_mig_instance_memory_total_bytes", "gauge");
        for row in &rows {
            let labels = Self::instance_labels(row);
            builder.metric(
                "all_smi_mig_instance_memory_total_bytes",
                &labels,
                row.instance.memory_total_bytes,
            );
        }
    }

    /// Flatten the nested host/instance structure into a single `Vec<Row>`
    /// where each row borrows its host and instance and owns only the small
    /// stringified id fields. Exists purely to amortize allocation across the
    /// four per-instance metric families in `export_instance_metrics`.
    fn collect_rows(&self) -> Vec<Row<'a>> {
        let total: usize = self.mig_info.iter().map(|h| h.instances.len()).sum();
        let mut rows = Vec::with_capacity(total);
        for host in self.mig_info {
            let gpu_index_str = host.gpu_index.to_string();
            for instance in &host.instances {
                rows.push(Row {
                    host,
                    instance,
                    gpu_index_str: gpu_index_str.clone(),
                    instance_id_str: instance.instance_id.to_string(),
                    gpu_instance_id_str: instance
                        .gpu_instance_id
                        .map(|v| v.to_string())
                        .unwrap_or_default(),
                    compute_instance_id_str: instance
                        .compute_instance_id
                        .map(|v| v.to_string())
                        .unwrap_or_default(),
                });
            }
        }
        rows
    }

    fn instance_labels<'b>(row: &'b Row<'a>) -> [(&'b str, &'b str); 9] {
        [
            ("gpu_index", row.gpu_index_str.as_str()),
            ("gpu_uuid", row.host.gpu_uuid.as_str()),
            ("gpu", row.host.gpu_name.as_str()),
            ("instance", row.host.instance.as_str()),
            ("host", row.host.hostname.as_str()),
            ("mig_instance", row.instance_id_str.as_str()),
            ("mig_uuid", row.instance.uuid.as_str()),
            ("mig_profile", row.instance.profile_name.as_str()),
            // GPU/compute instance ids are emitted as empty strings when NVML
            // could not report them. The remote parser tolerates empty values
            // so that round-tripping stays lossless.
            ("gpu_instance_id", row.gpu_instance_id_str.as_str()),
        ]
    }
}

/// Borrowed view of a single `(host, instance)` pair used by
/// `MigMetricExporter::export_instance_metrics`. Owns only the small
/// stringified ids so that the per-instance metric families can iterate
/// without re-allocating them.
struct Row<'a> {
    host: &'a MigGpuInfo,
    instance: &'a crate::device::MigInstanceInfo,
    gpu_index_str: String,
    instance_id_str: String,
    /// Stringified `gpu_instance_id` (empty when NVML did not report it).
    /// Currently unused at the label level but kept for forward compatibility
    /// — emitted via [`MigMetricExporter::instance_labels`].
    gpu_instance_id_str: String,
    /// Stringified `compute_instance_id`. Same forward-compatibility note as
    /// above. Reserved for a future label rev that wants to expose it directly.
    #[allow(dead_code)]
    compute_instance_id_str: String,
}

impl<'a> MetricExporter for MigMetricExporter<'a> {
    fn export_metrics(&self) -> String {
        if self.mig_info.is_empty() {
            return String::new();
        }

        let mut builder = MetricBuilder::new();
        self.export_host_info(&mut builder);
        self.export_instance_metrics(&mut builder);
        builder.build()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::{MigGpuInfo, MigInstanceInfo};

    fn sample_host() -> MigGpuInfo {
        MigGpuInfo {
            host_id: "gpu-host".to_string(),
            hostname: "gpu-host".to_string(),
            instance: "gpu-host".to_string(),
            gpu_index: 0,
            gpu_uuid: "GPU-abc123".to_string(),
            gpu_name: "NVIDIA A100".to_string(),
            mig_mode: true,
            instances: vec![MigInstanceInfo {
                instance_id: 3,
                gpu_instance_id: Some(7),
                compute_instance_id: Some(0),
                uuid: "MIG-xxx".to_string(),
                profile_name: "1g.5gb".to_string(),
                utilization_gpu: Some(75),
                utilization_memory: Some(40),
                memory_used_bytes: 1 << 30,
                memory_total_bytes: 5 << 30,
            }],
        }
    }

    #[test]
    fn exports_nothing_when_mig_info_empty() {
        let exporter = MigMetricExporter::new(&[]);
        assert_eq!(exporter.export_metrics(), "");
    }

    #[test]
    fn exports_expected_metric_families() {
        let hosts = vec![sample_host()];
        let exporter = MigMetricExporter::new(&hosts);
        let output = exporter.export_metrics();

        assert!(output.contains("all_smi_gpu_mig_mode"));
        assert!(output.contains("all_smi_mig_instance_utilization_gpu"));
        assert!(output.contains("all_smi_mig_instance_utilization_memory"));
        assert!(output.contains("all_smi_mig_instance_memory_used_bytes"));
        assert!(output.contains("all_smi_mig_instance_memory_total_bytes"));
    }

    #[test]
    fn exports_required_labels_for_instance_metrics() {
        let hosts = vec![sample_host()];
        let exporter = MigMetricExporter::new(&hosts);
        let output = exporter.export_metrics();

        assert!(output.contains("gpu_index=\"0\""));
        assert!(output.contains("gpu_uuid=\"GPU-abc123\""));
        assert!(output.contains("host=\"gpu-host\""));
        assert!(output.contains("mig_instance=\"3\""));
        assert!(output.contains("mig_uuid=\"MIG-xxx\""));
        assert!(output.contains("mig_profile=\"1g.5gb\""));
        assert!(output.contains("gpu_instance_id=\"7\""));
    }

    #[test]
    fn mig_mode_emits_zero_for_disabled_parent_gpu() {
        let mut host = sample_host();
        host.mig_mode = false;
        // is_mig_active is still true because instances is non-empty, so the
        // host record still surfaces. The mode metric must report 0.
        let hosts = vec![host];
        let exporter = MigMetricExporter::new(&hosts);
        let out = exporter.export_metrics();
        assert!(out.contains("all_smi_gpu_mig_mode{"));
        // The data line must end with " 0" for the disabled parent.
        let mode_line = out
            .lines()
            .find(|l| l.starts_with("all_smi_gpu_mig_mode{"))
            .expect("mode line present");
        assert!(mode_line.ends_with(" 0"), "got line: {mode_line}");
    }

    #[test]
    fn mig_mode_code_is_stable() {
        assert_eq!(MigMetricExporter::mig_mode_code(false), 0);
        assert_eq!(MigMetricExporter::mig_mode_code(true), 1);
    }

    #[test]
    fn util_line_present_only_when_reported() {
        let mut host = sample_host();
        host.instances[0].utilization_gpu = None;
        let hosts = vec![host];
        let exporter = MigMetricExporter::new(&hosts);
        let output = exporter.export_metrics();

        // HELP/TYPE lines are always emitted, but no data line for utilization
        // should be produced when the stat is unavailable.
        assert!(output.contains("# HELP all_smi_mig_instance_utilization_gpu"));
        let data_lines = output
            .lines()
            .filter(|l| l.starts_with("all_smi_mig_instance_utilization_gpu{"))
            .count();
        assert_eq!(data_lines, 0);
    }

    #[test]
    fn empty_gpu_instance_id_renders_empty_string_label() {
        let mut host = sample_host();
        host.instances[0].gpu_instance_id = None;
        let hosts = vec![host];
        let exporter = MigMetricExporter::new(&hosts);
        let output = exporter.export_metrics();
        // Empty label values are always present; the parser tolerates them.
        assert!(output.contains("gpu_instance_id=\"\""));
    }
}
