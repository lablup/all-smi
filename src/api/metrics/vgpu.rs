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

//! Prometheus exporter for NVIDIA vGPU metrics.
//!
//! Metrics produced by this exporter:
//!
//! * `all_smi_vgpu_utilization` (gauge, %): per-vGPU utilization.
//! * `all_smi_vgpu_memory_used_bytes` (gauge): framebuffer bytes in use.
//! * `all_smi_vgpu_memory_total_bytes` (gauge): framebuffer budget.
//! * `all_smi_vgpu_memory_utilization` (gauge, %): memory bandwidth use.
//! * `all_smi_vgpu_scheduler_state` (gauge): scheduler ARR mode (0=unsup,1=off,2=ARR).
//! * `all_smi_vgpu_scheduler_policy` (gauge): scheduler policy id.
//! * `all_smi_vgpu_host_mode` (gauge): 0=NonSriov, 1=Sriov, 2=Disabled.
//! * `all_smi_vgpu_host_info` (gauge): label-only info row per host GPU.
//!
//! All metrics carry the labels `gpu_index`, `gpu_uuid`, `instance`, and for
//! per-instance metrics also `vgpu_id`, `vgpu_uuid`, and `vgpu_type`.
//!
//! The exporter emits nothing when `vgpu_info` is empty — non-vGPU hosts stay
//! completely silent in the `/metrics` output.

use super::{MetricBuilder, MetricExporter};
use crate::device::VgpuHostInfo;

pub struct VgpuMetricExporter<'a> {
    pub vgpu_info: &'a [VgpuHostInfo],
}

impl<'a> VgpuMetricExporter<'a> {
    pub fn new(vgpu_info: &'a [VgpuHostInfo]) -> Self {
        Self { vgpu_info }
    }

    /// Numeric encoding of the host vGPU mode label. The mapping is stable
    /// (kept in sync with `device::readers::nvidia_vgpu`) so dashboards can
    /// cross-reference the `host_mode` label.
    fn host_mode_code(host_mode: &str) -> u32 {
        match host_mode {
            "NonSriov" => 0,
            "Sriov" => 1,
            _ => 2, // Disabled / unknown
        }
    }

    fn export_host_info(&self, builder: &mut MetricBuilder) {
        builder
            .help(
                "all_smi_vgpu_host_mode",
                "NVIDIA vGPU host mode (0=NonSriov, 1=Sriov, 2=Disabled)",
            )
            .type_("all_smi_vgpu_host_mode", "gauge");

        for host in self.vgpu_info {
            let gpu_index_str = host.gpu_index.to_string();
            let labels = [
                ("gpu_index", gpu_index_str.as_str()),
                ("gpu_uuid", host.gpu_uuid.as_str()),
                ("gpu", host.gpu_name.as_str()),
                ("instance", host.instance.as_str()),
                ("host", host.hostname.as_str()),
                ("host_mode", host.host_mode.as_str()),
            ];
            builder.metric(
                "all_smi_vgpu_host_mode",
                &labels,
                Self::host_mode_code(&host.host_mode),
            );
        }

        builder
            .help(
                "all_smi_vgpu_scheduler_state",
                "NVIDIA vGPU scheduler ARR mode (0=unsupported, 1=off, 2=adaptive round robin)",
            )
            .type_("all_smi_vgpu_scheduler_state", "gauge");

        for host in self.vgpu_info {
            let gpu_index_str = host.gpu_index.to_string();
            let arr_supported = if host.is_arr_supported {
                "true"
            } else {
                "false"
            };
            let labels = [
                ("gpu_index", gpu_index_str.as_str()),
                ("gpu_uuid", host.gpu_uuid.as_str()),
                ("gpu", host.gpu_name.as_str()),
                ("instance", host.instance.as_str()),
                ("host", host.hostname.as_str()),
                ("arr_supported", arr_supported),
            ];
            builder.metric(
                "all_smi_vgpu_scheduler_state",
                &labels,
                host.scheduler_arr_mode,
            );
        }

        builder
            .help(
                "all_smi_vgpu_scheduler_policy",
                "NVIDIA vGPU scheduler policy id",
            )
            .type_("all_smi_vgpu_scheduler_policy", "gauge");

        for host in self.vgpu_info {
            let gpu_index_str = host.gpu_index.to_string();
            let labels = [
                ("gpu_index", gpu_index_str.as_str()),
                ("gpu_uuid", host.gpu_uuid.as_str()),
                ("gpu", host.gpu_name.as_str()),
                ("instance", host.instance.as_str()),
                ("host", host.hostname.as_str()),
            ];
            builder.metric(
                "all_smi_vgpu_scheduler_policy",
                &labels,
                host.scheduler_policy,
            );
        }
    }

    fn export_instance_metrics(&self, builder: &mut MetricBuilder) {
        // Emit HELP/TYPE once per metric family to keep the output terse when
        // there are many instances. Missing values simply produce no line.
        builder
            .help(
                "all_smi_vgpu_utilization",
                "Per-vGPU GPU utilization percentage (0-100) as reported by NVML accounting",
            )
            .type_("all_smi_vgpu_utilization", "gauge");
        for (host, vgpu, gpu_index_str, instance_id_str) in self.iter_instances() {
            if let Some(util) = vgpu.gpu_utilization {
                let labels = Self::instance_labels(host, vgpu, &gpu_index_str, &instance_id_str);
                builder.metric("all_smi_vgpu_utilization", &labels, util);
            }
        }

        builder
            .help(
                "all_smi_vgpu_memory_utilization",
                "Per-vGPU memory bandwidth utilization percentage (0-100)",
            )
            .type_("all_smi_vgpu_memory_utilization", "gauge");
        for (host, vgpu, gpu_index_str, instance_id_str) in self.iter_instances() {
            if let Some(util) = vgpu.memory_utilization {
                let labels = Self::instance_labels(host, vgpu, &gpu_index_str, &instance_id_str);
                builder.metric("all_smi_vgpu_memory_utilization", &labels, util);
            }
        }

        builder
            .help(
                "all_smi_vgpu_memory_used_bytes",
                "Per-vGPU framebuffer memory used in bytes",
            )
            .type_("all_smi_vgpu_memory_used_bytes", "gauge");
        for (host, vgpu, gpu_index_str, instance_id_str) in self.iter_instances() {
            let labels = Self::instance_labels(host, vgpu, &gpu_index_str, &instance_id_str);
            builder.metric(
                "all_smi_vgpu_memory_used_bytes",
                &labels,
                vgpu.fb_used_bytes,
            );
        }

        builder
            .help(
                "all_smi_vgpu_memory_total_bytes",
                "Per-vGPU framebuffer memory budget in bytes",
            )
            .type_("all_smi_vgpu_memory_total_bytes", "gauge");
        for (host, vgpu, gpu_index_str, instance_id_str) in self.iter_instances() {
            let labels = Self::instance_labels(host, vgpu, &gpu_index_str, &instance_id_str);
            builder.metric(
                "all_smi_vgpu_memory_total_bytes",
                &labels,
                vgpu.fb_total_bytes,
            );
        }

        builder
            .help(
                "all_smi_vgpu_active",
                "Per-vGPU liveness (1=accounting PID active, 0=idle)",
            )
            .type_("all_smi_vgpu_active", "gauge");
        for (host, vgpu, gpu_index_str, instance_id_str) in self.iter_instances() {
            let labels = Self::instance_labels(host, vgpu, &gpu_index_str, &instance_id_str);
            builder.metric(
                "all_smi_vgpu_active",
                &labels,
                if vgpu.is_active { 1 } else { 0 },
            );
        }
    }

    fn iter_instances(
        &self,
    ) -> impl Iterator<
        Item = (
            &'a VgpuHostInfo,
            &'a crate::device::VgpuInfo,
            String,
            String,
        ),
    > {
        self.vgpu_info.iter().flat_map(|host| {
            let gpu_index_str = host.gpu_index.to_string();
            host.vgpus.iter().map(move |vgpu| {
                let instance_id_str = vgpu.instance_id.to_string();
                (host, vgpu, gpu_index_str.clone(), instance_id_str)
            })
        })
    }

    fn instance_labels<'b>(
        host: &'b VgpuHostInfo,
        vgpu: &'b crate::device::VgpuInfo,
        gpu_index_str: &'b str,
        instance_id_str: &'b str,
    ) -> [(&'b str, &'b str); 8] {
        [
            ("gpu_index", gpu_index_str),
            ("gpu_uuid", host.gpu_uuid.as_str()),
            ("gpu", host.gpu_name.as_str()),
            ("instance", host.instance.as_str()),
            ("host", host.hostname.as_str()),
            ("vgpu_id", instance_id_str),
            ("vgpu_uuid", vgpu.uuid.as_str()),
            ("vgpu_type", vgpu.vgpu_type_name.as_str()),
        ]
    }
}

impl<'a> MetricExporter for VgpuMetricExporter<'a> {
    fn export_metrics(&self) -> String {
        if self.vgpu_info.is_empty() {
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
    use crate::device::{VgpuHostInfo, VgpuInfo};
    use std::collections::HashMap;

    fn sample_host() -> VgpuHostInfo {
        VgpuHostInfo {
            host_id: "gpu-host".to_string(),
            hostname: "gpu-host".to_string(),
            instance: "gpu-host".to_string(),
            gpu_index: 0,
            gpu_uuid: "GPU-abc123".to_string(),
            gpu_name: "NVIDIA A100".to_string(),
            host_mode: "Sriov".to_string(),
            scheduler_policy: 1,
            scheduler_arr_mode: 2,
            is_arr_supported: true,
            vgpus: vec![VgpuInfo {
                instance_id: 42,
                uuid: "GRID-xxx".to_string(),
                vm_id: "vm-1".to_string(),
                vgpu_type_name: "GRID A100-8C".to_string(),
                fb_used_bytes: 1 << 30,
                fb_total_bytes: 8 << 30,
                gpu_utilization: Some(75),
                memory_utilization: Some(40),
                is_active: true,
            }],
            detail: HashMap::new(),
        }
    }

    #[test]
    fn exports_nothing_when_vgpu_info_empty() {
        let exporter = VgpuMetricExporter::new(&[]);
        assert_eq!(exporter.export_metrics(), "");
    }

    #[test]
    fn exports_expected_metric_families() {
        let hosts = vec![sample_host()];
        let exporter = VgpuMetricExporter::new(&hosts);
        let output = exporter.export_metrics();

        // Metric names
        assert!(output.contains("all_smi_vgpu_host_mode"));
        assert!(output.contains("all_smi_vgpu_scheduler_state"));
        assert!(output.contains("all_smi_vgpu_scheduler_policy"));
        assert!(output.contains("all_smi_vgpu_utilization"));
        assert!(output.contains("all_smi_vgpu_memory_utilization"));
        assert!(output.contains("all_smi_vgpu_memory_used_bytes"));
        assert!(output.contains("all_smi_vgpu_memory_total_bytes"));
        assert!(output.contains("all_smi_vgpu_active"));
    }

    #[test]
    fn exports_required_labels_for_instance_metrics() {
        let hosts = vec![sample_host()];
        let exporter = VgpuMetricExporter::new(&hosts);
        let output = exporter.export_metrics();

        // Instance metrics must carry all identifying labels
        assert!(output.contains("gpu_index=\"0\""));
        assert!(output.contains("gpu_uuid=\"GPU-abc123\""));
        assert!(output.contains("host=\"gpu-host\""));
        assert!(output.contains("vgpu_id=\"42\""));
        assert!(output.contains("vgpu_uuid=\"GRID-xxx\""));
        assert!(output.contains("vgpu_type=\"GRID A100-8C\""));
    }

    #[test]
    fn host_mode_code_is_stable_and_defaults_to_disabled() {
        assert_eq!(VgpuMetricExporter::host_mode_code("NonSriov"), 0);
        assert_eq!(VgpuMetricExporter::host_mode_code("Sriov"), 1);
        assert_eq!(VgpuMetricExporter::host_mode_code("Disabled"), 2);
        assert_eq!(VgpuMetricExporter::host_mode_code("garbage"), 2);
    }

    #[test]
    fn util_line_present_only_when_reported() {
        let mut host = sample_host();
        host.vgpus[0].gpu_utilization = None;
        let hosts = vec![host];
        let exporter = VgpuMetricExporter::new(&hosts);
        let output = exporter.export_metrics();
        // HELP/TYPE lines are always emitted, but no data line for utilization
        // should be produced when the stat is unavailable.
        assert!(output.contains("# HELP all_smi_vgpu_utilization"));
        // Count occurrences of "all_smi_vgpu_utilization{" to ensure no
        // instance data line was emitted.
        let data_lines = output
            .lines()
            .filter(|l| l.starts_with("all_smi_vgpu_utilization{"))
            .count();
        assert_eq!(data_lines, 0);
    }
}
