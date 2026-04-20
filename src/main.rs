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

mod api;
mod app_state;
mod cli;
mod cli_config;
mod common;
mod config_cmd;
mod device;
mod doctor;
#[macro_use]
mod parsing;
mod metrics;
mod network;
mod record;
mod snapshot;
mod storage;
mod ui;
mod utils;
mod view;

use api::run_api_mode;
use clap::Parser;
use cli::{Cli, Commands, LocalArgs};
use common::config_file::{self, Settings, SocketSetting};
use tokio::signal;
use utils::{RuntimeEnvironment, ensure_sudo_permissions_for_api};

// Sudo permission functions only needed on non-macOS platforms
#[cfg(not(target_os = "macos"))]
use utils::{ensure_sudo_permissions, ensure_sudo_permissions_with_fallback};

#[cfg(target_os = "macos")]
use device::is_apple_silicon;

// Use native macOS APIs (no sudo required)
#[cfg(target_os = "macos")]
use device::macos_native::{initialize_native_metrics_manager, shutdown_native_metrics_manager};

#[cfg(target_os = "linux")]
use device::hlsmi::{initialize_hlsmi_manager, shutdown_hlsmi_manager};
#[cfg(target_os = "linux")]
use device::platform_detection::has_gaudi;

#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::sync::atomic::AtomicBool;

#[cfg(target_os = "macos")]
static NATIVE_METRICS_INITIALIZED: AtomicBool = AtomicBool::new(false);

#[cfg(target_os = "linux")]
static HLSMI_INITIALIZED: AtomicBool = AtomicBool::new(false);

fn main() {
    // Set up panic handler for cleanup
    #[cfg(target_os = "macos")]
    setup_panic_handler();

    let cli = Cli::parse();

    // Handle the `config` subcommand synchronously — it never starts a
    // Tokio runtime and its I/O is purely local filesystem work.
    if let Some(Commands::Config(ref cfg_args)) = cli.command {
        let code = config_cmd::run(cli.config.as_deref(), &cfg_args.action);
        std::process::exit(code);
    }

    // Load the merged TOML + env settings now so every downstream mode
    // entry point can consume them. A malformed or missing explicit
    // `--config` file is a hard error and short-circuits startup.
    let settings = match config_file::load(cli.config.as_deref()) {
        Ok(outcome) => {
            for w in &outcome.warnings {
                eprintln!("warning: {w}");
            }
            // Surface unknown-key warnings at boot so operators learn
            // about typos (`[alarts]` instead of `[alerts]`) without
            // having to run `config print` separately. The keys are
            // already escape-sanitised at parse time, so printing them
            // cannot inject control sequences into the terminal.
            for k in &outcome.settings.unknown_keys {
                eprintln!("warning: unknown config key `{k}` (forward-compat — preserved)");
            }
            outcome.settings
        }
        Err(e) => {
            eprintln!("error: {e}");
            // `2` matches `config validate` semantics for consistency
            // with the issue spec: malformed config is an actionable
            // user error, not a crash.
            std::process::exit(2);
        }
    };

    // The snapshot subcommand is one-shot, scriptable, and may call into
    // potentially-hung hardware readers via `spawn_blocking`. Because
    // `spawn_blocking` cannot cancel the underlying OS thread on a
    // `tokio::time::timeout` firing, a hung NVML/TPU driver call would
    // permanently leak a Tokio blocking-pool worker if we reused the
    // long-running default runtime. We therefore build a dedicated
    // runtime with a conservative `max_blocking_threads(32)` specifically
    // for the snapshot invocation — the runtime drops when the function
    // returns and any still-running blocking threads exit with the
    // process. This bounds the per-invocation leak to at most 32
    // threads.
    if let Some(Commands::Snapshot(_)) = &cli.command {
        let runtime = match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .worker_threads(4)
            .max_blocking_threads(32)
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                eprintln!("error: failed to build snapshot tokio runtime: {e}");
                std::process::exit(1);
            }
        };
        runtime.block_on(async move {
            run_command(cli, settings).await;
        });
        return;
    }

    // Default runtime for `api`, `local`, `view`, and no-subcommand paths.
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(4)
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: failed to build tokio runtime: {e}");
            std::process::exit(1);
        }
    };
    runtime.block_on(async move {
        run_command(cli, settings).await;
    });
}

async fn run_command(cli: Cli, settings: Settings) {
    // Signal-handling policy by subcommand:
    //
    // * `Record` installs its own SIGINT/SIGTERM handlers (see
    //   `record::install_signal_handlers`) that set a cooperative stop
    //   flag. The record loop polls that flag, finishes the in-flight
    //   frame, and calls `RotatingWriter::finish()` to flush the zstd /
    //   gzip trailer before returning. The unconditional
    //   `std::process::exit(0)` handlers below would race with that
    //   shutdown path and truncate the output file to zero bytes
    //   (issue #187 acceptance: "SIGTERM during recording closes
    //   cleanly with a complete final JSON line"). Skip them for
    //   `Record` so the cooperative path wins.
    //
    // * Every other subcommand keeps the original behaviour — no device
    //   manager does partial-state flushing, so an immediate exit on
    //   signal is the desired shutdown semantics.
    let is_record = matches!(cli.command, Some(Commands::Record(_)));
    if !is_record {
        // Set up signal handler for clean shutdown
        tokio::spawn(async {
            signal::ctrl_c().await.expect("Failed to listen for Ctrl+C");
            #[cfg(target_os = "macos")]
            {
                // Cleanup native metrics manager on signal
                shutdown_native_metrics_manager();
            }
            #[cfg(target_os = "linux")]
            {
                // Always cleanup hlsmi on signal
                shutdown_hlsmi_manager();
            }
            std::process::exit(0);
        });

        // Also handle SIGTERM on Unix systems
        #[cfg(unix)]
        tokio::spawn(async {
            let mut sigterm = signal::unix::signal(signal::unix::SignalKind::terminate())
                .expect("Failed to listen for SIGTERM");
            sigterm.recv().await;
            #[cfg(target_os = "macos")]
            {
                // Cleanup native metrics manager on signal
                shutdown_native_metrics_manager();
            }
            #[cfg(target_os = "linux")]
            {
                // Always cleanup hlsmi on signal
                shutdown_hlsmi_manager();
            }
            std::process::exit(0);
        });
    }

    match cli.command {
        Some(Commands::Config(_)) => {
            // Already handled synchronously in `main` before the runtime
            // was built. This branch is unreachable in practice; keep it
            // so the match is exhaustive.
            unreachable!("config subcommand is dispatched before runtime");
        }
        Some(Commands::Api(mut args)) => {
            // When using native macOS APIs, no sudo is needed
            #[cfg(target_os = "macos")]
            let _ = ensure_sudo_permissions_for_api(); // Just for any other checks

            #[cfg(not(target_os = "macos"))]
            let _has_sudo = ensure_sudo_permissions_for_api();

            // Resolve every CLI field via the precedence chain (CLI >
            // env > file > default). Fields that were `None` on the CLI
            // take their value from the merged `Settings`.
            if args.port.is_none() {
                args.port = Some(settings.api.port);
            }
            if args.interval.is_none() {
                args.interval = Some(settings.api.interval_secs);
            }
            // `Option<bool>` encodes three states: explicit true /
            // explicit false / not provided. Only fall back to the
            // merged settings when the CLI left it as `None`; once
            // resolved, downstream consumers always see a bare `bool`.
            if args.processes.is_none() {
                args.processes = Some(settings.api.processes);
            }
            #[cfg(unix)]
            {
                if args.socket.is_none() {
                    args.socket = match &settings.api.socket {
                        SocketSetting::Unset | SocketSetting::Bool(false) => None,
                        SocketSetting::Bool(true) => Some(String::new()),
                        SocketSetting::Path(p) => Some(p.clone()),
                    };
                }
            }

            let interval = args.interval.unwrap_or(3);

            // Initialize native metrics manager (no sudo required)
            #[cfg(target_os = "macos")]
            if is_apple_silicon() {
                if let Err(e) = initialize_native_metrics_manager(interval * 1000) {
                    eprintln!("Warning: Failed to initialize native metrics manager: {e}");
                } else {
                    use std::sync::atomic::Ordering;
                    NATIVE_METRICS_INITIALIZED.store(true, Ordering::Relaxed);
                }
            }

            // Initialize hlsmi manager for Intel Gaudi on Linux
            #[cfg(target_os = "linux")]
            if has_gaudi() {
                match initialize_hlsmi_manager(interval) {
                    Err(e) => {
                        eprintln!("Warning: Failed to initialize hlsmi manager: {e}");
                    }
                    _ => {
                        use std::sync::atomic::Ordering;
                        HLSMI_INITIALIZED.store(true, Ordering::Relaxed);
                    }
                }
            }

            run_api_mode(&args, &settings).await;
        }
        Some(Commands::Local(mut args)) => {
            // On non-macOS platforms, require sudo
            #[cfg(not(target_os = "macos"))]
            ensure_sudo_permissions();

            // Precedence: CLI > settings (env > file) > default.
            if args.interval.is_none() {
                args.interval = settings.local.interval_secs;
            }

            // Initialize native metrics manager (no sudo required)
            #[cfg(target_os = "macos")]
            if is_apple_silicon() {
                let interval = args.interval.unwrap_or(2);
                if let Err(e) = initialize_native_metrics_manager(interval * 1000) {
                    eprintln!("Warning: Failed to initialize native metrics manager: {e}");
                } else {
                    use std::sync::atomic::Ordering;
                    NATIVE_METRICS_INITIALIZED.store(true, Ordering::Relaxed);
                }
            }

            // Initialize hlsmi manager for Intel Gaudi on Linux
            #[cfg(target_os = "linux")]
            if has_gaudi() {
                let interval = args.interval.unwrap_or(2);
                std::thread::spawn(move || match initialize_hlsmi_manager(interval) {
                    Err(e) => {
                        eprintln!("Warning: Failed to initialize hlsmi manager: {e}");
                    }
                    _ => {
                        use std::sync::atomic::Ordering;
                        HLSMI_INITIALIZED.store(true, Ordering::Relaxed);
                    }
                });
            }

            view::run_local_mode(&args, &settings).await;
        }
        Some(Commands::Snapshot(args)) => {
            // Snapshot mode is one-shot and scriptable: DO NOT request sudo,
            // do not initialize long-lived managers (macOS native / hlsmi).
            // Readers that require sudo or specialised managers will gracefully
            // degrade — their failures surface as `errors` entries rather than
            // aborting the snapshot, per the issue spec.
            //
            // Merge `[snapshot]` config defaults on top of the CLI args
            // so `default_format` / `default_pretty` from `config.toml`
            // take effect when the operator does not pass `--format` /
            // `--pretty` explicitly.
            let options = match snapshot::SnapshotOptions::from_args_with_settings(
                &args,
                Some(&settings.snapshot),
            ) {
                Ok(o) => o,
                Err(e) => {
                    eprintln!("error: {e}");
                    std::process::exit(2);
                }
            };
            match snapshot::run(options).await {
                Ok(()) => std::process::exit(0),
                Err(e) => {
                    if e.downcast_ref::<snapshot::SnapshotHardFailure>().is_some() {
                        eprintln!("error: {e}");
                        std::process::exit(1);
                    }
                    eprintln!("error: {e:#}");
                    std::process::exit(1);
                }
            }
        }
        Some(Commands::Doctor(args)) => {
            // Doctor is a read-only diagnostic subcommand: no sudo, no
            // long-lived manager init. Every check is bounded by a hard
            // 3-second timeout enforced in the orchestrator.
            match doctor::run_cli(&args).await {
                Ok(code) => std::process::exit(code),
                Err(e) => {
                    eprintln!("error: {e:#}");
                    std::process::exit(1);
                }
            }
        }
        Some(Commands::Record(args)) => {
            // Record mode shares the snapshot collector stack, so like
            // `snapshot` it runs without sudo and without initializing the
            // macOS native metrics manager — hardware readers that need
            // those privileges degrade gracefully into the error list.
            //
            // Merge `[record]` config defaults on top of CLI args so
            // `output_dir` / `compress` from `config.toml` take effect
            // when the operator does not pass `-o` / `--compress`.
            let opts = match record::RecorderOptions::from_args_with_settings(
                &args,
                Some(&settings.record),
            ) {
                Ok(o) => o,
                Err(e) => {
                    eprintln!("error: {e}");
                    std::process::exit(2);
                }
            };
            match record::run(opts).await {
                Ok(()) => std::process::exit(0),
                Err(e) => {
                    eprintln!("error: {e:#}");
                    std::process::exit(1);
                }
            }
        }
        Some(Commands::View(mut args)) => {
            // Precedence: CLI > settings (env > file) > default.
            // `hosts`/`hostfile` fill from config only when CLI omitted
            // them; the Backend.AI detection later relies on this
            // ordering too.
            if args.hosts.is_none() && !settings.view.hosts.is_empty() {
                args.hosts = Some(settings.view.hosts.clone());
            }
            if args.hostfile.is_none() {
                args.hostfile = settings.view.hostfile.clone();
            }
            if args.interval.is_none() {
                args.interval = settings.view.interval_secs;
            }

            // Replay mode bypasses the remote scrape path entirely — it
            // reads frames from disk and pushes them into the same
            // AppState the live view renders. Hardware, sudo, and host
            // discovery are all irrelevant in this branch.
            if args.replay.is_some() {
                view::run_replay_mode(&args, &settings).await;
                return;
            }

            // Remote mode - no sudo required

            // Check if we're in Backend.AI environment and no hosts/hostfile provided
            if args.hosts.is_none() && args.hostfile.is_none() {
                let runtime_env = RuntimeEnvironment::detect();

                if let Some(backend_ai_hosts) = runtime_env.get_backend_ai_hosts() {
                    eprintln!("Detected Backend.AI environment");
                    eprintln!("Auto-discovered cluster hosts from BACKENDAI_CLUSTER_HOSTS:");
                    for host in &backend_ai_hosts {
                        eprintln!("  - {host}");
                    }
                    args.hosts = Some(backend_ai_hosts);
                } else {
                    eprintln!("Error: Remote view mode requires --hosts or --hostfile");
                    eprintln!(
                        "Usage: all-smi view --hosts <URL>... or all-smi view --hostfile <FILE>"
                    );
                    if runtime_env.is_backend_ai() {
                        eprintln!(
                            "\nBackend.AI environment detected but BACKENDAI_CLUSTER_HOSTS is not set."
                        );
                        eprintln!("Set the environment variable with comma-separated host names:");
                        eprintln!("  export BACKENDAI_CLUSTER_HOSTS=\"host1,host2\"");
                    }
                    eprintln!("\nFor local monitoring, use: all-smi local");
                    std::process::exit(1);
                }
            }
            view::run_view_mode(&args, &settings).await;

            // Cleanup after view mode exits
            #[cfg(target_os = "macos")]
            {
                // Cleanup native metrics manager
                shutdown_native_metrics_manager();
            }
            #[cfg(target_os = "linux")]
            {
                // Always try to shutdown hlsmi, even if not fully initialized
                shutdown_hlsmi_manager();
            }
        }
        None => {
            // Honour `[general].default_mode` from the config file:
            // when set to "view" or "api", redispatch through the
            // matching branch instead of defaulting to local. This
            // wires the documented-but-previously-orphaned option so
            // operators can declare their preferred entry point once
            // in `config.toml` and stop typing the subcommand.
            match settings.general.default_mode.as_str() {
                "view" => {
                    let view_args = cli::ViewArgs {
                        hosts: None,
                        hostfile: None,
                        interval: None,
                        alert_temp: None,
                        alert_util_low_mins: None,
                        replay: None,
                        speed: 1.0,
                        start: None,
                        replay_loop: false,
                    };
                    // Re-enter the View arm with synthetic args. A
                    // cleaner factoring is a shared helper, but the
                    // existing handler is only reachable through this
                    // match; splitting it out is a larger change than
                    // this PR wants.
                    return Box::pin(run_command(
                        Cli {
                            config: cli.config,
                            command: Some(Commands::View(view_args)),
                        },
                        settings,
                    ))
                    .await;
                }
                "api" => {
                    let api_args = cli::ApiArgs {
                        port: None,
                        interval: None,
                        processes: None,
                        #[cfg(unix)]
                        socket: None,
                    };
                    return Box::pin(run_command(
                        Cli {
                            config: cli.config,
                            command: Some(Commands::Api(api_args)),
                        },
                        settings,
                    ))
                    .await;
                }
                _ => {
                    // "local" — fall through to the original local
                    // default behaviour below.
                }
            }

            // Default to local mode when no command is specified
            // On macOS, no sudo is needed
            #[cfg(target_os = "macos")]
            let has_sudo = true; // Always proceed, no sudo needed

            #[cfg(not(target_os = "macos"))]
            let has_sudo = ensure_sudo_permissions_with_fallback();

            if has_sudo {
                // Initialize native metrics manager (no sudo required)
                #[cfg(target_os = "macos")]
                if is_apple_silicon() {
                    if let Err(e) = initialize_native_metrics_manager(2000) {
                        eprintln!("Warning: Failed to initialize native metrics manager: {e}");
                    } else {
                        use std::sync::atomic::Ordering;
                        NATIVE_METRICS_INITIALIZED.store(true, Ordering::Relaxed);
                    }
                }

                // Initialize hlsmi manager for Intel Gaudi on Linux
                #[cfg(target_os = "linux")]
                if has_gaudi() {
                    std::thread::spawn(|| match initialize_hlsmi_manager(2) {
                        Err(e) => {
                            eprintln!("Warning: Failed to initialize hlsmi manager: {e}");
                        }
                        _ => {
                            use std::sync::atomic::Ordering;
                            HLSMI_INITIALIZED.store(true, Ordering::Relaxed);
                        }
                    });
                }

                let local_args = LocalArgs {
                    interval: settings.local.interval_secs,
                    alert_temp: None,
                    alert_util_low_mins: None,
                };
                view::run_local_mode(&local_args, &settings).await;

                // Cleanup after local mode exits
                #[cfg(target_os = "macos")]
                {
                    // Cleanup native metrics manager
                    shutdown_native_metrics_manager();
                }
                #[cfg(target_os = "linux")]
                {
                    // Always try to shutdown hlsmi, even if not fully initialized
                    shutdown_hlsmi_manager();
                }
            }
            // If user declined sudo and chose remote monitoring,
            // they were given instructions and the function exits
        }
    }

    // Final cleanup - ensure all managers are terminated
    #[cfg(target_os = "macos")]
    {
        shutdown_native_metrics_manager();
    }
    #[cfg(target_os = "linux")]
    {
        shutdown_hlsmi_manager();
    }
}

// Set up a panic handler to ensure cleanup
#[cfg(target_os = "macos")]
fn setup_panic_handler() {
    let default_panic = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        // Cleanup native metrics manager before panicking
        device::macos_native::shutdown_native_metrics_manager();
        default_panic(panic_info);
    }));
}
