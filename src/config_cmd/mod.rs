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

//! Runtime glue for the `all-smi config` subcommand (issue #192).
//!
//! Entrypoints:
//! - [`run`] dispatches on [`ConfigAction`] and returns a process exit
//!   code (0 on success, 2 on validation failure, 1 on unexpected
//!   runtime error).
//!
//! Keeping the implementation out of `cli.rs` keeps the clap-facing
//! surface a pure data declaration and lets the main binary import the
//! action handlers without a long switch statement inline.

use std::path::{Path, PathBuf};

use crate::cli::{ConfigAction, ConfigPrintArgs, ConfigPrintFormat, ConfigValidateArgs};
use crate::common::config_file::{self, ConfigError, Settings, SocketSetting, escape_printable};
use crate::common::paths;
use crate::common::secure_write;

mod example;
mod render;

pub use example::EXAMPLE_TOML;
pub use render::{render_json, render_toml};

/// Dispatch a `config` subcommand. Returns a process exit code.
pub fn run(explicit_config_path: Option<&Path>, action: &ConfigAction) -> i32 {
    match action {
        // `--config <path>` propagates to every sub-subcommand: `init`
        // writes there (instead of the platform-canonical path), and
        // `validate` treats it as the default target when the
        // positional argument is omitted. Without this thread the
        // global flag silently did nothing for `init`/`validate`,
        // contradicting its "global" clap attribute.
        ConfigAction::Init(args) => run_init(explicit_config_path, args.force),
        ConfigAction::Print(args) => run_print(explicit_config_path, args),
        ConfigAction::Validate(args) => run_validate(explicit_config_path, args),
    }
}

fn run_init(explicit: Option<&Path>, force: bool) -> i32 {
    let Some(target) = explicit
        .map(Path::to_path_buf)
        .or_else(paths::default_config_path)
    else {
        eprintln!("error: could not determine a config directory for this platform");
        return 1;
    };

    if let Err(e) = paths::ensure_parent_dir(&target) {
        eprintln!(
            "error: could not create config directory {}: {e}",
            target.parent().unwrap_or(Path::new("")).display()
        );
        return 1;
    }

    let already_exists = target.exists();
    if already_exists && !force {
        eprintln!(
            "error: config file already exists at {} — pass --force to overwrite",
            target.display()
        );
        return 1;
    }

    let result = if already_exists {
        // Force mode: atomic replace with hardening.
        secure_write::write_atomic_secure(&target, EXAMPLE_TOML.as_bytes())
    } else {
        // Fresh creation: refuse to follow symlinks, refuse to clobber.
        use std::io::Write;
        match secure_write::create_new_secure(&target) {
            Ok(mut f) => f
                .write_all(EXAMPLE_TOML.as_bytes())
                .and_then(|()| f.sync_all()),
            Err(e) => Err(e),
        }
    };

    match result {
        Ok(()) => {
            println!("wrote example config to {}", target.display());
            0
        }
        Err(e) => {
            eprintln!("error: failed to write config at {}: {e}", target.display());
            1
        }
    }
}

fn run_print(explicit: Option<&Path>, args: &ConfigPrintArgs) -> i32 {
    let outcome = match config_file::load(explicit) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("error: {e}");
            return exit_code_for(&e);
        }
    };

    // Surface load-time warnings so operators can see why their env
    // values got dropped, why a file failed the pre-parse, etc.
    for w in &outcome.warnings {
        eprintln!("warning: {w}");
    }
    // Unknown keys are already sanitised at parse time (see
    // `config_file::parse_toml` -> `escape_printable`), but re-apply
    // the escape defensively so a future code path that stores a raw
    // key into `unknown_keys` cannot regress the terminal-injection
    // guarantee.
    for k in &outcome.settings.unknown_keys {
        eprintln!(
            "warning: unknown config key `{}` (forward-compat — preserved)",
            escape_printable(k)
        );
    }

    let rendered = match args.format {
        ConfigPrintFormat::Toml => render_toml(&outcome.settings, args.show_secrets),
        ConfigPrintFormat::Json => render_json(&outcome.settings, args.show_secrets),
    };
    match rendered {
        Ok(s) => {
            print!("{s}");
            if !s.ends_with('\n') {
                println!();
            }
            0
        }
        Err(e) => {
            eprintln!("error: failed to render settings: {e}");
            1
        }
    }
}

fn run_validate(explicit: Option<&Path>, args: &ConfigValidateArgs) -> i32 {
    // Resolution order mirrors the operator's expectation for a
    // "global" `--config`: positional `validate <path>` wins first,
    // then the global `--config`, then the platform-canonical default.
    let path: PathBuf = if let Some(p) = &args.path {
        p.clone()
    } else if let Some(p) = explicit {
        p.to_path_buf()
    } else if let Some(p) = paths::default_config_path() {
        p
    } else {
        eprintln!("error: could not determine default config path; pass an explicit path");
        return 2;
    };

    if !path.exists() {
        eprintln!("error: config file not found at {}", path.display());
        return 2;
    }

    match config_file::validate_file(&path, args.strict) {
        Ok(settings) => {
            if !settings.unknown_keys.is_empty() {
                eprintln!("warnings:");
                for k in &settings.unknown_keys {
                    eprintln!("  - unknown key `{}`", escape_printable(k));
                }
            }
            println!("config OK: {}", path.display());
            0
        }
        Err(e) => {
            eprintln!("error: {e}");
            2
        }
    }
}

fn exit_code_for(err: &ConfigError) -> i32 {
    match err {
        ConfigError::Io { .. } => 2,
        ConfigError::Parse(_) => 2,
        ConfigError::SchemaVersion { .. } => 2,
        ConfigError::Semantic(_) => 2,
        ConfigError::UnknownKey(_) => 2,
    }
}

/// Redact secrets in-place for `print` output. Idempotent on missing
/// values. Callers clone `Settings` before invoking this — the loader
/// keeps the original intact. Currently unused (the TOML/JSON
/// renderers redact inline); kept for future call sites (e.g. a
/// structured-log dump of the merged config).
#[allow(dead_code)]
pub fn redact_secrets(s: &mut Settings) {
    if !s.alerts.webhook_url.is_empty() {
        s.alerts.webhook_url = "<redacted>".to_string();
    }
}

/// Helper for callers who need only the socket setting as a displayable
/// string. Returns `"false"`, `"true"`, or the explicit path. Currently
/// unused; kept as a deliberate helper for future CLI echo / doctor
/// callers that need the same formatting the renderer uses.
#[allow(dead_code)]
pub fn socket_display(s: &SocketSetting) -> String {
    match s {
        SocketSetting::Unset => "false".to_string(),
        SocketSetting::Bool(b) => b.to_string(),
        SocketSetting::Path(p) => format!("\"{p}\""),
    }
}
