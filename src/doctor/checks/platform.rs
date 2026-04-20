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

//! `platform.*` checks — OS, kernel, architecture, CPU, memory, uptime,
//! runtime target triple.

use std::time::Duration;

use crate::doctor::exec::try_exec;
use crate::doctor::types::{Check, CheckCtx, CheckResult, Severity};

static CHECKS: &[&Check] = &[&OS, &RUNTIME, &CPU, &MEMORY, &UPTIME, &HARDWARE];

pub fn checks() -> &'static [&'static Check] {
    CHECKS
}

static OS: Check = Check {
    id: "platform.os",
    title: "Operating system",
    severity_on_fail: Severity::Warn,
    run: check_os,
};

static RUNTIME: Check = Check {
    id: "platform.runtime",
    title: "Build target / runtime",
    severity_on_fail: Severity::Info,
    run: check_runtime,
};

static CPU: Check = Check {
    id: "platform.cpu",
    title: "CPU summary",
    severity_on_fail: Severity::Warn,
    run: check_cpu,
};

static MEMORY: Check = Check {
    id: "platform.memory",
    title: "System memory",
    severity_on_fail: Severity::Warn,
    run: check_memory,
};

static UPTIME: Check = Check {
    id: "platform.uptime",
    title: "System uptime",
    severity_on_fail: Severity::Info,
    run: check_uptime,
};

static HARDWARE: Check = Check {
    id: "platform.hardware",
    title: "Detected accelerator inventory",
    severity_on_fail: Severity::Info,
    run: check_hardware,
};

fn check_os(_ctx: &CheckCtx) -> CheckResult {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;

    // Best-effort kernel version: try `uname -sr` on Unix. Falls back to
    // a shorter message on Windows since `ver` output is less standard.
    #[cfg(unix)]
    {
        if let Some(out) = try_exec("uname", &["-sr"], Duration::from_millis(500))
            && out.success()
        {
            return CheckResult::Pass(format!("{} {arch}", out.stdout.trim()));
        }
    }
    #[cfg(windows)]
    {
        if let Some(out) = try_exec("cmd", &["/C", "ver"], Duration::from_millis(500))
            && out.success()
        {
            let ver = out
                .stdout
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("")
                .trim();
            if !ver.is_empty() {
                return CheckResult::Pass(format!("{ver} {arch}"));
            }
        }
    }

    CheckResult::Pass(format!("{os} {arch}"))
}

fn check_runtime(_ctx: &CheckCtx) -> CheckResult {
    // Target triple and libc flavour — important for the "why doesn't AMD
    // work in musl" question.
    let triple = current_target_triple();
    let env = current_target_env();

    let mut msg = format!("target {triple}");
    if !env.is_empty() {
        msg.push_str(&format!(" (env={env})"));
    }

    // The AMD backend is gated on `not(target_env = "musl")`; surface a
    // warning so users building with musl know AMD is unavailable.
    #[cfg(target_env = "musl")]
    {
        return CheckResult::Warn(
            format!("{msg} — musl builds do not include AMD GPU support"),
            Some("rebuild with a glibc target (e.g. x86_64-unknown-linux-gnu) for AMD".to_string()),
        );
    }

    #[allow(unreachable_code)]
    CheckResult::Pass(msg)
}

fn check_cpu(_ctx: &CheckCtx) -> CheckResult {
    let mut sys = sysinfo::System::new();
    sys.refresh_cpu_all();
    let cpus = sys.cpus();
    if cpus.is_empty() {
        return CheckResult::Warn(
            "no CPUs reported by sysinfo".to_string(),
            Some(
                "check /proc/cpuinfo readability and CPU affinity constraints (cgroups, cpusets)"
                    .to_string(),
            ),
        );
    }
    let brand = cpus[0].brand().trim();
    let count = num_cpus::get();
    let physical = num_cpus::get_physical();
    let name = if brand.is_empty() {
        "unknown model"
    } else {
        brand
    };
    CheckResult::Pass(format!("{name} ({physical} physical / {count} logical)"))
}

fn check_memory(_ctx: &CheckCtx) -> CheckResult {
    let mut sys = sysinfo::System::new();
    sys.refresh_memory();
    let total_kib = sys.total_memory() / 1024;
    if total_kib == 0 {
        return CheckResult::Warn(
            "sysinfo reported 0 B total memory".to_string(),
            Some(
                "kernel may be hiding /proc/meminfo — verify cgroup memory controller".to_string(),
            ),
        );
    }
    let total_gib = (total_kib as f64) / (1024.0 * 1024.0);
    CheckResult::Pass(format!("{total_gib:.1} GiB total"))
}

fn check_hardware(_ctx: &CheckCtx) -> CheckResult {
    // Pull the detection snapshot from the shared introspection API so
    // the doctor and `reader_factory` always agree on what hardware is
    // present (issue #188 reuse rule).
    let snap = crate::device::platform_detection::introspection::snapshot();
    let mut kinds: Vec<&'static str> = Vec::new();
    if snap.nvidia {
        kinds.push(if snap.jetson {
            "NVIDIA (Jetson)"
        } else {
            "NVIDIA"
        });
    }
    if snap.amd {
        kinds.push("AMD");
    }
    if snap.apple_silicon {
        kinds.push("Apple Silicon");
    }
    if snap.gaudi {
        kinds.push("Intel Gaudi");
    }
    if snap.google_tpu {
        kinds.push("Google TPU");
    }
    if snap.tenstorrent {
        kinds.push("Tenstorrent");
    }
    if snap.rebellions {
        kinds.push("Rebellions");
    }
    if snap.furiosa {
        kinds.push("FuriosaAI");
    }
    if kinds.is_empty() {
        CheckResult::Pass("no accelerators detected".to_string())
    } else {
        CheckResult::Pass(kinds.join(", "))
    }
}

fn check_uptime(_ctx: &CheckCtx) -> CheckResult {
    let secs = sysinfo::System::uptime();
    if secs == 0 {
        return CheckResult::Skip("uptime unavailable on this platform".to_string());
    }
    let days = secs / 86_400;
    let hours = (secs % 86_400) / 3_600;
    let mins = (secs % 3_600) / 60;
    CheckResult::Pass(format!("{days}d {hours}h {mins}m"))
}

fn current_target_triple() -> &'static str {
    // `cfg!` lookups are evaluated at compile time; listing each supported
    // triple keeps us inside `&'static str` without pulling build.rs in.
    if cfg!(all(
        target_arch = "x86_64",
        target_os = "linux",
        target_env = "gnu"
    )) {
        return "x86_64-unknown-linux-gnu";
    }
    if cfg!(all(
        target_arch = "x86_64",
        target_os = "linux",
        target_env = "musl"
    )) {
        return "x86_64-unknown-linux-musl";
    }
    if cfg!(all(
        target_arch = "aarch64",
        target_os = "linux",
        target_env = "gnu"
    )) {
        return "aarch64-unknown-linux-gnu";
    }
    if cfg!(all(
        target_arch = "aarch64",
        target_os = "linux",
        target_env = "musl"
    )) {
        return "aarch64-unknown-linux-musl";
    }
    if cfg!(all(target_arch = "x86_64", target_os = "macos")) {
        return "x86_64-apple-darwin";
    }
    if cfg!(all(target_arch = "aarch64", target_os = "macos")) {
        return "aarch64-apple-darwin";
    }
    if cfg!(all(
        target_arch = "x86_64",
        target_os = "windows",
        target_env = "msvc"
    )) {
        return "x86_64-pc-windows-msvc";
    }
    if cfg!(all(
        target_arch = "aarch64",
        target_os = "windows",
        target_env = "msvc"
    )) {
        return "aarch64-pc-windows-msvc";
    }
    "unknown"
}

fn current_target_env() -> &'static str {
    if cfg!(target_env = "musl") {
        return "musl";
    }
    if cfg!(target_env = "gnu") {
        return "gnu";
    }
    if cfg!(target_env = "msvc") {
        return "msvc";
    }
    ""
}
