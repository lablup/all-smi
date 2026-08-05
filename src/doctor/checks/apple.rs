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

//! `apple.*` checks — macOS version, Apple Silicon flag, IOReport
//! accessibility, root requirement.

#[cfg(target_os = "macos")]
use std::time::Duration;

#[cfg(target_os = "macos")]
use crate::doctor::exec::try_exec;
use crate::doctor::types::{Check, CheckCtx, CheckResult, Severity};

static CHECKS: &[&Check] = &[&MACOS_VERSION, &APPLE_SILICON, &SMC_ROOT];

pub fn checks() -> &'static [&'static Check] {
    CHECKS
}

static MACOS_VERSION: Check = Check {
    id: "apple.macos.version",
    title: "macOS version",
    severity_on_fail: Severity::Info,
    run: check_macos_version,
};

static APPLE_SILICON: Check = Check {
    id: "apple.silicon",
    title: "Apple Silicon detection",
    severity_on_fail: Severity::Info,
    run: check_apple_silicon,
};

static SMC_ROOT: Check = Check {
    id: "apple.smc",
    title: "SMC accessibility",
    severity_on_fail: Severity::Warn,
    run: check_smc_root,
};

fn check_macos_version(_ctx: &CheckCtx) -> CheckResult {
    #[cfg(target_os = "macos")]
    {
        if let Some(out) = try_exec("sw_vers", &["-productVersion"], Duration::from_millis(500))
            && out.success()
        {
            let v = out.stdout.trim();
            if !v.is_empty() {
                return CheckResult::Pass(format!("macOS {v}"));
            }
        }
        CheckResult::Warn("sw_vers did not return a version".to_string(), None)
    }
    #[cfg(not(target_os = "macos"))]
    {
        CheckResult::Skip("not macOS".to_string())
    }
}

fn check_apple_silicon(_ctx: &CheckCtx) -> CheckResult {
    #[cfg(target_os = "macos")]
    {
        if crate::device::platform_detection::is_apple_silicon() {
            CheckResult::Pass("arm64 Apple Silicon".to_string())
        } else {
            CheckResult::Pass("Intel Mac".to_string())
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        CheckResult::Skip("not macOS".to_string())
    }
}

/// Probe the SMC by actually opening a connection.
///
/// This used to report a euid check and advise re-running under sudo, which
/// was wrong: IOReport and the SMC are readable by unprivileged processes,
/// which is the whole basis of macOS support here. The advice also became
/// actively misleading for Intel Macs, whose CPU temperature, fan speeds, and
/// chassis power all come from this connection.
fn check_smc_root(_ctx: &CheckCtx) -> CheckResult {
    #[cfg(target_os = "macos")]
    {
        match crate::device::macos_native::smc::SMC::new() {
            Ok(_) => CheckResult::Pass("SMC reachable (no sudo required)".to_string()),
            Err(e) => CheckResult::Warn(
                format!("SMC connection failed: {e}"),
                Some(
                    "CPU/GPU temperature, fan speeds, and chassis power will be unavailable"
                        .to_string(),
                ),
            ),
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        CheckResult::Skip("not macOS".to_string())
    }
}
