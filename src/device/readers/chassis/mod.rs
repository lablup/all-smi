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

//! Chassis/Node-level monitoring module
//!
//! This module provides readers for chassis-level metrics including:
//! - Total power consumption (CPU+GPU+ANE)
//! - Thermal data (inlet/outlet temperature, thermal pressure)
//! - Cooling information (fan speeds)
//! - PSU status

// Native Apple Silicon chassis reader using IOReport/SMC (no sudo required)
#[cfg(target_os = "macos")]
mod apple_silicon_native;

// Intel Mac chassis reader using the SMC and NSProcessInfo (no sudo required)
#[cfg(target_os = "macos")]
mod intel_mac;

mod generic;

#[cfg(target_os = "macos")]
pub use apple_silicon_native::AppleSiliconNativeChassisReader;

#[cfg(target_os = "macos")]
pub use intel_mac::IntelMacChassisReader;

#[allow(unused_imports)]
pub use generic::GenericChassisReader;

use crate::device::ChassisReader;

/// Create a platform-appropriate chassis reader
pub fn create_chassis_reader() -> Box<dyn ChassisReader> {
    // On macOS, use native APIs (no sudo required)
    #[cfg(target_os = "macos")]
    {
        // The Apple Silicon reader depends on the IOReport-backed native
        // metrics manager, which never initializes on an Intel Mac. Choosing it
        // there produced no chassis block at all; Intel Macs get an SMC-backed
        // reader instead.
        if crate::device::is_intel_mac() {
            Box::new(IntelMacChassisReader::new())
        } else {
            Box::new(AppleSiliconNativeChassisReader::new())
        }
    }

    #[cfg(not(target_os = "macos"))]
    {
        // On other platforms, use generic reader that aggregates GPU power
        Box::new(GenericChassisReader::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_chassis_reader() {
        let reader = create_chassis_reader();
        // Just verify we can create a reader without panicking
        let _ = reader.get_chassis_info();
    }

    /// Reader selection must follow the architecture, because the Apple
    /// Silicon reader silently yields nothing on Intel hardware.
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_reader_selection_follows_architecture() {
        let reader = create_chassis_reader();
        let platform = reader
            .get_chassis_info()
            .and_then(|info| info.detail.get("platform").cloned());

        if crate::device::is_intel_mac() {
            assert_eq!(platform.as_deref(), Some("Intel Mac"));
        } else {
            // On Apple Silicon the reader needs the native metrics manager,
            // which the test binary does not initialize, so it may legitimately
            // report nothing. It must never claim to be an Intel Mac.
            assert_ne!(platform.as_deref(), Some("Intel Mac"));
        }
    }
}
