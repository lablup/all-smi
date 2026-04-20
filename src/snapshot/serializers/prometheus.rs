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
//
//! Prometheus snapshot serializer.
//!
//! Delegates to the shared
//! [`crate::api::metrics::render::render_prometheus_exposition`] helper so
//! the output is byte-for-byte identical to what the `/metrics` endpoint of
//! `all-smi api` emits for the same inputs. This upholds the acceptance
//! criterion:
//!
//! > `all-smi snapshot --format prometheus` byte-for-byte matches a single
//! > scrape of `api` mode's `/metrics` for the same data.
//!
//! Caveat: both code paths today pass `RuntimeEnvironment::default()`,
//! empty `vgpu_info`, and empty `mig_info` because neither
//! `api::server::run_api_mode` nor the snapshot collector populates those
//! extras. When (and if) one path starts populating them, the other must
//! follow to keep parity.

use anyhow::Result;

use crate::api::metrics::render::{MetricsRenderInputs, render_prometheus_exposition};
use crate::snapshot::Snapshot;
use crate::utils::RuntimeEnvironment;

/// Render a *single* snapshot to the Prometheus exposition format.
///
/// Prometheus scrape semantics are inherently single-sample, so the caller
/// already capped the samples list to one entry. Any soft reader errors
/// accumulated in `snap.errors` are written to stderr rather than injected
/// into the exposition, since Prometheus parsers reject unknown comment
/// lines on some scrapers.
pub fn render(snapshots: &[Snapshot]) -> Result<String> {
    // `run_with_collector` caps Prometheus output at a single sample; guard
    // defensively in case a future caller reuses this function.
    let snap = snapshots
        .first()
        .ok_or_else(|| anyhow::anyhow!("Prometheus serializer requires at least one snapshot"))?;

    // Empty slices for sections the snapshot did not collect so the
    // per-section exporters stay silent — matching `api::server::run_api_mode`
    // which leaves vgpu/mig empty and uses the default `RuntimeEnvironment`.
    let runtime_env = RuntimeEnvironment::default();
    let empty_vgpu = Vec::new();
    let empty_mig = Vec::new();

    let inputs = MetricsRenderInputs {
        gpu_info: snap.gpus.as_deref().unwrap_or(&[]),
        process_info: snap.processes.as_deref().unwrap_or(&[]),
        cpu_info: snap.cpus.as_deref().unwrap_or(&[]),
        memory_info: snap.memory.as_deref().unwrap_or(&[]),
        storage_info: snap.storage.as_deref().unwrap_or(&[]),
        runtime_environment: &runtime_env,
        chassis_info: snap.chassis.as_deref().unwrap_or(&[]),
        vgpu_info: &empty_vgpu,
        mig_info: &empty_mig,
    };

    let out = render_prometheus_exposition(&inputs);

    // Surface reader errors on stderr rather than polluting the exposition.
    for err in &snap.errors {
        eprintln!(
            "snapshot: {section} reader {kind}: {message}",
            section = err.section,
            kind = err.kind,
            message = err.message
        );
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::GpuInfo;
    use crate::snapshot::Snapshot;
    use std::collections::HashMap;

    fn make_gpu() -> GpuInfo {
        GpuInfo {
            uuid: "GPU-0".to_string(),
            time: "2026-04-20T00:00:00Z".to_string(),
            name: "Test GPU".to_string(),
            device_type: "GPU".to_string(),
            host_id: "host0".to_string(),
            hostname: "host0".to_string(),
            instance: "host0:9090".to_string(),
            utilization: 50.0,
            ane_utilization: 0.0,
            dla_utilization: None,
            tensorcore_utilization: None,
            temperature: 55,
            used_memory: 2048,
            total_memory: 8192,
            frequency: 1500,
            power_consumption: 200.0,
            gpu_core_count: None,
            temperature_threshold_slowdown: None,
            temperature_threshold_shutdown: None,
            temperature_threshold_max_operating: None,
            temperature_threshold_acoustic: None,
            performance_state: None,
            numa_node_id: None,
            gsp_firmware_mode: None,
            gsp_firmware_version: None,
            nvlink_remote_devices: Vec::new(),
            gpm_metrics: None,
            detail: HashMap::new(),
        }
    }

    #[test]
    fn empty_snapshot_renders_empty_string() {
        let snap = Snapshot {
            schema: 1,
            timestamp: "2026-04-20T00:00:00Z".to_string(),
            hostname: "host0".to_string(),
            gpus: None,
            cpus: None,
            memory: None,
            chassis: None,
            processes: None,
            storage: None,
            errors: Vec::new(),
        };
        let rendered = render(&[snap]).unwrap();
        assert_eq!(rendered, "");
    }

    #[test]
    fn gpu_snapshot_produces_expected_metric_names() {
        let snap = Snapshot {
            schema: 1,
            timestamp: "2026-04-20T00:00:00Z".to_string(),
            hostname: "host0".to_string(),
            gpus: Some(vec![make_gpu()]),
            cpus: None,
            memory: None,
            chassis: None,
            processes: None,
            storage: None,
            errors: Vec::new(),
        };
        let rendered = render(&[snap]).unwrap();
        assert!(
            rendered.contains("all_smi_gpu_utilization"),
            "missing GPU utilization metric: {rendered}"
        );
        assert!(rendered.contains("all_smi_gpu_memory_used_bytes"));
        assert!(rendered.contains("all_smi_gpu_temperature_celsius"));
    }

    #[test]
    fn empty_inputs_return_error() {
        let result = render(&[]);
        assert!(result.is_err());
    }
}
