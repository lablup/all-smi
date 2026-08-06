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
use crate::metrics::energy::PowerIntegrator;
use crate::storage::info::StorageInfo;
use crate::utils::RuntimeEnvironment;

use super::{
    MetricExporter, chassis::ChassisMetricExporter, cpu::CpuMetricExporter,
    disk::DiskMetricExporter, energy::EnergyMetricExporter,
    exporter_status::ExporterStatusMetricExporter, gpu::GpuMetricExporter,
    hardware::HardwareMetricExporter, memory::MemoryMetricExporter, mig::MigMetricExporter,
    npu::NpuMetricExporter, process::ProcessMetricExporter, runtime::RuntimeMetricExporter,
    vgpu::VgpuMetricExporter,
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
    /// Energy integrator backing the
    /// `all_smi_energy_consumed_joules_total` counter (issue #191).
    /// `None` when the caller has no accountant to surface (e.g.
    /// `snapshot --format prometheus` runs without a live integrator);
    /// the exporter then omits the metric family entirely.
    pub energy_integrator: Option<&'a PowerIntegrator>,
    /// Whether at least one collection cycle has populated the source of
    /// these inputs (issue #324). Drives the `all_smi_up` gauge.
    ///
    /// The live `/metrics` handler passes `!AppState::loading`; the
    /// one-shot `snapshot --format prometheus` path passes `true`
    /// because it collects synchronously before rendering. Byte-identical
    /// parity between the two paths therefore still holds for the same
    /// data *and* the same readiness.
    pub ready: bool,
}

/// Render the Prometheus exposition string for the given inputs.
///
/// The output is never empty: [`ExporterStatusMetricExporter`] emits
/// `all_smi_up` and `all_smi_build_info` unconditionally (issue #324).
/// Every *device* exporter in the chain still self-filters, so
/// non-applicable hosts (e.g. non-NVIDIA for NVLink/MIG/vGPU) contribute
/// nothing beyond that baseline.
pub fn render_prometheus_exposition(inputs: &MetricsRenderInputs<'_>) -> String {
    let mut all_metrics = String::new();

    // Baseline first (issue #324), so a consumer that reads only the head
    // of the response, or scrapes before the first collection cycle has
    // landed, still learns whether this exporter is up and which build it
    // is. Everything below this line self-filters; this block does not.
    let status_exporter = ExporterStatusMetricExporter::new(inputs.ready);
    all_metrics.push_str(&status_exporter.export_metrics());

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

    // Export the energy counter (issue #191). Self-filters to non-empty
    // integrators, so hosts that never reported power (no EnergyKey
    // recorded yet) contribute no output.
    if let Some(integrator) = inputs.energy_integrator {
        let energy_exporter = EnergyMetricExporter::new(integrator, inputs.gpu_info);
        all_metrics.push_str(&energy_exporter.export_metrics());
    }

    all_metrics
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_inputs(env: &RuntimeEnvironment, ready: bool) -> MetricsRenderInputs<'_> {
        MetricsRenderInputs {
            gpu_info: &[],
            process_info: &[],
            cpu_info: &[],
            memory_info: &[],
            storage_info: &[],
            runtime_environment: env,
            chassis_info: &[],
            vgpu_info: &[],
            mig_info: &[],
            energy_integrator: None,
            ready,
        }
    }

    /// Supersedes the former `empty_inputs_render_empty_string`, which
    /// asserted the exact behaviour issue #324 removed: an all-empty
    /// input set used to render zero bytes, so a scrape landing before
    /// the first collection cycle was indistinguishable from a scrape of
    /// a host with no devices. The baseline families are now the floor.
    #[test]
    fn empty_inputs_still_render_the_baseline() {
        let env = RuntimeEnvironment::default();
        let rendered = render_prometheus_exposition(&empty_inputs(&env, false));
        assert!(
            !rendered.is_empty(),
            "the exposition must never be byte-empty (issue #324)"
        );
        assert!(rendered.contains("all_smi_up{"), "{rendered}");
        assert!(rendered.contains("all_smi_build_info{"), "{rendered}");
    }

    /// Nothing but the baseline: with every device slice empty, the
    /// self-filtering exporters must still contribute nothing, so the
    /// added floor cannot be hiding a regression that made some other
    /// family unconditional.
    #[test]
    fn empty_inputs_render_only_the_baseline_families() {
        let env = RuntimeEnvironment::default();
        let rendered = render_prometheus_exposition(&empty_inputs(&env, false));
        for line in rendered.lines().filter(|l| !l.starts_with('#')) {
            assert!(
                line.starts_with("all_smi_up{") || line.starts_with("all_smi_build_info{"),
                "unexpected sample from an empty input set: {line}"
            );
        }
    }

    /// The pre-first-collection window is observable in-band, without
    /// timing the scrape.
    #[test]
    fn ready_flag_drives_the_up_gauge() {
        let env = RuntimeEnvironment::default();
        for (ready, expected) in [(false, " 0"), (true, " 1")] {
            let rendered = render_prometheus_exposition(&empty_inputs(&env, ready));
            let up_line = rendered
                .lines()
                .find(|l| l.starts_with("all_smi_up{"))
                .expect("all_smi_up sample line");
            assert!(
                up_line.ends_with(expected),
                "ready={ready} should render all_smi_up{expected}, got {up_line}"
            );
        }
    }
}
