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

//! ROCm version discovery, shared by the `amd.rocm.version` doctor check and
//! the AMD companion plugin.
//!
//! The authoritative source is `getROCmVersion()` in `librocm-core`, the same
//! call `amd-smi version` makes through `amdsmi_get_rocm_version()`.
//! `rocm-smi` and `rocminfo` do not report the ROCm release at all (`rocm-smi
//! --version` prints its own library version), so neither is consulted.
//!
//! Installs without `librocm-core` (ROCm before 5.2, or a musl build that
//! cannot `dlopen`) fall back to version files: the pre-7.x
//! `<root>/.info/version`, then the 7.x `<root>/core-<major>.<minor>/` trees
//! that ship no top-level `.info/version`.

use std::path::{Path, PathBuf};

/// A detected ROCm version and where it came from, for diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RocmVersion {
    pub version: String,
    pub source: String,
}

const ROCM_CORE_SONAME: &str = "librocm-core.so.1";

/// Legacy version files, relative to the install root, in lookup order.
const LEGACY_VERSION_FILES: &[&str] = &[
    ".info/version",
    ".info/version-dev",
    "lib/rocm-release-info/version",
];

/// Default install root, overridable with `ROCM_PATH` like ROCm tooling.
pub fn rocm_root() -> PathBuf {
    std::env::var_os("ROCM_PATH")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/opt/rocm"))
}

/// Detect the installed ROCm version, preferring `librocm-core`.
pub fn detect_rocm_version() -> Option<RocmVersion> {
    let root = rocm_root();
    query_rocm_core(&root).or_else(|| read_version_files(&root))
}

/// Call `getROCmVersion()` from `librocm-core`.
///
/// Tries the copy under the install root first so `ROCM_PATH` wins, then the
/// soname through the system loader path (ROCm registers it with ldconfig).
fn query_rocm_core(root: &Path) -> Option<RocmVersion> {
    let candidates = [
        root.join("lib").join(ROCM_CORE_SONAME),
        root.join("core/lib").join(ROCM_CORE_SONAME),
        PathBuf::from(ROCM_CORE_SONAME),
    ];
    candidates.iter().find_map(|candidate| {
        if candidate.is_absolute() && !candidate.is_file() {
            return None;
        }
        let (major, minor, patch) = call_get_rocm_version(candidate)?;
        Some(RocmVersion {
            version: format!("{major}.{minor}.{patch}"),
            source: format!("{} getROCmVersion", candidate.display()),
        })
    })
}

fn call_get_rocm_version(library: &Path) -> Option<(u32, u32, u32)> {
    use std::ffi::{c_int, c_uint};
    type GetRocmVersionFn = unsafe extern "C" fn(*mut c_uint, *mut c_uint, *mut c_uint) -> c_int;
    const VER_SUCCESS: c_int = 0;

    // SAFETY: librocm-core is a small C library from the ROCm install with no
    // initialization side effects; it stays loaded until `lib` drops below.
    let lib = unsafe { libloading::Library::new(library) }.ok()?;
    // SAFETY: the signature matches `VerErrors getROCmVersion(unsigned int*,
    // unsigned int*, unsigned int*)` from rocm-core's rocm_version.h.
    let get_version = unsafe { lib.get::<GetRocmVersionFn>(b"getROCmVersion\0") }.ok()?;
    let (mut major, mut minor, mut patch): (c_uint, c_uint, c_uint) = (0, 0, 0);
    // SAFETY: all three out-pointers are valid, non-null locals.
    let status = unsafe { get_version(&mut major, &mut minor, &mut patch) };
    (status == VER_SUCCESS && major > 0).then_some((major, minor, patch))
}

/// Read the version from install-tree files, legacy layout first.
fn read_version_files(root: &Path) -> Option<RocmVersion> {
    LEGACY_VERSION_FILES
        .iter()
        .map(|relative| root.join(relative))
        .chain(find_core_version_file(root))
        .find_map(|path| {
            let version = normalize_version(&std::fs::read_to_string(&path).ok()?)?;
            Some(RocmVersion {
                version,
                source: path.display().to_string(),
            })
        })
}

/// Locate the version file of a ROCm 7.x `core-*` layout under `root`.
///
/// Prefers the `core` symlink (the active install); otherwise picks the
/// highest-versioned `core-<major>.<minor>` directory.
fn find_core_version_file(root: &Path) -> Option<PathBuf> {
    let active = root.join("core/.info/version");
    if active.is_file() {
        return Some(active);
    }
    std::fs::read_dir(root)
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let key = core_dir_version(&name)?;
            let file = entry.path().join(".info/version");
            file.is_file().then_some((key, file))
        })
        .max_by(|(a, _), (b, _)| a.cmp(b))
        .map(|(_, file)| file)
}

/// Drop the build suffix after `-`, as `libamdgpu_top::get_rocm_version` does.
fn normalize_version(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let version = trimmed.split_once('-').map_or(trimmed, |(v, _)| v);
    (!version.is_empty()).then(|| version.to_string())
}

/// Parse `core-7.14` (or `core-7`) into a sortable numeric key.
fn core_dir_version(name: &str) -> Option<Vec<u32>> {
    let rest = name.strip_prefix("core-")?;
    rest.split('.')
        .map(|part| part.parse::<u32>().ok())
        .collect::<Option<Vec<_>>>()
        .filter(|parts| !parts.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_version(dir: &Path, version: &str) {
        std::fs::create_dir_all(dir.join(".info")).unwrap();
        std::fs::write(dir.join(".info/version"), version).unwrap();
    }

    fn version_from_files(root: &Path) -> Option<String> {
        read_version_files(root).map(|v| v.version)
    }

    #[test]
    fn legacy_layout_reads_top_level_version() {
        let root = tempfile::tempdir().unwrap();
        write_version(root.path(), "6.2.4-120\n");
        // A stray 7.x tree must not override the legacy file.
        write_version(&root.path().join("core-7.14"), "7.14.0\n");
        assert_eq!(version_from_files(root.path()).as_deref(), Some("6.2.4"));
    }

    #[test]
    fn active_core_symlink_wins() {
        let root = tempfile::tempdir().unwrap();
        write_version(&root.path().join("core-7.14"), "7.14.0\n");
        write_version(&root.path().join("core-7.2"), "7.2.0\n");
        std::os::unix::fs::symlink(root.path().join("core-7.2"), root.path().join("core")).unwrap();
        assert_eq!(version_from_files(root.path()).as_deref(), Some("7.2.0"));
    }

    #[test]
    fn highest_core_dir_is_used_without_symlink() {
        let root = tempfile::tempdir().unwrap();
        write_version(&root.path().join("core-7.2"), "7.2.0");
        write_version(&root.path().join("core-7.14"), "7.14.0-55");
        // Alias directories without a numeric suffix are ignored.
        write_version(&root.path().join("core-latest"), "9.9.9");
        assert_eq!(version_from_files(root.path()).as_deref(), Some("7.14.0"));
    }

    #[test]
    fn empty_install_has_no_version() {
        let root = tempfile::tempdir().unwrap();
        write_version(&root.path().join("core-7.14"), "  \n");
        assert_eq!(version_from_files(root.path()), None);
    }

    #[test]
    fn missing_rocm_core_library_is_none() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(
            call_get_rocm_version(&root.path().join(ROCM_CORE_SONAME)),
            None
        );
    }
}
