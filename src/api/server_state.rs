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
//
//! Composite axum state wiring the Prometheus `/metrics` handler and the
//! new SSE / snapshot handlers (issue #193) onto a single router.
//!
//! Each handler uses its own `State<...>` extractor thanks to axum 0.8's
//! [`axum::extract::FromRef`] derive — the `/metrics` endpoint continues
//! to see `SharedState`, while `/events` and `/snapshot` extract
//! [`crate::api::FrameBus`].

use axum::extract::FromRef;

use crate::api::FrameBus;
use crate::api::handlers::SharedState;

/// Composite router state. Handlers extract the sub-state they need via
/// `State<SharedState>` or `State<FrameBus>` — the `FromRef` derives
/// below make that work without wrapping handlers in tuples.
#[derive(Clone)]
pub struct ApiState {
    pub shared: SharedState,
    pub bus: FrameBus,
}

impl ApiState {
    pub fn new(shared: SharedState, bus: FrameBus) -> Self {
        Self { shared, bus }
    }
}

impl FromRef<ApiState> for SharedState {
    fn from_ref(input: &ApiState) -> Self {
        input.shared.clone()
    }
}

impl FromRef<ApiState> for FrameBus {
    fn from_ref(input: &ApiState) -> Self {
        input.bus.clone()
    }
}
