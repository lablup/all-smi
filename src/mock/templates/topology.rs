//! DGX-like topology mock template (issue #190).

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

//! Topology mock template — synthesizes a DGX-style topology graph so
//! the Topology tab can be exercised without real NVIDIA hardware.
//!
//! Gated by [`TOPOLOGY_ENV_VAR`] (`ALL_SMI_MOCK_TOPOLOGY=1`). When
//! enabled, produces:
//!
//! * `numa_node_id`: GPUs 0–3 → NUMA 0, GPUs 4–7 → NUMA 1.
//! * `nvlink_remote_device_type`: 7 GPU-to-GPU links per GPU (full mesh
//!   inside the node) plus 1 GPU-to-switch link per GPU — 64 total
//!   links across 8 GPUs, mirroring the DGX H100/H200 layout.
//! * `bandwidth_mb_s`: 50 000 MB/s (≈50 GB/s) — NvLink 5 speed.
//!
//! Unlike the legacy `ALL_SMI_MOCK_HARDWARE_DETAILS` gate, this template
//! targets exactly what the Topology tab needs so operators can enable
//! it independently of the broader hardware-detail mock.

use crate::mock::metrics::GpuMetrics;

/// Environment variable that gates DGX-like topology mock emission.
pub const TOPOLOGY_ENV_VAR: &str = "ALL_SMI_MOCK_TOPOLOGY";

/// `true` when the topology mock is enabled via env var.
pub fn is_topology_enabled() -> bool {
    std::env::var(TOPOLOGY_ENV_VAR)
        .map(|v| !v.is_empty() && v != "0")
        .unwrap_or(false)
}

/// Number of links per GPU in the synthetic DGX topology: 7 direct GPU
/// remotes (full mesh) + 1 switch remote = 8 links. NvSwitch overlay on
/// top of the full mesh keeps the graph legend honest.
pub const LINKS_PER_GPU: u32 = 8;

/// Per-link bandwidth hint in MB/s used by the mock template. 50 000
/// MB/s (≈50 GB/s) maps to the NvLink-5 tier in the edge classifier,
/// which yields "NV5" labels in the graph and matrix.
pub const LINK_BANDWIDTH_MB_S: u32 = 50_000;

/// Append topology-specific metric rows to the template when the env
/// var is set. No-op otherwise so the default mock output is unchanged.
pub fn maybe_add_topology_template(
    template: &mut String,
    gpu_name: &str,
    instance_name: &str,
    gpus: &[GpuMetrics],
) {
    if !is_topology_enabled() {
        return;
    }
    add_topology_template(template, gpu_name, instance_name, gpus);
}

/// Append the DGX-like topology rows unconditionally. Split out so
/// tests can exercise the emission without racing on the shared
/// `ALL_SMI_MOCK_TOPOLOGY` env var.
pub fn add_topology_template(
    template: &mut String,
    gpu_name: &str,
    instance_name: &str,
    gpus: &[GpuMetrics],
) {
    if gpus.is_empty() {
        return;
    }

    // --- NUMA node id: GPUs 0..mid go to NUMA 0, rest to NUMA 1. ---
    // Two-NUMA split mirrors the typical DGX H100 layout; a single-NUMA
    // layout tests the graceful-degradation path in the graph renderer.
    template.push_str("# HELP all_smi_gpu_numa_node_id NUMA node the GPU is attached to\n");
    template.push_str("# TYPE all_smi_gpu_numa_node_id gauge\n");
    let mid = gpus.len() / 2;
    for (i, gpu) in gpus.iter().enumerate() {
        let numa = if i < mid { 0 } else { 1 };
        let labels = format!(
            "gpu=\"{gpu_name}\", instance=\"{instance_name}\", gpu_uuid=\"{}\", gpu_index=\"{i}\"",
            gpu.uuid
        );
        template.push_str(&format!("all_smi_gpu_numa_node_id{{{labels}}} {numa}\n"));
    }

    // --- NvLink topology: 7 GPU + 1 switch link per GPU. ---
    template.push_str(
        "# HELP all_smi_nvlink_remote_device_type NvLink remote endpoint classification per \
         active link. Value is always 1; classification is carried in the `remote_type` label \
         (gpu / switch / ibmnpu / unknown). Optional `bandwidth_mb_s` label carries the \
         per-link bandwidth hint used by the topology tab's NVn classifier.\n",
    );
    template.push_str("# TYPE all_smi_nvlink_remote_device_type gauge\n");

    for (i, gpu) in gpus.iter().enumerate() {
        for link in 0..LINKS_PER_GPU {
            let remote_type = if link == LINKS_PER_GPU - 1 {
                "switch"
            } else {
                "gpu"
            };
            let labels = format!(
                "gpu=\"{gpu_name}\", instance=\"{instance_name}\", gpu_uuid=\"{}\", \
                 gpu_index=\"{i}\", link_index=\"{link}\", remote_type=\"{remote_type}\", \
                 bandwidth_mb_s=\"{LINK_BANDWIDTH_MB_S}\"",
                gpu.uuid
            );
            template.push_str(&format!(
                "all_smi_nvlink_remote_device_type{{{labels}}} 1\n"
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::metrics::GpuMetrics;

    fn mk_mock_gpus(count: usize) -> Vec<GpuMetrics> {
        (0..count)
            .map(|i| GpuMetrics {
                uuid: format!("GPU-mock-{i}"),
                utilization: 10.0,
                memory_used_bytes: 1024 * 1024 * 1024,
                memory_total_bytes: 80 * 1024 * 1024 * 1024,
                temperature_celsius: 50,
                power_consumption_watts: 300.0,
                frequency_mhz: 1800,
                ane_utilization_watts: 0.0,
                thermal_pressure_level: None,
            })
            .collect()
    }

    // Env-var-driven tests are intentionally omitted because rust-test
    // parallelism races on `std::env::set_var`. The gating behaviour
    // is a one-line check (`is_topology_enabled`) and is exercised
    // end-to-end by the mock server integration test.

    #[test]
    fn add_template_emits_numa_and_nvlink_rows() {
        let mut out = String::new();
        add_topology_template(&mut out, "NVIDIA H100", "mock-0", &mk_mock_gpus(8));
        assert!(
            out.contains("all_smi_gpu_numa_node_id"),
            "missing NUMA metric: {out}",
        );
        // 8 GPUs * 8 links = 64 nvlink rows
        let link_rows = out
            .lines()
            .filter(|l| l.starts_with("all_smi_nvlink_remote_device_type{"))
            .count();
        assert_eq!(link_rows, 64, "{out}");
        assert!(out.contains("bandwidth_mb_s=\"50000\""), "{out}");
        assert!(out.contains("remote_type=\"switch\""), "{out}");
    }

    #[test]
    fn two_numa_split() {
        let mut out = String::new();
        add_topology_template(&mut out, "NVIDIA H100", "mock-0", &mk_mock_gpus(8));
        let numa_lines: Vec<&str> = out
            .lines()
            .filter(|l| l.starts_with("all_smi_gpu_numa_node_id{"))
            .collect();
        let zeros = numa_lines.iter().filter(|l| l.ends_with(" 0")).count();
        let ones = numa_lines.iter().filter(|l| l.ends_with(" 1")).count();
        assert_eq!(zeros, 4, "expected 4 GPUs in NUMA 0: {out}");
        assert_eq!(ones, 4, "expected 4 GPUs in NUMA 1: {out}");
    }

    #[test]
    fn empty_gpu_list_is_no_op() {
        let mut out = String::new();
        add_topology_template(&mut out, "NVIDIA H100", "mock-0", &[]);
        assert!(out.is_empty(), "{out}");
    }

    #[test]
    fn every_row_carries_the_instance_label() {
        let mut out = String::new();
        add_topology_template(&mut out, "NVIDIA H100", "dgx-7", &mk_mock_gpus(2));
        for line in out.lines().filter(|l| l.starts_with("all_smi_")) {
            assert!(line.contains("instance=\"dgx-7\""), "{line}");
        }
    }
}
