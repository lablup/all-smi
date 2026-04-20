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

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Mutex, Notify};

use crate::app_state::AppState;
use crate::cli::{LocalArgs, ViewArgs};
use crate::common::config::AlertConfig;
use crate::common::config_file::Settings;
use crate::ui::alerts::Alerter;
use crate::view::data_collection::{ReplayDriver, initial_replay_state};
use crate::view::{
    data_collector::DataCollector, terminal_manager::TerminalManager, ui_loop::UiLoop,
};

/// Build the final [`AlertConfig`] for the current invocation:
/// starts from the settings-provided AlertConfig (already merged from
/// defaults + file + env) and applies any explicit CLI overrides on
/// top. Keeping the helper here avoids duplicating the precedence wire
/// through each mode entry point.
fn build_alert_config(
    settings: &Settings,
    alert_temp: Option<u32>,
    alert_util_low_mins: Option<u32>,
) -> AlertConfig {
    settings
        .alerts
        .clone()
        .with_cli_overrides(alert_temp, alert_util_low_mins)
}

pub async fn run_local_mode(args: &LocalArgs, settings: &Settings) {
    let mut startup_profiler = crate::utils::StartupProfiler::new();
    startup_profiler.checkpoint("Starting run_local_mode");

    // Initialize application state for local mode.
    // `is_local_mode = true` means no --hosts / --hostfile were supplied.
    // The UI gates the Cluster Overview card, dashboard items, and tabs row
    // behind `!is_local_mode` (see src/view/frame_renderer.rs render_main).
    // Build the AppState with the merged energy config so the
    // integrator's `gap_interpolate_seconds` honours the TOML value.
    // The settings layer has already merged defaults + file + env.
    let mut initial_state = AppState::with_energy_config(&settings.energy);
    initial_state.is_local_mode = true;
    // Apply the CLI-supplied alert thresholds on top of the
    // settings-provided AlertConfig (which already merges defaults +
    // config file + env per issue #192's precedence chain).
    let alert_config = build_alert_config(settings, args.alert_temp, args.alert_util_low_mins);
    initial_state.alerter = Alerter::new(alert_config);
    // Propagate the `[display]` section so renderers consuming
    // `AppState.display_config` honour the operator's color_scheme /
    // gauge_style / show_led_grid choices. Defaults are equivalent to
    // the pre-config-file behaviour when no config file is loaded.
    initial_state.display_config = settings.display.clone();
    let app_state = Arc::new(Mutex::new(initial_state));
    startup_profiler.checkpoint("AppState initialized");

    // Create shared notification handle for collector -> UI wakeups
    let data_notify = Arc::new(Notify::new());

    // Initialize terminal
    let _terminal_manager = match TerminalManager::new() {
        Ok(manager) => manager,
        Err(e) => {
            eprintln!("Failed to initialize terminal: {e}");
            return;
        }
    };
    startup_profiler.checkpoint("Terminal initialized");

    // Start data collection in background with notification handle
    let data_collector =
        DataCollector::with_notify(Arc::clone(&app_state), Arc::clone(&data_notify));
    let view_args = ViewArgs {
        interval: args.interval,
        alert_temp: args.alert_temp,
        alert_util_low_mins: args.alert_util_low_mins,
        ..ViewArgs::empty()
    };
    tokio::spawn(async move {
        data_collector.run_local_mode(view_args).await;
    });
    startup_profiler.checkpoint("Data collector spawned");

    // Run UI loop with the same notification handle
    let mut ui_loop = match UiLoop::new(app_state, data_notify) {
        Ok(ui_loop) => ui_loop,
        Err(e) => {
            eprintln!("Failed to initialize UI: {e}");
            return;
        }
    };
    startup_profiler.checkpoint("UI loop initialized");
    startup_profiler.finish();

    // Create ViewArgs again for UI loop
    let view_args = ViewArgs {
        interval: args.interval,
        alert_temp: args.alert_temp,
        alert_util_low_mins: args.alert_util_low_mins,
        ..ViewArgs::empty()
    };
    if let Err(e) = ui_loop.run(&view_args).await {
        eprintln!("UI loop error: {e}");
    }

    // Terminal cleanup is handled by TerminalManager's Drop trait
}

/// Enter the TUI in `--replay` mode. Instead of collecting live data we
/// stream frames from the given NDJSON file and push them into the same
/// `AppState` the live view renders from.
///
/// The UI renders the REPLAY status bar (see `ui::chrome::print_replay_bar`)
/// and the event handler accepts SPACE/`]`/`[`/`+`/`-`/`j`/`k`/`g`/`L`
/// while `AppState::replay` is `Some`. Filter-edit mode still takes
/// precedence over replay keys per the event handler's mode ladder.
pub async fn run_replay_mode(args: &ViewArgs, settings: &Settings) {
    let replay_path = match args.replay.as_ref() {
        Some(p) => p.clone(),
        None => {
            eprintln!("error: --replay requires a file path");
            return;
        }
    };

    // Open the replay file BEFORE entering the alternate screen so any
    // errors surface as normal stderr instead of being hidden behind the
    // TUI's background.
    let mut driver = match ReplayDriver::open(replay_path.clone()) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: {e}");
            return;
        }
    };

    // Seed app state: treat replay as "remote-ish" (not is_local_mode)
    // so the tab row renders from the hostnames embedded in the stream.
    let mut initial_state = AppState::with_energy_config(&settings.energy);
    initial_state.is_local_mode = false;
    initial_state.loading = false;
    // Replay mode still honours config-file alert thresholds (the
    // thresholds are cosmetic — they only drive transition events
    // against recorded frames) but ignores --alert-temp / --alert-util
    // which don't semantically apply to historical data.
    initial_state.alerter = Alerter::new(settings.alerts.clone());
    initial_state.display_config = settings.display.clone();
    // Propagate the `[display]` section so renderers consuming
    // `AppState.display_config` honour the operator's color_scheme /
    // gauge_style / show_led_grid choices. Defaults are equivalent to
    // the pre-config-file behaviour when no config file is loaded.
    initial_state.display_config = settings.display.clone();
    initial_state.replay = Some(initial_replay_state(args.speed.max(0.05), args.replay_loop));
    // If `--start HH:MM:SS` was given, enqueue the seek so the first
    // ReplayDriver tick honors it before drawing the first frame.
    if let Some(start) = args.start.as_deref()
        && let Ok(d) = crate::record::replay::parse_timecode(start)
        && let Some(r) = initial_state.replay.as_mut()
    {
        r.pending_seek = Some(d);
    }
    // Prime the tab list from the header's hosts so the tab row is
    // populated even before the first data frame is materialized. Apply
    // a defensive cap mirroring `replay::MAX_HEADER_HOSTS` — the
    // replayer already truncates at ingest, but belt-and-suspenders
    // here ensures that even a direct caller constructing a `Replayer`
    // via a future API cannot flood the tab row.
    let mut header_hosts = driver.total_hosts();
    if header_hosts.len() > crate::record::replay::MAX_HEADER_HOSTS {
        header_hosts.truncate(crate::record::replay::MAX_HEADER_HOSTS);
    }
    if !header_hosts.is_empty() {
        // Users tab sits right after "All" (issue #189) so the
        // cluster-wide tabs live together at the left edge.  Topology
        // (issue #190) follows Users so the three cluster-wide tabs
        // share the same prefix.
        let mut tabs = vec![
            "All".to_string(),
            crate::ui::tabs::USERS_TAB_NAME.to_string(),
            crate::ui::tabs::TOPOLOGY_TAB_NAME.to_string(),
        ];
        tabs.extend(header_hosts);
        initial_state.tabs = tabs;
    }
    let app_state = Arc::new(Mutex::new(initial_state));
    let data_notify = Arc::new(Notify::new());

    let _terminal_manager = match TerminalManager::new() {
        Ok(manager) => manager,
        Err(e) => {
            eprintln!("Failed to initialize terminal: {e}");
            return;
        }
    };

    // Replay driver task: ticks ~50ms, consumes pause/step/seek/speed
    // off AppState.replay, and pushes frames into the shared state.
    let state_for_driver = Arc::clone(&app_state);
    let notify_for_driver = Arc::clone(&data_notify);
    let driver_handle = tokio::spawn(async move {
        loop {
            if let Err(e) = driver.tick(Arc::clone(&state_for_driver)).await {
                // Hard errors (e.g. schema mismatch) surface in the UI
                // as a notification and halt the driver. The caller
                // can then exit cleanly with `q` / Ctrl-C.
                let mut state = state_for_driver.lock().await;
                let _ = state.notifications.error(format!("replay: {e}"));
                break;
            }
            notify_for_driver.notify_one();
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    });

    let mut ui_loop = match UiLoop::new(Arc::clone(&app_state), Arc::clone(&data_notify)) {
        Ok(ui_loop) => ui_loop,
        Err(e) => {
            eprintln!("Failed to initialize UI: {e}");
            driver_handle.abort();
            return;
        }
    };

    if let Err(e) = ui_loop.run(args).await {
        eprintln!("UI loop error: {e}");
    }

    driver_handle.abort();
}

/// Enter TUI with the agentless SSH transport (issue #194).
///
/// Unlike `run_view_mode`, this path does NOT need a pre-existing
/// `all-smi api` to be running on each target — it opens an SSH
/// session per target and runs `all-smi snapshot` (native) or
/// falls back to `nvidia-smi` / `rocm-smi` per
/// `--ssh-fallback`. Returns early with a user-facing error when
/// neither `--ssh` nor `--ssh-hostfile` produced a non-empty
/// target list.
pub async fn run_ssh_mode(args: &ViewArgs, settings: &Settings) {
    use std::time::Duration;

    use crate::network::ssh_target::{parse_hostfile, parse_ssh_arg};
    use crate::network::ssh_transport::{SshFallbackPolicy, StrictHostKey};
    use crate::view::data_collection::{SshStrategy, SshStrategyConfig};

    let mut targets = Vec::new();
    if let Some(raw) = args.ssh.as_deref() {
        match parse_ssh_arg(raw) {
            Ok(mut t) => targets.append(&mut t),
            Err(e) => {
                eprintln!("error: {e}");
                return;
            }
        }
    }
    if let Some(path) = args.ssh_hostfile.as_deref() {
        match parse_hostfile(path) {
            Ok(mut t) => targets.append(&mut t),
            Err(e) => {
                eprintln!("error: {e}");
                return;
            }
        }
    }

    if targets.is_empty() {
        eprintln!("error: --ssh and --ssh-hostfile produced no SSH targets");
        return;
    }

    let strict_host_key = match StrictHostKey::from_cli(&args.ssh_strict_host_key) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            return;
        }
    };

    let fallback_policy = match SshFallbackPolicy::from_cli(args.ssh_fallback.as_deref()) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            return;
        }
    };

    let strategy_config = SshStrategyConfig {
        targets,
        explicit_key: args.ssh_key.clone(),
        strict_host_key,
        known_hosts: args.ssh_known_hosts.clone(),
        connect_timeout: Duration::from_secs(args.ssh_timeout_secs.max(1)),
        fallback_policy,
        concurrency: args.ssh_concurrency.max(1),
    };

    let strategy = Arc::new(SshStrategy::new(strategy_config));

    let mut initial_state = AppState::with_energy_config(&settings.energy);
    initial_state.is_local_mode = false;
    let alert_config = build_alert_config(settings, args.alert_temp, args.alert_util_low_mins);
    initial_state.alerter = Alerter::new(alert_config);
    initial_state.display_config = settings.display.clone();
    let app_state = Arc::new(Mutex::new(initial_state));

    let data_notify = Arc::new(Notify::new());

    let _terminal_manager = match TerminalManager::new() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("Failed to initialize terminal: {e}");
            return;
        }
    };

    let data_collector =
        DataCollector::with_notify(Arc::clone(&app_state), Arc::clone(&data_notify));
    let args_clone = args.clone();
    let strategy_clone = Arc::clone(&strategy);
    tokio::spawn(async move {
        data_collector
            .run_ssh_mode(args_clone, strategy_clone)
            .await;
    });

    let mut ui_loop = match UiLoop::new(app_state, data_notify) {
        Ok(u) => u,
        Err(e) => {
            eprintln!("Failed to initialize UI: {e}");
            return;
        }
    };
    if let Err(e) = ui_loop.run(args).await {
        eprintln!("UI loop error: {e}");
    }
}

pub async fn run_view_mode(args: &ViewArgs, settings: &Settings) {
    // Initialize application state for remote mode.
    // `is_local_mode = false` whenever any --hosts / --hostfile argument is
    // supplied, including a single remote host.  The UI renders Cluster
    // Overview, dashboard items, and the tabs row only when this is false
    // (see src/view/frame_renderer.rs render_main).
    let mut initial_state = AppState::with_energy_config(&settings.energy);
    initial_state.is_local_mode = false;
    let alert_config = build_alert_config(settings, args.alert_temp, args.alert_util_low_mins);
    initial_state.alerter = Alerter::new(alert_config);
    initial_state.display_config = settings.display.clone();
    // Propagate the `[display]` section so renderers consuming
    // `AppState.display_config` honour the operator's color_scheme /
    // gauge_style / show_led_grid choices. Defaults are equivalent to
    // the pre-config-file behaviour when no config file is loaded.
    initial_state.display_config = settings.display.clone();
    let app_state = Arc::new(Mutex::new(initial_state));

    // Create shared notification handle for collector -> UI wakeups
    let data_notify = Arc::new(Notify::new());

    // Initialize terminal
    let _terminal_manager = match TerminalManager::new() {
        Ok(manager) => manager,
        Err(e) => {
            eprintln!("Failed to initialize terminal: {e}");
            return;
        }
    };

    // Start data collection in background with notification handle
    let data_collector =
        DataCollector::with_notify(Arc::clone(&app_state), Arc::clone(&data_notify));
    let args_clone = args.clone();
    tokio::spawn(async move {
        let hosts = args_clone.hosts.clone().unwrap_or_default();
        let hostfile = args_clone.hostfile.clone();

        // Remote mode
        data_collector
            .run_remote_mode(args_clone, hosts, hostfile)
            .await;
    });

    // Run UI loop with the same notification handle
    let mut ui_loop = match UiLoop::new(app_state, data_notify) {
        Ok(ui_loop) => ui_loop,
        Err(e) => {
            eprintln!("Failed to initialize UI: {e}");
            return;
        }
    };

    if let Err(e) = ui_loop.run(args).await {
        eprintln!("UI loop error: {e}");
    }

    // Terminal cleanup is handled by TerminalManager's Drop trait
}
