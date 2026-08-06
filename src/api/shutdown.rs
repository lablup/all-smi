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

//! Graceful-shutdown and readiness signalling for `all-smi api`
//! (issue #311).
//!
//! Split out of [`crate::api::server`], which was already past the file-size
//! soft limit, and because this is a self-contained concern: it is the
//! only place that decides what counts as a reason to stop serving.
//!
//! Before #311 there were exactly two such reasons, `Ctrl+C` and
//! `SIGTERM`. The Windows Service Control Manager needs a third: it
//! delivers Stop on a control-handler thread and gives the process no
//! signal to observe, so without an in-process trigger the handler
//! would have to call `std::process::exit` and strand the energy WAL
//! flush, breaking Prometheus counter monotonicity across a restart.
//!
//! The mirror question, when the service may report `SERVICE_RUNNING`,
//! is answered by the serving latch: each listener raises it after a
//! successful bind, so the SCM never sees a running service whose port
//! refuses connections.
//!
//! Note the deliberate narrowing of vocabulary since #311: this latch is
//! the *serving* signal, not the *readiness* signal. Issue #324 added the
//! latter as `/-/ready` and as the `all_smi_up` gauge, and considered
//! moving this latch behind it. It did not. [`mark_serving`] carries the
//! reasoning.

use std::sync::OnceLock;

use crate::api::latch::Latch;

/// Process-global latch raised by [`request_shutdown`] (issue #311).
static SHUTDOWN: OnceLock<Latch> = OnceLock::new();

/// Process-global latch raised once a listener is bound (issue #311).
static SERVING: OnceLock<Latch> = OnceLock::new();

fn shutdown_latch() -> &'static Latch {
    SHUTDOWN.get_or_init(Latch::new)
}

fn serving_latch() -> &'static Latch {
    SERVING.get_or_init(Latch::new)
}

/// Ask a running [`super::server::run_api_mode`] to shut down
/// gracefully, exactly as a `Ctrl+C` or `SIGTERM` would (issue #311).
///
/// This exists because the Windows Service Control Manager delivers its
/// Stop control on a control-handler thread and gives the process no
/// signal to observe. Routing that control here instead of calling
/// `std::process::exit` is what keeps the energy WAL flush on the
/// shutdown path, so Prometheus counters stay monotonic across a
/// service restart.
///
/// Safe to call from a non-async context, before the server starts, or
/// more than once. Calling it before startup latches the request, and
/// the first listener to install its shutdown future observes it
/// immediately.
///
/// Currently only the in-tree Windows service host calls this, so the
/// binary target has no non-Windows caller; the library target exposes
/// it on every platform for embedding hosts.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn request_shutdown() {
    shutdown_latch().trigger();
}

/// Whether a graceful shutdown has already been requested through
/// [`request_shutdown`].
#[cfg_attr(not(windows), allow(dead_code))]
pub fn shutdown_requested() -> bool {
    shutdown_latch().is_triggered()
}

/// Resolve once the API server has bound at least one listener and is
/// serving requests (issue #311).
///
/// The Windows service host awaits this before reporting
/// `SERVICE_RUNNING`, so the SCM never sees a running service whose port
/// is not yet accepting connections.
///
/// This resolves at bind, which is *before* the first collection cycle.
/// It is a serving signal, not a readiness one: see [`mark_serving`] for
/// why the two are kept apart, and use `/-/ready` when the question is
/// whether there is data to serve.
#[cfg_attr(not(windows), allow(dead_code))]
pub async fn wait_until_serving() {
    serving_latch().wait().await
}

/// Whether at least one listener has been bound.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn is_serving() -> bool {
    serving_latch().is_triggered()
}

/// Raise the serving latch. Called by each listener immediately after a
/// successful bind.
///
/// # Why this stays at bind rather than moving behind the first
/// collection cycle (issue #324)
///
/// #324 gave the exporter a real readiness signal (`/-/ready`, and the
/// `all_smi_up` gauge) and asked whether this latch should move with it,
/// since #311 documented it as opening before there was anything to
/// serve. It should not, for three reasons.
///
/// First, the two are different questions. The SCM has no readiness
/// concept: its state machine offers `SERVICE_START_PENDING` and
/// `SERVICE_RUNNING`, and `SERVICE_RUNNING` is a liveness verdict. The
/// natural liveness boundary for a network exporter is "the listener
/// answers". Readiness is now separately queryable at any time by anyone
/// who actually needs it, which is exactly the liveness/readiness split
/// Kubernetes and the Prometheus ecosystem already use.
///
/// Second, the inconsistency #324 was worried about is resolved at the
/// other end. Before #324 this latch opened onto an endpoint that served
/// zero bytes, so `SERVICE_RUNNING` really did promise nothing. Now
/// `/metrics` carries `all_smi_up 0` plus build info from the first
/// request onward, so the instant this latch opens there is a defined,
/// non-empty response. Giving `/metrics` a floor fixed the mismatch;
/// delaying the latch is not needed to fix it a second time.
///
/// Third, moving it has a concrete failure mode that is worse than the
/// one it would prevent. `crate::service_cmd::scm_host` reports
/// `StartPending` exactly once, with `wait_hint =
/// TRANSITION_WAIT_HINT_SECS` (10 s) and `checkpoint: 0`. Because the
/// checkpoint never increments, that single report is the entire start
/// budget the SCM grants. A first collection cycle on Windows means cold
/// COM/WMI initialization plus NVML enumeration, which on a
/// many-GPU host or a wedged driver can exceed 10 s. The SCM would then
/// fail the start and apply the configured recovery actions, restarting
/// the process into the same slow path: a boot loop on precisely the
/// hosts where the telemetry matters most. Reporting `SERVICE_RUNNING`
/// for a process that is up and honestly publishing `all_smi_up 0` is
/// the better trade. The same argument applies to any dependent service
/// that waits on this one, which would otherwise block on hardware
/// enumeration.
pub(crate) fn mark_serving() {
    serving_latch().trigger();
}

/// Complete when the process receives Ctrl+C on any platform, a
/// `SIGTERM` on Unix, or an in-process [`request_shutdown`]. Callers use
/// this to let `axum::serve` return so the parent function can run
/// post-shutdown cleanup (energy WAL flush, socket cleanup, etc.).
pub(crate) async fn shutdown_signal() {
    shutdown_signal_with(shutdown_latch()).await
}

/// The body of [`shutdown_signal`], parameterised over the external
/// trigger so the added source is testable without touching process
/// state or delivering a real signal.
async fn shutdown_signal_with(external: &Latch) {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(e) => {
                tracing::warn!("failed to install SIGTERM handler: {e}");
                // Fall back to ctrl_c only.
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    // A latch that is never triggered pends forever, so on a host where
    // nothing calls `request_shutdown` this arm can never win and the
    // Ctrl+C / SIGTERM behaviour is bit-for-bit what it was before.
    let external = external.wait();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
        _ = external => {
            tracing::info!("graceful shutdown requested by the host process");
        }
    }
}

// Test module lives in `shutdown_tests.rs` to keep this file focused.
#[cfg(test)]
#[path = "shutdown_tests.rs"]
mod tests;
