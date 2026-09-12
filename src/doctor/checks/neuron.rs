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

//! `neuron.*` checks — AWS Neuron (Trainium / Inferentia) device nodes,
//! `aws-neuronx-dkms` driver module, the driver's sysfs tree, and the
//! `aws-neuronx-tools` CLIs.
//!
//! This is the only vendor-specific diagnostic surface the project has:
//! there is no per-vendor health metric anywhere, so the PATH trap
//! described below has nowhere else to be reported.

use crate::doctor::types::{Check, CheckCtx, CheckResult, Severity};

static CHECKS: &[&Check] = &[&DEV_NODE, &DRIVER_MODULE, &SYSFS, &TOOLS];

pub fn checks() -> &'static [&'static Check] {
    CHECKS
}

/// Absolute paths of the Neuron CLIs. `/opt/aws/neuron/bin` is put on
/// `PATH` by the DLAMI profile scripts only, so under `sudo`, systemd,
/// or a container entrypoint an unqualified `neuron-ls` exits 127. The
/// reader always calls these by absolute path for that reason and this
/// check verifies the same paths.
#[cfg(target_os = "linux")]
const NEURON_LS_BIN: &str = "/opt/aws/neuron/bin/neuron-ls";
#[cfg(target_os = "linux")]
const NEURON_MONITOR_BIN: &str = "/opt/aws/neuron/bin/neuron-monitor";
#[cfg(target_os = "linux")]
const SYSFS_NEURON_ROOT: &str = "/sys/devices/virtual/neuron_device";

static DEV_NODE: Check = Check {
    id: "neuron.dev_node",
    title: "/dev/neuron* device nodes",
    severity_on_fail: Severity::Warn,
    run: check_dev_node,
};

static DRIVER_MODULE: Check = Check {
    id: "neuron.driver",
    title: "aws-neuronx-dkms kernel module",
    severity_on_fail: Severity::Warn,
    run: check_driver,
};

static SYSFS: Check = Check {
    id: "neuron.sysfs",
    title: "Neuron driver sysfs tree",
    severity_on_fail: Severity::Info,
    run: check_sysfs,
};

static TOOLS: Check = Check {
    id: "neuron.tools",
    title: "aws-neuronx-tools binaries",
    severity_on_fail: Severity::Warn,
    run: check_tools,
};

fn check_dev_node(_ctx: &CheckCtx) -> CheckResult {
    #[cfg(target_os = "linux")]
    {
        // The driver creates one /dev/neuronN per device, mode 0666
        // (world rw) with no group gate, so there is nothing to join and
        // every tool works fully as an unprivileged user.
        let nodes: Vec<String> = (0..crate::device::readers::common_cache::MAX_DEVICES)
            .map(|index| format!("/dev/neuron{index}"))
            .take_while(|path| std::path::Path::new(path).exists())
            .collect();
        if nodes.is_empty() {
            return CheckResult::Skip("/dev/neuron0 missing".to_string());
        }
        CheckResult::Pass(format!("{} node(s), first {}", nodes.len(), nodes[0]))
    }
    #[cfg(not(target_os = "linux"))]
    {
        CheckResult::Skip("AWS Neuron is Linux-only".to_string())
    }
}

fn check_driver(_ctx: &CheckCtx) -> CheckResult {
    #[cfg(target_os = "linux")]
    {
        let loaded = std::fs::read_to_string("/proc/modules")
            .map(|modules| modules.lines().any(|line| line.starts_with("neuron ")))
            .unwrap_or(false);
        if !loaded {
            return CheckResult::Skip("neuron module not present in /proc/modules".to_string());
        }
        match std::fs::read_to_string("/sys/module/neuron/version") {
            Ok(version) => CheckResult::Pass(format!("neuron {} loaded", version.trim())),
            Err(_) => CheckResult::Pass("neuron module loaded (version unreadable)".to_string()),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        CheckResult::Skip("/proc/modules is Linux-only".to_string())
    }
}

fn check_sysfs(_ctx: &CheckCtx) -> CheckResult {
    #[cfg(target_os = "linux")]
    {
        let root = std::path::Path::new(SYSFS_NEURON_ROOT);
        if !root.exists() {
            return CheckResult::Skip(format!("{SYSFS_NEURON_ROOT} missing"));
        }
        // Per-NeuronCore memory lives here and is populated with no
        // workload attached, so this tree — not neuron-monitor — is what
        // the reader depends on for memory.
        match std::fs::read_dir(root) {
            Ok(entries) => {
                let devices = entries.filter_map(|entry| entry.ok()).count();
                if devices == 0 {
                    return CheckResult::Warn(
                        format!("{SYSFS_NEURON_ROOT} exists but lists no device"),
                        Some("verify the neuron kernel module claimed the device".to_string()),
                    );
                }
                CheckResult::Pass(format!("{devices} device(s) under {SYSFS_NEURON_ROOT}"))
            }
            Err(error) => CheckResult::Fail(
                format!("{SYSFS_NEURON_ROOT} unreadable: {error}"),
                Some("reinstall aws-neuronx-dkms and reload the neuron module".to_string()),
            ),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        CheckResult::Skip("Neuron sysfs is Linux-only".to_string())
    }
}

fn check_tools(_ctx: &CheckCtx) -> CheckResult {
    #[cfg(target_os = "linux")]
    {
        let present: Vec<&str> = [NEURON_LS_BIN, NEURON_MONITOR_BIN]
            .into_iter()
            .filter(|path| std::path::Path::new(path).exists())
            .collect();
        if present.is_empty() {
            if std::path::Path::new("/dev/neuron0").exists() {
                return CheckResult::Warn(
                    "Neuron device present but /opt/aws/neuron/bin tools are missing".to_string(),
                    Some("install aws-neuronx-tools".to_string()),
                );
            }
            return CheckResult::Skip("aws-neuronx-tools not installed".to_string());
        }
        CheckResult::Pass(format!(
            "{} binary(ies): {}",
            present.len(),
            present.join(", ")
        ))
    }
    #[cfg(not(target_os = "linux"))]
    {
        CheckResult::Skip("AWS Neuron is Linux-only".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_ids_are_namespaced_and_runnable() {
        let ctx = CheckCtx::default();
        for check in checks() {
            assert!(check.id.starts_with("neuron."), "bad id {:?}", check.id);
            // Every check must be total: on a host without Neuron
            // hardware they all report Skip rather than panicking.
            let _ = (check.run)(&ctx);
        }
    }
}
