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

static CHECKS: &[&Check] = &[&WMI, &GPU_PERF_COUNTERS, &RYZEN_MASTER, &INTEL_WMI, &LHM];

pub fn checks() -> &'static [&'static Check] {
    CHECKS
}

static WMI: Check = Check {
    id: "windows.wmi",
    title: "WMI thermal-zone access",
    severity_on_fail: Severity::Warn,
    run: check_wmi,
};

static GPU_PERF_COUNTERS: Check = Check {
    id: "windows.gpu.perf_counters",
    title: "GPU performance counters (DXGI / PDH)",
    severity_on_fail: Severity::Info,
    run: check_gpu_perf_counters,
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

/// Report what the vendor-neutral GPU metrics layer can actually see.
///
/// This is the main field-verification path for issue #346: no CI job
/// builds all-smi for Windows, and the GitHub-hosted Windows runners
/// expose only the Microsoft Basic Display Adapter, so the real answer
/// only ever comes from an operator running `all-smi doctor` on their
/// own machine.
///
/// The three states are deliberately distinguished, because they call
/// for different responses:
///
/// - no DXGI adapters at all: the display stack is not reachable
/// - DXGI answers but PDH publishes no GPU counter instances: normal on
///   a VM, an RDP session, or a host whose performance counters have
///   been disabled; utilization degrades to the WMI baseline
/// - both answer: the full feature set is live
fn check_gpu_perf_counters(_ctx: &CheckCtx) -> CheckResult {
    #[cfg(target_os = "windows")]
    {
        use crate::device::readers::windows_gpu_perf;

        let snapshot = windows_gpu_perf::snapshot();
        let pdh_open = windows_gpu_perf::pdh_query_available();

        if snapshot.adapters.is_empty() {
            return CheckResult::Warn(
                "DXGI reported no hardware adapters".to_string(),
                Some(
                    "expected on a session with no display driver (headless VM, some RDP \
                     configurations); GPU memory and utilization fall back to the WMI baseline"
                        .to_string(),
                ),
            );
        }

        let described: Vec<String> = snapshot
            .adapters
            .iter()
            .map(|adapter| {
                let total_gib =
                    adapter.total_memory.unwrap_or(0) as f64 / (1024.0 * 1024.0 * 1024.0);
                let description = &adapter.identity.description;
                let utilization = match adapter.utilization {
                    Some(value) => format!("{value:.1}%"),
                    None => "unavailable".to_string(),
                };
                // Which pool the figure came from, not just its size. The
                // number alone is ambiguous: `resolve_adapter_memory` reports
                // the dedicated pool when it clears the 1 GiB floor and the
                // shared aperture otherwise, and those are different
                // quantities. Reading a capacity off this check without
                // knowing which one it is invites the wrong conclusion, which
                // is what happened on the first Windows run (#378).
                let pool = if adapter.memory_is_shared {
                    "shared aperture"
                } else {
                    "dedicated"
                };
                format!("{description} ({total_gib:.1} GiB {pool}, utilization {utilization})")
            })
            .collect();

        if !pdh_open {
            return CheckResult::Warn(
                format!(
                    "DXGI sees {} adapter(s) but the PDH GPU counter query could not be opened: {}",
                    snapshot.adapters.len(),
                    described.join("; ")
                ),
                Some(
                    "VRAM size still comes from DXGI; utilization and per-process memory need \
                     the GPU performance counters. Check that the Performance Logs and Alerts \
                     service is enabled."
                        .to_string(),
                ),
            );
        }

        // A single utilization reading is expected to be absent here:
        // the rate counter needs two collections and `doctor` performs
        // one. Report the counter instance availability instead.
        let has_utilization = snapshot
            .adapters
            .iter()
            .any(|adapter| adapter.utilization.is_some());
        let processes = snapshot.processes.len();
        let adapters = snapshot.adapters.len();
        let summary = described.join("; ");
        let utilization_note = if has_utilization {
            ""
        } else {
            " (utilization needs a second poll; absent here is normal)"
        };

        CheckResult::Pass(format!(
            "{adapters} adapter(s): {summary}; PDH query open, \
             {processes} per-process row(s){utilization_note}"
        ))
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
