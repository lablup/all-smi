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

//! Package-manager ownership detection for `all-smi service install`
//! (issue #309).
//!
//! When a package manager already installed `all-smi` it also shipped a
//! service definition, and installing a second one leaves two competing
//! definitions that drift apart on the next package upgrade. This module
//! recognises those installs so the subcommand can refuse and point at
//! the package manager's own command instead.
//!
//! Detection is deliberately conservative: it only fires on an exact
//! match of the packaged binary path, never on a heuristic. A false
//! negative costs nothing (the operator gets the install they asked
//! for); a false positive would block a legitimate install.
//!
//! The pure classifier [`classify`] takes its inputs as arguments so it
//! is testable on any host, including a macOS developer machine with no
//! dpkg database.

use std::path::{Path, PathBuf};

use super::ServiceError;

/// dpkg's file list for the `all-smi` package. Present only when the
/// deb is installed.
pub const DPKG_LIST_PATH: &str = "/var/lib/dpkg/info/all-smi.list";

/// Where the deb installs the binary. The dpkg refusal requires both
/// this exact path and the file list above, so a locally built binary
/// on a host that happens to have the deb installed is not blocked.
pub const DPKG_BINARY_PATH: &str = "/usr/bin/all-smi";

/// Homebrew installation prefixes across the platforms Homebrew
/// supports: Linuxbrew, Apple Silicon macOS, and Intel macOS.
pub const HOMEBREW_PREFIXES: &[&str] = &["/home/linuxbrew", "/opt/homebrew", "/usr/local/Cellar"];

/// Which package manager, if any, owns the running binary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PackageOwner {
    /// No package manager recognised; a subcommand install is safe.
    Unmanaged,
    /// Installed from the Debian package, which ships its own unit.
    Dpkg,
    /// Installed from a Homebrew formula, which ships its own service
    /// block for `brew services`.
    Homebrew,
}

/// Classify an executable path. Pure: every environmental input is an
/// argument.
///
/// * `exe` should already be canonicalized, because Homebrew installs a
///   `bin/` symlink that points into `Cellar/`.
/// * `dpkg_list_present` is whether [`DPKG_LIST_PATH`] exists.
pub fn classify(exe: &Path, dpkg_list_present: bool) -> PackageOwner {
    if dpkg_list_present && exe == Path::new(DPKG_BINARY_PATH) {
        return PackageOwner::Dpkg;
    }
    for prefix in HOMEBREW_PREFIXES {
        // `Path::starts_with` compares whole components, so
        // `/opt/homebrewery/bin/all-smi` does not match `/opt/homebrew`.
        if exe.starts_with(prefix) {
            return PackageOwner::Homebrew;
        }
    }
    PackageOwner::Unmanaged
}

/// Classify the currently running binary.
///
/// Returns [`PackageOwner::Unmanaged`] when the executable path cannot
/// be resolved at all: refusing an install because we could not identify
/// ourselves would be worse than allowing it.
pub fn detect() -> PackageOwner {
    let Ok(exe) = std::env::current_exe() else {
        return PackageOwner::Unmanaged;
    };
    let exe: PathBuf = exe.canonicalize().unwrap_or(exe);
    classify(&exe, Path::new(DPKG_LIST_PATH).exists())
}

/// Refuse an install that would fight a package manager, unless the
/// operator passed `--force`.
pub fn guard(force: bool) -> Result<(), ServiceError> {
    if force {
        return Ok(());
    }
    match detect() {
        PackageOwner::Unmanaged => Ok(()),
        PackageOwner::Dpkg => Err(ServiceError::PackageManaged(
            "The deb package already ships a systemd unit; use 'sudo systemctl enable --now \
             all-smi' instead"
                .to_string(),
        )),
        PackageOwner::Homebrew => Err(ServiceError::PackageManaged(
            "The Homebrew formula already ships a service definition; use 'brew services start \
             all-smi' instead"
                .to_string(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dpkg_requires_both_the_file_list_and_the_packaged_path() {
        let packaged = Path::new(DPKG_BINARY_PATH);
        assert_eq!(classify(packaged, true), PackageOwner::Dpkg);
        // File list absent: the binary merely lives at the same path
        // (a manual `cp` into /usr/bin), so nothing is package-managed.
        assert_eq!(classify(packaged, false), PackageOwner::Unmanaged);
    }

    #[test]
    fn dpkg_does_not_fire_for_a_locally_built_binary() {
        // A developer build on a host that also has the deb installed
        // must still be installable.
        let local = Path::new("/home/dev/all-smi/target/release/all-smi");
        assert_eq!(classify(local, true), PackageOwner::Unmanaged);
    }

    #[test]
    fn homebrew_prefixes_are_recognised() {
        for (path, label) in [
            (
                "/home/linuxbrew/.linuxbrew/Cellar/all-smi/0.25.0/bin/all-smi",
                "linuxbrew",
            ),
            (
                "/opt/homebrew/Cellar/all-smi/0.25.0/bin/all-smi",
                "apple silicon",
            ),
            (
                "/usr/local/Cellar/all-smi/0.25.0/bin/all-smi",
                "intel macOS",
            ),
        ] {
            assert_eq!(
                classify(Path::new(path), false),
                PackageOwner::Homebrew,
                "{label} prefix must be recognised: {path}"
            );
        }
    }

    #[test]
    fn homebrew_match_is_component_wise_not_substring() {
        // A directory whose name merely starts with a Homebrew prefix
        // must not be mistaken for one.
        assert_eq!(
            classify(Path::new("/opt/homebrewery/bin/all-smi"), false),
            PackageOwner::Unmanaged
        );
        assert_eq!(
            classify(Path::new("/usr/local/CellarX/bin/all-smi"), false),
            PackageOwner::Unmanaged
        );
    }

    #[test]
    fn unmanaged_paths_stay_unmanaged() {
        for path in [
            "/usr/local/bin/all-smi",
            "/opt/all-smi/bin/all-smi",
            "/home/dev/.cargo/bin/all-smi",
        ] {
            assert_eq!(
                classify(Path::new(path), true),
                PackageOwner::Unmanaged,
                "{path} must not be treated as package-managed"
            );
        }
    }

    #[test]
    fn force_bypasses_the_guard() {
        // `--force` short-circuits before any filesystem probing, so
        // this holds on every host regardless of what is installed.
        assert!(guard(true).is_ok());
    }

    #[test]
    fn guard_messages_name_the_package_manager_command() {
        // Exercise the message bodies without depending on the host's
        // actual install layout.
        let dpkg = ServiceError::PackageManaged(
            "The deb package already ships a systemd unit; use 'sudo systemctl enable --now \
             all-smi' instead"
                .to_string(),
        );
        assert!(dpkg.to_string().contains("systemctl enable --now all-smi"));
        let brew = ServiceError::PackageManaged(
            "The Homebrew formula already ships a service definition; use 'brew services start \
             all-smi' instead"
                .to_string(),
        );
        assert!(brew.to_string().contains("brew services start all-smi"));
    }
}
