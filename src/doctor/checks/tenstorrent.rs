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

//! `tenstorrent.*` checks — luwen availability (compile-time), /dev node
//! presence, tt-kmd module load.

use crate::doctor::types::{Check, CheckCtx, CheckResult, Severity};

static CHECKS: &[&Check] = &[&LUWEN, &DEV_NODE, &KMD_MODULE];

pub fn checks() -> &'static [&'static Check] {
    CHECKS
}

static LUWEN: Check = Check {
    id: "tenstorrent.luwen",
    title: "luwen library linked",
    severity_on_fail: Severity::Info,
    run: check_luwen,
};

static DEV_NODE: Check = Check {
    id: "tenstorrent.kmd",
    title: "/dev/tenstorrent presence",
    severity_on_fail: Severity::Warn,
    run: check_dev_node,
};

static KMD_MODULE: Check = Check {
    id: "tenstorrent.module",
    title: "tt-kmd kernel module",
    severity_on_fail: Severity::Info,
    run: check_kmd,
};

fn check_luwen(_ctx: &CheckCtx) -> CheckResult {
    #[cfg(target_os = "linux")]
    {
        // The luwen crates are a compile-time Linux dep; if this binary
        // was built for Linux they are linked in.
        CheckResult::Pass("luwen linked (Linux build)".to_string())
    }
    #[cfg(not(target_os = "linux"))]
    {
        CheckResult::Skip("luwen is a Linux-only compile-time dep".to_string())
    }
}

fn check_dev_node(_ctx: &CheckCtx) -> CheckResult {
    #[cfg(target_os = "linux")]
    {
        let p = std::path::Path::new("/dev/tenstorrent");
        if !p.exists() {
            return CheckResult::Skip("/dev/tenstorrent missing".to_string());
        }
        match std::fs::read_dir(p) {
            Ok(iter) => {
                let nodes: Vec<String> = iter
                    .filter_map(|e| e.ok())
                    .filter_map(|e| e.file_name().into_string().ok())
                    .collect();
                if nodes.is_empty() {
                    CheckResult::Warn(
                        "/dev/tenstorrent exists but is empty".to_string(),
                        Some("verify tt-kmd loaded and claimed the device".to_string()),
                    )
                } else {
                    CheckResult::Pass(format!("{} node(s): {}", nodes.len(), nodes.join(", ")))
                }
            }
            Err(e) => CheckResult::Fail(
                format!("/dev/tenstorrent unreadable: {e}"),
                Some("add the user to the tenstorrent group and re-login".to_string()),
            ),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        CheckResult::Skip("/dev/tenstorrent is Linux-only".to_string())
    }
}

fn check_kmd(_ctx: &CheckCtx) -> CheckResult {
    #[cfg(target_os = "linux")]
    {
        if let Ok(modules) = std::fs::read_to_string("/proc/modules")
            && modules
                .lines()
                .any(|l| l.starts_with("tenstorrent ") || l.starts_with("tt_kmd "))
        {
            return CheckResult::Pass("tt-kmd module loaded".to_string());
        }
        CheckResult::Skip("tt-kmd not present in /proc/modules".to_string())
    }
    #[cfg(not(target_os = "linux"))]
    {
        CheckResult::Skip("/proc/modules is Linux-only".to_string())
    }
}
