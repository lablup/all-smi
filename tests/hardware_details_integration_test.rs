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

//! Integration tests for the NVIDIA extended hardware-detail pipeline
//! introduced in issue #132: NUMA node id, GSP firmware mode / version,
//! NvLink remote device types, and GPM metrics.
//!
//! The tests build Prometheus-exposition snippets that exactly match the
//! strings produced by `api/metrics/hardware.rs`, then parse them back
//! through [`MetricsParser`] and assert every field survives the round
//! trip. If the exporter changes metric names, label ordering, or the
//! numeric encoding, update these strings in lockstep with that change.

use all_smi::device::NvLinkRemoteType;
use all_smi::network::metrics_parser::MetricsParser;
use regex::Regex;

fn regex() -> Regex {
    Regex::new(r"^all_smi_([^\{]+)\{([^}]+)\} ([\d\.]+)$").unwrap()
}

fn full_hardware_exposition() -> String {
    // Mirror what the exporter emits for a single NVIDIA A100 with:
    //   * NUMA node 0
    //   * GSP firmware enabled, version 550.54.15
    //   * 6 active NvLinks: 5 GPU-to-GPU + 1 GPU-to-Switch
    //   * GPM: SM occupancy 0.67, memory bandwidth util 0.42
    concat!(
        "all_smi_gpu_utilization{gpu=\"NVIDIA A100\", instance=\"node-9\", \
         uuid=\"GPU-HW\", index=\"0\"} 30.0\n",
        "all_smi_gpu_temperature_celsius{gpu=\"NVIDIA A100\", instance=\"node-9\", \
         uuid=\"GPU-HW\", index=\"0\"} 55\n",
        "# HELP all_smi_gpu_numa_node_id NUMA node the GPU is attached to\n",
        "# TYPE all_smi_gpu_numa_node_id gauge\n",
        "all_smi_gpu_numa_node_id{gpu=\"NVIDIA A100\", instance=\"node-9\", \
         uuid=\"GPU-HW\", index=\"0\"} 0\n",
        "# HELP all_smi_gpu_gsp_firmware_mode GSP firmware mode\n",
        "# TYPE all_smi_gpu_gsp_firmware_mode gauge\n",
        "all_smi_gpu_gsp_firmware_mode{gpu=\"NVIDIA A100\", instance=\"node-9\", \
         uuid=\"GPU-HW\", index=\"0\"} 1\n",
        "# HELP all_smi_gpu_gsp_firmware_version_info GSP firmware version\n",
        "# TYPE all_smi_gpu_gsp_firmware_version_info gauge\n",
        "all_smi_gpu_gsp_firmware_version_info{gpu=\"NVIDIA A100\", instance=\"node-9\", \
         uuid=\"GPU-HW\", index=\"0\", version=\"550.54.15\"} 1\n",
        "# HELP all_smi_nvlink_remote_device_type NvLink remote endpoint classification\n",
        "# TYPE all_smi_nvlink_remote_device_type gauge\n",
        "all_smi_nvlink_remote_device_type{gpu=\"NVIDIA A100\", instance=\"node-9\", \
         uuid=\"GPU-HW\", index=\"0\", link_index=\"0\", remote_type=\"gpu\"} 1\n",
        "all_smi_nvlink_remote_device_type{gpu=\"NVIDIA A100\", instance=\"node-9\", \
         uuid=\"GPU-HW\", index=\"0\", link_index=\"1\", remote_type=\"gpu\"} 1\n",
        "all_smi_nvlink_remote_device_type{gpu=\"NVIDIA A100\", instance=\"node-9\", \
         uuid=\"GPU-HW\", index=\"0\", link_index=\"2\", remote_type=\"gpu\"} 1\n",
        "all_smi_nvlink_remote_device_type{gpu=\"NVIDIA A100\", instance=\"node-9\", \
         uuid=\"GPU-HW\", index=\"0\", link_index=\"3\", remote_type=\"gpu\"} 1\n",
        "all_smi_nvlink_remote_device_type{gpu=\"NVIDIA A100\", instance=\"node-9\", \
         uuid=\"GPU-HW\", index=\"0\", link_index=\"4\", remote_type=\"gpu\"} 1\n",
        "all_smi_nvlink_remote_device_type{gpu=\"NVIDIA A100\", instance=\"node-9\", \
         uuid=\"GPU-HW\", index=\"0\", link_index=\"5\", remote_type=\"switch\"} 1\n",
        "# HELP all_smi_gpu_sm_occupancy GPM SM occupancy\n",
        "# TYPE all_smi_gpu_sm_occupancy gauge\n",
        "all_smi_gpu_sm_occupancy{gpu=\"NVIDIA A100\", instance=\"node-9\", \
         uuid=\"GPU-HW\", index=\"0\"} 0.67\n",
        "# HELP all_smi_gpu_memory_bandwidth_utilization GPM memory bandwidth util\n",
        "# TYPE all_smi_gpu_memory_bandwidth_utilization gauge\n",
        "all_smi_gpu_memory_bandwidth_utilization{gpu=\"NVIDIA A100\", instance=\"node-9\", \
         uuid=\"GPU-HW\", index=\"0\"} 0.42\n",
    )
    .to_string()
}

#[test]
fn hardware_detail_round_trip_preserves_all_fields() {
    let parser = MetricsParser::new();
    let (parsed, _, _, _, _, _) =
        parser.parse_metrics(&full_hardware_exposition(), "node-9:9090", &regex());

    assert_eq!(parsed.len(), 1, "expected exactly one GPU record");
    let gpu = &parsed[0];
    assert_eq!(gpu.uuid, "GPU-HW");
    assert_eq!(gpu.numa_node_id, Some(0));
    assert_eq!(gpu.gsp_firmware_mode, Some(1));
    assert_eq!(gpu.gsp_firmware_version.as_deref(), Some("550.54.15"));

    // NvLinks: 6 total, 5 gpu + 1 switch. Parser preserves link_index.
    assert_eq!(gpu.nvlink_remote_devices.len(), 6);
    let gpu_count = gpu
        .nvlink_remote_devices
        .iter()
        .filter(|l| l.remote_type == NvLinkRemoteType::Gpu)
        .count();
    let switch_count = gpu
        .nvlink_remote_devices
        .iter()
        .filter(|l| l.remote_type == NvLinkRemoteType::Switch)
        .count();
    assert_eq!(gpu_count, 5);
    assert_eq!(switch_count, 1);
    let link_indices: Vec<u32> = gpu
        .nvlink_remote_devices
        .iter()
        .map(|l| l.link_index)
        .collect();
    for expected in 0..6u32 {
        assert!(
            link_indices.contains(&expected),
            "missing link {expected} in {link_indices:?}"
        );
    }

    // GPM metrics: SM occupancy and memory BW util populated.
    let gpm = gpu.gpm_metrics.as_ref().expect("GPM metrics present");
    assert!((gpm.sm_occupancy.unwrap() - 0.67).abs() < 1e-4);
    assert!((gpm.memory_bandwidth_utilization.unwrap() - 0.42).abs() < 1e-4);
}

#[test]
fn hardware_detail_partial_scrape_preserves_absence() {
    // Older driver: only NUMA node id is exposed. GSP firmware, NvLink,
    // and GPM fields stay at the "unavailable" defaults.
    let partial = concat!(
        "all_smi_gpu_utilization{gpu=\"NVIDIA A100\", instance=\"node-9\", \
         uuid=\"GPU-OLD\", index=\"0\"} 10\n",
        "all_smi_gpu_temperature_celsius{gpu=\"NVIDIA A100\", instance=\"node-9\", \
         uuid=\"GPU-OLD\", index=\"0\"} 50\n",
        "all_smi_gpu_numa_node_id{gpu=\"NVIDIA A100\", instance=\"node-9\", \
         uuid=\"GPU-OLD\", index=\"0\"} 1\n",
    );
    let parser = MetricsParser::new();
    let (parsed, _, _, _, _, _) = parser.parse_metrics(partial, "node-9:9090", &regex());
    assert_eq!(parsed.len(), 1);
    let gpu = &parsed[0];
    assert_eq!(gpu.numa_node_id, Some(1));
    assert!(gpu.gsp_firmware_mode.is_none());
    assert!(gpu.gsp_firmware_version.is_none());
    assert!(gpu.nvlink_remote_devices.is_empty());
    assert!(gpu.gpm_metrics.is_none());
}

#[test]
fn non_nvidia_path_leaves_hardware_fields_unavailable() {
    // A scrape from an Apple Silicon / AMD node emits no hardware-detail
    // lines; the resulting `GpuInfo` must carry all new fields as their
    // "unavailable" defaults so the TUI renders nothing.
    let non_nvidia = concat!(
        "all_smi_gpu_utilization{gpu=\"Apple M2 Pro\", instance=\"mac-1\", \
         uuid=\"APPLE-0\", index=\"0\"} 20\n",
        "all_smi_gpu_temperature_celsius{gpu=\"Apple M2 Pro\", instance=\"mac-1\", \
         uuid=\"APPLE-0\", index=\"0\"} 55\n",
    );
    let parser = MetricsParser::new();
    let (parsed, _, _, _, _, _) = parser.parse_metrics(non_nvidia, "mac-1:9090", &regex());
    assert_eq!(parsed.len(), 1);
    let gpu = &parsed[0];
    assert!(gpu.numa_node_id.is_none());
    assert!(gpu.gsp_firmware_mode.is_none());
    assert!(gpu.gsp_firmware_version.is_none());
    assert!(gpu.nvlink_remote_devices.is_empty());
    assert!(gpu.gpm_metrics.is_none());
}

#[test]
fn nvlink_unknown_remote_type_is_preserved() {
    // The exporter may label a link with `remote_type="unknown"` when the
    // driver does not classify the endpoint; the parser must propagate
    // that classification rather than silently dropping the link.
    let text = concat!(
        "all_smi_gpu_utilization{gpu=\"NVIDIA H100\", instance=\"node-1\", \
         uuid=\"GPU-U\", index=\"0\"} 10\n",
        "all_smi_nvlink_remote_device_type{gpu=\"NVIDIA H100\", instance=\"node-1\", \
         uuid=\"GPU-U\", index=\"0\", link_index=\"0\", remote_type=\"unknown\"} 1\n",
        "all_smi_nvlink_remote_device_type{gpu=\"NVIDIA H100\", instance=\"node-1\", \
         uuid=\"GPU-U\", index=\"0\", link_index=\"1\", remote_type=\"ibmnpu\"} 1\n",
    );
    let parser = MetricsParser::new();
    let (parsed, _, _, _, _, _) = parser.parse_metrics(text, "node-1:9090", &regex());
    assert_eq!(parsed.len(), 1);
    let gpu = &parsed[0];
    assert_eq!(gpu.nvlink_remote_devices.len(), 2);
    assert!(
        gpu.nvlink_remote_devices
            .iter()
            .any(|l| l.remote_type == NvLinkRemoteType::Unknown)
    );
    assert!(
        gpu.nvlink_remote_devices
            .iter()
            .any(|l| l.remote_type == NvLinkRemoteType::IbmNpu)
    );
}

#[test]
fn parser_caps_nvlinks_at_max_per_gpu() {
    // A malicious remote exporter could emit more NvLinks than physically
    // possible. The parser must bound the per-GPU vector to avoid an
    // unbounded allocation. Current ceiling is 32 (see
    // `MAX_NVLINK_PER_GPU` in network/metrics_parser.rs); we test by
    // emitting 40 distinct link indices — the first 32 must be accepted,
    // the rest dropped.
    let mut lines = String::new();
    lines.push_str(
        "all_smi_gpu_utilization{gpu=\"NVIDIA H100\", instance=\"node-a\", \
         uuid=\"GPU-CAP\", index=\"0\"} 10\n",
    );
    for link in 0..40u32 {
        lines.push_str(&format!(
            "all_smi_nvlink_remote_device_type{{gpu=\"NVIDIA H100\", instance=\"node-a\", \
             uuid=\"GPU-CAP\", index=\"0\", link_index=\"{link}\", remote_type=\"gpu\"}} 1\n"
        ));
    }
    let parser = MetricsParser::new();
    let (parsed, _, _, _, _, _) = parser.parse_metrics(&lines, "node-a:9090", &regex());
    assert_eq!(parsed.len(), 1);
    assert!(
        parsed[0].nvlink_remote_devices.len() <= 32,
        "parser failed to cap NvLinks: got {}",
        parsed[0].nvlink_remote_devices.len()
    );
}

#[test]
fn parser_rejects_out_of_range_gsp_firmware_mode() {
    // Exporter emits 0/1/2 only. A buggy upstream that sends 99 must leave
    // the field as `None` so dashboards never render a bogus mode.
    let text = concat!(
        "all_smi_gpu_utilization{gpu=\"NVIDIA A100\", instance=\"node-1\", \
         uuid=\"GPU-B\", index=\"0\"} 10\n",
        "all_smi_gpu_gsp_firmware_mode{gpu=\"NVIDIA A100\", instance=\"node-1\", \
         uuid=\"GPU-B\", index=\"0\"} 99\n",
    );
    let parser = MetricsParser::new();
    let (parsed, _, _, _, _, _) = parser.parse_metrics(text, "node-1:9090", &regex());
    assert_eq!(parsed.len(), 1);
    assert!(parsed[0].gsp_firmware_mode.is_none());
}

#[test]
fn parser_rejects_out_of_range_gpm_fractions() {
    // GPM gauges are 0..=1 fractions. A malicious 9.9 emission must be
    // dropped rather than rendered as 990% occupancy.
    let text = concat!(
        "all_smi_gpu_utilization{gpu=\"NVIDIA H100\", instance=\"node-1\", \
         uuid=\"GPU-R\", index=\"0\"} 10\n",
        "all_smi_gpu_sm_occupancy{gpu=\"NVIDIA H100\", instance=\"node-1\", \
         uuid=\"GPU-R\", index=\"0\"} 9.9\n",
        "all_smi_gpu_memory_bandwidth_utilization{gpu=\"NVIDIA H100\", instance=\"node-1\", \
         uuid=\"GPU-R\", index=\"0\"} 9.9\n",
    );
    let parser = MetricsParser::new();
    let (parsed, _, _, _, _, _) = parser.parse_metrics(text, "node-1:9090", &regex());
    assert_eq!(parsed.len(), 1);
    // Neither GPM field accepted → no `GpmMetrics` allocated.
    assert!(parsed[0].gpm_metrics.is_none());
}

#[test]
fn parser_gpm_accepts_only_the_fields_present() {
    // A partial GPM emission (only SM occupancy) must populate that field
    // and leave the other as `None`, matching the exporter's "emit only
    // when you have data" contract.
    let text = concat!(
        "all_smi_gpu_utilization{gpu=\"NVIDIA H100\", instance=\"node-1\", \
         uuid=\"GPU-P\", index=\"0\"} 10\n",
        "all_smi_gpu_sm_occupancy{gpu=\"NVIDIA H100\", instance=\"node-1\", \
         uuid=\"GPU-P\", index=\"0\"} 0.55\n",
    );
    let parser = MetricsParser::new();
    let (parsed, _, _, _, _, _) = parser.parse_metrics(text, "node-1:9090", &regex());
    assert_eq!(parsed.len(), 1);
    let gpm = parsed[0].gpm_metrics.as_ref().expect("GPM populated");
    assert!((gpm.sm_occupancy.unwrap() - 0.55).abs() < 1e-4);
    assert!(gpm.memory_bandwidth_utilization.is_none());
}
