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

//! Issue #500: the tokenizer + borrowed-label parser must produce exactly
//! what the regex + `HashMap` parser produced, and over-long label strings
//! must no longer vanish silently.

use std::collections::HashMap;

use regex::Regex;
use serde_json::Value;

use super::accumulator::{MAX_LABEL_STRING_BYTES, OverlongLines, ParseAccumulator};
use super::{MetricsParser, ParsedRemoteMetrics};
use crate::api::metrics::MetricExporter;
use crate::device::{GpuInfo, MigGpuInfo, MigInstanceInfo, VgpuHostInfo, VgpuInfo};
use crate::network::prometheus_labels::Labels;
use crate::network::prometheus_labels::tests::legacy_parse_labels;
use crate::parsing::prometheus::split_prometheus_line;

const LEGACY_PATTERN: &str = r"^all_smi_([^\{]+)\{([^}]+)\} ([\d\.]+)$";

/// The pre-#500 pipeline: regex tokenizer with the old 256-byte name and
/// 1024-byte label-string caps, and the owned `HashMap` label parser,
/// routed through the same accumulator.
fn legacy_parse(text: &str, host: &str) -> ParsedRemoteMetrics {
    let re = Regex::new(LEGACY_PATTERN).unwrap();
    let parser = MetricsParser::new();
    let mut acc = ParseAccumulator::new();
    for line in text.lines() {
        let Some(caps) = re.captures(line.trim()) else {
            continue;
        };
        let (name, labels_str) = (&caps[1], &caps[2]);
        let Ok(value) = caps[3].parse::<f64>() else {
            continue;
        };
        if name.len() > 256 || labels_str.len() > 1024 {
            continue;
        }
        let labels = Labels::from_pairs(legacy_parse_labels(labels_str));
        acc.process_line(&parser, name, &labels, value, host);
    }
    acc.finish(&parser)
}

/// Order-independent rendering of a JSON value: object keys sorted, and the
/// wall-clock `time` stamp each parse writes removed.
fn canonical(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<(&String, &Value)> =
                map.iter().filter(|(k, _)| k.as_str() != "time").collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            let body: Vec<String> = entries
                .into_iter()
                .map(|(k, v)| format!("{k:?}:{}", canonical(v)))
                .collect();
            format!("{{{}}}", body.join(","))
        }
        Value::Array(items) => {
            let body: Vec<String> = items.iter().map(canonical).collect();
            format!("[{}]", body.join(","))
        }
        other => other.to_string(),
    }
}

fn canonical_rows<T: serde::Serialize>(rows: &[T]) -> Vec<String> {
    let mut out: Vec<String> = rows
        .iter()
        .map(|row| canonical(&serde_json::to_value(row).unwrap()))
        .collect();
    out.sort();
    out
}

fn assert_same(name: &str, text: &str) {
    let host = "fixture-host:9090";
    let new = MetricsParser::new().parse_metrics_with_chassis(text, host);
    let old = legacy_parse(text, host);
    let (n, o) = (&new.metrics, &old.metrics);
    assert_eq!(
        canonical_rows(&n.gpu_info),
        canonical_rows(&o.gpu_info),
        "{name}: gpu"
    );
    assert_eq!(
        canonical_rows(&n.cpu_info),
        canonical_rows(&o.cpu_info),
        "{name}: cpu"
    );
    assert_eq!(
        canonical_rows(&n.memory_info),
        canonical_rows(&o.memory_info),
        "{name}: memory"
    );
    assert_eq!(
        canonical_rows(&n.storage_info),
        canonical_rows(&o.storage_info),
        "{name}: storage"
    );
    assert_eq!(
        canonical_rows(&n.vgpu_info),
        canonical_rows(&o.vgpu_info),
        "{name}: vgpu"
    );
    assert_eq!(
        canonical_rows(&n.mig_info),
        canonical_rows(&o.mig_info),
        "{name}: mig"
    );
    assert_eq!(
        canonical_rows(&new.chassis_info),
        canonical_rows(&old.chassis_info),
        "{name}: chassis"
    );
    let mut np = n.process_info.clone();
    let mut op = o.process_info.clone();
    np.sort_by_key(|r| (r.pid, r.gpu_index));
    op.sort_by_key(|r| (r.pid, r.gpu_index));
    assert_eq!(np, op, "{name}: process");
}

fn vgpu_host() -> VgpuHostInfo {
    VgpuHostInfo {
        host_id: "node-42".to_string(),
        hostname: "node-42".to_string(),
        instance: "node-42".to_string(),
        gpu_index: 3,
        gpu_uuid: "GPU-A".to_string(),
        gpu_name: "NVIDIA A100".to_string(),
        host_mode: "Sriov".to_string(),
        scheduler_policy: 1,
        scheduler_arr_mode: 2,
        is_arr_supported: true,
        vgpus: vec![VgpuInfo {
            instance_id: 10,
            uuid: "GRID-10".to_string(),
            vm_id: "vm-node-01".to_string(),
            vgpu_type_name: "GRID A100-8C".to_string(),
            fb_used_bytes: 3 << 30,
            fb_total_bytes: 8 << 30,
            gpu_utilization: Some(64),
            memory_utilization: Some(45),
            is_active: true,
        }],
        detail: HashMap::new(),
    }
}

fn mig_host() -> MigGpuInfo {
    MigGpuInfo {
        host_id: "node-42".to_string(),
        hostname: "node-42".to_string(),
        instance: "node-42".to_string(),
        gpu_index: 3,
        gpu_uuid: "GPU-MIG".to_string(),
        gpu_name: "NVIDIA A100".to_string(),
        mig_mode: true,
        instances: vec![MigInstanceInfo {
            instance_id: 2,
            gpu_instance_id: Some(5),
            compute_instance_id: Some(0),
            uuid: "MIG-2".to_string(),
            profile_name: "3g.20gb".to_string(),
            utilization_gpu: Some(64),
            utilization_memory: Some(18),
            memory_used_bytes: 8 << 30,
            memory_total_bytes: 20 << 30,
        }],
    }
}

/// A discrete NVIDIA GPU carrying every detail key the NVML reader
/// (`create_device_detail` in `device/readers/nvidia.rs`) and the vGPU
/// reader can set, with realistic worst-case lengths.
fn fully_detailed_nvidia_gpu() -> GpuInfo {
    let detail: HashMap<String, String> = [
        ("Driver Version", "580.82.07"),
        ("CUDA Version", "13.0"),
        ("lib_name", "CUDA"),
        ("lib_version", "13.0"),
        ("Brand", "NvidiaRtx"),
        ("architecture", "Blackwell"),
        ("pcie_gen_current", "5"),
        ("pcie_width_current", "16"),
        ("pcie_gen_max", "5"),
        ("pcie_width_max", "16"),
        ("compute_mode", "ExclusiveProcess"),
        ("performance_state", "Fifteen"),
        ("power_limit_current", "1000.00"),
        ("power_limit_default", "1000.00"),
        ("power_limit_min", "200.00"),
        ("power_limit_max", "1000.00"),
        ("clock_graphics_max", "1980"),
        ("clock_memory_max", "2619"),
        ("ecc_mode_current", "Disabled"),
        ("ecc_mode_pending", "Enabled"),
        ("mig_mode_current", "Disabled"),
        ("mig_mode_pending", "Enabled"),
        ("vbios_version", "96.00.89.00.01"),
        ("vgpu_capable", "true"),
        ("arr_supported", "true"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    GpuInfo {
        uuid: "GPU-5b1c6e04-8d0f-6a3e-9b43-2f6b1a7c9d10".to_string(),
        time: String::new(),
        name: "NVIDIA RTX PRO 6000 Blackwell Server Edition".to_string(),
        device_type: "GPU".to_string(),
        host_id: "gpu-node-0123.cluster.example.internal".to_string(),
        hostname: "gpu-node-0123.cluster.example.internal".to_string(),
        instance: "gpu-node-0123.cluster.example.internal".to_string(),
        utilization: 50.0,
        ane_utilization: 0.0,
        dla_utilization: None,
        tensorcore_utilization: None,
        temperature: 70,
        used_memory: 1024,
        total_memory: 8192,
        frequency: 1500,
        power_consumption: 200.0,
        gpu_core_count: None,
        temperature_threshold_slowdown: None,
        temperature_threshold_shutdown: None,
        temperature_threshold_max_operating: None,
        temperature_threshold_acoustic: None,
        performance_state: None,
        fan_speed_rpm: None,
        numa_node_id: None,
        gsp_firmware_mode: None,
        gsp_firmware_version: None,
        nvlink_remote_devices: Vec::new(),
        gpm_metrics: None,
        detail,
    }
}

fn longest_label_string(text: &str) -> (usize, String) {
    text.lines()
        .filter_map(split_prometheus_line)
        .map(|(name, labels, _)| (labels.len(), name.to_string()))
        .max()
        .unwrap_or_default()
}

#[test]
fn new_parser_matches_legacy_on_mock_templates() {
    for (name, text) in crate::parsing::prometheus_fixtures::ALL {
        assert_same(name, text);
    }
}

#[test]
fn new_parser_matches_legacy_on_exporter_output() {
    let vgpu = vec![vgpu_host()];
    assert_same(
        "vgpu exporter",
        &crate::api::metrics::vgpu::VgpuMetricExporter::new(&vgpu).export_metrics(),
    );
    let mig = vec![mig_host()];
    assert_same(
        "mig exporter",
        &crate::api::metrics::mig::MigMetricExporter::new(&mig).export_metrics(),
    );
    let gpus = vec![fully_detailed_nvidia_gpu()];
    assert_same(
        "nvidia gpu exporter",
        &crate::api::metrics::gpu::GpuMetricExporter::new(&gpus).export_metrics(),
    );
}

/// Item 6 measurement: the longest label string each mock template and a
/// fully detailed NVIDIA `all_smi_gpu_info` line produce. Run with
/// `--nocapture` to print the table recorded in the PR.
#[test]
fn measured_label_lengths_fit_the_line_cap() {
    let mut rows: Vec<(String, usize, String)> = crate::parsing::prometheus_fixtures::ALL
        .iter()
        .map(|(name, text)| {
            let (len, metric) = longest_label_string(text);
            (format!("mock {name}"), len, metric)
        })
        .collect();

    let gpus = vec![fully_detailed_nvidia_gpu()];
    let exported = crate::api::metrics::gpu::GpuMetricExporter::new(&gpus).export_metrics();
    let (len, metric) = longest_label_string(&exported);
    rows.push(("NVIDIA GpuInfo, every detail key".to_string(), len, metric));

    for (source, len, metric) in &rows {
        eprintln!("| {source} | {len} | all_smi_{metric} |");
        assert!(*len <= MAX_LABEL_STRING_BYTES, "{source}: {len} bytes");
    }

    // The fully detailed line (769 bytes when measured) leaves too little
    // margin under the old 1024-byte cap; it must round-trip with the
    // identity labels the viewer reads from it.
    let parsed = MetricsParser::new().parse_metrics_with_chassis(&exported, "n:9090");
    let gpu = &parsed.metrics.gpu_info[0];
    for key in ["driver_version", "cuda_version", "architecture"] {
        assert!(
            gpu.detail.contains_key(key),
            "missing {key}: {:?}",
            gpu.detail
        );
    }
}

#[test]
fn gpu_info_line_over_old_cap_keeps_its_device_and_details() {
    let mut labels = vec![
        r#"gpu="Big GPU""#.to_string(),
        r#"instance="node-x""#.to_string(),
        r#"gpu_uuid="GPU-BIG""#.to_string(),
        r#"gpu_index="0""#.to_string(),
        r#"type="GPU""#.to_string(),
    ];
    for i in 0..40 {
        labels.push(format!(
            r#"detail_key_{i:02}="value-{i:02}-padding-padding""#
        ));
    }
    // Identity labels the viewer keeps, placed past byte 1024.
    labels.push(r#"driver_version="580.82.07""#.to_string());
    labels.push(r#"architecture="Hopper""#.to_string());
    let label_string = labels.join(", ");
    assert!(label_string.len() > 1500, "{}", label_string.len());
    let text = format!("all_smi_gpu_info{{{label_string}}} 1\n");

    let mut dropped = OverlongLines::default();
    let parsed = MetricsParser::new().parse_lines(&text, "n:9090", &mut dropped);
    assert_eq!(dropped.count(), 0);
    let gpu = &parsed.metrics.gpu_info[0];
    assert_eq!(gpu.uuid, "GPU-BIG");
    assert_eq!(gpu.name, "Big GPU");
    assert_eq!(
        gpu.detail.get("driver_version").map(String::as_str),
        Some("580.82.07")
    );
    assert_eq!(
        gpu.detail.get("architecture").map(String::as_str),
        Some("Hopper")
    );
    // The old 1024-byte cap dropped this line, and with it the device.
    assert!(legacy_parse(&text, "n:9090").metrics.gpu_info.is_empty());
}

#[test]
fn line_over_the_cap_is_counted_not_silently_lost() {
    let padding = "x".repeat(1000);
    let labels: Vec<String> = (0..40).map(|i| format!(r#"k{i}="{padding}""#)).collect();
    let text = format!(
        "all_smi_gpu_info{{gpu=\"G\", gpu_uuid=\"U\", {}}} 1\nall_smi_gpu_utilization{{gpu=\"G\", gpu_uuid=\"U\"}} 5\n",
        labels.join(",")
    );
    let mut dropped = OverlongLines::default();
    let parsed = MetricsParser::new().parse_lines(&text, "n:9090", &mut dropped);
    assert_eq!(dropped.count(), 1);
    // The other line still parses.
    assert_eq!(parsed.metrics.gpu_info[0].utilization, 5.0);

    let long_name = format!("all_smi_{}{{gpu=\"G\"}} 1\n", "n".repeat(300));
    let mut dropped = OverlongLines::default();
    MetricsParser::new().parse_lines(&long_name, "n:9090", &mut dropped);
    assert_eq!(dropped.count(), 1);
}
