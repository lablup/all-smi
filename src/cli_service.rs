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

//! Argument definitions for the `all-smi service` subcommand (issue
//! #309). Re-exported from `cli` for ergonomic `use crate::cli::...`
//! call sites; split here so the main CLI file stays below the
//! 500-line soft limit, mirroring [`crate::cli_config`].
//!
//! # Stability contract
//!
//! The subcommand names, the flag spellings, the scope semantics, and
//! the exit codes defined here are a cross-platform contract. The Linux
//! systemd backend is the first implementation; the macOS launchd
//! backend (issue #310) and the Windows SCM backend (issue #311) plug
//! into the same surface and must not change any of it. Adding a new
//! flag is additive and allowed; renaming or removing one is not.

use clap::{Args, Subcommand};

/// Arguments for the `all-smi service` subcommand (issue #309).
#[derive(Args, Clone, Debug)]
pub struct ServiceArgs {
    #[command(subcommand)]
    pub action: ServiceAction,
}

/// `service` sub-subcommands.
///
/// Every action accepts `--user` to select the per-user scope instead
/// of the system scope. The system scope is the default and requires
/// root for every mutating action.
#[derive(Subcommand, Clone, Debug)]
pub enum ServiceAction {
    /// Install the service definition and enable it at boot.
    ///
    /// Runtime configuration is deliberately not expressible here: port,
    /// interval, socket path, and the rest live in the environment file
    /// (`/etc/default/all-smi` on Linux) or the TOML config file, so a
    /// settings change never requires regenerating the service
    /// definition. Run `all-smi config path` to see the active TOML
    /// path.
    Install(ServiceInstallArgs),
    /// Stop, disable, and remove the service definition.
    Uninstall(ServiceUninstallArgs),
    /// Start the installed service now.
    Start(ServiceScopeArgs),
    /// Stop the running service.
    Stop(ServiceScopeArgs),
    /// Restart the service (stop then start).
    Restart(ServiceScopeArgs),
    /// Report whether the service is installed, enabled, and running.
    ///
    /// Exits 0 when the service is running and 3 when it is installed
    /// but stopped or not installed at all, mirroring the `systemctl
    /// is-active` convention.
    Status(ServiceStatusArgs),
    /// Service Control Manager entry point (Windows only, issue #311).
    ///
    /// Hidden from `--help`: this is not something an operator runs. The
    /// Windows SCM starts the registered binary with these arguments and
    /// expects the process to call `StartServiceCtrlDispatcher` within
    /// about 30 seconds. Run from an ordinary console it fails with an
    /// explanation rather than starting a server, because there is no
    /// dispatcher to connect to. Use `all-smi api` for that.
    ///
    /// The variant exists on every platform so the argument surface does
    /// not change shape per target; non-Windows builds answer with the
    /// standard "not supported" error.
    #[command(hide = true)]
    Run(ServiceRunArgs),
}

impl ServiceAction {
    /// Whether the caller asked for the per-user scope.
    ///
    /// Every action carries its own `--user` flag rather than hoisting
    /// it to the `service` group, so `all-smi service status --user`
    /// reads naturally and matches `systemctl --user` muscle memory.
    pub fn user_scope(&self) -> bool {
        match self {
            Self::Install(a) => a.user,
            Self::Uninstall(a) => a.user,
            Self::Start(a) | Self::Stop(a) | Self::Restart(a) => a.user,
            Self::Status(a) => a.user,
            // The supervisor decides which scope it launched us in;
            // there is nothing for the process itself to select.
            Self::Run(_) => false,
        }
    }
}

/// `service run` takes no arguments today. It exists as a struct rather
/// than a unit variant so a future flag (a log-level override, say) is a
/// purely additive change.
#[derive(Args, Clone, Debug)]
pub struct ServiceRunArgs {}

#[derive(Args, Clone, Debug)]
pub struct ServiceInstallArgs {
    /// Install a per-user service instead of a system-wide one. No root
    /// required. On Linux the unit lands in
    /// `$XDG_CONFIG_HOME/systemd/user/all-smi.service` and is managed
    /// with `systemctl --user`; boot persistence additionally requires
    /// `loginctl enable-linger <user>`.
    #[arg(long)]
    pub user: bool,

    /// Run the service as this account instead of root.
    ///
    /// Sets the service definition's user and group fields. The default
    /// for subcommand installs is root, because vendor CLIs (hl-smi,
    /// rbln-stat, furiosa-smi, tegrastats) have varying permission
    /// requirements and a wrong guess yields a silently empty metrics
    /// page. The Debian package takes the opposite default and ships a
    /// dedicated `all-smi` system account. Prefer a dedicated account
    /// wherever your vendor stack permits it. Ignored in `--user` scope.
    #[arg(long, value_name = "NAME")]
    pub service_user: Option<String>,

    /// Start the service immediately after installing it, in addition
    /// to enabling it at boot.
    #[arg(long)]
    pub now: bool,

    /// Proceed even when the install would be unsafe: a package manager
    /// already owns this binary and ships its own service definition, or
    /// an existing service definition was not written by `all-smi
    /// service` (it lacks the managed-by marker) and would be
    /// overwritten.
    #[arg(long)]
    pub force: bool,
}

#[derive(Args, Clone, Debug)]
pub struct ServiceUninstallArgs {
    /// Operate on the per-user service instead of the system-wide one.
    #[arg(long)]
    pub user: bool,

    /// Remove a service definition that lacks the managed-by marker.
    /// Without this flag `uninstall` refuses to touch a definition it
    /// did not write.
    #[arg(long)]
    pub force: bool,
}

/// Shared shape for the actions whose only knob is the scope.
#[derive(Args, Clone, Debug)]
pub struct ServiceScopeArgs {
    /// Operate on the per-user service instead of the system-wide one.
    #[arg(long)]
    pub user: bool,
}

#[derive(Args, Clone, Debug)]
pub struct ServiceStatusArgs {
    /// Report on the per-user service instead of the system-wide one.
    #[arg(long)]
    pub user: bool,

    /// Emit a machine-readable JSON object instead of the
    /// human-readable summary. Schema: `{ "installed": bool, "enabled":
    /// bool|null, "running": bool, "pid": number|null, "scope":
    /// "system"|"user", "detail": string }`.
    #[arg(long)]
    pub json: bool,
}
