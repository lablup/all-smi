//! Platform-specific mock template generators

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

pub mod amd_gpu;
pub mod apple_silicon;
pub mod common;
pub mod disk;
pub mod furiosa;
pub mod gaudi;
pub mod jetson;
pub mod nvidia;
pub mod rebellions;
pub mod tenstorrent;
pub mod vgpu;

// Re-export commonly used items
