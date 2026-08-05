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

//! launchd backend for `all-smi service` (issue #310).
//!
//! System scope writes `/Library/LaunchDaemons/com.lablup.all-smi.plist`
//! and drives the `system` launchd domain; user scope writes
//! `~/Library/LaunchAgents/com.lablup.all-smi.plist` and drives
//! `gui/$UID`.
//!
//! # Where launchd differs from systemd
//!
//! systemd separates "enabled at boot" from "loaded right now"; launchd
//! does not. A plist sitting in `LaunchDaemons` / `LaunchAgents` is
//! bootstrapped automatically at boot or login, and `RunAtLoad` then
//! starts it. So:
//!
//! * `install` without `--now` writes the plist and clears any
//!   `launchctl disable` override, but deliberately does **not**
//!   bootstrap. That is the exact launchd spelling of "enabled at boot,
//!   not running yet".
//! * `install --now` additionally boots the job out and back in, because
//!   launchd caches a loaded job's definition and bootstrapping over it
//!   fails instead of replacing it.
//! * `stop` boots the job out of its domain. The plist stays on disk, so
//!   the service returns at the next boot, matching `systemctl stop`.
//! * `status` reads plist presence from disk for `installed`, because
//!   `launchctl print` only knows about jobs that are currently loaded.
//!
//! This file owns the policy: what each verb means, and the on-disk
//! side of an install. Naming launchd objects, invoking `launchctl`,
//! and parsing its output all live in [`super::launchctl`]; what goes
//! inside a plist lives in [`super::plist`].
//!
//! The module compiles on non-macOS hosts under `cfg(test)` only, so
//! its pure logic (the managed-marker guard, the atomic plist write)
//! stays covered on Linux and Windows developer machines and in CI. It
//! is never dispatched off macOS.

use std::fs;
use std::io::Write;
use std::path::Path;

use super::launchctl::{
    self, Layout, PLIST_FILE_NAME, layout, not_loaded_detail, parse_print_output,
};
use super::plist::{self, RenderParams};
use super::{
    Scope, ServiceBackend, ServiceError, ServiceStatus, current_exe_canonical, require_elevation,
};

/// `all-smi service` backed by launchd.
#[derive(Debug, Clone, Copy, Default)]
pub struct LaunchdBackend;

impl LaunchdBackend {
    pub fn new() -> Self {
        Self
    }
}

// ── filesystem ────────────────────────────────────────────────────────

fn read_existing_plist(path: &Path) -> Result<Option<String>, ServiceError> {
    match fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(ServiceError::Io(e)),
    }
}

/// Refuse to overwrite or remove a plist this tool did not write.
fn guard_managed(path: &Path, force: bool, verb: &str) -> Result<(), ServiceError> {
    if force {
        return Ok(());
    }
    let Some(existing) = read_existing_plist(path)? else {
        return Ok(());
    };
    if plist::is_managed(&existing) {
        return Ok(());
    }
    Err(ServiceError::Conflict(format!(
        "{} was not written by `all-smi service` (it lacks the `{}` marker); refusing to {verb} \
         it. Pass --force to proceed, or remove the file yourself first.",
        path.display(),
        plist::MANAGED_MARKER
    )))
}

/// Create the log file's parent directory.
///
/// `0755` matters for the system daemon: launchd refuses nothing here,
/// but `/var/log/all-smi` has to stay readable by operators who are not
/// root, and must not be writable by anyone else.
fn create_log_dir(log_path: &Path) -> Result<(), ServiceError> {
    let Some(dir) = log_path.parent() else {
        return Ok(());
    };
    fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

/// Write the plist atomically with `0644`.
///
/// launchd validates ownership and permissions before loading a
/// LaunchDaemon and refuses a plist that is writable by group or other,
/// so the mode is not cosmetic. Creating the file as root (which system
/// scope always is) gives it `root:wheel`. The temporary file is
/// dot-prefixed and does not end in `.plist`, so a directory scan never
/// sees a half-written job definition.
fn write_plist(path: &Path, contents: &str) -> Result<(), ServiceError> {
    let parent = path.parent().ok_or_else(|| {
        ServiceError::Io(std::io::Error::other(format!(
            "plist path {} has no parent directory",
            path.display()
        )))
    })?;
    fs::create_dir_all(parent)?;

    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(PLIST_FILE_NAME);
    let tmp = parent.join(format!(".{file_name}.tmp"));

    {
        let mut opts = fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o644);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(contents.as_bytes())?;
        f.sync_all()?;
    }

    // `OpenOptions::mode` only applies to a freshly created file, so a
    // leftover temporary from an interrupted run could carry the wrong
    // mode. Set it explicitly.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o644))?;
    }

    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(ServiceError::Io(e));
    }
    Ok(())
}

// ── backend ───────────────────────────────────────────────────────────

impl ServiceBackend for LaunchdBackend {
    fn install(&self, spec: &super::InstallSpec) -> Result<(), ServiceError> {
        if spec.scope == Scope::System {
            require_elevation("install")?;
        }
        let layout = layout(spec.scope)?;
        guard_managed(&layout.plist, spec.force, "overwrite")?;

        let exec = current_exe_canonical()?;
        let rendered = plist::render_plist(&RenderParams {
            scope: spec.scope,
            exec_path: &exec,
            log_path: &layout.log,
            service_user: spec.service_user.as_deref(),
        })
        .map_err(|e| ServiceError::Conflict(e.to_string()))?;

        create_log_dir(&layout.log)?;
        write_plist(&layout.plist, &rendered)?;

        // Clear a persistent disable override, which survives both
        // uninstall and reboot once anything has run `launchctl
        // disable` (or `brew services stop`) against this label.
        launchctl::run_best_effort(&["enable", &layout.target]);

        if spec.start_now {
            launchctl::run_best_effort(&["bootout", &layout.target]);
            launchctl::bootstrap(&layout)?;
        }
        Ok(())
    }

    fn uninstall(&self, scope: Scope) -> Result<(), ServiceError> {
        self.remove(scope, false)
    }

    fn uninstall_forced(&self, scope: Scope) -> Result<(), ServiceError> {
        self.remove(scope, true)
    }

    fn start(&self, scope: Scope) -> Result<(), ServiceError> {
        let layout = self.prepare(scope, "start")?;
        if launchctl::print_job(&layout.target)?.is_some() {
            launchctl::run(&["kickstart", &layout.target])?;
        } else {
            launchctl::bootstrap(&layout)?;
        }
        Ok(())
    }

    fn stop(&self, scope: Scope) -> Result<(), ServiceError> {
        let layout = self.prepare(scope, "stop")?;
        // Booting out an already-unloaded job is an error in launchctl
        // but a no-op in intent, so ask first and keep `stop`
        // idempotent.
        if launchctl::print_job(&layout.target)?.is_some() {
            launchctl::run(&["bootout", &layout.target])?;
        }
        Ok(())
    }

    fn restart(&self, scope: Scope) -> Result<(), ServiceError> {
        let layout = self.prepare(scope, "restart")?;
        if launchctl::print_job(&layout.target)?.is_some() {
            launchctl::run(&["kickstart", "-k", &layout.target])?;
        } else {
            launchctl::bootstrap(&layout)?;
        }
        Ok(())
    }

    fn status(&self, scope: Scope) -> Result<ServiceStatus, ServiceError> {
        let layout = layout(scope)?;
        let installed = layout.plist.exists();
        let enabled =
            launchctl::query_disabled(&layout.domain).map(|disabled| installed && !disabled);

        let output = launchctl::output(&["print", layout.target.as_str()])?;
        if !output.status.success() {
            return Ok(ServiceStatus {
                installed,
                enabled,
                running: false,
                pid: None,
                detail: not_loaded_detail(installed, &launchctl::failure_text(&output)),
            });
        }

        let info = parse_print_output(&String::from_utf8_lossy(&output.stdout));
        let running = info.running();
        Ok(ServiceStatus {
            installed,
            enabled,
            running,
            // launchd keeps reporting the last pid briefly after exit;
            // never claim one for a job we do not consider running.
            pid: if running { info.pid } else { None },
            detail: if info.state.is_empty() {
                "loaded".to_string()
            } else {
                info.state
            },
        })
    }
}

impl LaunchdBackend {
    /// Shared preamble for the lifecycle verbs: elevate when needed,
    /// resolve the layout, and refuse when nothing is installed.
    fn prepare(&self, scope: Scope, verb: &'static str) -> Result<Layout, ServiceError> {
        if scope == Scope::System {
            require_elevation(verb)?;
        }
        let layout = layout(scope)?;
        if !layout.plist.exists() {
            return Err(ServiceError::NotInstalled);
        }
        Ok(layout)
    }

    fn remove(&self, scope: Scope, force: bool) -> Result<(), ServiceError> {
        if scope == Scope::System {
            require_elevation("uninstall")?;
        }
        let layout = layout(scope)?;
        if !layout.plist.exists() {
            return Err(ServiceError::NotInstalled);
        }
        guard_managed(&layout.plist, force, "remove")?;

        // A job that is already unloaded must not block removal.
        launchctl::run_best_effort(&["bootout", &layout.target]);
        fs::remove_file(&layout.plist)?;
        // Deliberately no `launchctl disable`: a disable override
        // outlives the plist and would silently defeat the next
        // install. The log directory is left in place so the operator
        // can still read why the service was removed.
        Ok(())
    }
}

// Test module lives in `launchd_tests.rs` to keep this file under the
// 500-line soft limit.
#[cfg(test)]
#[path = "launchd_tests.rs"]
mod tests;
