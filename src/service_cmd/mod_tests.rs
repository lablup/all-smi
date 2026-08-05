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

//! Unit tests for `service_cmd`'s shared plumbing. Kept in a sibling file so the
//! implementation stays under the 500-line soft limit.

use super::*;

#[test]
fn scope_from_user_flag() {
    assert_eq!(Scope::from_user_flag(true), Scope::User);
    assert_eq!(Scope::from_user_flag(false), Scope::System);
}

#[test]
fn scope_display_is_stable() {
    // The JSON status schema and every diagnostic message embed
    // these strings; renaming them is a breaking change.
    assert_eq!(Scope::System.to_string(), "system");
    assert_eq!(Scope::User.to_string(), "user");
}

#[test]
fn elevation_error_matches_documented_wording() {
    let err = require_elevation("install").unwrap_err();
    let msg = err.to_string();
    // Only assert the wording when the test process is unprivileged;
    // a root CI container legitimately returns Ok.
    assert!(
        msg.contains("requires root"),
        "elevation error must say what is required, got: {msg}"
    );
    assert!(
        msg.contains("--user"),
        "elevation error must offer the user-scope escape hatch, got: {msg}"
    );
    assert!(
        msg.starts_with("service install requires root"),
        "elevation error must name the verb first, got: {msg}"
    );
}

#[test]
fn install_spec_is_comparable() {
    let a = InstallSpec {
        scope: Scope::System,
        service_user: Some("all-smi".to_string()),
        start_now: true,
        force: false,
    };
    let b = a.clone();
    assert_eq!(a, b);
}

#[test]
fn default_status_is_not_installed() {
    let s = ServiceStatus::default();
    assert!(!s.installed);
    assert!(!s.running);
    assert_eq!(s.enabled, None);
    assert_eq!(s.pid, None);
}

/// A platform with no backend must fail with [`ServiceError::NotSupported`]
/// and a message that points at something actionable, so an operator who
/// hits it knows what to do next.
///
/// Linux, macOS, and Windows all dispatch a real backend now (#309,
/// #310, #311), so this no longer covers a "not implemented yet" arm.
/// What is left is the genuine `not(any(...))` fallback in [`backend`]:
/// FreeBSD, illumos, and anything else the crate happens to compile on,
/// where the answer is the canonical systemd unit to adapt by hand. Keep
/// this `cfg` the exact complement of the arms in [`backend`].
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
#[test]
fn unsupported_platform_backend_points_at_the_canonical_unit() {
    let err = backend().err().expect("no backend on this platform");
    let msg = err.to_string();
    assert!(matches!(err, ServiceError::NotSupported(_)));
    assert!(msg.contains("packaging/systemd/all-smi.service"));
}

#[cfg(target_os = "linux")]
#[test]
fn linux_backend_resolves() {
    assert!(backend().is_ok(), "Linux must always resolve a backend");
}

/// Issue #310: the macOS arm dispatches launchd rather than refusing.
#[cfg(target_os = "macos")]
#[test]
fn macos_backend_resolves() {
    assert!(backend().is_ok(), "macOS must resolve the launchd backend");
}

/// Issue #311 replaced the Windows "not supported yet" arm with the
/// Service Control Manager backend.
#[cfg(target_os = "windows")]
#[test]
fn windows_backend_resolves() {
    assert!(backend().is_ok(), "Windows must resolve the SCM backend");
}

/// A user-scope install has to warn about the session boundary, and the
/// escape hatch it names has to be the one that platform actually has.
/// Getting this wrong sends an operator chasing a command that does not
/// exist on their machine.
#[test]
fn user_scope_note_names_a_real_escape_hatch() {
    let note = user_scope_persistence_note();
    #[cfg(target_os = "macos")]
    assert!(
        note.contains("LaunchAgent") && note.contains("sudo all-smi service install"),
        "macOS note must point at the system LaunchDaemon, got: {note}"
    );
    #[cfg(not(target_os = "macos"))]
    assert!(
        note.contains("enable-linger"),
        "the systemd note must point at lingering, got: {note}"
    );
}

/// Issue #311: `service run` is the Service Control Manager entry
/// point. The supervisor decides which scope it launched the process
/// in, so there is nothing for `--user` to select.
#[test]
fn service_run_selects_no_scope() {
    let action = crate::cli::ServiceAction::Run(crate::cli_service::ServiceRunArgs {});
    assert!(!action.user_scope());
}

/// On a platform with no Service Control Manager, `service run` must
/// fail rather than quietly behaving like `all-smi api`. An operator who
/// copies the hidden subcommand from a Windows runbook onto a Linux box
/// needs to be told, not silently given a foreground server on a port
/// nothing is scraping.
#[cfg(not(windows))]
#[test]
fn service_run_is_refused_off_windows() {
    let code = run(&crate::cli::ServiceAction::Run(
        crate::cli_service::ServiceRunArgs {},
    ));
    assert_eq!(code, EXIT_ERROR);
}
