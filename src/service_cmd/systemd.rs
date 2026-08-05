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

//! systemd backend for `all-smi service` (issue #309).
//!
//! System scope writes `/etc/systemd/system/all-smi.service` and drives
//! `systemctl`; user scope writes
//! `$XDG_CONFIG_HOME/systemd/user/all-smi.service` (falling back to
//! `~/.config`) and drives `systemctl --user`.
//!
//! The module compiles on non-Linux hosts under `cfg(test)` only, so the
//! pure logic below (path resolution, `systemctl show` parsing, the
//! managed-marker guard) stays covered on macOS and Windows developer
//! machines. It is never dispatched off Linux.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::template::{self, RenderParams};
use super::{
    SERVICE_NAME, Scope, ServiceBackend, ServiceError, ServiceStatus, current_exe_canonical,
    require_elevation,
};
use crate::utils::command::new_command;

/// Unit file name, identical in both scopes.
pub const UNIT_FILE_NAME: &str = "all-smi.service";

/// Where system units installed by an administrator belong.
const SYSTEM_UNIT_DIR: &str = "/etc/systemd/system";

/// systemd creates this directory only when it is PID 1. Its presence is
/// the canonical "am I running under systemd" probe.
const SYSTEMD_RUNTIME_MARKER: &str = "/run/systemd/system";

/// Properties queried for `status`. Order matters only for readability;
/// the parser is key-driven.
const SHOW_PROPERTIES: &str = "LoadState,ActiveState,SubState,UnitFileState,MainPID";

fn no_systemd_error() -> ServiceError {
    ServiceError::NotSupported(
        "systemd is not managing this host (/run/systemd/system is absent), so there is nothing \
         for `all-smi service` to install into. Adapt the canonical unit at \
         packaging/systemd/all-smi.service in the all-smi source tree \
         (https://github.com/lablup/all-smi/blob/main/packaging/systemd/all-smi.service) to \
         OpenRC, runit, sysvinit, or whatever supervises this host."
            .to_string(),
    )
}

/// `all-smi service` backed by systemd.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemdBackend;

impl SystemdBackend {
    pub fn new() -> Self {
        Self
    }
}

/// Whether systemd is supervising this host.
pub fn systemd_available() -> bool {
    Path::new(SYSTEMD_RUNTIME_MARKER).exists()
}

fn require_systemd() -> Result<(), ServiceError> {
    if systemd_available() {
        Ok(())
    } else {
        Err(no_systemd_error())
    }
}

/// Base directory for user-scope configuration, honouring
/// `$XDG_CONFIG_HOME` exactly as systemd does.
fn user_config_base() -> Result<PathBuf, ServiceError> {
    match std::env::var_os("XDG_CONFIG_HOME") {
        Some(v) if !v.is_empty() => Ok(PathBuf::from(v)),
        _ => dirs::home_dir().map(|h| h.join(".config")).ok_or_else(|| {
            ServiceError::NotSupported(
                "cannot locate a home directory for the user-scope unit; set $XDG_CONFIG_HOME or \
                 $HOME, or install the system-scope service with sudo"
                    .to_string(),
            )
        }),
    }
}

/// Pure half of the user unit directory resolution, kept separate so it
/// is testable without mutating the process environment.
fn user_unit_dir_from(config_base: &Path) -> PathBuf {
    config_base.join("systemd").join("user")
}

/// Absolute path of the unit file for `scope`.
pub fn unit_path(scope: Scope) -> Result<PathBuf, ServiceError> {
    match scope {
        Scope::System => Ok(Path::new(SYSTEM_UNIT_DIR).join(UNIT_FILE_NAME)),
        Scope::User => Ok(user_unit_dir_from(&user_config_base()?).join(UNIT_FILE_NAME)),
    }
}

/// Build the `systemctl` invocation for `scope`.
fn systemctl_command(scope: Scope, args: &[&str]) -> Command {
    let mut cmd = new_command("systemctl");
    if scope == Scope::User {
        cmd.arg("--user");
    }
    cmd.args(args);
    cmd
}

fn describe(scope: Scope, args: &[&str]) -> String {
    let scope_flag = if scope == Scope::User { " --user" } else { "" };
    format!("systemctl{scope_flag} {}", args.join(" "))
}

/// Run `systemctl` and return stdout, mapping a non-zero exit onto
/// [`ServiceError::CommandFailed`].
fn systemctl(scope: Scope, args: &[&str]) -> Result<String, ServiceError> {
    let output = systemctl_command(scope, args).output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stderr = if stderr.is_empty() {
            format!("exited with {}", output.status)
        } else {
            stderr
        };
        return Err(ServiceError::CommandFailed {
            cmd: describe(scope, args),
            stderr,
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Run `systemctl` for its side effect, tolerating failure.
///
/// Used only on the teardown path, where a unit that is already stopped,
/// already disabled, or outright broken must not abort the removal.
fn systemctl_best_effort(scope: Scope, args: &[&str]) {
    if let Err(e) = systemctl(scope, args) {
        eprintln!("warning: {e}");
    }
}

/// Read an existing unit file, if any.
fn read_existing_unit(path: &Path) -> Result<Option<String>, ServiceError> {
    match fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(ServiceError::Io(e)),
    }
}

/// Refuse to overwrite or remove a unit file this tool did not write.
fn guard_managed(path: &Path, force: bool, verb: &str) -> Result<(), ServiceError> {
    if force {
        return Ok(());
    }
    let Some(existing) = read_existing_unit(path)? else {
        return Ok(());
    };
    if template::is_managed(&existing) {
        return Ok(());
    }
    Err(ServiceError::Conflict(format!(
        "{} was not written by `all-smi service` (it lacks the `{}` marker); refusing to {verb} \
         it. Pass --force to proceed, or remove the file yourself first.",
        path.display(),
        template::MANAGED_MARKER
    )))
}

/// Write the unit atomically with world-readable permissions.
///
/// systemd needs `0644`: the manager reads unit files as root, but
/// `systemctl show`/`status` for unprivileged users reads them too. The
/// temporary file is dot-prefixed so systemd's directory scan (which
/// matches `*.service`) never sees a half-written unit.
fn write_unit(path: &Path, contents: &str) -> Result<(), ServiceError> {
    let parent = path.parent().ok_or_else(|| {
        ServiceError::Io(std::io::Error::other(format!(
            "unit path {} has no parent directory",
            path.display()
        )))
    })?;
    fs::create_dir_all(parent)?;

    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(UNIT_FILE_NAME);
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

/// Parse `systemctl show -p …` key/value output into a [`ServiceStatus`].
///
/// `systemctl show` exits 0 even for a unit that does not exist, so the
/// absence of the unit is expressed through `LoadState=not-found` rather
/// than an error.
pub fn parse_show_output(raw: &str) -> ServiceStatus {
    let mut load_state = "";
    let mut active_state = "";
    let mut sub_state = "";
    let mut unit_file_state = "";
    let mut main_pid: Option<u32> = None;

    for line in raw.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "LoadState" => load_state = value,
            "ActiveState" => active_state = value,
            "SubState" => sub_state = value,
            "UnitFileState" => unit_file_state = value,
            "MainPID" => main_pid = value.parse::<u32>().ok().filter(|p| *p != 0),
            _ => {}
        }
    }

    // "not-found" is systemd's answer for an unknown unit. An empty
    // LoadState means the property was not reported at all, which older
    // systemd versions do for unknown units.
    let installed = !load_state.is_empty() && load_state != "not-found";

    let enabled = match unit_file_state {
        "enabled" | "enabled-runtime" | "static" | "alias" | "indirect" | "generated"
        | "transient" => Some(true),
        "disabled" | "masked" | "masked-runtime" | "linked" | "linked-runtime" => Some(false),
        // "" (not reported), "bad", and anything a future systemd adds.
        _ => None,
    };

    // Match `systemctl is-active`, which treats only "active" and
    // "reloading" as running.
    let running = matches!(active_state, "active" | "reloading");

    let detail = match (active_state, sub_state) {
        ("", "") if !installed => "not installed".to_string(),
        ("", "") => load_state.to_string(),
        (a, "") => a.to_string(),
        (a, s) => format!("{a} ({s})"),
    };

    ServiceStatus {
        installed,
        enabled,
        running,
        // systemd reports the previous MainPID for a moment after exit;
        // never claim a pid for a service we do not consider running.
        pid: if running { main_pid } else { None },
        detail,
    }
}

impl ServiceBackend for SystemdBackend {
    fn install(&self, spec: &super::InstallSpec) -> Result<(), ServiceError> {
        require_systemd()?;
        if spec.scope == Scope::System {
            require_elevation("install")?;
        }

        let path = unit_path(spec.scope)?;
        guard_managed(&path, spec.force, "overwrite")?;

        let exec = current_exe_canonical()?;
        let unit = template::render_unit(&RenderParams {
            scope: spec.scope,
            exec_path: &exec,
            service_user: spec.service_user.as_deref(),
        })
        .map_err(|e| ServiceError::Conflict(e.to_string()))?;

        write_unit(&path, &unit)?;
        systemctl(spec.scope, &["daemon-reload"])?;

        let mut enable_args = vec!["enable"];
        if spec.start_now {
            enable_args.push("--now");
        }
        enable_args.push(SERVICE_NAME);
        systemctl(spec.scope, &enable_args)?;
        Ok(())
    }

    fn uninstall(&self, scope: Scope) -> Result<(), ServiceError> {
        self.remove(scope, false)
    }

    fn uninstall_forced(&self, scope: Scope) -> Result<(), ServiceError> {
        self.remove(scope, true)
    }

    fn start(&self, scope: Scope) -> Result<(), ServiceError> {
        self.lifecycle(scope, "start")
    }

    fn stop(&self, scope: Scope) -> Result<(), ServiceError> {
        self.lifecycle(scope, "stop")
    }

    fn restart(&self, scope: Scope) -> Result<(), ServiceError> {
        self.lifecycle(scope, "restart")
    }

    fn status(&self, scope: Scope) -> Result<ServiceStatus, ServiceError> {
        require_systemd()?;
        let raw = systemctl(scope, &["show", SERVICE_NAME, "-p", SHOW_PROPERTIES])?;
        Ok(parse_show_output(&raw))
    }
}

impl SystemdBackend {
    fn lifecycle(&self, scope: Scope, verb: &'static str) -> Result<(), ServiceError> {
        require_systemd()?;
        if scope == Scope::System {
            require_elevation(verb)?;
        }
        if !unit_path(scope)?.exists() {
            return Err(ServiceError::NotInstalled);
        }
        systemctl(scope, &[verb, SERVICE_NAME])?;
        Ok(())
    }

    fn remove(&self, scope: Scope, force: bool) -> Result<(), ServiceError> {
        require_systemd()?;
        if scope == Scope::System {
            require_elevation("uninstall")?;
        }

        let path = unit_path(scope)?;
        if !path.exists() {
            return Err(ServiceError::NotInstalled);
        }
        guard_managed(&path, force, "remove")?;

        // A stopped, disabled, or syntactically broken unit must not
        // block removal, so these three are advisory.
        systemctl_best_effort(scope, &["stop", SERVICE_NAME]);
        systemctl_best_effort(scope, &["disable", SERVICE_NAME]);

        fs::remove_file(&path)?;
        systemctl(scope, &["daemon-reload"])?;
        systemctl_best_effort(scope, &["reset-failed", SERVICE_NAME]);
        Ok(())
    }
}

// Test module lives in `systemd_tests.rs` to keep this file under the
// 500-line soft limit.
#[cfg(test)]
#[path = "systemd_tests.rs"]
mod tests;
