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

//! Apple SMC (System Management Controller) bindings for macOS
//!
//! This module provides FFI bindings to the Apple SMC for reading:
//! - CPU and GPU temperatures
//! - System power (PSTR key)
//! - Fan speeds
//!
//! ## SMC Key Format
//! SMC keys are 4-character codes (FourCC) that identify specific sensors:
//! - `TC0P`, `TC0D`: CPU proximity/die temperature
//! - `TG0P`, `TG0D`: GPU proximity/die temperature
//! - `PSTR`: System power consumption
//! - `F0Ac`: Fan 0 actual speed
//!
//! ## References
//! - macmon project by vladkens
//! - stats project by exelban
//! - osx-cpu-temp project
//! - mactop project for dynamic key discovery

use std::ffi::c_void;
use std::sync::OnceLock;

/// Maximum number of temperature keys to discover per category (similar to mactop's limit)
const MAX_TEMP_KEYS: usize = 64;

/// Discovered temperature keys (CPU keys, GPU keys)
/// Using a single static prevents race conditions where one call discovers keys
/// but the other category's keys are discarded.
struct DiscoveredTempKeys {
    cpu_keys: Vec<String>,
    gpu_keys: Vec<String>,
}

static DISCOVERED_KEYS: OnceLock<DiscoveredTempKeys> = OnceLock::new();

// IOKit framework linkage
#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn mach_task_self() -> u32;
    fn IOServiceMatching(name: *const i8) -> *mut c_void;
    fn IOServiceGetMatchingService(master_port: u32, matching: *mut c_void) -> u32;
    fn IOServiceOpen(device: u32, owning_task: u32, conn_type: u32, conn: *mut u32) -> i32;
    fn IOServiceClose(conn: u32) -> i32;
    fn IOConnectCallStructMethod(
        conn: u32,
        selector: u32,
        input: *const c_void,
        input_size: usize,
        output: *mut c_void,
        output_size: *mut usize,
    ) -> i32;
}

/// SMC data type identifiers
const SMC_TYPE_UI8: u32 = u32::from_be_bytes(*b"ui8 ");
const SMC_TYPE_UI16: u32 = u32::from_be_bytes(*b"ui16");
const SMC_TYPE_UI32: u32 = u32::from_be_bytes(*b"ui32");
const SMC_TYPE_FLT: u32 = u32::from_be_bytes(*b"flt ");
const SMC_TYPE_SP78: u32 = u32::from_be_bytes(*b"sp78");
const SMC_TYPE_FP1F: u32 = u32::from_be_bytes(*b"fp1f");
const SMC_TYPE_FP2E: u32 = u32::from_be_bytes(*b"fp2e");
const SMC_TYPE_FP4C: u32 = u32::from_be_bytes(*b"fp4c");
const SMC_TYPE_FP5B: u32 = u32::from_be_bytes(*b"fp5b");
const SMC_TYPE_FP6A: u32 = u32::from_be_bytes(*b"fp6a");
const SMC_TYPE_FP79: u32 = u32::from_be_bytes(*b"fp79");
const SMC_TYPE_FP88: u32 = u32::from_be_bytes(*b"fp88");
const SMC_TYPE_FPA6: u32 = u32::from_be_bytes(*b"fpa6");
const SMC_TYPE_FPC4: u32 = u32::from_be_bytes(*b"fpc4");
const SMC_TYPE_FPE2: u32 = u32::from_be_bytes(*b"fpe2");

/// SMC command selectors
const SMC_CMD_READ_KEY: u8 = 5;
const SMC_CMD_READ_KEY_INFO: u8 = 9;
const SMC_CMD_READ_INDEX: u8 = 8;

/// SMC kernel selector
const KERNEL_INDEX_SMC: u32 = 2;

/// SMC key information structure
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct KeyInfo {
    data_size: u32,
    data_type: u32,
    data_attributes: u8,
}

/// SMC key data version
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct KeyDataVer {
    major: u8,
    minor: u8,
    build: u8,
    reserved: u8,
    release: u16,
}

/// SMC power limit data
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct PLimitData {
    version: u16,
    length: u16,
    cpu_p_limit: u32,
    gpu_p_limit: u32,
    mem_p_limit: u32,
}

/// SMC key data structure for communication with kernel
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct KeyData {
    key: u32,
    vers: KeyDataVer,
    p_limit_data: PLimitData,
    key_info: KeyInfo,
    result: u8,
    status: u8,
    data8: u8,
    data32: u32,
    bytes: [u8; 32],
}

/// Convert FourCC string to u32
fn str_to_fourcc(s: &str) -> u32 {
    let bytes = s.as_bytes();
    if bytes.len() != 4 {
        return 0;
    }
    u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

/// Apple SMC client
#[allow(clippy::upper_case_acronyms)]
pub struct SMC {
    conn: u32,
}

impl SMC {
    /// Open a connection to the SMC
    pub fn new() -> Result<Self, &'static str> {
        unsafe {
            let matching = IOServiceMatching(c"AppleSMC".as_ptr());
            if matching.is_null() {
                return Err("Failed to create IOService matching dictionary");
            }

            let device = IOServiceGetMatchingService(0, matching);
            if device == 0 {
                return Err("SMC device not found");
            }

            let mut conn: u32 = 0;
            let result = IOServiceOpen(device, mach_task_self(), 0, &mut conn);

            if result != 0 {
                return Err("Failed to open SMC connection");
            }

            Ok(Self { conn })
        }
    }

    /// Read raw data from SMC
    fn read(&self, input: &KeyData) -> Result<KeyData, &'static str> {
        unsafe {
            let mut output: KeyData = KeyData::default();
            let mut output_size = std::mem::size_of::<KeyData>();

            let result = IOConnectCallStructMethod(
                self.conn,
                KERNEL_INDEX_SMC,
                input as *const KeyData as *const c_void,
                std::mem::size_of::<KeyData>(),
                &mut output as *mut KeyData as *mut c_void,
                &mut output_size,
            );

            if result != 0 {
                return Err("SMC read failed");
            }

            Ok(output)
        }
    }

    /// Read key information
    fn read_key_info(&self, key: &str) -> Result<KeyInfo, &'static str> {
        let key_code = str_to_fourcc(key);

        let input = KeyData {
            key: key_code,
            data8: SMC_CMD_READ_KEY_INFO,
            ..Default::default()
        };

        let output = self.read(&input)?;

        Ok(output.key_info)
    }

    /// Get the total number of SMC keys
    ///
    /// Based on mactop's SMCGetKeyCount implementation
    pub fn get_key_count(&self) -> Result<u32, &'static str> {
        let key_code = str_to_fourcc("#KEY");

        let input = KeyData {
            key: key_code,
            data8: SMC_CMD_READ_KEY_INFO,
            ..Default::default()
        };

        let info_output = self.read(&input)?;

        let input = KeyData {
            key: key_code,
            key_info: KeyInfo {
                data_size: info_output.key_info.data_size,
                ..Default::default()
            },
            data8: SMC_CMD_READ_KEY,
            ..Default::default()
        };

        let output = self.read(&input)?;

        // Key count is stored as big-endian u32 in first 4 bytes
        let count = u32::from_be_bytes([
            output.bytes[0],
            output.bytes[1],
            output.bytes[2],
            output.bytes[3],
        ]);

        Ok(count)
    }

    /// Get key name by index
    ///
    /// Based on mactop's SMCGetKeyFromIndex implementation
    pub fn get_key_from_index(&self, index: u32) -> Result<String, &'static str> {
        let input = KeyData {
            data8: SMC_CMD_READ_INDEX,
            data32: index,
            ..Default::default()
        };

        let output = self.read(&input)?;

        // Key is stored as u32 in big-endian format
        let key = output.key;
        let key_bytes = key.to_be_bytes();

        // Convert to string (4-character FourCC code)
        let key_str = String::from_utf8_lossy(&key_bytes).to_string();

        Ok(key_str)
    }

    /// Discover all temperature keys dynamically
    ///
    /// Based on mactop's loadSMCTempKeys implementation:
    /// - Iterates through all SMC keys
    /// - Filters for 'flt ' type (float, dataType == 1718383648)
    /// - CPU keys: Tp* or Te* prefix
    /// - GPU keys: Tg* prefix
    ///
    /// Returns at most MAX_TEMP_KEYS per category to prevent unbounded growth.
    pub fn discover_temperature_keys(&self) -> (Vec<String>, Vec<String>) {
        let mut cpu_keys = Vec::with_capacity(MAX_TEMP_KEYS);
        let mut gpu_keys = Vec::with_capacity(MAX_TEMP_KEYS);

        let key_count = match self.get_key_count() {
            Ok(count) => count,
            Err(_) => return (cpu_keys, gpu_keys),
        };

        for i in 0..key_count {
            // Stop early if we've found enough keys in both categories
            if cpu_keys.len() >= MAX_TEMP_KEYS && gpu_keys.len() >= MAX_TEMP_KEYS {
                break;
            }

            let key = match self.get_key_from_index(i) {
                Ok(k) => k,
                Err(_) => continue,
            };

            // Get key info to check data type
            let key_info = match self.read_key_info(&key) {
                Ok(info) => info,
                Err(_) => continue,
            };

            // Filter for 'flt ' type (1718383648 == b"flt " as u32)
            if key_info.data_type != SMC_TYPE_FLT {
                continue;
            }

            let key_bytes = key.as_bytes();
            if key_bytes.len() < 2 {
                continue;
            }

            // CPU temperature keys: Tp* or Te*
            if key_bytes[0] == b'T'
                && (key_bytes[1] == b'p' || key_bytes[1] == b'e')
                && cpu_keys.len() < MAX_TEMP_KEYS
            {
                cpu_keys.push(key);
            }
            // GPU temperature keys: Tg*
            else if key_bytes[0] == b'T' && key_bytes[1] == b'g' && gpu_keys.len() < MAX_TEMP_KEYS
            {
                gpu_keys.push(key);
            }
        }

        (cpu_keys, gpu_keys)
    }

    /// Read a value from the SMC
    pub fn read_value(&mut self, key: &str) -> Result<f64, &'static str> {
        let key_info = self.read_key_info(key)?;
        let key_code = str_to_fourcc(key);

        let input = KeyData {
            key: key_code,
            key_info: KeyInfo {
                data_size: key_info.data_size,
                ..Default::default()
            },
            data8: SMC_CMD_READ_KEY,
            ..Default::default()
        };

        let output = self.read(&input)?;

        // Convert bytes to value based on data type
        let value = self.convert_value(&output.bytes, key_info.data_type, key_info.data_size);

        Ok(value)
    }

    /// Convert raw bytes to a floating point value based on SMC data type
    fn convert_value(&self, bytes: &[u8; 32], data_type: u32, data_size: u32) -> f64 {
        let size = data_size as usize;
        if size == 0 || size > 32 {
            return 0.0;
        }

        match data_type {
            SMC_TYPE_UI8 => bytes[0] as f64,
            SMC_TYPE_UI16 => u16::from_be_bytes([bytes[0], bytes[1]]) as f64,
            SMC_TYPE_UI32 => u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as f64,
            // Apple Silicon SMC stores `flt ` (IEEE 754 single-precision)
            // values in little-endian byte order, unlike the legacy fixed-point
            // types (SP78, FP*) which remain big-endian. This matches what
            // mactop and asitop do; using `from_be_bytes` here yields random
            // garbage that varies between calls because the bit pattern is
            // misinterpreted as a wildly different float.
            SMC_TYPE_FLT => f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as f64,
            SMC_TYPE_SP78 => {
                // Signed 7.8 fixed point
                let raw = i16::from_be_bytes([bytes[0], bytes[1]]);
                raw as f64 / 256.0
            }
            SMC_TYPE_FP1F => {
                // Unsigned 1.15 fixed point
                let raw = u16::from_be_bytes([bytes[0], bytes[1]]);
                raw as f64 / 32768.0
            }
            SMC_TYPE_FP2E => {
                // Unsigned 2.14 fixed point
                let raw = u16::from_be_bytes([bytes[0], bytes[1]]);
                raw as f64 / 16384.0
            }
            SMC_TYPE_FP4C => {
                // Unsigned 4.12 fixed point
                let raw = u16::from_be_bytes([bytes[0], bytes[1]]);
                raw as f64 / 4096.0
            }
            SMC_TYPE_FP5B => {
                // Unsigned 5.11 fixed point
                let raw = u16::from_be_bytes([bytes[0], bytes[1]]);
                raw as f64 / 2048.0
            }
            SMC_TYPE_FP6A => {
                // Unsigned 6.10 fixed point
                let raw = u16::from_be_bytes([bytes[0], bytes[1]]);
                raw as f64 / 1024.0
            }
            SMC_TYPE_FP79 => {
                // Unsigned 7.9 fixed point
                let raw = u16::from_be_bytes([bytes[0], bytes[1]]);
                raw as f64 / 512.0
            }
            SMC_TYPE_FP88 => {
                // Unsigned 8.8 fixed point
                let raw = u16::from_be_bytes([bytes[0], bytes[1]]);
                raw as f64 / 256.0
            }
            SMC_TYPE_FPA6 => {
                // Unsigned 10.6 fixed point
                let raw = u16::from_be_bytes([bytes[0], bytes[1]]);
                raw as f64 / 64.0
            }
            SMC_TYPE_FPC4 => {
                // Unsigned 12.4 fixed point
                let raw = u16::from_be_bytes([bytes[0], bytes[1]]);
                raw as f64 / 16.0
            }
            SMC_TYPE_FPE2 => {
                // Unsigned 14.2 fixed point
                let raw = u16::from_be_bytes([bytes[0], bytes[1]]);
                raw as f64 / 4.0
            }
            _ => {
                // Unknown type, try to interpret as simple bytes
                if size == 1 {
                    bytes[0] as f64
                } else if size == 2 {
                    u16::from_be_bytes([bytes[0], bytes[1]]) as f64
                } else if size >= 4 {
                    u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as f64
                } else {
                    0.0
                }
            }
        }
    }

    /// Get average CPU temperature
    ///
    /// First tries common static keys, then falls back to dynamically
    /// discovered keys if no readings are obtained.
    pub fn get_cpu_temperature(&mut self) -> Option<f64> {
        let mut temps: Vec<f64> = Vec::new();

        // Try common CPU temperature keys first
        let static_keys = [
            "Tp01", "Tp02", "Tp05", "Tp06", "Tp09", "Tp0A", "TC0P", "TC0D",
        ];

        for key in static_keys {
            if let Ok(value) = self.read_value(key)
                && (10.0..=120.0).contains(&value)
            {
                temps.push(value);
            }
        }

        // If static keys didn't work, try dynamically discovered keys
        if temps.is_empty() {
            let discovered = DISCOVERED_KEYS.get_or_init(|| {
                let (cpu_keys, gpu_keys) = self.discover_temperature_keys();
                DiscoveredTempKeys { cpu_keys, gpu_keys }
            });

            for key in &discovered.cpu_keys {
                if let Ok(value) = self.read_value(key)
                    && (10.0..=120.0).contains(&value)
                {
                    temps.push(value);
                }
            }
        }

        if temps.is_empty() {
            return None;
        }

        Some(temps.iter().sum::<f64>() / temps.len() as f64)
    }

    /// Get average GPU temperature
    ///
    /// First tries common static keys, then falls back to dynamically
    /// discovered keys if no readings are obtained.
    pub fn get_gpu_temperature(&mut self) -> Option<f64> {
        let mut temps: Vec<f64> = Vec::new();

        // Try common GPU temperature keys first
        let static_keys = ["Tg0f", "Tg0j", "TG0P", "TG0D"];

        for key in static_keys {
            if let Ok(value) = self.read_value(key)
                && (10.0..=120.0).contains(&value)
            {
                temps.push(value);
            }
        }

        // If static keys didn't work, try dynamically discovered keys
        if temps.is_empty() {
            let discovered = DISCOVERED_KEYS.get_or_init(|| {
                let (cpu_keys, gpu_keys) = self.discover_temperature_keys();
                DiscoveredTempKeys { cpu_keys, gpu_keys }
            });

            for key in &discovered.gpu_keys {
                if let Ok(value) = self.read_value(key)
                    && (10.0..=120.0).contains(&value)
                {
                    temps.push(value);
                }
            }
        }

        if temps.is_empty() {
            return None;
        }

        Some(temps.iter().sum::<f64>() / temps.len() as f64)
    }

    /// Read system power (PSTR key)
    pub fn get_system_power(&mut self) -> Option<f64> {
        self.read_value("PSTR").ok()
    }

    /// Read fan speeds
    pub fn get_fan_speeds(&mut self) -> Vec<(String, u32)> {
        let mut fans = Vec::new();

        // Try to read fan count
        let fan_count = match self.read_value("FNum") {
            Ok(v) => v as u32,
            Err(_) => 2, // Default to checking 2 fans
        };

        for i in 0..fan_count.min(8) {
            let key = format!("F{i}Ac");
            if let Ok(speed) = self.read_value(&key) {
                fans.push((format!("Fan {i}"), speed as u32));
            }
        }

        fans
    }
}

impl Drop for SMC {
    fn drop(&mut self) {
        unsafe {
            IOServiceClose(self.conn);
        }
    }
}

// Safety: SMC uses IOKit which is thread-safe
unsafe impl Send for SMC {}

/// SMC metrics collection result
#[derive(Debug, Default, Clone)]
pub struct SMCMetrics {
    pub cpu_temperature: Option<f64>,
    pub gpu_temperature: Option<f64>,
    pub system_power: Option<f64>,
    pub fan_speeds: Vec<(String, u32)>,
}

impl SMCMetrics {
    /// Collect all SMC metrics
    pub fn collect() -> Self {
        let mut metrics = Self::default();

        if let Ok(mut smc) = SMC::new() {
            metrics.cpu_temperature = smc.get_cpu_temperature();
            metrics.gpu_temperature = smc.get_gpu_temperature();
            metrics.system_power = smc.get_system_power();
            metrics.fan_speeds = smc.get_fan_speeds();
        }

        metrics
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fourcc_conversion() {
        assert_eq!(str_to_fourcc("TC0P"), u32::from_be_bytes(*b"TC0P"));
        assert_eq!(str_to_fourcc("PSTR"), u32::from_be_bytes(*b"PSTR"));
    }

    #[test]
    fn test_invalid_fourcc() {
        assert_eq!(str_to_fourcc("ABC"), 0); // Too short
        assert_eq!(str_to_fourcc("ABCDE"), 0); // Too long
    }

    #[test]
    fn test_smc_type_flt_constant() {
        // Verify SMC_TYPE_FLT matches the expected value from mactop (1718383648)
        assert_eq!(SMC_TYPE_FLT, 1718383648);
        assert_eq!(SMC_TYPE_FLT, u32::from_be_bytes(*b"flt "));
    }

    #[test]
    fn test_hash_key_fourcc() {
        // Test the #KEY special key used for key count
        assert_eq!(str_to_fourcc("#KEY"), u32::from_be_bytes(*b"#KEY"));
    }

    /// Sanity-check little-endian decoding of the `flt ` SMC type. Apple
    /// Silicon stores temperature sensor floats as little-endian; if this
    /// regresses we get garbage values like 1e-32 or 3e36 instead of real
    /// temperatures (the symptom that motivated this conversion).
    #[test]
    fn test_flt_little_endian_decoding() {
        let smc = SMC { conn: 0 }; // convert_value doesn't touch the connection
        let mut bytes = [0u8; 32];
        // 51.2°C as IEEE 754 single = 0x424ccccd
        bytes[0..4].copy_from_slice(&0x424ccccd_u32.to_le_bytes());
        let value = smc.convert_value(&bytes, SMC_TYPE_FLT, 4);
        assert!(
            (value - 51.2).abs() < 0.01,
            "expected ~51.2, got {value} — float endianness may have regressed"
        );
    }
}
