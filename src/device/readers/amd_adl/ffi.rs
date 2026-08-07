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

//! Hand-declared ADL types and entry points.
//!
//! Only the handful of symbols all-smi actually calls are declared, and
//! no AMD header is vendored, which keeps the project Apache-2.0 clean.
//! The signatures and layouts are transcribed from AMD's public
//! `adl_structures.h` and `adl_defines.h`.
//!
//! ## Why the surface is this small
//!
//! Every additional `#[repr(C)]` struct declared against a library we
//! cannot compile against, cannot run, and have no CI coverage for is
//! unverifiable risk: a layout mistake is memory corruption, not a
//! compile error. So this module declares exactly one output struct plus
//! scalar-only entry points.
//!
//! Notably absent is `AdapterInfo`, the 1568-byte struct that
//! `ADL2_Adapter_AdapterInfo_Get` fills in. It carries the PCI bus /
//! device / function numbers and the PNP string that would let an ADL
//! adapter be matched to a specific card. Declaring it would be the
//! single largest ABI liability in this file, and ADL sizes its write by
//! its own `sizeof`, so getting it wrong overflows our buffer. The
//! reader avoids needing it by only augmenting when the machine has
//! exactly one AMD GPU; see the module docs in the parent.
//!
//! ## Calling convention
//!
//! AMD declares the `ADL2_*` entry points as cdecl and only
//! `ADL_MAIN_MALLOC_CALLBACK` as `__stdcall`, so the entry points use
//! `extern "C"` and the callback uses `extern "system"`. The two
//! coincide on x86-64, which is the only Windows target all-smi ships
//! (`release.yml` builds `x86_64-pc-windows-msvc` alone), but they
//! differ on i686 and getting them backwards there would corrupt the
//! stack.

use std::ffi::c_int;
#[cfg(target_os = "windows")]
use std::ffi::c_void;

/// `ADL_MAX_PATH` from `adl_defines.h`. Not used by any struct declared
/// here; kept as documentation of the value the omitted `AdapterInfo`
/// layout depends on.
#[allow(dead_code)]
pub const ADL_MAX_PATH: usize = 256;

/// `ADL_PMLOG_MAX_SENSORS` from `adl_structures.h`.
pub const ADL_PMLOG_MAX_SENSORS: usize = 256;

/// `ADL_OK`, the success status shared by every ADL entry point.
#[cfg(target_os = "windows")]
pub const ADL_OK: c_int = 0;

/// Opaque `ADL_CONTEXT_HANDLE`.
#[cfg(target_os = "windows")]
pub type AdlContextHandle = *mut c_void;

/// `ADL_MAIN_MALLOC_CALLBACK`.
#[cfg(target_os = "windows")]
pub type AdlMainMallocCallback = unsafe extern "system" fn(c_int) -> *mut c_void;

/// One entry of `ADLPMLogDataOutput::sensors`.
///
/// `supported` is a boolean flag; `value` is the reading, whose unit
/// depends on the sensor index.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct AdlSingleSensorData {
    pub supported: c_int,
    pub value: c_int,
}

/// `ADLPMLogDataOutput`: a fixed-size, sensor-index-addressed table.
///
/// The array is indexed by `ADLSensorType`, so entry `n` is the sensor
/// whose enum value is `n`. Entries the card does not publish have
/// `supported == 0`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct AdlPmLogDataOutput {
    /// `ulSize` in the header, declared `int` despite the name.
    pub ul_size: c_int,
    pub sensors: [AdlSingleSensorData; ADL_PMLOG_MAX_SENSORS],
}

impl Default for AdlPmLogDataOutput {
    fn default() -> Self {
        Self {
            ul_size: 0,
            sensors: [AdlSingleSensorData::default(); ADL_PMLOG_MAX_SENSORS],
        }
    }
}

// Layout assertions. These are the only automated check that exists for
// this ABI: nothing in CI compiles for Windows, and no test can call the
// real library. A mismatch here is memory corruption at runtime, so the
// assertions are mandatory rather than decorative. Same rationale as the
// `RmQuickStats` assertion in `device/windows_temp/amd_ryzen.rs`.
const _: () = assert!(
    std::mem::size_of::<AdlSingleSensorData>() == 8,
    "ADLSingleSensorData must be two 32-bit ints"
);
const _: () = assert!(
    std::mem::align_of::<AdlSingleSensorData>() == 4,
    "ADLSingleSensorData must be 4-byte aligned, or the sensor array strides wrong"
);
const _: () = assert!(
    std::mem::size_of::<AdlPmLogDataOutput>() == 4 + 8 * ADL_PMLOG_MAX_SENSORS,
    "ADLPMLogDataOutput must be a leading int followed by a packed 256-entry sensor array"
);
const _: () = assert!(
    std::mem::size_of::<AdlPmLogDataOutput>() == 2052,
    "ADLPMLogDataOutput is 2052 bytes in AMD's headers"
);

/// A PMLog output buffer with trailing headroom.
///
/// The module docs above refuse to declare `AdapterInfo` because ADL
/// sizes its write by its own `sizeof` and a short buffer is a memory
/// smash rather than an error. That reasoning applies to
/// `ADLPMLogDataOutput` too: `ADL2_New_QueryPMLogData_Get` takes a bare
/// pointer with no length, so if a future driver ships a larger
/// `ADL_PMLOG_MAX_SENSORS` it would write past a tightly-sized buffer.
///
/// Raising `ADL_PMLOG_MAX_SENSORS` would break every existing consumer,
/// so it is unlikely, but "unlikely" is not a reason to hand a driver an
/// exact-fit buffer in a long-running daemon. The padding gives a 4x
/// table room to land, and [`Self::validated`] rejects a response whose
/// reported size does not match what we asked for.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct AdlPmLogDataBuffer {
    pub output: AdlPmLogDataOutput,
    /// Never read. Present so an oversized write lands in our allocation
    /// instead of in whatever follows it.
    _headroom: [AdlSingleSensorData; ADL_PMLOG_MAX_SENSORS * 3],
}

impl Default for AdlPmLogDataBuffer {
    fn default() -> Self {
        Self {
            output: AdlPmLogDataOutput::default(),
            _headroom: [AdlSingleSensorData::default(); ADL_PMLOG_MAX_SENSORS * 3],
        }
    }
}

impl AdlPmLogDataBuffer {
    /// The filled-in table, or `None` when the driver reported a size
    /// that does not correspond to the layout declared here.
    ///
    /// `ulSize` is documented as the size of the returned structure.
    /// Drivers in the field have been observed leaving it at zero, so a
    /// zero is accepted rather than treated as failure; only a positive
    /// value that disagrees with our layout is rejected, since that is
    /// the signal that this file's ABI no longer matches the driver's.
    pub fn validated(&self) -> Option<&AdlPmLogDataOutput> {
        let reported = self.output.ul_size;
        if reported == 0 || reported as usize == std::mem::size_of::<AdlPmLogDataOutput>() {
            return Some(&self.output);
        }
        None
    }
}

const _: () = assert!(
    std::mem::size_of::<AdlPmLogDataBuffer>() > std::mem::size_of::<AdlPmLogDataOutput>(),
    "the headroom must actually extend the allocation"
);

// The entry points are only callable where the library exists, so they
// are declared for Windows alone. The struct layouts above stay
// unconditional: their compile-time assertions are the one automated
// check this ABI can have, and they must run on the Linux test runner
// too, since that is the only runner this repository has.

/// `ADL2_Main_Control_Create`.
///
/// The second parameter is `iEnumConnectedAdapters`; passing 1 restricts
/// enumeration to adapters that are actually present.
#[cfg(target_os = "windows")]
pub type Adl2MainControlCreate = unsafe extern "C" fn(
    callback: AdlMainMallocCallback,
    enum_connected_adapters: c_int,
    context: *mut AdlContextHandle,
) -> c_int;

/// `ADL2_Main_Control_Destroy`.
///
/// Declared for completeness and deliberately never called: the context
/// is created once and lives for the process, so there is no teardown
/// point. Destroying it on a static's drop would run at an unspecified
/// time relative to the driver unloading.
#[cfg(target_os = "windows")]
#[allow(dead_code)]
pub type Adl2MainControlDestroy = unsafe extern "C" fn(context: AdlContextHandle) -> c_int;

/// `ADL2_Adapter_NumberOfAdapters_Get`.
#[cfg(target_os = "windows")]
pub type Adl2AdapterNumberOfAdaptersGet =
    unsafe extern "C" fn(context: AdlContextHandle, num_adapters: *mut c_int) -> c_int;

/// `ADL2_Overdrive_Caps`.
///
/// `supported` and `enabled` are booleans; `version` is the Overdrive
/// generation, which is 8 on the PMLog-capable parts this reader
/// targets.
#[cfg(target_os = "windows")]
pub type Adl2OverdriveCaps = unsafe extern "C" fn(
    context: AdlContextHandle,
    adapter_index: c_int,
    supported: *mut c_int,
    enabled: *mut c_int,
    version: *mut c_int,
) -> c_int;

/// `ADL2_New_QueryPMLogData_Get`.
#[cfg(target_os = "windows")]
pub type Adl2NewQueryPmLogDataGet = unsafe extern "C" fn(
    context: AdlContextHandle,
    adapter_index: c_int,
    output: *mut AdlPmLogDataOutput,
) -> c_int;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sensor_table_is_addressable_by_index() {
        // The whole extraction strategy rests on entry `n` being the
        // sensor whose enum value is `n`, so assert the array actually
        // strides by one 8-byte record.
        let mut output = AdlPmLogDataOutput::default();
        output.sensors[27].supported = 1;
        output.sensors[27].value = 71;

        let base = &output.sensors[0] as *const AdlSingleSensorData as usize;
        let entry = &output.sensors[27] as *const AdlSingleSensorData as usize;
        assert_eq!(entry - base, 27 * 8);

        // And the leading int must not overlap the first sensor.
        let struct_base = &output as *const AdlPmLogDataOutput as usize;
        assert_eq!(base - struct_base, 4);
    }

    #[test]
    fn the_padded_buffer_gives_an_oversized_write_somewhere_to_land() {
        // The point of the headroom: a driver shipping a larger sensor
        // table writes into our allocation instead of past it. The
        // buffer must hold a table four times the declared length, plus
        // the leading size word.
        assert!(
            std::mem::size_of::<AdlPmLogDataBuffer>()
                >= std::mem::size_of::<c_int>()
                    + 4 * std::mem::size_of::<AdlSingleSensorData>() * ADL_PMLOG_MAX_SENSORS
        );
        // And the output must sit at the very start, since that is the
        // pointer handed to ADL.
        let buffer = AdlPmLogDataBuffer::default();
        let buffer_base = &buffer as *const AdlPmLogDataBuffer as usize;
        let output_base = &buffer.output as *const AdlPmLogDataOutput as usize;
        assert_eq!(buffer_base, output_base);
    }

    #[test]
    fn validation_accepts_a_matching_or_absent_size_and_rejects_a_conflicting_one() {
        let mut buffer = AdlPmLogDataBuffer::default();

        // Drivers have been seen leaving ulSize at zero; that is not a
        // failure signal.
        buffer.output.ul_size = 0;
        assert!(buffer.validated().is_some());

        buffer.output.ul_size = std::mem::size_of::<AdlPmLogDataOutput>() as i32;
        assert!(buffer.validated().is_some());

        // A positive size that disagrees with our layout means this
        // file's ABI no longer matches the driver's, which is exactly
        // when we must not interpret the contents.
        buffer.output.ul_size = 4096;
        assert!(buffer.validated().is_none());
    }

    #[test]
    fn default_reports_every_sensor_unsupported() {
        let output = AdlPmLogDataOutput::default();
        assert!(output.sensors.iter().all(|s| s.supported == 0));
        assert_eq!(output.sensors.len(), ADL_PMLOG_MAX_SENSORS);
    }
}
