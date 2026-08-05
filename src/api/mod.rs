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

pub mod collection_loop;
pub mod frame_bus;
pub mod handlers;
/// One-way boolean latch used for shutdown and readiness signalling
/// (issue #311).
pub mod latch;
pub mod metrics;
pub mod server;
pub mod server_state;
/// Graceful-shutdown and readiness signalling (issue #311).
pub mod shutdown;

pub use frame_bus::FrameBus;
pub use server::*;
// `shutdown` is deliberately not glob re-exported here. Its entry
// points are reached through `api::shutdown::…`, which is how the
// Windows service host imports them; a re-export would be an unused
// import in the binary target, where the module tree is private.
