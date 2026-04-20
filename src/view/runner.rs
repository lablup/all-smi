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
use crate::ui::alerts::Alerter;
use crate::view::data_collection::{ReplayDriver, initial_replay_state};
use crate::view::{
    data_collector::DataCollector, terminal_manager::TerminalManager, ui_loop::UiLoop,
};

pub async fn run_local_mode(args: &LocalArgs) {
    let mut startup_profiler = crate::utils::StartupProfiler::new();
    startup_profiler.checkpoint("Starting run_local_mode");

    // Initialize application state for local mode.
    // `is_local_mode = true` means no --hosts / --hostfile were supplied.
    // The UI gates the Cluster Overview card, dashboard items, and tabs row
    // behind `!is_local_mode` (see src/view/frame_renderer.rs render_main).
    let mut initial_state = AppState::new();
    initial_state.is_local_mode = true;
    // Apply CLI-supplied alert thresholds on top of the compiled defaults.
    // When the companion config-file issue lands these will be overlaid by
    // the TOML loader before this point; CLI flags then win per the
    // standard clap override chain.
    let alert_config =
        AlertConfig::default().with_cli_overrides(args.alert_temp, args.alert_util_low_mins);
    initial_state.alerter = Alerter::new(alert_config);
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
        hosts: None,
        hostfile: None,
        interval: args.interval,
        alert_temp: args.alert_temp,
        alert_util_low_mins: args.alert_util_low_mins,
        replay: None,
        speed: 1.0,
        start: None,
        replay_loop: false,
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
        hosts: None,
        hostfile: None,
        interval: args.interval,
        alert_temp: args.alert_temp,
        alert_util_low_mins: args.alert_util_low_mins,
        replay: None,
        speed: 1.0,
        start: None,
        replay_loop: false,
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
pub async fn run_replay_mode(args: &ViewArgs) {
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
    let mut initial_state = AppState::new();
    initial_state.is_local_mode = false;
    initial_state.loading = false;
    initial_state.alerter = Alerter::new(AlertConfig::default());
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
        let mut tabs = vec!["All".to_string()];
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

pub async fn run_view_mode(args: &ViewArgs) {
    // Initialize application state for remote mode.
    // `is_local_mode = false` whenever any --hosts / --hostfile argument is
    // supplied, including a single remote host.  The UI renders Cluster
    // Overview, dashboard items, and the tabs row only when this is false
    // (see src/view/frame_renderer.rs render_main).
    let mut initial_state = AppState::new();
    initial_state.is_local_mode = false;
    let alert_config =
        AlertConfig::default().with_cli_overrides(args.alert_temp, args.alert_util_low_mins);
    initial_state.alerter = Alerter::new(alert_config);
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
