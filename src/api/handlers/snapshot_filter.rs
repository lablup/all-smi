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

//! `?include=` section filter and JSON body serialization shared by the
//! `/snapshot` and `/events` handlers (issue #193).

use serde_json::Value;

use crate::api::metrics::process::{
    MAX_COMMAND_LABEL_LEN, MAX_NAME_LABEL_LEN, MAX_USER_LABEL_LEN, ProcessMetricExporter,
};
use crate::snapshot::{Snapshot, sanitize_json_floats};

/// Section filter parsed from the `?include=` query parameter.
///
/// The filter is applied at serialization time (rather than at collection
/// time) so the background collector does not need to know which mix of
/// sections the next client will request — every collection cycle
/// populates every section once and every client reads the subset it
/// asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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

    /// Whether a snapshot collected for `self` holds every section
    /// `other` asks for, so it can answer a request filtered by `other`.
    pub fn covers(&self, other: &SectionFilter) -> bool {
        (self.gpu || !other.gpu)
            && (self.cpu || !other.cpu)
            && (self.memory || !other.memory)
            && (self.chassis || !other.chassis)
            && (self.process || !other.process)
            && (self.storage || !other.storage)
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

#[cfg(test)]
thread_local! {
    /// Number of snapshot bodies serialized on the current thread. Lets
    /// tests prove a frame is serialized once per `(filter, pretty)` and
    /// not once per client (issue #503).
    pub(crate) static SERIALIZE_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Apply a [`SectionFilter`] to a snapshot and produce the wire-format
/// `serde_json::Value`. Non-finite floats are sanitised through
/// [`sanitize_json_floats`] so NVML / TPU driver quirks cannot fail
/// serialization.
///
/// The object is built field by field, so a section the filter excludes
/// is never converted at all (issue #503): with the default filter that
/// skips `processes` and `storage`, the two largest sections on a busy
/// host. The result is identical to converting the whole snapshot and
/// removing the excluded sections afterwards.
pub fn filter_snapshot_value(snapshot: &Snapshot, filter: &SectionFilter) -> Value {
    build_filtered_value(snapshot, filter).unwrap_or(Value::Null)
}

fn build_filtered_value(
    snapshot: &Snapshot,
    filter: &SectionFilter,
) -> Result<Value, serde_json::Error> {
    /// Convert one allowed, populated section. `None` sections stay
    /// absent, matching `skip_serializing_if = "Option::is_none"`.
    fn section<T: serde::Serialize>(
        map: &mut serde_json::Map<String, Value>,
        key: &str,
        allowed: bool,
        rows: &Option<Vec<T>>,
    ) -> Result<(), serde_json::Error> {
        if allowed && let Some(rows) = rows {
            let mut value = serde_json::to_value(rows)?;
            sanitize_json_floats(&mut value);
            map.insert(key.to_string(), value);
        }
        Ok(())
    }

    let mut map = serde_json::Map::new();
    map.insert("schema".to_string(), Value::from(snapshot.schema));
    map.insert(
        "timestamp".to_string(),
        Value::String(snapshot.timestamp.clone()),
    );
    map.insert(
        "hostname".to_string(),
        Value::String(snapshot.hostname.clone()),
    );
    section(&mut map, "gpus", filter.gpu, &snapshot.gpus)?;
    section(&mut map, "cpus", filter.cpu, &snapshot.cpus)?;
    section(&mut map, "memory", filter.memory, &snapshot.memory)?;
    section(&mut map, "chassis", filter.chassis, &snapshot.chassis)?;
    section(&mut map, "processes", filter.process, &snapshot.processes)?;
    section(&mut map, "storage", filter.storage, &snapshot.storage)?;
    // Apply the same per-field byte caps the Prometheus exporter uses
    // for process labels (`api::metrics::process`). Without this the
    // JSON/SSE path would broadcast full argv strings — a privacy
    // leak (DB URLs, API tokens) and a response-size amplification
    // vector that is already mitigated on the `/metrics` surface.
    if let Some(Value::Array(procs)) = map.get_mut("processes") {
        truncate_process_labels_in_place(procs);
    }
    // `errors` is metadata, kept whatever the filter says, and absent
    // when empty (`skip_serializing_if = "Vec::is_empty"`).
    if !snapshot.errors.is_empty() {
        map.insert(
            "errors".to_string(),
            serde_json::to_value(&snapshot.errors)?,
        );
    }
    Ok(Value::Object(map))
}

/// Serialize the filtered snapshot to the body `/snapshot` returns and
/// `/events` sends as an event's data.
pub fn serialize_filtered(
    snapshot: &Snapshot,
    filter: &SectionFilter,
    pretty: bool,
) -> Result<String, serde_json::Error> {
    #[cfg(test)]
    SERIALIZE_COUNT.with(|count| count.set(count.get() + 1));

    let value = filter_snapshot_value(snapshot, filter);
    if pretty {
        serde_json::to_string_pretty(&value)
    } else {
        serde_json::to_string(&value)
    }
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
#[path = "snapshot_filter_tests.rs"]
mod tests;
