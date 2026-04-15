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

//! Integration tests for the NVIDIA extended-temperature / P-state pipeline
//! introduced in issue #130.
//!
//! Round-trip the Prometheus exporter-style output through the remote metrics
//! parser to assert that every new field survives the network hop. The
//! exposition strings below mirror what `api/metrics/gpu.rs` emits; if the
//! exporter changes metric names or label ordering, update these strings in
//! lockstep.

use all_smi::network::metrics_parser::MetricsParser;
use regex::Regex;

fn regex() -> Regex {
    Regex::new(r"^all_smi_([^\{]+)\{([^}]+)\} ([\d\.]+)$").unwrap()
}

fn full_exposition() -> String {
    concat!(
        "# HELP all_smi_gpu_utilization GPU utilization percentage\n",
        "# TYPE all_smi_gpu_utilization gauge\n",
        "all_smi_gpu_utilization{gpu=\"NVIDIA A100\", instance=\"node-7\", \
         uuid=\"GPU-RT\", index=\"0\"} 42.0\n",
        "# HELP all_smi_gpu_temperature_celsius GPU temperature in celsius\n",
        "# TYPE all_smi_gpu_temperature_celsius gauge\n",
        "all_smi_gpu_temperature_celsius{gpu=\"NVIDIA A100\", instance=\"node-7\", \
         uuid=\"GPU-RT\", index=\"0\"} 72\n",
        "# HELP all_smi_gpu_temperature_threshold_slowdown_celsius \
         GPU slowdown temperature threshold in Celsius\n",
        "# TYPE all_smi_gpu_temperature_threshold_slowdown_celsius gauge\n",
        "all_smi_gpu_temperature_threshold_slowdown_celsius{gpu=\"NVIDIA A100\", \
         instance=\"node-7\", uuid=\"GPU-RT\", index=\"0\"} 90\n",
        "# HELP all_smi_gpu_temperature_threshold_shutdown_celsius \
         GPU shutdown temperature threshold in Celsius\n",
        "# TYPE all_smi_gpu_temperature_threshold_shutdown_celsius gauge\n",
        "all_smi_gpu_temperature_threshold_shutdown_celsius{gpu=\"NVIDIA A100\", \
         instance=\"node-7\", uuid=\"GPU-RT\", index=\"0\"} 95\n",
        "# HELP all_smi_gpu_temperature_threshold_max_operating_celsius \
         GPU maximum operating temperature threshold in Celsius\n",
        "# TYPE all_smi_gpu_temperature_threshold_max_operating_celsius gauge\n",
        "all_smi_gpu_temperature_threshold_max_operating_celsius{gpu=\"NVIDIA A100\", \
         instance=\"node-7\", uuid=\"GPU-RT\", index=\"0\"} 85\n",
        "# HELP all_smi_gpu_temperature_threshold_acoustic_celsius \
         GPU acoustic (noise) temperature threshold in Celsius\n",
        "# TYPE all_smi_gpu_temperature_threshold_acoustic_celsius gauge\n",
        "all_smi_gpu_temperature_threshold_acoustic_celsius{gpu=\"NVIDIA A100\", \
         instance=\"node-7\", uuid=\"GPU-RT\", index=\"0\"} 77\n",
        "# HELP all_smi_gpu_performance_state \
         GPU performance state (0=P0 fastest, 15=P15 idlest; metric is omitted when the device does not report a P-state)\n",
        "# TYPE all_smi_gpu_performance_state gauge\n",
        "all_smi_gpu_performance_state{gpu=\"NVIDIA A100\", instance=\"node-7\", \
         uuid=\"GPU-RT\", index=\"0\"} 4\n",
    )
    .to_string()
}

fn partial_exposition() -> String {
    // Older driver: slowdown + shutdown known, the others absent.
    concat!(
        "all_smi_gpu_utilization{gpu=\"NVIDIA A100\", instance=\"node-7\", \
         uuid=\"GPU-RT\", index=\"0\"} 10.0\n",
        "all_smi_gpu_temperature_celsius{gpu=\"NVIDIA A100\", instance=\"node-7\", \
         uuid=\"GPU-RT\", index=\"0\"} 60\n",
        "all_smi_gpu_temperature_threshold_slowdown_celsius{gpu=\"NVIDIA A100\", \
         instance=\"node-7\", uuid=\"GPU-RT\", index=\"0\"} 91\n",
        "all_smi_gpu_temperature_threshold_shutdown_celsius{gpu=\"NVIDIA A100\", \
         instance=\"node-7\", uuid=\"GPU-RT\", index=\"0\"} 96\n",
    )
    .to_string()
}

#[test]
fn thermal_and_pstate_round_trip_preserves_all_fields() {
    let parser = MetricsParser::new();
    let (parsed, _, _, _, _, _) = parser.parse_metrics(&full_exposition(), "node-7:9090", &regex());

    assert_eq!(parsed.len(), 1, "expected exactly one GPU record");
    let gpu = &parsed[0];
    assert_eq!(gpu.uuid, "GPU-RT");
    assert_eq!(gpu.temperature, 72);
    assert_eq!(gpu.temperature_threshold_slowdown, Some(90));
    assert_eq!(gpu.temperature_threshold_shutdown, Some(95));
    assert_eq!(gpu.temperature_threshold_max_operating, Some(85));
    assert_eq!(gpu.temperature_threshold_acoustic, Some(77));
    assert_eq!(gpu.performance_state, Some(4));
}

#[test]
fn thermal_and_pstate_round_trip_handles_partial_reports() {
    // Older drivers: only slowdown + shutdown known. The others must stay
    // `None` after parse, not default to 0.
    let parser = MetricsParser::new();
    let (parsed, _, _, _, _, _) =
        parser.parse_metrics(&partial_exposition(), "node-7:9090", &regex());
    assert_eq!(parsed.len(), 1);
    let gpu = &parsed[0];
    assert_eq!(gpu.temperature_threshold_slowdown, Some(91));
    assert_eq!(gpu.temperature_threshold_shutdown, Some(96));
    assert!(gpu.temperature_threshold_max_operating.is_none());
    assert!(gpu.temperature_threshold_acoustic.is_none());
    assert!(gpu.performance_state.is_none());
}

#[test]
fn non_nvidia_path_leaves_fields_none() {
    // Parsing a scrape from a non-NVIDIA node must leave the new fields
    // `None` — i.e. the feature is a complete no-op on non-NVIDIA sources.
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
    assert!(gpu.temperature_threshold_slowdown.is_none());
    assert!(gpu.temperature_threshold_shutdown.is_none());
    assert!(gpu.temperature_threshold_max_operating.is_none());
    assert!(gpu.temperature_threshold_acoustic.is_none());
    assert!(gpu.performance_state.is_none());
}

#[test]
fn performance_state_omission_means_unavailable() {
    // Contract: when the device does not report a P-state, the exporter
    // omits the `all_smi_gpu_performance_state` line entirely (Prometheus
    // convention for "no data"). The parser MUST surface that absence as
    // `None` rather than synthesising a reading.
    //
    // We model the wire format directly: build a scrape with util +
    // temperature lines but no P-state line, and assert the parsed
    // record has `performance_state == None`.
    let parser = MetricsParser::new();
    let text = concat!(
        "all_smi_gpu_utilization{gpu=\"NVIDIA A100\", instance=\"node-7\", \
         uuid=\"GPU-X\", index=\"0\"} 10\n",
        "all_smi_gpu_temperature_celsius{gpu=\"NVIDIA A100\", instance=\"node-7\", \
         uuid=\"GPU-X\", index=\"0\"} 50\n",
    );
    let (parsed, _, _, _, _, _) = parser.parse_metrics(text, "node-7:9090", &regex());
    assert_eq!(parsed.len(), 1);
    assert!(parsed[0].performance_state.is_none());
}
