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

//! TOML configuration file support for `all-smi` (issue #192).
//!
//! This module exposes:
//! - [`Settings`] — the root runtime configuration struct, built from
//!   compiled defaults merged with TOML file values and environment
//!   variable overrides.
//! - [`load`] — the primary loader. Accepts an explicit `Option<Path>`
//!   from `--config`, falls back to platform-appropriate paths, and
//!   returns a fully merged [`Settings`] plus any unknown-keys report.
//! - [`LoadOutcome`] — what `load` returns: the settings, the resolved
//!   path (when one was actually read), and any unknown keys for
//!   `config print` to warn about.
//! - [`SUPPORTED_SCHEMA_VERSION`] — version gate; mismatching `schema_version`
//!   produces a hard error.
//!
//! Precedence rule (highest → lowest): CLI flag > env var > config file
//! > compiled default. CLI merging lives in the caller — this module
//! > handles the file + env layer only.
//!
//! Module layout:
//! - [`crate::common::config_schema`] defines the on-disk TOML types
//!   (pure serde data projections).
//! - [`crate::common::config_env`] applies env-var overrides to a
//!   [`Settings`] (canonical + backward-compat legacy aliases).

use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};

use toml::Value as TomlValue;

use crate::common::config::{AlertConfig, EnergyConfig};
use crate::common::config_apply;
use crate::common::config_env;
use crate::common::config_schema::{KNOWN_TOP_LEVEL, RawConfig};
use crate::common::paths;

/// Hard ceiling on the size of a config file we are willing to read.
///
/// A legitimate `config.toml` is hundreds of bytes; even with every
/// commented option expanded it stays well under a kilobyte. Capping
/// the read at 1 MiB prevents a malicious or accidental oversized file
/// (e.g. a log file renamed over the config path, a `/dev/zero`
/// symlink, etc.) from OOM-ing the process at startup. The cap is
/// enforced both via metadata (fast path) and via `Read::take`
/// (authoritative — covers named pipes, devices, and any path where
/// the metadata length is a lie).
pub(crate) const MAX_CONFIG_BYTES: u64 = 1 << 20; // 1 MiB

// Re-export the on-disk schema types so existing callers can keep
// importing through this module.
pub use crate::common::config_schema::SocketSetting;

/// Schema version the current binary understands. Future versions may
/// bump this and migrate fields; for v1 we reject mismatches outright.
pub const SUPPORTED_SCHEMA_VERSION: u32 = 1;

// ---------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------

/// Errors produced by [`load`] and [`parse_toml`]. Distinguished enough
/// for callers (CLI, `config validate`) to emit the right exit codes.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// I/O error while reading the file.
    #[error("config file I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// The TOML parser rejected the file. Includes the underlying
    /// message which already names line/column.
    #[error("config file parse error: {0}")]
    Parse(String),
    /// `schema_version` in the file is newer than
    /// [`SUPPORTED_SCHEMA_VERSION`], or otherwise out of range.
    #[error(
        "config file schema_version = {found} is not supported \
         (this build understands schema_version = {supported})"
    )]
    SchemaVersion { found: u32, supported: u32 },
    /// A semantic validation failure (e.g. `theme = "rainbow"`).
    #[error("config file semantic error: {0}")]
    Semantic(String),
    /// With `--strict`, any unknown key at the top-level or inside a
    /// known section is reported here. The key is pre-escaped via
    /// [`escape_printable`] so a quoted TOML key containing ANSI
    /// escapes cannot inject cursor-control sequences into operator
    /// terminals.
    #[error("config file unknown key: {0}")]
    UnknownKey(String),
}

/// Escape control characters in a string so it is safe to print to a
/// terminal.
///
/// TOML permits arbitrary codepoints inside quoted keys
/// (e.g. `"foo\u001b[31mRED"`). Blindly passing such keys to
/// `eprintln!` would allow a hostile config file to inject cursor
/// movement, colour, or clear-screen sequences into the operator's
/// terminal — a meaningful attack surface for multi-tenant hosts where
/// one user controls the config file and another reads the validator
/// output.
///
/// This helper replaces every control character (`U+0000` – `U+001F`
/// and `U+007F`) with its `\u{XXXX}` escape so output stays printable
/// and auditable. Non-control characters pass through unchanged.
pub fn escape_printable(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        let code = c as u32;
        if code < 0x20 || code == 0x7f {
            out.push_str(&format!("\\u{{{code:04X}}}"));
        } else {
            out.push(c);
        }
    }
    out
}

// ---------------------------------------------------------------------
// Runtime Settings — merged view
// ---------------------------------------------------------------------

/// Merged configuration handed to every subcommand entry point. Each
/// field is populated via the precedence chain (defaults → file → env)
/// inside [`load`]; the CLI layer overrides [`Settings`] further.
#[derive(Debug, Clone)]
pub struct Settings {
    pub general: GeneralSettings,
    pub local: LocalSettings,
    pub view: ViewSettings,
    pub api: ApiSettings,
    pub alerts: AlertConfig,
    pub energy: EnergyConfig,
    pub display: DisplaySettings,
    pub record: RecordSettings,
    pub snapshot: SnapshotSettings,
    /// Path the settings were actually loaded from. `None` means we
    /// used compiled defaults + env overrides only.
    pub source_path: Option<PathBuf>,
    /// Unknown top-level / section keys encountered during parse.
    /// Populated even when `--strict` is off so `config print` can warn.
    pub unknown_keys: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct GeneralSettings {
    pub default_mode: String,
    pub theme: String,
    pub locale: String,
}

#[derive(Debug, Clone, Default)]
pub struct LocalSettings {
    pub interval_secs: Option<u64>,
}

#[derive(Debug, Clone, Default)]
pub struct ViewSettings {
    pub hostfile: Option<String>,
    pub hosts: Vec<String>,
    pub interval_secs: Option<u64>,
    // Agentless SSH transport (issue #194). Empty / None means "use CLI
    // defaults". `ssh` (targets) and `ssh_hostfile` are *additive* with
    // CLI: a TOML-provided target list is merged with `--ssh` on the
    // command line at dispatch time.
    pub ssh: Vec<String>,
    pub ssh_hostfile: Option<String>,
    pub ssh_key: Option<String>,
    pub ssh_config: Option<String>,
    pub ssh_strict_host_key: Option<String>,
    pub ssh_timeout_secs: Option<u64>,
    pub ssh_fallback: Option<String>,
    pub ssh_known_hosts: Option<String>,
    pub ssh_concurrency: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct ApiSettings {
    pub port: u16,
    pub socket: SocketSetting,
    pub processes: bool,
    pub interval_secs: u64,
}

#[derive(Debug, Clone)]
pub struct DisplaySettings {
    pub color_scheme: String,
    pub gauge_style: String,
    pub show_led_grid: bool,
}

impl Default for DisplaySettings {
    fn default() -> Self {
        Self {
            color_scheme: "default".to_string(),
            gauge_style: "blocks".to_string(),
            show_led_grid: true,
        }
    }
}

#[derive(Debug, Clone)]
pub struct RecordSettings {
    pub output_dir: String,
    pub compress: String,
}

#[derive(Debug, Clone)]
pub struct SnapshotSettings {
    pub default_format: String,
    pub default_pretty: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            general: GeneralSettings {
                default_mode: "local".to_string(),
                theme: "auto".to_string(),
                locale: "en".to_string(),
            },
            local: LocalSettings::default(),
            view: ViewSettings::default(),
            api: ApiSettings {
                port: 9090,
                socket: SocketSetting::Unset,
                processes: false,
                interval_secs: 3,
            },
            alerts: AlertConfig::default(),
            energy: EnergyConfig::default(),
            display: DisplaySettings {
                color_scheme: "default".to_string(),
                gauge_style: "blocks".to_string(),
                show_led_grid: true,
            },
            record: RecordSettings {
                output_dir: "~/.cache/all-smi/records".to_string(),
                compress: "zstd".to_string(),
            },
            snapshot: SnapshotSettings {
                default_format: "json".to_string(),
                default_pretty: true,
            },
            source_path: None,
            unknown_keys: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------
// Outcome + entry points
// ---------------------------------------------------------------------

/// Result returned by [`load`]. Callers that need to warn on unknown
/// keys (`config print`) read `settings.unknown_keys`; callers that
/// need to enforce strictness (`config validate --strict`) should
/// instead consult [`validate_file`].
#[derive(Debug)]
pub struct LoadOutcome {
    pub settings: Settings,
    /// Warnings emitted during the merge (e.g., ignored env values).
    pub warnings: Vec<String>,
}

/// Load configuration from the given path (or the platform default).
///
/// * `explicit` — path supplied via `--config`. When `Some`, the loader
///   fails if the file does not exist or cannot be read. When `None`,
///   the loader probes [`crate::common::paths::candidate_config_paths`]
///   and silently falls back to defaults when no file is found.
///
/// Env-var overrides are applied on top of the file values before the
/// final [`Settings`] is returned. CLI-flag precedence is applied later
/// in the caller (see `src/main.rs`).
pub fn load(explicit: Option<&Path>) -> Result<LoadOutcome, ConfigError> {
    let mut warnings = Vec::new();

    let (raw, source_path, unknown_keys): (RawConfig, Option<PathBuf>, Vec<String>) =
        if let Some(path) = explicit {
            let contents = read_config_capped(path).map_err(|e| ConfigError::Io {
                path: path.to_path_buf(),
                source: e,
            })?;
            let (raw, unknown) = parse_toml(&contents)?;
            check_schema_version(&raw)?;
            (raw, Some(path.to_path_buf()), unknown)
        } else if let Some(path) = paths::discover_existing_config() {
            match read_config_capped(&path) {
                Ok(contents) => {
                    let (raw, unknown) = parse_toml(&contents)?;
                    check_schema_version(&raw)?;
                    (raw, Some(path), unknown)
                }
                Err(e) => {
                    // The file existed when `discover_existing_config`
                    // was called but disappeared under us (race); treat
                    // as "no file" rather than a hard error.
                    warnings.push(format!(
                        "config: could not read {} ({e}); using defaults",
                        path.display()
                    ));
                    (RawConfig::default(), None, Vec::new())
                }
            }
        } else {
            (RawConfig::default(), None, Vec::new())
        };

    let mut settings = Settings {
        source_path,
        unknown_keys,
        ..Settings::default()
    };

    config_apply::apply_file(&raw, &mut settings)?;
    config_env::apply_env(&mut settings, &mut warnings);

    Ok(LoadOutcome { settings, warnings })
}

/// Schema-version gate. Returning an error short-circuits `load`.
fn check_schema_version(raw: &RawConfig) -> Result<(), ConfigError> {
    if let Some(v) = raw.schema_version
        && v != SUPPORTED_SCHEMA_VERSION
    {
        return Err(ConfigError::SchemaVersion {
            found: v,
            supported: SUPPORTED_SCHEMA_VERSION,
        });
    }
    Ok(())
}

/// Public validation entry point used by `all-smi config validate
/// --strict`. Returns `Ok(())` when the file parses and no unknown keys
/// are present. Without `strict`, unknown keys are allowed — the caller
/// should still call [`load`] to get the semantic errors.
pub fn validate_file(path: &Path, strict: bool) -> Result<Settings, ConfigError> {
    let contents = read_config_capped(path).map_err(|e| ConfigError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    let (raw, unknown_keys) = parse_toml(&contents)?;
    check_schema_version(&raw)?;

    if strict && !unknown_keys.is_empty() {
        return Err(ConfigError::UnknownKey(unknown_keys.join(", ")));
    }

    let mut settings = Settings {
        source_path: Some(path.to_path_buf()),
        unknown_keys,
        ..Settings::default()
    };
    config_apply::apply_file(&raw, &mut settings)?;

    Ok(settings)
}

// ---------------------------------------------------------------------
// Parser + merge helpers
// ---------------------------------------------------------------------

/// Parse a TOML document and report any unknown top-level keys.
///
/// Implementation detail: we parse once into a generic `toml::Value`
/// so we can enumerate the top-level keys independently of the typed
/// deserializer (which silently ignores unknown keys when
/// `deny_unknown_fields` is not set). The same parsed value is then
/// fed into `TomlValue::try_into` to materialise the typed
/// [`RawConfig`] — no second parse of the source text, saving one full
/// traversal of the document plus its allocations on every load.
pub fn parse_toml(contents: &str) -> Result<(RawConfig, Vec<String>), ConfigError> {
    let value: TomlValue =
        toml::from_str(contents).map_err(|e| ConfigError::Parse(e.to_string()))?;
    let raw: RawConfig = value
        .clone()
        .try_into()
        .map_err(|e: toml::de::Error| ConfigError::Parse(e.to_string()))?;

    let mut unknown = BTreeSet::new();
    if let TomlValue::Table(top) = &value {
        for key in top.keys() {
            if !KNOWN_TOP_LEVEL.contains(&key.as_str()) {
                // Pre-escape so downstream `eprintln!` sites cannot
                // have ANSI / cursor-control sequences injected into
                // operator terminals via a hostile config key. A
                // quoted TOML key like `"x\u001b[31m"` is parsed
                // verbatim; without this step the unknown-key warning
                // would paint the rest of the stderr output red.
                unknown.insert(escape_printable(key));
            }
        }
        scan_unknown_subkeys(top, &mut unknown);
    }

    Ok((raw, unknown.into_iter().collect()))
}

/// Read a config file into a `String`, enforcing a hard size cap of
/// [`MAX_CONFIG_BYTES`] and refusing to follow symlinks.
///
/// Defence-in-depth rationale:
/// * **Size cap.** A legitimate config is well under a kilobyte. Without
///   a cap, a renamed log file or a malicious attempt to OOM the daemon
///   at startup would be read unconditionally. We check the metadata
///   length first (fast path) and then also wrap the read in
///   `Read::take(MAX + 1)` so pipes / devices / anything where
///   `metadata().len()` lies still gets capped.
/// * **Symlink refusal.** Even with `read_to_string`, a symlink at the
///   canonical config path (e.g. `~/.config/all-smi/config.toml ->
///   /etc/shadow`) would happily be followed and the target's contents
///   leaked through parse-error messages. We refuse to read symlinks
///   outright; if the operator genuinely wants to point at a file
///   elsewhere they can use `--config /actual/path.toml`.
fn read_config_capped(path: &Path) -> io::Result<String> {
    use std::io::Read;

    // Symlink rejection. `symlink_metadata` does NOT follow; a plain
    // `metadata()` call would.
    match std::fs::symlink_metadata(path) {
        Ok(md) if md.file_type().is_symlink() => {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "refusing to read config at {} — path is a symlink",
                    path.display()
                ),
            ));
        }
        Ok(_) => {}
        // Fall through on metadata errors so `File::open` below produces
        // the canonical NotFound / permission error the caller expects.
        Err(_) => {}
    }

    let f = std::fs::File::open(path)?;
    if let Ok(md) = f.metadata()
        && md.len() > MAX_CONFIG_BYTES
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("config file > {MAX_CONFIG_BYTES} bytes; refusing to read"),
        ));
    }
    let mut buf = String::new();
    // `take(MAX + 1)` so that if we DO read past the cap we can tell the
    // difference between "file is exactly at the cap" and "file exceeded
    // the cap" without allocating beyond the cap + 1 byte.
    f.take(MAX_CONFIG_BYTES + 1).read_to_string(&mut buf)?;
    if buf.len() as u64 > MAX_CONFIG_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("config file > {MAX_CONFIG_BYTES} bytes; refusing to parse"),
        ));
    }
    Ok(buf)
}

/// Walk each known section and record any unrecognised sub-keys. Keeps
/// unknown keys fully qualified (e.g. `api.foo`). Sub-keys containing
/// control characters are escaped via [`escape_printable`] so downstream
/// consumers can print them safely — see the top-level key path for
/// rationale.
fn scan_unknown_subkeys(top: &toml::map::Map<String, TomlValue>, out: &mut BTreeSet<String>) {
    use toml::map::Map;
    let check = |name: &str, known: &[&str], out: &mut BTreeSet<String>, top: &Map<_, _>| {
        if let Some(TomlValue::Table(sec)) = top.get(name) {
            for k in sec.keys() {
                if !known.contains(&k.as_str()) {
                    out.insert(format!("{name}.{}", escape_printable(k)));
                }
            }
        }
    };
    check("general", &["default_mode", "theme", "locale"], out, top);
    check("local", &["interval_secs"], out, top);
    check("view", &["hostfile", "hosts", "interval_secs"], out, top);
    check(
        "api",
        &["port", "socket", "processes", "interval_secs"],
        out,
        top,
    );
    check(
        "alerts",
        &[
            "enabled",
            "temp_warn_c",
            "temp_crit_c",
            "util_idle_pct",
            "util_idle_warn_mins",
            "hysteresis_c",
            "bell_on_critical",
            "webhook_url",
            "power_crit_w",
        ],
        out,
        top,
    );
    check(
        "energy",
        &[
            "price_per_kwh",
            "currency",
            "show_cost",
            "wal_path",
            "gap_interpolate_seconds",
            "wal_enabled",
        ],
        out,
        top,
    );
    check(
        "display",
        &["color_scheme", "gauge_style", "show_led_grid"],
        out,
        top,
    );
    check("record", &["output_dir", "compress"], out, top);
    check("snapshot", &["default_format", "default_pretty"], out, top);
}

// Test module is in `config_file_tests.rs` to keep this file under the
// 500-line soft limit.
#[cfg(test)]
#[path = "config_file_tests.rs"]
mod tests;
