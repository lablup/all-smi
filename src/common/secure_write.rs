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

//! Shared secure file writer (`O_NOFOLLOW` + `0o600`).
//!
//! Consolidates the symlink-hardening + restrictive-mode pattern used by
//! the snapshot writer (`src/snapshot/mod.rs::write_output_atomic`) and
//! the record writer (`src/record/writer.rs::open_secure`). Issue #192
//! adds a third caller — `all-smi config init` — and calls for a shared
//! helper to prevent future divergence.
//!
//! Two entry points:
//!
//! * [`create_new_secure`] — opens a freshly-created file with
//!   `create_new(true)` so the call fails if the path already exists
//!   (avoids clobbering an unrelated file or following an attacker-
//!   planted symlink). The concrete hardening flags (`O_NOFOLLOW` on
//!   Unix, `share_mode(0)` on Windows) match the existing record writer.
//! * [`write_atomic_secure`] — writes content to a sibling `.tmp` file
//!   using the hardening flags above, `sync_all`-s it, then renames it
//!   over the final path. Callers get atomicity *and* the hardening at
//!   the cost of one extra inode briefly existing. Used by `config init`
//!   in `--force` mode to safely replace an existing config file.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Open a brand-new file at `path`, refusing to follow symlinks and
/// refusing to clobber an existing file. Matches the hardening used by
/// `record::writer::open_secure` and `snapshot::write_output_atomic`.
///
/// * Unix: `O_NOFOLLOW | O_CREAT | O_EXCL | O_WRONLY`, mode `0o600`.
/// * Windows: exclusive `share_mode(0)` + `create_new`.
/// * Other targets: plain `create_new`, best effort.
///
/// Returns a writable `File`; the caller owns flush/close discipline.
pub fn create_new_secure(path: &Path) -> io::Result<File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .custom_flags(libc::O_NOFOLLOW)
            .mode(0o600)
            .open(path)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .share_mode(0)
            .open(path)
    }
    #[cfg(not(any(unix, windows)))]
    {
        OpenOptions::new().write(true).create_new(true).open(path)
    }
}

/// Write `contents` to `final_path` atomically with hardened flags.
///
/// Behaviour mirrors `snapshot::write_output_atomic`:
/// 1. Pick a sibling `.tmp` path (with numeric suffix to avoid collisions).
/// 2. Open it with `O_NOFOLLOW` + `0o600` (Unix) or exclusive share mode
///    (Windows).
/// 3. Write the full contents, `sync_all()`.
/// 4. `fs::rename` onto `final_path`.
///
/// Unlike [`create_new_secure`], this entry point accepts an existing
/// `final_path`; the rename replaces it. Used by `config init --force`.
pub fn write_atomic_secure(final_path: &Path, contents: &[u8]) -> io::Result<()> {
    let tmp_path = pick_tmp_path(final_path);

    let file_result = {
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .custom_flags(libc::O_NOFOLLOW)
                .mode(0o600)
                .open(&tmp_path)
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .share_mode(0)
                .open(&tmp_path)
        }
        #[cfg(not(any(unix, windows)))]
        {
            OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&tmp_path)
        }
    };

    let mut file = file_result?;

    if let Err(e) = file.write_all(contents) {
        let _ = fs::remove_file(&tmp_path);
        return Err(e);
    }
    if let Err(e) = file.sync_all() {
        let _ = fs::remove_file(&tmp_path);
        return Err(e);
    }
    drop(file);

    if let Err(e) = fs::rename(&tmp_path, final_path) {
        let _ = fs::remove_file(&tmp_path);
        return Err(e);
    }

    Ok(())
}

/// Pick a temp-file path next to `final_path`. Starts with `<path>.tmp`
/// and appends a numeric suffix when that name is already taken, up to
/// 64 attempts, to reduce collision risk when multiple invocations
/// target the same directory concurrently.
fn pick_tmp_path(final_path: &Path) -> PathBuf {
    let base = {
        let mut p = final_path.as_os_str().to_os_string();
        p.push(".tmp");
        PathBuf::from(p)
    };
    if !base.exists() {
        return base;
    }
    for i in 1..=64 {
        let mut p = final_path.as_os_str().to_os_string();
        p.push(format!(".tmp.{i}"));
        let candidate = PathBuf::from(p);
        if !candidate.exists() {
            return candidate;
        }
    }
    base
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn create_new_secure_creates_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("new.toml");
        let mut f = create_new_secure(&path).expect("create should succeed");
        f.write_all(b"hello\n").unwrap();
        drop(f);
        let mut content = String::new();
        File::open(&path)
            .unwrap()
            .read_to_string(&mut content)
            .unwrap();
        assert_eq!(content, "hello\n");
    }

    #[test]
    fn create_new_secure_refuses_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("already.toml");
        std::fs::write(&path, b"existing").unwrap();
        let result = create_new_secure(&path);
        assert!(
            result.is_err(),
            "create_new_secure must refuse to clobber existing"
        );
    }

    #[cfg(unix)]
    #[test]
    fn create_new_secure_refuses_symlink() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        std::fs::write(&target, "do-not-clobber").unwrap();
        let link = dir.path().join("link");
        symlink(&target, &link).unwrap();
        let result = create_new_secure(&link);
        assert!(result.is_err(), "must refuse to follow symlink");
        let remaining = std::fs::read_to_string(&target).unwrap();
        assert_eq!(remaining, "do-not-clobber");
    }

    #[cfg(unix)]
    #[test]
    fn create_new_secure_sets_0o600_mode() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mode.toml");
        let _ = create_new_secure(&path).unwrap();
        let meta = std::fs::metadata(&path).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn write_atomic_secure_replaces_existing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("replace.toml");
        std::fs::write(&path, b"old").unwrap();
        write_atomic_secure(&path, b"new-content").unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "new-content");
    }

    #[test]
    fn write_atomic_secure_creates_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("new-atomic.toml");
        write_atomic_secure(&path, b"fresh").unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "fresh");
    }

    #[cfg(unix)]
    #[test]
    fn write_atomic_secure_sets_0o600_mode() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("atomic-mode.toml");
        write_atomic_secure(&path, b"content").unwrap();
        let meta = std::fs::metadata(&path).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    }
}
