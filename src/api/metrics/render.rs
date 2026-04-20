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

//! Shared Prometheus exposition renderer.
//!
//! Both the live `api::handlers::metrics_handler` and the one-shot
//! `snapshot --format prometheus` path call into
//! [`render_prometheus_exposition`]. This guarantees that given identical
//! inputs, the two paths produce byte-identical output — which is the
//! acceptance criterion for the `snapshot` subcommand:
//!
//! > `all-smi snapshot --format prometheus` byte-for-byte matches a single
//! > scrape of `api` mode's `/metrics` for the same data.
//!
//! The exporter chain mirrors the original ordering encoded in
//! `metrics_handler` before extraction (gpu → npu → process → cpu → memory
//! → disk → runtime → chassis → vgpu → mig → hardware) so any dashboard
//! that parses line order stays compatible.

use crate::device::{
    ChassisInfo, CpuInfo, GpuInfo, MemoryInfo, MigGpuInfo, ProcessInfo, VgpuHostInfo,
};
use crate::storage::info::StorageInfo;
use crate::utils::RuntimeEnvironment;

use super::{
    MetricExporter, chassis::ChassisMetricExporter, cpu::CpuMetricExporter,
    disk::DiskMetricExporter, gpu::GpuMetricExporter, hardware::HardwareMetricExporter,
    memory::MemoryMetricExporter, mig::MigMetricExporter, npu::NpuMetricExporter,
    process::ProcessMetricExporter, runtime::RuntimeMetricExporter, vgpu::VgpuMetricExporter,
};

/// Borrowed references to the metric sources that feed the exposition.
///
/// Keeping this as a struct of references (rather than taking the full
/// `AppState` or `Snapshot` by reference) lets both callers build it
/// on-the-fly without cloning, and documents exactly which fields the
/// exposition depends on.
pub struct MetricsRenderInputs<'a> {
    pub gpu_info: &'a [GpuInfo],
    pub process_info: &'a [ProcessInfo],
    pub cpu_info: &'a [CpuInfo],
    pub memory_info: &'a [MemoryInfo],
    pub storage_info: &'a [StorageInfo],
    pub runtime_environment: &'a RuntimeEnvironment,
    pub chassis_info: &'a [ChassisInfo],
    pub vgpu_info: &'a [VgpuHostInfo],
    pub mig_info: &'a [MigGpuInfo],
}

/// Render the Prometheus exposition string for the given inputs.
///
/// Output is empty when no exporter writes anything. Every exporter in the
/// chain self-filters so non-applicable hosts (e.g. non-NVIDIA for
/// NVLink/MIG/vGPU) stay silent.
pub fn render_prometheus_exposition(inputs: &MetricsRenderInputs<'_>) -> String {
    let mut all_metrics = String::new();

    // Export GPU/NPU metrics
    if !inputs.gpu_info.is_empty() {
        // Export GPU/NPU metrics together since the exporters handle filtering
        let gpu_exporter = GpuMetricExporter::new(inputs.gpu_info);
        all_metrics.push_str(&gpu_exporter.export_metrics());

        let npu_exporter = NpuMetricExporter::new(inputs.gpu_info);
        all_metrics.push_str(&npu_exporter.export_metrics());
    }

    // Export process metrics
    if !inputs.process_info.is_empty() {
        let process_exporter = ProcessMetricExporter::new(inputs.process_info);
        all_metrics.push_str(&process_exporter.export_metrics());
    }

    // Export CPU metrics
    if !inputs.cpu_info.is_empty() {
        let cpu_exporter = CpuMetricExporter::new(inputs.cpu_info);
        all_metrics.push_str(&cpu_exporter.export_metrics());
    }

    // Export memory metrics
    if !inputs.memory_info.is_empty() {
        let memory_exporter = MemoryMetricExporter::new(inputs.memory_info);
        all_metrics.push_str(&memory_exporter.export_metrics());
    }

    // Export disk metrics
    if !inputs.storage_info.is_empty() {
        let disk_exporter = DiskMetricExporter::new(inputs.storage_info);
        all_metrics.push_str(&disk_exporter.export_metrics());
    }

    // Export runtime environment metrics (self-filters: a
    // `RuntimeEnvironment::default()` emits nothing because neither
    // container nor virtualization flags are set).
    let runtime_exporter = RuntimeMetricExporter::new(inputs.runtime_environment);
    all_metrics.push_str(&runtime_exporter.export_metrics());

    // Export chassis metrics
    if !inputs.chassis_info.is_empty() {
        let chassis_exporter = ChassisMetricExporter::new(inputs.chassis_info);
        all_metrics.push_str(&chassis_exporter.export_metrics());
    }

    // Export vGPU metrics (NVIDIA vGPU hosts only; silent no-op otherwise).
    if !inputs.vgpu_info.is_empty() {
        let vgpu_exporter = VgpuMetricExporter::new(inputs.vgpu_info);
        all_metrics.push_str(&vgpu_exporter.export_metrics());
    }

    // Export MIG metrics (NVIDIA datacenter GPUs with MIG enabled; silent
    // no-op on consumer cards, pre-Ampere GPUs, and non-MIG hosts).
    if !inputs.mig_info.is_empty() {
        let mig_exporter = MigMetricExporter::new(inputs.mig_info);
        all_metrics.push_str(&mig_exporter.export_metrics());
    }

    // Export extended hardware details (issue #132): NUMA node id, GSP
    // firmware mode + version, NvLink remote device types, optional GPM
    // gauges. The exporter self-filters to NVIDIA GPUs that populated at
    // least one of the new fields so non-NVIDIA and older-driver paths
    // stay silent in the `/metrics` output.
    if !inputs.gpu_info.is_empty() {
        let hw_exporter = HardwareMetricExporter::new(inputs.gpu_info);
        all_metrics.push_str(&hw_exporter.export_metrics());
    }

    all_metrics
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_inputs_render_empty_string() {
        let env = RuntimeEnvironment::default();
        let inputs = MetricsRenderInputs {
            gpu_info: &[],
            process_info: &[],
            cpu_info: &[],
            memory_info: &[],
            storage_info: &[],
            runtime_environment: &env,
            chassis_info: &[],
            vgpu_info: &[],
            mig_info: &[],
        };
        let rendered = render_prometheus_exposition(&inputs);
        assert_eq!(rendered, "");
    }
}
