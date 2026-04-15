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

//! Integration tests for the NVIDIA vGPU pipeline.
//!
//! Exercises the Prometheus-format round-trip from exporter text (crafted to
//! mirror what `api::metrics::vgpu::VgpuMetricExporter` emits) through the
//! remote metrics parser, asserting that every field survives unchanged.

use all_smi::prelude::*;
use regex::Regex;

fn regex() -> Regex {
    Regex::new(r"^all_smi_([^\{]+)\{([^}]+)\} ([\d\.]+)$").unwrap()
}

/// Replicate the exporter output format. Kept close to
/// `api/metrics/vgpu.rs`; if that exporter adds new metrics, this test should
/// be updated in lockstep.
fn exported_metrics_text() -> String {
    let mut out = String::new();
    out.push_str("# HELP all_smi_vgpu_host_mode NVIDIA vGPU host mode\n");
    out.push_str("# TYPE all_smi_vgpu_host_mode gauge\n");
    out.push_str(concat!(
        "all_smi_vgpu_host_mode{gpu_index=\"3\", gpu_uuid=\"GPU-A\", ",
        "gpu=\"NVIDIA A100\", instance=\"node-42\", host=\"node-42\", ",
        "host_mode=\"Sriov\"} 1\n"
    ));
    out.push_str("# HELP all_smi_vgpu_scheduler_state\n");
    out.push_str("# TYPE all_smi_vgpu_scheduler_state gauge\n");
    out.push_str(concat!(
        "all_smi_vgpu_scheduler_state{gpu_index=\"3\", gpu_uuid=\"GPU-A\", ",
        "gpu=\"NVIDIA A100\", instance=\"node-42\", host=\"node-42\", ",
        "arr_supported=\"true\"} 2\n"
    ));
    out.push_str("# HELP all_smi_vgpu_scheduler_policy\n");
    out.push_str("# TYPE all_smi_vgpu_scheduler_policy gauge\n");
    out.push_str(concat!(
        "all_smi_vgpu_scheduler_policy{gpu_index=\"3\", gpu_uuid=\"GPU-A\", ",
        "gpu=\"NVIDIA A100\", instance=\"node-42\", host=\"node-42\"} 1\n"
    ));
    // instance metrics
    out.push_str("# HELP all_smi_vgpu_utilization\n");
    out.push_str("# TYPE all_smi_vgpu_utilization gauge\n");
    out.push_str(concat!(
        "all_smi_vgpu_utilization{gpu_index=\"3\", gpu_uuid=\"GPU-A\", gpu=\"NVIDIA A100\", ",
        "instance=\"node-42\", host=\"node-42\", vgpu_id=\"10\", vgpu_uuid=\"GRID-10\", ",
        "vgpu_type=\"GRID A100-8C\"} 64\n"
    ));
    out.push_str("# HELP all_smi_vgpu_memory_used_bytes\n");
    out.push_str("# TYPE all_smi_vgpu_memory_used_bytes gauge\n");
    out.push_str(concat!(
        "all_smi_vgpu_memory_used_bytes{gpu_index=\"3\", gpu_uuid=\"GPU-A\", gpu=\"NVIDIA A100\", ",
        "instance=\"node-42\", host=\"node-42\", vgpu_id=\"10\", vgpu_uuid=\"GRID-10\", ",
        "vgpu_type=\"GRID A100-8C\"} 3221225472\n"
    ));
    out.push_str("# HELP all_smi_vgpu_memory_total_bytes\n");
    out.push_str("# TYPE all_smi_vgpu_memory_total_bytes gauge\n");
    out.push_str(concat!(
        "all_smi_vgpu_memory_total_bytes{gpu_index=\"3\", gpu_uuid=\"GPU-A\", gpu=\"NVIDIA A100\", ",
        "instance=\"node-42\", host=\"node-42\", vgpu_id=\"10\", vgpu_uuid=\"GRID-10\", ",
        "vgpu_type=\"GRID A100-8C\"} 8589934592\n"
    ));
    out.push_str("# HELP all_smi_vgpu_active\n");
    out.push_str("# TYPE all_smi_vgpu_active gauge\n");
    out.push_str(concat!(
        "all_smi_vgpu_active{gpu_index=\"3\", gpu_uuid=\"GPU-A\", gpu=\"NVIDIA A100\", ",
        "instance=\"node-42\", host=\"node-42\", vgpu_id=\"10\", vgpu_uuid=\"GRID-10\", ",
        "vgpu_type=\"GRID A100-8C\"} 1\n"
    ));
    out
}

#[test]
fn vgpu_metrics_parser_roundtrip_preserves_all_fields() {
    use all_smi::network::metrics_parser::MetricsParser;

    let parser = MetricsParser::new();
    let (_gpu, _cpu, _mem, _store, parsed) =
        parser.parse_metrics(&exported_metrics_text(), "127.0.0.1:9090", &regex());

    assert_eq!(parsed.len(), 1, "expected one host record");
    let got = &parsed[0];

    assert_eq!(got.gpu_uuid, "GPU-A");
    assert_eq!(got.gpu_name, "NVIDIA A100");
    assert_eq!(got.host_mode, "Sriov");
    assert_eq!(got.scheduler_policy, 1);
    assert_eq!(got.scheduler_arr_mode, 2);
    assert!(got.is_arr_supported);
    assert_eq!(got.gpu_index, 3);
    assert_eq!(got.vgpus.len(), 1);

    let inst = &got.vgpus[0];
    assert_eq!(inst.instance_id, 10);
    assert_eq!(inst.uuid, "GRID-10");
    assert_eq!(inst.vgpu_type_name, "GRID A100-8C");
    assert_eq!(inst.gpu_utilization, Some(64));
    assert_eq!(inst.fb_used_bytes, 3 * (1 << 30));
    assert_eq!(inst.fb_total_bytes, 8 * (1 << 30));
    assert!(inst.is_active);
}

#[test]
fn vgpu_parser_is_empty_on_bare_metal_metrics() {
    use all_smi::network::metrics_parser::MetricsParser;

    let parser = MetricsParser::new();
    let non_vgpu = concat!(
        "all_smi_gpu_utilization{gpu=\"RTX\", instance=\"x\", uuid=\"GPU-1\", index=\"0\"} 10\n",
        "all_smi_cpu_utilization{cpu_model=\"AMD\", instance=\"x\", hostname=\"x\", index=\"0\"} 20\n",
    );

    let (gpu, _cpu, _mem, _storage, parsed) = parser.parse_metrics(non_vgpu, "x", &regex());
    assert_eq!(gpu.len(), 1, "sanity: GPU row still parsed");
    assert!(
        parsed.is_empty(),
        "vGPU parser must stay quiet on bare-metal"
    );
}

#[test]
fn library_api_exposes_vgpu_info_method() {
    // Smoke test: simply verifying `AllSmi::get_vgpu_info` exists and returns
    // a Vec. On CI hosts without NVML the method returns empty, which is the
    // correct no-op behaviour we document.
    let smi = AllSmi::new().expect("AllSmi::new should not fail");
    let info: Vec<VgpuHostInfo> = smi.get_vgpu_info();
    // We can't assert that info is empty because the CI host could theoretically
    // be a vGPU host. We just assert the call shape compiles and does not panic.
    let _ = info;
}
