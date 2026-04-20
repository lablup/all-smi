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

//! # all-smi
//!
//! A cross-platform library for monitoring GPU, NPU, CPU, and memory hardware.
//!
//! `all-smi` provides a unified API for querying hardware metrics across multiple
//! platforms and device types including NVIDIA GPUs, AMD GPUs, Apple Silicon,
//! Intel Gaudi NPUs, Google TPUs, Tenstorrent, Rebellions, and Furiosa NPUs.
//!
//! ## Quick Start
//!
//! ```rust,no_run
//! use all_smi::{AllSmi, Result};
//!
//! fn main() -> Result<()> {
//!     // Initialize with auto-detection
//!     let smi = AllSmi::new()?;
//!
//!     // Get all GPU/NPU information
//!     for gpu in smi.get_gpu_info() {
//!         println!("{}: {}% utilization, {:.1}W",
//!             gpu.name, gpu.utilization, gpu.power_consumption);
//!     }
//!
//!     // Get CPU information
//!     for cpu in smi.get_cpu_info() {
//!         println!("{}: {:.1}% utilization", cpu.cpu_model, cpu.utilization);
//!     }
//!
//!     // Get memory information
//!     for mem in smi.get_memory_info() {
//!         println!("Memory: {:.1}% used", mem.utilization);
//!     }
//!
//!     Ok(())
//! }
//! ```
//!
//! ## Using the Prelude
//!
//! For convenience, you can import all common types with the prelude:
//!
//! ```rust,no_run
//! use all_smi::prelude::*;
//!
//! fn main() -> Result<()> {
//!     let smi = AllSmi::new()?;
//!     let gpus: Vec<GpuInfo> = smi.get_gpu_info();
//!     println!("Found {} GPU(s)", gpus.len());
//!     Ok(())
//! }
//! ```
//!
//! ## Platform Support
//!
//! | Platform | GPUs | NPUs | CPU | Memory |
//! |----------|------|------|-----|--------|
//! | Linux | NVIDIA, AMD | Gaudi, TPU, Tenstorrent, Rebellions, Furiosa | Yes | Yes |
//! | macOS | Apple Silicon | - | Yes | Yes |
//! | Windows | NVIDIA, AMD | - | Yes | Yes |
//!
//! ## Features
//!
//! - **GPU Monitoring**: Utilization, memory, temperature, power, frequency
//! - **NPU Monitoring**: Intel Gaudi, Google TPU, Tenstorrent, Rebellions, Furiosa
//! - **CPU Monitoring**: Utilization, frequency, temperature, P/E cores (Apple Silicon)
//! - **Memory Monitoring**: System RAM, swap, buffers, cache
//! - **Process Monitoring**: GPU processes with memory usage
//! - **Chassis Monitoring**: Total power, thermal pressure, fan speeds

// =============================================================================
// Public Library API
// =============================================================================

/// High-level client API for hardware monitoring.
pub mod client;

/// Unified error types for the library.
pub mod error;

/// Prelude module for convenient imports.
pub mod prelude;

// Re-export main types at crate root for convenience
pub use client::{AllSmi, AllSmiConfig, DeviceType};
pub use error::{Error, Result};

// =============================================================================
// Internal Modules (also exported for advanced usage and testing)
// =============================================================================

/// Prometheus metric exporters and HTTP handlers used by `all-smi api` mode.
///
/// Exposed for integration tests that need to assert on the exact exporter
/// output. The HTTP handler side of the module depends on the `cli` feature
/// because it pulls in [`app_state`], `axum`, and `tower-http`.
#[cfg(feature = "cli")]
pub mod api;

/// Device readers and types for GPU, CPU, memory monitoring.
pub mod device;

/// Parsing utilities and macros.
#[macro_use]
pub mod parsing;

/// Application state management.
#[cfg(feature = "cli")]
pub mod app_state;

/// Command-line interface definitions.
#[cfg(feature = "cli")]
pub mod cli;

/// Self-diagnosis and support-bundle subcommand (issue #188).
///
/// Exposed under `cli` because the orchestrator depends on tokio + clap
/// and the `anyhow` error plumbing used by every other CLI entry point.
#[cfg(feature = "cli")]
pub mod doctor;

/// Cluster metrics aggregation, coordination, and energy accounting.
///
/// Gated on `cli` because `coordinator` and `energy` both depend on
/// [`app_state`], and the Prometheus / TUI callers only exist in that
/// feature tree.
#[cfg(feature = "cli")]
pub mod metrics;

/// Network client for remote monitoring.
#[cfg(feature = "cli")]
pub mod network;

/// Storage monitoring.
pub mod storage;

/// Common traits for collectors and exporters.
pub mod traits;

/// Terminal UI components.
#[cfg(feature = "cli")]
pub mod ui;

/// One-shot snapshot subcommand (JSON / CSV / Prometheus).
///
/// Gated on `cli` because it reuses the Prometheus exporters from
/// [`api::metrics`], which themselves depend on the CLI feature tree.
#[cfg(feature = "cli")]
pub mod snapshot;

/// Re-export of the snapshot entry point for programmatic use.
///
/// See [`snapshot::run`] for the full contract, including the note on
/// blocking-pool leaks when a reader times out. Embedding callers should
/// provision a conservative
/// [`tokio::runtime::Builder::max_blocking_threads`] to bound the blast
/// radius — the CLI in `main.rs` uses a dedicated short-lived runtime with
/// `max_blocking_threads(32)` per snapshot invocation.
#[cfg(feature = "cli")]
pub use snapshot::{
    Snapshot, SnapshotError, SnapshotHardFailure, SnapshotOptions, run as run_snapshot,
};

/// Utility functions.
pub mod utils;

/// Configuration module.
pub mod common {
    /// Configuration management.
    pub mod config;
}
