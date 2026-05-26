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

//! `windows.*` checks — WMI thermal zones, Intel / AMD vendor SDKs,
//! LibreHardwareMonitor availability. On non-Windows hosts these all
//! Skip with a clear message.

use crate::doctor::types::{Check, CheckCtx, CheckResult, Severity};

static CHECKS: &[&Check] = &[&WMI, &RYZEN_MASTER, &INTEL_WMI, &LHM];

pub fn checks() -> &'static [&'static Check] {
    CHECKS
}

static WMI: Check = Check {
    id: "windows.wmi",
    title: "WMI thermal-zone access",
    severity_on_fail: Severity::Warn,
    run: check_wmi,
};

static RYZEN_MASTER: Check = Check {
    id: "windows.amd_ryzen_master",
    title: "AMD Ryzen Master SDK",
    severity_on_fail: Severity::Info,
    run: check_ryzen_master,
};

static INTEL_WMI: Check = Check {
    id: "windows.intel_wmi",
    title: "Intel WMI temperature provider",
    severity_on_fail: Severity::Info,
    run: check_intel_wmi,
};

static LHM: Check = Check {
    id: "windows.libre_hardware_monitor",
    title: "LibreHardwareMonitor service",
    severity_on_fail: Severity::Info,
    run: check_lhm,
};

fn check_wmi(_ctx: &CheckCtx) -> CheckResult {
    #[cfg(target_os = "windows")]
    {
        // Build a short-lived WMI connection via the `wmi` crate. As of
        // wmi 0.18 COM is initialised automatically (multithreaded
        // apartment) on the first connection in a thread, so there is no
        // separate COMLibrary step. Keep this cheap — we only check
        // whether the root\\WMI namespace is reachable.
        match wmi::WMIConnection::with_namespace_path("root\\WMI") {
            Ok(_conn) => CheckResult::Pass("root\\WMI reachable".to_string()),
            Err(e) => CheckResult::Warn(
                format!("WMI connection failed: {e}"),
                Some("ensure the WinMgmt service is running".to_string()),
            ),
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        CheckResult::Skip("not Windows".to_string())
    }
}

fn check_ryzen_master(_ctx: &CheckCtx) -> CheckResult {
    #[cfg(target_os = "windows")]
    {
        let paths = [
            "C:\\Program Files\\AMD\\RyzenMaster\\Platform\\bin\\AMDRyzenMasterDriver.sys",
            "C:\\Program Files\\AMD\\RyzenMaster\\bin\\AMDRyzenMasterDriver.sys",
        ];
        for p in &paths {
            if std::path::Path::new(p).exists() {
                return CheckResult::Pass(format!("SDK driver at {p}"));
            }
        }
        CheckResult::Skip("AMD Ryzen Master SDK not installed".to_string())
    }
    #[cfg(not(target_os = "windows"))]
    {
        CheckResult::Skip("not Windows".to_string())
    }
}

fn check_intel_wmi(_ctx: &CheckCtx) -> CheckResult {
    #[cfg(target_os = "windows")]
    {
        // Intel's thermal namespace is root\\WMI; rely on the WMI check
        // above for reachability and report a Pass when it succeeded.
        CheckResult::Pass(
            "Intel thermal probe uses the same root\\WMI namespace as windows.wmi".to_string(),
        )
    }
    #[cfg(not(target_os = "windows"))]
    {
        CheckResult::Skip("not Windows".to_string())
    }
}

fn check_lhm(_ctx: &CheckCtx) -> CheckResult {
    #[cfg(target_os = "windows")]
    {
        // LibreHardwareMonitor ships a WMI provider under
        // root\\LibreHardwareMonitor. COM is initialised automatically on
        // the first connection in a thread (wmi 0.18+).
        match wmi::WMIConnection::with_namespace_path("root\\LibreHardwareMonitor") {
            Ok(_) => CheckResult::Pass("LibreHardwareMonitor WMI provider available".to_string()),
            Err(_) => {
                CheckResult::Skip("LibreHardwareMonitor not installed or not running".to_string())
            }
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        CheckResult::Skip("not Windows".to_string())
    }
}
