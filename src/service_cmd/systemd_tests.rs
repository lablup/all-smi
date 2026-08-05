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

//! Unit tests for `service_cmd::systemd`. Kept in a sibling file so the
//! implementation stays under the 500-line soft limit.

use super::*;

// ---- unit path resolution ---------------------------------------

#[test]
fn system_unit_path_is_the_admin_directory() {
    let path = unit_path(Scope::System).expect("system path never needs a home directory");
    assert_eq!(path, PathBuf::from("/etc/systemd/system/all-smi.service"));
}

#[test]
fn user_unit_dir_follows_the_xdg_layout() {
    let base = Path::new("/home/dev/.config");
    assert_eq!(
        user_unit_dir_from(base),
        PathBuf::from("/home/dev/.config/systemd/user")
    );
    // An explicit $XDG_CONFIG_HOME is honoured verbatim.
    assert_eq!(
        user_unit_dir_from(Path::new("/srv/cfg")),
        PathBuf::from("/srv/cfg/systemd/user")
    );
}

// ---- systemctl invocation shape ---------------------------------

#[test]
fn user_scope_adds_the_user_flag() {
    assert_eq!(
        describe(Scope::User, &["enable", "--now", "all-smi"]),
        "systemctl --user enable --now all-smi"
    );
    assert_eq!(
        describe(Scope::System, &["daemon-reload"]),
        "systemctl daemon-reload"
    );
}

// ---- `systemctl show` parsing -----------------------------------

#[test]
fn parses_a_running_enabled_unit() {
    let fixture = "\
LoadState=loaded
ActiveState=active
SubState=running
UnitFileState=enabled
MainPID=48213
";
    let status = parse_show_output(fixture);
    assert!(status.installed);
    assert_eq!(status.enabled, Some(true));
    assert!(status.running);
    assert_eq!(status.pid, Some(48213));
    assert_eq!(status.detail, "active (running)");
}

#[test]
fn parses_a_stopped_but_installed_unit() {
    let fixture = "\
LoadState=loaded
ActiveState=inactive
SubState=dead
UnitFileState=disabled
MainPID=0
";
    let status = parse_show_output(fixture);
    assert!(status.installed);
    assert_eq!(status.enabled, Some(false));
    assert!(!status.running);
    assert_eq!(status.pid, None);
    assert_eq!(status.detail, "inactive (dead)");
}

#[test]
fn parses_an_unknown_unit() {
    // What `systemctl show` prints for a unit that does not exist.
    let fixture = "\
LoadState=not-found
ActiveState=inactive
SubState=dead
UnitFileState=
MainPID=0
";
    let status = parse_show_output(fixture);
    assert!(!status.installed);
    assert_eq!(status.enabled, None);
    assert!(!status.running);
    assert_eq!(status.pid, None);
}

#[test]
fn parses_a_failed_unit_without_claiming_a_pid() {
    // systemd keeps reporting the last MainPID briefly after a
    // crash; a stopped service must never report one.
    let fixture = "\
LoadState=loaded
ActiveState=failed
SubState=failed
UnitFileState=enabled
MainPID=1234
";
    let status = parse_show_output(fixture);
    assert!(status.installed);
    assert!(!status.running);
    assert_eq!(status.pid, None);
    assert_eq!(status.detail, "failed (failed)");
}

#[test]
fn treats_activating_and_reloading_correctly() {
    let activating = parse_show_output(
        "LoadState=loaded\nActiveState=activating\nSubState=start\nUnitFileState=enabled\nMainPID=9\n",
    );
    assert!(
        !activating.running,
        "`systemctl is-active` reports activating as not-yet-active"
    );

    let reloading = parse_show_output(
        "LoadState=loaded\nActiveState=reloading\nSubState=reload\nUnitFileState=enabled\nMainPID=9\n",
    );
    assert!(reloading.running, "reloading is still an active service");
    assert_eq!(reloading.pid, Some(9));
}

#[test]
fn masked_unit_reports_disabled() {
    let status = parse_show_output(
        "LoadState=masked\nActiveState=inactive\nSubState=dead\nUnitFileState=masked\nMainPID=0\n",
    );
    assert!(status.installed);
    assert_eq!(status.enabled, Some(false));
    assert!(!status.running);
}

#[test]
fn static_unit_counts_as_enabled() {
    let status = parse_show_output(
        "LoadState=loaded\nActiveState=active\nSubState=running\nUnitFileState=static\nMainPID=5\n",
    );
    assert_eq!(status.enabled, Some(true));
}

#[test]
fn parser_ignores_unrelated_and_malformed_lines() {
    let fixture = "\
Description=all-smi GPU/NPU metrics exporter (API mode)
this line has no equals sign
LoadState=loaded
ActiveState=active
SubState=running
UnitFileState=enabled
MainPID=not-a-number
";
    let status = parse_show_output(fixture);
    assert!(status.installed);
    assert!(status.running);
    assert_eq!(
        status.pid, None,
        "an unparsable MainPID must degrade to None"
    );
}

#[test]
fn parser_tolerates_empty_input() {
    let status = parse_show_output("");
    assert!(!status.installed);
    assert!(!status.running);
    assert_eq!(status.detail, "not installed");
}

// ---- managed-marker guard ---------------------------------------

#[test]
fn guard_allows_a_missing_unit_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(UNIT_FILE_NAME);
    assert!(guard_managed(&path, false, "overwrite").is_ok());
}

#[test]
fn guard_allows_a_unit_this_tool_wrote() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(UNIT_FILE_NAME);
    let unit = template::render_unit(&RenderParams {
        scope: Scope::System,
        exec_path: Path::new("/usr/local/bin/all-smi"),
        service_user: None,
    })
    .unwrap();
    fs::write(&path, unit).unwrap();
    assert!(guard_managed(&path, false, "overwrite").is_ok());
}

#[test]
fn guard_refuses_a_foreign_unit_without_force() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(UNIT_FILE_NAME);
    fs::write(&path, "[Unit]\nDescription=hand written\n").unwrap();

    let err = guard_managed(&path, false, "overwrite")
        .expect_err("a foreign unit must not be clobbered silently");
    let msg = err.to_string();
    assert!(matches!(err, ServiceError::Conflict(_)));
    assert!(
        msg.contains("--force"),
        "must offer the escape hatch: {msg}"
    );
    assert!(
        msg.contains(template::MANAGED_MARKER),
        "must name the marker it looked for: {msg}"
    );

    // --force lifts the refusal.
    assert!(guard_managed(&path, true, "overwrite").is_ok());
}

// ---- unit file writing ------------------------------------------

#[test]
fn write_unit_creates_the_directory_and_leaves_no_temp_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nested").join(UNIT_FILE_NAME);
    write_unit(&path, "[Unit]\nDescription=x\n").unwrap();

    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "[Unit]\nDescription=x\n"
    );
    let leftovers: Vec<_> = fs::read_dir(path.parent().unwrap())
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".tmp"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "temp files left behind: {leftovers:?}"
    );
}

#[test]
fn write_unit_is_idempotent_and_world_readable() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(UNIT_FILE_NAME);
    write_unit(&path, "first\n").unwrap();
    write_unit(&path, "second\n").unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "second\n");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o644, "systemd needs a world-readable unit file");
    }
}
