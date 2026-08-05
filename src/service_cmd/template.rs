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

//! systemd unit rendering for `all-smi service install` (issue #309).
//!
//! There is exactly one unit definition in this repository:
//! `packaging/systemd/all-smi.service`. The Debian package installs a
//! byte-identical copy through `debian/all-smi.service` (a CI job
//! enforces that), and this module embeds the same file with
//! `include_str!` and rewrites the handful of directives that depend on
//! how the operator is installing it.
//!
//! Rewrites applied:
//!
//! | Directive | System scope | User scope |
//! |---|---|---|
//! | `ExecStart=` | canonicalized `current_exe()` | same |
//! | `User=` / `Group=` | `--service-user` value, or dropped so the service runs as root | dropped |
//! | `WantedBy=` | `multi-user.target` | `default.target` |
//! | everything in [`USER_SCOPE_DROPPED_PREFIXES`] | kept | dropped |
//!
//! Everything else, hardening included, is passed through verbatim so
//! the shipped unit stays the single source of truth: editing the
//! sandboxing there changes the packaged service and every subcommand
//! install at once.
//!
//! One consequence worth knowing when reading logs: `ProtectHome=true`
//! survives into a system-scope unit, so a service running as root
//! cannot see `/root/.config/all-smi/config.toml`. That is deliberate.
//! The machine-wide `/etc/all-smi/config.toml` is the configuration path
//! for a system service, and it is a discovery candidate exactly so this
//! works (see [`crate::common::paths::candidate_config_paths`]).

use std::path::Path;

use super::Scope;

/// Marker comment stamped as the first line of every unit this tool
/// writes. `install` and `uninstall` both refuse to touch a unit file
/// that lacks it unless `--force` is passed, which keeps a hand-written
/// or distro-shipped unit from being silently clobbered.
pub const MANAGED_MARKER: &str = "# Managed by 'all-smi service'";

/// The canonical unit, embedded verbatim at compile time.
pub const UNIT_TEMPLATE: &str = include_str!("../../packaging/systemd/all-smi.service");

/// Directives stripped when rendering a user-scope unit.
///
/// # The rule
///
/// A per-user `systemd --user` manager runs unprivileged. Any directive
/// whose setup needs a privilege the manager does not hold makes the
/// unit fail *before* `ExecStart` is reached, so the service never
/// starts at all. The failure surfaces as an opaque "control process
/// exited with error code" from `systemctl --user start`, with the real
/// cause only in the journal. Every such directive must be dropped here.
///
/// Directives that are implemented purely with `prctl` or a seccomp
/// filter need no privilege and are kept in both scopes:
/// `NoNewPrivileges=` and `RestrictSUIDSGID=`.
///
/// # Verified failure modes
///
/// Reproduced against systemd 255 on Ubuntu 24.04 with a real per-user
/// manager, using `/bin/sleep` as `ExecStart` so the application itself
/// was out of the picture:
///
/// | Directive | Exit status | Journal |
/// |---|---|---|
/// | `SupplementaryGroups=` | `216/GROUP` | `Failed to determine supplementary groups: No such process` |
/// | `ProtectKernelModules=` | `218/CAPABILITIES` | `Failed to set up user namespacing for unprivileged user` then `Failed to drop capabilities: Operation not permitted` |
///
/// `ProtectKernelModules=` is the non-obvious one: it reads as pure
/// seccomp, but it also strips `CAP_SYS_MODULE` from the capability
/// bounding set and hides `/usr/lib/modules`, and an unprivileged
/// manager can only do that from inside a user namespace. Where the host
/// denies unprivileged user namespaces, the unit dies at 218 before
/// exec.
pub const USER_SCOPE_DROPPED_PREFIXES: &[&str] = &[
    // ---- Need a privilege the user manager does not hold ----
    // 216/GROUP: a user manager cannot change supplementary groups.
    "SupplementaryGroups=",
    // 218/CAPABILITIES where unprivileged user namespaces are denied:
    // altering the capability bounding set requires one.
    "ProtectKernelModules=",
    // The following need a private mount namespace, which an
    // unprivileged manager can only obtain through a user namespace.
    // Ubuntu 24.04 and later restrict those through AppArmor, and a unit
    // whose namespace setup fails does not start (226/NAMESPACE).
    // `ProtectHome=` is doubly wrong here: it would hide the operator's
    // own `~/.config/all-smi/config.toml` from their own service.
    "ProtectSystem=",
    "ProtectHome=",
    "PrivateTmp=",
    "ProtectControlGroups=",
    // ---- Meaningless or wrong in a user manager ----
    // The user cache directory resolves normally, so pinning the WAL at
    // /var/cache/all-smi would point it somewhere unwritable.
    "Environment=ALL_SMI_ENERGY_WAL_PATH=",
    // network-online.target is a system target; referencing it from a
    // user unit only produces a startup warning.
    "After=network-online.target",
    "Wants=network-online.target",
];

// The complementary "kept in user scope" list lives in the test module
// (`USER_SCOPE_KEPT_HARDENING` in `template_tests.rs`). It is a test
// contract, not runtime data: the renderer only ever consults the drop
// list above. Keeping it here would be dead code in the binary target,
// where dead-code analysis is per target and a `pub` item is not
// automatically live the way it is in the library target.

/// Inputs for [`render_unit`].
#[derive(Debug, Clone)]
pub struct RenderParams<'a> {
    pub scope: Scope,
    /// Absolute path to the binary the unit should exec. Callers pass
    /// the canonicalized `current_exe()`.
    pub exec_path: &'a Path,
    /// Account for `User=` / `Group=`. `None` renders a unit with
    /// neither directive, so systemd runs it as root. Ignored in user
    /// scope.
    pub service_user: Option<&'a str>,
}

/// Reasons an executable path cannot be expressed in a systemd
/// `ExecStart=` line.
#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    #[error(
        "executable path `{0}` is not valid UTF-8; systemd unit files must be UTF-8, so the \
         binary has to live at a UTF-8 path to be installed as a service"
    )]
    NonUtf8Path(String),
    #[error(
        "executable path `{0}` contains a character systemd would reinterpret in ExecStart (a \
         double quote, a backslash, or a newline); move the binary to a plainer path"
    )]
    UnsafePath(String),
}

/// Render the unit for one install.
pub fn render_unit(params: &RenderParams<'_>) -> Result<String, RenderError> {
    let exec = exec_start_token(params.exec_path)?;
    let user_scope = params.scope == Scope::User;

    let mut out = String::with_capacity(UNIT_TEMPLATE.len() + 512);
    out.push_str(MANAGED_MARKER);
    out.push('\n');
    out.push_str("# Written by `all-smi service install`. Manual edits are lost on the next\n");
    out.push_str("# install. Put runtime settings in the environment file or the TOML config\n");
    out.push_str("# instead; run `all-smi config path` to print the active TOML path.\n");

    for line in UNIT_TEMPLATE.lines() {
        let key = line.trim_start();

        if key.starts_with("ExecStart=") {
            out.push_str(&format!("ExecStart={exec} api\n"));
            continue;
        }

        if key.starts_with("User=") || key.starts_with("Group=") {
            if user_scope {
                continue;
            }
            // Without an explicit account the unit runs as root, which
            // is the documented default for subcommand installs: vendor
            // CLIs differ too much for a safe guess.
            if let Some(account) = params.service_user {
                let directive = if key.starts_with("User=") {
                    "User"
                } else {
                    "Group"
                };
                out.push_str(&format!("{directive}={account}\n"));
            }
            continue;
        }

        if user_scope {
            if USER_SCOPE_DROPPED_PREFIXES
                .iter()
                .any(|prefix| key.starts_with(prefix))
            {
                continue;
            }
            if key.starts_with("WantedBy=") {
                out.push_str("WantedBy=default.target\n");
                continue;
            }
        }

        out.push_str(line);
        out.push('\n');
    }

    Ok(out)
}

/// Whether `contents` was written by this tool.
pub fn is_managed(contents: &str) -> bool {
    contents.lines().any(|l| l.trim_end() == MANAGED_MARKER)
}

/// Turn an executable path into a systemd `ExecStart` token.
///
/// systemd applies its own unescaping to the value, so a path can only
/// be embedded once it is known to survive that round trip:
///
/// * `%` starts a specifier and is escaped as `%%`.
/// * Whitespace, `;`, and `'` split or terminate the command line, so
///   such a path is wrapped in double quotes.
/// * A double quote, backslash, or newline cannot be expressed safely
///   and is rejected outright rather than silently mangled.
fn exec_start_token(path: &Path) -> Result<String, RenderError> {
    let raw = path
        .to_str()
        .ok_or_else(|| RenderError::NonUtf8Path(path.display().to_string()))?;

    if raw.contains('"') || raw.contains('\\') || raw.contains('\n') || raw.contains('\r') {
        return Err(RenderError::UnsafePath(raw.to_string()));
    }

    let escaped = raw.replace('%', "%%");
    let needs_quotes = escaped
        .chars()
        .any(|c| c.is_whitespace() || c == ';' || c == '\'');
    if needs_quotes {
        Ok(format!("\"{escaped}\""))
    } else {
        Ok(escaped)
    }
}

// Test module lives in `template_tests.rs` to keep this file under the
// 500-line soft limit.
#[cfg(test)]
#[path = "template_tests.rs"]
mod tests;
