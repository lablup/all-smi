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

/// Platforms with no backend yet must fail with a message that names
/// the follow-up issue, so an operator who hits it knows where to look.
#[cfg(not(any(target_os = "linux", target_os = "windows")))]
#[test]
fn non_linux_backend_points_at_the_follow_up_issue() {
    let err = backend().err().expect("no backend on this platform yet");
    let msg = err.to_string();
    assert!(matches!(err, ServiceError::NotSupported(_)));
    #[cfg(target_os = "macos")]
    assert!(
        msg.contains("issues/310"),
        "macOS arm must name issue #310, got: {msg}"
    );
    #[cfg(not(target_os = "macos"))]
    assert!(msg.contains("packaging/systemd/all-smi.service"));
}

#[cfg(target_os = "linux")]
#[test]
fn linux_backend_resolves() {
    assert!(backend().is_ok(), "Linux must always resolve a backend");
}

/// Issue #311 replaced the Windows "not supported yet" arm with the
/// Service Control Manager backend.
#[cfg(target_os = "windows")]
#[test]
fn windows_backend_resolves() {
    assert!(backend().is_ok(), "Windows must resolve the SCM backend");
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
