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

//! Thermal state monitoring for macOS
//!
//! This module provides access to the system thermal state using
//! NSProcessInfo.thermalState, which indicates the current thermal
//! pressure level on the system.
//!
//! ## Thermal States
//! - Nominal: Normal operating conditions
//! - Fair: Elevated thermal load
//! - Serious: High thermal load, system may throttle
//! - Critical: Maximum thermal load, heavy throttling
//!
//! ## References
//! - Apple Developer Documentation: NSProcessInfo.thermalState

use std::ffi::c_void;

// Objective-C runtime linkage
#[link(name = "objc", kind = "dylib")]
unsafe extern "C" {
    fn objc_getClass(name: *const i8) -> *mut c_void;
    fn sel_registerName(name: *const i8) -> *mut c_void;
    fn objc_msgSend(receiver: *mut c_void, selector: *mut c_void, ...) -> *mut c_void;
}

/// Thermal state levels as defined by NSProcessInfo
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i64)]
pub enum ThermalState {
    /// The thermal state is within normal limits
    Nominal = 0,
    /// The thermal state is slightly elevated
    Fair = 1,
    /// The thermal state is high
    Serious = 2,
    /// The thermal state is significantly impacting performance
    Critical = 3,
}

impl ThermalState {
    /// Convert from raw integer value
    pub fn from_raw(value: i64) -> Self {
        match value {
            0 => ThermalState::Nominal,
            1 => ThermalState::Fair,
            2 => ThermalState::Serious,
            3 => ThermalState::Critical,
            _ => ThermalState::Nominal, // Default to nominal for unknown values
        }
    }

    /// Get the string representation for thermal state
    pub fn as_str(&self) -> &'static str {
        match self {
            ThermalState::Nominal => "Nominal",
            ThermalState::Fair => "Fair",
            ThermalState::Serious => "Serious",
            ThermalState::Critical => "Critical",
        }
    }

    /// Check if the system is under thermal pressure
    #[cfg_attr(not(test), allow(dead_code))] // Only the tests call it.
    pub fn is_throttling(&self) -> bool {
        matches!(self, ThermalState::Serious | ThermalState::Critical)
    }
}

impl std::fmt::Display for ThermalState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Get the current system thermal state
///
/// This uses the NSProcessInfo.thermalState property to get the current
/// thermal pressure level on the system.
///
/// # Returns
/// The current thermal state, or Nominal if the state cannot be determined.
///
/// # Safety
/// This function uses Objective-C runtime FFI which requires proper
/// initialization, but NSProcessInfo is always available on macOS.
pub fn get_thermal_state() -> ThermalState {
    unsafe {
        // Get NSProcessInfo class
        let class = objc_getClass(c"NSProcessInfo".as_ptr());
        if class.is_null() {
            return ThermalState::Nominal;
        }

        // Get processInfo selector
        let process_info_sel = sel_registerName(c"processInfo".as_ptr());
        if process_info_sel.is_null() {
            return ThermalState::Nominal;
        }

        // Get shared process info instance
        let process_info = objc_msgSend(class, process_info_sel);
        if process_info.is_null() {
            return ThermalState::Nominal;
        }

        // Get thermalState selector
        let thermal_state_sel = sel_registerName(c"thermalState".as_ptr());
        if thermal_state_sel.is_null() {
            return ThermalState::Nominal;
        }

        // Get thermal state value
        let state = objc_msgSend(process_info, thermal_state_sel) as i64;
        ThermalState::from_raw(state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_thermal_state_from_raw() {
        assert_eq!(ThermalState::from_raw(0), ThermalState::Nominal);
        assert_eq!(ThermalState::from_raw(1), ThermalState::Fair);
        assert_eq!(ThermalState::from_raw(2), ThermalState::Serious);
        assert_eq!(ThermalState::from_raw(3), ThermalState::Critical);
        assert_eq!(ThermalState::from_raw(99), ThermalState::Nominal); // Unknown
    }

    #[test]
    fn test_thermal_state_strings() {
        assert_eq!(ThermalState::Nominal.as_str(), "Nominal");
        assert_eq!(ThermalState::Fair.as_str(), "Fair");
        assert_eq!(ThermalState::Serious.as_str(), "Serious");
        assert_eq!(ThermalState::Critical.as_str(), "Critical");
    }

    #[test]
    fn test_is_throttling() {
        assert!(!ThermalState::Nominal.is_throttling());
        assert!(!ThermalState::Fair.is_throttling());
        assert!(ThermalState::Serious.is_throttling());
        assert!(ThermalState::Critical.is_throttling());
    }

    #[test]
    fn test_display_trait() {
        assert_eq!(format!("{}", ThermalState::Nominal), "Nominal");
        assert_eq!(format!("{}", ThermalState::Critical), "Critical");
    }
}
