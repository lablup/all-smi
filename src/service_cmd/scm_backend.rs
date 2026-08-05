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

//! Windows Service Control Manager backend for `all-smi service`
//! (issue #311).
//!
//! Everything reachable from here goes through `windows-service`, never
//! `sc.exe`, so failures arrive as Win32 error codes this module can
//! translate rather than as text it would have to scrape.
//!
//! The module is a thin adapter on purpose. Every decision that can be
//! expressed without touching the SCM lives in [`super::scm`], which
//! compiles and is unit tested on every platform; what remains here is
//! handle plumbing that no amount of restructuring would make testable
//! off Windows.
//!
//! # Scope
//!
//! Only [`Scope::System`] exists. The SCM has no per-user services, so
//! `--user` is a hard [`ServiceError::NotSupported`] rather than a
//! degraded mode.
//!
//! # Elevation
//!
//! No pre-flight token probe. Each action opens exactly the handles it
//! needs, and the SCM answers `ERROR_ACCESS_DENIED` when the caller
//! cannot have them; [`super::scm::map_os_error`] turns that into the
//! elevation refusal. That tests the capability actually required
//! instead of a proxy for it, so an Administrator whose access is
//! blocked by service ACLs gets the same actionable message as an
//! unelevated user, and `status` keeps working without elevation
//! because it never asks for a right it does not need.

// Restore the dead-code lint the parent module blanket-allows off
// Linux; see the note there. This file only exists on Windows, so the
// attribute is unconditional.
#![warn(dead_code)]

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use windows_service::service::{
    Service, ServiceAccess, ServiceAction as ScmFailureAction, ServiceActionType,
    ServiceErrorControl, ServiceFailureActions, ServiceFailureResetPeriod, ServiceInfo,
    ServiceStartType, ServiceState, ServiceType,
};
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

use super::scm::{self, RawScmStatus};
use super::{
    InstallSpec, SERVICE_NAME, Scope, ServiceBackend, ServiceError, ServiceStatus,
    current_exe_canonical,
};

/// How long to sleep between `QueryServiceStatus` polls while waiting
/// for a stop to complete.
const STOP_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// `all-smi service` backed by the Windows Service Control Manager.
#[derive(Debug, Clone, Copy, Default)]
pub struct ScmBackend;

impl ScmBackend {
    pub fn new() -> Self {
        Self
    }
}

// ---------------------------------------------------------------------
// Error plumbing
// ---------------------------------------------------------------------

/// Pull the Win32 code out of a `windows-service` error, when it has
/// one.
///
/// Shared with [`super::scm_host`], which needs the same translation for
/// the dispatcher's own failures.
pub(super) fn raw_code(err: &windows_service::Error) -> Option<i32> {
    match err {
        windows_service::Error::Winapi(io) => io.raw_os_error(),
        _ => None,
    }
}

/// Render a `windows-service` error usefully.
///
/// Its `Display` for the winapi variant is the constant string "IO error
/// in winapi call", which tells an operator nothing; the wrapped
/// `io::Error` carries the actual system message.
pub(super) fn describe(err: &windows_service::Error) -> String {
    match err {
        windows_service::Error::Winapi(io) => io.to_string(),
        other => other.to_string(),
    }
}

fn map_err(verb: &str, err: &windows_service::Error) -> ServiceError {
    scm::map_os_error(verb, raw_code(err), &describe(err))
}

fn is_missing_service(err: &windows_service::Error) -> bool {
    raw_code(err) == Some(scm::error_code::SERVICE_DOES_NOT_EXIST)
}

// ---------------------------------------------------------------------
// Handle acquisition
// ---------------------------------------------------------------------

fn open_manager(access: ServiceManagerAccess, verb: &str) -> Result<ServiceManager, ServiceError> {
    ServiceManager::local_computer(None::<&OsStr>, access).map_err(|e| map_err(verb, &e))
}

/// Open the `all-smi` service, distinguishing "not registered" from
/// every other failure.
fn open_service(
    manager: &ServiceManager,
    access: ServiceAccess,
    verb: &str,
) -> Result<Option<Service>, ServiceError> {
    match manager.open_service(SERVICE_NAME, access) {
        Ok(service) => Ok(Some(service)),
        Err(e) if is_missing_service(&e) => Ok(None),
        Err(e) => Err(map_err(verb, &e)),
    }
}

/// The registered command line, or `None` when it cannot be read.
///
/// A caller that holds `CHANGE_CONFIG` but somehow not `QUERY_CONFIG`
/// still gets a usable answer: the idempotency check treats an unknown
/// command line the same as a foreign one and demands `--force`.
fn registered_command_line(service: &Service) -> Option<String> {
    service
        .query_config()
        .ok()
        .map(|cfg| cfg.executable_path.to_string_lossy().into_owned())
}

/// Snapshot the SCM's view of the service.
fn read_status(service: &Service, verb: &str) -> Result<ServiceStatus, ServiceError> {
    let status = service.query_status().map_err(|e| map_err(verb, &e))?;
    let start_type = service.query_config().ok().map(|c| c.start_type.to_raw());
    Ok(scm::map_status(RawScmStatus {
        current_state: status.current_state as u32,
        process_id: status.process_id,
        start_type,
    }))
}

// ---------------------------------------------------------------------
// Install
// ---------------------------------------------------------------------

/// Resolve the path to register, with the `\\?\` prefix
/// `canonicalize` adds unwrapped so `services.msc` shows something an
/// operator recognises.
fn executable_to_register() -> Result<PathBuf, ServiceError> {
    let exe = current_exe_canonical()?;
    match exe.to_str() {
        Some(s) => Ok(PathBuf::from(scm::strip_verbatim_prefix(s))),
        // A non-UTF-8 executable path is pathological on Windows, where
        // paths are UTF-16; register it unchanged rather than failing.
        None => Ok(exe),
    }
}

fn build_service_info(exe: &Path, spec: &InstallSpec) -> ServiceInfo {
    ServiceInfo {
        name: OsString::from(SERVICE_NAME),
        display_name: OsString::from(scm::SERVICE_DISPLAY_NAME),
        service_type: ServiceType::OWN_PROCESS,
        // Plain AutoStart, not delayed: a monitoring exporter that
        // appears two minutes into boot has already missed the window
        // an operator investigating a boot-time thermal event needs.
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: exe.to_path_buf(),
        launch_arguments: scm::LAUNCH_ARGUMENTS.iter().map(OsString::from).collect(),
        dependencies: vec![],
        // `None` means LocalSystem. NVML, the WMI thermal-zone classes
        // under root\cimv2 and root\wmi, the AMD Ryzen Master interface,
        // and LibreHardwareMonitor-style sensor access all need it; a
        // least-privilege evaluation is tracked separately.
        account_name: spec.service_user.clone().map(OsString::from),
        account_password: None,
    }
}

/// Apply the pieces of the configuration that `CreateService` and
/// `ChangeServiceConfig` do not cover.
fn apply_extended_config(service: &Service, verb: &str) -> Result<(), ServiceError> {
    service
        .set_description(scm::SERVICE_DESCRIPTION)
        .map_err(|e| map_err(verb, &e))?;

    // Restart three times, five seconds apart, then give up until the
    // failure counter resets a day later. `taskkill /F` and a panic both
    // land here.
    let delay = Duration::from_secs(scm::RESTART_DELAY_SECS);
    let actions = vec![
        ScmFailureAction {
            action_type: ServiceActionType::Restart,
            delay,
        },
        ScmFailureAction {
            action_type: ServiceActionType::Restart,
            delay,
        },
        ScmFailureAction {
            action_type: ServiceActionType::Restart,
            delay,
        },
    ];
    service
        .update_failure_actions(ServiceFailureActions {
            reset_period: ServiceFailureResetPeriod::After(Duration::from_secs(
                scm::FAILURE_RESET_PERIOD_SECS,
            )),
            reboot_msg: None,
            command: None,
            actions: Some(actions),
        })
        .map_err(|e| map_err(verb, &e))?;

    // Without this, the SCM only runs the failure actions when the
    // process terminates abnormally. The service host reports a
    // non-zero service-specific exit code when it fails to bind a
    // listener, and that has to be treated as a failure too.
    service
        .set_failure_actions_on_non_crash_failures(true)
        .map_err(|e| map_err(verb, &e))?;
    Ok(())
}

/// Create `%PROGRAMDATA%\all-smi` and its `logs` subdirectory.
///
/// Done at install time, while the caller is elevated, so the service
/// account never has to create a directory under `%PROGRAMDATA%` itself.
/// Advisory: a failure here does not block registration, because the
/// service host recreates the directory on demand.
fn prepare_program_data() {
    let dir = super::scm_log::log_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!(
            "warning: could not create the log directory {}: {e}",
            dir.display()
        );
    }
}

// ---------------------------------------------------------------------
// ServiceBackend
// ---------------------------------------------------------------------

impl ServiceBackend for ScmBackend {
    fn install(&self, spec: &InstallSpec) -> Result<(), ServiceError> {
        if spec.scope == Scope::User {
            return Err(scm::user_scope_unsupported());
        }
        if spec.service_user.is_some() {
            eprintln!(
                "warning: --service-user on Windows only works for accounts that log on without \
                 a password (NT AUTHORITY\\LocalService, NT AUTHORITY\\NetworkService, or a group \
                 managed service account). A normal account needs a password, which this \
                 subcommand cannot supply, and registration will fail."
            );
        }

        let exe = executable_to_register()?;
        let info = build_service_info(&exe, spec);
        let manager = open_manager(
            ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE,
            "install",
        )?;

        let access = ServiceAccess::QUERY_CONFIG
            | ServiceAccess::CHANGE_CONFIG
            | ServiceAccess::QUERY_STATUS
            | ServiceAccess::START
            | ServiceAccess::STOP;

        let service = match open_service(&manager, access, "install")? {
            Some(existing) => {
                // Idempotent re-install: reconfigure a service that
                // already points at this binary, refuse one that points
                // elsewhere. This is the Windows analogue of the Linux
                // backend's managed-by marker guard, since the SCM
                // offers nowhere to stamp a marker.
                if !spec.force {
                    let command_line = registered_command_line(&existing).unwrap_or_default();
                    if !scm::command_line_targets(&command_line, &exe) {
                        return Err(scm::binary_path_conflict(&command_line, &exe));
                    }
                }
                existing
                    .change_config(&info)
                    .map_err(|e| map_err("install", &e))?;
                existing
            }
            None => manager
                .create_service(&info, access)
                .map_err(|e| map_err("install", &e))?,
        };

        apply_extended_config(&service, "install")?;
        prepare_program_data();

        if spec.start_now {
            start_service(&service, "install")?;
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
        let service = self.open_for(scope, ServiceAccess::START, "start")?;
        start_service(&service, "start")
    }

    fn stop(&self, scope: Scope) -> Result<(), ServiceError> {
        let service = self.open_for(
            scope,
            ServiceAccess::STOP | ServiceAccess::QUERY_STATUS,
            "stop",
        )?;
        stop_service(&service, "stop")?;
        // `ControlService` only *requests* the stop. Waiting for
        // SERVICE_STOPPED keeps `stop` in line with `systemctl stop`,
        // so a script that stops and then checks status, or stops and
        // then rebinds the port, sees a settled system.
        await_stopped(&service, "stop")
    }

    fn restart(&self, scope: Scope) -> Result<(), ServiceError> {
        let service = self.open_for(
            scope,
            ServiceAccess::START | ServiceAccess::STOP | ServiceAccess::QUERY_STATUS,
            "restart",
        )?;
        // Stop first and wait for SERVICE_STOPPED: the SCM refuses a
        // start while the previous instance is still stopping, and the
        // listener socket is not released until the process exits.
        stop_service(&service, "restart")?;
        await_stopped(&service, "restart")?;
        start_service(&service, "restart")
    }

    fn status(&self, scope: Scope) -> Result<ServiceStatus, ServiceError> {
        if scope == Scope::User {
            return Err(scm::user_scope_unsupported());
        }
        let manager = open_manager(ServiceManagerAccess::CONNECT, "status")?;
        // Query rights are granted to authenticated users by the default
        // service security descriptor, so `status` works unelevated.
        let access = ServiceAccess::QUERY_STATUS | ServiceAccess::QUERY_CONFIG;
        match open_service(&manager, access, "status")? {
            Some(service) => read_status(&service, "status"),
            None => Ok(scm::not_installed_status()),
        }
    }
}

impl ScmBackend {
    /// Open the service for a lifecycle action, mapping "not registered"
    /// onto [`ServiceError::NotInstalled`].
    fn open_for(
        &self,
        scope: Scope,
        access: ServiceAccess,
        verb: &str,
    ) -> Result<Service, ServiceError> {
        if scope == Scope::User {
            return Err(scm::user_scope_unsupported());
        }
        let manager = open_manager(ServiceManagerAccess::CONNECT, verb)?;
        open_service(&manager, access, verb)?.ok_or(ServiceError::NotInstalled)
    }

    fn remove(&self, scope: Scope, force: bool) -> Result<(), ServiceError> {
        if scope == Scope::User {
            return Err(scm::user_scope_unsupported());
        }
        let manager = open_manager(ServiceManagerAccess::CONNECT, "uninstall")?;
        let access = ServiceAccess::QUERY_STATUS
            | ServiceAccess::QUERY_CONFIG
            | ServiceAccess::STOP
            | ServiceAccess::DELETE;
        let service =
            open_service(&manager, access, "uninstall")?.ok_or(ServiceError::NotInstalled)?;

        // Same guard as install: refuse to delete a service of this name
        // that runs some other binary, unless --force was passed.
        if !force {
            let exe = executable_to_register()?;
            let command_line = registered_command_line(&service).unwrap_or_default();
            if !scm::command_line_targets(&command_line, &exe) {
                return Err(scm::binary_path_conflict(&command_line, &exe));
            }
        }

        stop_service(&service, "uninstall")?;
        await_stopped(&service, "uninstall")?;
        service.delete().map_err(|e| map_err("uninstall", &e))?;
        Ok(())
    }
}

// ---------------------------------------------------------------------
// Lifecycle primitives
// ---------------------------------------------------------------------

/// Start the service, treating "already running" as success.
fn start_service(service: &Service, verb: &str) -> Result<(), ServiceError> {
    match service.start(&[] as &[&OsStr]) {
        Ok(()) => Ok(()),
        Err(e) if scm::is_benign_lifecycle_error(raw_code(&e)) => Ok(()),
        Err(e) => Err(map_err(verb, &e)),
    }
}

/// Ask the service to stop, treating "already stopped" as success.
fn stop_service(service: &Service, verb: &str) -> Result<(), ServiceError> {
    match service.stop() {
        Ok(_) => Ok(()),
        Err(e) if scm::is_benign_lifecycle_error(raw_code(&e)) => Ok(()),
        Err(e) => Err(map_err(verb, &e)),
    }
}

/// Poll until the service reports `SERVICE_STOPPED`.
///
/// Deleting or restarting a service that is still draining leaves the
/// listening socket bound and, for delete, defers the removal until the
/// last handle closes. Waiting here is what makes `uninstall` followed
/// immediately by `install` work.
fn await_stopped(service: &Service, verb: &str) -> Result<(), ServiceError> {
    let deadline = Instant::now() + Duration::from_secs(scm::STOP_TIMEOUT_SECS);
    loop {
        let status = service.query_status().map_err(|e| map_err(verb, &e))?;
        if status.current_state == ServiceState::Stopped {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(scm::stop_timeout_error(verb));
        }
        std::thread::sleep(STOP_POLL_INTERVAL);
    }
}

// Test module lives in `scm_backend_tests.rs` to keep this file under
// the 500-line soft limit.
#[cfg(test)]
#[path = "scm_backend_tests.rs"]
mod tests;
