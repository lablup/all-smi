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

//! `privileges.*` checks — uid/euid, root status, group membership, device
//! node access. Windows is a structural skip because the UNIX uid/gid
//! model does not translate.

use std::time::Duration;

use crate::doctor::exec::try_exec;
use crate::doctor::types::{Check, CheckCtx, CheckResult, Severity};

static CHECKS: &[&Check] = &[&USER, &ROOT, &VIDEO_RENDER, &DEV_DRI, &DEV_TENSTORRENT];

pub fn checks() -> &'static [&'static Check] {
    CHECKS
}

static USER: Check = Check {
    id: "privileges.user",
    title: "Calling user identity",
    severity_on_fail: Severity::Info,
    run: check_user,
};

static ROOT: Check = Check {
    id: "privileges.root",
    title: "Root / sudo availability",
    severity_on_fail: Severity::Warn,
    run: check_root,
};

static VIDEO_RENDER: Check = Check {
    id: "privileges.video_render_group",
    title: "video/render group membership",
    severity_on_fail: Severity::Warn,
    run: check_video_render_group,
};

static DEV_DRI: Check = Check {
    id: "privileges.dev_dri",
    title: "/dev/dri access",
    severity_on_fail: Severity::Warn,
    run: check_dev_dri,
};

static DEV_TENSTORRENT: Check = Check {
    id: "privileges.dev_tenstorrent",
    title: "/dev/tenstorrent access",
    severity_on_fail: Severity::Info,
    run: check_dev_tenstorrent,
};

fn check_user(_ctx: &CheckCtx) -> CheckResult {
    #[cfg(unix)]
    {
        // SAFETY: getuid / geteuid are always-safe thread-local reads.
        let (uid, euid) = unsafe { (libc::getuid(), libc::geteuid()) };
        let name = whoami::username().unwrap_or_else(|_| "unknown".to_string());
        if uid == euid {
            CheckResult::Pass(format!("{name} (uid={uid})"))
        } else {
            CheckResult::Warn(
                format!("{name} (uid={uid}, euid={euid}) — setuid context detected"),
                None,
            )
        }
    }
    #[cfg(not(unix))]
    {
        let name = whoami::username().unwrap_or_else(|_| "unknown".to_string());
        CheckResult::Pass(name)
    }
}

fn check_root(_ctx: &CheckCtx) -> CheckResult {
    #[cfg(unix)]
    {
        // SAFETY: geteuid is always-safe.
        let euid = unsafe { libc::geteuid() };
        if euid == 0 {
            // macOS readers (Apple Silicon chassis) require root; on
            // Linux, running as root is merely convenient.
            return CheckResult::Pass("running as root".to_string());
        }
        let sudo_env = std::env::var("SUDO_USER").ok();
        if let Some(user) = sudo_env {
            return CheckResult::Pass(format!("non-root ({user}) via sudo parent"));
        }
        // On macOS the `local` mode needs root; surface a warn so the
        // user gets a hint.
        #[cfg(target_os = "macos")]
        return CheckResult::Warn(
            "not running as root; macOS `all-smi local` requires sudo".to_string(),
            Some("re-run with `sudo all-smi ...` or use `all-smi api`".to_string()),
        );
        #[cfg(not(target_os = "macos"))]
        return CheckResult::Pass("running as unprivileged user".to_string());
    }
    #[cfg(not(unix))]
    {
        CheckResult::Skip("root/uid model not applicable on this platform".to_string())
    }
}

fn check_video_render_group(_ctx: &CheckCtx) -> CheckResult {
    #[cfg(target_os = "linux")]
    {
        let groups = read_groups();
        let has_video = groups.iter().any(|g| g == "video");
        let has_render = groups.iter().any(|g| g == "render");

        match (has_video, has_render) {
            (true, true) => CheckResult::Pass("member of video + render".to_string()),
            (true, false) => CheckResult::Warn(
                "member of video but not render".to_string(),
                Some(
                    "run `sudo usermod -aG render $USER` and re-login for AMD/DRI access"
                        .to_string(),
                ),
            ),
            (false, true) => CheckResult::Warn(
                "member of render but not video".to_string(),
                Some(
                    "run `sudo usermod -aG video $USER` and re-login for legacy DRI access"
                        .to_string(),
                ),
            ),
            (false, false) => CheckResult::Warn(
                "not in video or render groups".to_string(),
                Some("run `sudo usermod -aG video,render $USER` and re-login".to_string()),
            ),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        CheckResult::Skip("video/render groups are a Linux concept".to_string())
    }
}

fn check_dev_dri(_ctx: &CheckCtx) -> CheckResult {
    #[cfg(target_os = "linux")]
    {
        let p = std::path::Path::new("/dev/dri");
        if !p.exists() {
            return CheckResult::Skip("/dev/dri does not exist (no DRI devices)".to_string());
        }
        match std::fs::read_dir(p) {
            Ok(iter) => {
                let count = iter.count();
                CheckResult::Pass(format!("{count} node(s) present"))
            }
            Err(e) => CheckResult::Fail(
                format!("/dev/dri unreadable: {e}"),
                Some("check the video/render group membership and device-cgroup rules".to_string()),
            ),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        CheckResult::Skip("/dev/dri is Linux-specific".to_string())
    }
}

fn check_dev_tenstorrent(_ctx: &CheckCtx) -> CheckResult {
    #[cfg(target_os = "linux")]
    {
        let p = std::path::Path::new("/dev/tenstorrent");
        if !p.exists() {
            return CheckResult::Skip("/dev/tenstorrent does not exist".to_string());
        }
        match std::fs::read_dir(p) {
            Ok(iter) => {
                let count = iter.count();
                if count == 0 {
                    return CheckResult::Warn(
                        "/dev/tenstorrent exists but is empty".to_string(),
                        Some("verify the tt-kmd kernel module is loaded".to_string()),
                    );
                }
                CheckResult::Pass(format!("{count} device node(s)"))
            }
            Err(e) => CheckResult::Fail(
                format!("/dev/tenstorrent unreadable: {e}"),
                Some("add your user to the appropriate group and re-login".to_string()),
            ),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        CheckResult::Skip("/dev/tenstorrent is Linux-specific".to_string())
    }
}

#[cfg(target_os = "linux")]
fn read_groups() -> Vec<String> {
    // Prefer `id -Gn` — it's bounded, widely available, and honours
    // nsswitch/sssd so we don't miss group overlays.
    if let Some(out) = try_exec("id", &["-Gn"], Duration::from_millis(500))
        && out.success()
    {
        return out
            .stdout
            .split_whitespace()
            .map(|s| s.to_string())
            .collect();
    }
    Vec::new()
}
