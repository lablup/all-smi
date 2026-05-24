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

//! Integration tests for issue #213 — `--help` exposes the active TOML
//! config file location, and the `--config` flag's own help text no
//! longer requires the user to run a side-effecting command
//! (`config init`) to discover the default path.
//!
//! These tests intentionally exercise the lib-level API
//! (`all_smi::cli::build_command_with_runtime_help`) rather than
//! shelling out to the compiled binary so they stay fast and avoid
//! spawning a child process for each assertion.

use all_smi::cli::{build_command_with_runtime_help, config_help_block};
use all_smi::common::paths;

/// `--help` must render the runtime-composed "Configuration file"
/// block. Without this, a fresh user who reaches for `-h` has no way
/// to know a TOML config exists or where it lives.
#[test]
fn help_shows_configuration_file_block() {
    let mut cmd = build_command_with_runtime_help();
    let help = cmd.render_help().to_string();
    assert!(
        help.contains("Configuration file:"),
        "help must contain `Configuration file:` block.\n--- help ---\n{help}"
    );
    assert!(
        help.contains("Active path (this platform):"),
        "help must label the active path.\n--- help ---\n{help}"
    );
    assert!(
        help.contains("Override path with --config"),
        "help must document the --config override.\n--- help ---\n{help}"
    );
}

/// The composed help text must keep the existing Energy Session block
/// in place. Regression guard against the runtime-composition refactor
/// accidentally dropping the static block.
#[test]
fn help_preserves_existing_energy_block() {
    let mut cmd = build_command_with_runtime_help();
    let help = cmd.render_help().to_string();
    assert!(
        help.contains("Energy Session"),
        "Energy Session block must still be rendered.\n--- help ---\n{help}"
    );
    assert!(
        help.contains("ALL_SMI_ENERGY_PRICE"),
        "Energy env-var docs must still be rendered.\n--- help ---\n{help}"
    );
}

/// The active path line must carry an existence marker — either
/// `(active)`, `(not found)`, or the no-config explainer. Without a
/// marker, the user cannot tell from `--help` whether the file exists.
#[test]
fn help_active_path_carries_existence_marker() {
    let block = config_help_block();
    let has_marker = block.contains("(active)")
        || block.contains("(not found)")
        || block.contains("no config path");
    assert!(
        has_marker,
        "config_help_block must carry an existence marker.\n--- block ---\n{block}"
    );
}

/// The help block must report the same active path the implicit loader
/// would use: first existing candidate if present, otherwise the
/// platform-canonical default. This matters on macOS, where the loader
/// accepts `~/.config/all-smi/config.toml` as a fallback.
#[test]
fn help_active_path_uses_loader_resolution() {
    let block = config_help_block();
    match paths::active_config_path() {
        Some(path) => assert!(
            block.contains(&path.display().to_string()),
            "help must contain loader-active path {}.\n--- block ---\n{block}",
            path.display()
        ),
        None => assert!(
            block.contains("no config path"),
            "help must explain missing config path.\n--- block ---\n{block}"
        ),
    }
}

/// The `--config` flag's own help text must guide users toward a
/// read-only discovery path (`config path` or the inline help block)
/// rather than only `config init`, which has the side effect of
/// writing a file.
#[test]
fn config_flag_help_points_at_readonly_discovery() {
    let mut cmd = build_command_with_runtime_help();
    let help = cmd.render_help().to_string();
    // Anywhere in the rendered help, the read-only discovery command
    // must be mentioned — either in the flag docstring or in the
    // Configuration file block.
    assert!(
        help.contains("config path"),
        "help must mention the read-only `config path` command.\n--- help ---\n{help}"
    );
}

/// `--version` must still render through the runtime-composed command.
/// Regression guard for the clap derive vs. runtime mutation interaction.
#[test]
fn version_flag_still_renders() {
    let cmd = build_command_with_runtime_help();
    let version = cmd.render_version().to_string();
    assert!(
        !version.trim().is_empty(),
        "render_version must produce non-empty text"
    );
}

/// The `config path` subcommand must be reachable from the
/// runtime-composed command tree — i.e. clap parses it as a known
/// subcommand. Regression guard: future edits to `ConfigAction` that
/// drop the variant would surface as a parse failure here.
#[test]
fn config_path_subcommand_is_reachable() {
    let cmd = build_command_with_runtime_help();
    let matches = cmd
        .try_get_matches_from(["all-smi", "config", "path"])
        .expect("`config path` must parse cleanly");
    let (sub_name, sub_matches) = matches
        .subcommand()
        .expect("config subcommand must be present");
    assert_eq!(sub_name, "config");
    let (inner_name, _) = sub_matches
        .subcommand()
        .expect("config sub-subcommand must be present");
    assert_eq!(inner_name, "path");
}

/// `config path --json` is also reachable; verifies the `--json` flag
/// is wired through.
#[test]
fn config_path_accepts_json_flag() {
    let cmd = build_command_with_runtime_help();
    let _matches = cmd
        .try_get_matches_from(["all-smi", "config", "path", "--json"])
        .expect("`config path --json` must parse cleanly");
}
