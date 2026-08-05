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

//! Tests for the pure Windows SCM helpers (issue #311).
//!
//! These run on every host, which is the whole point: the SCM itself is
//! unreachable from a macOS or Linux developer machine, so the status
//! mapping, the error translation, and the command-line parsing are only
//! ever exercised here.

use std::path::{Path, PathBuf};

use super::*;
use crate::service_cmd::ServiceError;

// -- status mapping ---------------------------------------------------

#[test]
fn running_state_maps_to_running_with_a_pid() {
    let status = map_status(RawScmStatus {
        current_state: state::RUNNING,
        process_id: Some(4242),
        start_type: Some(start_type::AUTO_START),
    });
    assert!(status.installed);
    assert!(status.running);
    assert_eq!(status.pid, Some(4242));
    assert_eq!(status.enabled, Some(true));
    assert_eq!(status.detail, "running");
}

#[test]
fn only_the_running_state_counts_as_running() {
    for (raw, label) in [
        (state::STOPPED, "stopped"),
        (state::START_PENDING, "start pending"),
        (state::STOP_PENDING, "stop pending"),
        (state::CONTINUE_PENDING, "continue pending"),
        (state::PAUSE_PENDING, "pause pending"),
        (state::PAUSED, "paused"),
    ] {
        let status = map_status(RawScmStatus {
            current_state: raw,
            process_id: Some(7),
            start_type: Some(start_type::AUTO_START),
        });
        assert!(
            !status.running,
            "{label} must not be reported as running (raw {raw})"
        );
        assert_eq!(
            status.pid, None,
            "{label} must not carry a pid: the SCM keeps reporting a stale one"
        );
        assert_eq!(status.detail, label);
    }
}

#[test]
fn a_zero_pid_is_never_surfaced() {
    let status = map_status(RawScmStatus {
        current_state: state::RUNNING,
        process_id: Some(0),
        start_type: Some(start_type::AUTO_START),
    });
    assert!(status.running);
    assert_eq!(status.pid, None);
}

#[test]
fn unknown_states_are_reported_verbatim_rather_than_guessed() {
    let status = map_status(RawScmStatus {
        current_state: 99,
        process_id: None,
        start_type: None,
    });
    assert!(!status.running);
    assert_eq!(status.detail, "unknown state (99)");
    assert_eq!(status.enabled, None);
}

#[test]
fn boot_capable_start_types_map_to_enabled() {
    for raw in [
        start_type::BOOT_START,
        start_type::SYSTEM_START,
        start_type::AUTO_START,
    ] {
        assert_eq!(maps_to_enabled(Some(raw)), Some(true), "raw {raw}");
    }
}

#[test]
fn manual_and_disabled_start_types_map_to_not_enabled() {
    for raw in [start_type::DEMAND_START, start_type::DISABLED] {
        assert_eq!(maps_to_enabled(Some(raw)), Some(false), "raw {raw}");
    }
}

#[test]
fn an_unreported_or_unknown_start_type_is_unknown_not_disabled() {
    assert_eq!(maps_to_enabled(None), None);
    assert_eq!(maps_to_enabled(Some(42)), None);
}

#[test]
fn not_installed_status_is_the_empty_case() {
    let status = not_installed_status();
    assert!(!status.installed);
    assert!(!status.running);
    assert_eq!(status.pid, None);
    assert_eq!(status.enabled, None);
    assert_eq!(status.detail, "not installed");
}

// -- error mapping ----------------------------------------------------

#[test]
fn access_denied_becomes_the_elevation_refusal() {
    let err = map_os_error(
        "install",
        Some(error_code::ACCESS_DENIED),
        "Access is denied.",
    );
    assert!(matches!(err, ServiceError::NeedsElevation(_)));
    let text = err.to_string();
    assert!(text.contains("Administrator"), "got: {text}");
    assert!(text.contains("Run as administrator"), "got: {text}");
    assert!(text.contains("install"), "the verb must appear: {text}");
    assert!(
        !text.contains("os error 5"),
        "the raw error must not leak: {text}"
    );
}

#[test]
fn every_mutating_verb_gets_an_elevation_message_naming_it() {
    for verb in ["install", "uninstall", "start", "stop", "restart"] {
        let err = map_os_error(verb, Some(error_code::ACCESS_DENIED), "Access is denied.");
        assert!(
            err.to_string().contains(verb),
            "{verb} must appear in its own refusal"
        );
    }
}

#[test]
fn a_missing_service_becomes_not_installed() {
    let err = map_os_error(
        "status",
        Some(error_code::SERVICE_DOES_NOT_EXIST),
        "The specified service does not exist as an installed service.",
    );
    assert!(matches!(err, ServiceError::NotInstalled));
}

#[test]
fn a_service_pending_deletion_is_a_conflict_with_a_next_step() {
    let err = map_os_error(
        "install",
        Some(error_code::SERVICE_MARKED_FOR_DELETE),
        "marked for deletion",
    );
    assert!(matches!(err, ServiceError::Conflict(_)));
    assert!(err.to_string().contains("services.msc"));
}

#[test]
fn a_service_created_by_a_racing_process_is_a_conflict() {
    let err = map_os_error(
        "install",
        Some(error_code::SERVICE_EXISTS),
        "The specified service already exists.",
    );
    assert!(matches!(err, ServiceError::Conflict(_)));
    assert!(err.to_string().contains("--force"));
}

#[test]
fn unrecognised_codes_keep_their_original_text() {
    let err = map_os_error("start", Some(1053), "The service did not respond in time.");
    match err {
        ServiceError::CommandFailed { cmd, stderr } => {
            assert!(cmd.contains("start"));
            assert!(stderr.contains("did not respond"));
        }
        other => panic!("expected CommandFailed, got {other:?}"),
    }
}

#[test]
fn a_missing_error_code_still_produces_a_usable_failure() {
    let err = map_os_error("stop", None, "something went wrong");
    assert!(matches!(err, ServiceError::CommandFailed { .. }));
    assert!(err.to_string().contains("something went wrong"));
}

#[test]
fn already_running_and_already_stopped_are_benign() {
    assert!(is_benign_lifecycle_error(Some(
        error_code::SERVICE_ALREADY_RUNNING
    )));
    assert!(is_benign_lifecycle_error(Some(
        error_code::SERVICE_NOT_ACTIVE
    )));
}

#[test]
fn real_failures_are_not_treated_as_benign() {
    assert!(!is_benign_lifecycle_error(Some(error_code::ACCESS_DENIED)));
    assert!(!is_benign_lifecycle_error(Some(
        error_code::SERVICE_DOES_NOT_EXIST
    )));
    assert!(!is_benign_lifecycle_error(None));
}

#[test]
fn user_scope_is_refused_with_a_task_scheduler_pointer() {
    let err = user_scope_unsupported();
    assert!(matches!(err, ServiceError::NotSupported(_)));
    let text = err.to_string();
    assert!(text.contains("Task Scheduler"), "got: {text}");
    assert!(text.contains("schtasks"), "got: {text}");
}

#[test]
fn the_console_entry_point_failure_explains_itself() {
    let msg = console_entry_point_message(
        Some(error_code::FAILED_SERVICE_CONTROLLER_CONNECT),
        "The service process could not connect to the service controller.",
    );
    assert!(msg.contains("Service Control Manager entry point"), "{msg}");
    assert!(msg.contains("service install --now"), "{msg}");
    assert!(
        msg.contains("all-smi api"),
        "operators need the foreground alternative: {msg}"
    );
}

#[test]
fn other_dispatcher_failures_keep_their_detail() {
    let msg = console_entry_point_message(Some(1), "some other problem");
    assert!(msg.contains("some other problem"), "{msg}");
    assert!(!msg.contains("entry point"), "{msg}");
}

#[test]
fn the_stop_timeout_names_the_log_location() {
    let err = stop_timeout_error("uninstall");
    let text = err.to_string();
    assert!(text.contains("SERVICE_STOPPED"), "{text}");
    assert!(text.contains("PROGRAMDATA"), "{text}");
}

// -- command-line handling --------------------------------------------

#[test]
fn an_unquoted_command_line_yields_the_bare_program() {
    assert_eq!(
        executable_from_command_line(r"C:\Tools\all-smi.exe service run"),
        Some(r"C:\Tools\all-smi.exe")
    );
}

#[test]
fn a_quoted_command_line_yields_the_path_inside_the_quotes() {
    assert_eq!(
        executable_from_command_line(r#""C:\Program Files\all-smi\all-smi.exe" service run"#),
        Some(r"C:\Program Files\all-smi\all-smi.exe")
    );
}

#[test]
fn a_command_line_without_arguments_still_parses() {
    assert_eq!(
        executable_from_command_line(r"C:\Tools\all-smi.exe"),
        Some(r"C:\Tools\all-smi.exe")
    );
    assert_eq!(
        executable_from_command_line(r#""C:\Program Files\x\y.exe""#),
        Some(r"C:\Program Files\x\y.exe")
    );
}

#[test]
fn leading_whitespace_is_ignored() {
    assert_eq!(
        executable_from_command_line("   C:\\Tools\\all-smi.exe service run"),
        Some(r"C:\Tools\all-smi.exe")
    );
}

#[test]
fn unparseable_command_lines_yield_nothing() {
    // Empty, whitespace only, an unterminated quote, and an empty
    // quoted program are all "cannot prove this is ours".
    assert_eq!(executable_from_command_line(""), None);
    assert_eq!(executable_from_command_line("   "), None);
    assert_eq!(executable_from_command_line(r#""C:\unterminated"#), None);
    assert_eq!(executable_from_command_line(r#""" service run"#), None);
}

#[test]
fn path_comparison_ignores_case_and_separator_style() {
    assert_eq!(
        normalize_windows_path(r"C:\Program Files\All-SMI\all-smi.EXE"),
        normalize_windows_path("c:/program files/all-smi/all-smi.exe")
    );
}

#[test]
fn path_comparison_ignores_a_trailing_separator() {
    assert_eq!(
        normalize_windows_path(r"C:\ProgramData\all-smi\"),
        normalize_windows_path(r"C:\ProgramData\all-smi")
    );
}

#[test]
fn a_root_path_survives_trailing_separator_trimming() {
    // Trimming must not turn `C:\` into the empty string and make two
    // unrelated roots compare equal.
    assert_eq!(normalize_windows_path(r"C:\"), "c:");
    assert_ne!(
        normalize_windows_path(r"C:\"),
        normalize_windows_path(r"D:\")
    );
    assert_eq!(normalize_windows_path("\\"), "\\");
}

#[test]
fn the_canonicalize_verbatim_prefix_is_unwrapped_for_drive_paths() {
    assert_eq!(
        strip_verbatim_prefix(r"\\?\C:\Program Files\all-smi\all-smi.exe"),
        r"C:\Program Files\all-smi\all-smi.exe"
    );
    assert_eq!(
        strip_verbatim_prefix(r"\\?\d:/tools/all-smi.exe"),
        r"d:/tools/all-smi.exe"
    );
}

#[test]
fn non_drive_paths_keep_their_verbatim_prefix() {
    // Rewriting a verbatim UNC path needs a different transformation, so
    // leaving it alone is the only correct answer here.
    for path in [
        r"\\?\UNC\fileserver\share\all-smi.exe",
        r"C:\Tools\all-smi.exe",
        r"\\?\",
        r"\\?\C:",
        "",
    ] {
        assert_eq!(
            strip_verbatim_prefix(path),
            path,
            "must be unchanged: {path}"
        );
    }
}

#[test]
fn a_service_running_our_binary_is_recognised_as_ours() {
    let exe = PathBuf::from(r"C:\Program Files\all-smi\all-smi.exe");
    assert!(command_line_targets(
        r#""C:\Program Files\all-smi\all-smi.exe" service run"#,
        &exe
    ));
    // Same binary, different casing and separators, as the SCM may well
    // have stored it after a repair install.
    assert!(command_line_targets(
        r#""c:/program files/all-smi/ALL-SMI.EXE" service run"#,
        &exe
    ));
}

#[test]
fn a_service_running_a_different_binary_is_not_ours() {
    let exe = PathBuf::from(r"C:\Program Files\all-smi\all-smi.exe");
    assert!(!command_line_targets(
        r#""C:\Users\dev\target\release\all-smi.exe" service run"#,
        &exe
    ));
    // A prefix must not match: `all-smi.exe` is not `all-smi-mock.exe`.
    assert!(!command_line_targets(
        r"C:\Program Files\all-smi\all-smi-mock.exe service run",
        &exe
    ));
}

#[test]
fn an_unparseable_command_line_never_claims_a_match() {
    let exe = PathBuf::from(r"C:\Tools\all-smi.exe");
    assert!(!command_line_targets("", &exe));
    assert!(!command_line_targets(r#""C:\unterminated"#, &exe));
}

#[test]
fn the_conflict_message_names_both_binaries_and_the_way_out() {
    let err = binary_path_conflict(
        r#""C:\Old\all-smi.exe" service run"#,
        Path::new(r"C:\New\all-smi.exe"),
    );
    assert!(matches!(err, ServiceError::Conflict(_)));
    let text = err.to_string();
    assert!(text.contains(r"C:\Old\all-smi.exe"), "{text}");
    assert!(text.contains(r"C:\New\all-smi.exe"), "{text}");
    assert!(text.contains("--force"), "{text}");
}

#[test]
fn the_conflict_message_falls_back_to_the_raw_command_line() {
    let err = binary_path_conflict("", Path::new(r"C:\New\all-smi.exe"));
    assert!(err.to_string().contains(r"C:\New\all-smi.exe"));
}

// -- service identity -------------------------------------------------

#[test]
fn the_launch_arguments_are_the_hidden_scm_entry_point() {
    assert_eq!(LAUNCH_ARGUMENTS, &["service", "run"]);
}

#[test]
fn the_service_metadata_is_operator_legible() {
    assert!(SERVICE_DISPLAY_NAME.starts_with("all-smi"));
    assert!(SERVICE_DESCRIPTION.contains("github.com/lablup/all-smi"));
}

#[test]
fn the_log_layout_is_bounded() {
    assert_eq!(LOG_DIR_NAME, "logs");
    // An unbounded retention would let an idle exporter fill a system
    // volume, so this is a real constraint even though it is checkable
    // at compile time.
    const _: () = assert!(LOG_RETENTION_FILES > 0);
    assert_eq!(LOG_FILE_PREFIX, "all-smi");
    assert_eq!(LOG_FILE_SUFFIX, "log");
}
