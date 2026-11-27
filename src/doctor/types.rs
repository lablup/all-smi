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
}

/// Per-invocation state threaded into every check. Kept tiny on purpose so
/// adding a new check only needs to read a handful of fields.
#[derive(Clone, Debug)]
pub struct CheckCtx {
    /// `--verbose` flag: checks may opt into extra work when set.
    // Read only by the Level Zero checks.
    #[cfg_attr(not(all_smi_level_zero), allow(dead_code))]
    pub verbose: bool,
    /// Remote endpoints supplied via `--remote-check`. Used only by the
    /// `network.*` checks.
    pub remote_checks: Vec<String>,
    /// Hard timeout used as the ceiling for any command this check spawns
    /// via the doctor's exec helpers. Individual checks can also be wrapped
    /// in a coarser deadline at the orchestrator level.
    // Read only by the Linux Neuron checks.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub command_timeout: Duration,
}

impl Default for CheckCtx {
    fn default() -> Self {
        Self {
            verbose: false,
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
pub struct Check {
    /// Stable ID (documented in the README) used by `--skip` / `--only`
    /// filters and by JSON consumers.
    pub id: &'static str,
    /// Short title shown in the human-readable output above the message.
    pub title: &'static str,
    /// Check body. Must be blocking-safe: the orchestrator drives each
    /// invocation through `tokio::task::spawn_blocking`.
    pub run: fn(&CheckCtx) -> CheckResult,
}
