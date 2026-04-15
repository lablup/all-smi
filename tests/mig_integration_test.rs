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

//! Integration tests for the NVIDIA MIG pipeline.
//!
//! Exercises the Prometheus-format round-trip from exporter text (crafted to
//! mirror what `api::metrics::mig::MigMetricExporter` emits) through the
//! remote metrics parser, asserting that every field survives unchanged.

use all_smi::prelude::*;
use regex::Regex;

fn regex() -> Regex {
    Regex::new(r"^all_smi_([^\{]+)\{([^}]+)\} ([\d\.]+)$").unwrap()
}

/// Replicate the exporter output format. Kept close to
/// `api/metrics/mig.rs`; if that exporter adds new metrics, this test should
/// be updated in lockstep.
fn exported_metrics_text() -> String {
    let mut out = String::new();
    out.push_str("# HELP all_smi_gpu_mig_mode NVIDIA MIG mode\n");
    out.push_str("# TYPE all_smi_gpu_mig_mode gauge\n");
    out.push_str(concat!(
        "all_smi_gpu_mig_mode{gpu_index=\"3\", gpu_uuid=\"GPU-MIG\", ",
        "gpu=\"NVIDIA A100\", instance=\"node-42\", host=\"node-42\"} 1\n"
    ));
    // instance metrics
    out.push_str("# HELP all_smi_mig_instance_utilization_gpu\n");
    out.push_str("# TYPE all_smi_mig_instance_utilization_gpu gauge\n");
    out.push_str(concat!(
        "all_smi_mig_instance_utilization_gpu{gpu_index=\"3\", gpu_uuid=\"GPU-MIG\", gpu=\"NVIDIA A100\", ",
        "instance=\"node-42\", host=\"node-42\", mig_instance=\"2\", mig_uuid=\"MIG-2\", ",
        "mig_profile=\"3g.20gb\", gpu_instance_id=\"5\", compute_instance_id=\"0\"} 64\n"
    ));
    out.push_str("# HELP all_smi_mig_instance_utilization_memory\n");
    out.push_str("# TYPE all_smi_mig_instance_utilization_memory gauge\n");
    out.push_str(concat!(
        "all_smi_mig_instance_utilization_memory{gpu_index=\"3\", gpu_uuid=\"GPU-MIG\", gpu=\"NVIDIA A100\", ",
        "instance=\"node-42\", host=\"node-42\", mig_instance=\"2\", mig_uuid=\"MIG-2\", ",
        "mig_profile=\"3g.20gb\", gpu_instance_id=\"5\", compute_instance_id=\"0\"} 18\n"
    ));
    out.push_str("# HELP all_smi_mig_instance_memory_used_bytes\n");
    out.push_str("# TYPE all_smi_mig_instance_memory_used_bytes gauge\n");
    out.push_str(concat!(
        "all_smi_mig_instance_memory_used_bytes{gpu_index=\"3\", gpu_uuid=\"GPU-MIG\", gpu=\"NVIDIA A100\", ",
        "instance=\"node-42\", host=\"node-42\", mig_instance=\"2\", mig_uuid=\"MIG-2\", ",
        "mig_profile=\"3g.20gb\", gpu_instance_id=\"5\", compute_instance_id=\"0\"} 8589934592\n"
    ));
    out.push_str("# HELP all_smi_mig_instance_memory_total_bytes\n");
    out.push_str("# TYPE all_smi_mig_instance_memory_total_bytes gauge\n");
    out.push_str(concat!(
        "all_smi_mig_instance_memory_total_bytes{gpu_index=\"3\", gpu_uuid=\"GPU-MIG\", gpu=\"NVIDIA A100\", ",
        "instance=\"node-42\", host=\"node-42\", mig_instance=\"2\", mig_uuid=\"MIG-2\", ",
        "mig_profile=\"3g.20gb\", gpu_instance_id=\"5\", compute_instance_id=\"0\"} 21474836480\n"
    ));
    out
}

#[test]
fn mig_metrics_parser_roundtrip_preserves_all_fields() {
    use all_smi::network::metrics_parser::MetricsParser;

    let parser = MetricsParser::new();
    let (_gpu, _cpu, _mem, _store, _vgpu, parsed) =
        parser.parse_metrics(&exported_metrics_text(), "127.0.0.1:9090", &regex());

    assert_eq!(parsed.len(), 1, "expected one host record");
    let got = &parsed[0];

    assert_eq!(got.gpu_uuid, "GPU-MIG");
    assert_eq!(got.gpu_name, "NVIDIA A100");
    assert!(got.mig_mode);
    assert_eq!(got.gpu_index, 3);
    assert_eq!(got.instances.len(), 1);

    let inst = &got.instances[0];
    assert_eq!(inst.instance_id, 2);
    assert_eq!(inst.uuid, "MIG-2");
    assert_eq!(inst.profile_name, "3g.20gb");
    assert_eq!(inst.gpu_instance_id, Some(5));
    assert_eq!(inst.compute_instance_id, Some(0));
    assert_eq!(inst.utilization_gpu, Some(64));
    assert_eq!(inst.utilization_memory, Some(18));
    assert_eq!(inst.memory_used_bytes, 8 * (1 << 30));
    assert_eq!(inst.memory_total_bytes, 20 * (1 << 30));
}

#[test]
fn mig_parser_is_empty_on_bare_metal_metrics() {
    use all_smi::network::metrics_parser::MetricsParser;

    let parser = MetricsParser::new();
    let non_mig = concat!(
        "all_smi_gpu_utilization{gpu=\"RTX\", instance=\"x\", uuid=\"GPU-1\", index=\"0\"} 10\n",
        "all_smi_cpu_utilization{cpu_model=\"AMD\", instance=\"x\", hostname=\"x\", index=\"0\"} 20\n",
    );

    let (gpu, _cpu, _mem, _storage, _vgpu, parsed) = parser.parse_metrics(non_mig, "x", &regex());
    assert_eq!(gpu.len(), 1, "sanity: GPU row still parsed");
    assert!(
        parsed.is_empty(),
        "MIG parser must stay quiet on bare-metal"
    );
}

#[test]
fn mig_parser_attaches_multiple_instances_to_same_host() {
    use all_smi::network::metrics_parser::MetricsParser;

    let mut text = String::new();
    text.push_str(concat!(
        "all_smi_gpu_mig_mode{gpu_index=\"0\", gpu_uuid=\"GPU-Z\", gpu=\"NVIDIA H100\", ",
        "instance=\"node-9\", host=\"node-9\"} 1\n",
    ));
    for i in 0..7 {
        // Realistic 7g.10gb partitioning.
        text.push_str(&format!(
            "all_smi_mig_instance_utilization_gpu{{gpu_index=\"0\", gpu_uuid=\"GPU-Z\", gpu=\"NVIDIA H100\", \
             instance=\"node-9\", host=\"node-9\", mig_instance=\"{i}\", mig_uuid=\"MIG-Z-{i}\", \
             mig_profile=\"1g.10gb\", gpu_instance_id=\"{}\", compute_instance_id=\"0\"}} {}\n",
            i + 1,
            10 * i
        ));
    }

    let parser = MetricsParser::new();
    let (_gpu, _cpu, _mem, _store, _vgpu, parsed) =
        parser.parse_metrics(&text, "node-9:9090", &regex());
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].instances.len(), 7);
    // Sorted by instance_id ascending — first must be id 0, last id 6.
    assert_eq!(parsed[0].instances[0].instance_id, 0);
    assert_eq!(parsed[0].instances[6].instance_id, 6);
}

#[test]
fn library_api_exposes_mig_info_method() {
    // Smoke test: simply verifying `AllSmi::get_mig_info` exists and returns
    // a Vec. On CI hosts without MIG-enabled NVIDIA GPUs the method returns
    // empty, which is the correct no-op behaviour we document.
    let smi = AllSmi::new().expect("AllSmi::new should not fail");
    let info: Vec<MigGpuInfo> = smi.get_mig_info();
    // We can't assert the Vec is empty because the CI host could theoretically
    // be a MIG-enabled GPU host. We just assert the call shape compiles and
    // does not panic.
    let _ = info;
}
