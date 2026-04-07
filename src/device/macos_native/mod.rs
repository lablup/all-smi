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

//! Native macOS APIs for Apple Silicon metrics collection
//!
//! This module provides native macOS API bindings that replace the external
//! `powermetrics` command for collecting Apple Silicon hardware metrics.
//!
//! ## Key Benefits
//! - No sudo required - IOReport API works without root privileges
//! - Faster collection - Direct API calls vs spawning external process
//! - More stable - No process management needed
//! - Additional metrics - System Power (PSTR), actual temperature values
//!
//! ## Modules
//! - `ioreport`: IOReport API for power and residency metrics
//! - `smc`: Apple SMC for temperature and system power metrics
//! - `thermal`: NSProcessInfo thermal state binding
//! - `manager`: Unified manager for native metrics collection

mod ioreport;
mod metrics;
mod smc;
mod thermal;

pub mod manager;

// Re-export public types for use by apple_silicon_native reader and main
#[allow(unused_imports)]
pub use manager::{
    NativeMetricsManager, get_native_metrics_manager, initialize_native_metrics_manager,
    shutdown_native_metrics_manager,
};
