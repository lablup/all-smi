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

//! `nvidia.*` checks — NVML loadability, driver version, nvidia-smi
//! presence, MIG / vGPU / VISIBLE env knobs.

use std::time::Duration;

use crate::doctor::exec::{try_exec, which};
use crate::doctor::types::{Check, CheckCtx, CheckResult, Severity};

static CHECKS: &[&Check] = &[
    &NVML_LOADABLE,
    &SMI_BINARY,
    &DRIVER_VERSION,
    &VISIBLE_ENV,
    &MIG_MODE,
];

pub fn checks() -> &'static [&'static Check] {
    CHECKS
}

static NVML_LOADABLE: Check = Check {
    id: "nvidia.nvml.loadable",
    title: "NVML library loadable",
    severity_on_fail: Severity::Error,
    run: check_nvml_loadable,
};

static SMI_BINARY: Check = Check {
    id: "nvidia.smi.binary",
    title: "nvidia-smi binary",
    severity_on_fail: Severity::Warn,
    run: check_smi,
};

static DRIVER_VERSION: Check = Check {
    id: "nvidia.driver.version",
    title: "Driver version",
    severity_on_fail: Severity::Warn,
    run: check_driver_version,
};

static VISIBLE_ENV: Check = Check {
    id: "nvidia.env.visible_devices",
    title: "NVIDIA_VISIBLE_DEVICES / CUDA_VISIBLE_DEVICES",
    severity_on_fail: Severity::Info,
    run: check_visible_env,
};

static MIG_MODE: Check = Check {
    id: "nvidia.mig.mode",
    title: "MIG mode",
    severity_on_fail: Severity::Info,
    run: check_mig,
};

fn check_nvml_loadable(_ctx: &CheckCtx) -> CheckResult {
    // The `nvml-wrapper` crate loads libnvidia-ml via dlopen on first
    // Nvml::init(). Attempting the init is the cheapest authoritative test:
    // if the library is missing or the driver isn't usable, init() fails
    // with a discriminator error.
    match nvml_wrapper::Nvml::init() {
        Ok(_) => CheckResult::Pass("NVML initialised".to_string()),
        Err(e) => {
            let msg = e.to_string();
            let fix = if msg.to_lowercase().contains("driver") {
                Some(
                    "install or update the NVIDIA driver (nvidia-driver-580+ recommended)"
                        .to_string(),
                )
            } else if msg.to_lowercase().contains("library") || msg.to_lowercase().contains("load")
            {
                Some(
                    "set LD_LIBRARY_PATH to include the directory containing libnvidia-ml.so.1"
                        .to_string(),
                )
            } else {
                Some("re-run `nvidia-smi` manually for a more detailed driver error".to_string())
            };
            CheckResult::Fail(msg, fix)
        }
    }
}

fn check_smi(_ctx: &CheckCtx) -> CheckResult {
    let path = match which("nvidia-smi") {
        Some(p) => p,
        None => {
            return CheckResult::Skip(
                "nvidia-smi not found on PATH (may not be a GPU host)".to_string(),
            );
        }
    };

    match try_exec("nvidia-smi", &["--version"], Duration::from_millis(2_000)) {
        Some(out) if out.success() => {
            let first = out
                .stdout
                .lines()
                .next()
                .unwrap_or("nvidia-smi found")
                .trim();
            CheckResult::Pass(format!("{first} ({path})"))
        }
        Some(out) if out.timed_out => CheckResult::Warn(
            format!("nvidia-smi at {path} timed out"),
            Some(
                "driver is likely hung; try `sudo rmmod nvidia_uvm nvidia_drm nvidia_modeset nvidia` and reload"
                    .to_string(),
            ),
        ),
        Some(out) => CheckResult::Fail(
            format!(
                "nvidia-smi at {path} returned status {}: {}",
                out.status,
                out.stderr.trim()
            ),
            Some("investigate with `nvidia-smi -q`".to_string()),
        ),
        None => CheckResult::Skip(format!("nvidia-smi at {path} could not be launched")),
    }
}

fn check_driver_version(_ctx: &CheckCtx) -> CheckResult {
    // Prefer /proc/driver/nvidia/version when available — doesn't spawn
    // a subprocess and is authoritative on Linux.
    #[cfg(target_os = "linux")]
    {
        if let Ok(s) = std::fs::read_to_string("/proc/driver/nvidia/version") {
            let first = s.lines().next().unwrap_or("").trim();
            if !first.is_empty() {
                return CheckResult::Pass(first.to_string());
            }
        }
    }

    // Fallback: ask nvidia-smi.
    match try_exec(
        "nvidia-smi",
        &[
            "--query-gpu=driver_version",
            "--format=csv,noheader,nounits",
        ],
        Duration::from_millis(2_000),
    ) {
        Some(out) if out.success() => {
            let v = out.stdout.lines().next().unwrap_or("").trim();
            if v.is_empty() {
                CheckResult::Skip("nvidia-smi returned empty driver_version".to_string())
            } else {
                CheckResult::Pass(v.to_string())
            }
        }
        Some(out) if out.timed_out => CheckResult::Warn(
            "nvidia-smi timed out while reading driver version".to_string(),
            Some("driver may be hung".to_string()),
        ),
        _ => CheckResult::Skip("nvidia-smi not available".to_string()),
    }
}

fn check_visible_env(_ctx: &CheckCtx) -> CheckResult {
    let cuda = std::env::var("CUDA_VISIBLE_DEVICES").ok();
    let nvidia = std::env::var("NVIDIA_VISIBLE_DEVICES").ok();
    let mut msgs = Vec::new();
    if let Some(v) = cuda.as_ref() {
        msgs.push(format!("CUDA_VISIBLE_DEVICES={v}"));
    }
    if let Some(v) = nvidia.as_ref() {
        msgs.push(format!("NVIDIA_VISIBLE_DEVICES={v}"));
    }
    if msgs.is_empty() {
        return CheckResult::Pass("no GPU visibility restrictions".to_string());
    }

    // Heuristic warning: if CUDA_VISIBLE_DEVICES is literally empty or "-1",
    // CUDA will hide every GPU — a common foot-gun.
    if cuda.as_deref() == Some("") || cuda.as_deref() == Some("-1") {
        return CheckResult::Warn(
            msgs.join(", "),
            Some(
                "CUDA_VISIBLE_DEVICES is set to a value that hides every GPU; unset or list indices"
                    .to_string(),
            ),
        );
    }
    CheckResult::Pass(msgs.join(", "))
}

fn check_mig(_ctx: &CheckCtx) -> CheckResult {
    // Ask nvidia-smi directly so we don't need NVML's MIG support paths.
    match try_exec(
        "nvidia-smi",
        &["--query-gpu=mig.mode.current", "--format=csv,noheader"],
        Duration::from_millis(2_000),
    ) {
        Some(out) if out.success() => {
            let modes: Vec<&str> = out.stdout.lines().map(|l| l.trim()).collect();
            if modes.is_empty() {
                return CheckResult::Skip("no MIG info reported".to_string());
            }
            let enabled = modes
                .iter()
                .filter(|m| m.eq_ignore_ascii_case("Enabled"))
                .count();
            let disabled = modes.len() - enabled;
            if enabled == 0 {
                CheckResult::Pass("MIG disabled on all GPUs".to_string())
            } else {
                CheckResult::Pass(format!(
                    "MIG enabled on {enabled} / {} GPUs (disabled on {disabled})",
                    modes.len()
                ))
            }
        }
        Some(out) if out.timed_out => CheckResult::Warn(
            "nvidia-smi MIG query timed out".to_string(),
            Some("driver may be hung".to_string()),
        ),
        _ => CheckResult::Skip("nvidia-smi not available for MIG query".to_string()),
    }
}
