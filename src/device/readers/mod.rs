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

// Module for device readers with reduced code duplication

// Chassis-level monitoring (node power, thermal, BMC)
pub mod chassis;

// Common caching utilities shared across all readers
pub mod common_cache;

// Native Apple Silicon reader using IOReport/SMC (no sudo required)
#[cfg(target_os = "macos")]
pub mod apple_silicon_native;

pub mod furiosa;
pub mod gaudi;
#[cfg(target_os = "linux")]
pub mod google_tpu;
pub mod nvidia;
pub mod nvidia_jetson;
pub mod nvidia_vgpu;
pub mod rebellions;
#[cfg(target_os = "linux")]
pub mod tpu_grpc;
#[cfg(target_os = "linux")]
pub mod tpu_info_runner;
#[cfg(target_os = "linux")]
pub mod tpu_pjrt;
#[cfg(target_os = "linux")]
pub mod tpu_sysfs;

#[cfg(target_os = "linux")]
pub mod tenstorrent;

#[cfg(all(target_os = "linux", not(target_env = "musl")))]
pub mod amd;

#[cfg(target_os = "windows")]
pub mod amd_windows;
