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

//! `tpu.*` checks — libtpu presence, TPU_NAME, accel device vendor check.

use crate::doctor::types::{Check, CheckCtx, CheckResult, Severity};

static CHECKS: &[&Check] = &[&LIBTPU, &TPU_NAME, &ACCEL_VENDOR];

pub fn checks() -> &'static [&'static Check] {
    CHECKS
}

static LIBTPU: Check = Check {
    id: "tpu.libtpu",
    title: "libtpu presence",
    severity_on_fail: Severity::Warn,
    run: check_libtpu,
};

static TPU_NAME: Check = Check {
    id: "tpu.env.name",
    title: "TPU_NAME environment",
    severity_on_fail: Severity::Info,
    run: check_tpu_name,
};

static ACCEL_VENDOR: Check = Check {
    id: "tpu.accel.vendor",
    title: "/dev/accel* Google vendor ID",
    severity_on_fail: Severity::Info,
    run: check_accel_vendor,
};

fn check_libtpu(_ctx: &CheckCtx) -> CheckResult {
    #[cfg(target_os = "linux")]
    {
        // Common install locations — same set the internal reader searches.
        for p in &[
            "/usr/lib/libtpu.so",
            "/usr/local/lib/libtpu.so",
            "/opt/tpu/lib/libtpu.so",
        ] {
            if std::path::Path::new(p).exists() {
                return CheckResult::Pass(format!("libtpu at {p}"));
            }
        }
        if let Ok(lib_path) = std::env::var("TPU_LIBRARY_PATH")
            && std::path::Path::new(&lib_path).exists()
        {
            return CheckResult::Pass(format!("libtpu at {lib_path} (via TPU_LIBRARY_PATH)"));
        }
        CheckResult::Skip("libtpu not found".to_string())
    }
    #[cfg(not(target_os = "linux"))]
    {
        CheckResult::Skip("libtpu is Linux-only".to_string())
    }
}

fn check_tpu_name(_ctx: &CheckCtx) -> CheckResult {
    if let Ok(name) = std::env::var("TPU_NAME") {
        CheckResult::Pass(format!("TPU_NAME={name}"))
    } else {
        CheckResult::Skip("TPU_NAME not set".to_string())
    }
}

fn check_accel_vendor(_ctx: &CheckCtx) -> CheckResult {
    #[cfg(target_os = "linux")]
    {
        let accel = std::path::Path::new("/sys/class/accel");
        if !accel.exists() {
            return CheckResult::Skip("/sys/class/accel missing".to_string());
        }
        if let Ok(iter) = std::fs::read_dir(accel) {
            for entry in iter.flatten() {
                if let Some(name) = entry.file_name().to_str()
                    && let Ok(v) =
                        std::fs::read_to_string(format!("/sys/class/accel/{name}/device/vendor"))
                    && v.trim() == "0x1ae0"
                {
                    return CheckResult::Pass(format!("Google vendor on {name}"));
                }
            }
        }
        CheckResult::Skip("no accel* with Google vendor ID".to_string())
    }
    #[cfg(not(target_os = "linux"))]
    {
        CheckResult::Skip("/sys/class/accel is Linux-only".to_string())
    }
}
