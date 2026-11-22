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

//! Tests for the `?include=` filter and the snapshot body serializer.

use super::*;
use crate::snapshot::Snapshot;

fn sample_snapshot() -> Snapshot {
    Snapshot {
        schema: 1,
        timestamp: "2026-04-20T00:00:00Z".to_string(),
        hostname: "host".to_string(),
        gpus: Some(Vec::new()),
        cpus: Some(Vec::new()),
        memory: Some(Vec::new()),
        chassis: Some(Vec::new()),
        processes: None,
        storage: None,
        errors: Vec::new(),
    }
}

#[test]
fn parse_include_default_when_missing() {
    let f = parse_include(None);
    assert_eq!(f, SectionFilter::default_http());
}

#[test]
fn parse_include_default_when_empty() {
    let f = parse_include(Some(""));
    assert_eq!(f, SectionFilter::default_http());
    let f = parse_include(Some("   "));
    assert_eq!(f, SectionFilter::default_http());
}

#[test]
fn parse_include_accepts_gpu_only() {
    let f = parse_include(Some("gpu"));
    assert!(f.gpu);
    assert!(!f.cpu);
    assert!(!f.memory);
    assert!(!f.chassis);
    assert!(!f.process);
    assert!(!f.storage);
}

#[test]
fn parse_include_accepts_aliases() {
    let f = parse_include(Some("gpus,cpus,processes,disk"));
    assert!(f.gpu);
    assert!(f.cpu);
    assert!(f.process);
    assert!(f.storage);
}

#[test]
fn parse_include_unknown_only_falls_back_to_default() {
    let f = parse_include(Some("bogus"));
    assert_eq!(f, SectionFilter::default_http());
}

#[test]
fn filter_removes_unrequested_sections() {
    let snap = sample_snapshot();
    let filter = parse_include(Some("gpu"));
    let value = filter_snapshot_value(&snap, &filter);
    assert!(value.get("gpus").is_some());
    assert!(value.get("cpus").is_none());
    assert!(value.get("memory").is_none());
    assert!(value.get("chassis").is_none());
    // Metadata is always kept.
    assert_eq!(value["schema"], serde_json::json!(1));
    assert_eq!(
        value["timestamp"],
        serde_json::json!("2026-04-20T00:00:00Z")
    );
}

#[test]
fn filter_keeps_errors_array_regardless_of_section_filter() {
    // Errors are snapshot metadata, not a device section, and must be
    // preserved even when only one section is requested so clients
    // can still see reader failures.
    let mut snap = sample_snapshot();
    snap.errors.push(crate::snapshot::SnapshotError {
        section: "gpu".to_string(),
        kind: "timeout".to_string(),
        message: "fake".to_string(),
    });
    let filter = parse_include(Some("gpu"));
    let value = filter_snapshot_value(&snap, &filter);
    assert!(value["errors"].is_array());
    assert_eq!(value["errors"].as_array().unwrap().len(), 1);
}

/// Regression for the security review of #193: a process with a 4
/// KiB command line must not appear verbatim in the JSON body. The
/// Prometheus exporter already caps these labels (see
/// `api::metrics::process`); the JSON/SSE surface must inherit the
/// same guarantee so an attacker cannot exfiltrate secrets embedded
/// in argv through the cross-origin SSE stream.
#[test]
fn filter_truncates_long_process_command_line() {
    use crate::device::ProcessInfo;

    let long_cmd = "python ".to_string() + &"A".repeat(4096);
    let proc = ProcessInfo {
        device_id: 0,
        device_uuid: "uuid-0".to_string(),
        pid: 42,
        process_name: "python".to_string(),
        used_memory: 0,
        cpu_percent: 0.0,
        memory_percent: 0.0,
        memory_rss: 0,
        memory_vms: 0,
        user: "alice".to_string(),
        state: "R".to_string(),
        start_time: "00:00:00".to_string(),
        cpu_time: 0,
        command: long_cmd.clone(),
        ppid: 1,
        threads: 1,
        uses_gpu: true,
        priority: 0,
        nice_value: 0,
        gpu_utilization: 0.0,
    };
    let mut snap = sample_snapshot();
    snap.processes = Some(vec![proc]);
    // The include must request `process`; the default HTTP filter
    // drops the processes section entirely.
    let filter = parse_include(Some("gpu,cpu,memory,chassis,process"));
    let value = filter_snapshot_value(&snap, &filter);
    let procs = value["processes"].as_array().expect("processes array");
    assert_eq!(procs.len(), 1);
    let rendered_command = procs[0]["command"].as_str().expect("command string");
    assert!(
        rendered_command.len() < long_cmd.len(),
        "command must be truncated; got {} bytes",
        rendered_command.len()
    );
    assert!(
        rendered_command.contains("bytes truncated"),
        "truncation marker missing: {rendered_command}"
    );
    assert!(
        !rendered_command.contains(&"A".repeat(4096)),
        "full argv must not leak verbatim"
    );
}

/// Regression: a `process_name` longer than `MAX_NAME_LABEL_LEN`
/// must also be truncated. Name fields can grow on Windows (long
/// service paths) and through Linux namespaces.
#[test]
fn filter_truncates_long_process_name() {
    use crate::device::ProcessInfo;

    let long_name = "N".repeat(512);
    let proc = ProcessInfo {
        device_id: 0,
        device_uuid: String::new(),
        pid: 1,
        process_name: long_name.clone(),
        used_memory: 0,
        cpu_percent: 0.0,
        memory_percent: 0.0,
        memory_rss: 0,
        memory_vms: 0,
        user: "root".to_string(),
        state: "S".to_string(),
        start_time: "00:00:00".to_string(),
        cpu_time: 0,
        command: String::new(),
        ppid: 1,
        threads: 1,
        uses_gpu: false,
        priority: 0,
        nice_value: 0,
        gpu_utilization: 0.0,
    };
    let mut snap = sample_snapshot();
    snap.processes = Some(vec![proc]);
    let filter = parse_include(Some("gpu,cpu,memory,chassis,process"));
    let value = filter_snapshot_value(&snap, &filter);
    let rendered = value["processes"][0]["process_name"]
        .as_str()
        .expect("process_name string");
    assert!(rendered.len() < long_name.len());
    assert!(rendered.contains("bytes truncated"));
}

/// Regression: a `user` field longer than `MAX_USER_LABEL_LEN` must
/// also be truncated. Long LDAP DNs and Windows SIDs can overflow
/// this on enterprise clusters.
#[test]
fn filter_truncates_long_user_field() {
    use crate::device::ProcessInfo;

    let long_user = "U".repeat(512);
    let proc = ProcessInfo {
        device_id: 0,
        device_uuid: String::new(),
        pid: 1,
        process_name: "p".to_string(),
        used_memory: 0,
        cpu_percent: 0.0,
        memory_percent: 0.0,
        memory_rss: 0,
        memory_vms: 0,
        user: long_user.clone(),
        state: "S".to_string(),
        start_time: "00:00:00".to_string(),
        cpu_time: 0,
        command: String::new(),
        ppid: 1,
        threads: 1,
        uses_gpu: false,
        priority: 0,
        nice_value: 0,
        gpu_utilization: 0.0,
    };
    let mut snap = sample_snapshot();
    snap.processes = Some(vec![proc]);
    let filter = parse_include(Some("gpu,cpu,memory,chassis,process"));
    let value = filter_snapshot_value(&snap, &filter);
    let rendered = value["processes"][0]["user"].as_str().expect("user string");
    assert!(rendered.len() < long_user.len());
    assert!(rendered.contains("bytes truncated"));
}

/// Short process fields must round-trip unchanged so normal
/// operators still see the full executable name and argv.
#[test]
fn filter_leaves_short_process_fields_intact() {
    use crate::device::ProcessInfo;

    let proc = ProcessInfo {
        device_id: 0,
        device_uuid: "uuid".to_string(),
        pid: 1,
        process_name: "python".to_string(),
        used_memory: 0,
        cpu_percent: 0.0,
        memory_percent: 0.0,
        memory_rss: 0,
        memory_vms: 0,
        user: "alice".to_string(),
        state: "R".to_string(),
        start_time: "00:00:00".to_string(),
        cpu_time: 0,
        command: "python train.py".to_string(),
        ppid: 1,
        threads: 1,
        uses_gpu: true,
        priority: 0,
        nice_value: 0,
        gpu_utilization: 0.0,
    };
    let mut snap = sample_snapshot();
    snap.processes = Some(vec![proc]);
    let filter = parse_include(Some("gpu,cpu,memory,chassis,process"));
    let value = filter_snapshot_value(&snap, &filter);
    assert_eq!(
        value["processes"][0]["command"].as_str(),
        Some("python train.py")
    );
    assert_eq!(value["processes"][0]["user"].as_str(), Some("alice"));
    assert_eq!(
        value["processes"][0]["process_name"].as_str(),
        Some("python")
    );
}

/// `filter_snapshot_value` as it was before issue #503: convert the whole
/// snapshot, then remove what the filter excludes. Kept as the reference
/// the field-by-field build must match byte for byte.
fn old_filter_snapshot_value(snapshot: &Snapshot, filter: &SectionFilter) -> Value {
    let mut value = serde_json::to_value(snapshot).unwrap_or(Value::Null);
    sanitize_json_floats(&mut value);
    if let Value::Object(ref mut map) = value {
        // Apply the same per-field byte caps the Prometheus exporter uses
        // for process labels (`api::metrics::process`). Without this the
        // JSON/SSE path would broadcast full argv strings — a privacy
        // leak (DB URLs, API tokens) and a response-size amplification
        // vector that is already mitigated on the `/metrics` surface.
        if let Some(Value::Array(procs)) = map.get_mut("processes") {
            truncate_process_labels_in_place(procs);
        }
        for section in ["gpus", "cpus", "memory", "chassis", "processes", "storage"] {
            if !filter.allows(section) {
                map.remove(section);
            }
        }
    }
    value
}

/// A snapshot with every section populated, including the cases the
/// filter treats specially: a process whose fields need truncating,
/// non-finite floats, and a non-empty `errors` array.
fn full_snapshot() -> Snapshot {
    use crate::device::{MemoryInfo, ProcessInfo};
    use crate::storage::info::StorageInfo;

    let process = ProcessInfo {
        device_id: 0,
        device_uuid: "GPU-A".to_string(),
        pid: 4242,
        process_name: "n".repeat(300),
        used_memory: 1024,
        cpu_percent: f64::NAN,
        memory_percent: 1.0,
        memory_rss: 0,
        memory_vms: 0,
        user: "u".repeat(300),
        state: "R".to_string(),
        start_time: "01:02:03".to_string(),
        cpu_time: 0,
        command: "python ".to_string() + &"A".repeat(4096),
        ppid: 1,
        threads: 1,
        uses_gpu: true,
        priority: 20,
        nice_value: 0,
        gpu_utilization: f64::INFINITY,
    };
    let memory = MemoryInfo {
        index: 0,
        host_id: "host".to_string(),
        hostname: "host".to_string(),
        instance: "host".to_string(),
        total_bytes: 64 << 30,
        used_bytes: 16 << 30,
        available_bytes: 48 << 30,
        free_bytes: 40 << 30,
        buffers_bytes: 0,
        cached_bytes: 0,
        swap_total_bytes: 0,
        swap_used_bytes: 0,
        swap_free_bytes: 0,
        utilization: f64::NAN,
        time: String::new(),
    };
    Snapshot {
        schema: 1,
        timestamp: "2026-04-20T00:00:00Z".to_string(),
        hostname: "host".to_string(),
        gpus: Some(Vec::new()),
        cpus: Some(vec![crate::device::CpuInfo::test_default()]),
        memory: Some(vec![memory]),
        chassis: Some(vec![crate::device::ChassisInfo::default()]),
        processes: Some(vec![process]),
        storage: Some(vec![StorageInfo {
            mount_point: "/".to_string(),
            total_bytes: 1 << 40,
            available_bytes: 1 << 39,
            host_id: "host".to_string(),
            hostname: "host".to_string(),
            index: 0,
        }]),
        errors: vec![crate::snapshot::SnapshotError {
            section: "gpu".to_string(),
            kind: "timeout".to_string(),
            message: "fake".to_string(),
        }],
    }
}

fn every_filter() -> Vec<SectionFilter> {
    (0u8..64)
        .map(|bits| SectionFilter {
            gpu: bits & 1 != 0,
            cpu: bits & 2 != 0,
            memory: bits & 4 != 0,
            chassis: bits & 8 != 0,
            process: bits & 16 != 0,
            storage: bits & 32 != 0,
        })
        .collect()
}

/// Golden test over all 64 filter combinations, compact and pretty: the
/// field-by-field build produces the bytes the convert-then-remove
/// implementation did.
#[test]
fn every_filter_combination_serializes_to_the_same_bytes_as_before() {
    let mut sparse = full_snapshot();
    sparse.processes = None;
    sparse.storage = None;
    sparse.errors.clear();

    for snapshot in [full_snapshot(), sparse] {
        for filter in every_filter() {
            let old = old_filter_snapshot_value(&snapshot, &filter);
            assert_eq!(
                filter_snapshot_value(&snapshot, &filter),
                old,
                "value differs for {filter:?}"
            );
            assert_eq!(
                serialize_filtered(&snapshot, &filter, false).unwrap(),
                serde_json::to_string(&old).unwrap(),
                "compact body differs for {filter:?}"
            );
            assert_eq!(
                serialize_filtered(&snapshot, &filter, true).unwrap(),
                serde_json::to_string_pretty(&old).unwrap(),
                "pretty body differs for {filter:?}"
            );
        }
    }
}

#[test]
fn covers_means_every_requested_section_was_collected() {
    let all = SectionFilter {
        gpu: true,
        cpu: true,
        memory: true,
        chassis: true,
        process: true,
        storage: true,
    };
    let default = SectionFilter::default_http();
    assert!(all.covers(&default));
    assert!(default.covers(&default));
    assert!(!default.covers(&all));
    assert!(default.covers(&parse_include(Some("gpu"))));
    assert!(!parse_include(Some("gpu")).covers(&parse_include(Some("gpu,storage"))));
}
