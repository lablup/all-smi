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
//! row, so a wrong stride is caught on the second row even when the
//! first happens to parse. The buffer is pre-filled with a poison
//! pattern rather than zeros, so a row ADL never wrote
//! ([`RowState::Untouched`]) is distinguishable from a row the driver
//! memset but did not populate ([`RowState::Blank`]), which AMD's own
//! SDK sample treats as normal (it zero-fills and then filters by
//! `iPresent`). Whenever verification fails the reader declines
//! multi-GPU attribution, leaving the DXGI and PDH baseline (the
//! pre-#353 behavior, which a single-GPU machine keeps
//! unconditionally), and the `amd.adl.adapters` doctor check dumps the
//! same fields with a per-row state tag so the transcribed layout can
//! be confirmed or refuted from real hardware.
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

/// The byte every field of [`AdapterInfo::poisoned`] is filled with.
///
/// `0xAA` is deliberately neither NUL nor printable ASCII, so a poison
/// row can never pass the string checks by accident, and a poisoned
/// `c_int` (`0xAAAAAAAA`, a large negative value) can never pass the
/// index or size checks either.
pub const ADAPTER_POISON_BYTE: u8 = 0xAA;

/// What the driver did to one row of an [`AdapterInfoArray`].
///
/// Distinguishing these four states is the entire reason the buffer is
/// poison-filled rather than zero-filled: with a zero fill, "the driver
/// never wrote this row" and "the driver memset the array and did not
/// populate this row" are the same bytes, and the second is the healthy
/// behavior AMD's own SDK sample expects (it zero-fills, calls, then
/// filters by `iPresent`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowState {
    /// The poison fill is intact: ADL never wrote this row. A trailing
    /// run of these is what a declared struct *smaller* than the
    /// driver's produces (the driver derives its row count as
    /// `iInputSize / sizeof` and stops early); row 0 untouched means
    /// the call wrote nothing at all.
    Untouched,
    /// All zero: the driver memset the row but did not populate it,
    /// the normal shape for an adapter slot filtered out by
    /// `iPresent`-style logic.
    Blank,
    /// Written and consistent with the transcribed layout
    /// ([`AdapterInfo::looks_sane`]).
    Populated,
    /// Written but inconsistent with the transcribed layout: the
    /// unambiguous signature of a wrong field offset or stride.
    Garbled,
}

impl AdapterInfo {
    /// The poison pre-fill [`AdapterInfoArray::for_count`] uses: every
    /// `int` is `0xAAAAAAAA` and every string byte is `0xAA`.
    ///
    /// `AdapterInfo` is all plain old data (nine `c_int`s and six byte
    /// arrays, no padding, pinned by the layout assertions above), so
    /// every bit pattern is a valid value and a poison-filled buffer is
    /// fully initialized memory; the pattern only has to be one no
    /// driver write could plausibly reproduce.
    pub fn poisoned() -> Self {
        let poison_int = u32::from_ne_bytes([ADAPTER_POISON_BYTE; 4]) as c_int;
        Self {
            i_size: poison_int,
            i_adapter_index: poison_int,
            str_udid: [ADAPTER_POISON_BYTE; ADL_MAX_PATH],
            i_bus_number: poison_int,
            i_device_number: poison_int,
            i_function_number: poison_int,
            i_vendor_id: poison_int,
            str_adapter_name: [ADAPTER_POISON_BYTE; ADL_MAX_PATH],
            str_display_name: [ADAPTER_POISON_BYTE; ADL_MAX_PATH],
            i_present: poison_int,
            i_exist: poison_int,
            str_driver_path: [ADAPTER_POISON_BYTE; ADL_MAX_PATH],
            str_driver_path_ext: [ADAPTER_POISON_BYTE; ADL_MAX_PATH],
            str_pnp_string: [ADAPTER_POISON_BYTE; ADL_MAX_PATH],
            i_os_display_index: poison_int,
        }
    }

    /// Whether the driver memset this row without populating it: every
    /// byte zero. One of the two inputs to [`Self::classify`], which is
    /// the single source of truth on how a row is treated.
    ///
    /// A blank row is **not** a failure. AMD's own ADL SDK sample
    /// zero-fills its `AdapterInfo` array, calls
    /// `ADL2_Adapter_AdapterInfo_Get`, and then filters the result by
    /// `iPresent` / `ADL2_Adapter_Active_Get` rather than assuming
    /// every requested row comes back populated, so a real driver
    /// plausibly leaves some rows at their memset zeros on perfectly
    /// healthy hardware. An earlier revision rejected any blank row as
    /// short-write evidence; that conflated two different facts, which
    /// the poison pre-fill now separates: a row the driver *never
    /// wrote* still carries the poison ([`RowState::Untouched`]), while
    /// a zeroed row proves the driver wrote zeros over it
    /// ([`RowState::Blank`]).
    ///
    /// Accepting blank rows is safe because the worst case is
    /// under-attribution, never mis-attribution. A populated row 0
    /// proves every field offset through 1568 is right (a mid-struct
    /// size error such as a different `ADL_MAX_PATH` would garble its
    /// strings), so the residual unknown is only the stride, and a
    /// wrong stride shows up either as a garbled later row (rejected by
    /// [`AdapterInfoArray::validated`]) or as the driver writing fewer
    /// rows, which costs attribution of the missing cards but can never
    /// report one card's telemetry against another. Interleaved blanks
    /// are the signature of healthy `iPresent` filtering and are
    /// deliberately not rejected; a stride mismatch produces garbled
    /// rows or a trailing untouched run, not interleaving.
    pub fn is_blank(&self) -> bool {
        *self == Self::default()
    }

    /// Whether a *written* row is consistent with the layout declared
    /// above. One of the inputs to [`Self::classify`]; callers should
    /// use `classify`, which first separates the untouched and blank
    /// rows this predicate is not meant to judge (an all-zero row
    /// passes every check below vacuously).
    ///
    /// Three independent signals, all of which a correct layout passes
    /// trivially and a wrong one fails almost surely:
    ///
    /// - the four string fields must be NUL-terminated printable ASCII
    ///   (a shifted or misdeclared layout puts binary data there);
    /// - `iSize`, when the driver fills it in, must equal our
    ///   `size_of` (zero is accepted: the PMLog path has seen drivers
    ///   leave size fields untouched);
    /// - `iAdapterIndex` must be a plausible small index rather than,
    ///   say, the first four bytes of a string that landed there.
    ///
    /// Only 4 of the struct's 6 string fields are checked here:
    /// `strUDID`, `strAdapterName`, `strDisplayName`, and
    /// `strPNPString`. `strDriverPath` (offset 800) and
    /// `strDriverPathExt` (offset 1056) are skipped deliberately, not
    /// by oversight. They sit strictly between `strDisplayName` (536)
    /// and `strPNPString` (1312), so `strPNPString` parsing correctly
    /// already means the stride carried it through both skipped fields
    /// intact; the marginal detection gain from checking them too is
    /// small. Each additional strictness arm is also a new way for this
    /// check to decline on real hardware nobody has tested yet, which
    /// cuts against this module's stated bias toward declining rather
    /// than guessing only when the alternative is *wrong* data, not
    /// merely *unverified* code paths. Revisit once a real host has
    /// produced a passing `amd.adl.adapters` dump to check the omitted
    /// fields against.
    ///
    /// This is a *layout* check, not a data-quality check: plausibility
    /// of the PCI bus/device/function values is judged at grouping
    /// time, per entry, so one odd row cannot disable attribution for
    /// the whole machine.
    pub fn looks_sane(&self) -> bool {
        let size_ok = self.i_size == 0 || self.i_size as usize == std::mem::size_of::<Self>();
        let index_ok = (0..256).contains(&self.i_adapter_index);
        size_ok
            && index_ok
            && adl_string(&self.str_udid).is_some()
            && adl_string(&self.str_adapter_name).is_some()
            && adl_string(&self.str_display_name).is_some()
            && adl_string(&self.str_pnp_string).is_some()
    }

    /// Classify what the driver did to this row. The single source of
    /// truth for row handling: [`AdapterInfoArray::validated`] builds
    /// its accept/reject decision on it and the `amd.adl.adapters`
    /// doctor check prints it per row.
    ///
    /// The order matters. The poison bytes are neither NUL nor
    /// printable ASCII, so an untouched row would otherwise fall
    /// through to [`RowState::Garbled`]; and an all-zero row passes
    /// [`Self::looks_sane`] vacuously, so blankness must be decided
    /// before saneness.
    pub fn classify(&self) -> RowState {
        if *self == Self::poisoned() {
            RowState::Untouched
        } else if self.is_blank() {
            RowState::Blank
        } else if self.looks_sane() {
            RowState::Populated
        } else {
            RowState::Garbled
        }
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
    /// Allocate a poison-filled buffer for `requested` adapter rows
    /// plus headroom.
    ///
    /// Poison rather than zeros so that after the call, a row ADL never
    /// wrote is distinguishable from a row the driver memset without
    /// populating; see [`RowState`]. `AdapterInfo` is all plain old
    /// data, so the poison-filled buffer is fully initialized valid
    /// memory before the driver ever sees it.
    pub fn for_count(requested: usize) -> Self {
        let allocated = requested * 2 + 4;
        Self {
            entries: vec![AdapterInfo::poisoned(); allocated],
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
    /// nothing and leaves every row with its poison fill intact, which
    /// [`Self::validated`] rejects because row 0 is not populated. The
    /// loader bounds the adapter count long before this matters.
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

    /// Per-row [`RowState`] for the requested rows, in order. This is
    /// what the `amd.adl.adapters` doctor check prints next to each
    /// dump line, so a real-hardware report says decisively whether the
    /// driver populated, memset, or never touched each row.
    pub fn row_states(&self) -> Vec<RowState> {
        self.requested_entries()
            .iter()
            .map(AdapterInfo::classify)
            .collect()
    }

    /// The populated rows, or `None` when the table as a whole cannot
    /// be trusted.
    ///
    /// Rejected when any of the following holds; otherwise the
    /// [`RowState::Populated`] rows are returned and blank or untouched
    /// rows are dropped silently (they were already harmless
    /// downstream: they group to an empty PNP string, which
    /// `plan_attribution` matches against nothing):
    ///
    /// 1. **Any row is [`RowState::Garbled`].** Written bytes that
    ///    contradict the layout are the unambiguous transcription
    ///    error, whichever field or stride produced them.
    /// 2. **Row 0 is not [`RowState::Populated`].** ADL always has at
    ///    least one adapter to describe when the call succeeds, so an
    ///    untouched or blank row 0 means the call wrote nothing usable;
    ///    this also keeps the `input_size() == 0` refusal and the
    ///    "declared size so much smaller than the driver's that zero
    ///    rows fit" case rejected.
    /// 3. **No row is [`RowState::Populated`].** Implied by rule 2 but
    ///    kept explicit: an accepted table always carries at least one
    ///    adapter.
    /// 4. **The `iAdapterIndex` values of the populated rows are not
    ///    all distinct.** A stride mismatch tends to repeat or scramble
    ///    them, so distinctness is cheap extra stride evidence.
    ///
    /// Every row participates, not just the first, because the stride
    /// errors point in opposite directions. A declared size *larger*
    /// than the driver's fits more rows than the driver's own stride,
    /// so the driver writes at a smaller stride and our row 1 reads
    /// misaligned bytes: garbled, caught by rule 1 (row 0 alone would
    /// pass, since offsets within one row are self-consistent). A
    /// declared size *smaller* than the driver's makes the driver
    /// derive a lower row count from `iInputSize` and stop early,
    /// leaving a trailing untouched run; when at least one row was
    /// still written that shape is accepted, because it is
    /// indistinguishable from healthy `iPresent` filtering and its
    /// worst case is under-attribution of the missing cards, never
    /// mis-attribution.
    ///
    /// The real gate downstream is the PNP join in `plan_attribution`,
    /// which is far stronger evidence than anything here: a garbled
    /// read cannot produce a `strPNPString` that exactly equals a WMI
    /// `PNPDeviceID`, so a successful match confirms the layout
    /// empirically on every poll.
    pub fn validated(&self) -> Option<Vec<AdapterInfo>> {
        let entries = self.requested_entries();
        let states = self.row_states();
        if states.contains(&RowState::Garbled) {
            return None;
        }
        if states.first() != Some(&RowState::Populated) {
            return None;
        }
        let populated: Vec<AdapterInfo> = entries
            .iter()
            .zip(&states)
            .filter(|(_, state)| **state == RowState::Populated)
            .map(|(entry, _)| *entry)
            .collect();
        if populated.is_empty() {
            return None;
        }
        let mut indices: Vec<c_int> = populated.iter().map(|row| row.i_adapter_index).collect();
        indices.sort_unstable();
        indices.dedup();
        if indices.len() != populated.len() {
            return None;
        }
        Some(populated)
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
    fn rows_classify_by_what_the_driver_did_to_them() {
        // Poison intact: never written. The poison bytes are neither
        // NUL nor printable, so order matters: an untouched row must
        // not fall through to Garbled.
        assert_eq!(AdapterInfo::poisoned().classify(), RowState::Untouched);

        // All zero: memset but not populated. An all-zero row passes
        // every `looks_sane` arm vacuously, so blankness must be
        // decided before saneness.
        let blank = AdapterInfo::default();
        assert!(blank.is_blank());
        assert!(blank.looks_sane());
        assert_eq!(blank.classify(), RowState::Blank);

        // Written and consistent with the layout.
        let written = sane_entry(0, r"PCI\VEN_1002&DEV_744C\6&ABCD&0&19");
        assert_eq!(written.classify(), RowState::Populated);

        // Written bytes that contradict the layout.
        let mut garbage = written;
        garbage.str_pnp_string = [0xFF; ADL_MAX_PATH];
        assert_eq!(garbage.classify(), RowState::Garbled);
    }

    #[test]
    fn a_trailing_untouched_run_is_accepted_and_dropped() {
        // A declared size *smaller* than the driver's makes ADL derive
        // a lower row count from iInputSize and stop early, leaving the
        // tail poison. With row 0 populated that shape is accepted:
        // the worst case is under-attribution of the missing cards,
        // never mis-attribution, and it is indistinguishable from a
        // driver that simply had less to say.
        let mut array = AdapterInfoArray::for_count(3);
        array.entries[0] = sane_entry(0, r"PCI\VEN_1002&DEV_744C\6&ABCD&0&19");
        array.entries[1] = sane_entry(1, r"PCI\VEN_1002&DEV_164E\4&FEDC&0&41");
        // entries[2] still carries the poison fill.
        let accepted = array.validated().expect("trailing untouched run must pass");
        assert_eq!(accepted.len(), 2);
        assert_eq!(
            array.row_states(),
            vec![
                RowState::Populated,
                RowState::Populated,
                RowState::Untouched
            ]
        );
        // The doctor still gets every raw row to report.
        assert_eq!(array.requested_entries().len(), 3);
    }

    #[test]
    fn blank_rows_are_dropped_whether_trailing_or_interleaved() {
        // Interleaved blanks are the signature of healthy iPresent
        // filtering (AMD's own SDK sample zero-fills and then filters),
        // so they must not reject the table; only the populated rows
        // come back.
        let mut array = AdapterInfoArray::for_count(3);
        array.entries[0] = sane_entry(0, r"PCI\VEN_1002&DEV_744C\6&ABCD&0&19");
        array.entries[1] = AdapterInfo::default();
        array.entries[2] = sane_entry(2, r"PCI\VEN_1002&DEV_164E\4&FEDC&0&41");
        let accepted = array.validated().expect("interleaved blanks must pass");
        assert_eq!(accepted.len(), 2);
        assert_eq!(accepted[0].i_adapter_index, 0);
        assert_eq!(accepted[1].i_adapter_index, 2);

        // Trailing blanks behave the same.
        let mut array = AdapterInfoArray::for_count(2);
        array.entries[0] = sane_entry(0, r"PCI\VEN_1002&DEV_744C\6&ABCD&0&19");
        array.entries[1] = AdapterInfo::default();
        assert_eq!(array.validated().map(|rows| rows.len()), Some(1));
    }

    #[test]
    fn a_table_without_a_populated_first_row_is_rejected() {
        // Row 0 blank: the driver memset the buffer but described no
        // adapter, which a successful call never legitimately does.
        let mut array = AdapterInfoArray::for_count(2);
        array.entries[0] = AdapterInfo::default();
        array.entries[1] = sane_entry(1, r"PCI\VEN_1002&DEV_164E\4&FEDC&0&41");
        assert!(array.validated().is_none());

        // Row 0 untouched: the call wrote nothing at all. This is also
        // the shape the input_size() == 0 refusal produces, and the
        // shape of a declared size so much smaller than the driver's
        // that zero rows fit the byte budget.
        let untouched = AdapterInfoArray::for_count(2);
        assert_eq!(
            untouched.row_states(),
            vec![RowState::Untouched, RowState::Untouched]
        );
        assert!(untouched.validated().is_none());
    }

    #[test]
    fn duplicate_adapter_indices_among_populated_rows_are_rejected() {
        // A stride mismatch tends to repeat or scramble iAdapterIndex;
        // two populated rows claiming the same index is cheap extra
        // stride evidence, and no healthy enumeration produces it.
        let mut array = AdapterInfoArray::for_count(2);
        array.entries[0] = sane_entry(0, r"PCI\VEN_1002&DEV_744C\6&ABCD&0&19");
        array.entries[1] = sane_entry(0, r"PCI\VEN_1002&DEV_164E\4&FEDC&0&41");
        assert!(array.validated().is_none());
    }

    #[test]
    fn validation_rejects_the_whole_table_when_any_row_is_garbage() {
        let mut array = AdapterInfoArray::for_count(2);
        array.entries[0] = sane_entry(0, r"PCI\VEN_1002&DEV_744C\6&ABCD&0&19");
        array.entries[1] = sane_entry(1, r"PCI\VEN_1002&DEV_164E\4&FEDC&0&41");
        assert!(array.validated().is_some());
        assert_eq!(array.validated().map(|rows| rows.len()), Some(2));

        // A wrong stride leaves the first row parseable and garbles the
        // second; that must reject everything, blanks or not, because a
        // layout that is wrong for one row is wrong for all of them.
        array.entries[1].str_udid = [0xFF; ADL_MAX_PATH];
        assert!(array.validated().is_none());
    }
}
