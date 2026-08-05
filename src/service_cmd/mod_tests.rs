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
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
#[test]
fn unimplemented_backend_points_at_the_follow_up_issue() {
    let err = backend().err().expect("no backend on this platform yet");
    let msg = err.to_string();
    assert!(matches!(err, ServiceError::NotSupported(_)));
    #[cfg(target_os = "windows")]
    assert!(
        msg.contains("issues/311"),
        "Windows arm must name issue #311, got: {msg}"
    );
    #[cfg(not(target_os = "windows"))]
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
