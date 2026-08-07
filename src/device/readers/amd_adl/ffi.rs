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
//! compile error. So this module declares exactly two output structs
//! plus scalar-only entry points, and both structs carry a defence
//! beyond the compile-time assertions.
//!
//! ## `AdapterInfo`, and why declaring it became acceptable
//!
//! Earlier revisions refused to declare [`AdapterInfo`], the struct that
//! carries the PCI bus / device / function numbers and the PNP string
//! tying an ADL adapter index to a physical card, on the grounds that
//! ADL sizes its write by its own `sizeof` and a wrong layout overflows
//! the buffer. That reasoning was half right. Unlike
//! `ADL2_New_QueryPMLogData_Get`, which takes a bare pointer,
//! `ADL2_Adapter_AdapterInfo_Get` takes the buffer size as its
//! `iInputSize` parameter, so a correct input size, plus the same
//! padded-buffer pattern proven on [`AdlPmLogDataBuffer`], reduces the
//! failure mode from a buffer overflow to reading fields at wrong
//! offsets: garbage, not corruption.
//!
//! And the struct is self-verifying against exactly that residual
//! failure. Four of its fields are 256-byte NUL-terminated ASCII
//! strings at known offsets; a correct layout reads them as legible
//! device paths, a wrong one reads them as garbage.
//! [`AdapterInfoArray::validated`] performs that check at runtime, per
//! entry, so a wrong stride is caught on the second entry even when the
//! first happens to parse, and a row ADL never wrote is a failure
//! rather than an empty adapter. Whenever verification fails the reader
//! declines multi-GPU attribution, leaving the DXGI and PDH baseline
//! (the pre-#353 behavior, which a single-GPU machine keeps
//! unconditionally), and the `amd.adl.adapters` doctor check dumps the
//! same fields so the transcribed layout can be confirmed or refuted
//! from real hardware.
//!
//! The declared layout is the **Windows** variant: `adl_structures.h`
//! appends `iExist`, the two driver-path strings, `strPNPString`, and
//! `iOSDisplayIndex` under `#if defined(_WIN32) || defined(_WIN64)`,
//! and appends a different tail on Linux, so the struct is 1572 bytes
//! here and a different size elsewhere. That is fine: the only caller
//! sits behind `cfg(target_os = "windows")`, and the declaration stays
//! unconditional so its assertions and tests run on the Linux runner,
//! the only runner this repository has.
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

/// `ADL_MAX_PATH` from `adl_defines.h`: the length of every string
/// field in [`AdapterInfo`].
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
/// `ADL2_New_QueryPMLogData_Get` takes a bare pointer with no length,
/// so ADL sizes its write by its own `sizeof` and a short buffer is a
/// memory smash rather than an error: if a future driver ships a larger
/// `ADL_PMLOG_MAX_SENSORS` it would write past a tightly-sized buffer.
/// (This is the risk the module docs describe for `AdapterInfo`, in its
/// worse no-`iInputSize` form.)
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

/// `AdapterInfo` from `adl_structures.h`, **Windows layout**.
///
/// Transcribed field by field from AMD's public header. Everything is a
/// 32-bit `int` or a `char[ADL_MAX_PATH]` array, so the natural
/// alignment is 4 and there is no padding anywhere; the compile-time
/// assertions below pin every load-bearing offset and the 1572-byte
/// total. The header's `#if defined(_WIN32) || defined(_WIN64)` block
/// contributes the last five fields; the Linux variant has a different
/// tail and a different size, and is deliberately not declared because
/// nothing outside `cfg(target_os = "windows")` ever calls the entry
/// point that fills this in.
///
/// The string fields are declared `[u8; ADL_MAX_PATH]` rather than C's
/// signed `char`: the two types have identical size and alignment, and
/// `u8` is what the byte-level string handling in [`adl_string`] wants.
///
/// This transcription cannot be checked by any compiler or CI job (see
/// the module docs), which is why nothing consumes an `AdapterInfo`
/// without it first passing [`AdapterInfoArray::validated`].
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdapterInfo {
    /// Size of the structure. Some ADL revisions fill it in, some leave
    /// the caller's zero; [`Self::looks_sane`] accepts either but
    /// rejects a positive value that disagrees with our layout.
    pub i_size: c_int,
    /// The ADL adapter index this row describes.
    pub i_adapter_index: c_int,
    /// Unique driver-assigned device identifier.
    pub str_udid: [u8; ADL_MAX_PATH],
    /// PCI bus number.
    pub i_bus_number: c_int,
    /// PCI device number.
    pub i_device_number: c_int,
    /// PCI function number.
    pub i_function_number: c_int,
    pub i_vendor_id: c_int,
    /// Marketing name, e.g. "AMD Radeon RX 7900 XTX".
    pub str_adapter_name: [u8; ADL_MAX_PATH],
    /// OS display name, e.g. `\\.\DISPLAY1`.
    pub str_display_name: [u8; ADL_MAX_PATH],
    pub i_present: c_int,
    // --- Windows-only tail below ---
    pub i_exist: c_int,
    pub str_driver_path: [u8; ADL_MAX_PATH],
    pub str_driver_path_ext: [u8; ADL_MAX_PATH],
    /// The Windows PNP device instance path, e.g.
    /// `PCI\VEN_1002&DEV_744C&SUBSYS_...\6&...&0&00000019`. The same
    /// string WMI reports as `PNPDeviceID`, which `amd_windows` stores
    /// as the GPU uuid; this field is what makes the ADL-to-GPU join
    /// exact.
    pub str_pnp_string: [u8; ADL_MAX_PATH],
    pub i_os_display_index: c_int,
}

impl Default for AdapterInfo {
    fn default() -> Self {
        Self {
            i_size: 0,
            i_adapter_index: 0,
            str_udid: [0; ADL_MAX_PATH],
            i_bus_number: 0,
            i_device_number: 0,
            i_function_number: 0,
            i_vendor_id: 0,
            str_adapter_name: [0; ADL_MAX_PATH],
            str_display_name: [0; ADL_MAX_PATH],
            i_present: 0,
            i_exist: 0,
            str_driver_path: [0; ADL_MAX_PATH],
            str_driver_path_ext: [0; ADL_MAX_PATH],
            str_pnp_string: [0; ADL_MAX_PATH],
            i_os_display_index: 0,
        }
    }
}

// AdapterInfo layout assertions. 9 ints (36 bytes) + 6 char[256] arrays
// (1536 bytes) = 1572, every field naturally 4-aligned, no padding.
// Note this is NOT the 1568 some derivations arrive at by dropping
// `iAdapterIndex`. The string offsets are individually pinned because
// the runtime self-verification and the GPU matching both read through
// them.
const _: () = assert!(
    std::mem::size_of::<AdapterInfo>() == 1572,
    "AdapterInfo (Windows layout) is 1572 bytes in AMD's headers"
);
const _: () = assert!(
    std::mem::align_of::<AdapterInfo>() == 4,
    "AdapterInfo must be 4-byte aligned, or the adapter array strides wrong"
);
const _: () = assert!(std::mem::offset_of!(AdapterInfo, str_udid) == 8);
const _: () = assert!(std::mem::offset_of!(AdapterInfo, i_bus_number) == 264);
const _: () = assert!(std::mem::offset_of!(AdapterInfo, i_device_number) == 268);
const _: () = assert!(std::mem::offset_of!(AdapterInfo, i_function_number) == 272);
const _: () = assert!(std::mem::offset_of!(AdapterInfo, i_vendor_id) == 276);
const _: () = assert!(std::mem::offset_of!(AdapterInfo, str_adapter_name) == 280);
const _: () = assert!(std::mem::offset_of!(AdapterInfo, str_display_name) == 536);
const _: () = assert!(std::mem::offset_of!(AdapterInfo, i_present) == 792);
const _: () = assert!(std::mem::offset_of!(AdapterInfo, str_pnp_string) == 1312);
const _: () = assert!(std::mem::offset_of!(AdapterInfo, i_os_display_index) == 1568);

/// The NUL-terminated printable-ASCII prefix of an ADL string field, or
/// `None` when the bytes do not look like a string at all: no
/// terminator anywhere, or a non-printable byte before it. Garbage
/// here is the signature of a misdeclared layout, which is exactly what
/// [`AdapterInfo::looks_sane`] uses it to detect. An empty string (NUL
/// at offset 0) is a valid string, not garbage.
pub fn adl_string(bytes: &[u8]) -> Option<&str> {
    let len = bytes.iter().position(|&b| b == 0)?;
    let prefix = &bytes[..len];
    if !prefix.iter().all(|&b| (0x20..=0x7E).contains(&b)) {
        return None;
    }
    // Printable ASCII is valid UTF-8 by construction.
    std::str::from_utf8(prefix).ok()
}

/// Best-effort rendering of an ADL string field for diagnostics: up to
/// the first NUL, or the whole array when no NUL exists, with invalid
/// UTF-8 replaced. Used by the `amd.adl.adapters` doctor dump, where
/// garbage bytes are precisely the evidence being collected.
pub fn adl_string_lossy(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

impl AdapterInfo {
    /// Whether ADL left this row exactly as [`AdapterInfoArray::for_count`]
    /// pre-filled it: every byte still zero.
    ///
    /// A row ADL actually wrote always carries something, a UDID, a
    /// marketing name, a vendor id, so an untouched row means ADL
    /// filled fewer rows than `iInputSize` asked for. That is what
    /// happens when the real `sizeof(AdapterInfo)` is larger than the
    /// one transcribed here: ADL derives its row count as
    /// `iInputSize / sizeof` and stops early, leaving the tail blank.
    /// Since that is precisely the transcription error the runtime
    /// verification exists to catch, [`Self::looks_sane`] treats a
    /// blank row as a failure rather than as an empty adapter. Without
    /// it a short write reads as a table of valid rows whose strings
    /// are all legitimately empty, and the `amd.adl.adapters` doctor
    /// check would report PASS over a buffer the driver never touched.
    pub fn is_blank(&self) -> bool {
        *self == Self::default()
    }

    /// Whether this entry is consistent with the layout declared above.
    ///
    /// Four independent signals, all of which a correct layout passes
    /// trivially and a wrong one fails almost surely:
    ///
    /// - the row must not be blank, i.e. ADL must have written it at
    ///   all (see [`Self::is_blank`] for why a short write is a layout
    ///   signal and not an empty adapter);
    /// - the four string fields must be NUL-terminated printable ASCII
    ///   (a shifted or misdeclared layout puts binary data there);
    /// - `iSize`, when the driver fills it in, must equal our
    ///   `size_of` (zero is accepted: the PMLog path has seen drivers
    ///   leave size fields untouched);
    /// - `iAdapterIndex` must be a plausible small index rather than,
    ///   say, the first four bytes of a string that landed there.
    ///
    /// This is a *layout* check, not a data-quality check: plausibility
    /// of the PCI bus/device/function values is judged at grouping
    /// time, per entry, so one odd row cannot disable attribution for
    /// the whole machine.
    pub fn looks_sane(&self) -> bool {
        if self.is_blank() {
            return false;
        }
        let size_ok = self.i_size == 0 || self.i_size as usize == std::mem::size_of::<Self>();
        let index_ok = (0..256).contains(&self.i_adapter_index);
        size_ok
            && index_ok
            && adl_string(&self.str_udid).is_some()
            && adl_string(&self.str_adapter_name).is_some()
            && adl_string(&self.str_display_name).is_some()
            && adl_string(&self.str_pnp_string).is_some()
    }
}

/// An `AdapterInfo` array with trailing headroom, the counterpart of
/// [`AdlPmLogDataBuffer`] for `ADL2_Adapter_AdapterInfo_Get`.
///
/// That entry point does take an `iInputSize` parameter, so unlike the
/// PMLog call the driver is *told* how large the buffer is. The
/// headroom (a full second copy of the requested rows, plus slack) is
/// kept anyway: it is cheap, and it means a driver that sizes its write
/// by its own larger `sizeof` in disregard of `iInputSize` still lands
/// inside our allocation instead of behind it.
///
/// A `requested` count whose byte size does not fit a `c_int`
/// advertises 0 rather than wrapping, so the driver is never handed a
/// plausible-looking size that exceeds the allocation; the loader
/// bounds the adapter count long before that matters.
pub struct AdapterInfoArray {
    entries: Vec<AdapterInfo>,
    requested: usize,
}

impl AdapterInfoArray {
    /// Allocate a zeroed buffer for `requested` adapter rows plus
    /// headroom.
    pub fn for_count(requested: usize) -> Self {
        let allocated = requested * 2 + 4;
        Self {
            entries: vec![AdapterInfo::default(); allocated],
            requested,
        }
    }

    /// Base pointer handed to `ADL2_Adapter_AdapterInfo_Get`.
    pub fn as_mut_ptr(&mut self) -> *mut AdapterInfo {
        self.entries.as_mut_ptr()
    }

    /// The `iInputSize` argument: the size of the `requested` rows the
    /// caller asked for, *not* the (larger) allocation. Reporting the
    /// true request keeps the driver's own bounds accounting honest;
    /// the headroom exists for drivers that ignore it.
    ///
    /// The conversion is checked rather than a truncating cast: this is
    /// the one number here that, if it ever exceeded the allocation,
    /// would be a buffer overflow inside a closed-source driver, so a
    /// silent wrap must not be able to produce it. A request too large
    /// to express in a `c_int` reports 0, which makes the driver write
    /// nothing and leaves every row blank, which
    /// [`AdapterInfo::looks_sane`] then rejects. The loader bounds the
    /// adapter count long before this matters.
    pub fn input_size(&self) -> c_int {
        self.requested
            .checked_mul(std::mem::size_of::<AdapterInfo>())
            .and_then(|bytes| c_int::try_from(bytes).ok())
            .unwrap_or(0)
    }

    /// The requested rows without any validation. For diagnostics only:
    /// the doctor dump wants to show exactly what the driver wrote even
    /// (especially) when it fails verification.
    pub fn requested_entries(&self) -> &[AdapterInfo] {
        &self.entries[..self.requested]
    }

    /// The filled-in rows, or `None` when any row fails
    /// [`AdapterInfo::looks_sane`].
    ///
    /// Every row is checked, not just the first: with a wrong struct
    /// *size* the first row still parses (offsets within it are right)
    /// and it is the second row onward that garbles, so on the
    /// multi-adapter machines this struct exists for, the later rows
    /// are the stride check. A size wrong in the other direction (ours
    /// larger than the driver's) makes ADL write fewer rows than we
    /// requested rather than garbled ones, so the later rows stay
    /// blank; [`AdapterInfo::is_blank`] is what turns that into a
    /// failure instead of a table of empty adapters.
    pub fn validated(&self) -> Option<&[AdapterInfo]> {
        let entries = self.requested_entries();
        entries
            .iter()
            .all(AdapterInfo::looks_sane)
            .then_some(entries)
    }
}

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

/// `ADL2_Adapter_AdapterInfo_Get`.
///
/// Fills `info` with one [`AdapterInfo`] per adapter index.
/// `input_size` is the byte size of the buffer the caller allocated,
/// conventionally `count * sizeof(AdapterInfo)` after asking
/// `ADL2_Adapter_NumberOfAdapters_Get` for the count. Called through
/// [`AdapterInfoArray`], which over-allocates and reports the honest
/// request size.
#[cfg(target_os = "windows")]
pub type Adl2AdapterAdapterInfoGet = unsafe extern "C" fn(
    context: AdlContextHandle,
    info: *mut AdapterInfo,
    input_size: c_int,
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

    /// Write a NUL-terminated ASCII string into an ADL char array.
    fn fill(field: &mut [u8; ADL_MAX_PATH], text: &str) {
        field.fill(0);
        field[..text.len()].copy_from_slice(text.as_bytes());
    }

    /// An entry that looks like what a real driver writes.
    fn sane_entry(index: i32, pnp: &str) -> AdapterInfo {
        let mut entry = AdapterInfo {
            i_adapter_index: index,
            i_bus_number: 3,
            i_device_number: 0,
            i_function_number: 0,
            i_vendor_id: 1002,
            i_present: 1,
            i_exist: 1,
            ..AdapterInfo::default()
        };
        fill(
            &mut entry.str_udid,
            "PCI_VEN_1002&DEV_744C&REV_C8_6&12A2C3D4&0&19A",
        );
        fill(&mut entry.str_adapter_name, "AMD Radeon RX 7900 XTX");
        fill(&mut entry.str_display_name, r"\\.\DISPLAY1");
        fill(&mut entry.str_pnp_string, pnp);
        entry
    }

    #[test]
    fn the_four_string_fields_sit_at_their_transcribed_offsets() {
        // The runtime self-verification and the GPU matching both read
        // through these offsets, so pin them against an instance and
        // not just via the const assertions: this is the test the issue
        // asks for, phrased so a failure names the field.
        let info = AdapterInfo::default();
        let base = &info as *const AdapterInfo as usize;
        for (name, offset, expected) in [
            ("strUDID", info.str_udid.as_ptr() as usize - base, 8),
            (
                "strAdapterName",
                info.str_adapter_name.as_ptr() as usize - base,
                280,
            ),
            (
                "strDisplayName",
                info.str_display_name.as_ptr() as usize - base,
                536,
            ),
            (
                "strPNPString",
                info.str_pnp_string.as_ptr() as usize - base,
                1312,
            ),
        ] {
            assert_eq!(offset, expected, "{name} is misplaced");
        }
    }

    #[test]
    fn adl_strings_accept_device_paths_and_reject_binary_garbage() {
        let mut field = [0u8; ADL_MAX_PATH];
        assert_eq!(adl_string(&field), Some(""));

        fill(&mut field, r"PCI\VEN_1002&DEV_744C");
        assert_eq!(adl_string(&field), Some(r"PCI\VEN_1002&DEV_744C"));

        // No terminator anywhere: what a shifted layout reads out of
        // the middle of a neighbouring field.
        let unterminated = [b'A'; ADL_MAX_PATH];
        assert_eq!(adl_string(&unterminated), None);

        // Binary bytes before the terminator: an int reinterpreted as
        // the start of a string.
        let mut binary = [0u8; ADL_MAX_PATH];
        binary[0] = 0x01;
        binary[1] = 0x9F;
        assert_eq!(adl_string(&binary), None);

        // The lossy variant renders both anyway; it exists to collect
        // evidence, not to gatekeep.
        assert_eq!(adl_string_lossy(&field), r"PCI\VEN_1002&DEV_744C");
        assert_eq!(adl_string_lossy(&unterminated).len(), ADL_MAX_PATH);
    }

    #[test]
    fn a_realistic_entry_passes_and_layout_garbage_fails() {
        assert!(sane_entry(0, r"PCI\VEN_1002&DEV_744C\6&ABCD&0&19").looks_sane());

        // iSize agreeing with our size_of, or left at zero, are both
        // fine; a positive disagreement is the ABI-drift signal.
        let mut entry = sane_entry(0, r"PCI\VEN_1002&DEV_744C\6&ABCD&0&19");
        entry.i_size = std::mem::size_of::<AdapterInfo>() as c_int;
        assert!(entry.looks_sane());
        entry.i_size = 1568; // the off-by-one-int derivation
        assert!(!entry.looks_sane());

        // An adapter index that is really string bytes.
        let mut entry = sane_entry(0, r"PCI\VEN_1002&DEV_744C\6&ABCD&0&19");
        entry.i_adapter_index = i32::from_le_bytes(*b"PCI\\");
        assert!(!entry.looks_sane());

        // A string field holding unterminated bytes.
        let mut entry = sane_entry(0, r"PCI\VEN_1002&DEV_744C\6&ABCD&0&19");
        entry.str_pnp_string = [b'x'; ADL_MAX_PATH];
        assert!(!entry.looks_sane());
    }

    #[test]
    fn the_adapter_array_reports_the_requested_size_but_allocates_more() {
        let mut array = AdapterInfoArray::for_count(3);
        assert_eq!(
            array.input_size() as usize,
            3 * std::mem::size_of::<AdapterInfo>()
        );
        assert_eq!(array.requested_entries().len(), 3);
        // The headroom: the allocation must hold at least a full second
        // copy of the requested rows.
        assert!(array.entries.len() >= 2 * array.requested);
        // And the pointer handed to ADL must be the first entry.
        let base = array.as_mut_ptr() as usize;
        assert_eq!(base, array.entries.as_ptr() as usize);
    }

    #[test]
    fn an_inexpressible_request_size_advertises_zero_rather_than_wrapping() {
        // `input_size` is the number the driver writes against, so it
        // must never exceed the allocation. A count large enough to
        // overflow `c_int` reports 0 (the driver writes nothing and the
        // untouched rows fail verification) instead of wrapping into a
        // plausible-looking positive size.
        let array = AdapterInfoArray {
            entries: Vec::new(),
            requested: usize::MAX / 8,
        };
        assert_eq!(array.input_size(), 0);
    }

    #[test]
    fn a_row_the_driver_never_wrote_is_not_a_valid_empty_adapter() {
        // Every arm of `looks_sane` is individually satisfied by the
        // zero row `for_count` pre-fills the buffer with: `iSize` is 0
        // (accepted), `iAdapterIndex` is 0 (in range), and each string
        // field is a NUL at offset 0, which `adl_string` documents as
        // the empty string rather than garbage. The blank check is the
        // only thing standing between that and a table of "valid"
        // adapters ADL never touched.
        let blank = AdapterInfo::default();
        assert!(blank.is_blank());
        assert!(!blank.looks_sane());
        assert_eq!(adl_string(&blank.str_udid), Some(""));

        // And one written byte anywhere is enough to stop being blank.
        let written = sane_entry(0, r"PCI\VEN_1002&DEV_744C\6&ABCD&0&19");
        assert!(!written.is_blank());
        assert!(written.looks_sane());
    }

    #[test]
    fn a_short_write_leaving_trailing_rows_blank_fails_verification() {
        // The failure mode of transcribing a struct *larger* than the
        // driver's: ADL derives its row count as `iInputSize / sizeof`,
        // stops early, and leaves our tail untouched. Nothing garbles,
        // so the per-row string checks alone would pass the table; the
        // short write itself is the layout evidence.
        let mut array = AdapterInfoArray::for_count(3);
        array.entries[0] = sane_entry(0, r"PCI\VEN_1002&DEV_744C\6&ABCD&0&19");
        array.entries[1] = sane_entry(1, r"PCI\VEN_1002&DEV_164E\4&FEDC&0&41");
        // entries[2] is still the zero we handed the driver.
        assert!(array.validated().is_none());

        // The doctor still gets the raw rows to report, blank ones
        // included, since "ADL wrote two of the three rows we asked
        // for" is exactly the evidence that names the error.
        assert_eq!(array.requested_entries().len(), 3);
    }

    #[test]
    fn validation_rejects_the_whole_table_when_any_row_is_garbage() {
        let mut array = AdapterInfoArray::for_count(2);
        array.entries[0] = sane_entry(0, r"PCI\VEN_1002&DEV_744C\6&ABCD&0&19");
        array.entries[1] = sane_entry(1, r"PCI\VEN_1002&DEV_164E\4&FEDC&0&41");
        assert!(array.validated().is_some());
        assert_eq!(array.validated().unwrap().len(), 2);

        // A wrong stride leaves the first row parseable and garbles the
        // second; that must reject everything, because a layout that is
        // wrong for one row is wrong for all of them.
        array.entries[1].str_udid = [0xFF; ADL_MAX_PATH];
        assert!(array.validated().is_none());
    }
}
