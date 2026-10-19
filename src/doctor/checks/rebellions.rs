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
    // Probe `rbln-stat --json` — the same call the reader uses.
    for cmd in &["rbln-stat", "rbln-smi"] {
        if let Some(out) = try_exec(cmd, &["--json"], Duration::from_millis(2_000))
            && out.success()
        {
            return match kmd_version(&out.stdout) {
                Some(version) => CheckResult::Pass(format!("KMD {version}")),
                None => CheckResult::Pass(format!("{cmd} --json succeeded (no KMD_version field)")),
            };
        }
    }
    CheckResult::Skip("rbln-stat not available".to_string())
}

/// The driver version from `rbln-stat --json`, which reports it as the
/// top-level `KMD_version`. The tool has no `driver_version` key, which is
/// what this check used to look for.
fn kmd_version(json: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    let version = value.get("KMD_version")?.as_str()?.trim();
    (!version.is_empty()).then(|| version.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kmd_version_is_read_from_real_output() {
        let rebel100 = include_str!("../../../tests/fixtures/rbln-stat-rebel100.json");
        assert_eq!(kmd_version(rebel100).as_deref(), Some("3.5.1"));

        let atom_plus = include_str!("../../../tests/fixtures/rbln-stat-loaded.json");
        assert!(kmd_version(atom_plus).is_some());
    }

    #[test]
    fn missing_or_malformed_version_is_none() {
        assert_eq!(kmd_version(r#"{"devices": []}"#), None);
        assert_eq!(kmd_version(r#"{"KMD_version": ""}"#), None);
        assert_eq!(kmd_version(r#"{"KMD_version": 3}"#), None);
        assert_eq!(kmd_version("not json"), None);
    }
}
