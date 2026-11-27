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

//! Mock generator interface for the all-smi mock server.
//!
//! [`mock_generator`](crate::traits::mock_generator) defines the
//! [`MockGenerator`](crate::traits::MockGenerator) trait and its support
//! types, which the platform templates under `src/mock/templates/` implement
//! and import through `all_smi::traits::mock_generator`.

pub mod mock_generator;

pub use mock_generator::{
    MockConfig, MockData, MockError, MockGenerator, MockPlatform, MockResult,
};
