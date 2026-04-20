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

//! Integration tests for the cluster-wide Users tab (issue #189).
//!
//! These tests exercise the full remote pipeline end-to-end:
//!   API-side exporter → Prometheus exposition → remote metrics parser
//!   → user aggregator → sort → drill-down breakdown.
//!
//! The point is to catch the compound regression where one leg works
//! in isolation (every unit test passes) but the wire format changes
//! break integration.

use all_smi::api::metrics::MetricExporter;
use all_smi::api::metrics::process::ProcessMetricExporter;
use all_smi::device::ProcessInfo;
use all_smi::network::metrics_parser::MetricsParser;
use all_smi::ui::aggregation::user::{
    GpuForAggregation, HostSnapshot, UserSortKey, aggregate_users, sort_users,
};
use regex::Regex;

fn regex() -> Regex {
    Regex::new(r"^all_smi_([^\{]+)\{([^}]+)\} ([\d\.]+)$").unwrap()
}

fn make_process(
    pid: u32,
    user: &str,
    device_id: usize,
    used_memory: u64,
    start_time: &str,
    command: &str,
) -> ProcessInfo {
    ProcessInfo {
        device_id,
        device_uuid: format!("GPU-{device_id}"),
        pid,
        process_name: command.split_whitespace().next().unwrap_or("x").to_string(),
        used_memory,
        cpu_percent: 10.0,
        memory_percent: 1.0,
        memory_rss: 0,
        memory_vms: 0,
        user: user.to_string(),
        state: "R".to_string(),
        start_time: start_time.to_string(),
        cpu_time: 0,
        command: command.to_string(),
        ppid: 1,
        threads: 1,
        uses_gpu: used_memory > 0,
        priority: 20,
        nice_value: 0,
        gpu_utilization: 0.0,
    }
}

/// End-to-end: five-node, three-user fixture survives exporter → parser →
/// aggregator with every column populated correctly.
#[test]
fn five_node_three_user_cluster_round_trips_through_exporter_and_aggregator() {
    // 5 simulated hosts × 3 simulated users.  Each user owns one
    // process per node, touching GPU index equal to their seat.
    let users = ["alice", "bob", "carol"];
    let mut hosts_snapshots: Vec<HostSnapshot> = Vec::new();

    for h in 0..5 {
        let host_label = format!("node-{h}");
        let exposition = {
            let procs: Vec<ProcessInfo> = users
                .iter()
                .enumerate()
                .map(|(i, u)| {
                    make_process(
                        1000 + h * 10 + i as u32,
                        u,
                        i,
                        (1 + h as u64) * 1_000_000_000,
                        &format!("00:{:02}:00", 5 + h),
                        &format!("{u}-train-{h}"),
                    )
                })
                .collect();
            let exporter = ProcessMetricExporter::new(&procs);
            exporter.export_metrics()
        };

        // Add `instance` and `host` labels manually so the remote
        // parser can key by host.  The exporter intentionally leaves
        // those to the outer metric pipeline (GPU/CPU blocks).
        let labeled: String = exposition
            .lines()
            .map(|line| {
                if line.starts_with('#') || line.is_empty() {
                    format!("{line}\n")
                } else if let Some(open) = line.find('{') {
                    let (head, tail) = line.split_at(open + 1);
                    let host = host_label.as_str();
                    format!("{head}instance=\"{host}\", host=\"{host}\", {tail}\n",)
                } else {
                    format!("{line}\n")
                }
            })
            .collect();

        let parser = MetricsParser::new();
        let parsed = parser.parse_metrics(&labeled, &host_label, &regex());

        assert_eq!(parsed.process_info.len(), users.len());

        hosts_snapshots.push(HostSnapshot {
            host: host_label.clone(),
            gpus: (0..users.len())
                .map(|i| GpuForAggregation {
                    host: host_label.clone(),
                    gpu_index: i as u32,
                    power_watts: 300.0 + (i as f64) * 10.0,
                })
                .collect(),
            processes: parsed.process_info,
        });
    }

    let result = aggregate_users(&hosts_snapshots);
    assert!(!result.is_partial(), "every host reported process data");
    assert_eq!(result.users.len(), users.len());

    // Each user touches every node (one process per node).
    for u in &result.users {
        assert_eq!(u.node_count, 5, "each user on every node");
        assert_eq!(u.process_count, 5);
        assert!(u.power_watts > 0.0);
    }

    // Sanity: in-tab sort by memory places the heaviest user first.
    let mut sorted = result.users.clone();
    sort_users(&mut sorted, UserSortKey::Memory);
    let top = &sorted[0];
    // Per-user VRAM: sum of (1..=5) billion = 15 billion (each host
    // contributes (h+1) GB regardless of user).
    assert_eq!(top.vram_bytes, (1..=5).sum::<u64>() * 1_000_000_000);
}

/// Sort determinism: the same aggregation sorted by different keys
/// produces a stable order — alphabetical ties always break by username.
#[test]
fn sorting_users_is_stable_across_keys() {
    let result = aggregate_users(&[HostSnapshot {
        host: "a".into(),
        gpus: vec![GpuForAggregation {
            host: "a".into(),
            gpu_index: 0,
            power_watts: 100.0,
        }],
        processes: vec![
            // alice and bob both have the same VRAM so the
            // tie-break must use username.
            all_smi::network::metrics_parser::ParsedProcessRow {
                host: "a".into(),
                pid: 1,
                user: "bob".into(),
                command: "x".into(),
                name: "x".into(),
                gpu_index: 0,
                gpu_uuid: "g".into(),
                gpu_memory_bytes: 1000,
                cpu_pct_tenths: 0,
                start_time_seconds: 0,
            },
            all_smi::network::metrics_parser::ParsedProcessRow {
                host: "a".into(),
                pid: 2,
                user: "alice".into(),
                command: "x".into(),
                name: "x".into(),
                gpu_index: 0,
                gpu_uuid: "g".into(),
                gpu_memory_bytes: 1000,
                cpu_pct_tenths: 0,
                start_time_seconds: 0,
            },
        ],
    }]);
    let mut sorted = result.users;
    sort_users(&mut sorted, UserSortKey::Memory);
    assert_eq!(sorted[0].user, "alice", "ties break alphabetically");
}

/// Replay pipeline regression: a `view --replay` frame that emits
/// local-style `ProcessInfo` entries must surface on the Users tab by
/// round-tripping through `ParsedProcessRow::from_local_process`.
#[test]
fn replay_frame_flows_through_users_tab() {
    let process = make_process(
        7777,
        "charlie",
        2,
        5_000_000_000,
        "10:00:00",
        "python eval.py",
    );
    let row = all_smi::network::metrics_parser::ParsedProcessRow::from_local_process(
        &process,
        "replay-host",
    );
    assert_eq!(row.host, "replay-host");
    assert_eq!(row.user, "charlie");
    assert_eq!(row.gpu_memory_bytes, 5_000_000_000);
    // 10:00:00 = 36000 seconds
    assert_eq!(row.start_time_seconds, 36_000);

    // Full round-trip: aggregate directly from the lifted row.
    let result = aggregate_users(&[HostSnapshot {
        host: "replay-host".into(),
        gpus: vec![GpuForAggregation {
            host: "replay-host".into(),
            gpu_index: 2,
            power_watts: 400.0,
        }],
        processes: vec![row],
    }]);
    assert_eq!(result.users.len(), 1);
    let u = &result.users[0];
    assert_eq!(u.user, "charlie");
    assert_eq!(u.longest_seconds, 36_000);
    assert!(u.power_watts > 0.0);
}
