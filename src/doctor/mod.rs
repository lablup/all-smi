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

//! `all-smi doctor` subcommand — self-diagnosis and support bundle.
//!
//! The module is organised as:
//!
//! * [`types`] — `Check`, `CheckCtx`, `CheckResult`, `Severity`.
//! * [`exec`] — bounded-timeout child-process helpers.
//! * [`redact`] — hostname / IP / MAC / username / kptr scrubbers.
//! * [`report`] — human-readable and JSON renderers.
//! * [`bundle`] — tar.gz support-bundle builder.
//! * [`checks`] — one module per category (`platform`, `privileges`, …).
//!
//! The entry point [`run`] registers every check, filters the registry by
//! `--skip` / `--only`, drives each check under a hard 3-second timeout on
//! a small Tokio pool, and then renders the result.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::Serialize;

use crate::cli::DoctorArgs;

pub mod bundle;
pub mod checks;
pub mod exec;
pub mod redact;
pub mod report;
pub mod types;

pub use types::{Check, CheckCtx, CheckResult};
// Re-exported so external callers can match on the severity classifier
// returned by a custom `Check` definition.
#[allow(unused_imports)]
pub use types::Severity;

/// JSON report schema version. Bumped when the shape of [`Report`] breaks
/// backwards-compatibility for scripted consumers.
pub const REPORT_SCHEMA_VERSION: u32 = 1;

/// Hard per-check timeout mandated by the issue.
pub const CHECK_HARD_TIMEOUT: Duration = Duration::from_secs(3);

/// Tokio pool size for running checks in parallel. Small enough to keep
/// resource usage bounded on constrained hosts; large enough that the
/// 47-check suite finishes well inside the 3-second ceiling.
const CHECK_PARALLELISM: usize = 8;

/// Resolved configuration for a doctor run. Mirrors [`DoctorArgs`] but
/// pre-normalises the pieces that multiple modules need (redaction opts,
/// filter predicates, etc.).
#[derive(Clone, Debug)]
pub struct DoctorOptions {
    pub json: bool,
    pub verbose: bool,
    pub bundle_path: Option<PathBuf>,
    pub include_identifiers: bool,
    pub remote_checks: Vec<String>,
    pub skip: Vec<String>,
    pub only: Vec<String>,
    /// Whether the human renderer should emit ANSI colour codes.
    pub use_color: bool,
}

impl DoctorOptions {
    pub fn from_args(args: &DoctorArgs) -> Self {
        let use_color = should_use_color();
        Self {
            json: args.json,
            verbose: args.verbose,
            bundle_path: args.bundle.clone(),
            include_identifiers: args.include_identifiers,
            remote_checks: args.remote_check.clone(),
            skip: args.skip.clone(),
            only: args.only.clone(),
            use_color,
        }
    }

    /// Build the context object threaded into each check.
    pub fn to_ctx(&self) -> CheckCtx {
        CheckCtx {
            verbose: self.verbose,
            include_identifiers: self.include_identifiers,
            remote_checks: self.remote_checks.clone(),
            // Leave ~500ms headroom under the hard per-check ceiling so
            // exec calls can wind down cleanly.
            command_timeout: Duration::from_millis(2_500),
        }
    }

    pub fn redact_options(&self) -> redact::RedactOptions {
        if self.include_identifiers {
            redact::RedactOptions::passthrough()
        } else {
            redact::RedactOptions::default()
        }
    }
}

fn should_use_color() -> bool {
    use std::io::IsTerminal;
    if std::env::var("NO_COLOR").is_ok_and(|v| !v.is_empty()) {
        return false;
    }
    std::io::stdout().is_terminal()
}

/// Outcome of running a single check, ready for rendering. Owned strings
/// because the renderers hand out `&CheckOutcome` across await points.
#[derive(Clone, Debug, Serialize)]
pub struct CheckOutcome {
    pub id: String,
    pub title: String,
    /// One of "pass", "warn", "fail", "skip".
    pub status: &'static str,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
    /// Seconds spent running the check. Emitted for debugging under
    /// `--verbose`; the JSON renderer always includes it for
    /// post-processing.
    pub duration_ms: u64,
}

/// Final report assembled after every check runs. The field order is
/// stable for scripted diff'ing across versions.
#[derive(Clone, Debug, Serialize)]
pub struct Report {
    pub schema: u32,
    pub version: String,
    pub timestamp: String,
    pub summary: Summary,
    pub checks: Vec<CheckOutcome>,
}

#[derive(Copy, Clone, Debug, Default, Serialize)]
pub struct Summary {
    pub pass: u32,
    pub warn: u32,
    pub fail: u32,
    pub skip: u32,
}

impl Summary {
    /// Exit code per issue spec: `0` nothing wrong, `1` warnings only,
    /// `2` any failures.
    pub fn exit_code(&self) -> i32 {
        if self.fail > 0 {
            2
        } else if self.warn > 0 {
            1
        } else {
            0
        }
    }
}

/// Run the doctor subcommand end-to-end.
///
/// Returns the fully rendered [`Report`] so callers (the CLI driver, the
/// bundle writer, integration tests) can decide how to present it. The
/// exit code is derived from [`Summary::exit_code`].
pub async fn run(opts: DoctorOptions) -> Result<Report> {
    let all_checks = checks::all();
    let selected: Vec<&'static Check> = all_checks
        .iter()
        .filter(|c| passes_filter(c.id, &opts.only, &opts.skip))
        .copied()
        .collect();

    let ctx = Arc::new(opts.to_ctx());
    let semaphore = Arc::new(tokio::sync::Semaphore::new(CHECK_PARALLELISM));
    let mut handles = Vec::with_capacity(selected.len());

    for check in selected {
        let ctx = Arc::clone(&ctx);
        let semaphore = Arc::clone(&semaphore);
        handles.push(tokio::spawn(async move {
            // Acquire a slot in the parallel-execution pool. Only drops
            // when the check returns.
            let _permit = semaphore
                .acquire()
                .await
                .expect("semaphore closed unexpectedly");
            run_check(check, ctx).await
        }));
    }

    let mut outcomes = Vec::with_capacity(handles.len());
    for h in handles {
        match h.await {
            Ok(outcome) => outcomes.push(outcome),
            Err(join_err) => {
                // A spawn-join failure should never occur under normal
                // operation, but if it does we want a visible record
                // rather than a silent drop.
                outcomes.push(CheckOutcome {
                    id: "doctor.internal.join".to_string(),
                    title: "Check runner panic".to_string(),
                    status: "fail",
                    message: format!("check task failed to join: {join_err}"),
                    fix: Some("file a bug with the doctor output".to_string()),
                    duration_ms: 0,
                });
            }
        }
    }

    // Stable sort by check ID so the output order is deterministic across
    // runs — the parallel executor can finish checks in any order.
    outcomes.sort_by(|a, b| a.id.cmp(&b.id));

    let summary = summarise(&outcomes);
    let version = env!("CARGO_PKG_VERSION").to_string();
    let timestamp = chrono::Utc::now().to_rfc3339();

    Ok(Report {
        schema: REPORT_SCHEMA_VERSION,
        version,
        timestamp,
        summary,
        checks: outcomes,
    })
}

async fn run_check(check: &'static Check, ctx: Arc<CheckCtx>) -> CheckOutcome {
    let start = Instant::now();
    // Every check runs under the hard 3-second ceiling regardless of how
    // many bounded-timeout exec calls it makes internally. `spawn_blocking`
    // is used because several checks call synchronous FS / env / libc
    // helpers that are cheapest to invoke in blocking mode.
    let check_run = check.run;
    let ctx_owned = (*ctx).clone();
    let join = tokio::task::spawn_blocking(move || (check_run)(&ctx_owned));

    let result = match tokio::time::timeout(CHECK_HARD_TIMEOUT, join).await {
        Ok(Ok(r)) => r,
        Ok(Err(join_err)) => CheckResult::Warn(
            format!("internal error: {join_err}"),
            Some("file a bug with the doctor output".to_string()),
        ),
        Err(_) => CheckResult::Warn(
            "check timed out".to_string(),
            Some(format!(
                "this check exceeded its {CHECK_HARD_TIMEOUT:?} budget; rerun with --verbose or report a bug"
            )),
        ),
    };

    let duration_ms = start.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
    let status = result.tag().to_ascii_lowercase();
    let (message, fix) = split_result(result);
    CheckOutcome {
        id: check.id.to_string(),
        title: check.title.to_string(),
        status: status_tag(&status),
        message,
        fix,
        duration_ms,
    }
}

fn split_result(r: CheckResult) -> (String, Option<String>) {
    match r {
        CheckResult::Pass(m) | CheckResult::Skip(m) => (m, None),
        CheckResult::Warn(m, fix) | CheckResult::Fail(m, fix) => (m, fix),
    }
}

fn status_tag(lower: &str) -> &'static str {
    // Map the owned lowercased tag back to a `&'static str` for the
    // serialised report — `CheckOutcome.status` is `&'static str` to
    // avoid allocating per-outcome.
    match lower {
        "pass" => "pass",
        "warn" => "warn",
        "fail" => "fail",
        _ => "skip",
    }
}

fn summarise(outcomes: &[CheckOutcome]) -> Summary {
    let mut s = Summary::default();
    for o in outcomes {
        match o.status {
            "pass" => s.pass += 1,
            "warn" => s.warn += 1,
            "fail" => s.fail += 1,
            "skip" => s.skip += 1,
            _ => {}
        }
    }
    s
}

/// Return `true` when `id` should run given `only` and `skip` filters.
///
/// Filter semantics:
/// * `only` takes precedence. When non-empty, only IDs that match any
///   entry are included.
/// * An entry matches if the check ID equals it, or if the check ID
///   starts with it followed by `.` (prefix-with-dot match), so users
///   can write `--only privileges` to run every `privileges.*` check.
/// * `skip` is consulted for every ID that passed the `only` filter.
pub fn passes_filter(id: &str, only: &[String], skip: &[String]) -> bool {
    if !only.is_empty() && !only.iter().any(|f| id_matches_prefix(id, f)) {
        return false;
    }
    !skip.iter().any(|f| id_matches_prefix(id, f))
}

fn id_matches_prefix(id: &str, prefix: &str) -> bool {
    if prefix.is_empty() {
        return false;
    }
    if id == prefix {
        return true;
    }
    id.starts_with(prefix) && id[prefix.len()..].starts_with('.')
}

/// Resolve the bundle path — returns `None` when `--bundle` was not
/// supplied, otherwise the pre-normalised [`PathBuf`]. Kept around so
/// library callers that embed the doctor can introspect whether a bundle
/// will be written without re-reading [`DoctorOptions::bundle_path`].
#[allow(dead_code)]
pub fn bundle_path(opts: &DoctorOptions) -> Option<&std::path::Path> {
    opts.bundle_path.as_deref()
}

/// CLI-facing driver: run the report, print it, optionally write the
/// bundle, return the exit code.
pub async fn run_cli(args: &DoctorArgs) -> Result<i32> {
    let opts = DoctorOptions::from_args(args);
    let report = run(opts.clone()).await.context("doctor run failed")?;

    // Print the report to stdout in the requested format.
    let redact = opts.redact_options();
    if opts.json {
        report::render_json(&report, &redact, std::io::stdout().lock())
            .context("failed to render JSON report")?;
    } else {
        report::render_human(&report, &redact, &opts, std::io::stdout().lock())
            .context("failed to render human report")?;
    }

    // Write the bundle if requested. The bundle writer re-renders the
    // report internally so the in-archive copy is always well-formed
    // regardless of what was streamed to stdout.
    if let Some(path) = opts.bundle_path.as_ref() {
        bundle::write_bundle(path, &report, &opts)
            .with_context(|| format!("failed to write support bundle to {}", path.display()))?;
    }

    Ok(report.summary.exit_code())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_only_prefix_wins() {
        let only = vec!["privileges".to_string()];
        let skip = vec![];
        assert!(passes_filter("privileges.user", &only, &skip));
        assert!(!passes_filter("platform.os", &only, &skip));
    }

    #[test]
    fn filter_skip_after_only() {
        let only = vec!["nvidia".to_string()];
        let skip = vec!["nvidia.nvml.loadable".to_string()];
        assert!(passes_filter("nvidia.driver", &only, &skip));
        assert!(!passes_filter("nvidia.nvml.loadable", &only, &skip));
    }

    #[test]
    fn filter_empty_defaults_allow() {
        assert!(passes_filter("any.id", &[], &[]));
    }

    #[test]
    fn filter_exact_id_matches() {
        let only = vec!["platform.os".to_string()];
        assert!(passes_filter("platform.os", &only, &[]));
        assert!(!passes_filter("platform.osx", &only, &[]));
    }

    #[test]
    fn summary_exit_code_priority() {
        let s = Summary {
            pass: 5,
            warn: 2,
            fail: 0,
            skip: 0,
        };
        assert_eq!(s.exit_code(), 1);
        let s = Summary {
            pass: 5,
            warn: 2,
            fail: 1,
            skip: 0,
        };
        assert_eq!(s.exit_code(), 2);
        let s = Summary {
            pass: 5,
            warn: 0,
            fail: 0,
            skip: 0,
        };
        assert_eq!(s.exit_code(), 0);
    }
}
