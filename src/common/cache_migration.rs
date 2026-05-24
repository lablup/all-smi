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

//! Best-effort one-time relocation of legacy `~/.cache/all-smi/...`
//! data to the platform-correct cache dir (issue #229).
//!
//! On Linux without `$XDG_CACHE_HOME`, the old and new paths are
//! identical and this is a no-op. On macOS, Windows, and Linux with
//! `$XDG_CACHE_HOME` set, the cache root relocates and we move any
//! pre-existing `records/` directory or `energy-wal.bin` file so
//! operators don't lose recordings or crash-recovery state across the
//! upgrade. The users-CSV exports are deliberately *not* migrated:
//! they carry per-second timestamps and operators may have accumulated
//! hundreds; bulk-renaming them is more disruptive than helpful.
//!
//! Hard rules:
//!
//! * Never overwrite the destination — if a recordings directory or
//!   WAL already exists at the new root, the old data is left in
//!   place. Operators can rename or merge by hand.
//! * Never follow a symlink at the legacy root or source entry — the writer-side
//!   `O_NOFOLLOW` defences in `record/writer.rs`,
//!   `view/event_handler.rs::open_export_secure`, and
//!   `metrics/energy_wal.rs::open_secure_append` exist because the
//!   legacy `~/.cache/all-smi` path is a well-known location an
//!   attacker on a shared host could pre-plant a symlink at. Honour
//!   the same threat model here: skip with a stderr warning rather
//!   than blindly rename through the link.

use std::path::Path;

use crate::common::paths::{APP_DIR_NAME, cache_dir};

/// Run the migration. Safe to call multiple times — once the legacy
/// directory has been drained, subsequent invocations are no-ops.
///
/// Errors are intentionally swallowed (with a stderr warning) so a
/// transient filesystem issue during startup never blocks the process.
pub fn migrate_legacy_cache_paths() {
    let Some(new_root) = cache_dir() else {
        return;
    };
    let Some(home) = dirs::home_dir() else {
        return;
    };
    let old_root = home.join(".cache").join(APP_DIR_NAME);
    // Common case on Linux without `$XDG_CACHE_HOME`: old == new and
    // there is nothing to move. Exit before touching the filesystem.
    if old_root == new_root {
        return;
    }
    migrate_legacy_cache_root(&old_root, &new_root);
}

fn migrate_legacy_cache_root(old_root: &Path, new_root: &Path) {
    let meta = match old_root.symlink_metadata() {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => {
            eprintln!(
                "all-smi: could not inspect legacy cache {}: {e}",
                old_root.display()
            );
            return;
        }
    };
    if meta.file_type().is_symlink() {
        eprintln!(
            "all-smi: skipping cache migration of {} (is a symlink)",
            old_root.display()
        );
        return;
    }
    if !meta.is_dir() {
        return;
    }
    // Best-effort: if the new root cannot be created we still try the
    // individual renames; either they succeed (the parent already
    // existed) or each `try_move` reports its own error.
    let _ = std::fs::create_dir_all(new_root);
    for name in MIGRATION_TARGETS {
        try_move(&old_root.join(name), &new_root.join(name));
    }
    // Deliberately do NOT remove the (possibly-empty) old_root —
    // operators may have other tools writing there.
}

/// Per-platform entries we attempt to migrate. The user-CSV files
/// (`users-<timestamp>.csv`) are excluded by design — see the module
/// docs.
const MIGRATION_TARGETS: &[&str] = &["records", "energy-wal.bin"];

/// Rename `src` to `dst` with the safety invariants described in the
/// module docs. Public-but-`pub(crate)` so the unit tests in this file
/// can drive it directly against tempdirs without leaning on the
/// process-wide `$HOME` / `cache_dir()` state.
pub(crate) fn try_move(src: &Path, dst: &Path) {
    // Never overwrite — losing data at the destination would be a
    // worse outcome than not migrating at all. Use `symlink_metadata`
    // rather than `exists()` so a dangling destination symlink is still
    // treated as occupied and preserved.
    match dst.symlink_metadata() {
        Ok(_) => return,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            eprintln!(
                "all-smi: could not inspect cache migration destination {}: {e}",
                dst.display()
            );
            return;
        }
    }
    // Refuse to migrate a symlink at the source. An attacker who
    // pre-planted `~/.cache/all-smi/records -> /etc` would otherwise
    // redirect the rename to a location of their choosing.
    let Ok(meta) = src.symlink_metadata() else {
        return;
    };
    if meta.file_type().is_symlink() {
        eprintln!(
            "all-smi: skipping cache migration of {} (is a symlink)",
            src.display()
        );
        return;
    }
    // Make sure the destination parent exists so `rename` cannot fail
    // with `ENOENT` on a fresh install where the new cache root was
    // just created above.
    if let Some(parent) = dst.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match std::fs::rename(src, dst) {
        Ok(()) => eprintln!(
            "all-smi: migrated legacy cache {} -> {}",
            src.display(),
            dst.display()
        ),
        Err(e) => {
            // Most likely cause on Linux/macOS: source and destination
            // are on different filesystems (e.g. `$XDG_CACHE_HOME`
            // pointing at a tmpfs while `~` lives on persistent disk).
            // We don't fall back to copy+delete — recording data can
            // be large and silently doubling it is worse than just
            // leaving the operator to move it by hand.
            eprintln!(
                "all-smi: could not migrate legacy cache {}: {e}",
                src.display()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir() -> tempfile::TempDir {
        tempfile::tempdir().expect("create tempdir")
    }

    #[test]
    fn try_move_skips_when_source_missing() {
        let root = tempdir();
        let src = root.path().join("nope");
        let dst = root.path().join("dst");
        try_move(&src, &dst);
        assert!(!dst.exists(), "missing source must not create destination");
    }

    #[test]
    fn try_move_renames_file_when_dst_missing() {
        let root = tempdir();
        let src = root.path().join("energy-wal.bin");
        let dst_dir = root.path().join("new");
        let dst = dst_dir.join("energy-wal.bin");
        std::fs::write(&src, b"payload").unwrap();
        try_move(&src, &dst);
        assert!(dst.exists(), "destination must be created");
        assert!(!src.exists(), "source must be moved");
        assert_eq!(std::fs::read(&dst).unwrap(), b"payload");
    }

    #[test]
    fn try_move_renames_directory_when_dst_missing() {
        let root = tempdir();
        let src = root.path().join("records");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("a.ndjson.zst"), b"frame").unwrap();
        let dst_dir = root.path().join("new");
        let dst = dst_dir.join("records");
        try_move(&src, &dst);
        assert!(dst.exists(), "destination dir must be created");
        assert!(!src.exists(), "source dir must be moved");
        assert!(dst.join("a.ndjson.zst").exists(), "child must follow");
    }

    #[test]
    fn try_move_no_op_when_dst_exists() {
        let root = tempdir();
        let src = root.path().join("records");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("src.ndjson.zst"), b"src").unwrap();
        let dst = root.path().join("records-new");
        std::fs::create_dir_all(&dst).unwrap();
        std::fs::write(dst.join("dst.ndjson.zst"), b"dst").unwrap();
        try_move(&src, &dst);
        // Both must remain untouched.
        assert!(
            src.join("src.ndjson.zst").exists(),
            "source must be preserved"
        );
        assert!(
            dst.join("dst.ndjson.zst").exists(),
            "destination must be preserved"
        );
        // Destination must not have absorbed the source's contents.
        assert!(!dst.join("src.ndjson.zst").exists());
    }

    /// On platforms without symlink support (mostly: Windows without
    /// developer mode), the symlink probe never fires; the test is
    /// gated to Unix where `symlink` always works.
    #[cfg(unix)]
    #[test]
    fn try_move_refuses_symlink_source() {
        let root = tempdir();
        let real = root.path().join("real.bin");
        std::fs::write(&real, b"important").unwrap();
        let link = root.path().join("energy-wal.bin");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let dst = root.path().join("new").join("energy-wal.bin");
        try_move(&link, &dst);
        assert!(!dst.exists(), "symlink source must not be migrated");
        // The symlink itself must still exist (we did not delete it)
        // and still point at the real file (we did not follow it).
        let resolved = std::fs::read_link(&link).unwrap();
        assert_eq!(resolved, real);
        assert!(real.exists(), "follow-through target untouched");
    }

    #[cfg(unix)]
    #[test]
    fn migration_refuses_symlink_legacy_root() {
        let root = tempdir();
        let target = root.path().join("attacker-controlled");
        std::fs::create_dir_all(target.join("records")).unwrap();
        std::fs::write(target.join("records").join("a.ndjson.zst"), b"frame").unwrap();
        std::fs::write(target.join("energy-wal.bin"), b"wal").unwrap();

        let legacy_root = root.path().join("all-smi-link");
        std::os::unix::fs::symlink(&target, &legacy_root).unwrap();
        let new_root = root.path().join("new-cache");

        migrate_legacy_cache_root(&legacy_root, &new_root);

        assert!(
            !new_root.exists(),
            "migration must not create a destination from a symlinked legacy root"
        );
        assert!(
            target.join("records").join("a.ndjson.zst").exists(),
            "follow-through records target must remain untouched"
        );
        assert!(
            target.join("energy-wal.bin").exists(),
            "follow-through WAL target must remain untouched"
        );
        assert_eq!(std::fs::read_link(&legacy_root).unwrap(), target);
    }

    #[cfg(unix)]
    #[test]
    fn try_move_preserves_dangling_destination_symlink() {
        let root = tempdir();
        let src = root.path().join("energy-wal.bin");
        std::fs::write(&src, b"payload").unwrap();
        let dst = root.path().join("new").join("energy-wal.bin");
        std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
        let missing_target = root.path().join("missing-target");
        std::os::unix::fs::symlink(&missing_target, &dst).unwrap();

        try_move(&src, &dst);

        assert!(src.exists(), "source must remain when destination exists");
        assert_eq!(
            std::fs::read_link(&dst).unwrap(),
            missing_target,
            "dangling destination symlink must not be overwritten"
        );
    }
}
