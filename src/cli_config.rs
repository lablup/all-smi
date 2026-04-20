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

//! Argument definitions for the `all-smi config` subcommand (issue
//! #192). Re-exported from `cli` for ergonomic `use crate::cli::...`
//! call sites; split here so the main CLI file stays below the 500-line
//! soft limit.

use std::path::PathBuf;

use clap::{Args, Subcommand, ValueEnum};

/// Arguments for the `all-smi config` subcommand (issue #192).
#[derive(Args, Clone, Debug)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub action: ConfigAction,
}

/// `config` sub-subcommands. See the issue for the contract; briefly:
///
/// - `config init [--force]` writes a commented example config to the
///   platform-canonical path.
/// - `config print [--format toml|json] [--show-secrets]` prints the
///   merged effective configuration, with `webhook_url` redacted unless
///   `--show-secrets` is set.
/// - `config validate [<path>] [--strict]` parses the given (or default)
///   file and reports any errors, exiting 0 on valid and 2 on invalid.
#[derive(Subcommand, Clone, Debug)]
pub enum ConfigAction {
    /// Write a commented example config.toml at the default location.
    /// Refuses to overwrite without `--force`.
    Init(ConfigInitArgs),
    /// Print the effective merged configuration in TOML or JSON form.
    Print(ConfigPrintArgs),
    /// Parse a config file and report any errors. Exit 0 on valid, 2
    /// on invalid.
    Validate(ConfigValidateArgs),
}

#[derive(Args, Clone, Debug)]
pub struct ConfigInitArgs {
    /// Overwrite an existing config file (atomically).
    #[arg(long)]
    pub force: bool,
}

/// Output format for `config print`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum ConfigPrintFormat {
    Toml,
    Json,
}

impl std::fmt::Display for ConfigPrintFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Toml => write!(f, "toml"),
            Self::Json => write!(f, "json"),
        }
    }
}

#[derive(Args, Clone, Debug)]
pub struct ConfigPrintArgs {
    /// Output format — `toml` or `json`.
    #[arg(long, value_enum, default_value_t = ConfigPrintFormat::Toml)]
    pub format: ConfigPrintFormat,
    /// By default `webhook_url` is redacted to avoid leaking bot tokens
    /// in terminal scrollback. Set this flag to print it verbatim.
    #[arg(long)]
    pub show_secrets: bool,
}

#[derive(Args, Clone, Debug)]
pub struct ConfigValidateArgs {
    /// File to validate. Defaults to the platform-canonical config
    /// path. When the path does not exist the command exits 2.
    pub path: Option<PathBuf>,
    /// Reject unknown keys (top-level or inside known sections).
    /// Without this flag unknown keys produce a warning but not an
    /// error, so the config remains forward-compatible.
    #[arg(long)]
    pub strict: bool,
}
