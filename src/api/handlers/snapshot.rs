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

//! One-shot `/snapshot` JSON handler (issue #193).
//!
//! Serves the most recent frame published through the shared
//! [`FrameBus`]. If the last frame is older than `2 × collection_interval`
//! (for example, the background collector is hung or the server just
//! started and no cycle has completed yet), the handler falls back to a
//! fresh collection so the response never silently serves stale data.

use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::Value;

use crate::api::frame_bus::FrameBus;
use crate::api::metrics::process::{
    MAX_COMMAND_LABEL_LEN, MAX_NAME_LABEL_LEN, MAX_USER_LABEL_LEN, ProcessMetricExporter,
};
use crate::cli::SnapshotIncludes;
use crate::snapshot::{
    DefaultSnapshotCollector, SNAPSHOT_SCHEMA_VERSION, Snapshot, collect_once, sanitize_json_floats,
};

/// Per-reader timeout used when `/snapshot` forces a fresh collection.
/// Matches the CLI `snapshot --timeout-ms` default so operators see the
/// same behaviour across both surfaces.
const FRESH_COLLECT_TIMEOUT: Duration = Duration::from_millis(5_000);

#[derive(Debug, Default, Deserialize)]
pub struct SnapshotQuery {
    /// Comma-separated section filter. Accepts the same names as the CLI
    /// `snapshot --include` flag: `gpu,cpu,memory,chassis,process,storage`.
    /// Unknown names are silently ignored so a client typo does not
    /// surface as a 400.
    pub include: Option<String>,
    /// Pretty-print the JSON body. `?pretty=1` / `?pretty=true` enable;
    /// anything else (including the absence of the param) disables.
    pub pretty: Option<String>,
}

pub async fn snapshot_handler(
    State(bus): State<FrameBus>,
    Query(params): Query<SnapshotQuery>,
) -> Response {
    let filter = parse_include(params.include.as_deref());
    let pretty = matches!(
        params.pretty.as_deref(),
        Some("1") | Some("true") | Some("yes")
    );

    let snapshot = match resolve_snapshot(&bus, &filter).await {
        Ok(s) => s,
        Err(err) => {
            tracing::warn!("/snapshot: fresh collect failed: {err}");
            return error_response(StatusCode::SERVICE_UNAVAILABLE, &err);
        }
    };

    let value = filter_snapshot_value(&snapshot, &filter);
    let body = match if pretty {
        serde_json::to_string_pretty(&value)
    } else {
        serde_json::to_string(&value)
    } {
        Ok(s) => s,
        Err(err) => {
            tracing::warn!("/snapshot: JSON serialization failed: {err}");
            return error_response(StatusCode::INTERNAL_SERVER_ERROR, &err.to_string());
        }
    };

    let mut headers = no_cache_headers();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );

    (StatusCode::OK, headers, body).into_response()
}

/// Common no-cache header set shared between the success and error
/// responses. Without them a reverse proxy can silently cache a transient
/// `/snapshot` failure and keep serving the stale error to subsequent
/// clients — the SSE spec already bans this on `/events` and we mirror it
/// here for symmetry (issue #193).
fn no_cache_headers() -> HeaderMap {
    let mut h = HeaderMap::new();
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert("X-Accel-Buffering", HeaderValue::from_static("no"));
    h
}

fn error_response(status: StatusCode, message: &str) -> Response {
    let body = Json(serde_json::json!({
        "schema": SNAPSHOT_SCHEMA_VERSION,
        "error": message,
    }));
    (status, no_cache_headers(), body).into_response()
}

/// Resolve the frame served to the caller.
///
/// Reads the last published frame from the bus. If that frame is older
/// than `2 × collection_interval` (or no frame has been published yet),
/// a fresh collection is performed synchronously honouring the caller's
/// `?include=` filter so the stale-fallback path is observationally
/// indistinguishable from the cached path (issue #193). Fresh collections
/// never race the background task because `DefaultSnapshotCollector::new()`
/// builds its own reader set each call.
async fn resolve_snapshot(bus: &FrameBus, filter: &SectionFilter) -> Result<Arc<Snapshot>, String> {
    let interval = bus.collection_interval();
    let stale_after = interval.saturating_mul(2);
    if let Some(frame) = bus.latest().await
        && frame.published_at.elapsed() <= stale_after
    {
        return Ok(frame.snapshot);
    }

    // Serialise fresh collects with a single-flight lock. A burst of
    // `/snapshot` requests against a freshly-started server, or against
    // a server whose collection loop has stalled, would otherwise each
    // spawn their own `DefaultSnapshotCollector` and saturate the Tokio
    // blocking pool — an amplification attack on an otherwise cheap
    // HTTP endpoint. Holding the lock while we re-check `latest()`
    // means a winning collector's output is observed by every queued
    // caller without any of them issuing their own hardware read.
    let _guard = bus.lock_fresh_collect().await;

    // Re-check `latest()` under the lock: another task may have
    // refreshed the frame while we were queued, or the background
    // collection loop may have published a cycle concurrently.
    if let Some(frame) = bus.latest().await
        && frame.published_at.elapsed() <= stale_after
    {
        return Ok(frame.snapshot);
    }

    // Fall back to a fresh collection. Map the caller-visible
    // `SectionFilter` onto the collector-level `SnapshotIncludes` so the
    // response carries exactly the sections the client asked for — the
    // cached path does the same via the always-collected background
    // frame, so matching behaviour here keeps the two paths
    // observationally identical.
    let includes = SnapshotIncludes {
        gpu: filter.gpu,
        cpu: filter.cpu,
        memory: filter.memory,
        chassis: filter.chassis,
        process: filter.process,
        storage: filter.storage,
    };
    let collector = Arc::new(DefaultSnapshotCollector::new());
    let snap = collect_once(collector, &includes, FRESH_COLLECT_TIMEOUT).await;
    Ok(Arc::new(snap))
}

// ---------------------------------------------------------------------
// Include filter — shared with the SSE handler.
// ---------------------------------------------------------------------

/// Section filter parsed from the `?include=` query parameter.
///
/// The filter is applied at serialization time (rather than at collection
/// time) so the background collector does not need to know which mix of
/// sections the next client will request — every collection cycle
/// populates every section once and every client reads the subset it
/// asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SectionFilter {
    pub gpu: bool,
    pub cpu: bool,
    pub memory: bool,
    pub chassis: bool,
    pub process: bool,
    pub storage: bool,
}

impl SectionFilter {
    /// Default HTTP-surface filter: `gpu,cpu,memory,chassis` per the
    /// issue spec. `process` and `storage` are expensive / noisy and
    /// stay opt-in.
    pub fn default_http() -> Self {
        Self {
            gpu: true,
            cpu: true,
            memory: true,
            chassis: true,
            process: false,
            storage: false,
        }
    }

    /// Whether `section` is requested. Unknown section names return
    /// `false` so they simply do not appear in the filtered output.
    pub fn allows(&self, section: &str) -> bool {
        match section {
            "gpus" => self.gpu,
            "cpus" => self.cpu,
            "memory" => self.memory,
            "chassis" => self.chassis,
            "processes" => self.process,
            "storage" => self.storage,
            _ => true, // Non-section metadata (schema, timestamp, errors)
        }
    }
}

/// Parse an `?include=...` query parameter into a [`SectionFilter`].
///
/// * Missing / empty value → [`SectionFilter::default_http`].
/// * Unknown section names → silently dropped so client-side typos don't
///   produce a 400.
pub fn parse_include(raw: Option<&str>) -> SectionFilter {
    let Some(raw) = raw else {
        return SectionFilter::default_http();
    };
    if raw.trim().is_empty() {
        return SectionFilter::default_http();
    }
    let mut filter = SectionFilter {
        gpu: false,
        cpu: false,
        memory: false,
        chassis: false,
        process: false,
        storage: false,
    };
    for raw_name in raw.split(',') {
        match raw_name.trim().to_ascii_lowercase().as_str() {
            "" => continue,
            "gpu" | "gpus" => filter.gpu = true,
            "cpu" | "cpus" => filter.cpu = true,
            "memory" | "mem" => filter.memory = true,
            "chassis" => filter.chassis = true,
            "process" | "processes" => filter.process = true,
            "storage" | "disk" => filter.storage = true,
            _ => {
                // Ignore unknown names but trace them so the operator
                // can spot typos without a failed request.
                tracing::debug!(unknown_section = raw_name, "unknown /snapshot include name");
            }
        }
    }
    // If the filter ended up entirely empty (e.g. `?include=unknown`),
    // fall back to the default so the response is still useful.
    if !(filter.gpu
        || filter.cpu
        || filter.memory
        || filter.chassis
        || filter.process
        || filter.storage)
    {
        return SectionFilter::default_http();
    }
    filter
}

/// Apply a [`SectionFilter`] to a snapshot and produce the wire-format
/// `serde_json::Value`. Non-finite floats are sanitised through
/// [`sanitize_json_floats`] so NVML / TPU driver quirks cannot fail
/// serialization.
pub fn filter_snapshot_value(snapshot: &Snapshot, filter: &SectionFilter) -> Value {
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

/// Apply `MAX_COMMAND_LABEL_LEN` / `MAX_NAME_LABEL_LEN` /
/// `MAX_USER_LABEL_LEN` to the `command`, `process_name`, and `user`
/// fields of a serialized `ProcessInfo` array. Mirrors the Prometheus
/// exporter's truncation so the same privacy + amplification guarantees
/// apply to the JSON/SSE surfaces. Every other field (PIDs, memory
/// counters, timings) is left untouched.
fn truncate_process_labels_in_place(procs: &mut [Value]) {
    for proc in procs.iter_mut() {
        if let Value::Object(ref mut fields) = *proc {
            truncate_string_field(fields, "command", MAX_COMMAND_LABEL_LEN);
            truncate_string_field(fields, "process_name", MAX_NAME_LABEL_LEN);
            truncate_string_field(fields, "user", MAX_USER_LABEL_LEN);
        }
    }
}

fn truncate_string_field(fields: &mut serde_json::Map<String, Value>, key: &str, max_len: usize) {
    if let Some(Value::String(s)) = fields.get(key) {
        let truncated = ProcessMetricExporter::truncate_for_label(s, max_len);
        if let std::borrow::Cow::Owned(new_value) = truncated {
            fields.insert(key.to_string(), Value::String(new_value));
        }
    }
}

#[cfg(test)]
mod tests {
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
}
