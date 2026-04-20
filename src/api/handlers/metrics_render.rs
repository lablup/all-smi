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

use axum::extract::State;
use std::sync::Arc;
use tokio::sync::RwLock;

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
    };
    render_prometheus_exposition(&inputs)
}
