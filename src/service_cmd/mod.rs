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

//! Cross-platform service-management framework for `all-smi api`
//! (issue #309).
//!
//! # Layering
//!
//! * [`ServiceBackend`] is the platform contract. Exactly one backend is
//!   selected at compile time by [`backend`].
//! * [`run`] is the CLI entry point. It owns everything that must behave
//!   identically on every platform: scope selection, the package-manager
//!   refusal, operator-facing output, and the process exit code.
//! * The backends own everything platform-specific: where the service
//!   definition lives, how it is rendered, and which supervisor command
//!   applies it.
//!
//! # Adding a platform
//!
//! [`backend`] already carries a `cfg` arm for Linux, macOS, and
//! Windows, and all three are filled in: systemd (issue #309), launchd
//! (issue #310), and the Windows Service Control Manager (issue #311).
//! Each of the two follow-ups landed the same way: replace one arm, add
//! sibling modules, change nothing else in this file. A fourth platform
//! follows the same shape. Keep the [`ServiceBackend`] method
//! signatures and the exit codes below untouched: they are the
//! cross-platform contract the CLI documents.
//!
//! # Exit codes
//!
//! * `0` — the action succeeded. For `status`, the service is running.
//! * `1` — the action failed (needs elevation, unsupported platform,
//!   package-managed binary, supervisor command failed, I/O error).
//! * `3` — `status` only: the service is installed but stopped, or not
//!   installed at all. Mirrors `systemctl is-active`.

// Linux, macOS, and Windows each dispatch a real backend, so all three
// keep dead-code detection fully active and the blanket allow applies
// only to a platform with no backend at all, where the shared plumbing
// the backends consume (the error variants, the elevation probe,
// `SERVICE_NAME`) genuinely has no caller. The Windows-only modules
// were written against an active lint and re-assert it for themselves
// with an inner `#![warn(dead_code)]`; those inner attributes are now
// redundant but harmless, and they keep the modules honest if this
// predicate ever widens again.
#![cfg_attr(
    not(any(target_os = "linux", target_os = "macos", target_os = "windows")),
    allow(dead_code)
)]

use std::path::PathBuf;

use crate::cli::ServiceAction;

pub mod detect;

// Each backend compiles on its own platform, and under `cfg(test)`
// everywhere else so its pure logic (definition rendering, supervisor
// output parsing, path layout) stays covered on every developer machine
// and in CI. Neither is ever dispatched off its own platform.
//
// The `allow(dead_code)` companions are the per-target blindness guard:
// this crate compiles the module tree twice, and in the binary target a
// `pub` item is live only if it is reachable. A backend that the local
// `backend()` arm does not return therefore looks dead there even
// though it is exercised by the library target's tests.
#[cfg(any(target_os = "linux", test))]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub mod systemd;
#[cfg(any(target_os = "linux", test))]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub mod template;

#[cfg(any(target_os = "macos", test))]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub mod launchctl;
#[cfg(any(target_os = "macos", test))]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub mod launchd;
#[cfg(any(target_os = "macos", test))]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub mod plist;

// The Windows SCM backend (issue #311). Same split as the systemd
// backend: `scm` holds everything that can be decided without touching
// the Service Control Manager and therefore compiles (and is tested)
// everywhere, while the three modules below are handle plumbing over
// `windows-service` and exist only on Windows.
//
// Nothing in CI compiles the three Windows-only modules today: the test
// job runs on Linux, and `cargo check --target x86_64-pc-windows-msvc`
// on this crate dies in `zstd-sys`, whose build script needs a C
// toolchain with Windows headers. If you change them, check them the
// way they were written: an isolated probe crate that `#[path]`-includes
// the real `service_cmd`, `common`, `cli`, `cli_service`, and
// `utils::command` sources, stubs `crate::api` and `crate::device` (the
// two that drag in the device readers), and depends only on
// `windows-service`, `tracing-appender`, and the pure-Rust crates those
// modules need. Give it both a library and a binary target, because
// this crate compiles its module tree twice and a `pub` item that is
// always live in a library target can be dead behind a binary's private
// module root. Then run:
//
//     cargo clippy --target x86_64-pc-windows-msvc --lib --bins --tests -- -D warnings
//
// Verify the probe actually reaches the files before trusting it: paste
// a deliberate type error into each and confirm it fails.
//
// `scm` carries the same `allow(dead_code)` companion as the systemd,
// template, and launchd modules above, for the same reason: off Windows
// it is compiled only under `cfg(test)`, where `pub` no longer implies
// reachable, and several of its constants are consumed solely by the
// Windows-only adapters. Its own inner `#![cfg_attr(windows,
// warn(dead_code))]` keeps the lint live where the callers exist.
#[cfg(any(windows, test))]
#[cfg_attr(not(windows), allow(dead_code))]
pub mod scm;
#[cfg(windows)]
pub mod scm_backend;
#[cfg(windows)]
pub mod scm_host;
#[cfg(windows)]
pub mod scm_log;

/// The service identifier every backend is named after: the systemd
/// unit name minus its suffix, the trailing component of the launchd
/// label, the SCM service name.
///
/// The systemd and SCM backends both pass it to their supervisor
/// verbatim; launchd needs a reverse-DNS label, so [`plist::LABEL`]
/// spells the whole thing out instead. That makes this constant
/// genuinely uncalled in a macOS **binary** build, where reachability
/// rather than `pub` decides liveness, even though the library target
/// and the tests both use it.
#[cfg_attr(not(any(target_os = "linux", target_os = "windows")), allow(dead_code))]
pub const SERVICE_NAME: &str = "all-smi";

/// Exit code for a successful action, and for `status` when running.
pub const EXIT_OK: i32 = 0;
/// Exit code for any failure.
pub const EXIT_ERROR: i32 = 1;
/// Exit code for `status` when the service is not running.
pub const EXIT_NOT_RUNNING: i32 = 3;

/// Which supervisor instance an action targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scope {
    /// The machine-wide supervisor. Every mutating action requires root.
    System,
    /// The invoking user's own supervisor. No elevation required, and
    /// no boot persistence without an explicit opt-in per platform.
    User,
}

impl Scope {
    /// Map the CLI `--user` flag onto a scope. System is the default.
    pub fn from_user_flag(user: bool) -> Self {
        if user { Self::User } else { Self::System }
    }

    /// Stable lowercase name used in JSON output and diagnostics.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::User => "user",
        }
    }
}

impl std::fmt::Display for Scope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Everything a backend needs to materialise a service definition.
///
/// Deliberately carries no port, interval, or socket field: runtime
/// configuration lives in the environment file and the TOML config, so a
/// settings change never regenerates the service definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallSpec {
    pub scope: Scope,
    /// Account to run as. `None` means the platform default, which is
    /// root for system scope and the invoking user for user scope.
    pub service_user: Option<String>,
    /// Start the service immediately in addition to enabling it.
    pub start_now: bool,
    /// Overwrite a definition this tool did not write.
    pub force: bool,
}

/// Observed state of the service in one scope.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ServiceStatus {
    /// A service definition exists in this scope.
    pub installed: bool,
    /// Whether it starts at boot. `None` when the platform cannot say.
    pub enabled: Option<bool>,
    /// The service is running right now.
    pub running: bool,
    /// Main process id when running.
    pub pid: Option<u32>,
    /// Short human-readable state, e.g. `active (running)`.
    pub detail: String,
}

/// Failure modes shared by every backend.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ServiceError {
    /// The action needs root or Administrator and the caller has neither.
    #[error("{0}")]
    NeedsElevation(String),
    /// No supervisor backend applies to this platform or host.
    #[error("{0}")]
    NotSupported(String),
    /// A package manager owns this binary and ships its own service
    /// definition; installing a second one would fight it.
    #[error("{0}")]
    PackageManaged(String),
    /// The action would clobber or remove a service definition this tool
    /// did not write, or the request cannot be expressed as one.
    ///
    /// Added beyond the variant list in issue #309: the managed-by
    /// marker guard needs a distinct failure mode, and folding it into
    /// [`ServiceError::PackageManaged`] would produce a misleading
    /// "use your package manager instead" hint.
    #[error("{0}")]
    Conflict(String),
    /// No service definition exists in the requested scope.
    #[error("the all-smi service is not installed in this scope")]
    NotInstalled,
    /// Filesystem failure while reading or writing a service definition.
    #[error("{0}")]
    Io(#[from] std::io::Error),
    /// A supervisor command exited non-zero.
    #[error("`{cmd}` failed: {stderr}")]
    CommandFailed { cmd: String, stderr: String },
}

/// Platform contract implemented once per supervisor.
///
/// Implementations must be idempotent: re-running `install` over a
/// definition carrying the managed-by marker updates it in place, and
/// `stop` on an already-stopped service succeeds.
pub trait ServiceBackend {
    /// Write the service definition and register it for boot.
    fn install(&self, spec: &InstallSpec) -> Result<(), ServiceError>;
    /// Stop, deregister, and remove the service definition. Refuses a
    /// definition that lacks the managed-by marker; see
    /// [`ServiceBackend::uninstall_forced`].
    fn uninstall(&self, scope: Scope) -> Result<(), ServiceError>;
    fn start(&self, scope: Scope) -> Result<(), ServiceError>;
    fn stop(&self, scope: Scope) -> Result<(), ServiceError>;
    fn restart(&self, scope: Scope) -> Result<(), ServiceError>;
    fn status(&self, scope: Scope) -> Result<ServiceStatus, ServiceError>;

    /// `uninstall` that also removes a definition lacking the
    /// managed-by marker, backing the CLI's `--force` flag.
    ///
    /// The default delegates to [`ServiceBackend::uninstall`], which is
    /// correct for any backend that does not stamp a marker. Backends
    /// that do stamp one override this method.
    fn uninstall_forced(&self, scope: Scope) -> Result<(), ServiceError> {
        self.uninstall(scope)
    }
}

/// Select the backend for the current platform.
///
/// Each arm is independent so a follow-up issue can replace exactly one
/// without touching the others.
pub fn backend() -> Result<Box<dyn ServiceBackend>, ServiceError> {
    #[cfg(target_os = "linux")]
    {
        Ok(Box::new(systemd::SystemdBackend::new()))
    }

    #[cfg(target_os = "macos")]
    {
        Ok(Box::new(launchd::LaunchdBackend::new()))
    }

    #[cfg(target_os = "windows")]
    {
        Ok(Box::new(scm_backend::ScmBackend::new()))
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        Err(ServiceError::NotSupported(
            "`all-smi service` supports Linux (systemd) only on this build; adapt \
             packaging/systemd/all-smi.service from the all-smi source tree to your \
             init system."
                .to_string(),
        ))
    }
}

/// Refuse a privileged action when the caller is not elevated.
///
/// Shared by every backend so the wording stays identical across
/// platforms. `verb` is the subcommand name, e.g. `install`.
///
/// The Windows backend deliberately does not route through here (see
/// [`is_elevated`]), so in a Windows **binary** build this function has
/// no caller and reachability rather than `pub` decides liveness. The
/// allow is scoped to exactly that, rather than blanket-allowing dead
/// code across the whole non-Linux tree.
#[cfg_attr(windows, allow(dead_code))]
pub fn require_elevation(verb: &str) -> Result<(), ServiceError> {
    if is_elevated() {
        return Ok(());
    }
    Err(ServiceError::NeedsElevation(format!(
        "service {verb} requires root; re-run with sudo, or pass --user for a per-user service"
    )))
}

#[cfg(unix)]
fn is_elevated() -> bool {
    // SAFETY: `geteuid` takes no arguments, reads no caller-provided
    // memory, and is documented as always succeeding.
    unsafe { libc::geteuid() == 0 }
}

#[cfg(not(unix))]
#[cfg_attr(windows, allow(dead_code))]
fn is_elevated() -> bool {
    // The Windows SCM backend does not route through here. It lets the
    // Service Control Manager answer the question by opening exactly the
    // handles each action needs and translating `ERROR_ACCESS_DENIED`
    // through `scm::map_os_error`, which tests the capability actually
    // required rather than a proxy for it. Any other non-Unix platform
    // has no backend at all, so "not elevated" stays the safe answer.
    false
}

/// Handle `service run`, the Windows Service Control Manager entry
/// point (issue #311).
///
/// Dispatched before [`backend`] because it is the process side of the
/// service rather than a management action: it never opens the SCM to
/// manage a service, it *is* the service.
fn run_service_host() -> i32 {
    #[cfg(windows)]
    {
        scm_host::run()
    }

    #[cfg(not(windows))]
    {
        report(&ServiceError::NotSupported(
            "`all-smi service run` is the Windows Service Control Manager entry point and has no \
             meaning on this platform. Use `all-smi api` to serve metrics in the foreground, or \
             `all-smi service install` to register a supervised service."
                .to_string(),
        ))
    }
}

/// Resolve the absolute path of the running binary for embedding into a
/// service definition. Falls back to the unresolved path when the
/// filesystem cannot canonicalize it (for example a deleted-and-replaced
/// binary on a filesystem without `/proc`).
pub fn current_exe_canonical() -> Result<PathBuf, ServiceError> {
    let exe = std::env::current_exe()?;
    Ok(exe.canonicalize().unwrap_or(exe))
}

/// CLI entry point. Returns the process exit code; see the module-level
/// "Exit codes" section.
pub fn run(action: &ServiceAction) -> i32 {
    // `run` is the process side of the service, not a management
    // action, so it is dispatched before a backend is selected.
    if let ServiceAction::Run(_) = action {
        return run_service_host();
    }

    let scope = Scope::from_user_flag(action.user_scope());
    let backend = match backend() {
        Ok(b) => b,
        Err(e) => return report(&e),
    };

    match action {
        ServiceAction::Install(args) => {
            // The package-manager refusal lives here, not in the
            // backends, so every platform inherits it unchanged.
            if let Err(e) = detect::guard(args.force) {
                return report(&e);
            }
            if scope == Scope::User && args.service_user.is_some() {
                eprintln!(
                    "warning: --service-user is ignored in --user scope; a user service \
                     always runs as the invoking user"
                );
            }
            let spec = InstallSpec {
                scope,
                service_user: args.service_user.clone(),
                start_now: args.now,
                force: args.force,
            };
            match backend.install(&spec) {
                Ok(()) => {
                    report_install_success(&spec);
                    EXIT_OK
                }
                Err(e) => report(&e),
            }
        }
        ServiceAction::Uninstall(args) => {
            let result = if args.force {
                backend.uninstall_forced(scope)
            } else {
                backend.uninstall(scope)
            };
            match result {
                Ok(()) => {
                    println!("Removed the all-smi {scope} service.");
                    EXIT_OK
                }
                Err(e) => report(&e),
            }
        }
        ServiceAction::Start(_) => simple(backend.start(scope), scope, "Started"),
        ServiceAction::Stop(_) => simple(backend.stop(scope), scope, "Stopped"),
        ServiceAction::Restart(_) => simple(backend.restart(scope), scope, "Restarted"),
        ServiceAction::Status(args) => match backend.status(scope) {
            Ok(status) => {
                if args.json {
                    print_status_json(&status, scope);
                } else {
                    print_status_text(&status, scope);
                }
                if status.running {
                    EXIT_OK
                } else {
                    EXIT_NOT_RUNNING
                }
            }
            Err(e) => report(&e),
        },
        // Dispatched above, before a backend was selected. Kept so the
        // match stays exhaustive.
        ServiceAction::Run(_) => unreachable!("service run is dispatched before backend selection"),
    }
}

fn simple(result: Result<(), ServiceError>, scope: Scope, past_tense: &str) -> i32 {
    match result {
        Ok(()) => {
            println!("{past_tense} the all-smi {scope} service.");
            EXIT_OK
        }
        Err(e) => report(&e),
    }
}

fn report_install_success(spec: &InstallSpec) {
    let scope = spec.scope;
    println!("Installed the all-smi {scope} service and enabled it.");
    if spec.start_now {
        println!("It is running now.");
    } else {
        let flag = if scope == Scope::User { " --user" } else { "" };
        println!("It is not running yet. Start it with: all-smi service start{flag}");
    }
    if scope == Scope::User {
        println!("{}", user_scope_persistence_note());
    }
    println!(
        "Runtime settings live in {SETTINGS_SOURCES}, not in the service definition. Run \
         `all-smi config path` to see the active TOML path."
    );
}

/// Where an operator changes a running service's settings. launchd has
/// no `EnvironmentFile=` equivalent, so on macOS the TOML config is the
/// whole story.
#[cfg(target_os = "macos")]
const SETTINGS_SOURCES: &str = "the TOML config";
#[cfg(not(target_os = "macos"))]
const SETTINGS_SOURCES: &str = "the environment file and the TOML config";

/// Platform-specific caveat printed after a user-scope install. Every
/// platform bounds a per-user service to a login session, but each one
/// spells the escape hatch differently.
#[cfg(target_os = "macos")]
fn user_scope_persistence_note() -> String {
    "Note: a LaunchAgent runs only while you are logged in to a desktop session and stops at \
     logout. launchd has no per-user lingering, so boot persistence on a headless node means the \
     system LaunchDaemon: `sudo all-smi service install --now`."
        .to_string()
}

#[cfg(not(target_os = "macos"))]
fn user_scope_persistence_note() -> String {
    let user = whoami::username().unwrap_or_else(|_| "<user>".to_string());
    format!(
        "Note: a user service only runs while you are logged in. Run \
         `loginctl enable-linger {user}` for boot persistence."
    )
}

fn print_status_text(status: &ServiceStatus, scope: Scope) {
    if !status.installed {
        println!("all-smi ({scope} scope): not installed");
        return;
    }
    let enabled = match status.enabled {
        Some(true) => "enabled",
        Some(false) => "disabled",
        None => "enablement unknown",
    };
    let running = if status.running { "running" } else { "stopped" };
    println!("all-smi ({scope} scope): installed, {enabled}, {running}");
    if !status.detail.is_empty() {
        println!("  state: {}", status.detail);
    }
    if let Some(pid) = status.pid {
        println!("  main pid: {pid}");
    }
}

fn print_status_json(status: &ServiceStatus, scope: Scope) {
    let value = serde_json::json!({
        "installed": status.installed,
        "enabled": status.enabled,
        "running": status.running,
        "pid": status.pid,
        "scope": scope.as_str(),
        "detail": status.detail,
    });
    match serde_json::to_string_pretty(&value) {
        Ok(s) => println!("{s}"),
        // Serializing a closed set of bools, integers, and strings
        // cannot fail; fall back to the text form rather than panicking.
        Err(_) => print_status_text(status, scope),
    }
}

/// Print an error and map it onto the process exit code.
fn report(err: &ServiceError) -> i32 {
    eprintln!("error: {err}");
    if let ServiceError::PackageManaged(_) = err {
        eprintln!("hint: pass --force to install alongside the package-managed definition anyway");
    }
    EXIT_ERROR
}

// Test module lives in `mod_tests.rs` to keep this file under the
// 500-line soft limit.
#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
