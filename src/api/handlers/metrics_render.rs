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

//! Prometheus `/metrics` HTTP handler.
//!
//! Renders the merged reader outputs from [`AppState`] through the shared
//! exposition writer in [`crate::api::metrics::render`]. Kept separate from
//! the SSE / snapshot handlers (issue #193) so adding new routes does not
//! force a rebuild of this unchanged hot path.
//!
//! Status-code contract (issue #324): this endpoint answers `200` for its
//! entire lifetime, including the window between the listener binding and
//! the first collection cycle landing. It is deliberately *not* a
//! readiness probe, because turning it into one would break every scraper
//! that already treats a non-200 as a failed target. What changed in #324
//! is that the body is no longer empty in that window: the exposition
//! carries `all_smi_up 0` plus build info until the first cycle completes,
//! then `all_smi_up 1` plus the full metric set. Consumers that need a
//! yes/no gate use `/-/ready` (see [`crate::api::handlers::ready`]).

use axum::extract::State;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::api::handlers::ready::is_ready;
use crate::api::metrics::render::{MetricsRenderInputs, render_prometheus_exposition};
use crate::app_state::AppState;

pub type SharedState = Arc<RwLock<AppState>>;

pub async fn metrics_handler(State(state): State<SharedState>) -> String {
    let state = state.read().await;
    let inputs = MetricsRenderInputs {
        gpu_info: &state.gpu_info,
        process_info: &state.process_info,
        cpu_info: &state.cpu_info,
        memory_info: &state.memory_info,
        storage_info: &state.storage_info,
        runtime_environment: &state.runtime_environment,
        chassis_info: &state.chassis_info,
        vgpu_info: &state.vgpu_info,
        mig_info: &state.mig_info,
        // Energy counter (issue #191) reflects the integrator owned by
        // AppState; we export the PowerIntegrator directly so the
        // counter's HELP/TYPE header lines are only emitted when there
        // is at least one device with recorded samples.
        energy_integrator: Some(state.energy.integrator()),
        // Same predicate `/-/ready` answers with, read under the same
        // lock acquisition as the data itself so the gauge cannot
        // disagree with the samples it is rendered alongside (issue
        // #324).
        ready: is_ready(&state),
    };
    render_prometheus_exposition(&inputs)
}
