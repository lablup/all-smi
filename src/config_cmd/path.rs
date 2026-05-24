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

//! Implementation of `all-smi config path` (issue #213).
//!
//! Strictly read-only — never opens a file for write, never creates a
//! directory. Resolves the same path helpers the loader uses
//! ([`paths::default_config_path`] / [`paths::candidate_config_paths`])
//! and prints them in either a scannable text format or a stable JSON
//! schema for shell scripts.
//!
//! The printed absolute path embeds the local username; per the issue
//! body that is acceptable (it is the operator's own machine, and
//! every other path-printing CLI tool — `cargo`, `rustup`, `npm` —
//! does the same). We never print secrets such as `webhook_url`; that
//! redaction lives in [`config_cmd::redact_secrets`] and is enforced
//! by the renderers used by `config print`.

use std::io::{self, Write};
use std::path::Path;

use serde_json::json;

use crate::cli::ConfigPathArgs;
use crate::common::paths;

/// Entry point for `all-smi config path`. Returns a process exit code
/// (0 on success, 1 only on writer failure — we never produce an error
/// for an absent config file, since the whole point of this command is
/// to *tell* the user the file is absent).
///
/// * `explicit` mirrors the global `--config <PATH>` argument. When
///   `Some`, the override path is reported as the active path and the
///   candidate-search list is suppressed (search-order is irrelevant
///   when discovery is bypassed). When `None`, the platform-canonical
///   path is reported and the full ordered candidate list is printed.
/// * `args.json` selects the machine-readable JSON output.
pub fn run_path(explicit: Option<&Path>, args: &ConfigPathArgs) -> i32 {
    let mut stdout = io::stdout().lock();
    let mut stderr = io::stderr().lock();
    let result = if args.json {
        write_json(&mut stdout, explicit)
    } else {
        write_text(&mut stdout, explicit)
    };
    if let Err(e) = result {
        let _ = writeln!(stderr, "error: failed to write config path: {e}");
        return 1;
    }
    0
}

/// Render the human-readable form to `out`. Format mirrors the issue
/// spec verbatim so operators copying from the issue body can paste
/// the output and recognise it.
fn write_text<W: Write>(out: &mut W, explicit: Option<&Path>) -> io::Result<()> {
    if let Some(p) = explicit {
        writeln!(out, "{}", paths::format_path_with_existence(Some(p)))?;
        writeln!(out, "source: --config override (discovery bypassed)")?;
        return Ok(());
    }

    let active = paths::active_config_path();
    writeln!(
        out,
        "{}",
        paths::format_path_with_existence(active.as_deref())
    )?;

    let candidates = paths::candidate_config_paths();
    if candidates.is_empty() {
        // Already covered by the `(no config path…)` message in the
        // first line; nothing further to print.
        return Ok(());
    }
    writeln!(out, "search order:")?;
    for (i, candidate) in candidates.iter().enumerate() {
        writeln!(out, "  {}. {}", i + 1, candidate.display())?;
    }
    Ok(())
}

/// Render the machine-readable JSON form to `out`.
///
/// Schema (stable as of issue #213):
/// ```json
/// {
///   "active": "/Users/you/Library/Application Support/all-smi/config.toml",
///   "exists": false,
///   "overridden": false,
///   "search_order": [
///     "/Users/you/Library/Application Support/all-smi/config.toml",
///     "/Users/you/.config/all-smi/config.toml"
///   ]
/// }
/// ```
///
/// * `active` is the path the loader would actually open (either the
///   `--config` override, the first existing implicit candidate, or the
///   canonical default path where a new config would be created).
///   `null` only when no home directory can be resolved.
/// * `exists` reflects `Path::exists()` for `active`.
/// * `overridden` is `true` when `--config` was supplied.
/// * `search_order` is the ordered list the loader would probe;
///   suppressed (empty) when `overridden` is `true` because discovery
///   never runs in that case.
fn write_json<W: Write>(out: &mut W, explicit: Option<&Path>) -> io::Result<()> {
    let overridden = explicit.is_some();
    let active_path: Option<std::path::PathBuf> = match explicit {
        Some(p) => Some(p.to_path_buf()),
        None => paths::active_config_path(),
    };
    let exists = active_path.as_deref().map(Path::exists).unwrap_or(false);
    let search_order: Vec<String> = if overridden {
        Vec::new()
    } else {
        paths::candidate_config_paths()
            .into_iter()
            .map(|p| p.display().to_string())
            .collect()
    };
    let value = json!({
        "active": active_path.as_deref().map(|p| p.display().to_string()),
        "exists": exists,
        "overridden": overridden,
        "search_order": search_order,
    });
    let rendered = serde_json::to_string_pretty(&value).map_err(io::Error::other)?;
    writeln!(out, "{rendered}")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The text output must always begin with a marker line and never
    /// be empty even when there is no resolvable home directory.
    #[test]
    fn text_output_never_empty_with_default_resolution() {
        let mut buf: Vec<u8> = Vec::new();
        write_text(&mut buf, None).expect("write must succeed");
        let s = String::from_utf8(buf).unwrap();
        assert!(!s.trim().is_empty(), "text output must not be empty");
    }

    /// With `--config <path>` the text output reports the override and
    /// suppresses the candidate-search list (discovery doesn't run).
    #[test]
    fn text_output_with_explicit_reports_override() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, b"# stub").unwrap();
        let mut buf: Vec<u8> = Vec::new();
        write_text(&mut buf, Some(&path)).expect("write must succeed");
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains(&path.display().to_string()));
        assert!(
            s.contains("(active)"),
            "existing override file must be marked (active), got: {s}"
        );
        assert!(
            s.contains("--config override"),
            "override sourcing must be disclosed, got: {s}"
        );
        assert!(
            !s.contains("search order:"),
            "search order must be suppressed when overridden, got: {s}"
        );
    }

    /// Without an override the text output prints the default path and
    /// the ordered candidate list (where one is resolvable).
    #[test]
    fn text_output_prints_search_order_when_not_overridden() {
        let mut buf: Vec<u8> = Vec::new();
        write_text(&mut buf, None).expect("write must succeed");
        let s = String::from_utf8(buf).unwrap();
        if paths::candidate_config_paths().is_empty() {
            // No home dir resolvable on this host; the first line
            // already covers the no-config case, so search order is
            // expected to be omitted.
            assert!(s.contains("no config path"));
        } else {
            assert!(s.contains("search order:"));
        }
    }

    /// The JSON output must always parse and carry the documented
    /// fields. Missing fields would break scripts depending on the
    /// schema.
    #[test]
    fn json_output_has_stable_schema() {
        let mut buf: Vec<u8> = Vec::new();
        write_json(&mut buf, None).expect("write must succeed");
        let s = String::from_utf8(buf).unwrap();
        let value: serde_json::Value = serde_json::from_str(&s).expect("valid JSON");
        assert!(value.get("active").is_some(), "missing `active`");
        assert!(value.get("exists").is_some(), "missing `exists`");
        assert!(value.get("overridden").is_some(), "missing `overridden`");
        assert!(
            value.get("search_order").is_some(),
            "missing `search_order`"
        );
        assert_eq!(value["overridden"], false);
    }

    /// JSON output with an explicit override reflects the override and
    /// reports an empty `search_order` (discovery is bypassed).
    #[test]
    fn json_output_reflects_override() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, b"# stub").unwrap();

        let mut buf: Vec<u8> = Vec::new();
        write_json(&mut buf, Some(&path)).expect("write must succeed");
        let s = String::from_utf8(buf).unwrap();
        let value: serde_json::Value = serde_json::from_str(&s).expect("valid JSON");
        assert_eq!(value["overridden"], true);
        assert_eq!(value["exists"], true);
        assert_eq!(value["active"], path.display().to_string());
        let order = value["search_order"]
            .as_array()
            .expect("search_order must be array");
        assert!(
            order.is_empty(),
            "search_order must be empty when overridden, got: {order:?}"
        );
    }

    /// The runner is read-only: it MUST NOT create the file even when
    /// `Path::exists()` returns false. Regression guard against a
    /// future refactor that accidentally calls
    /// `secure_write::create_new_secure` from this path.
    #[test]
    fn run_path_does_not_create_file() {
        let dir = tempfile::tempdir().unwrap();
        let absent = dir.path().join("never-written.toml");
        assert!(!absent.exists(), "precondition: file must not exist");

        // Direct call to the JSON writer so we don't print to stdout
        // during tests, but the JSON path is the strictest — if any
        // future regression were to add a write, it would happen
        // inside the resolver helpers shared with the text path too.
        let mut buf: Vec<u8> = Vec::new();
        write_json(&mut buf, Some(&absent)).expect("write must succeed");

        // Critical assertion — the runner must NOT have touched disk.
        assert!(
            !absent.exists(),
            "config path runner must NOT create the file"
        );
    }
}
