//! NVIDIA vGPU mock template generator.
//!
//! Simulates a vGPU-enabled NVIDIA host by emitting the same Prometheus
//! families that `api/metrics/vgpu.rs` produces from real NVML data. Controlled
//! at runtime by the `ALL_SMI_MOCK_VGPU` environment variable — setting it
//! to any non-empty value adds vGPU metrics to NVIDIA mock responses so
//! integration tests and UIs can be exercised without vGPU hardware.

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

use crate::mock::metrics::GpuMetrics;

/// Environment variable that gates vGPU emission in mock responses.
pub const VGPU_ENV_VAR: &str = "ALL_SMI_MOCK_VGPU";

/// Number of synthetic vGPU instances per GPU when the feature is enabled.
const VGPUS_PER_GPU: usize = 4;

/// Append vGPU metric families to the NVIDIA mock template when the feature
/// is enabled via [`VGPU_ENV_VAR`]. No-op on bare-metal mocks.
pub fn maybe_add_vgpu_template(
    template: &mut String,
    instance_name: &str,
    gpu_name: &str,
    gpus: &[GpuMetrics],
) {
    if !is_vgpu_enabled() {
        return;
    }
    add_vgpu_template(template, instance_name, gpu_name, gpus);
}

/// Append vGPU metric families unconditionally. Exposed for unit tests.
pub fn add_vgpu_template(
    template: &mut String,
    instance_name: &str,
    gpu_name: &str,
    gpus: &[GpuMetrics],
) {
    // Host-level metrics -------------------------------------------------------
    template.push_str(
        "# HELP all_smi_vgpu_host_mode NVIDIA vGPU host mode (0=NonSriov, 1=Sriov, 2=Disabled)\n",
    );
    template.push_str("# TYPE all_smi_vgpu_host_mode gauge\n");
    for (i, gpu) in gpus.iter().enumerate() {
        let labels = format!(
            "gpu_index=\"{i}\", gpu_uuid=\"{}\", gpu=\"{gpu_name}\", instance=\"{instance_name}\", host=\"{instance_name}\", host_mode=\"Sriov\"",
            gpu.uuid
        );
        template.push_str(&format!("all_smi_vgpu_host_mode{{{labels}}} 1\n"));
    }

    template.push_str(
        "# HELP all_smi_vgpu_scheduler_state NVIDIA vGPU scheduler ARR mode (0=unsupported, 1=off, 2=adaptive round robin)\n",
    );
    template.push_str("# TYPE all_smi_vgpu_scheduler_state gauge\n");
    for (i, gpu) in gpus.iter().enumerate() {
        let labels = format!(
            "gpu_index=\"{i}\", gpu_uuid=\"{}\", gpu=\"{gpu_name}\", instance=\"{instance_name}\", host=\"{instance_name}\", arr_supported=\"true\"",
            gpu.uuid
        );
        template.push_str(&format!("all_smi_vgpu_scheduler_state{{{labels}}} 2\n"));
    }

    template.push_str("# HELP all_smi_vgpu_scheduler_policy NVIDIA vGPU scheduler policy id\n");
    template.push_str("# TYPE all_smi_vgpu_scheduler_policy gauge\n");
    for (i, gpu) in gpus.iter().enumerate() {
        let labels = format!(
            "gpu_index=\"{i}\", gpu_uuid=\"{}\", gpu=\"{gpu_name}\", instance=\"{instance_name}\", host=\"{instance_name}\"",
            gpu.uuid
        );
        template.push_str(&format!("all_smi_vgpu_scheduler_policy{{{labels}}} 1\n"));
    }

    // Per-instance metrics -----------------------------------------------------
    template
        .push_str("# HELP all_smi_vgpu_utilization Per-vGPU GPU utilization percentage (0-100)\n");
    template.push_str("# TYPE all_smi_vgpu_utilization gauge\n");
    emit_instance_family(template, instance_name, gpu_name, gpus, |i, j| {
        format!(
            "all_smi_vgpu_utilization{{{}}} {{{{VGPU_UTIL_{i}_{j}}}}}\n",
            format_instance_labels(i, j, instance_name, gpu_name, &gpus[i].uuid)
        )
    });

    template.push_str(
        "# HELP all_smi_vgpu_memory_utilization Per-vGPU memory bandwidth utilization percentage\n",
    );
    template.push_str("# TYPE all_smi_vgpu_memory_utilization gauge\n");
    emit_instance_family(template, instance_name, gpu_name, gpus, |i, j| {
        format!(
            "all_smi_vgpu_memory_utilization{{{}}} {{{{VGPU_MEMUTIL_{i}_{j}}}}}\n",
            format_instance_labels(i, j, instance_name, gpu_name, &gpus[i].uuid)
        )
    });

    template.push_str("# HELP all_smi_vgpu_memory_used_bytes Per-vGPU framebuffer memory used\n");
    template.push_str("# TYPE all_smi_vgpu_memory_used_bytes gauge\n");
    emit_instance_family(template, instance_name, gpu_name, gpus, |i, j| {
        format!(
            "all_smi_vgpu_memory_used_bytes{{{}}} {{{{VGPU_MEMUSED_{i}_{j}}}}}\n",
            format_instance_labels(i, j, instance_name, gpu_name, &gpus[i].uuid)
        )
    });

    template.push_str("# HELP all_smi_vgpu_memory_total_bytes Per-vGPU framebuffer budget\n");
    template.push_str("# TYPE all_smi_vgpu_memory_total_bytes gauge\n");
    emit_instance_family(template, instance_name, gpu_name, gpus, |i, j| {
        format!(
            "all_smi_vgpu_memory_total_bytes{{{}}} {{{{VGPU_MEMTOTAL_{i}_{j}}}}}\n",
            format_instance_labels(i, j, instance_name, gpu_name, &gpus[i].uuid)
        )
    });

    template.push_str("# HELP all_smi_vgpu_active Per-vGPU liveness (1=active, 0=idle)\n");
    template.push_str("# TYPE all_smi_vgpu_active gauge\n");
    emit_instance_family(template, instance_name, gpu_name, gpus, |i, j| {
        format!(
            "all_smi_vgpu_active{{{}}} {{{{VGPU_ACTIVE_{i}_{j}}}}}\n",
            format_instance_labels(i, j, instance_name, gpu_name, &gpus[i].uuid)
        )
    });
}

/// Replace per-instance vGPU placeholders with synthetic values. Call from
/// the NVIDIA response renderer after the regular metric substitutions.
pub fn maybe_render_vgpu_response(response: String, gpus: &[GpuMetrics]) -> String {
    if !is_vgpu_enabled() {
        return response;
    }
    render_vgpu_response(response, gpus)
}

/// Unconditional renderer; used by tests.
pub fn render_vgpu_response(mut response: String, gpus: &[GpuMetrics]) -> String {
    use rand::{RngExt, rng};
    let mut rng = rng();

    for (i, gpu) in gpus.iter().enumerate() {
        // Slice the GPU's framebuffer evenly across the synthetic vGPUs so
        // the totals are consistent with the parent GPU.
        let per_total = gpu.memory_total_bytes / VGPUS_PER_GPU as u64;
        for j in 0..VGPUS_PER_GPU {
            // Active vGPUs get a realistic utilization + memory footprint;
            // inactive ones produce zeros.
            let active = rng.random_bool(0.7);
            let util = if active {
                rng.random_range(10..90_u32)
            } else {
                0
            };
            let mem_util = if active {
                rng.random_range(5..60_u32)
            } else {
                0
            };
            let used = if active {
                rng.random_range((per_total / 10)..(per_total * 9 / 10))
            } else {
                0
            };
            let active_flag = if active { 1 } else { 0 };

            response = response
                .replace(&format!("{{{{VGPU_UTIL_{i}_{j}}}}}"), &util.to_string())
                .replace(
                    &format!("{{{{VGPU_MEMUTIL_{i}_{j}}}}}"),
                    &mem_util.to_string(),
                )
                .replace(&format!("{{{{VGPU_MEMUSED_{i}_{j}}}}}"), &used.to_string())
                .replace(
                    &format!("{{{{VGPU_MEMTOTAL_{i}_{j}}}}}"),
                    &per_total.to_string(),
                )
                .replace(
                    &format!("{{{{VGPU_ACTIVE_{i}_{j}}}}}"),
                    &active_flag.to_string(),
                );
        }
    }

    response
}

/// `true` when the vGPU mock mode is enabled via env var.
pub fn is_vgpu_enabled() -> bool {
    std::env::var(VGPU_ENV_VAR)
        .map(|v| !v.is_empty())
        .unwrap_or(false)
}

fn format_instance_labels(
    i: usize,
    j: usize,
    instance_name: &str,
    gpu_name: &str,
    gpu_uuid: &str,
) -> String {
    let vgpu_type = synth_vgpu_type(j);
    format!(
        "gpu_index=\"{i}\", gpu_uuid=\"{gpu_uuid}\", gpu=\"{gpu_name}\", instance=\"{instance_name}\", host=\"{instance_name}\", vgpu_id=\"{vgpu_id}\", vgpu_uuid=\"GRID-mock-{i}-{j}\", vgpu_type=\"{vgpu_type}\"",
        vgpu_id = synth_vgpu_id(i, j)
    )
}

fn emit_instance_family(
    template: &mut String,
    _instance_name: &str,
    _gpu_name: &str,
    gpus: &[GpuMetrics],
    render_line: impl Fn(usize, usize) -> String,
) {
    for i in 0..gpus.len() {
        for j in 0..VGPUS_PER_GPU {
            template.push_str(&render_line(i, j));
        }
    }
}

fn synth_vgpu_id(gpu_index: usize, vgpu_index: usize) -> u32 {
    (gpu_index as u32) * 100 + vgpu_index as u32
}

fn synth_vgpu_type(vgpu_index: usize) -> String {
    let profiles = [
        "GRID A100-2C",
        "GRID A100-4C",
        "GRID A100-8C",
        "GRID A100-10C",
    ];
    profiles[vgpu_index % profiles.len()].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_gpu(uuid: &str) -> GpuMetrics {
        GpuMetrics {
            uuid: uuid.to_string(),
            utilization: 50.0,
            memory_used_bytes: 20 * (1 << 30),
            memory_total_bytes: 80 * (1 << 30),
            temperature_celsius: 60,
            power_consumption_watts: 300.0,
            frequency_mhz: 1500,
            ane_utilization_watts: 0.0,
            thermal_pressure_level: None,
        }
    }

    #[test]
    fn template_emits_expected_families() {
        let gpus = vec![fake_gpu("GPU-1"), fake_gpu("GPU-2")];
        let mut template = String::new();
        add_vgpu_template(&mut template, "node-01", "NVIDIA A100", &gpus);

        for family in [
            "all_smi_vgpu_host_mode",
            "all_smi_vgpu_scheduler_state",
            "all_smi_vgpu_scheduler_policy",
            "all_smi_vgpu_utilization",
            "all_smi_vgpu_memory_utilization",
            "all_smi_vgpu_memory_used_bytes",
            "all_smi_vgpu_memory_total_bytes",
            "all_smi_vgpu_active",
        ] {
            assert!(template.contains(family), "missing family {family}");
        }
    }

    #[test]
    fn template_uses_expected_instance_labels() {
        let gpus = vec![fake_gpu("GPU-7")];
        let mut template = String::new();
        add_vgpu_template(&mut template, "node-42", "NVIDIA A100", &gpus);
        assert!(template.contains("host=\"node-42\""));
        assert!(template.contains("gpu_uuid=\"GPU-7\""));
        assert!(template.contains("vgpu_id=\"0\""));
        assert!(template.contains("vgpu_uuid=\"GRID-mock-0-0\""));
        assert!(template.contains("vgpu_type=\"GRID A100-2C\""));
    }

    #[test]
    fn rendering_substitutes_all_placeholders() {
        let gpus = vec![fake_gpu("GPU-1")];
        let mut template = String::new();
        add_vgpu_template(&mut template, "node", "NVIDIA A100", &gpus);

        let rendered = render_vgpu_response(template, &gpus);
        assert!(
            !rendered.contains("{{VGPU_"),
            "Placeholders remain unsubstituted:\n{rendered}"
        );
    }

    #[test]
    fn is_vgpu_enabled_reflects_env_var() {
        // Save prior state to avoid bleed between tests.
        let prior = std::env::var(VGPU_ENV_VAR).ok();
        // SAFETY: Single-threaded test harness for this specific test; env
        // mutation is confined to this scope. See rustc note about setenv
        // being unsafe on Unix across threads — this test is not parallelised
        // with env-sensitive siblings.
        unsafe {
            std::env::remove_var(VGPU_ENV_VAR);
        }
        assert!(!is_vgpu_enabled());

        unsafe {
            std::env::set_var(VGPU_ENV_VAR, "1");
        }
        assert!(is_vgpu_enabled());

        // Empty value treated as disabled.
        unsafe {
            std::env::set_var(VGPU_ENV_VAR, "");
        }
        assert!(!is_vgpu_enabled());

        // Restore prior state.
        unsafe {
            match prior {
                Some(v) => std::env::set_var(VGPU_ENV_VAR, v),
                None => std::env::remove_var(VGPU_ENV_VAR),
            }
        }
    }
}
