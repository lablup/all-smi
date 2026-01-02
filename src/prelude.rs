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

//! The all-smi prelude.
//!
//! This module provides convenient re-exports of commonly used types
//! for library users. Import everything with:
//!
//! ```rust
//! use all_smi::prelude::*;
//! ```
//!
//! This will import the main [`AllSmi`] client, error types, and all
//! the data structures returned by the API.
//!
//! # Example
//!
//! ```rust,no_run
//! use all_smi::prelude::*;
//!
//! fn main() -> Result<()> {
//!     let smi = AllSmi::new()?;
//!
//!     for gpu in smi.get_gpu_info() {
//!         println!("{}: {}% util", gpu.name, gpu.utilization);
//!     }
//!
//!     Ok(())
//! }
//! ```

// Main client API
pub use crate::client::{AllSmi, AllSmiConfig, DeviceType};

// Error types
pub use crate::error::{Error, Result};

// Core data types - GPU/NPU
pub use crate::device::{GpuInfo, ProcessInfo};

// Core data types - CPU
pub use crate::device::{
    AppleSiliconCpuInfo, CoreType, CoreUtilization, CpuInfo, CpuPlatformType, CpuSocketInfo,
};

// Core data types - Memory
pub use crate::device::MemoryInfo;

// Core data types - Chassis
pub use crate::device::{ChassisInfo, FanInfo, PsuInfo, PsuStatus};

// Traits for advanced usage
pub use crate::device::{ChassisReader, CpuReader, GpuReader, MemoryReader};

// Factory functions for advanced usage
pub use crate::device::{
    create_chassis_reader, get_cpu_readers, get_gpu_readers, get_memory_readers,
};
