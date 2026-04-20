//! Synthetic process metric template for mock API responses.
//!
//! Emits the `all_smi_process_*` metric families the real exporter
//! produces (see `src/api/metrics/process.rs`) so the cluster-wide
//! Users tab (issue #189) can be exercised in mock clusters without a
//! live `--processes` scrape.  Gated by the `ALL_SMI_MOCK_PROCESSES`
//! environment variable — setting it to any non-empty value makes
//! every mock node emit 2-4 synthetic processes per GPU owned by a
//! rotating pool of users.

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

/// Environment variable that gates synthetic process emission.
pub const PROCESS_ENV_VAR: &str = "ALL_SMI_MOCK_PROCESSES";

/// Rotating pool of user names so a single 5-node cluster exercises
/// multi-host aggregation.  "root" lets the `f` filter exercise the
/// system-hide path.
const USER_POOL: &[&str] = &["inureyes", "yeonji", "mira", "root"];

/// Rotating pool of commands that look recognisable in the `CMD`
/// column on the Users tab.
const COMMAND_POOL: &[&str] = &[
    "python train.py --bs=128 --epochs=100 --model=llama-70b",
    "python eval.py --split=val --checkpoint=epoch_32.pt",
    "/opt/llm/infer -m /models/phi-4-14b.safetensors --batch 4",
    "containerd-shim-runc-v2 -namespace k8s.io -id abc123",
    "node /workspace/run-agent.js --env prod",
];

/// Returns `true` when the environment variable is set to a non-empty
/// value (any value; matches the `ALL_SMI_MOCK_VGPU` convention).
pub fn is_process_mock_enabled() -> bool {
    std::env::var(PROCESS_ENV_VAR)
        .map(|v| !v.is_empty())
        .unwrap_or(false)
}

/// Append synthetic process metric lines to `template` when the mock
/// flag is enabled.  Safe no-op otherwise.
pub fn maybe_add_process_template(template: &mut String, instance_name: &str, gpus: &[GpuMetrics]) {
    if !is_process_mock_enabled() {
        return;
    }
    add_process_template(template, instance_name, gpus);
}

/// Append synthetic process rows unconditionally.  Exposed for unit
/// tests (and for the template callers that want to wire the feature
/// in without reading `std::env` for each render).
pub fn add_process_template(template: &mut String, instance_name: &str, gpus: &[GpuMetrics]) {
    template
        .push_str("# HELP all_smi_process_memory_used_bytes Process GPU memory used in bytes\n");
    template.push_str("# TYPE all_smi_process_memory_used_bytes gauge\n");

    // Helper closure to emit one process line per metric family.  We
    // produce a small deterministic set of processes per GPU so the
    // Users tab shows stable-ish aggregates across reload cycles
    // without a full mock-time RNG setup.
    let mut memory_lines = String::new();
    let mut start_lines = String::new();
    let mut cpu_lines = String::new();
    let mut pid_counter: u32 = 10_000;

    for (i, gpu) in gpus.iter().enumerate() {
        // Drive the number of processes from the GPU index so we get
        // a spread of load in the table (1–4 per GPU).
        let process_count = 1 + (i % 4);
        for j in 0..process_count {
            let user = USER_POOL[(i + j) % USER_POOL.len()];
            let command = COMMAND_POOL[(i * 2 + j) % COMMAND_POOL.len()];
            let name = command.split_whitespace().next().unwrap_or("process");
            let memory =
                ((gpu.memory_used_bytes as usize) / process_count.max(1)).max(1024 * 1024 * 64); // at least 64 MiB
            let start_seconds = 60 * (1 + i as u64 * 7 + j as u64);
            let cpu_pct = 5.0 + (j as f64) * 3.5;

            let labels = format!(
                "pid=\"{pid_counter}\", name=\"{name}\", user=\"{user}\", device_id=\"{i}\", \
                 gpu_index=\"{i}\", device_uuid=\"{uuid}\", command=\"{command}\", \
                 instance=\"{instance_name}\", host=\"{instance_name}\"",
                uuid = gpu.uuid,
            );

            memory_lines.push_str(&format!(
                "all_smi_process_memory_used_bytes{{{labels}}} {memory}\n"
            ));
            start_lines.push_str(&format!(
                "all_smi_process_start_time_seconds{{{labels}}} {start_seconds}\n"
            ));
            cpu_lines.push_str(&format!(
                "all_smi_process_cpu_percent{{{labels}}} {cpu_pct:.2}\n"
            ));

            pid_counter += 1;
        }
    }

    template.push_str(&memory_lines);

    template.push_str(
        "# HELP all_smi_process_start_time_seconds Wall-clock seconds since the \
         process started (TIME+ equivalent)\n",
    );
    template.push_str("# TYPE all_smi_process_start_time_seconds gauge\n");
    template.push_str(&start_lines);

    template.push_str("# HELP all_smi_process_cpu_percent Process CPU utilization percentage\n");
    template.push_str("# TYPE all_smi_process_cpu_percent gauge\n");
    template.push_str(&cpu_lines);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_gpu(idx: usize) -> GpuMetrics {
        GpuMetrics {
            uuid: format!("GPU-MOCK-{idx}"),
            utilization: 50.0,
            memory_used_bytes: 10 * 1024 * 1024 * 1024,
            memory_total_bytes: 40 * 1024 * 1024 * 1024,
            temperature_celsius: 60,
            power_consumption_watts: 200.0,
            frequency_mhz: 1500,
            ane_utilization_watts: 0.0,
            thermal_pressure_level: None,
        }
    }

    #[test]
    fn template_is_empty_when_env_var_unset() {
        // SAFETY: unit test single-threaded access to env.
        unsafe { std::env::remove_var(PROCESS_ENV_VAR) };
        let mut out = String::new();
        maybe_add_process_template(&mut out, "node1", &[make_gpu(0)]);
        assert!(out.is_empty(), "got: {out}");
    }

    #[test]
    fn add_process_template_emits_three_families() {
        let mut out = String::new();
        add_process_template(&mut out, "node1", &[make_gpu(0), make_gpu(1)]);
        assert!(out.contains("all_smi_process_memory_used_bytes"));
        assert!(out.contains("all_smi_process_start_time_seconds"));
        assert!(out.contains("all_smi_process_cpu_percent"));
        assert!(out.contains("user=\"inureyes\""));
        assert!(out.contains("gpu_index=\"0\""));
        assert!(out.contains("gpu_index=\"1\""));
    }

    #[test]
    fn every_row_carries_instance_and_host_labels() {
        // The remote collector keys by host — missing these labels
        // would silently lose every row.
        let mut out = String::new();
        add_process_template(&mut out, "dgx-7", &[make_gpu(0)]);
        assert!(out.contains("instance=\"dgx-7\""));
        assert!(out.contains("host=\"dgx-7\""));
    }
}
