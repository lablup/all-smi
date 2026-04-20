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

//! `rebellions.*` checks — rbln-stat / rbln-smi path, driver version.

use std::time::Duration;

use crate::doctor::exec::{try_exec, which};
use crate::doctor::types::{Check, CheckCtx, CheckResult, Severity};

static CHECKS: &[&Check] = &[&RBLNSTAT, &DRIVER];

pub fn checks() -> &'static [&'static Check] {
    CHECKS
}

static RBLNSTAT: Check = Check {
    id: "rebellions.rblnstat",
    title: "rbln-stat binary",
    severity_on_fail: Severity::Warn,
    run: check_rblnstat,
};

static DRIVER: Check = Check {
    id: "rebellions.driver",
    title: "Rebellions driver",
    severity_on_fail: Severity::Info,
    run: check_driver,
};

fn check_rblnstat(_ctx: &CheckCtx) -> CheckResult {
    // Look for either `rbln-stat` or `rbln-smi` in canonical locations,
    // then fall back to PATH. Windows/macOS fall through to Skip.
    for p in &[
        "/usr/local/bin/rbln-stat",
        "/usr/bin/rbln-stat",
        "/usr/local/bin/rbln-smi",
        "/usr/bin/rbln-smi",
    ] {
        if std::path::Path::new(p).exists() {
            return CheckResult::Pass(format!("present at {p}"));
        }
    }
    for cmd in &["rbln-stat", "rbln-smi"] {
        if let Some(path) = which(cmd) {
            return CheckResult::Pass(format!("{cmd} at {path}"));
        }
    }
    CheckResult::Skip("neither rbln-stat nor rbln-smi found".to_string())
}

fn check_driver(_ctx: &CheckCtx) -> CheckResult {
    // Probe rbln-stat -j (JSON output) — same call the reader uses.
    for cmd in &["rbln-stat", "rbln-smi"] {
        if let Some(out) = try_exec(cmd, &["-j"], Duration::from_millis(2_000))
            && out.success()
        {
            // Try to pull a driver_version field out of the JSON without
            // decoding the whole payload.
            for line in out.stdout.lines() {
                let trimmed = line.trim();
                if let Some(idx) = trimmed.find("\"driver_version\"") {
                    let tail = &trimmed[idx..];
                    if let Some(start) = tail.find(':')
                        && let Some(rest) = tail.get(start + 1..)
                    {
                        let v = rest
                            .trim()
                            .trim_matches(|c: char| c == '"' || c == ',' || c.is_whitespace());
                        if !v.is_empty() {
                            return CheckResult::Pass(format!("driver {v}"));
                        }
                    }
                }
            }
            return CheckResult::Pass(format!("{cmd} -j succeeded (no driver_version field)"));
        }
    }
    CheckResult::Skip("rbln-stat not available".to_string())
}
