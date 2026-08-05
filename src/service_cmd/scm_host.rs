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

//! The `all-smi service run` host: the process side of the Windows
//! service (issue #311).
//!
//! `service run` is hidden from `--help` because no operator should type
//! it. The Service Control Manager starts the registered binary with
//! these arguments and expects the process to call
//! `StartServiceCtrlDispatcher` promptly; run from a console the call
//! fails with `ERROR_FAILED_SERVICE_CONTROLLER_CONNECT` and this module
//! explains that rather than surfacing the raw error.
//!
//! # Lifecycle
//!
//! 1. Install the rolling file subscriber, because stdout is void here.
//! 2. Register the control handler and report `SERVICE_START_PENDING`
//!    with a wait hint.
//! 3. Load the merged TOML + environment configuration and build the
//!    Tokio runtime.
//! 4. Report `SERVICE_RUNNING` once a listener is actually bound, not
//!    when the task is merely spawned.
//! 5. On Stop or Shutdown, report `SERVICE_STOP_PENDING`, raise the
//!    process-wide shutdown latch, and let `run_api_mode` drain the
//!    server and flush the energy WAL before reporting
//!    `SERVICE_STOPPED`.
//!
//! Step 5 is the reason the shutdown latch exists at all. Calling
//! `std::process::exit` from the control handler would strand the
//! pending Joule deltas and break Prometheus counter monotonicity across
//! a service restart.

#![warn(dead_code)]

use std::ffi::OsString;
use std::sync::OnceLock;
use std::time::Duration;

use windows_service::service::{
    ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState,
    ServiceStatus as ScmStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::{define_windows_service, service_dispatcher};

use super::scm_backend::{describe, raw_code};
use super::{EXIT_ERROR, EXIT_OK, SERVICE_NAME, scm, scm_log};
use crate::api::shutdown as api_shutdown;
use crate::cli::ApiArgs;
use crate::common::config_file::{self, Settings};

/// Set once the control handler is registered, so the handler itself can
/// report `SERVICE_STOP_PENDING` before the shutdown has finished.
static STATUS_HANDLE: OnceLock<service_control_handler::ServiceStatusHandle> = OnceLock::new();

/// Service-specific exit code reported when the server stopped without
/// anyone asking it to: a port already in use, a fatal bind error, or an
/// unexpected return from `run_api_mode`. Non-zero so the SCM's
/// configured failure actions restart us.
const EXIT_CODE_UNEXPECTED_STOP: u32 = 1;

/// Entry point for `all-smi service run`. Blocks until the service
/// stops.
pub fn run() -> i32 {
    match service_dispatcher::start(SERVICE_NAME, ffi_service_main) {
        Ok(()) => EXIT_OK,
        Err(e) => {
            eprintln!(
                "error: {}",
                scm::console_entry_point_message(raw_code(&e), &describe(&e))
            );
            EXIT_ERROR
        }
    }
}

define_windows_service!(ffi_service_main, service_main);

/// Called by the generated FFI shim on a background thread once the
/// dispatcher has connected.
fn service_main(_arguments: Vec<OsString>) {
    // Logging first so every later failure has somewhere to land.
    let log_dir = scm_log::init();

    if let Err(e) = run_service(&log_dir) {
        // Best effort: the message reaches the log only when `init`
        // succeeded, which is exactly the case where the operator has
        // somewhere to read it.
        tracing::error!("all-smi service failed: {e}");
        report(
            ServiceState::Stopped,
            ServiceControlAccept::empty(),
            ServiceExitCode::ServiceSpecific(EXIT_CODE_UNEXPECTED_STOP),
            Duration::default(),
        );
    }
}

fn run_service(log_dir: &Result<std::path::PathBuf, String>) -> Result<(), String> {
    let event_handler = move |control: ServiceControl| -> ServiceControlHandlerResult {
        match control {
            // Every service must answer Interrogate, even as a no-op.
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            ServiceControl::Stop | ServiceControl::Shutdown => {
                report(
                    ServiceState::StopPending,
                    ServiceControlAccept::empty(),
                    ServiceExitCode::Win32(0),
                    Duration::from_secs(scm::TRANSITION_WAIT_HINT_SECS),
                );
                // Reaches the same graceful path Ctrl+C takes on Unix,
                // including the energy WAL flush. Returns immediately;
                // the drain happens on the main thread.
                api_shutdown::request_shutdown();
                ServiceControlHandlerResult::NoError
            }
            _ => ServiceControlHandlerResult::NotImplemented,
        }
    };

    let status_handle =
        service_control_handler::register(SERVICE_NAME, event_handler).map_err(|e| {
            format!(
                "could not register the service control handler: {}",
                describe(&e)
            )
        })?;
    // `OnceLock::set` only fails if the handler was already registered,
    // which cannot happen: `service_main` runs once per process.
    let _ = STATUS_HANDLE.set(status_handle);

    // Accept Stop from here on rather than only once running. A service
    // whose hardware probe wedges during startup would otherwise be
    // unstoppable except by `taskkill`.
    let accepted = ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN;
    report(
        ServiceState::StartPending,
        accepted,
        ServiceExitCode::Win32(0),
        Duration::from_secs(scm::TRANSITION_WAIT_HINT_SECS),
    );

    match log_dir {
        Ok(dir) => tracing::info!("all-smi service starting; logging to {}", dir.display()),
        Err(e) => eprintln!("warning: {e}"),
    }

    let settings = load_settings()?;
    let args = api_args(&settings);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(4)
        .build()
        .map_err(|e| format!("could not build the Tokio runtime: {e}"))?;

    // Report SERVICE_RUNNING only once a listener is bound, so the SCM
    // never shows a running service whose port refuses connections.
    runtime.spawn(async move {
        api_shutdown::wait_until_serving().await;
        // A Stop that arrived during startup already moved us to
        // STOP_PENDING; going back to RUNNING would be a lie.
        if !api_shutdown::shutdown_requested() {
            report(
                ServiceState::Running,
                accepted,
                ServiceExitCode::Win32(0),
                Duration::default(),
            );
        }
    });

    runtime.block_on(async {
        crate::api::run_api_mode(&args, &settings).await;
    });

    // `run_api_mode` returns only after the listeners have drained and
    // the energy WAL has been flushed.
    let exit_code = if api_shutdown::shutdown_requested() {
        tracing::info!("all-smi service stopped cleanly");
        ServiceExitCode::Win32(0)
    } else {
        tracing::error!(
            "the API server exited without a stop request; the SCM will apply the configured \
             failure actions"
        );
        ServiceExitCode::ServiceSpecific(EXIT_CODE_UNEXPECTED_STOP)
    };
    report(
        ServiceState::Stopped,
        ServiceControlAccept::empty(),
        exit_code,
        Duration::default(),
    );
    Ok(())
}

/// Push a status transition to the SCM. Best effort: a failure here is
/// logged, never propagated, because there is no recovery from "the SCM
/// will not listen" other than exiting, which it will notice anyway.
fn report(
    current_state: ServiceState,
    controls_accepted: ServiceControlAccept,
    exit_code: ServiceExitCode,
    wait_hint: Duration,
) {
    let Some(handle) = STATUS_HANDLE.get() else {
        return;
    };
    let status = ScmStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state,
        controls_accepted,
        exit_code,
        // The checkpoint only matters when a pending transition reports
        // repeated progress; every transition here is a single step.
        checkpoint: 0,
        wait_hint,
        // The SCM fills this in for an OWN_PROCESS service.
        process_id: None,
    };
    if let Err(e) = handle.set_service_status(status) {
        tracing::error!(
            "could not report {current_state:?} to the service control manager: {}",
            describe(&e)
        );
    }
}

/// Load the merged TOML + environment configuration.
///
/// The service reads the same candidates `all-smi config path` lists.
/// `%PROGRAMDATA%\all-smi\config.toml` is the one that matters here:
/// LocalSystem's `%APPDATA%` resolves into the systemprofile directory,
/// which no operator will ever edit.
fn load_settings() -> Result<Settings, String> {
    let outcome = config_file::load(None).map_err(|e| format!("configuration error: {e}"))?;
    for w in &outcome.warnings {
        tracing::warn!("config: {w}");
    }
    for k in &outcome.settings.unknown_keys {
        tracing::warn!("config: unknown key `{k}` (forward-compatible, preserved)");
    }
    match crate::common::paths::discover_existing_config() {
        Some(path) => tracing::info!("config: loaded {}", path.display()),
        None => tracing::info!(
            "config: no file found in the search path; using compiled defaults plus environment \
             overrides"
        ),
    }
    Ok(outcome.settings)
}

/// Build the API arguments from configuration alone.
///
/// There is no command line to honour: the SCM launches the binary with
/// exactly `service run`, so every knob comes from the config file or
/// the environment, which is the whole point of keeping runtime settings
/// out of the service definition.
fn api_args(settings: &Settings) -> ApiArgs {
    ApiArgs {
        port: Some(settings.api.port),
        interval: Some(settings.api.interval_secs),
        processes: Some(settings.api.processes),
    }
}

// Test module lives in `scm_host_tests.rs` to keep this file under the
// 500-line soft limit.
#[cfg(test)]
#[path = "scm_host_tests.rs"]
mod tests;
