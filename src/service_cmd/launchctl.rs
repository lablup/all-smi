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

//! The `launchctl(1)` interface used by the launchd backend (issue
//! #310): how launchd objects are named, how the tool is invoked, and
//! how its output is parsed.
//!
//! Split out of [`super::launchd`] so the backend file stays under the
//! 500-line limit, and so the half that is pure (layout resolution and
//! output parsing) sits together and can be unit tested on any host.
//!
//! Nothing here knows what a plist contains; that is [`super::plist`].

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use super::plist::LABEL;
use super::{Scope, ServiceError};
use crate::utils::command::new_command;

/// Plist file name, identical in both scopes.
pub const PLIST_FILE_NAME: &str = "com.lablup.all-smi.plist";

/// Where an administrator's system-wide daemons belong. Apple reserves
/// `/System/Library/LaunchDaemons` for itself.
const SYSTEM_PLIST_DIR: &str = "/Library/LaunchDaemons";

/// Log file for the system daemon. The parent directory is created at
/// install time and left behind by `uninstall`, so an operator can still
/// read why the service died after removing it.
const SYSTEM_LOG_PATH: &str = "/var/log/all-smi/all-smi.log";

/// Per-user plist directory, relative to the home directory.
const USER_AGENT_SUBDIR: &str = "Library/LaunchAgents";

/// Per-user log file, relative to the home directory. `~/Library/Logs`
/// is the Apple-sanctioned location and is what Console.app shows.
const USER_LOG_SUBPATH: &str = "Library/Logs/all-smi/all-smi.log";

/// `launchctl bootout` returns before launchd has finished tearing the
/// job down, so an immediately following `bootstrap` can lose the race
/// and fail with "Operation already in progress". Retry a few times
/// before giving up.
const BOOTSTRAP_ATTEMPTS: u32 = 5;
const BOOTSTRAP_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(200);
/// Everything one scope resolves to: where its plist and log live, and
/// how launchctl names its domain and its job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    pub plist: PathBuf,
    pub log: PathBuf,
    /// Domain argument for `launchctl bootstrap`, e.g. `gui/501`.
    pub domain: String,
    /// Service target for every other launchctl verb, e.g.
    /// `gui/501/com.lablup.all-smi`.
    pub target: String,
}

/// Pure half of layout resolution, kept separate so both scopes are
/// testable on any host without a real home directory or uid.
pub fn layout_from(scope: Scope, home: &Path, uid: u32) -> Layout {
    match scope {
        Scope::System => Layout {
            plist: Path::new(SYSTEM_PLIST_DIR).join(PLIST_FILE_NAME),
            log: PathBuf::from(SYSTEM_LOG_PATH),
            domain: "system".to_string(),
            target: format!("system/{LABEL}"),
        },
        Scope::User => Layout {
            plist: home.join(USER_AGENT_SUBDIR).join(PLIST_FILE_NAME),
            log: home.join(USER_LOG_SUBPATH),
            domain: format!("gui/{uid}"),
            target: format!("gui/{uid}/{LABEL}"),
        },
    }
}

/// Resolve the layout for `scope` against this process's environment.
pub fn layout(scope: Scope) -> Result<Layout, ServiceError> {
    if scope == Scope::System {
        // The system domain needs neither a home directory nor a uid.
        return Ok(layout_from(scope, Path::new("/"), 0));
    }
    let uid = gui_uid()?;
    let home = dirs::home_dir().ok_or_else(|| {
        ServiceError::NotSupported(
            "cannot locate a home directory for the LaunchAgent; set $HOME, or drop --user to \
             install the system LaunchDaemon with sudo"
                .to_string(),
        )
    })?;
    Ok(layout_from(scope, &home, uid))
}

/// The uid whose `gui` domain a user-scope action targets.
///
/// Refuses uid 0 rather than producing `gui/0`. That domain does not
/// exist: root has no GUI login session, so `launchctl bootstrap gui/0`
/// fails with an error that says nothing about the real mistake, which
/// is almost always a reflexive `sudo` in front of an otherwise correct
/// `--user` command.
#[cfg(unix)]
fn gui_uid() -> Result<u32, ServiceError> {
    // SAFETY: `getuid` takes no arguments, reads no caller-provided
    // memory, and is documented as always succeeding.
    let uid = unsafe { libc::getuid() };
    if uid == 0 {
        return Err(ServiceError::NotSupported(
            "--user targets the gui/0 launchd domain, which does not exist because root has no \
             login session. Drop sudo to manage your own LaunchAgent, or drop --user to install \
             the system LaunchDaemon."
                .to_string(),
        ));
    }
    Ok(uid)
}

/// Non-Unix stand-in so the pure logic in this module still compiles
/// under `cfg(test)` on Windows. launchd exists only on macOS, so this
/// arm is never reachable from a dispatched backend.
#[cfg(not(unix))]
fn gui_uid() -> Result<u32, ServiceError> {
    Err(ServiceError::NotSupported(
        "launchd is macOS-only".to_string(),
    ))
}

// ── launchctl plumbing ────────────────────────────────────────────────

pub(super) fn describe(args: &[&str]) -> String {
    format!("launchctl {}", args.join(" "))
}

fn command(args: &[&str]) -> Command {
    let mut cmd = new_command("launchctl");
    cmd.args(args);
    cmd
}

pub(super) fn output(args: &[&str]) -> Result<Output, ServiceError> {
    Ok(command(args).output()?)
}

/// Run `launchctl` and return stdout, mapping a non-zero exit onto
/// [`ServiceError::CommandFailed`].
pub(super) fn run(args: &[&str]) -> Result<String, ServiceError> {
    let output = output(args)?;
    if !output.status.success() {
        return Err(ServiceError::CommandFailed {
            cmd: describe(args),
            stderr: failure_text(&output),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Run `launchctl` for its side effect, tolerating failure.
///
/// Used where the desired end state is already satisfied by the failure:
/// booting out a job that is not loaded, or enabling one that was never
/// disabled.
pub(super) fn run_best_effort(args: &[&str]) {
    let _ = output(args);
}

/// Human-readable reason a launchctl invocation failed. launchctl prints
/// its diagnostics to stderr, but falls back to stdout for some verbs.
pub(super) fn failure_text(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if !stderr.is_empty() {
        return stderr;
    }
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !stdout.is_empty() {
        return stdout;
    }
    format!("exited with {}", output.status)
}

/// `launchctl print <target>` stdout, or `None` when the job is not
/// loaded in that domain.
///
/// `launchctl print` exits 113 for an unknown service, so a non-zero
/// exit is the normal "not loaded" answer rather than an error.
pub(super) fn print_job(target: &str) -> Result<Option<String>, ServiceError> {
    let output = output(&["print", target])?;
    if output.status.success() {
        Ok(Some(String::from_utf8_lossy(&output.stdout).into_owned()))
    } else {
        Ok(None)
    }
}

/// Bootstrap the job, retrying past the teardown race described on
/// [`BOOTSTRAP_ATTEMPTS`].
pub(super) fn bootstrap(layout: &Layout) -> Result<(), ServiceError> {
    let plist_arg = path_arg(&layout.plist)?;
    let args = ["bootstrap", layout.domain.as_str(), plist_arg];
    let mut last = None;
    for attempt in 0..BOOTSTRAP_ATTEMPTS {
        match run(&args) {
            Ok(_) => return Ok(()),
            Err(e) => {
                last = Some(e);
                if attempt + 1 < BOOTSTRAP_ATTEMPTS {
                    std::thread::sleep(BOOTSTRAP_RETRY_DELAY);
                }
            }
        }
    }
    Err(last.unwrap_or_else(|| ServiceError::CommandFailed {
        cmd: describe(&args),
        stderr: "bootstrap did not run".to_string(),
    }))
}

/// Borrow a path as a launchctl argument, refusing one that is not UTF-8.
pub(super) fn path_arg(path: &Path) -> Result<&str, ServiceError> {
    path.to_str().ok_or_else(|| {
        ServiceError::Conflict(format!(
            "path `{}` is not valid UTF-8 and cannot be passed to launchctl",
            path.display()
        ))
    })
}

// ── parsing ───────────────────────────────────────────────────────────

/// The subset of `launchctl print <service-target>` this backend needs.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PrintInfo {
    /// The job's top-level `state` value, e.g. `running`, `not running`.
    pub state: String,
    /// The job's pid when it has one.
    pub pid: Option<u32>,
}

impl PrintInfo {
    /// launchd reports `state = running` only while the program is
    /// actually executing; `waiting`, `not running`, and `spawn
    /// scheduled` all mean it is not.
    pub fn running(&self) -> bool {
        self.state == "running"
    }
}

/// Parse `launchctl print <service-target>` output.
///
/// `launchctl print` nests dictionaries (endpoints, semaphores) that
/// repeat the `state` key with unrelated values such as `active`, so
/// only the first occurrence of each key, which is the job's own, is
/// taken. Keys are matched on the whole trimmed name so neighbours like
/// `jetsam priority` and `last exit code` cannot be mistaken for them.
pub fn parse_print_output(raw: &str) -> PrintInfo {
    let mut state: Option<String> = None;
    let mut pid: Option<u32> = None;

    for line in raw.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "state" if state.is_none() => state = Some(value.to_string()),
            "pid" if pid.is_none() => pid = value.parse::<u32>().ok().filter(|p| *p != 0),
            _ => {}
        }
    }

    PrintInfo {
        state: state.unwrap_or_default(),
        pid,
    }
}

/// Find `label` in `launchctl print-disabled <domain>` output.
///
/// Returns `Some(true)` when the label carries a persistent disable
/// override, `Some(false)` when it is explicitly enabled, and `None`
/// when the domain does not mention it at all. Both spellings launchctl
/// has used are accepted: `=> true` / `=> false` on older releases,
/// `=> disabled` / `=> enabled` since Sonoma.
pub fn parse_disabled(raw: &str, label: &str) -> Option<bool> {
    let needle = format!("\"{label}\"");
    for line in raw.lines() {
        let Some(rest) = line.trim().strip_prefix(needle.as_str()) else {
            continue;
        };
        let Some(value) = rest.split("=>").nth(1) else {
            continue;
        };
        return match value.trim() {
            "true" | "disabled" => Some(true),
            "false" | "enabled" => Some(false),
            _ => None,
        };
    }
    None
}

/// Whether the domain carries a persistent disable override for our
/// label. `None` when launchctl could not answer.
pub(super) fn query_disabled(domain: &str) -> Option<bool> {
    let output = output(&["print-disabled", domain]).ok()?;
    if !output.status.success() {
        return None;
    }
    let raw = String::from_utf8_lossy(&output.stdout);
    Some(parse_disabled(&raw, LABEL).unwrap_or(false))
}
/// Detail line for a job launchctl could not print.
///
/// The usual reason is that the plist is on disk but nothing has
/// bootstrapped it yet, which is exactly the state `install` without
/// `--now` leaves behind. Anything else (most often a system-domain
/// query from an unprivileged shell) is surfaced verbatim, because
/// silently reporting "not loaded" for a permission failure would be a
/// lie.
pub(super) fn not_loaded_detail(installed: bool, stderr: &str) -> String {
    if !installed {
        return "not installed".to_string();
    }
    if stderr.contains("Could not find service") {
        return "not loaded".to_string();
    }
    let first = stderr.lines().next().unwrap_or("").trim();
    if first.is_empty() {
        "not loaded".to_string()
    } else {
        format!("loaded state unknown: {first}")
    }
}

// Test module lives in `launchctl_tests.rs` to keep this file under the
// 500-line soft limit.
#[cfg(test)]
#[path = "launchctl_tests.rs"]
mod tests;
