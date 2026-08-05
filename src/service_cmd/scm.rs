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

//! Pure half of the Windows Service Control Manager backend
//! (issue #311).
//!
//! Everything here takes its inputs as arguments and touches neither the
//! SCM nor the filesystem, so all of it is unit tested on macOS and
//! Linux developer machines where `windows-service` cannot even be
//! compiled. [`super::scm_backend`] is the thin adapter that turns real
//! `windows_service` types into these primitives and back.
//!
//! What lives here:
//!
//! * service identity (display name, description, launch arguments),
//! * the raw `SERVICE_STATUS` / `QUERY_SERVICE_CONFIG` value mapping
//!   onto the cross-platform [`ServiceStatus`],
//! * Win32 error-code translation, including the elevation refusal,
//! * command-line parsing for the idempotency check, because the SCM
//!   stores a whole command line where the caller supplied a path.

// The parent module blanket-allows dead code off Linux, which predates
// this backend and would hide an unused item in the whole Windows tree.
// Restore detection here so `clippy -D warnings` against the Windows
// target is worth running. Scoped to Windows because several items below
// are consumed only by the Windows-only adapter modules, and this file
// is also compiled under `cfg(test)` on macOS and Linux purely so its
// pure logic stays covered there.
#![cfg_attr(windows, warn(dead_code))]

use std::path::Path;

use super::{ServiceError, ServiceStatus};

/// Display name shown in `services.msc` and `sc.exe query`.
pub const SERVICE_DISPLAY_NAME: &str = "all-smi GPU/NPU Metrics Exporter";

/// Long description shown in the service properties dialog.
pub const SERVICE_DESCRIPTION: &str = "Exports GPU, NPU, CPU, memory, and chassis metrics in \
     Prometheus format on the configured port. Part of all-smi \
     (https://github.com/lablup/all-smi).";

/// Arguments the SCM passes to the registered binary.
///
/// Deliberately carries no port or interval: runtime configuration lives
/// in `%PROGRAMDATA%\all-smi\config.toml` and the environment, so
/// changing a setting never means re-registering the service.
pub const LAUNCH_ARGUMENTS: &[&str] = &["service", "run"];

/// Seconds the SCM should wait for a failed service before restarting
/// it.
pub const RESTART_DELAY_SECS: u64 = 5;

/// Window over which failures are counted before the counter resets, in
/// seconds. One day, matching the issue's specification.
pub const FAILURE_RESET_PERIOD_SECS: u64 = 86_400;

/// How long the SCM is asked to wait for start and stop transitions.
pub const TRANSITION_WAIT_HINT_SECS: u64 = 10;

/// How long `uninstall` waits for a running service to reach
/// `SERVICE_STOPPED` before giving up and reporting the timeout.
pub const STOP_TIMEOUT_SECS: u64 = 30;

/// Directory beneath the `all-smi` `%PROGRAMDATA%` folder that holds
/// rotated log files.
pub const LOG_DIR_NAME: &str = "logs";

/// Log filename prefix. The rolling appender inserts the date between
/// the prefix and the suffix, so files are named `all-smi.2026-08-05.log`.
pub const LOG_FILE_PREFIX: &str = "all-smi";

/// Log filename suffix.
pub const LOG_FILE_SUFFIX: &str = "log";

/// How many daily log files to retain. Two weeks is enough to
/// investigate a weekend incident on Monday without letting an idle
/// exporter fill a system volume.
pub const LOG_RETENTION_FILES: usize = 14;

// ---------------------------------------------------------------------
// Raw Win32 values
// ---------------------------------------------------------------------
//
// Spelled out rather than imported so this module compiles on any host.
// `scm_backend` asserts they agree with the `windows-service` enums, so
// a divergence is a compile-time-visible test failure on Windows rather
// than a silent misreport.

/// `SERVICE_STATUS.dwCurrentState` values.
pub mod state {
    pub const STOPPED: u32 = 1;
    pub const START_PENDING: u32 = 2;
    pub const STOP_PENDING: u32 = 3;
    pub const RUNNING: u32 = 4;
    pub const CONTINUE_PENDING: u32 = 5;
    pub const PAUSE_PENDING: u32 = 6;
    pub const PAUSED: u32 = 7;
}

/// `QUERY_SERVICE_CONFIG.dwStartType` values.
pub mod start_type {
    pub const BOOT_START: u32 = 0;
    pub const SYSTEM_START: u32 = 1;
    pub const AUTO_START: u32 = 2;
    pub const DEMAND_START: u32 = 3;
    pub const DISABLED: u32 = 4;
}

/// Win32 error codes the SCM paths translate rather than surface raw.
pub mod error_code {
    /// The caller's token is not elevated, or lacks the requested SCM
    /// access right.
    pub const ACCESS_DENIED: i32 = 5;
    /// `StartServiceCtrlDispatcher` refused because the process was not
    /// launched by the SCM. This is what `service run` from a console
    /// hits.
    pub const FAILED_SERVICE_CONTROLLER_CONNECT: i32 = 1063;
    /// The named service is not registered.
    pub const SERVICE_DOES_NOT_EXIST: i32 = 1060;
    /// `start` on an already-running service.
    pub const SERVICE_ALREADY_RUNNING: i32 = 1056;
    /// `stop` on an already-stopped service.
    pub const SERVICE_NOT_ACTIVE: i32 = 1062;
    /// `create_service` for a name that already exists.
    pub const SERVICE_EXISTS: i32 = 1073;
    /// The service is queued for deletion and cannot be reconfigured
    /// until every open handle closes.
    pub const SERVICE_MARKED_FOR_DELETE: i32 = 1072;
}

// ---------------------------------------------------------------------
// Status mapping
// ---------------------------------------------------------------------

/// What the SCM reported, in raw Win32 terms.
///
/// `start_type` is `None` when `QueryServiceConfig` was not consulted or
/// failed, which is a legitimate outcome for a caller holding only
/// `SERVICE_QUERY_STATUS`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RawScmStatus {
    pub current_state: u32,
    pub process_id: Option<u32>,
    pub start_type: Option<u32>,
}

/// Human-readable name for a raw service state.
pub fn describe_state(current_state: u32) -> String {
    match current_state {
        state::STOPPED => "stopped".to_string(),
        state::START_PENDING => "start pending".to_string(),
        state::STOP_PENDING => "stop pending".to_string(),
        state::RUNNING => "running".to_string(),
        state::CONTINUE_PENDING => "continue pending".to_string(),
        state::PAUSE_PENDING => "pause pending".to_string(),
        state::PAUSED => "paused".to_string(),
        other => format!("unknown state ({other})"),
    }
}

/// Whether a start type makes the service come up at boot.
///
/// `None` for a value the SCM did not report or that a future Windows
/// adds, mirroring how the systemd backend reports an unrecognised
/// `UnitFileState`.
pub fn maps_to_enabled(start_type: Option<u32>) -> Option<bool> {
    match start_type {
        Some(start_type::BOOT_START | start_type::SYSTEM_START | start_type::AUTO_START) => {
            Some(true)
        }
        Some(start_type::DEMAND_START | start_type::DISABLED) => Some(false),
        _ => None,
    }
}

/// Map a raw SCM report onto the cross-platform [`ServiceStatus`].
///
/// Only `SERVICE_RUNNING` counts as running. The pending states are
/// deliberately excluded: reporting a start-pending service as running
/// would make `all-smi service status` exit 0 while `/metrics` still
/// refuses connections, which is precisely the lie the exit code exists
/// to prevent.
pub fn map_status(raw: RawScmStatus) -> ServiceStatus {
    let running = raw.current_state == state::RUNNING;
    ServiceStatus {
        installed: true,
        enabled: maps_to_enabled(raw.start_type),
        running,
        // The SCM keeps reporting the last process id for a moment after
        // a service exits, and reports 0 for a service that has none.
        pid: if running {
            raw.process_id.filter(|p| *p != 0)
        } else {
            None
        },
        detail: describe_state(raw.current_state),
    }
}

/// The status of a service that is not registered at all.
pub fn not_installed_status() -> ServiceStatus {
    ServiceStatus {
        installed: false,
        enabled: None,
        running: false,
        pid: None,
        detail: "not installed".to_string(),
    }
}

// ---------------------------------------------------------------------
// Error mapping
// ---------------------------------------------------------------------

/// The refusal an unelevated mutating action gets.
///
/// Operators otherwise see a bare `os error 5`, which says nothing about
/// what to do next.
pub fn elevation_message(verb: &str) -> String {
    format!(
        "service {verb} requires Administrator rights on Windows; re-run it from an elevated \
         terminal (right-click Command Prompt, Windows Terminal, or PowerShell and choose \
         \"Run as administrator\")"
    )
}

/// The refusal `--user` gets on Windows.
///
/// The SCM has no per-user scope at all, so unlike Linux this is not a
/// missing feature that a later release could fill in.
pub fn user_scope_unsupported() -> ServiceError {
    ServiceError::NotSupported(
        "--user is not supported on Windows: the Service Control Manager has no per-user service \
         scope, so there is nothing for all-smi to install into. Drop --user and run the install \
         from an elevated terminal, or register a per-user startup task with Task Scheduler \
         instead, for example: schtasks /create /tn all-smi /tr \"<path>\\all-smi.exe api\" /sc \
         onlogon"
            .to_string(),
    )
}

/// Translate a Win32 error raised by an SCM call.
///
/// `verb` names the subcommand for the operator-facing message and
/// `detail` carries the original error text so nothing is lost when the
/// code is one this function does not special-case.
pub fn map_os_error(verb: &str, code: Option<i32>, detail: &str) -> ServiceError {
    match code {
        Some(error_code::ACCESS_DENIED) => ServiceError::NeedsElevation(elevation_message(verb)),
        Some(error_code::SERVICE_DOES_NOT_EXIST) => ServiceError::NotInstalled,
        Some(error_code::SERVICE_MARKED_FOR_DELETE) => ServiceError::Conflict(format!(
            "the all-smi service is queued for deletion and cannot be {verb}ed until every open \
             handle to it closes. Close services.msc if it is open, then retry."
        )),
        // Only reachable as a race: another process registered the
        // service between this one's existence check and its create.
        Some(error_code::SERVICE_EXISTS) => ServiceError::Conflict(
            "a Windows service named `all-smi` was registered by another process while this \
             install was running. Re-run the install to reconfigure it, or pass --force."
                .to_string(),
        ),
        _ => ServiceError::CommandFailed {
            cmd: format!("Service Control Manager: {verb}"),
            stderr: detail.to_string(),
        },
    }
}

/// Whether a Win32 error means "the thing you asked for already holds".
///
/// `start` on a running service and `stop` on a stopped one both have to
/// succeed, because [`super::ServiceBackend`] documents its lifecycle
/// methods as idempotent.
pub fn is_benign_lifecycle_error(code: Option<i32>) -> bool {
    matches!(
        code,
        Some(error_code::SERVICE_ALREADY_RUNNING) | Some(error_code::SERVICE_NOT_ACTIVE)
    )
}

/// The failure `service run` reports when it was not launched by the
/// SCM.
pub fn console_entry_point_message(code: Option<i32>, detail: &str) -> String {
    if code == Some(error_code::FAILED_SERVICE_CONTROLLER_CONNECT) {
        "`all-smi service run` is the Service Control Manager entry point, not a way to start the \
         exporter by hand. Windows starts it for you once the service is registered. Run \
         `all-smi service install --now` from an elevated terminal to register and start the \
         service, or run `all-smi api` to serve metrics in the foreground."
            .to_string()
    } else {
        format!("failed to connect to the Windows service control dispatcher: {detail}")
    }
}

// ---------------------------------------------------------------------
// Command-line handling
// ---------------------------------------------------------------------

/// Extract the program token from an SCM `lpBinaryPathName`.
///
/// `QueryServiceConfig` reports the whole command line, not the path the
/// caller passed to `CreateService`, so an install written by this tool
/// comes back as `"C:\Program Files\all-smi\all-smi.exe" service run`.
/// The idempotency check needs the executable alone.
///
/// Windows parses `argv[0]` without backslash escaping: a leading quote
/// runs to the next quote, and an unquoted token runs to the first
/// space. Returns `None` for an empty or unterminated-quote command
/// line, which the caller treats as "cannot prove this is ours".
pub fn executable_from_command_line(command_line: &str) -> Option<&str> {
    let trimmed = command_line.trim_start();
    if let Some(rest) = trimmed.strip_prefix('"') {
        let end = rest.find('"')?;
        let exe = &rest[..end];
        if exe.is_empty() { None } else { Some(exe) }
    } else {
        let end = trimmed.find([' ', '\t']).unwrap_or(trimmed.len());
        let exe = &trimmed[..end];
        if exe.is_empty() { None } else { Some(exe) }
    }
}

/// Normalise a Windows path for comparison.
///
/// Windows filesystems are case-insensitive and accept `/` as a
/// separator, and `current_exe()` and the SCM's stored command line do
/// not necessarily agree on either. Comparing the raw strings would make
/// a re-install of the very same binary look like a conflict and demand
/// `--force`.
pub fn normalize_windows_path(path: &str) -> String {
    let replaced = path.replace('/', "\\");
    let trimmed = replaced.trim_end_matches('\\');
    // Paths are compared, never displayed, so ASCII lowercasing is
    // enough: Windows drive letters and the reserved characters are all
    // ASCII, and a Unicode-aware fold would still not match the exact
    // NTFS upcase table.
    if trimmed.is_empty() {
        replaced.to_ascii_lowercase()
    } else {
        trimmed.to_ascii_lowercase()
    }
}

/// Drop the `\\?\` verbatim prefix that `Path::canonicalize` adds on
/// Windows.
///
/// `current_exe().canonicalize()` returns `\\?\C:\Tools\all-smi.exe`.
/// The SCM accepts that, but it then shows up in `services.msc`, in
/// `sc.exe qc`, and in every support bundle, where it reads as a
/// mistake. Only the drive-letter form is unwrapped: a verbatim UNC path
/// (`\\?\UNC\server\share\...`) needs a different rewrite to stay valid,
/// and an executable served from a UNC share is rare enough that leaving
/// it verbatim, and correct, is the better trade.
pub fn strip_verbatim_prefix(path: &str) -> &str {
    let Some(rest) = path.strip_prefix(r"\\?\") else {
        return path;
    };
    let bytes = rest.as_bytes();
    let is_drive_path = bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/');
    if is_drive_path { rest } else { path }
}

/// Whether an existing service's command line launches `exe`.
///
/// `false` when the command line cannot be parsed, which makes the
/// caller demand `--force` rather than silently reconfigure a service it
/// cannot identify.
pub fn command_line_targets(command_line: &str, exe: &Path) -> bool {
    let Some(existing) = executable_from_command_line(command_line) else {
        return false;
    };
    let Some(exe) = exe.to_str() else {
        return false;
    };
    normalize_windows_path(existing) == normalize_windows_path(exe)
}

/// The refusal an install gets when a service of the same name already
/// exists but runs a different binary.
///
/// Reuses [`ServiceError::Conflict`], the same variant the Linux backend
/// raises for a unit file it did not write: both mean "an existing
/// definition is not ours, and overwriting it needs an explicit
/// decision".
pub fn binary_path_conflict(existing_command_line: &str, current_exe: &Path) -> ServiceError {
    let existing = executable_from_command_line(existing_command_line)
        .map(str::to_string)
        .unwrap_or_else(|| existing_command_line.to_string());
    ServiceError::Conflict(format!(
        "a Windows service named `all-smi` already exists and runs {existing}, not {}. Refusing \
         to repoint it. Pass --force to reconfigure it anyway, or remove it first with \
         `all-smi service uninstall --force`.",
        current_exe.display()
    ))
}

/// The failure `uninstall` reports when the service will not stop.
pub fn stop_timeout_error(verb: &str) -> ServiceError {
    ServiceError::CommandFailed {
        cmd: format!("Service Control Manager: {verb}"),
        stderr: format!(
            "the service did not reach SERVICE_STOPPED within {STOP_TIMEOUT_SECS}s; check the log \
             under %PROGRAMDATA%\\all-smi\\{LOG_DIR_NAME} and retry"
        ),
    }
}

// Test module lives in `scm_tests.rs` to keep this file under the
// 500-line soft limit.
#[cfg(test)]
#[path = "scm_tests.rs"]
mod tests;
