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

//! Platform-aware configuration path resolution for `all-smi` (issue #192).
//!
//! Handles:
//! - Linux: `$XDG_CONFIG_HOME/all-smi/config.toml`, fallback
//!   `~/.config/all-smi/config.toml`.
//! - macOS: `~/Library/Application Support/all-smi/config.toml` with
//!   `~/.config/all-smi/config.toml` accepted as fallback for parity.
//! - Windows: `%APPDATA%\all-smi\config.toml`.
//!
//! Public surface:
//! - [`default_config_path`] — the primary canonical path for the
//!   current platform, used by `config init` and implicit load.
//! - [`candidate_config_paths`] — ordered list of paths that should be
//!   probed on implicit load; first existing file wins.
//! - [`expand_tilde`] — expands a leading `~/` to the user's home
//!   directory. Used for every config-file string that is a path.
//! - [`config_dir`] — parent directory of the canonical config path
//!   (used by `config init` to `create_dir_all` before writing).

use std::path::{Path, PathBuf};

/// The final filename of the config file, identical across platforms.
pub const CONFIG_FILE_NAME: &str = "config.toml";

/// The app-specific subdirectory under the platform config root.
pub const APP_DIR_NAME: &str = "all-smi";

/// Expand a leading `~` or `~/` in a path-like string to the user's
/// home directory. Returns the input unchanged when no home directory is
/// available (e.g. `$HOME` unset on Linux, no `UserProfile` on Windows).
///
/// This function does **not** attempt `~user/` style expansion — that
/// behaviour is shell-specific and requires `getpwnam` plumbing. Only
/// the leading-`~` case is handled, matching the `dirs` crate and the
/// behaviour every other `all-smi` codepath already assumes.
///
/// Shared by every settings consumer that needs to resolve a
/// potentially-tilde-prefixed path (`energy_wal`, `hostfile`,
/// `record.output_dir`, etc.). Formerly duplicated in
/// `metrics::energy_wal` — consolidated here so there is a single
/// canonical implementation.
pub fn expand_tilde(input: impl AsRef<Path>) -> PathBuf {
    let path = input.as_ref();
    let Some(s) = path.to_str() else {
        return path.to_path_buf();
    };
    if let Some(rest) = s.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
        return path.to_path_buf();
    }
    if s == "~" {
        if let Some(home) = dirs::home_dir() {
            return home;
        }
        return path.to_path_buf();
    }
    path.to_path_buf()
}

/// Resolve the canonical config directory for the current platform.
///
/// * Linux: `$XDG_CONFIG_HOME/all-smi` when set, else
///   `~/.config/all-smi`.
/// * macOS: `~/Library/Application Support/all-smi`. Our loader also
///   accepts `~/.config/all-smi/` as a fallback for parity with Linux,
///   but this function returns only the canonical Apple-recommended
///   location — `config init` writes there.
/// * Windows: `%APPDATA%\all-smi`.
///
/// Returns `None` when no home-like directory can be located. Callers
/// treat that as "no config support" and fall back to compiled defaults
/// plus env vars.
pub fn config_dir() -> Option<PathBuf> {
    // The `dirs::config_dir()` function returns the right primary dir
    // on every supported platform:
    // - Linux: `$XDG_CONFIG_HOME` or `~/.config`
    // - macOS: `~/Library/Application Support`
    // - Windows: `%APPDATA%` (Roaming)
    dirs::config_dir().map(|d| d.join(APP_DIR_NAME))
}

/// The primary canonical config-file path for the current platform.
/// Used by `config init` for the write target and by implicit load as
/// the first candidate.
pub fn default_config_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join(CONFIG_FILE_NAME))
}

/// Ordered list of paths the loader tries when no `--config` flag is
/// supplied. First existing file wins. When none exist the caller
/// proceeds with compiled defaults + env overrides.
///
/// * Linux: only the canonical path (XDG).
/// * macOS: canonical Apple path first, then `~/.config/all-smi/config.toml`
///   as a parity fallback for operators migrating from Linux.
/// * Windows: only the canonical `%APPDATA%` path.
pub fn candidate_config_paths() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(primary) = default_config_path() {
        out.push(primary);
    }
    // macOS parity fallback — issue spec: "fallback
    // `~/.config/all-smi/config.toml` accepted".
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = dirs::home_dir() {
            let xdg_like = home
                .join(".config")
                .join(APP_DIR_NAME)
                .join(CONFIG_FILE_NAME);
            // Avoid duplicates if the user somehow has the Apple path
            // pointing at ~/.config (shouldn't happen but be defensive).
            if !out.iter().any(|p| p == &xdg_like) {
                out.push(xdg_like);
            }
        }
    }
    out
}

/// Pick the first path from [`candidate_config_paths`] that exists on
/// disk. Returns `None` when no candidate file exists — in that case
/// the loader returns compiled defaults.
pub fn discover_existing_config() -> Option<PathBuf> {
    candidate_config_paths().into_iter().find(|p| p.exists())
}

/// Return the parent directory of `path`, creating intermediate
/// directories when needed. Matches `fs::create_dir_all` semantics.
pub fn ensure_parent_dir(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_tilde_noop_without_prefix() {
        let p = expand_tilde(Path::new("/etc/passwd"));
        assert_eq!(p, PathBuf::from("/etc/passwd"));
    }

    #[test]
    fn expand_tilde_replaces_home_marker() {
        // When home is available, `~/foo` should resolve under it.
        if let Some(home) = dirs::home_dir() {
            let p = expand_tilde(Path::new("~/foo/bar"));
            assert_eq!(p, home.join("foo/bar"));
        }
    }

    #[test]
    fn expand_tilde_bare_tilde() {
        if let Some(home) = dirs::home_dir() {
            let p = expand_tilde(Path::new("~"));
            assert_eq!(p, home);
        }
    }

    #[test]
    fn expand_tilde_passthrough_for_relative() {
        let p = expand_tilde(Path::new("relative/path"));
        assert_eq!(p, PathBuf::from("relative/path"));
    }

    #[test]
    fn config_dir_ends_with_app_name() {
        if let Some(dir) = config_dir() {
            assert!(dir.ends_with(APP_DIR_NAME));
        }
    }

    #[test]
    fn default_config_path_ends_with_file_name() {
        if let Some(path) = default_config_path() {
            assert!(path.ends_with(CONFIG_FILE_NAME));
        }
    }

    #[test]
    fn candidate_config_paths_nonempty_when_home_available() {
        if dirs::home_dir().is_some() {
            let paths = candidate_config_paths();
            assert!(!paths.is_empty());
        }
    }
}
