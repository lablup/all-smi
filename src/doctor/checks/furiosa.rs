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

//! `furiosa.*` checks — feature flag enabled, furiosa-smi binary.

#[cfg(target_os = "linux")]
use crate::doctor::exec::which;
use crate::doctor::types::{Check, CheckCtx, CheckResult, Severity};

static CHECKS: &[&Check] = &[&FEATURE, &SMI];

pub fn checks() -> &'static [&'static Check] {
    CHECKS
}

static FEATURE: Check = Check {
    id: "furiosa.feature",
    title: "Furiosa feature compiled in",
    severity_on_fail: Severity::Info,
    run: check_feature,
};

static SMI: Check = Check {
    id: "furiosa.smi",
    title: "furiosa-smi binary",
    severity_on_fail: Severity::Warn,
    run: check_smi,
};

fn check_feature(_ctx: &CheckCtx) -> CheckResult {
    // The Furiosa backend is opt-in via the `furiosa` cargo feature. The
    // canonical way to detect it at runtime is `cfg(feature = "...")`.
    #[cfg(feature = "furiosa")]
    {
        CheckResult::Pass("compiled with `furiosa` feature".to_string())
    }
    #[cfg(not(feature = "furiosa"))]
    {
        CheckResult::Skip("compiled without `furiosa` feature".to_string())
    }
}

fn check_smi(_ctx: &CheckCtx) -> CheckResult {
    // furiosa-smi binary lookup. On non-Linux hosts it's a hard skip.
    #[cfg(target_os = "linux")]
    {
        for p in &["/usr/bin/furiosa-smi", "/usr/local/bin/furiosa-smi"] {
            if std::path::Path::new(p).exists() {
                return CheckResult::Pass(format!("present at {p}"));
            }
        }
        if let Some(path) = which("furiosa-smi") {
            return CheckResult::Pass(format!("furiosa-smi at {path}"));
        }
        CheckResult::Skip("furiosa-smi not found".to_string())
    }
    #[cfg(not(target_os = "linux"))]
    {
        CheckResult::Skip("Furiosa is Linux-only".to_string())
    }
}
