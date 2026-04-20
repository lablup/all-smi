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

//! Core types shared across the `doctor` subcommand.

use std::time::Duration;

/// Severity tag associated with a `Fail` result. Consumers downgrade
/// fail-on-`Info` to a warning and treat `Error` as the exit-code-2 trigger.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Severity {
    /// Informational — even a `Fail` here does not affect the exit code.
    Info,
    /// Warning — the user probably wants to know but the feature still works.
    Warn,
    /// Error — the feature is broken or unsafe to run.
    Error,
}

/// Outcome of a single check.
///
/// `String` rather than `&'static str` for the message because many checks
/// compose a message from runtime state (version strings, detected paths,
/// etc.). The optional second field on `Warn` and `Fail` is a short
/// user-facing remediation hint.
#[derive(Clone, Debug)]
pub enum CheckResult {
    /// The probed subsystem is healthy; the string is a one-line confirmation.
    Pass(String),
    /// The probed subsystem works but has a concern; optional fix hint.
    Warn(String, Option<String>),
    /// The probed subsystem is broken; optional fix hint.
    Fail(String, Option<String>),
    /// The check does not apply to this host (wrong OS, missing binary, etc.).
    Skip(String),
}

impl CheckResult {
    /// Short tag used in human output and JSON status fields.
    pub fn tag(&self) -> &'static str {
        match self {
            CheckResult::Pass(_) => "PASS",
            CheckResult::Warn(_, _) => "WARN",
            CheckResult::Fail(_, _) => "FAIL",
            CheckResult::Skip(_) => "SKIP",
        }
    }

    /// Primary diagnostic message for the result.
    pub fn message(&self) -> &str {
        match self {
            CheckResult::Pass(m)
            | CheckResult::Warn(m, _)
            | CheckResult::Fail(m, _)
            | CheckResult::Skip(m) => m,
        }
    }

    /// Optional remediation hint shown below WARN/FAIL lines and embedded
    /// in the JSON `fix` field. Retained on the public API for library
    /// consumers that build custom renderers on top of [`CheckResult`].
    #[allow(dead_code)]
    pub fn fix(&self) -> Option<&str> {
        match self {
            CheckResult::Warn(_, fix) | CheckResult::Fail(_, fix) => fix.as_deref(),
            _ => None,
        }
    }
}

/// Per-invocation state threaded into every check. Kept tiny on purpose so
/// adding a new check only needs to read a handful of fields.
///
/// `verbose`, `include_identifiers`, and `command_timeout` are fields that
/// *some* checks read conditionally (e.g. platform-specific or verbose-only
/// probes). Compilers without the relevant platform trait them as dead;
/// `#[allow(dead_code)]` on the struct keeps them visible to the full
/// cross-platform surface without per-field cfg noise.
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct CheckCtx {
    /// `--verbose` flag: checks may opt into extra work when set.
    pub verbose: bool,
    /// `--include-identifiers`: redaction behaviour is centralised in
    /// [`crate::doctor::redact`], but a check that wants to emit identifiers
    /// in its `message` may consult this flag directly.
    pub include_identifiers: bool,
    /// Remote endpoints supplied via `--remote-check`. Used only by the
    /// `network.*` checks.
    pub remote_checks: Vec<String>,
    /// Hard timeout used as the ceiling for any command this check spawns
    /// via the doctor's exec helpers. Individual checks can also be wrapped
    /// in a coarser deadline at the orchestrator level.
    pub command_timeout: Duration,
}

impl Default for CheckCtx {
    fn default() -> Self {
        Self {
            verbose: false,
            include_identifiers: false,
            remote_checks: Vec::new(),
            command_timeout: Duration::from_millis(2_500),
        }
    }
}

/// Static descriptor for a single check.
///
/// `run` is a plain function pointer so we can keep the registry as a
/// `const`-ish vector assembled at startup without any `Arc<dyn Fn>`
/// indirection. Checks are pure; they receive `&CheckCtx` and return a
/// [`CheckResult`].
///
/// `severity_on_fail` is read by future renderers that want to classify
/// failures by severity (e.g. `Info` FAIL treated as WARN-equivalent
/// when the exit-code strictness flag is disabled). Current renderers map
/// status tags directly from the `CheckResult` variant, so the field
/// looks unused to the compiler — keep `#[allow(dead_code)]` until the
/// strict-mode flag lands.
#[allow(dead_code)]
pub struct Check {
    /// Stable ID (documented in the README) used by `--skip` / `--only`
    /// filters and by JSON consumers.
    pub id: &'static str,
    /// Short title shown in the human-readable output above the message.
    pub title: &'static str,
    /// Severity associated with a `Fail` result. `Info` fails do not
    /// influence the exit code.
    pub severity_on_fail: Severity,
    /// Check body. Must be blocking-safe: the orchestrator drives each
    /// invocation through `tokio::task::spawn_blocking`.
    pub run: fn(&CheckCtx) -> CheckResult,
}
