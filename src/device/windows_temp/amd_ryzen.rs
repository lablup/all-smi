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

//! AMD Ryzen Master SDK temperature source.
//!
//! Uses the AMD Ryzen Master Monitoring DLL to get CPU temperature.
//! The DLL is typically installed with AMD chipset drivers or Ryzen Master app.
//!
//! Reference: https://www.amd.com/en/developer/ryzen-master-monitoring-sdk.html
//!
//! ## Security Note
//! This module only loads DLLs from absolute paths in standard AMD installation
//! directories to prevent DLL hijacking attacks. Relative paths are not used.
//!
//! ## Thread Safety
//! The AMD Ryzen Master SDK is assumed to be thread-safe for read operations.
//! All SDK calls are protected by a RwLock for additional safety.

use super::TemperatureResult;
use libloading::{Library, Symbol};
use once_cell::sync::OnceCell;
use std::ffi::c_int;
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

/// Helper to get read lock, recovering from poisoned state.
/// This prevents the application from panicking if another thread panicked while holding the lock.
fn read_lock<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Helper to get write lock, recovering from poisoned state.
fn write_lock<T>(lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    lock.write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Paths to search for the AMD Ryzen Master Monitoring DLL.
///
/// SECURITY: Only absolute paths are used to prevent DLL hijacking attacks.
/// We do NOT search the current directory or relative paths.
const AMD_DLL_PATHS: &[&str] = &[
    // Default AMD driver installation path (64-bit)
    "C:\\Program Files\\AMD\\RyzenMaster\\bin\\AMDRyzenMasterMonitoringDLL.dll",
    // Alternative path (32-bit installation on 64-bit Windows)
    "C:\\Program Files (x86)\\AMD\\RyzenMaster\\bin\\AMDRyzenMasterMonitoringDLL.dll",
    // AMD chipset driver installation path
    "C:\\Program Files\\AMD\\CNext\\CNext\\AMDRyzenMasterMonitoringDLL.dll",
];

/// Quick stats structure from the AMD Ryzen Master Monitoring SDK.
///
/// This structure matches the `RMQuickStats` struct from the official AMD SDK.
/// Reference: AMD Ryzen Master Monitoring SDK v2.6.0
///
/// ## Layout Verification
/// The struct uses `#[repr(C)]` to ensure C-compatible memory layout.
/// Expected size: 7 fields × 8 bytes (f64) = 56 bytes on all platforms.
///
/// ## Field Order
/// The field order matches the official SDK header file. If the SDK is updated,
/// this struct may need to be updated to match.
///
/// ## Safety
/// This struct is used in FFI calls to the AMD SDK. Incorrect layout would cause
/// memory corruption. The compile-time size assertion below helps catch mismatches.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct RmQuickStats {
    /// CPU temperature in Celsius
    pub d_temperature: f64,
    /// Peak core speed in MHz
    pub d_peak_core_speed: f64,
    /// Average core speed in MHz
    pub d_avg_core_speed: f64,
    /// Fabric clock in MHz (if available)
    pub d_fabric_clock: f64,
    /// Memory clock in MHz (if available)
    pub d_mem_clock: f64,
    /// CPU power in Watts
    pub d_cpu_power: f64,
    /// SoC power in Watts (if available)
    pub d_soc_power: f64,
}

// Compile-time size verification to catch layout mismatches
const _: () = assert!(
    std::mem::size_of::<RmQuickStats>() == 56,
    "RmQuickStats size mismatch - expected 56 bytes (7 × f64)"
);

/// Type alias for PlatformInit function.
type PlatformInitFn = unsafe extern "C" fn() -> c_int;

/// Type alias for ShortQuery function.
type ShortQueryFn = unsafe extern "C" fn(*mut RmQuickStats) -> c_int;

/// Type alias for PlatformUninit function.
type PlatformUninitFn = unsafe extern "C" fn() -> c_int;

/// Cached AMD library state.
struct AmdLibraryState {
    library: Library,
    initialized: bool,
}

/// AMD Ryzen Master SDK temperature source.
pub struct AmdRyzenSource {
    /// Cached library state
    library_state: RwLock<Option<AmdLibraryState>>,
    /// Whether we've already tried to load the library
    load_attempted: OnceCell<bool>,
}

impl Default for AmdRyzenSource {
    fn default() -> Self {
        Self::new()
    }
}

impl AmdRyzenSource {
    /// Create a new AMD Ryzen source.
    pub fn new() -> Self {
        Self {
            library_state: RwLock::new(None),
            load_attempted: OnceCell::new(),
        }
    }

    /// Attempt to load the AMD DLL.
    fn try_load_library(&self) -> bool {
        // Only attempt to load once
        *self.load_attempted.get_or_init(|| self.do_load_library())
    }

    /// Actually load the library (called once).
    fn do_load_library(&self) -> bool {
        for path in AMD_DLL_PATHS {
            // SAFETY: Loading a shared library is inherently unsafe.
            // We trust that the AMD DLL at the known paths is valid.
            match unsafe { Library::new(*path) } {
                Ok(lib) => {
                    // Try to initialize the platform
                    // SAFETY: We're calling the documented PlatformInit function.
                    let init_result: Result<Symbol<PlatformInitFn>, _> =
                        unsafe { lib.get(b"PlatformInit\0") };

                    if let Ok(init_fn) = init_result {
                        // SAFETY: Calling the PlatformInit function from the loaded library.
                        let result = unsafe { init_fn() };
                        if result == 0 {
                            // Successfully initialized
                            *write_lock(&self.library_state) = Some(AmdLibraryState {
                                library: lib,
                                initialized: true,
                            });
                            return true;
                        }
                    }
                    // Initialization failed, continue to next path
                }
                Err(_) => continue,
            }
        }
        false
    }

    /// Get temperature from AMD Ryzen Master SDK.
    ///
    /// # Returns
    /// * `TemperatureResult::Success(temp)` - Temperature in Celsius
    /// * `TemperatureResult::NotFound` - DLL not found or not installed
    /// * `TemperatureResult::Error` - Transient error during query
    /// * `TemperatureResult::NoValidReading` - Query succeeded but returned invalid data
    pub fn get_temperature(&self) -> TemperatureResult {
        // Try to load if not already attempted
        if !self.try_load_library() {
            return TemperatureResult::NotFound;
        }

        let state_guard = read_lock(&self.library_state);
        let state = match state_guard.as_ref() {
            Some(s) if s.initialized => s,
            _ => return TemperatureResult::NotFound,
        };

        // Get the ShortQuery function
        // SAFETY: We're getting a symbol from a successfully loaded library.
        let query_fn: Result<Symbol<ShortQueryFn>, _> =
            unsafe { state.library.get(b"ShortQuery\0") };

        match query_fn {
            Ok(query) => {
                let mut stats = RmQuickStats::default();
                // SAFETY: Calling the documented ShortQuery function with a valid pointer.
                let result = unsafe { query(&mut stats) };

                if result == 0 {
                    // Validate the temperature reading
                    let temp = stats.d_temperature;
                    if temp > 0.0 && temp < 150.0 {
                        // Use round() for more accurate conversion
                        return TemperatureResult::Success(temp.round() as u32);
                    }
                    TemperatureResult::NoValidReading
                } else {
                    TemperatureResult::Error
                }
            }
            Err(_) => TemperatureResult::Error,
        }
    }
}

impl Drop for AmdRyzenSource {
    fn drop(&mut self) {
        // Clean up the AMD library
        if let Some(state) = write_lock(&self.library_state).take() {
            if state.initialized {
                // SAFETY: We're calling PlatformUninit to clean up resources.
                let uninit_fn: Result<Symbol<PlatformUninitFn>, _> =
                    unsafe { state.library.get(b"PlatformUninit\0") };

                if let Ok(uninit) = uninit_fn {
                    // SAFETY: Calling the documented cleanup function.
                    unsafe {
                        uninit();
                    }
                }
            }
            // Library is dropped when state goes out of scope
        }
    }
}
