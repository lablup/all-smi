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

pub mod aggregator;
pub mod local_collector;
pub mod remote_collector;
pub mod replay_collector;
pub mod ssh_strategy;
pub mod strategy;

#[allow(unused_imports)] // Re-exported for embedding crates / future callers.
pub use aggregator::DataAggregator;
pub use local_collector::LocalCollector;
pub use remote_collector::RemoteCollectorBuilder;
pub use replay_collector::{ReplayDriver, initial_replay_state};
pub use ssh_strategy::{SshStrategy, SshStrategyConfig};
pub use strategy::{CollectionConfig, DataCollectionStrategy};
