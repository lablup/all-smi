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

/// Upper bound on discovered CPU temperature keys (`Tp*`/`Te*` on Apple
/// Silicon, `TC*` on Intel).
///
/// This is a runaway guard, not a sampling budget: it is sized so no shipping
/// Mac reaches it, which keeps the reported average over the complete sensor
/// set rather than over an arbitrary prefix of the key table. Measured counts:
/// 23 on an M5 Max, single digits on Intel.
const MAX_CPU_TEMP_KEYS: usize = 256;

/// Upper bound on discovered GPU temperature keys (`Tg*` on Apple Silicon,
/// `TG*` on Intel).
///
/// Separate from [`MAX_CPU_TEMP_KEYS`] because the two families have very
/// different real counts: an M5 Max exposes 84 `Tg*` sensors against 23
/// `Tp*`/`Te*` ones. The previous shared cap of 64 truncated that set, so the
/// reported GPU temperature averaged whichever 64 sensors the key table
/// happened to list first. Sized to leave headroom above the largest known
/// part (an Ultra is roughly two Max dies) so truncation stays theoretical.
const MAX_GPU_TEMP_KEYS: usize = 512;

/// Maximum number of fans to probe. No Mac ships with more than a handful.
const MAX_FANS: u32 = 8;

/// Upper bound for a plausible fan speed in RPM. Readings above this come from
/// a key that is missing or holds something other than a tachometer value.
const MAX_PLAUSIBLE_FAN_RPM: f64 = 20_000.0;

/// Upper bound for a plausible whole-machine or CPU-package power reading in
/// watts. The largest Mac Pro power supply is well under this.
const MAX_PLAUSIBLE_POWER_WATTS: f64 = 2_000.0;

/// True when `value` is a plausible whole-machine or CPU-package power
/// reading in watts (finite and within `0.0..=MAX_PLAUSIBLE_POWER_WATTS`).
///
/// Guards `get_system_power` and `get_cpu_package_power` against a missing or
/// differently-typed SMC key surfacing as a bogus metric: a wrong data type
/// can decode to `NaN`, a huge magnitude, or a negative value, none of which
/// is a real power draw.
fn is_plausible_power_watts(value: f64) -> bool {
    value.is_finite() && (0.0..=MAX_PLAUSIBLE_POWER_WATTS).contains(&value)
}

/// True when `value` is a plausible fan speed in RPM (finite and within
/// `0.0..=MAX_PLAUSIBLE_FAN_RPM`). Same rationale as
/// [`is_plausible_power_watts`], applied to `get_fan_readings`.
fn is_plausible_fan_rpm(value: f64) -> bool {
    value.is_finite() && (0.0..=MAX_PLAUSIBLE_FAN_RPM).contains(&value)
}

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
    fn IOObjectRelease(object: u32) -> i32;
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

/// Sensor category a discovered SMC temperature key belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TempKeyCategory {
    Cpu,
    Gpu,
}

/// Classify an SMC key as a CPU or GPU temperature sensor.
///
/// Two disjoint sensor families are recognized, and each one is pinned to the
/// data type it actually uses. That pairing is what keeps Intel support from
/// disturbing Apple Silicon: the two naming schemes differ in the case of the
/// second character, so no key can be claimed by both families.
///
/// - Apple Silicon uses `flt ` (IEEE 754) sensors named `Tp*` (performance
///   die), `Te*` (efficiency die) and `Tg*` (GPU die).
/// - Intel uses `sp78` fixed-point sensors named `TC*` (CPU proximity/die, for
///   example TC0P and TC0D) and `TG*` (GPU proximity/die, TG0P and TG0D).
///
/// Every discovered reading is still range-filtered by the callers, so a
/// misclassified sensor cannot pull an average outside plausible temperatures.
fn classify_temperature_key(key: &str, data_type: u32) -> Option<TempKeyCategory> {
    if !is_temperature_key_candidate(key) {
        return None;
    }
    let bytes = key.as_bytes();

    match data_type {
        SMC_TYPE_FLT => match bytes[1] {
            b'p' | b'e' => Some(TempKeyCategory::Cpu),
            b'g' => Some(TempKeyCategory::Gpu),
            _ => None,
        },
        SMC_TYPE_SP78 => match bytes[1] {
            b'C' => Some(TempKeyCategory::Cpu),
            b'G' => Some(TempKeyCategory::Gpu),
            _ => None,
        },
        _ => None,
    }
}

/// True when `key`'s *name alone* leaves any chance that
/// [`classify_temperature_key`] accepts it.
///
/// Discovery learns a key's name and its data type through two separate IOKit
/// round trips, and the name is the cheaper of the two. Every key whose name
/// already rules out both sensor families can therefore skip the second round
/// trip entirely. On an M5 Max that is 3,623 of 3,739 keys.
///
/// [`classify_temperature_key`] delegates its name test here so the two can
/// never disagree: a name this rejects is a name the classifier rejects.
fn is_temperature_key_candidate(key: &str) -> bool {
    let bytes = key.as_bytes();
    bytes.len() >= 2 && bytes[0] == b'T' && matches!(bytes[1], b'p' | b'e' | b'g' | b'C' | b'G')
}

/// FourCC of the lowest key name any temperature sensor can have (`T\0\0\0`).
const TEMP_KEY_RANGE_START: u32 = u32::from_be_bytes([b'T', 0, 0, 0]);

/// FourCC one past the highest key name any temperature sensor can have
/// (`U\0\0\0`). Every candidate name starts with `T`, so the sorted key table
/// confines them to `TEMP_KEY_RANGE_START..TEMP_KEY_RANGE_END`.
const TEMP_KEY_RANGE_END: u32 = u32::from_be_bytes([b'U', 0, 0, 0]);

/// Result of one pass over the SMC key table, including how much of the table
/// the pass had to touch.
///
/// The counts exist so callers (and tests) can assert that discovery stops
/// short of the whole table; they are not used to compute any metric.
#[derive(Debug, Clone, Default)]
pub struct TempKeyScan {
    /// Discovered CPU sensor keys, in key-table order.
    pub cpu_keys: Vec<String>,
    /// Discovered GPU sensor keys, in key-table order.
    pub gpu_keys: Vec<String>,
    /// Key-table indices actually read, including binary-search probes.
    pub scanned_keys: u32,
    /// Total number of keys the SMC reports.
    pub total_keys: u32,
    /// Whether the sorted-table fast path produced this result. False means
    /// the exhaustive fallback ran. Diagnostic only; no metric depends on it.
    #[allow(dead_code)]
    pub used_sorted_range: bool,
}

/// Convert a FourCC code back to its 4-character name.
fn fourcc_to_string(key: u32) -> String {
    String::from_utf8_lossy(&key.to_be_bytes()).to_string()
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

            // `IOServiceGetMatchingService` returns a +1-retained object that
            // the caller owns. `IOServiceOpen` creates an independent user
            // client and does not adopt that reference, so the service handle
            // has to be released on both outcomes or every call leaks a mach
            // port right. `SMCMetrics::collect` opens a fresh connection once
            // per collection cycle, so the leak was unbounded over the
            // lifetime of a long-running `view` or `api` process.
            IOObjectRelease(device);

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

    /// Get the raw FourCC code of the key at `index`.
    ///
    /// The undecoded `u32` is what the key table is ordered by, so range
    /// queries compare these rather than the lossily-decoded names returned by
    /// [`get_key_from_index`].
    fn get_key_code_from_index(&self, index: u32) -> Result<u32, &'static str> {
        let input = KeyData {
            data8: SMC_CMD_READ_INDEX,
            data32: index,
            ..Default::default()
        };

        Ok(self.read(&input)?.key)
    }

    /// Get key name by index
    ///
    /// Based on mactop's SMCGetKeyFromIndex implementation
    ///
    /// Part of this module's public surface; discovery itself compares the
    /// undecoded codes from [`get_key_code_from_index`](Self::get_key_code_from_index).
    #[allow(dead_code)]
    pub fn get_key_from_index(&self, index: u32) -> Result<String, &'static str> {
        Ok(fourcc_to_string(self.get_key_code_from_index(index)?))
    }

    /// Discover all temperature keys dynamically
    ///
    /// Keeps the keys that [`classify_temperature_key`] recognizes as CPU or
    /// GPU sensors, capped per category by [`MAX_CPU_TEMP_KEYS`] and
    /// [`MAX_GPU_TEMP_KEYS`].
    ///
    /// See [`scan_temperature_keys`](Self::scan_temperature_keys) for how much
    /// of the key table this has to read.
    pub fn discover_temperature_keys(&self) -> (Vec<String>, Vec<String>) {
        let scan = self.scan_temperature_keys();
        (scan.cpu_keys, scan.gpu_keys)
    }

    /// Discover temperature keys and report how much of the key table was read.
    ///
    /// The SMC key table is ordered by FourCC, so every candidate name (all of
    /// which start with `T`) lives in one contiguous span. This binary-searches
    /// for the start of that span and walks only to its end, which on an M5 Max
    /// touches 359 of 3,739 indices instead of all of them. Within the span,
    /// [`is_temperature_key_candidate`] rejects most names outright so the
    /// second round trip (`read_key_info`) is spent only on plausible keys.
    ///
    /// Ordering is checked, not assumed. Nothing short of reading the whole
    /// table can prove it, so this takes the two cheap checks available and
    /// backstops them: the indices the binary search already read must be
    /// non-decreasing, the key below the span must sort below it, and a span
    /// that yields no sensors at all is treated as a failed assumption rather
    /// than as a machine with no sensors. Any of those falling through runs the
    /// exhaustive scan that predates this, which is the authority on what the
    /// table contains. The fallback also skips the second round trip for
    /// non-candidate names, so it is cheaper than the original was.
    pub fn scan_temperature_keys(&self) -> TempKeyScan {
        let total_keys = match self.get_key_count() {
            Ok(count) => count,
            Err(_) => return TempKeyScan::default(),
        };

        if let Some(scan) = self.scan_sorted_temperature_range(total_keys)
            && !(scan.cpu_keys.is_empty() && scan.gpu_keys.is_empty())
        {
            return scan;
        }

        self.scan_all_keys(total_keys)
    }

    /// Fast path: locate the `T*` span in a FourCC-ordered key table and read
    /// only that span.
    ///
    /// Returns `None` when the table does not behave like a sorted one, which
    /// hands the caller back to [`scan_all_keys`](Self::scan_all_keys).
    fn scan_sorted_temperature_range(&self, total_keys: u32) -> Option<TempKeyScan> {
        if total_keys == 0 {
            return None;
        }

        let mut scanned_keys = 0u32;
        // Probes recorded as (index, code); used to verify the table really is
        // ordered before its ordering is relied on.
        let mut probes: Vec<(u32, u32)> = Vec::new();

        // Lower bound: first index whose key is >= "T\0\0\0".
        let (mut lo, mut hi) = (0u32, total_keys);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let code = self.get_key_code_from_index(mid).ok()?;
            scanned_keys += 1;
            probes.push((mid, code));

            if code < TEMP_KEY_RANGE_START {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }

        // Confirm the lower edge: the key before the span must sort below it.
        if lo > 0 {
            let code = self.get_key_code_from_index(lo - 1).ok()?;
            scanned_keys += 1;
            probes.push((lo - 1, code));
            if code >= TEMP_KEY_RANGE_START {
                return None;
            }
        }

        // The probes must be consistent with an ordered table. Sorting by index
        // and requiring non-decreasing codes catches a table that is not
        // ordered the way the search assumed.
        probes.sort_unstable_by_key(|(index, _)| *index);
        if probes.windows(2).any(|w| w[0].1 > w[1].1) {
            return None;
        }

        let mut cpu_keys = Vec::new();
        let mut gpu_keys = Vec::new();

        for index in lo..total_keys {
            let code = match self.get_key_code_from_index(index) {
                Ok(code) => code,
                // Tolerated the same way the exhaustive scan tolerates it.
                // The span's end is decided by the next key that reads back,
                // so a gap costs extra indices but cannot end the walk early.
                Err(_) => {
                    scanned_keys += 1;
                    continue;
                }
            };
            scanned_keys += 1;

            if code >= TEMP_KEY_RANGE_END {
                break;
            }

            self.classify_key_at(code, &mut cpu_keys, &mut gpu_keys);

            if cpu_keys.len() >= MAX_CPU_TEMP_KEYS && gpu_keys.len() >= MAX_GPU_TEMP_KEYS {
                break;
            }
        }

        Some(TempKeyScan {
            cpu_keys,
            gpu_keys,
            scanned_keys,
            total_keys,
            used_sorted_range: true,
        })
    }

    /// Fallback: walk the whole key table, as this did before the span search.
    fn scan_all_keys(&self, total_keys: u32) -> TempKeyScan {
        let mut cpu_keys = Vec::new();
        let mut gpu_keys = Vec::new();
        let mut scanned_keys = 0u32;

        for index in 0..total_keys {
            // Nothing left that either category can accept.
            if cpu_keys.len() >= MAX_CPU_TEMP_KEYS && gpu_keys.len() >= MAX_GPU_TEMP_KEYS {
                break;
            }

            let code = match self.get_key_code_from_index(index) {
                Ok(code) => code,
                Err(_) => {
                    scanned_keys += 1;
                    continue;
                }
            };
            scanned_keys += 1;

            self.classify_key_at(code, &mut cpu_keys, &mut gpu_keys);
        }

        TempKeyScan {
            cpu_keys,
            gpu_keys,
            scanned_keys,
            total_keys,
            used_sorted_range: false,
        }
    }

    /// Read `code`'s data type and file it under the category it belongs to.
    ///
    /// Skips the `read_key_info` round trip for any name that cannot classify,
    /// and honours the per-category caps.
    fn classify_key_at(&self, code: u32, cpu_keys: &mut Vec<String>, gpu_keys: &mut Vec<String>) {
        let key = fourcc_to_string(code);
        if !is_temperature_key_candidate(&key) {
            return;
        }

        let Ok(key_info) = self.read_key_info(&key) else {
            return;
        };

        match classify_temperature_key(&key, key_info.data_type) {
            Some(TempKeyCategory::Cpu) if cpu_keys.len() < MAX_CPU_TEMP_KEYS => cpu_keys.push(key),
            Some(TempKeyCategory::Gpu) if gpu_keys.len() < MAX_GPU_TEMP_KEYS => gpu_keys.push(key),
            _ => {}
        }
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
    ///
    /// PSTR is the SMC's own estimate of total system draw in watts. It exists
    /// on most Intel Macs and is model-dependent, so callers must treat it as
    /// an approximation rather than a metered value. Implausible readings
    /// (non-finite, negative, or beyond any Mac's power envelope) are dropped
    /// so a missing or differently-typed key cannot surface as a metric.
    pub fn get_system_power(&mut self) -> Option<f64> {
        let value = self.read_value("PSTR").ok()?;
        is_plausible_power_watts(value).then_some(value)
    }

    /// Read CPU package power in watts, when the model exposes it.
    ///
    /// The SMC power key family varies across Intel Mac generations, so the
    /// candidates are tried in order of specificity: package total, package
    /// cores, then the older per-package core key. Returns `None` when none of
    /// them is present, which is a normal outcome on many models.
    pub fn get_cpu_package_power(&mut self) -> Option<f64> {
        const CPU_POWER_KEYS: [&str; 3] = ["PCPT", "PCPC", "PC0C"];

        for key in CPU_POWER_KEYS {
            if let Ok(value) = self.read_value(key)
                && is_plausible_power_watts(value)
                && value > 0.0
            {
                return Some(value);
            }
        }

        None
    }

    /// Read every fan the SMC reports, including its maximum rated speed.
    ///
    /// `FNum` holds the fan count; `F<i>Ac` and `F<i>Mx` hold the actual and
    /// maximum RPM of fan `i`. A fan whose actual speed cannot be read is
    /// skipped rather than reported as zero.
    pub fn get_fan_readings(&mut self) -> Vec<FanReading> {
        let mut fans = Vec::new();

        // Try to read fan count
        let fan_count = match self.read_value("FNum") {
            Ok(v) if v.is_finite() && v >= 0.0 => v as u32,
            _ => 2, // Default to checking 2 fans
        };

        for index in 0..fan_count.min(MAX_FANS) {
            let actual_rpm = match self.read_value(&format!("F{index}Ac")) {
                Ok(v) if is_plausible_fan_rpm(v) => v as u32,
                _ => continue,
            };

            let max_rpm = match self.read_value(&format!("F{index}Mx")) {
                Ok(v) if is_plausible_fan_rpm(v) => v as u32,
                _ => 0,
            };

            fans.push(FanReading {
                index,
                actual_rpm,
                max_rpm,
            });
        }

        fans
    }

    /// Read fan speeds as `(name, rpm)` pairs.
    pub fn get_fan_speeds(&mut self) -> Vec<(String, u32)> {
        self.get_fan_readings()
            .into_iter()
            .map(|fan| (fan.name(), fan.actual_rpm))
            .collect()
    }
}

/// Lazily opened, reusable SMC connection.
///
/// Readers are polled on the UI refresh interval, so the connection is opened
/// once and then reused instead of handshaking with IOKit on every cycle
/// (which is what [`SMCMetrics::collect`] does, acceptable there because the
/// native metrics manager caches its results). A failed open is remembered as
/// `Unavailable` so a machine without a reachable SMC does not pay for a retry
/// on every poll either.
#[derive(Default)]
pub enum SmcConnection {
    #[default]
    Unopened,
    Open(SMC),
    Unavailable,
}

impl SmcConnection {
    /// Return the open connection, opening it on first use.
    ///
    /// Returns `None` once the SMC has been found unreachable.
    pub fn get(&mut self) -> Option<&mut SMC> {
        if matches!(self, SmcConnection::Unopened) {
            *self = match SMC::new() {
                Ok(smc) => SmcConnection::Open(smc),
                Err(_) => SmcConnection::Unavailable,
            };
        }

        match self {
            SmcConnection::Open(smc) => Some(smc),
            _ => None,
        }
    }
}

/// A single fan reading from the SMC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FanReading {
    /// Zero-based fan index as used in the `F<i>Ac` key family.
    pub index: u32,
    /// Current speed in RPM.
    pub actual_rpm: u32,
    /// Maximum rated speed in RPM, or 0 when the SMC does not report one.
    pub max_rpm: u32,
}

impl FanReading {
    /// Human-readable fan name for display and metric labels.
    pub fn name(&self) -> String {
        format!("Fan {}", self.index)
    }
}

impl Drop for SMC {
    fn drop(&mut self) {
        // A zero `conn` is MACH_PORT_NULL, which only the unit tests construct.
        // Closing it is harmless but pointless, so skip it.
        if self.conn != 0 {
            // SAFETY: `conn` is an io_connect_t returned by a successful
            // `IOServiceOpen` in `SMC::new`, owned exclusively by `self`.
            // `SMC` has a `Drop` impl (so it cannot be `Copy`) and derives no
            // `Clone`, so the handle is closed exactly once.
            unsafe {
                IOServiceClose(self.conn);
            }
        }
    }
}

// SAFETY: `conn` is a mach port name in this task's IPC namespace. It carries
// no thread-local state and no thread affinity, so the handle stays valid when
// the owning `SMC` moves between threads (readers are polled from Tokio's
// blocking pool, which does not pin a task to one thread).
//
// `Sync` is deliberately NOT implemented: the read path takes `&mut self`, and
// concurrent `IOConnectCallStructMethod` calls on a single AppleSMC user client
// are not guaranteed safe. Callers that need shared access wrap this in a
// `Mutex`, which is what makes them `Sync` without any further `unsafe`.
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

    /// The name pre-filter is the classifier's own first gate, so anything the
    /// classifier can accept must survive it. If these ever disagree,
    /// discovery silently stops finding sensors it used to find.
    #[test]
    fn name_prefilter_accepts_everything_the_classifier_can() {
        let names = [
            "Tp01", "Tp0X", "Te05", "Tg0f", "Tg7L", "TC0P", "TC0D", "TG0P", "TG0D", "TB0T", "TW0P",
            "Ts0P", "PSTR", "F0Ac", "#KEY", "Tp", "T", "",
        ];
        for name in names {
            for data_type in [SMC_TYPE_FLT, SMC_TYPE_SP78, SMC_TYPE_UI8, SMC_TYPE_UI32] {
                if classify_temperature_key(name, data_type).is_some() {
                    assert!(
                        is_temperature_key_candidate(name),
                        "{name} classifies but the pre-filter would have skipped it"
                    );
                }
            }
        }
    }

    /// Keys the pre-filter rejects are exactly the keys whose second IOKit
    /// round trip discovery is allowed to skip.
    #[test]
    fn name_prefilter_rejects_non_temperature_names() {
        for name in [
            "PSTR", "F0Ac", "#KEY", "TB0T", "TW0P", "Ts0P", "VP0R", "T", "",
        ] {
            assert!(
                !is_temperature_key_candidate(name),
                "{name} should not cost a key-info round trip"
            );
        }
        for name in ["Tp01", "Te05", "Tg0f", "TC0P", "TG0D"] {
            assert!(is_temperature_key_candidate(name), "{name} must be probed");
        }
    }

    /// The span search assumes every candidate name sorts inside
    /// `TEMP_KEY_RANGE_START..TEMP_KEY_RANGE_END`. A candidate outside it would
    /// be skipped without the fallback ever noticing.
    #[test]
    fn every_candidate_name_sorts_inside_the_scanned_span() {
        for second in *b"pegCG" {
            for low in [0x00u8, b'0', b'z', 0xFF] {
                let code = u32::from_be_bytes([b'T', second, low, low]);
                assert!(
                    (TEMP_KEY_RANGE_START..TEMP_KEY_RANGE_END).contains(&code),
                    "{code:#010x} falls outside the scanned span"
                );
            }
        }
        assert!(str_to_fourcc("Sxxx") < TEMP_KEY_RANGE_START);
        assert!(str_to_fourcc("U000") >= TEMP_KEY_RANGE_END);
    }

    /// GPU and CPU sensors are capped independently. A shared cap of 64
    /// truncated the 84 `Tg*` sensors an M5 Max exposes, which made the
    /// reported GPU temperature an average over whichever ones the key table
    /// listed first.
    #[test]
    fn gpu_key_cap_clears_real_sensor_counts() {
        const {
            assert!(
                MAX_GPU_TEMP_KEYS > 84,
                "an M5 Max exposes 84 Tg* sensors; the cap must not truncate them"
            );
            assert!(
                MAX_CPU_TEMP_KEYS > 23,
                "an M5 Max exposes 23 Tp*/Te* sensors"
            );
        }
    }

    #[test]
    fn fourcc_round_trips_through_its_name() {
        for name in ["Tp01", "Tg0f", "TC0P", "#KEY", "PSTR"] {
            assert_eq!(fourcc_to_string(str_to_fourcc(name)), name);
        }
    }

    /// Discovery must stop short of the whole key table.
    ///
    /// The early exit it used to rely on required both categories to saturate a
    /// shared 64-key cap, which no real Mac does, so every discovery walked all
    /// 3,739 keys of an M5 Max table twice over. Skipped where the SMC is not
    /// reachable (a VM or a sandboxed runner), which is not a failure.
    #[test]
    #[cfg(target_os = "macos")]
    fn discovery_stops_before_the_end_of_the_key_table() {
        let Ok(smc) = SMC::new() else {
            return;
        };
        let scan = smc.scan_temperature_keys();
        if scan.total_keys == 0 {
            return;
        }

        assert!(
            scan.scanned_keys < scan.total_keys,
            "scanned {} of {} keys; discovery walked the whole table",
            scan.scanned_keys,
            scan.total_keys
        );
    }

    /// Whichever path discovery took, the keys it returns must be ones the
    /// classifier actually accepts, and it must respect the per-category caps.
    #[test]
    #[cfg(target_os = "macos")]
    fn discovered_keys_are_classifiable_and_capped() {
        let Ok(smc) = SMC::new() else {
            return;
        };
        let scan = smc.scan_temperature_keys();

        assert!(scan.cpu_keys.len() <= MAX_CPU_TEMP_KEYS);
        assert!(scan.gpu_keys.len() <= MAX_GPU_TEMP_KEYS);
        for key in scan.cpu_keys.iter().chain(scan.gpu_keys.iter()) {
            assert!(
                is_temperature_key_candidate(key),
                "{key} is not a temperature sensor name"
            );
        }
    }

    /// The span search and the exhaustive fallback must agree. They are two
    /// implementations of one question, and only the slow one is obviously
    /// correct, so the fast one is checked against it on real hardware.
    #[test]
    #[cfg(target_os = "macos")]
    fn the_span_search_finds_what_the_full_scan_finds() {
        let Ok(smc) = SMC::new() else {
            return;
        };
        let total_keys = match smc.get_key_count() {
            Ok(count) if count > 0 => count,
            _ => return,
        };

        let Some(fast) = smc.scan_sorted_temperature_range(total_keys) else {
            return;
        };
        let full = smc.scan_all_keys(total_keys);

        assert!(fast.used_sorted_range, "the fast path must report itself");
        assert!(!full.used_sorted_range, "the fallback must report itself");
        assert_eq!(fast.cpu_keys, full.cpu_keys, "CPU keys disagree");
        assert_eq!(fast.gpu_keys, full.gpu_keys, "GPU keys disagree");
        assert!(
            fast.scanned_keys < full.scanned_keys,
            "the span search read {} keys against the full scan's {}",
            fast.scanned_keys,
            full.scanned_keys
        );
    }

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

    /// Apple Silicon sensors are `flt ` floats named `Tp*`/`Te*` (CPU) and
    /// `Tg*` (GPU). This is the behavior discovery had before Intel keys were
    /// added and it must be preserved exactly.
    #[test]
    fn classifies_apple_silicon_float_sensors() {
        assert_eq!(
            classify_temperature_key("Tp01", SMC_TYPE_FLT),
            Some(TempKeyCategory::Cpu)
        );
        assert_eq!(
            classify_temperature_key("Te05", SMC_TYPE_FLT),
            Some(TempKeyCategory::Cpu)
        );
        assert_eq!(
            classify_temperature_key("Tg0f", SMC_TYPE_FLT),
            Some(TempKeyCategory::Gpu)
        );
    }

    /// Intel Macs report temperatures as `sp78` fixed point under the classic
    /// TC*/TG* names, which the old `flt `-only filter never found.
    #[test]
    fn classifies_intel_fixed_point_sensors() {
        assert_eq!(
            classify_temperature_key("TC0P", SMC_TYPE_SP78),
            Some(TempKeyCategory::Cpu)
        );
        assert_eq!(
            classify_temperature_key("TC0D", SMC_TYPE_SP78),
            Some(TempKeyCategory::Cpu)
        );
        assert_eq!(
            classify_temperature_key("TG0P", SMC_TYPE_SP78),
            Some(TempKeyCategory::Gpu)
        );
        assert_eq!(
            classify_temperature_key("TG0D", SMC_TYPE_SP78),
            Some(TempKeyCategory::Gpu)
        );
    }

    /// The two families must stay disjoint. Accepting an Intel-named key as a
    /// float, or an Apple Silicon-named key as fixed point, would let unrelated
    /// sensors pollute the temperature averages on the other architecture.
    #[test]
    fn temperature_families_do_not_cross_data_types() {
        assert_eq!(classify_temperature_key("TC0P", SMC_TYPE_FLT), None);
        assert_eq!(classify_temperature_key("TG0P", SMC_TYPE_FLT), None);
        assert_eq!(classify_temperature_key("Tp01", SMC_TYPE_SP78), None);
        assert_eq!(classify_temperature_key("Tg0f", SMC_TYPE_SP78), None);
    }

    /// Non-temperature keys and unrelated data types must never be collected,
    /// whatever their name looks like.
    #[test]
    fn rejects_non_temperature_keys() {
        // Right prefix letters, wrong leading character.
        assert_eq!(classify_temperature_key("Fp01", SMC_TYPE_FLT), None);
        // Temperature-looking name in a category we do not track.
        assert_eq!(classify_temperature_key("TA0P", SMC_TYPE_SP78), None);
        // Power and fan keys.
        assert_eq!(classify_temperature_key("PSTR", SMC_TYPE_FLT), None);
        assert_eq!(classify_temperature_key("F0Ac", SMC_TYPE_FLT), None);
        // Recognized names carried by an unrelated data type.
        assert_eq!(classify_temperature_key("Tp01", SMC_TYPE_UI32), None);
        assert_eq!(classify_temperature_key("TC0P", SMC_TYPE_UI8), None);
        // Degenerate keys.
        assert_eq!(classify_temperature_key("", SMC_TYPE_FLT), None);
        assert_eq!(classify_temperature_key("T", SMC_TYPE_FLT), None);
    }

    /// SP78 is signed 7.8 fixed point, which is how every Intel Mac reports
    /// temperatures. Getting the divisor wrong yields readings off by 256x.
    #[test]
    fn test_sp78_fixed_point_decoding() {
        let smc = SMC { conn: 0 }; // convert_value doesn't touch the connection
        let mut bytes = [0u8; 32];
        // 52.5°C in signed 7.8 fixed point = 52.5 * 256 = 13440 = 0x3480
        bytes[0..2].copy_from_slice(&0x3480_i16.to_be_bytes());
        let value = smc.convert_value(&bytes, SMC_TYPE_SP78, 2);
        assert!(
            (value - 52.5).abs() < 0.01,
            "expected ~52.5, got {value} for the Intel sp78 temperature encoding"
        );
    }

    #[test]
    fn fan_reading_names_are_indexed() {
        let fan = FanReading {
            index: 1,
            actual_rpm: 2100,
            max_rpm: 5500,
        };
        assert_eq!(fan.name(), "Fan 1");
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

    /// `get_system_power` and `get_cpu_package_power` both rely on this
    /// predicate to drop a reading from a missing or differently-typed SMC
    /// key before it can surface as a bogus metric.
    #[test]
    fn plausible_power_watts_rejects_non_finite_negative_and_out_of_range_values() {
        assert!(is_plausible_power_watts(0.0));
        assert!(is_plausible_power_watts(127.99)); // sp78 saturation point
        assert!(is_plausible_power_watts(MAX_PLAUSIBLE_POWER_WATTS));

        assert!(!is_plausible_power_watts(-0.01));
        assert!(!is_plausible_power_watts(MAX_PLAUSIBLE_POWER_WATTS + 0.01));
        assert!(!is_plausible_power_watts(f64::NAN));
        assert!(!is_plausible_power_watts(f64::INFINITY));
        assert!(!is_plausible_power_watts(f64::NEG_INFINITY));
    }

    /// Same contract as the power predicate, applied to `get_fan_readings`.
    #[test]
    fn plausible_fan_rpm_rejects_non_finite_negative_and_out_of_range_values() {
        assert!(is_plausible_fan_rpm(0.0));
        assert!(is_plausible_fan_rpm(5500.0));
        assert!(is_plausible_fan_rpm(MAX_PLAUSIBLE_FAN_RPM));

        assert!(!is_plausible_fan_rpm(-1.0));
        assert!(!is_plausible_fan_rpm(MAX_PLAUSIBLE_FAN_RPM + 0.01));
        assert!(!is_plausible_fan_rpm(f64::NAN));
        assert!(!is_plausible_fan_rpm(f64::INFINITY));
    }
}
