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
//! - Windows: `%APPDATA%\all-smi\config.toml`, plus
//!   `%PROGRAMDATA%\all-smi\config.toml` as the machine-wide fallback
//!   the Service Control Manager backend reads.
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

/// System-wide configuration file on Linux (issue #309).
///
/// A daemon supervised by systemd runs as a dedicated account with no
/// home directory, so the per-user XDG candidate never resolves for it.
/// This path is the machine-wide fallback the packaged service reads.
/// It is a discovery candidate only: `all-smi config init` still writes
/// the per-user file, because writing into `/etc` is an administrator's
/// decision, not a side effect of a helper command.
#[cfg(target_os = "linux")]
pub const LINUX_SYSTEM_CONFIG_PATH: &str = "/etc/all-smi/config.toml";

/// Machine-wide `all-smi` data directory beneath a `%PROGRAMDATA%` root
/// (issue #311).
///
/// Split out as a pure function so the Windows layout stays unit tested
/// on non-Windows developer machines, where `%PROGRAMDATA%` does not
/// exist. The Windows service backend builds its log directory on top of
/// the same root, so this is the single place the layout is decided.
#[cfg(any(windows, test))]
pub fn program_data_app_dir(program_data_root: &Path) -> PathBuf {
    program_data_root.join(APP_DIR_NAME)
}

/// Environment variable naming the machine-wide application data root.
/// Windows environment lookups are case-insensitive, so the canonical
/// mixed-case spelling is fine.
#[cfg(windows)]
pub const PROGRAM_DATA_ENV: &str = "ProgramData";

/// Where `%PROGRAMDATA%` points on a stock Windows install. Used only
/// when the variable is missing from the environment, which should not
/// happen but must not make the service unconfigurable if it does.
#[cfg(windows)]
pub const PROGRAM_DATA_FALLBACK: &str = r"C:\ProgramData";

/// Resolve the machine-wide application data root.
#[cfg(windows)]
pub fn program_data_root() -> PathBuf {
    match std::env::var_os(PROGRAM_DATA_ENV) {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => PathBuf::from(PROGRAM_DATA_FALLBACK),
    }
}

/// System-wide configuration file on Windows (issue #311).
///
/// A service registered by `all-smi service install` runs as
/// LocalSystem, whose `%APPDATA%` resolves to
/// `C:\Windows\System32\config\systemprofile\AppData\Roaming` — a
/// directory no operator will ever open, let alone edit. `%PROGRAMDATA%`
/// is the machine-wide counterpart of `/etc`, so that is where an
/// administrator configures the service. Discovery candidate only:
/// `all-smi config init` still writes the per-user file.
#[cfg(windows)]
pub fn windows_system_config_path() -> PathBuf {
    program_data_app_dir(&program_data_root()).join(CONFIG_FILE_NAME)
}

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

/// Resolve the canonical cache directory for the current platform.
///
/// * Linux: `$XDG_CACHE_HOME/all-smi` when set, else `~/.cache/all-smi`.
/// * macOS: `~/Library/Caches/all-smi`.
/// * Windows: `%LOCALAPPDATA%\all-smi`.
///
/// Returns `None` when no home-like directory can be located. Callers
/// must handle that — typically by falling back to a relative path or
/// reporting an error.
///
/// All `all-smi` cache writers (record output, energy WAL, users CSV
/// export) resolve their base directory through this helper so the
/// layout is consistent across platforms and across consumers
/// (issue #229).
pub fn cache_dir() -> Option<PathBuf> {
    // `dirs::cache_dir()` returns the right primary dir on every
    // supported platform:
    // - Linux: `$XDG_CACHE_HOME` or `~/.cache`
    // - macOS: `~/Library/Caches`
    // - Windows: `%LOCALAPPDATA%`
    dirs::cache_dir().map(|d| d.join(APP_DIR_NAME))
}

/// The primary canonical config-file path for the current platform.
/// Used by `config init` for the write target and by implicit load as
/// the first candidate.
pub fn default_config_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join(CONFIG_FILE_NAME))
}

/// Append `path` unless an identical entry is already queued.
///
/// Candidate order is meaningful (first existing file wins), so a
/// duplicate would be harmless but confusing in `all-smi config path`
/// output. Every branch of [`candidate_config_paths`] goes through this
/// helper so a new platform cannot reintroduce duplicates.
fn push_unique(out: &mut Vec<PathBuf>, path: PathBuf) {
    if !out.iter().any(|p| p == &path) {
        out.push(path);
    }
}

/// Ordered list of paths the loader tries when no `--config` flag is
/// supplied. First existing file wins. When none exist the caller
/// proceeds with compiled defaults + env overrides.
///
/// The list is built in two tiers, each a set of sibling `cfg` branches:
///
/// 1. **Per-user candidates.** The platform-canonical path from
///    [`default_config_path`], plus any per-platform parity fallback.
/// 2. **System-wide candidates** (issue #309). A daemon runs as a
///    dedicated account with no home directory, so it needs a
///    machine-wide file. Linux contributes `/etc/all-smi/config.toml`
///    and Windows contributes `%PROGRAMDATA%\all-smi\config.toml`
///    (issue #311); the macOS counterpart (#310) is a sibling branch in
///    the same tier.
///
/// Current resolution per platform:
///
/// * Linux: the XDG path, then `/etc/all-smi/config.toml`.
/// * macOS: the canonical Apple path, then `~/.config/all-smi/config.toml`
///   as a parity fallback for operators migrating from Linux.
/// * Windows: the `%APPDATA%` path, then
///   `%PROGRAMDATA%\all-smi\config.toml`.
pub fn candidate_config_paths() -> Vec<PathBuf> {
    let mut out = Vec::new();

    // --- Tier 1: per-user candidates ---------------------------------
    if let Some(primary) = default_config_path() {
        push_unique(&mut out, primary);
    }
    // macOS parity fallback — issue spec: "fallback
    // `~/.config/all-smi/config.toml` accepted".
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = dirs::home_dir() {
            push_unique(
                &mut out,
                home.join(".config")
                    .join(APP_DIR_NAME)
                    .join(CONFIG_FILE_NAME),
            );
        }
    }

    // --- Tier 2: system-wide candidates (issue #309) ------------------
    // Always ordered after the per-user tier so an operator's own file
    // still wins over the machine-wide one when both exist.
    #[cfg(target_os = "linux")]
    {
        push_unique(&mut out, PathBuf::from(LINUX_SYSTEM_CONFIG_PATH));
    }
    // Windows (issue #311). Ordered after the `%APPDATA%` candidate
    // pushed by the per-user tier above, so an interactive operator's
    // own file still wins; the service, which has no usable `%APPDATA%`,
    // falls through to this one.
    #[cfg(windows)]
    {
        push_unique(&mut out, windows_system_config_path());
    }

    out
}

/// Pick the first path from [`candidate_config_paths`] that exists on
/// disk. Returns `None` when no candidate file exists — in that case
/// the loader returns compiled defaults.
pub fn discover_existing_config() -> Option<PathBuf> {
    candidate_config_paths().into_iter().find(|p| p.exists())
}

/// Path the implicit loader would treat as active for user-facing
/// discovery: the first existing candidate if one is present, otherwise
/// the platform-canonical default path where a new config would be
/// created.
///
/// This keeps `all-smi --help` and `all-smi config path` aligned with
/// [`crate::common::config_file::load`]. On macOS in particular, the
/// loader accepts `~/.config/all-smi/config.toml` as a fallback; if that
/// file exists while the Apple-canonical path does not, this function
/// reports the fallback as active instead of incorrectly labelling the
/// missing canonical path as the active one.
pub fn active_config_path() -> Option<PathBuf> {
    discover_existing_config().or_else(default_config_path)
}

/// Render a candidate config path together with an `(active)` or
/// `(not found)` existence marker for display in `--help` and the
/// `config path` subcommand. Both surfaces share this so the marker
/// vocabulary stays consistent.
///
/// * `Some(path)` whose target exists → `"<path>   (active)"`
/// * `Some(path)` whose target is missing → `"<path>   (not found)"`
/// * `None` (no resolvable home directory) → a clear inline message
///   instead of an empty string, so operators on bare/CI shells
///   immediately understand why no path was printed.
pub fn format_path_with_existence(path: Option<&Path>) -> String {
    match path {
        Some(p) => {
            let marker = if p.exists() { "active" } else { "not found" };
            format!("{}   ({marker})", p.display())
        }
        None => "(no config path resolvable — set $HOME or $XDG_CONFIG_HOME)".to_string(),
    }
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
    fn cache_dir_ends_with_app_name() {
        if let Some(dir) = cache_dir() {
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

    #[test]
    fn push_unique_drops_repeats_and_preserves_order() {
        let mut out = Vec::new();
        push_unique(&mut out, PathBuf::from("/a"));
        push_unique(&mut out, PathBuf::from("/b"));
        push_unique(&mut out, PathBuf::from("/a"));
        assert_eq!(out, vec![PathBuf::from("/a"), PathBuf::from("/b")]);
    }

    #[test]
    fn candidate_config_paths_has_no_duplicates() {
        let paths = candidate_config_paths();
        let mut seen = paths.clone();
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), paths.len(), "duplicate candidates: {paths:?}");
    }

    /// Issue #309: a systemd-supervised daemon runs as an account with
    /// no home directory, so the machine-wide file must be discoverable.
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_candidates_include_the_system_wide_path() {
        let paths = candidate_config_paths();
        let system = PathBuf::from(LINUX_SYSTEM_CONFIG_PATH);
        assert!(
            paths.contains(&system),
            "/etc/all-smi/config.toml must be a discovery candidate on Linux: {paths:?}"
        );
    }

    /// The operator's own file must win over the machine-wide one when
    /// both exist, so the system path is ordered last.
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_system_candidate_is_ordered_after_the_user_candidate() {
        let paths = candidate_config_paths();
        let system = PathBuf::from(LINUX_SYSTEM_CONFIG_PATH);
        let system_index = paths
            .iter()
            .position(|p| p == &system)
            .expect("system candidate must be present");
        if let Some(user) = default_config_path() {
            let user_index = paths
                .iter()
                .position(|p| p == &user)
                .expect("user candidate must be present");
            assert!(
                user_index < system_index,
                "the per-user candidate must be probed first: {paths:?}"
            );
        }
    }

    /// `config init` must never target `/etc`: writing there is an
    /// administrator's decision, not a side effect of a helper command.
    #[cfg(target_os = "linux")]
    #[test]
    fn default_config_path_is_never_the_system_wide_path() {
        if let Some(p) = default_config_path() {
            assert_ne!(p, PathBuf::from(LINUX_SYSTEM_CONFIG_PATH));
        }
    }

    /// Issue #311: the pure half of the Windows machine-wide layout,
    /// asserted on every host so the layout cannot silently change on a
    /// developer machine that never compiles the Windows branches.
    #[test]
    fn program_data_app_dir_appends_the_app_directory() {
        let dir = program_data_app_dir(Path::new(r"C:\ProgramData"));
        assert!(dir.ends_with(APP_DIR_NAME), "got {}", dir.display());
        assert_eq!(
            dir.join(CONFIG_FILE_NAME).file_name(),
            Some(std::ffi::OsStr::new(CONFIG_FILE_NAME))
        );
    }

    /// Issue #311: a service running as LocalSystem cannot reach a
    /// useful `%APPDATA%`, so the machine-wide file must be discoverable.
    #[cfg(windows)]
    #[test]
    fn windows_candidates_include_the_program_data_path() {
        let paths = candidate_config_paths();
        assert!(
            paths.contains(&windows_system_config_path()),
            "%PROGRAMDATA%\\all-smi\\config.toml must be a discovery candidate: {paths:?}"
        );
    }

    /// The operator's own file must win over the machine-wide one.
    #[cfg(windows)]
    #[test]
    fn windows_program_data_candidate_is_ordered_after_the_user_candidate() {
        let paths = candidate_config_paths();
        let system = windows_system_config_path();
        let system_index = paths
            .iter()
            .position(|p| p == &system)
            .expect("system candidate must be present");
        if let Some(user) = default_config_path() {
            let user_index = paths
                .iter()
                .position(|p| p == &user)
                .expect("user candidate must be present");
            assert!(
                user_index < system_index,
                "the per-user candidate must be probed first: {paths:?}"
            );
        }
    }

    /// `config init` must never target `%PROGRAMDATA%`: writing there is
    /// an administrator's decision, not a helper command's side effect.
    #[cfg(windows)]
    #[test]
    fn windows_default_config_path_is_never_the_program_data_path() {
        if let Some(p) = default_config_path() {
            assert_ne!(p, windows_system_config_path());
        }
    }

    #[test]
    fn active_config_path_matches_loader_resolution() {
        let expected = discover_existing_config().or_else(default_config_path);
        assert_eq!(active_config_path(), expected);
    }

    #[test]
    fn format_path_with_existence_marks_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.toml");
        std::fs::write(&file, b"# stub").unwrap();
        let rendered = format_path_with_existence(Some(&file));
        assert!(
            rendered.contains("(active)"),
            "expected (active) marker, got: {rendered}"
        );
        assert!(rendered.contains(&file.display().to_string()));
    }

    #[test]
    fn format_path_with_existence_marks_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let absent = dir.path().join("nope.toml");
        let rendered = format_path_with_existence(Some(&absent));
        assert!(
            rendered.contains("(not found)"),
            "expected (not found) marker, got: {rendered}"
        );
    }

    #[test]
    fn format_path_with_existence_handles_none() {
        let rendered = format_path_with_existence(None);
        assert!(rendered.contains("no config path"));
        assert!(rendered.contains("HOME"));
    }
}
