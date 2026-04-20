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
use std::fs::{self, OpenOptions};
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
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
///
/// # Blocking-pool considerations
///
/// Each reader runs inside `tokio::task::spawn_blocking` guarded by a
/// `tokio::time::timeout`. When a reader exceeds its budget the outer
/// future's `Elapsed` branch fires and the `JoinHandle` is dropped — but
/// `spawn_blocking` does **not** cancel the underlying OS thread. The
/// thread continues running until the wrapped syscall returns, occupying
/// one worker from Tokio's blocking pool (default cap 512). A misbehaving
/// NVML/TPU call can therefore leak a worker for the entire process
/// lifetime.
///
/// Callers embedding this function in a long-running service should
/// provision the Tokio runtime with a conservative
/// [`tokio::runtime::Builder::max_blocking_threads`] so a burst of
/// pathological readers cannot exhaust the pool. The CLI dispatch in
/// `main.rs` builds a short-lived runtime with
/// `max_blocking_threads(32)` per snapshot invocation for exactly this
/// reason.
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
        write_output_atomic(Path::new(path), &rendered)?;
    }

    Ok(())
}

/// Write `contents` to `final_path` atomically and with restrictive
/// permissions.
///
/// On Unix the temporary file is opened with `O_NOFOLLOW` and mode `0o600`
/// so an attacker cannot redirect the write via a pre-existing symlink and
/// the result is only readable by its owner. On Windows the file is opened
/// with `share_mode(0)` (exclusive access) — symlink TOCTOU on Windows is
/// handled with different mitigations which are out of scope for this
/// function.
///
/// The output is first written to a sibling file `<final_path>.tmp` (with
/// up to 64 collision retries), `sync_all`-ed, then renamed onto the final
/// path. Callers should assume the final file exists only if the function
/// returned `Ok(())`.
fn write_output_atomic(final_path: &Path, contents: &str) -> Result<()> {
    let tmp_path = pick_tmp_path(final_path);

    let file_result = {
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .custom_flags(libc::O_NOFOLLOW)
                .mode(0o600)
                .open(&tmp_path)
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .share_mode(0)
                .open(&tmp_path)
        }
        #[cfg(not(any(unix, windows)))]
        {
            OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&tmp_path)
        }
    };

    let mut file = file_result
        .with_context(|| format!("failed to create snapshot temp file {}", tmp_path.display()))?;

    if let Err(e) = file.write_all(contents.as_bytes()) {
        let _ = fs::remove_file(&tmp_path);
        return Err(e)
            .with_context(|| format!("failed to write snapshot data to {}", tmp_path.display()));
    }
    if let Err(e) = file.sync_all() {
        let _ = fs::remove_file(&tmp_path);
        return Err(e)
            .with_context(|| format!("failed to fsync snapshot temp file {}", tmp_path.display()));
    }
    drop(file);

    if let Err(e) = fs::rename(&tmp_path, final_path) {
        let _ = fs::remove_file(&tmp_path);
        return Err(e).with_context(|| {
            format!(
                "failed to rename snapshot temp file {} -> {}",
                tmp_path.display(),
                final_path.display()
            )
        });
    }

    Ok(())
}

/// Pick a temp-file path next to `final_path`. Starts with `<path>.tmp` and
/// appends a numeric suffix when that name is already taken to reduce
/// collision risk when multiple snapshot invocations target the same
/// directory concurrently.
fn pick_tmp_path(final_path: &Path) -> PathBuf {
    let base = {
        let mut p = final_path.as_os_str().to_os_string();
        p.push(".tmp");
        PathBuf::from(p)
    };
    if !base.exists() {
        return base;
    }
    for i in 1..=64 {
        let mut p = final_path.as_os_str().to_os_string();
        p.push(format!(".tmp.{i}"));
        let candidate = PathBuf::from(p);
        if !candidate.exists() {
            return candidate;
        }
    }
    // Fall back to the original base name: the open call will fail if the
    // conflicting file still exists, surfacing the error to the caller
    // rather than guessing forever.
    base
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

/// Recursively replace non-finite `f64` numbers (`NaN`, `+Inf`, `-Inf`)
/// inside a [`serde_json::Value`] with `Value::Null`. Called before any
/// serialization path so that neither the JSON nor CSV writers can abort
/// on a single misbehaving device field.
///
/// Rationale: `serde_json::Number::from_f64` refuses non-finite values,
/// which causes `to_string(snapshot)` to fail for the WHOLE snapshot when
/// any device carries a `NaN` (common on NVML drivers when a fan RPM is
/// unavailable, for example) or `Infinity` (some vendors emit `+Inf` for
/// "unknown"). Downgrading to `null` keeps the rest of the snapshot
/// intact — consumers that need a numeric default can substitute it
/// themselves.
pub fn sanitize_json_floats(v: &mut Value) {
    match v {
        Value::Number(n) => {
            if let Some(f) = n.as_f64()
                && !f.is_finite()
            {
                *v = Value::Null;
            }
        }
        Value::Array(arr) => {
            for item in arr.iter_mut() {
                sanitize_json_floats(item);
            }
        }
        Value::Object(obj) => {
            for item in obj.values_mut() {
                sanitize_json_floats(item);
            }
        }
        _ => {}
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

/// Convert an iterable of typed devices into the sanitized `Vec<Value>`
/// shape the CSV serializer consumes. A single device that fails to
/// serialize (extremely rare — would require a custom `Serialize` impl to
/// return an error) is logged to stderr and skipped rather than silently
/// elided as `Value::Null` like the old `unwrap_or(Value::Null)` pattern.
///
/// Each resulting `Value` also has its non-finite `f64` numbers replaced
/// with `Value::Null` via [`sanitize_json_floats`], so the CSV writer
/// cannot fail on `NaN`/`Inf` emitted by flaky drivers.
fn bucket<T: serde::Serialize>(section: &'static str, items: &[T]) -> Vec<Value> {
    let mut out = Vec::with_capacity(items.len());
    for (i, item) in items.iter().enumerate() {
        match serde_json::to_value(item) {
            Ok(mut v) => {
                sanitize_json_floats(&mut v);
                out.push(augment_device_json(section, i, v));
            }
            Err(e) => {
                eprintln!(
                    "snapshot: skipping {section} device index {i}: serialization failed: {e}"
                );
            }
        }
    }
    out
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
        buckets.push(("gpu", bucket("gpu", gpus)));
    }
    if includes.cpu
        && let Some(cpus) = snap.cpus.as_ref()
    {
        buckets.push(("cpu", bucket("cpu", cpus)));
    }
    if includes.memory
        && let Some(mems) = snap.memory.as_ref()
    {
        buckets.push(("memory", bucket("memory", mems)));
    }
    if includes.chassis
        && let Some(ch) = snap.chassis.as_ref()
    {
        buckets.push(("chassis", bucket("chassis", ch)));
    }
    if includes.process
        && let Some(procs) = snap.processes.as_ref()
    {
        buckets.push(("process", bucket("process", procs)));
    }
    if includes.storage
        && let Some(sto) = snap.storage.as_ref()
    {
        buckets.push(("storage", bucket("storage", sto)));
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
