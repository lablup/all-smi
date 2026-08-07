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

//! `amd.*` checks — ROCm, libamdgpu_top, DRI access, musl-build gating.

#[cfg(target_os = "linux")]
use std::time::Duration;

#[cfg(target_os = "linux")]
use crate::doctor::exec::try_exec;
use crate::doctor::types::{Check, CheckCtx, CheckResult, Severity};

static CHECKS: &[&Check] = &[
    &ROCM_VERSION,
    &LIBAMDGPU_TOP_ABI,
    &DRI_PERMS,
    &MUSL_GATE,
    &ADL_LIBRARY,
    &ADL_SENSORS,
];

pub fn checks() -> &'static [&'static Check] {
    CHECKS
}

static ROCM_VERSION: Check = Check {
    id: "amd.rocm.version",
    title: "ROCm version",
    severity_on_fail: Severity::Warn,
    run: check_rocm,
};

static LIBAMDGPU_TOP_ABI: Check = Check {
    id: "amd.libamdgpu_top.abi",
    title: "libamdgpu_top ABI",
    severity_on_fail: Severity::Warn,
    run: check_libamdgpu_top,
};

static DRI_PERMS: Check = Check {
    id: "amd.dri.perms",
    title: "/dev/dri permissions",
    severity_on_fail: Severity::Warn,
    run: check_dri_perms,
};

static MUSL_GATE: Check = Check {
    id: "amd.build.target_env",
    title: "AMD build-time availability",
    severity_on_fail: Severity::Warn,
    run: check_musl_gate,
};

static ADL_LIBRARY: Check = Check {
    id: "amd.adl.library",
    title: "AMD ADL library (atiadlxx.dll)",
    severity_on_fail: Severity::Info,
    run: check_adl_library,
};

static ADL_SENSORS: Check = Check {
    id: "amd.adl.sensors",
    title: "AMD ADL PMLog sensors",
    severity_on_fail: Severity::Info,
    run: check_adl_sensors,
};

/// Report whether `atiadlxx.dll` loaded and an ADL2 context was created.
fn check_adl_library(_ctx: &CheckCtx) -> CheckResult {
    #[cfg(target_os = "windows")]
    {
        use crate::device::readers::amd_adl::loader;

        let path = loader::dll_path();
        if !std::path::Path::new(path).exists() {
            return CheckResult::Skip(format!(
                "{path} not present; install AMD's graphics driver for temperature, power, fan, \
                 and clock readings"
            ));
        }
        if !loader::library_available() {
            return CheckResult::Warn(
                format!("{path} exists but ADL2_Main_Control_Create failed"),
                Some(
                    "the driver install may be partial; reinstalling AMD Software usually \
                     restores it"
                        .to_string(),
                ),
            );
        }
        match loader::selected_adapter_index() {
            Some(index) => CheckResult::Pass(format!(
                "{path} loaded, PMLog-capable adapter index {index}"
            )),
            None => CheckResult::Warn(
                format!("{path} loaded but no adapter exposes the PMLog sensor table"),
                Some(
                    "expected on pre-Vega cards, whose sensors live behind the legacy Overdrive \
                     5/6/7 entry points that all-smi does not implement"
                        .to_string(),
                ),
            ),
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        CheckResult::Skip("ADL is Windows-only".to_string())
    }
}

/// Dump the PMLog sensor table as raw `index=value` pairs.
///
/// This is the field-verification path for the sensor index mapping in
/// `device/readers/amd_adl/sensors.rs`. Those indices are transcribed
/// from AMD's public headers and cannot be checked by anything in CI:
/// no job compiles all-smi for Windows and no test can call the real
/// library. Printing the raw table lets an operator on real hardware
/// confirm or correct the mapping without a code change shipping first.
///
/// The dump is deliberately unfiltered and shows indices rather than
/// names, because an unexpected index is exactly the evidence needed.
fn check_adl_sensors(_ctx: &CheckCtx) -> CheckResult {
    #[cfg(target_os = "windows")]
    {
        use crate::device::readers::amd_adl::{loader, sensors};

        let Some(output) = loader::sample() else {
            return CheckResult::Skip(
                "no PMLog sample available (see amd.adl.library)".to_string(),
            );
        };

        let raw = sensors::supported_raw(&output);
        if raw.is_empty() {
            return CheckResult::Warn(
                "PMLog returned a table with no supported sensors".to_string(),
                None,
            );
        }

        let readout = sensors::extract(&output);
        let dump = raw
            .iter()
            .map(|(index, value)| format!("{index}={value}"))
            .collect::<Vec<_>>()
            .join(" ");

        let interpreted = format!(
            "edge={:?}C hotspot={:?}C mem={:?}C power={:?}W fan={:?}rpm gfx={:?}MHz \
             mclk={:?}MHz activity={:?}%",
            readout.temperature_edge_c,
            readout.temperature_hotspot_c,
            readout.temperature_mem_c,
            readout.power_w,
            readout.fan_rpm,
            readout.clock_gfx_mhz,
            readout.clock_mem_mhz,
            readout.activity_gfx_pct,
        );

        let count = raw.len();
        if readout.is_empty() {
            return CheckResult::Warn(
                format!(
                    "{count} sensor(s) reported supported but none passed the range guard. \
                     raw: {dump}"
                ),
                Some(
                    "this is what a shifted ADLSensorType enum looks like; please report the raw \
                     dump so the index mapping can be corrected"
                        .to_string(),
                ),
            );
        }

        CheckResult::Pass(format!(
            "{count} supported sensor(s); interpreted: {interpreted}; raw: {dump}"
        ))
    }
    #[cfg(not(target_os = "windows"))]
    {
        CheckResult::Skip("ADL is Windows-only".to_string())
    }
}

fn check_rocm(_ctx: &CheckCtx) -> CheckResult {
    #[cfg(target_os = "linux")]
    {
        // Check canonical install locations first.
        for path in &[
            "/opt/rocm/.info/version",
            "/opt/rocm/.info/version-dev",
            "/opt/rocm/lib/rocm-release-info/version",
        ] {
            if let Ok(s) = std::fs::read_to_string(path) {
                let v = s.trim();
                if !v.is_empty() {
                    return CheckResult::Pass(format!("ROCm {v} ({path})"));
                }
            }
        }
        // Fall back to `rocminfo`.
        if let Some(out) = try_exec("rocminfo", &[], Duration::from_millis(2_500))
            && out.success()
            && let Some(line) = out.stdout.lines().find(|l| l.contains("ROCm"))
        {
            return CheckResult::Pass(line.trim().to_string());
        }
        CheckResult::Skip("ROCm not detected (neither /opt/rocm nor rocminfo)".to_string())
    }
    #[cfg(not(target_os = "linux"))]
    {
        CheckResult::Skip("ROCm is Linux-only".to_string())
    }
}

fn check_libamdgpu_top(_ctx: &CheckCtx) -> CheckResult {
    #[cfg(all(target_os = "linux", not(target_env = "musl")))]
    {
        // The `libamdgpu_top` crate is linked at compile time; if this
        // binary was built with AMD support the dep is present. Surface
        // the crate version as the ABI identifier.
        CheckResult::Pass(format!(
            "linked libamdgpu_top {}",
            env!("CARGO_PKG_VERSION")
        ))
    }
    #[cfg(all(target_os = "linux", target_env = "musl"))]
    {
        CheckResult::Skip(
            "libamdgpu_top not linked in musl builds (see amd.build.target_env)".to_string(),
        )
    }
    #[cfg(not(target_os = "linux"))]
    {
        CheckResult::Skip("libamdgpu_top is Linux-only".to_string())
    }
}

fn check_dri_perms(_ctx: &CheckCtx) -> CheckResult {
    #[cfg(target_os = "linux")]
    {
        let p = std::path::Path::new("/dev/dri");
        if !p.exists() {
            return CheckResult::Skip("/dev/dri missing".to_string());
        }
        match std::fs::read_dir(p) {
            Ok(iter) => {
                let nodes: Vec<String> = iter
                    .filter_map(|e| e.ok())
                    .filter_map(|e| e.file_name().into_string().ok())
                    .filter(|n| n.starts_with("render") || n.starts_with("card"))
                    .collect();
                if nodes.is_empty() {
                    CheckResult::Skip("no card* or render* nodes".to_string())
                } else {
                    CheckResult::Pass(format!("{} node(s): {}", nodes.len(), nodes.join(", ")))
                }
            }
            Err(e) => CheckResult::Fail(
                format!("/dev/dri unreadable: {e}"),
                Some("ensure the caller is in the render group".to_string()),
            ),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        CheckResult::Skip("/dev/dri is Linux-only".to_string())
    }
}

fn check_musl_gate(_ctx: &CheckCtx) -> CheckResult {
    #[cfg(target_env = "musl")]
    {
        CheckResult::Warn(
            "musl build — AMD support compiled out".to_string(),
            Some("use a glibc build (x86_64-unknown-linux-gnu) for AMD GPU monitoring".to_string()),
        )
    }
    #[cfg(not(target_env = "musl"))]
    {
        CheckResult::Pass("glibc or non-Linux target — AMD support available".to_string())
    }
}
