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

//! Cluster-wide aggregation primitives for the remote `view`.
//!
//! The submodules here take already-parsed per-host records (see
//! `src/network/metrics_parser.rs`) and collapse them into operator-facing
//! summaries without touching the terminal or the UI loop.  Keeping the
//! math in a pure-function module makes the Users tab (issue #189) easy
//! to unit-test independently of the rendering path.

pub mod user;
