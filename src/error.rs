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

//! Unified error types for the all-smi library.
//!
//! This module provides a comprehensive error hierarchy for library users,
//! covering platform initialization, device access, and I/O operations.
//!
//! # Example
//!
//! ```rust,no_run
//! use all_smi::{AllSmi, Error, Result};
//!
//! fn main() -> Result<()> {
//!     let smi = AllSmi::new()?;
//!     let gpus = smi.get_gpu_info();
//!     println!("Found {} GPU(s)", gpus.len());
//!     Ok(())
//! }
//! ```

use thiserror::Error;

/// The main error type for all-smi library operations.
///
/// This enum covers all possible error conditions that can occur when
/// using the all-smi library, from initialization failures to device
/// access issues.
#[derive(Debug, Error)]
pub enum Error {
    /// Platform initialization failed.
    ///
    /// This error occurs when the underlying platform-specific libraries
    /// or APIs cannot be initialized (e.g., NVML, IOReport, hl-smi).
    #[error("Platform initialization failed: {0}")]
    PlatformInit(String),

    /// No supported devices were found on the system.
    ///
    /// This is not necessarily an error condition - it simply indicates
    /// that no GPUs, NPUs, or other accelerators were detected.
    #[error("No supported devices found")]
    NoDevicesFound,

    /// Device access error occurred.
    ///
    /// This error occurs when a device was detected but could not be
    /// accessed or queried for metrics.
    #[error("Device access error: {0}")]
    DeviceAccess(String),

    /// Permission denied when accessing device resources.
    ///
    /// Some platforms require elevated privileges to access certain
    /// metrics (e.g., AMD GPUs on Linux require sudo access).
    #[error("Permission denied: {0}")]
    PermissionDenied(String),

    /// Feature not supported on this platform.
    ///
    /// This error is returned when attempting to use functionality
    /// that is not available on the current platform or hardware.
    #[error("Feature not supported on this platform: {0}")]
    NotSupported(String),

    /// An I/O error occurred.
    ///
    /// This wraps standard I/O errors that may occur during file
    /// system operations or process execution.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// A specialized Result type for all-smi operations.
///
/// This type alias simplifies error handling by using the library's
/// unified [`Error`] type.
///
/// # Example
///
/// ```rust,no_run
/// use all_smi::{AllSmi, Result};
///
/// fn get_gpu_count() -> Result<usize> {
///     let smi = AllSmi::new()?;
///     Ok(smi.get_gpu_info().len())
/// }
/// ```
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_display() {
        let err = Error::PlatformInit("NVML not found".to_string());
        assert_eq!(
            err.to_string(),
            "Platform initialization failed: NVML not found"
        );

        let err = Error::NoDevicesFound;
        assert_eq!(err.to_string(), "No supported devices found");

        let err = Error::DeviceAccess("GPU 0 not responding".to_string());
        assert_eq!(err.to_string(), "Device access error: GPU 0 not responding");

        let err = Error::PermissionDenied("Cannot access /dev/dri".to_string());
        assert_eq!(err.to_string(), "Permission denied: Cannot access /dev/dri");

        let err = Error::NotSupported("ANE metrics".to_string());
        assert_eq!(
            err.to_string(),
            "Feature not supported on this platform: ANE metrics"
        );
    }

    #[test]
    fn test_error_from_io() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        let err: Error = io_err.into();
        assert!(matches!(err, Error::Io(_)));
    }

    #[test]
    fn test_error_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Error>();
    }
}
