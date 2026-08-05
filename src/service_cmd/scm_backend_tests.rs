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

//! Windows-only tests for the SCM adapter (issue #311).
//!
//! These compile and run only on Windows. Their main job is to be the
//! bridge the cross-platform tests cannot be: [`super::super::scm`]
//! spells the raw Win32 values out as plain integers so it compiles
//! anywhere, and the assertions below are what prove those literals
//! still agree with the `windows-service` enums the adapter feeds them
//! from. Nothing here touches the live Service Control Manager, so the
//! suite stays runnable on an unelevated Windows CI worker.

use super::*;

#[test]
fn the_raw_state_constants_match_the_windows_service_enum() {
    for (variant, ours, label) in [
        (ServiceState::Stopped, scm::state::STOPPED, "STOPPED"),
        (
            ServiceState::StartPending,
            scm::state::START_PENDING,
            "START_PENDING",
        ),
        (
            ServiceState::StopPending,
            scm::state::STOP_PENDING,
            "STOP_PENDING",
        ),
        (ServiceState::Running, scm::state::RUNNING, "RUNNING"),
        (
            ServiceState::ContinuePending,
            scm::state::CONTINUE_PENDING,
            "CONTINUE_PENDING",
        ),
        (
            ServiceState::PausePending,
            scm::state::PAUSE_PENDING,
            "PAUSE_PENDING",
        ),
        (ServiceState::Paused, scm::state::PAUSED, "PAUSED"),
    ] {
        assert_eq!(
            variant as u32, ours,
            "scm::state::{label} has drifted from the Win32 value"
        );
    }
}

#[test]
fn the_raw_start_type_constants_match_the_windows_service_enum() {
    for (variant, ours, label) in [
        (
            ServiceStartType::AutoStart,
            scm::start_type::AUTO_START,
            "AUTO_START",
        ),
        (
            ServiceStartType::OnDemand,
            scm::start_type::DEMAND_START,
            "DEMAND_START",
        ),
        (
            ServiceStartType::Disabled,
            scm::start_type::DISABLED,
            "DISABLED",
        ),
    ] {
        assert_eq!(
            variant.to_raw(),
            ours,
            "scm::start_type::{label} has drifted from the Win32 value"
        );
    }
}

#[test]
fn the_registered_path_is_not_a_verbatim_path() {
    // `canonicalize` returns `\\?\C:\...` on Windows, which the SCM
    // accepts but every operator-facing tool then echoes back verbatim.
    let exe = executable_to_register().expect("current_exe must resolve");
    let rendered = exe.to_string_lossy().into_owned();
    assert!(
        !rendered.starts_with(r"\\?\C:") && !rendered.starts_with(r"\\?\c:"),
        "registered path still carries the verbatim prefix: {rendered}"
    );
}

#[test]
fn the_service_info_registers_the_hidden_entry_point_at_autostart() {
    let exe = executable_to_register().expect("current_exe must resolve");
    let spec = InstallSpec {
        scope: Scope::System,
        service_user: None,
        start_now: false,
        force: false,
    };
    let info = build_service_info(&exe, &spec);

    assert_eq!(info.name, OsString::from(SERVICE_NAME));
    assert_eq!(info.service_type, ServiceType::OWN_PROCESS);
    assert_eq!(info.start_type, ServiceStartType::AutoStart);
    assert_eq!(
        info.launch_arguments,
        vec![OsString::from("service"), OsString::from("run")]
    );
    assert!(
        info.account_name.is_none(),
        "the default account must be LocalSystem"
    );
    assert!(info.account_password.is_none());
    assert!(info.dependencies.is_empty());
}

#[test]
fn a_service_account_is_passed_through_without_a_password() {
    let exe = executable_to_register().expect("current_exe must resolve");
    let spec = InstallSpec {
        scope: Scope::System,
        service_user: Some("NT AUTHORITY\\LocalService".to_string()),
        start_now: false,
        force: false,
    };
    let info = build_service_info(&exe, &spec);
    assert_eq!(
        info.account_name,
        Some(OsString::from("NT AUTHORITY\\LocalService"))
    );
    assert!(info.account_password.is_none());
}

#[test]
fn user_scope_is_refused_by_every_action() {
    let backend = ScmBackend::new();
    let spec = InstallSpec {
        scope: Scope::User,
        service_user: None,
        start_now: false,
        force: false,
    };
    let refusals: Vec<ServiceError> = vec![
        backend.install(&spec).unwrap_err(),
        backend.uninstall(Scope::User).unwrap_err(),
        backend.uninstall_forced(Scope::User).unwrap_err(),
        backend.start(Scope::User).unwrap_err(),
        backend.stop(Scope::User).unwrap_err(),
        backend.restart(Scope::User).unwrap_err(),
        backend.status(Scope::User).unwrap_err(),
    ];
    for err in refusals {
        assert!(
            matches!(err, ServiceError::NotSupported(_)),
            "--user must be refused as unsupported, got {err:?}"
        );
        assert!(err.to_string().contains("Task Scheduler"));
    }
}

#[test]
fn a_windows_error_keeps_the_system_message_rather_than_the_placeholder() {
    // `windows_service::Error::Winapi`'s own Display is the constant
    // "IO error in winapi call"; the wrapped io::Error is where the
    // operator-legible text lives.
    let err = windows_service::Error::Winapi(std::io::Error::from_raw_os_error(
        scm::error_code::ACCESS_DENIED,
    ));
    assert_eq!(raw_code(&err), Some(scm::error_code::ACCESS_DENIED));
    assert_ne!(describe(&err), "IO error in winapi call");
    assert!(is_missing_service(&windows_service::Error::Winapi(
        std::io::Error::from_raw_os_error(scm::error_code::SERVICE_DOES_NOT_EXIST)
    )));

    let mapped = map_err("install", &err);
    assert!(matches!(mapped, ServiceError::NeedsElevation(_)));
}

#[test]
fn a_non_winapi_error_has_no_raw_code_and_still_maps() {
    let err = windows_service::Error::ArgumentHasNulByte("service name");
    assert_eq!(raw_code(&err), None);
    assert!(matches!(
        map_err("install", &err),
        ServiceError::CommandFailed { .. }
    ));
}
