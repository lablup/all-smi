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

//! `container.*` checks — runtime detection, cgroup v1/v2, resource
//! limits, Kubernetes ServiceAccount.
//!
//! Reuses [`crate::utils::runtime_environment`] so the doctor and the main
//! TUI agree on what "running in a container" means.

#[cfg(target_os = "linux")]
use std::path::Path;

use crate::doctor::types::{Check, CheckCtx, CheckResult, Severity};
#[cfg(target_os = "linux")]
use crate::utils::runtime_environment::{ContainerRuntime, detect_container_environment};

static CHECKS: &[&Check] = &[&RUNTIME, &CGROUP, &K8S_SA];

pub fn checks() -> &'static [&'static Check] {
    CHECKS
}

static RUNTIME: Check = Check {
    id: "container.runtime",
    title: "Container runtime",
    severity_on_fail: Severity::Info,
    run: check_runtime,
};

static CGROUP: Check = Check {
    id: "container.cgroup",
    title: "cgroup version + limits",
    severity_on_fail: Severity::Info,
    run: check_cgroup,
};

static K8S_SA: Check = Check {
    id: "container.k8s_serviceaccount",
    title: "Kubernetes ServiceAccount token",
    severity_on_fail: Severity::Info,
    run: check_k8s_sa,
};

fn check_runtime(_ctx: &CheckCtx) -> CheckResult {
    #[cfg(target_os = "linux")]
    {
        let info = detect_container_environment();
        match info.runtime {
            ContainerRuntime::None => {
                CheckResult::Pass("bare metal / VM (no container)".to_string())
            }
            _ => CheckResult::Pass(format!(
                "{}{}",
                info.runtime.as_str(),
                info.container_id
                    .as_ref()
                    .map(|id| format!(" (id={id})"))
                    .unwrap_or_default()
            )),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        CheckResult::Skip("container runtime detection is Linux-only".to_string())
    }
}

fn check_cgroup(_ctx: &CheckCtx) -> CheckResult {
    #[cfg(target_os = "linux")]
    {
        let version = detect_cgroup_version();
        let mut summary = vec![format!("cgroup{version}")];
        if let Some(cpu) = cpu_limit_summary() {
            summary.push(cpu);
        }
        if let Some(mem) = mem_limit_summary() {
            summary.push(mem);
        }
        if let Some(cpuset) = cpuset_summary() {
            summary.push(cpuset);
        }
        CheckResult::Pass(summary.join(", "))
    }
    #[cfg(not(target_os = "linux"))]
    {
        CheckResult::Skip("cgroup is a Linux concept".to_string())
    }
}

fn check_k8s_sa(_ctx: &CheckCtx) -> CheckResult {
    #[cfg(target_os = "linux")]
    {
        let token = Path::new("/var/run/secrets/kubernetes.io/serviceaccount/token");
        if token.exists() {
            let ns =
                std::fs::read_to_string("/var/run/secrets/kubernetes.io/serviceaccount/namespace")
                    .ok()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty());
            let msg = match ns {
                Some(n) => format!("present (namespace={n})"),
                None => "present".to_string(),
            };
            CheckResult::Pass(msg)
        } else if std::env::var("KUBERNETES_SERVICE_HOST").is_ok() {
            CheckResult::Warn(
                "KUBERNETES_SERVICE_HOST set but token is missing".to_string(),
                Some("check the ServiceAccount token mount".to_string()),
            )
        } else {
            CheckResult::Skip("not a Kubernetes pod".to_string())
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        CheckResult::Skip("Kubernetes pods are Linux-only in this tool".to_string())
    }
}

#[cfg(target_os = "linux")]
fn detect_cgroup_version() -> &'static str {
    // cgroup v2 mounts expose cgroup.controllers at the root; v1 has the
    // familiar cpu, memory, etc. subtrees under /sys/fs/cgroup.
    if Path::new("/sys/fs/cgroup/cgroup.controllers").exists() {
        "v2"
    } else if Path::new("/sys/fs/cgroup/cpu").exists()
        || Path::new("/sys/fs/cgroup/memory").exists()
    {
        "v1"
    } else {
        "unknown"
    }
}

#[cfg(target_os = "linux")]
fn cpu_limit_summary() -> Option<String> {
    // v2: cpu.max contains "<quota> <period>" or "max <period>".
    if let Ok(s) = std::fs::read_to_string("/sys/fs/cgroup/cpu.max") {
        let trimmed = s.trim();
        if trimmed.starts_with("max") {
            return Some("cpu.max=unlimited".to_string());
        }
        let parts: Vec<&str> = trimmed.split_whitespace().collect();
        if parts.len() == 2
            && let (Ok(q), Ok(p)) = (parts[0].parse::<i64>(), parts[1].parse::<i64>())
            && p > 0
        {
            let cpus = q as f64 / p as f64;
            return Some(format!("cpu.max={cpus:.2}"));
        }
    }
    // v1: cfs_quota_us / cfs_period_us.
    let quota = std::fs::read_to_string("/sys/fs/cgroup/cpu/cpu.cfs_quota_us")
        .ok()
        .and_then(|s| s.trim().parse::<i64>().ok());
    let period = std::fs::read_to_string("/sys/fs/cgroup/cpu/cpu.cfs_period_us")
        .ok()
        .and_then(|s| s.trim().parse::<i64>().ok());
    if let (Some(q), Some(p)) = (quota, period)
        && p > 0
    {
        if q < 0 {
            return Some("cpu.cfs=unlimited".to_string());
        }
        let cpus = q as f64 / p as f64;
        return Some(format!("cpu.cfs={cpus:.2}"));
    }
    None
}

#[cfg(target_os = "linux")]
fn mem_limit_summary() -> Option<String> {
    // v2: memory.max is "max" or bytes.
    if let Ok(s) = std::fs::read_to_string("/sys/fs/cgroup/memory.max") {
        let t = s.trim();
        if t == "max" {
            return Some("memory.max=unlimited".to_string());
        }
        if let Ok(bytes) = t.parse::<u64>() {
            let gib = bytes as f64 / (1024.0 * 1024.0 * 1024.0);
            return Some(format!("memory.max={gib:.2} GiB"));
        }
    }
    // v1: memory.limit_in_bytes.
    if let Ok(s) = std::fs::read_to_string("/sys/fs/cgroup/memory/memory.limit_in_bytes")
        && let Ok(bytes) = s.trim().parse::<u64>()
    {
        // 9223372036854771712 is the sentinel "no limit" value.
        if bytes >= u64::MAX / 4 {
            return Some("memory.limit=unlimited".to_string());
        }
        let gib = bytes as f64 / (1024.0 * 1024.0 * 1024.0);
        return Some(format!("memory.limit={gib:.2} GiB"));
    }
    None
}

#[cfg(target_os = "linux")]
fn cpuset_summary() -> Option<String> {
    // v2 layout: cpuset.cpus.effective is the authoritative value.
    if let Ok(s) = std::fs::read_to_string("/sys/fs/cgroup/cpuset.cpus.effective") {
        let t = s.trim();
        if !t.is_empty() {
            return Some(format!("cpuset={t}"));
        }
    }
    // v1 layout:
    if let Ok(s) = std::fs::read_to_string("/sys/fs/cgroup/cpuset/cpuset.cpus") {
        let t = s.trim();
        if !t.is_empty() {
            return Some(format!("cpuset={t}"));
        }
    }
    None
}
