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

//! launchd property-list rendering for `all-smi service install` on
//! macOS (issue #310).
//!
//! There is exactly one plist definition in this repository:
//! `packaging/launchd/com.lablup.all-smi.plist`. It is a valid system
//! LaunchDaemon on its own, so an operator can copy it into
//! `/Library/LaunchDaemons` by hand. This module embeds the same file
//! with `include_str!` and rewrites the handful of keys that depend on
//! how the operator is installing it, mirroring what
//! [`super::template`] does for the systemd unit.
//!
//! Rewrites applied:
//!
//! | Key | System scope | User scope |
//! |---|---|---|
//! | `ProgramArguments` | canonicalized `current_exe()` plus `api` | same |
//! | `StandardOutPath` / `StandardErrorPath` | `/var/log/all-smi/all-smi.log` | `~/Library/Logs/all-smi/all-smi.log` |
//! | `UserName` | `--service-user` value, else `root` | dropped |
//! | `GroupName` | dropped when `--service-user` is given, else `wheel` | dropped |
//! | everything in [`USER_SCOPE_DROPPED_KEYS`] | kept | dropped |
//!
//! Everything else is passed through verbatim so the shipped plist
//! stays the single source of truth.
//!
//! `GroupName` is dropped rather than mirrored when `--service-user`
//! names an account, which is where this diverges from the systemd
//! renderer. systemd can assume a group named after the account exists,
//! because that is how `systemd-sysusers` creates one. macOS has no such
//! convention: a regular account's primary group is `staff`, and a
//! service account created with `dscl` may have any group at all.
//! Omitting `GroupName` makes launchd use the account's primary group
//! straight out of the password database, which is always right.

use std::path::Path;

use super::Scope;

/// Marker comment stamped into every plist this tool writes.
/// `install` and `uninstall` both refuse to touch a plist that lacks it
/// unless `--force` is passed, which keeps a hand-written or
/// vendor-shipped plist from being silently clobbered.
///
/// XML comments are legal anywhere after the prolog and are discarded by
/// `CFPropertyList`, so launchd never sees this line: `plutil -lint`
/// accepts the rendered file unchanged.
pub const MANAGED_MARKER: &str = "<!-- Managed by 'all-smi service' -->";

/// The canonical launchd job label, shared by both scopes. They live in
/// different launchd domains, so the same label never collides.
pub const LABEL: &str = "com.lablup.all-smi";

/// The canonical plist, embedded verbatim at compile time.
pub const PLIST_TEMPLATE: &str = include_str!("../../packaging/launchd/com.lablup.all-smi.plist");

/// Keys stripped when rendering a user-scope LaunchAgent.
///
/// # The rule, and how launchd differs from systemd here
///
/// This is the launchd counterpart of
/// [`super::template::USER_SCOPE_DROPPED_PREFIXES`], but the failure
/// mode is the opposite one, so the reason to drop these is different
/// and worth stating precisely.
///
/// A `gui/$UID` domain is owned by an unprivileged user and cannot
/// `setuid` to another account. systemd answers that by refusing the
/// unit outright: a user-scope unit carrying `SupplementaryGroups=` dies
/// at `216/GROUP` before `ExecStart`. launchd does not. Bootstrapping a
/// LaunchAgent that carries `UserName`, `GroupName`, or `InitGroups`
/// succeeds, the job runs, and the keys are **silently ignored**.
///
/// Measured on macOS 26.6 (Darwin 25.6) by bootstrapping a probe agent
/// into `gui/501` whose program printed `id`:
///
/// | Plist keys | `launchctl bootstrap` | Effective uid/gid |
/// |---|---|---|
/// | none | succeeds | `501` / `20` |
/// | `UserName root`, `GroupName wheel` | succeeds | `501` / `20` |
///
/// So these are dropped not to avoid a crash but because keeping them
/// would ship a plist that lies. A file in `~/Library/LaunchAgents`
/// declaring `UserName root` reads, to an operator auditing what runs
/// privileged on the machine, as a root job; it is not one. Two further
/// reasons make the silent-ignore behaviour a bad thing to lean on:
/// `launchd.plist(5)` documents these as requiring root and says nothing
/// about ignoring them, so the fallback is unspecified and free to
/// become fatal in a later release; and `InitGroups` is explicitly
/// documented as ignored whenever `UserName` is unset, which in user
/// scope it now always is.
///
/// Keys that need no privilege stay in both scopes, notably
/// `SoftResourceLimits`: raising a *soft* rlimit up to the inherited
/// hard limit is allowed for any process, so the file-descriptor bump
/// applies to a LaunchAgent too. `HardResourceLimits` is deliberately
/// absent from the canonical plist for the mirror-image reason: only
/// root can raise a hard limit, and lowering one buys nothing here.
pub const USER_SCOPE_DROPPED_KEYS: &[&str] = &["UserName", "GroupName", "InitGroups"];

/// Inputs for [`render_plist`].
#[derive(Debug, Clone)]
pub struct RenderParams<'a> {
    pub scope: Scope,
    /// Absolute path to the binary the job should exec. Callers pass
    /// the canonicalized `current_exe()`.
    pub exec_path: &'a Path,
    /// Absolute path for `StandardOutPath` and `StandardErrorPath`.
    pub log_path: &'a Path,
    /// Account for `UserName`. `None` keeps the canonical `root` /
    /// `wheel` pair. Ignored in user scope.
    pub service_user: Option<&'a str>,
}

/// Reasons a path cannot be embedded in a plist.
#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    #[error(
        "path `{0}` is not valid UTF-8; property lists are XML, so the binary and its log have to \
         live at UTF-8 paths to be installed as a service"
    )]
    NonUtf8Path(String),
    #[error(
        "path `{0}` contains a control character that XML 1.0 cannot represent; move the file to a \
         plainer path"
    )]
    UnsafePath(String),
    #[error(
        "account name `{0}` contains a character that is not allowed in a launchd UserName (only \
         letters, digits, `_`, `-`, and `.`)"
    )]
    UnsafeAccount(String),
}

/// Render the plist for one install.
///
/// The template is walked as a sequence of `<key>` / value pairs rather
/// than line by line, because a plist value can be a multi-line
/// `<array>` or `<dict>` and dropping a key means dropping its whole
/// value with it.
pub fn render_plist(params: &RenderParams<'_>) -> Result<String, RenderError> {
    let exec = xml_text(params.exec_path)?;
    let log = xml_text(params.log_path)?;
    let user_scope = params.scope == Scope::User;
    if let Some(account) = params.service_user
        && !user_scope
    {
        validate_account(account)?;
    }

    let lines: Vec<&str> = PLIST_TEMPLATE.lines().collect();
    let mut out = String::with_capacity(PLIST_TEMPLATE.len() + 512);
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();

        // The marker and the provenance note go directly after the
        // DOCTYPE: the XML declaration has to stay on line 1, so a
        // plist cannot carry a first-line marker the way a unit file
        // does.
        if trimmed.starts_with("<!DOCTYPE") {
            out.push_str(line);
            out.push('\n');
            out.push_str(MANAGED_MARKER);
            out.push('\n');
            out.push_str(PROVENANCE_COMMENT);
            i += 1;
            continue;
        }

        let Some(key) = parse_key(trimmed) else {
            out.push_str(line);
            out.push('\n');
            i += 1;
            continue;
        };

        let value_end = value_end_index(&lines, i + 1);
        let indent = leading_whitespace(line);

        match key {
            "ProgramArguments" => {
                out.push_str(line);
                out.push('\n');
                out.push_str(&format!("{indent}<array>\n"));
                out.push_str(&format!("{indent}\t<string>{exec}</string>\n"));
                out.push_str(&format!("{indent}\t<string>api</string>\n"));
                out.push_str(&format!("{indent}</array>\n"));
            }
            "StandardOutPath" | "StandardErrorPath" => {
                out.push_str(line);
                out.push('\n');
                out.push_str(&format!("{indent}<string>{log}</string>\n"));
            }
            "UserName" => {
                if !user_scope {
                    out.push_str(line);
                    out.push('\n');
                    let account = params.service_user.unwrap_or("root");
                    out.push_str(&format!("{indent}<string>{account}</string>\n"));
                }
            }
            "GroupName" => {
                // Kept only for the default root daemon; see the module
                // docs for why an explicit account drops it instead of
                // mirroring the name.
                if !user_scope && params.service_user.is_none() {
                    for l in &lines[i..value_end] {
                        out.push_str(l);
                        out.push('\n');
                    }
                }
            }
            k if user_scope && USER_SCOPE_DROPPED_KEYS.contains(&k) => {}
            _ => {
                for l in &lines[i..value_end] {
                    out.push_str(l);
                    out.push('\n');
                }
            }
        }

        i = value_end;
    }

    Ok(out)
}

/// Whether `contents` was written by this tool.
pub fn is_managed(contents: &str) -> bool {
    contents.lines().any(|l| l.trim() == MANAGED_MARKER)
}

/// Provenance note emitted right below [`MANAGED_MARKER`].
///
/// XML forbids `--` inside a comment, so this text must never grow a
/// double hyphen: no `--force`, no em-dash-like runs.
const PROVENANCE_COMMENT: &str = "<!-- Written by `all-smi service install`. Manual edits are lost \
                                  on the next install.\n     Put runtime settings in the TOML \
                                  config instead; run `all-smi config path`\n     to print the \
                                  active TOML path. -->\n";

/// Extract `X` from a `<key>X</key>` line, or `None` for anything else.
fn parse_key(trimmed: &str) -> Option<&str> {
    trimmed
        .strip_prefix("<key>")
        .and_then(|r| r.strip_suffix("</key>"))
}

/// Index just past the value element that starts at `start`.
///
/// A value is either a single self-contained line (`<string>…</string>`,
/// `<true/>`, `<integer>…</integer>`) or a container that runs until its
/// matching close tag. Container nesting is counted so a `<dict>` inside
/// a `<dict>` does not terminate the outer one early.
fn value_end_index(lines: &[&str], start: usize) -> usize {
    let mut i = start;
    // Skip blank lines between the key and its value.
    while i < lines.len() && lines[i].trim().is_empty() {
        i += 1;
    }
    if i >= lines.len() {
        return lines.len();
    }

    let opener = lines[i].trim();
    let tag = if opener.starts_with("<array>") {
        Some(("<array>", "</array>"))
    } else if opener.starts_with("<dict>") {
        Some(("<dict>", "</dict>"))
    } else {
        None
    };

    let Some((open, close)) = tag else {
        return i + 1;
    };

    let mut depth = 0usize;
    while i < lines.len() {
        let t = lines[i].trim();
        if t.starts_with(open) {
            depth += 1;
        }
        if t.starts_with(close) || t.ends_with(close) {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                return i + 1;
            }
        }
        i += 1;
    }
    lines.len()
}

/// Leading whitespace of `line`, reused so a rewritten value keeps the
/// template's indentation.
fn leading_whitespace(line: &str) -> &str {
    let end = line
        .find(|c: char| !c.is_whitespace())
        .unwrap_or(line.len());
    &line[..end]
}

/// Turn a path into XML character data for a `<string>` element.
///
/// `&` and `<` have to be escaped or the plist stops parsing; `>` is
/// escaped too because it is only conditionally legal as raw text.
/// Control characters (including a newline embedded in a filename) are
/// rejected: XML 1.0 cannot represent them at all, not even as numeric
/// references.
fn xml_text(path: &Path) -> Result<String, RenderError> {
    let raw = path
        .to_str()
        .ok_or_else(|| RenderError::NonUtf8Path(path.display().to_string()))?;
    if raw.chars().any(|c| c.is_control()) {
        return Err(RenderError::UnsafePath(raw.escape_debug().to_string()));
    }
    Ok(xml_escape(raw))
}

fn xml_escape(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for c in raw.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

/// Reject an account name that could not be a real macOS short name.
///
/// This is not cosmetic. The name is written into the plist unescaped
/// and then handed to `getpwnam` by launchd, so restricting it to the
/// POSIX portable set keeps XML injection and shell-looking names out of
/// a root-owned LaunchDaemon.
fn validate_account(name: &str) -> Result<(), RenderError> {
    let ok = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'));
    if ok {
        Ok(())
    } else {
        Err(RenderError::UnsafeAccount(name.escape_debug().to_string()))
    }
}

// Test module lives in `plist_tests.rs` to keep this file under the
// 500-line soft limit.
#[cfg(test)]
#[path = "plist_tests.rs"]
mod tests;
