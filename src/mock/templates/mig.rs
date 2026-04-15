//! NVIDIA MIG mock template generator.
//!
//! Simulates a MIG-enabled NVIDIA host by emitting the same Prometheus
//! families that `api/metrics/mig.rs` produces from real NVML data.
//! Controlled at runtime by the `ALL_SMI_MOCK_MIG` environment variable —
//! setting it to any non-empty value adds MIG metrics to NVIDIA mock
//! responses so integration tests and UIs can be exercised without MIG
//! hardware.

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

/// Environment variable that gates MIG emission in mock responses.
pub const MIG_ENV_VAR: &str = "ALL_SMI_MOCK_MIG";

/// Realistic MIG profiles spanning the full A100/H100 spectrum. The choice
/// of 5 instances per GPU (1+2+3+1=7-slot equivalent) matches a typical
/// "1g.5gb x4 + 3g.20gb x1" partitioning that fills an A100. This is one of
/// the configurations the issue asked for; switching profiles at runtime
/// can be done by adjusting the `MIG_PROFILES` slice below.
const MIG_PROFILES: &[(u32, u64, &str)] = &[
    // (instance_id, fb_total_bytes, profile_name)
    (0, 5 * (1 << 30), "1g.5gb"),
    (1, 5 * (1 << 30), "1g.5gb"),
    (2, 10 * (1 << 30), "2g.10gb"),
    (3, 20 * (1 << 30), "3g.20gb"),
    (4, 40 * (1 << 30), "7g.40gb"),
];

/// Append MIG metric families to the NVIDIA mock template when the feature
/// is enabled via [`MIG_ENV_VAR`]. No-op on bare-metal mocks.
pub fn maybe_add_mig_template(
    template: &mut String,
    instance_name: &str,
    gpu_name: &str,
    gpus: &[GpuMetrics],
) {
    if !is_mig_enabled() {
        return;
    }
    add_mig_template(template, instance_name, gpu_name, gpus);
}

/// Append MIG metric families unconditionally. Exposed for unit tests.
pub fn add_mig_template(
    template: &mut String,
    instance_name: &str,
    gpu_name: &str,
    gpus: &[GpuMetrics],
) {
    // Host-level: gpu_mig_mode --------------------------------------------
    template.push_str(
        "# HELP all_smi_gpu_mig_mode NVIDIA MIG mode (1=enabled, 0=disabled) per parent GPU\n",
    );
    template.push_str("# TYPE all_smi_gpu_mig_mode gauge\n");
    for (i, gpu) in gpus.iter().enumerate() {
        let labels = format!(
            "gpu_index=\"{i}\", gpu_uuid=\"{}\", gpu=\"{gpu_name}\", instance=\"{instance_name}\", host=\"{instance_name}\"",
            gpu.uuid
        );
        template.push_str(&format!("all_smi_gpu_mig_mode{{{labels}}} 1\n"));
    }

    // Per-instance metrics -------------------------------------------------
    template.push_str(
        "# HELP all_smi_mig_instance_utilization_gpu Per-MIG-instance GPU SM utilization percentage (0-100)\n",
    );
    template.push_str("# TYPE all_smi_mig_instance_utilization_gpu gauge\n");
    emit_instance_family(template, instance_name, gpu_name, gpus, |i, j| {
        format!(
            "all_smi_mig_instance_utilization_gpu{{{}}} {{{{MIG_UTIL_{i}_{j}}}}}\n",
            format_instance_labels(i, j, instance_name, gpu_name, &gpus[i].uuid)
        )
    });

    template.push_str(
        "# HELP all_smi_mig_instance_utilization_memory Per-MIG-instance memory bandwidth utilization percentage (0-100)\n",
    );
    template.push_str("# TYPE all_smi_mig_instance_utilization_memory gauge\n");
    emit_instance_family(template, instance_name, gpu_name, gpus, |i, j| {
        format!(
            "all_smi_mig_instance_utilization_memory{{{}}} {{{{MIG_MEMUTIL_{i}_{j}}}}}\n",
            format_instance_labels(i, j, instance_name, gpu_name, &gpus[i].uuid)
        )
    });

    template.push_str(
        "# HELP all_smi_mig_instance_memory_used_bytes Per-MIG-instance framebuffer memory used\n",
    );
    template.push_str("# TYPE all_smi_mig_instance_memory_used_bytes gauge\n");
    emit_instance_family(template, instance_name, gpu_name, gpus, |i, j| {
        format!(
            "all_smi_mig_instance_memory_used_bytes{{{}}} {{{{MIG_MEMUSED_{i}_{j}}}}}\n",
            format_instance_labels(i, j, instance_name, gpu_name, &gpus[i].uuid)
        )
    });

    template.push_str(
        "# HELP all_smi_mig_instance_memory_total_bytes Per-MIG-instance framebuffer total carve-out\n",
    );
    template.push_str("# TYPE all_smi_mig_instance_memory_total_bytes gauge\n");
    emit_instance_family(template, instance_name, gpu_name, gpus, |i, j| {
        format!(
            "all_smi_mig_instance_memory_total_bytes{{{}}} {}\n",
            format_instance_labels(i, j, instance_name, gpu_name, &gpus[i].uuid),
            MIG_PROFILES[j].1
        )
    });
}

/// Replace per-instance MIG placeholders with synthetic values. Call from
/// the NVIDIA response renderer after the regular metric substitutions.
pub fn maybe_render_mig_response(response: String, gpus: &[GpuMetrics]) -> String {
    if !is_mig_enabled() {
        return response;
    }
    render_mig_response(response, gpus)
}

/// Unconditional renderer; used by tests.
pub fn render_mig_response(mut response: String, gpus: &[GpuMetrics]) -> String {
    use rand::{RngExt, rng};
    let mut rng = rng();

    for (i, _gpu) in gpus.iter().enumerate() {
        for (j, &(_, fb_total, _)) in MIG_PROFILES.iter().enumerate() {
            // Active instances get realistic utilization + memory footprint;
            // a small fraction stay idle to exercise the "no util reported"
            // path on the consumer side.
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
                rng.random_range((fb_total / 10)..(fb_total * 9 / 10))
            } else {
                0
            };

            response = response
                .replace(&format!("{{{{MIG_UTIL_{i}_{j}}}}}"), &util.to_string())
                .replace(
                    &format!("{{{{MIG_MEMUTIL_{i}_{j}}}}}"),
                    &mem_util.to_string(),
                )
                .replace(&format!("{{{{MIG_MEMUSED_{i}_{j}}}}}"), &used.to_string());
        }
    }

    response
}

/// `true` when the MIG mock mode is enabled via env var.
pub fn is_mig_enabled() -> bool {
    std::env::var(MIG_ENV_VAR)
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
    let (mig_instance, _, profile) = MIG_PROFILES[j];
    // Synthesize plausible IDs: gi=mig_instance+1, ci=0 (single compute slice).
    let gi = mig_instance + 1;
    let ci = 0_u32;
    format!(
        "gpu_index=\"{i}\", gpu_uuid=\"{gpu_uuid}\", gpu=\"{gpu_name}\", instance=\"{instance_name}\", host=\"{instance_name}\", mig_instance=\"{mig_instance}\", mig_uuid=\"MIG-mock-{i}-{j}\", mig_profile=\"{profile}\", gpu_instance_id=\"{gi}\", compute_instance_id=\"{ci}\""
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
        for j in 0..MIG_PROFILES.len() {
            template.push_str(&render_line(i, j));
        }
    }
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
        add_mig_template(&mut template, "node-01", "NVIDIA A100", &gpus);

        for family in [
            "all_smi_gpu_mig_mode",
            "all_smi_mig_instance_utilization_gpu",
            "all_smi_mig_instance_utilization_memory",
            "all_smi_mig_instance_memory_used_bytes",
            "all_smi_mig_instance_memory_total_bytes",
        ] {
            assert!(template.contains(family), "missing family {family}");
        }
    }

    #[test]
    fn template_uses_expected_instance_labels() {
        let gpus = vec![fake_gpu("GPU-7")];
        let mut template = String::new();
        add_mig_template(&mut template, "node-42", "NVIDIA A100", &gpus);
        assert!(template.contains("host=\"node-42\""));
        assert!(template.contains("gpu_uuid=\"GPU-7\""));
        assert!(template.contains("mig_instance=\"0\""));
        assert!(template.contains("mig_uuid=\"MIG-mock-0-0\""));
        assert!(template.contains("mig_profile=\"1g.5gb\""));
    }

    #[test]
    fn template_emits_one_row_per_profile_per_gpu() {
        let gpus = vec![fake_gpu("GPU-1"), fake_gpu("GPU-2")];
        let mut template = String::new();
        add_mig_template(&mut template, "node", "NVIDIA A100", &gpus);

        // 2 GPUs * 5 profiles = 10 placeholder rows in the utilization family.
        let count = template
            .matches("all_smi_mig_instance_utilization_gpu{")
            .count();
        assert_eq!(count, 10);
    }

    #[test]
    fn rendering_substitutes_all_placeholders() {
        let gpus = vec![fake_gpu("GPU-1")];
        let mut template = String::new();
        add_mig_template(&mut template, "node", "NVIDIA A100", &gpus);

        let rendered = render_mig_response(template, &gpus);
        assert!(
            !rendered.contains("{{MIG_"),
            "Placeholders remain unsubstituted:\n{rendered}"
        );
    }

    #[test]
    fn is_mig_enabled_reflects_env_var() {
        let prior = std::env::var(MIG_ENV_VAR).ok();
        // SAFETY: Single-threaded test harness for this specific test; env
        // mutation is confined to this scope. Same rationale as the vGPU
        // mock's `is_vgpu_enabled_reflects_env_var` test.
        unsafe {
            std::env::remove_var(MIG_ENV_VAR);
        }
        assert!(!is_mig_enabled());

        unsafe {
            std::env::set_var(MIG_ENV_VAR, "1");
        }
        assert!(is_mig_enabled());

        unsafe {
            std::env::set_var(MIG_ENV_VAR, "");
        }
        assert!(!is_mig_enabled());

        unsafe {
            match prior {
                Some(v) => std::env::set_var(MIG_ENV_VAR, v),
                None => std::env::remove_var(MIG_ENV_VAR),
            }
        }
    }
}
