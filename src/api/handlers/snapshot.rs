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

    let snapshot = match resolve_snapshot(&bus).await {
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

    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    // Mirror the /events advice so operators putting all-smi behind a
    // cachey reverse proxy still get the newest frame even on repeated
    // hits.
    headers.insert("X-Accel-Buffering", HeaderValue::from_static("no"));

    (StatusCode::OK, headers, body).into_response()
}

fn error_response(status: StatusCode, message: &str) -> Response {
    let body = Json(serde_json::json!({
        "schema": SNAPSHOT_SCHEMA_VERSION,
        "error": message,
    }));
    (status, body).into_response()
}

/// Resolve the frame served to the caller.
///
/// Reads the last published frame from the bus. If that frame is older
/// than `2 × collection_interval` (or no frame has been published yet),
/// a fresh collection is performed synchronously with the default
/// includes. Fresh collections never race the background task because
/// `DefaultSnapshotCollector::new()` builds its own reader set each
/// call.
async fn resolve_snapshot(bus: &FrameBus) -> Result<Arc<Snapshot>, String> {
    let interval = bus.collection_interval();
    let stale_after = interval.saturating_mul(2);
    if let Some(frame) = bus.latest().await
        && frame.published_at.elapsed() <= stale_after
    {
        return Ok(frame.snapshot);
    }

    // Fall back to a fresh collection. The `all-includes` set gives the
    // client the complete payload; the handler filters later per the
    // request.
    let all_includes = SnapshotIncludes {
        gpu: true,
        cpu: true,
        memory: true,
        chassis: true,
        process: false,
        storage: false,
    };
    let collector = Arc::new(DefaultSnapshotCollector::new());
    let snap = collect_once(collector, &all_includes, FRESH_COLLECT_TIMEOUT).await;
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
        for section in ["gpus", "cpus", "memory", "chassis", "processes", "storage"] {
            if !filter.allows(section) {
                map.remove(section);
            }
        }
    }
    value
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
}
