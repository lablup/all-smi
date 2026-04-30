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

//! `gaudi.*` checks — hl-smi presence, HL device nodes, driver version.

#[cfg(target_os = "linux")]
use std::time::Duration;

#[cfg(target_os = "linux")]
use crate::doctor::exec::{try_exec, which};
use crate::doctor::types::{Check, CheckCtx, CheckResult, Severity};

static CHECKS: &[&Check] = &[&HLSMI_BINARY, &DEVICE_NODES, &DRIVER];

pub fn checks() -> &'static [&'static Check] {
    CHECKS
}

static HLSMI_BINARY: Check = Check {
    id: "gaudi.hlsmi",
    title: "hl-smi binary",
    severity_on_fail: Severity::Warn,
    run: check_hlsmi,
};

static DEVICE_NODES: Check = Check {
    id: "gaudi.devices",
    title: "Gaudi device nodes",
    severity_on_fail: Severity::Warn,
    run: check_devices,
};

static DRIVER: Check = Check {
    id: "gaudi.driver",
    title: "Habana driver version",
    severity_on_fail: Severity::Info,
    run: check_driver,
};

fn check_hlsmi(_ctx: &CheckCtx) -> CheckResult {
    #[cfg(target_os = "linux")]
    {
        // Search canonical install paths before falling back to PATH.
        for p in &[
            "/usr/bin/hl-smi",
            "/usr/local/bin/hl-smi",
            "/opt/habanalabs/bin/hl-smi",
        ] {
            if std::path::Path::new(p).exists() {
                return CheckResult::Pass(format!("present at {p}"));
            }
        }
        if let Some(path) = which("hl-smi") {
            return CheckResult::Pass(format!("present at {path}"));
        }
        CheckResult::Skip("hl-smi not found".to_string())
    }
    #[cfg(not(target_os = "linux"))]
    {
        CheckResult::Skip("Gaudi is Linux-only".to_string())
    }
}

fn check_devices(_ctx: &CheckCtx) -> CheckResult {
    #[cfg(target_os = "linux")]
    {
        // Accept both the legacy /dev/hl* layout and the newer /dev/accel/accel*.
        let accel = std::path::Path::new("/dev/accel");
        if accel.exists()
            && let Ok(iter) = std::fs::read_dir(accel)
        {
            let nodes: Vec<String> = iter
                .filter_map(|e| e.ok())
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|n| n.starts_with("accel"))
                .collect();
            // Verify the vendor ID so we don't mistakenly flag Google TPUs.
            let is_habana = nodes.iter().any(|n| {
                let sysfs = format!("/sys/class/accel/{n}/device/vendor");
                matches!(std::fs::read_to_string(&sysfs).map(|s| s.trim().to_string()), Ok(v) if v == "0x1da3")
            });
            if is_habana {
                return CheckResult::Pass(format!("{} Habana accel node(s)", nodes.len()));
            }
        }
        // Legacy layout:
        if std::path::Path::new("/dev/hl0").exists() {
            let mut count = 0;
            for i in 0..32 {
                if std::path::Path::new(&format!("/dev/hl{i}")).exists() {
                    count += 1;
                }
            }
            return CheckResult::Pass(format!("{count} /dev/hl* node(s)"));
        }
        CheckResult::Skip("no Habana device nodes found".to_string())
    }
    #[cfg(not(target_os = "linux"))]
    {
        CheckResult::Skip("Gaudi is Linux-only".to_string())
    }
}

fn check_driver(_ctx: &CheckCtx) -> CheckResult {
    #[cfg(target_os = "linux")]
    {
        // Ask hl-smi for driver version.
        if let Some(out) = try_exec(
            "hl-smi",
            &["-Q", "driver_version", "-f", "csv,noheader"],
            Duration::from_millis(2_000),
        ) && out.success()
        {
            let v = out.stdout.lines().next().unwrap_or("").trim();
            if !v.is_empty() {
                return CheckResult::Pass(v.to_string());
            }
        }
        CheckResult::Skip("hl-smi did not report a driver version".to_string())
    }
    #[cfg(not(target_os = "linux"))]
    {
        CheckResult::Skip("Gaudi is Linux-only".to_string())
    }
}
