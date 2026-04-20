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

//! One-shot snapshot of hardware state.
//!
//! Backs the `all-smi snapshot` subcommand and the library-visible
//! [`run`] entry point. Reuses the existing Prometheus exporters from
//! [`crate::api::metrics`] rather than re-implementing them, so the
//! `snapshot --format prometheus` output stays byte-identical to a single
//! `/metrics` scrape of `all-smi api`.
//!
//! Submodule layout:
//!
//! * [`options`] — pure-data config (`SnapshotOptions`, `Snapshot`,
//!   `SnapshotError`, `SnapshotHardFailure`).
//! * [`collector`] — `SnapshotCollector` trait + default wrapper,
//!   `spawn_blocking` + `timeout` orchestration.
//! * [`query`] — dot-path evaluator used by the CSV serializer.
//! * [`serializers`] — format-specific writers (json, csv, prometheus).

use std::collections::HashSet;
use std::fs::File;
use std::io::{self, IsTerminal, Write};
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use serde_json::{Map, Value};

use crate::cli::{SnapshotFormat, SnapshotIncludes};

pub mod collector;
pub mod options;
pub mod query;
pub mod serializers;

pub use collector::{DefaultSnapshotCollector, SnapshotCollector, collect_once};
// Re-exports that form the public library surface. `#[allow(unused_imports)]`
// suppresses the "binary doesn't call them directly" warning — they're here
// for library consumers and integration tests.
#[allow(unused_imports)]
pub use options::{
    SNAPSHOT_SCHEMA_VERSION, Snapshot, SnapshotError, SnapshotHardFailure, SnapshotOptions,
};

/// Run the snapshot subcommand end-to-end: collect N samples, serialize
/// them per `opts.format`, and write to `opts.output` (or stdout).
///
/// # Exit-code convention
///
/// This function returns `anyhow::Result<()>`. The caller in `main.rs`
/// should distinguish three outcomes:
///
/// * `Ok(())` → exit `0`. The output was written; it may include partial
///   errors in the `errors` array.
/// * `Err` with a [`SnapshotHardFailure`] attached → exit `1` ("hard
///   failure": no devices collected at all across every sample).
/// * Any other `Err` → exit `1` with the error message on stderr.
pub async fn run(opts: SnapshotOptions) -> Result<()> {
    let collector = Arc::new(DefaultSnapshotCollector::new());
    run_with_collector(opts, collector).await
}

/// Generic entry point parameterised on a collector for testability.
pub async fn run_with_collector<C: SnapshotCollector + 'static>(
    opts: SnapshotOptions,
    collector: Arc<C>,
) -> Result<()> {
    let writer_is_stdout = opts.output.as_deref().is_none_or(|p| p == "-");
    let stdout_is_tty = io::stdout().is_terminal();

    // Collect all samples first so a file writer receives a single atomic
    // write instead of N interleaved ones. The Prometheus format only runs
    // a single sample (multi-sample Prometheus has no canonical shape), so
    // we cap the sample count there to 1 and log a warning.
    let sample_count = match opts.format {
        SnapshotFormat::Prometheus => {
            if opts.samples > 1 {
                eprintln!(
                    "warning: --samples > 1 is ignored for --format prometheus (single scrape)"
                );
            }
            1
        }
        _ => opts.samples.max(1),
    };

    let mut snapshots: Vec<Snapshot> = Vec::with_capacity(sample_count as usize);
    for i in 0..sample_count {
        if i > 0 && !opts.interval.is_zero() {
            tokio::time::sleep(opts.interval).await;
        }
        let snap = collect_once(collector.clone(), &opts.includes, opts.timeout_per_reader).await;
        snapshots.push(snap);
    }

    // Hard failure = every sample returned zero devices. Soft failure (at
    // least one device collected) still yields exit 0 with errors surfaced
    // inline.
    let hard_failure = snapshots.iter().all(|s| s.device_count() == 0);
    if hard_failure {
        return Err(anyhow::Error::new(SnapshotHardFailure));
    }

    // Materialise the output before opening the writer so a serialization
    // failure does not leave a half-written file on disk.
    let rendered = render(&opts, &snapshots, stdout_is_tty)?;

    if writer_is_stdout {
        let stdout = io::stdout();
        let mut handle = stdout.lock();
        handle
            .write_all(rendered.as_bytes())
            .context("failed to write snapshot to stdout")?;
        handle.flush().ok();
    } else {
        let path = opts.output.as_deref().unwrap();
        let mut file =
            File::create(Path::new(path)).with_context(|| format!("failed to create {path}"))?;
        file.write_all(rendered.as_bytes())
            .with_context(|| format!("failed to write to {path}"))?;
        file.flush().ok();
    }

    Ok(())
}

fn render(opts: &SnapshotOptions, snapshots: &[Snapshot], stdout_is_tty: bool) -> Result<String> {
    match opts.format {
        SnapshotFormat::Json => {
            let pretty = opts.effective_pretty(stdout_is_tty);
            serializers::json::render(snapshots, pretty)
        }
        SnapshotFormat::Csv => serializers::csv::render(opts, snapshots),
        SnapshotFormat::Prometheus => serializers::prometheus::render(snapshots),
    }
}

/// Augment a serialized device JSON with synthetic fields so
/// `--query index,section,...` works against uniform paths across every
/// section. Public for tests.
pub fn augment_device_json(section: &str, index: usize, mut value: Value) -> Value {
    if let Value::Object(ref mut map) = value {
        map.entry("index")
            .or_insert(Value::Number(serde_json::Number::from(index)));
        map.entry("section")
            .or_insert(Value::String(section.to_string()));
    } else {
        // Wrap primitives so the query layer still sees an object.
        let mut obj = Map::new();
        obj.insert("index".to_string(), Value::Number(index.into()));
        obj.insert("section".to_string(), Value::String(section.to_string()));
        obj.insert("value".to_string(), value);
        return Value::Object(obj);
    }
    value
}

/// Expand a typed snapshot into `(section, Vec<Value>)` buckets for the
/// CSV writer. Each bucket is ordered as requested by the user via the
/// `--include` flag so CSV output is deterministic.
///
/// The returned vector preserves the iteration order of the standard
/// section list (gpu, cpu, memory, chassis, process, storage) but only
/// includes sections that were requested *and* have at least one device
/// collected, matching the "absent key" rule for JSON.
pub fn buckets_for_csv(
    snap: &Snapshot,
    includes: &SnapshotIncludes,
) -> Vec<(&'static str, Vec<Value>)> {
    let mut buckets: Vec<(&'static str, Vec<Value>)> = Vec::new();
    if includes.gpu
        && let Some(gpus) = snap.gpus.as_ref()
    {
        buckets.push((
            "gpu",
            gpus.iter()
                .enumerate()
                .map(|(i, g)| {
                    augment_device_json("gpu", i, serde_json::to_value(g).unwrap_or(Value::Null))
                })
                .collect(),
        ));
    }
    if includes.cpu
        && let Some(cpus) = snap.cpus.as_ref()
    {
        buckets.push((
            "cpu",
            cpus.iter()
                .enumerate()
                .map(|(i, c)| {
                    augment_device_json("cpu", i, serde_json::to_value(c).unwrap_or(Value::Null))
                })
                .collect(),
        ));
    }
    if includes.memory
        && let Some(mems) = snap.memory.as_ref()
    {
        buckets.push((
            "memory",
            mems.iter()
                .enumerate()
                .map(|(i, m)| {
                    augment_device_json("memory", i, serde_json::to_value(m).unwrap_or(Value::Null))
                })
                .collect(),
        ));
    }
    if includes.chassis
        && let Some(ch) = snap.chassis.as_ref()
    {
        buckets.push((
            "chassis",
            ch.iter()
                .enumerate()
                .map(|(i, c)| {
                    augment_device_json(
                        "chassis",
                        i,
                        serde_json::to_value(c).unwrap_or(Value::Null),
                    )
                })
                .collect(),
        ));
    }
    if includes.process
        && let Some(procs) = snap.processes.as_ref()
    {
        buckets.push((
            "process",
            procs
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    augment_device_json(
                        "process",
                        i,
                        serde_json::to_value(p).unwrap_or(Value::Null),
                    )
                })
                .collect(),
        ));
    }
    if includes.storage
        && let Some(sto) = snap.storage.as_ref()
    {
        buckets.push((
            "storage",
            sto.iter()
                .enumerate()
                .map(|(i, s)| {
                    augment_device_json(
                        "storage",
                        i,
                        serde_json::to_value(s).unwrap_or(Value::Null),
                    )
                })
                .collect(),
        ));
    }
    buckets
}

/// Set of all default CSV column names used when `--query` is not provided.
/// Kept here so tests and serializers agree on the canonical default layout.
pub fn default_csv_columns() -> Vec<&'static str> {
    // Ordered to mirror `nvidia-smi --query-gpu=...` defaults where possible.
    // Columns not present in a given section resolve to empty strings.
    vec![
        "section",
        "index",
        "hostname",
        "name",
        "uuid",
        "utilization",
        "used_memory",
        "total_memory",
        "temperature",
        "power_consumption",
    ]
}

/// Compute the list of unique column names to emit for CSV, based on either
/// the user's `--query` or the default set.
pub fn effective_csv_columns(opts: &SnapshotOptions) -> Vec<String> {
    if opts.query.is_empty() {
        default_csv_columns()
            .into_iter()
            .map(String::from)
            .collect()
    } else {
        // Preserve order, but drop duplicates so `--query foo,foo` does not
        // emit two identical columns.
        let mut seen: HashSet<String> = HashSet::new();
        let mut out = Vec::with_capacity(opts.query.len());
        for c in &opts.query {
            let trimmed = c.trim();
            if trimmed.is_empty() {
                continue;
            }
            if seen.insert(trimmed.to_string()) {
                out.push(trimmed.to_string());
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn augment_injects_index_and_section() {
        let raw = serde_json::json!({ "name": "gpu0" });
        let out = augment_device_json("gpu", 3, raw);
        assert_eq!(out["index"], serde_json::json!(3));
        assert_eq!(out["section"], serde_json::json!("gpu"));
        assert_eq!(out["name"], serde_json::json!("gpu0"));
    }

    #[test]
    fn augment_preserves_existing_index() {
        // If the device already carries an `index` field (some readers do),
        // don't clobber it.
        let raw = serde_json::json!({ "name": "gpu0", "index": 99 });
        let out = augment_device_json("gpu", 0, raw);
        assert_eq!(out["index"], serde_json::json!(99));
    }

    #[test]
    fn default_csv_columns_is_non_empty_and_unique() {
        let cols = default_csv_columns();
        assert!(!cols.is_empty());
        let as_set: HashSet<&&'static str> = cols.iter().collect();
        assert_eq!(as_set.len(), cols.len(), "columns must be unique");
    }

    #[test]
    fn effective_csv_columns_dedups_user_query() {
        let opts = SnapshotOptions {
            query: vec![
                "name".to_string(),
                "utilization".to_string(),
                "name".to_string(),
                " ".to_string(),
            ],
            ..Default::default()
        };
        let cols = effective_csv_columns(&opts);
        assert_eq!(cols, vec!["name".to_string(), "utilization".to_string()]);
    }
}
