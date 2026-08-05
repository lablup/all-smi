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

//! Unit tests for the launchd backend's on-disk half: the managed
//! marker guard, the atomic plist write, and log-directory creation.
//! Kept in a sibling file so the implementation stays under the
//! 500-line soft limit.

use std::path::Path;

use super::*;

// ── the managed marker guard ──────────────────────────────────────────

#[test]
fn an_absent_plist_never_blocks_an_install() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(PLIST_FILE_NAME);
    assert!(guard_managed(&path, false, "overwrite").is_ok());
}

#[test]
fn a_plist_we_wrote_may_be_overwritten_and_removed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(PLIST_FILE_NAME);
    let rendered = plist::render_plist(&RenderParams {
        scope: Scope::System,
        exec_path: Path::new("/opt/all-smi/bin/all-smi"),
        log_path: Path::new("/var/log/all-smi/all-smi.log"),
        service_user: None,
    })
    .unwrap();
    fs::write(&path, &rendered).unwrap();
    assert!(guard_managed(&path, false, "overwrite").is_ok());
    assert!(guard_managed(&path, false, "remove").is_ok());
}

#[test]
fn a_foreign_plist_is_refused_and_the_message_says_how_to_proceed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(PLIST_FILE_NAME);
    fs::write(&path, "<plist version=\"1.0\"><dict/></plist>\n").unwrap();

    let err = guard_managed(&path, false, "remove").expect_err("a foreign plist must be refused");
    assert!(matches!(err, ServiceError::Conflict(_)));
    let msg = err.to_string();
    assert!(msg.contains("--force"), "must name the override: {msg}");
    assert!(msg.contains(plist::MANAGED_MARKER), "must quote the marker");

    // `--force` is the documented escape hatch and must actually work.
    assert!(guard_managed(&path, true, "remove").is_ok());
}

// ── plist writing ─────────────────────────────────────────────────────

#[test]
fn a_written_plist_is_not_group_or_world_writable() {
    // launchd validates permissions before loading a LaunchDaemon and
    // refuses one anybody but its owner can rewrite, which would
    // otherwise be a local root escalation.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(PLIST_FILE_NAME);
    write_plist(&path, "<plist/>\n").unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "<plist/>\n");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o644, "expected 0644, got {mode:o}");
    }
}

#[test]
fn writing_leaves_no_temporary_behind_and_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(PLIST_FILE_NAME);
    write_plist(&path, "<plist>first</plist>\n").unwrap();
    write_plist(&path, "<plist>second</plist>\n").unwrap();
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "<plist>second</plist>\n"
    );

    let entries: Vec<String> = fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(entries, vec![PLIST_FILE_NAME.to_string()]);
}

#[test]
fn the_log_directory_is_created_readable() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("all-smi").join("all-smi.log");
    create_log_dir(&log).unwrap();
    let parent = log.parent().unwrap();
    assert!(parent.is_dir());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(parent).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755, "expected 0755, got {mode:o}");
    }
    // Idempotent: a reinstall must not fail on an existing directory.
    create_log_dir(&log).unwrap();
}
