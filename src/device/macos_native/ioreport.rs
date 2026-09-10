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

//! IOReport API bindings for macOS
//!
//! This module provides FFI bindings to Apple's private IOReport framework,
//! which is used to collect power and performance metrics on Apple Silicon.
//!
//! ## Channel Groups
//! - `Energy Model`: Power consumption (CPU, GPU, ANE, DRAM)
//! - `CPU Stats`: CPU core performance states and residency
//! - `GPU Stats`: GPU performance states and residency
//!
//! ## Energy Model: exact channel matching
//!
//! The group is a hierarchy, not a list of rails. On an M5 Max (macOS 27) it
//! holds 364 channels: the `CPU Energy` roll-up, the clusters that sum to it
//! (`MCPU0`, `MCPU1`, `PCPU`), per-core channels, `*_SRAM`, 300 `*DTL*`
//! telemetry channels, `GPU0`, `GPU Energy`, and a dozen other blocks (see
//! `tests/fixtures/ioreport/m5_max_energy_model.tsv`). Substring matching on
//! `CPU` summed the roll-up together with everything under it. One loaded
//! batch:
//!
//! | family | channels | watts |
//! |---|---|---|
//! | `CPU Energy` (roll-up) | 1 | 36.17 |
//! | `MCPU0`, `MCPU1`, `PCPU` (clusters) | 3 | 36.17 |
//! | `MCPU*DTL*`, `PCPUDTL*` (per-core telemetry) | 300 | 26.01 |
//! | `MCPUx_y` (per-core) | 12 | 5.20 |
//! | `*_SRAM` | 18 | 3.13 |
//! | reported as CPU power | 334 | 106.68 |
//!
//! That is 2.95x the roll-up. `GPU0` (mJ) and `GPU Energy` (nJ) carry the
//! same energy, so matching both counted the GPU twice. Channels are matched
//! by exact name in [`classify_energy_channel`]: `GPU Energy`; names ending in
//! `CPU Energy` (`DIE_<n>_CPU Energy` on multi-die packages); `GPU<n>` only
//! when a sample has no `GPU Energy`; and the top-level `ANE`/`DRAM` shapes
//! (`ANE`, `ANE0`, `ANE0_1`). Nothing else is summed.
//!
//! ## Energy Model: power from publication timestamps
//!
//! The mJ counters do not advance continuously. On an M5 Max (macOS 27) the
//! driver publishes all of them together in batches 2.03 to 2.23 s apart
//! (measured spans 2027.5 to 2228.1 ms); between publications both the value
//! and its timestamp are frozen. Dividing by the poll window misreads at every
//! interval. At 1 s it alternates between 0 W and a whole batch over 1 s
//! (about 2x). Holding the zero ticks is not enough either, because the window
//! stays quantized to the poll grid: a 2.1 s batch spans 2 s or 3 s of polls,
//! so a 1 s poll reads 1.05x or 0.70x, and at the 3 s API default every window
//! holds one or two batches and the reading beats between 0.7x and 1.4x.
//!
//! Two more measured behaviors constrain the design:
//! - `GPU Energy` (nJ) is stamped at sample time (0.1 to 0.4 ms old) and moves
//!   on every sample under load. At idle it reads exactly 0 nJ per window, so
//!   a zero there is 0 W, not a missing publication.
//! - Publications split. A batch is sometimes followed 9 to 26 ms later by a
//!   small second publication with its own timestamp (`CPU Energy`: 4969 mJ
//!   over 2043.0 ms, then 65 mJ over 14.8 ms), and one channel can land a
//!   sample after the others (`DRAM0` once published 25 ms after
//!   `CPU Energy`, with its own 2158.5 ms span). A single "the roll-up moved"
//!   clock cannot time both, so timing is per channel.
//!
//! So power comes from the driver's own publication times. Each channel's
//! `RawElements` data holds xnu's `IOReportElement`
//! (`IOKernelReportStructs.h`): provider id at byte 0, channel id at 8,
//! channel type at 16, the `mach_absolute_time` of the driver's last update at
//! 24, and the value at 32. [`EnergyTracker`] keeps a `(value, timestamp)`
//! baseline per channel and, when a channel publishes, reports
//! `(value - baseline) / (timestamp - baseline timestamp)`. It holds the
//! previous reading while a channel has not published, folds publications
//! under 50 ms into the next span, and drops a reading after 10 s without
//! one. A channel whose elements carry no usable timestamp is timed by when
//! each sample was taken, which is the old poll-window behavior.
//!
//! The timestamps have to be read from raw samples. In the output of
//! `IOReportCreateSamplesDelta` an element's timestamp is the older sample's,
//! so a delta cannot say when the energy it carries was published. Residency
//! and frequency still come from the delta: those counters advance on every
//! sample.
//!
//! ## References
//! - macmon project by vladkens
//! - asitop project by tlkh
//! - OSXPrivateSDK IOReport.h

use super::energy::{EnergyObservation, EnergyReadings, EnergyTracker, classify_energy_channel};
use core_foundation::base::{
    CFEqual, CFGetTypeID, CFRelease, CFRetain, CFType, CFTypeRef, TCFType,
};
use core_foundation::data::{CFData, CFDataGetTypeID, CFDataRef};
use core_foundation::dictionary::{
    CFDictionary, CFDictionaryGetTypeID, CFDictionaryGetValue, CFDictionaryRef,
    CFMutableDictionaryRef,
};
use core_foundation::string::{CFString, CFStringRef};
use std::ffi::c_void;
use std::marker::{PhantomData, PhantomPinned};
use std::ptr;
use std::sync::OnceLock;
use std::time::Instant;

/// Static CFStringRef constants for IOReport channel groups and dictionary keys.
/// These are created once, retained with CFRetain, and kept for the lifetime
/// of the application to avoid use-after-free issues with temporary CFString objects.
///
/// SAFETY: CFStringRef pointers are immutable once created and CFRetain ensures
/// they live for the application's lifetime. They can be safely shared across threads.
struct CFStringRefs {
    energy_model: CFStringRef,
    cpu_stats: CFStringRef,
    cpu_perf_states: CFStringRef,
    gpu_stats: CFStringRef,
    gpu_perf_states: CFStringRef,
    raw_elements: CFStringRef,
}

// SAFETY: CFStringRef is an immutable reference type. Once created and retained,
// CFStrings are thread-safe for read-only access. We never mutate these pointers.
unsafe impl Send for CFStringRefs {}
unsafe impl Sync for CFStringRefs {}

impl CFStringRefs {
    fn new() -> Self {
        // SAFETY: We create CFStrings, get their raw pointers, then call CFRetain
        // to ensure they live for the program's lifetime. The CFString objects
        // go out of scope but the underlying CF objects are retained.
        unsafe {
            let energy_model = {
                let s = CFString::new(ENERGY_MODEL);
                let ptr = s.as_concrete_TypeRef();
                CFRetain(ptr as *const c_void);
                ptr
            };
            let cpu_stats = {
                let s = CFString::new(CPU_STATS);
                let ptr = s.as_concrete_TypeRef();
                CFRetain(ptr as *const c_void);
                ptr
            };
            let cpu_perf_states = {
                let s = CFString::new(CPU_PERF_STATES);
                let ptr = s.as_concrete_TypeRef();
                CFRetain(ptr as *const c_void);
                ptr
            };
            let gpu_stats = {
                let s = CFString::new(GPU_STATS);
                let ptr = s.as_concrete_TypeRef();
                CFRetain(ptr as *const c_void);
                ptr
            };
            let gpu_perf_states = {
                let s = CFString::new(GPU_PERF_STATES);
                let ptr = s.as_concrete_TypeRef();
                CFRetain(ptr as *const c_void);
                ptr
            };
            let raw_elements = {
                let s = CFString::new(RAW_ELEMENTS);
                let ptr = s.as_concrete_TypeRef();
                CFRetain(ptr as *const c_void);
                ptr
            };

            Self {
                energy_model,
                cpu_stats,
                cpu_perf_states,
                gpu_stats,
                gpu_perf_states,
                raw_elements,
            }
        }
    }
}

/// Global static CFString constants to prevent use-after-free
static CFSTRING_REFS: OnceLock<CFStringRefs> = OnceLock::new();

/// Get or initialize the static CFString constants
fn get_cfstring_refs() -> &'static CFStringRefs {
    CFSTRING_REFS.get_or_init(CFStringRefs::new)
}

/// One `IOReportCopyChannelsInGroup` query, in a form that can cross a thread
/// boundary.
///
/// SAFETY: both fields are `CFStringRef`s owned by [`CFStringRefs`], which
/// retains them for the life of the process and never mutates them. The same
/// reasoning that makes `CFStringRefs` `Sync` makes this `Send`.
#[derive(Clone, Copy)]
struct ChannelGroupQuery {
    group: CFStringRef,
    subgroup: CFStringRef,
}

unsafe impl Send for ChannelGroupQuery {}

impl ChannelGroupQuery {
    /// Run the query. Returns null when the group does not exist on this host.
    fn copy_channels(self) -> CFDictionaryRef {
        unsafe { IOReportCopyChannelsInGroup(self.group, self.subgroup, 0, 0, 0) }
    }
}

/// Process-wide cache of the merged channel description used to open every
/// IOReport subscription.
///
/// SAFETY: holds an owned (+1 retained) reference to an immutable
/// CFDictionary that is never released and never mutated after publication.
/// Immutable CF objects are safe to read concurrently, and the only consumer,
/// [`IOReport::new`], reads it solely as the source of a
/// `CFDictionaryCreateMutableCopy`.
struct MergedChannels(CFDictionaryRef);

unsafe impl Send for MergedChannels {}
unsafe impl Sync for MergedChannels {}

/// Cached result of enumerating and merging the three channel groups.
///
/// The set of channels a machine exposes is fixed by its hardware, so this is
/// computed once per process. That matters because
/// `IOReportCopyChannelsInGroup` is by far the most expensive part of opening
/// a subscription, and a library consumer can construct and drop several
/// clients over a process's life (issue #374).
static MERGED_CHANNELS: OnceLock<MergedChannels> = OnceLock::new();

/// Enumerate the three channel groups and merge them into one dictionary.
///
/// The three queries are independent reads that are only combined afterwards,
/// so they run concurrently. Only the Energy Model group is required: the CPU
/// and GPU stats groups are tolerated as absent, which is the same asymmetry
/// the sequential version had.
///
/// Returns an owned (+1 retained) dictionary.
fn build_merged_channels() -> Result<CFDictionaryRef, &'static str> {
    let refs = get_cfstring_refs();

    // Spawn all three before joining any, so they overlap.
    let energy = spawn_channel_query(refs.energy_model, ptr::null());
    let cpu = spawn_channel_query(refs.cpu_stats, refs.cpu_perf_states);
    let gpu = spawn_channel_query(refs.gpu_stats, refs.gpu_perf_states);

    // `Err` means that worker panicked, which is distinct from a group that
    // simply does not exist on this host: the latter returns a null dictionary.
    let energy_channels = energy.join();
    let cpu_channels = cpu.join().unwrap_or(ptr::null());
    let gpu_channels = gpu.join().unwrap_or(ptr::null());

    let energy_channels = match energy_channels {
        Ok(dict) if !dict.is_null() => dict,
        outcome => {
            // The optional groups may still have succeeded; do not leak them.
            for dict in [cpu_channels, gpu_channels] {
                if !dict.is_null() {
                    unsafe { CFRelease(dict as *const c_void) };
                }
            }
            return Err(match outcome {
                Err(_) => "IOReport Energy Model channel enumeration panicked",
                Ok(_) => "Failed to get Energy Model channels",
            });
        }
    };

    unsafe {
        // Merge all channels into one dictionary
        if !cpu_channels.is_null() {
            IOReportMergeChannels(energy_channels, cpu_channels, ptr::null());
            CFRelease(cpu_channels as *const c_void);
        }
        if !gpu_channels.is_null() {
            IOReportMergeChannels(energy_channels, gpu_channels, ptr::null());
            CFRelease(gpu_channels as *const c_void);
        }
    }

    Ok(energy_channels)
}

/// A `CFDictionaryRef` that may cross a thread boundary.
///
/// SAFETY: used only to hand an owned dictionary back from the worker thread
/// that created it to the thread that joins it. Ownership moves with the
/// value, so no two threads ever hold it at once.
struct OwnedDict(CFDictionaryRef);

unsafe impl Send for OwnedDict {}

/// One channel-group query, either already running on its own thread or
/// waiting to run on the calling thread.
enum ChannelQuery {
    Spawned(std::thread::JoinHandle<OwnedDict>),
    /// No thread was available, so the query runs inline when joined. That
    /// makes the enumeration sequential again, which is exactly the behavior
    /// this replaced, rather than a failure.
    Inline(ChannelGroupQuery),
}

impl ChannelQuery {
    /// Wait for the query. `Err` means the worker panicked; `Ok(null)` means
    /// the group does not exist on this host.
    fn join(self) -> Result<CFDictionaryRef, ()> {
        match self {
            Self::Spawned(handle) => handle.join().map(|OwnedDict(dict)| dict).map_err(|_| ()),
            Self::Inline(query) => Ok(query.copy_channels()),
        }
    }
}

/// Start one `IOReportCopyChannelsInGroup` query on its own thread.
///
/// Thread creation can fail under resource pressure, and `thread::spawn`
/// panics when it does. This is reached from `AllSmi::with_config`, a library
/// entry point that reports failure through `Result`, so a refused thread
/// degrades to running the query inline instead of unwinding through it.
fn spawn_channel_query(group: CFStringRef, subgroup: CFStringRef) -> ChannelQuery {
    let query = ChannelGroupQuery { group, subgroup };
    match std::thread::Builder::new()
        .name("all-smi-ioreport".to_string())
        .spawn(move || OwnedDict(query.copy_channels()))
    {
        Ok(handle) => ChannelQuery::Spawned(handle),
        Err(_) => ChannelQuery::Inline(query),
    }
}

/// Borrow the process-wide merged channel description, building it on first
/// use.
///
/// The returned reference stays valid for the life of the process and must not
/// be released by the caller.
fn merged_channels() -> Result<CFDictionaryRef, &'static str> {
    if let Some(cached) = MERGED_CHANNELS.get() {
        return Ok(cached.0);
    }

    let built = build_merged_channels()?;

    // Another thread may have published first; release the loser's copy rather
    // than leaking it. Either way the cache now holds the reference to use.
    if let Err(MergedChannels(duplicate)) = MERGED_CHANNELS.set(MergedChannels(built)) {
        unsafe { CFRelease(duplicate as *const c_void) };
    }

    // Set above either published `built` or found a winner already there, so
    // the cache is populated by now either way.
    MERGED_CHANNELS
        .get()
        .map(|cached| cached.0)
        .ok_or("IOReport channel cache was not published")
}

/// Opaque IOReport subscription reference
#[repr(C)]
struct IOReportSubscription {
    _data: [u8; 0],
    _phantom: PhantomData<(*mut u8, PhantomPinned)>,
}

type IOReportSubscriptionRef = *const IOReportSubscription;

// FFI declarations for IOReport library
#[link(name = "IOReport", kind = "dylib")]
unsafe extern "C" {
    fn IOReportCopyChannelsInGroup(
        group: CFStringRef,
        subgroup: CFStringRef,
        a: u64,
        b: u64,
        c: u64,
    ) -> CFDictionaryRef;

    fn IOReportMergeChannels(
        a: CFDictionaryRef,
        b: CFDictionaryRef,
        nil: CFTypeRef,
    ) -> CFDictionaryRef;

    fn IOReportCreateSubscription(
        a: *const c_void,
        desired_channels: CFMutableDictionaryRef,
        subscribed_channels: *mut CFMutableDictionaryRef,
        channel_id: u64,
        b: CFTypeRef,
    ) -> IOReportSubscriptionRef;

    fn IOReportCreateSamples(
        subscription: IOReportSubscriptionRef,
        channels: CFMutableDictionaryRef,
        a: CFTypeRef,
    ) -> CFDictionaryRef;

    fn IOReportCreateSamplesDelta(
        prev: CFDictionaryRef,
        curr: CFDictionaryRef,
        a: CFTypeRef,
    ) -> CFDictionaryRef;

    fn IOReportChannelGetGroup(channel: CFDictionaryRef) -> CFStringRef;
    fn IOReportChannelGetSubGroup(channel: CFDictionaryRef) -> CFStringRef;
    fn IOReportChannelGetChannelName(channel: CFDictionaryRef) -> CFStringRef;
    fn IOReportChannelGetUnitLabel(channel: CFDictionaryRef) -> CFStringRef;
    fn IOReportSimpleGetIntegerValue(channel: CFDictionaryRef, a: i32) -> i64;
    fn IOReportStateGetCount(channel: CFDictionaryRef) -> i32;
    fn IOReportStateGetNameForIndex(channel: CFDictionaryRef, index: i32) -> CFStringRef;
    fn IOReportStateGetResidency(channel: CFDictionaryRef, index: i32) -> i64;
}

// IOKit FFI declarations for GPU frequency discovery
#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOServiceMatching(name: *const i8) -> *mut c_void;
    fn IOServiceGetMatchingServices(
        master_port: u32,
        matching: *mut c_void,
        existing: *mut u32,
    ) -> i32;
    fn IOIteratorNext(iterator: u32) -> u32;
    fn IORegistryEntryGetName(entry: u32, name: *mut i8) -> i32;
    fn IORegistryEntryCreateCFProperties(
        entry: u32,
        properties: *mut CFMutableDictionaryRef,
        allocator: *const c_void,
        options: u32,
    ) -> i32;
    fn IOObjectRelease(object: u32) -> i32;
}

/// `mach_timebase_info_data_t`: `mach_absolute_time` ticks convert to
/// nanoseconds as `ticks * numer / denom`.
#[repr(C)]
#[derive(Default)]
struct MachTimebaseInfo {
    numer: u32,
    denom: u32,
}

// The mach clock IOReport stamps its elements with. Both live in libSystem,
// which every macOS binary links; they are declared here because the `libc`
// bindings for them are deprecated.
unsafe extern "C" {
    fn mach_absolute_time() -> u64;
    fn mach_timebase_info(info: *mut MachTimebaseInfo) -> i32;
}

/// Every `voltage-states*` table published by the IOKit pmgr/clpc device.
///
/// IOReport's performance-state channels report residency per *state name*
/// (`IDLE`, `V0P14`, `P3`, ...), never a clock. The `voltage-states*`
/// properties on the `AppleARMIODevice` `pmgr` (or `clpc`) node are the only
/// place the matching megahertz values live, so both the GPU path and the CPU
/// path must join a residency histogram against one of these tables to produce
/// a frequency at all.
///
/// Loaded once at startup. Entries are `(property name, frequencies in MHz,
/// ascending)`, sorted by property name; tables that parse to nothing are
/// dropped, which is how the several `voltage-states*` keys that hold clock
/// periods or bare state indices instead of hertz get excluded.
static PMGR_VOLTAGE_STATES: OnceLock<Vec<(String, Vec<u32>)>> = OnceLock::new();

/// GPU frequency table, resolved once from [`PMGR_VOLTAGE_STATES`].
static GPU_FREQUENCIES: OnceLock<Vec<u32>> = OnceLock::new();

/// pmgr keys holding the GPU performance-state table, most specific first.
const GPU_TABLE_KEYS: [&str; 2] = ["voltage-states9-sram", "voltage-states9"];

/// pmgr keys holding the efficiency-cluster table, most specific first.
///
/// The plain key is listed as a fallback because some chips publish the clock
/// there. On chips where both exist (M1 Ultra, for one) the plain key holds
/// clock *periods* rather than hertz; those fail the plausibility range in
/// [`parse_voltage_states_bytes`] and never reach this list, so listing the key
/// costs nothing.
const E_CLUSTER_TABLE_KEYS: [&str; 2] = ["voltage-states1-sram", "voltage-states1"];

/// pmgr keys holding the performance-cluster table, most specific first.
const P_CLUSTER_TABLE_KEYS: [&str; 2] = ["voltage-states5-sram", "voltage-states5"];

/// Load every `voltage-states*` table from the IOKit pmgr/clpc device.
///
/// - Matches the `AppleARMIODevice` service
/// - Looks for the `pmgr` or `clpc` node
/// - Reads every `voltage-states*` property off it
/// - Parses frequency values (first 4 bytes of each 8-byte entry, in Hz)
fn load_pmgr_voltage_states() -> Vec<(String, Vec<u32>)> {
    unsafe {
        let matching = IOServiceMatching(c"AppleARMIODevice".as_ptr());
        if matching.is_null() {
            return vec![];
        }

        let mut iterator: u32 = 0;
        // kIOMainPortDefault is 0
        if IOServiceGetMatchingServices(0, matching, &mut iterator) != 0 {
            return vec![];
        }

        let mut tables: Vec<(String, Vec<u32>)> = vec![];
        let mut entry = IOIteratorNext(iterator);

        while entry != 0 {
            let mut name_buf = [0i8; 128];
            IORegistryEntryGetName(entry, name_buf.as_mut_ptr());

            let name = std::ffi::CStr::from_ptr(name_buf.as_ptr())
                .to_str()
                .unwrap_or("");

            // Look for pmgr or clpc device
            if name == "pmgr" || name == "clpc" {
                let mut properties: CFMutableDictionaryRef = ptr::null_mut();
                if IORegistryEntryCreateCFProperties(entry, &mut properties, ptr::null(), 0) == 0
                    && !properties.is_null()
                {
                    tables = extract_voltage_state_tables(properties);
                    CFRelease(properties as *const c_void);
                }
            }

            IOObjectRelease(entry);
            if !tables.is_empty() {
                break; // Found the frequency tables, stop searching
            }
            entry = IOIteratorNext(iterator);
        }

        IOObjectRelease(iterator);
        tables
    }
}

/// Collect every parseable `voltage-states*` property from a pmgr node.
fn extract_voltage_state_tables(properties: CFMutableDictionaryRef) -> Vec<(String, Vec<u32>)> {
    unsafe {
        let count = core_foundation::dictionary::CFDictionaryGetCount(properties) as usize;
        if count == 0 {
            return vec![];
        }

        let mut keys: Vec<*const c_void> = vec![ptr::null(); count];
        let mut values: Vec<*const c_void> = vec![ptr::null(); count];
        core_foundation::dictionary::CFDictionaryGetKeysAndValues(
            properties,
            keys.as_mut_ptr(),
            values.as_mut_ptr(),
        );

        let mut tables: Vec<(String, Vec<u32>)> = Vec::new();

        for i in 0..count {
            let key_ref = keys[i] as CFStringRef;
            if key_ref.is_null() {
                continue;
            }

            let key_str = cfstr_to_string(key_ref).unwrap_or_default();
            if !key_str.starts_with("voltage-states") {
                continue;
            }

            let data_ref = values[i] as core_foundation::data::CFDataRef;
            if data_ref.is_null() {
                continue;
            }

            let frequencies = parse_voltage_states_data(data_ref);
            if frequencies.is_empty() {
                continue;
            }

            tables.push((key_str, frequencies));
        }

        // Sort so selection is deterministic across runs; IOKit does not
        // guarantee dictionary ordering.
        tables.sort_by(|a, b| a.0.cmp(&b.0));
        tables
    }
}

/// Minimum plausible clock in Hz (100 MHz).
const MIN_FREQ_HZ: u64 = 100_000_000;
/// Maximum plausible clock in Hz (6 GHz), comfortably above any shipping Apple
/// Silicon CPU or GPU clock.
const MAX_FREQ_HZ: u64 = 6_000_000_000;
/// Range of the 32-bit hertz field, used to undo wraparound.
const U32_SPAN_HZ: u64 = 1 << 32;
/// A wrapped entry can only follow one already near the 32-bit ceiling
/// (~4.295 GHz), so wrap correction is applied only above this threshold.
const WRAP_GUARD_HZ: u64 = 4_000_000_000;
/// Maximum number of frequency entries to parse
const MAX_FREQ_ENTRIES: usize = 64;

/// Parse voltage-states data to extract frequencies in MHz
fn parse_voltage_states_data(data_ref: core_foundation::data::CFDataRef) -> Vec<u32> {
    unsafe {
        let data = CFData::wrap_under_get_rule(data_ref);
        parse_voltage_states_bytes(data.bytes())
    }
}

/// Parse a `voltage-states*` payload into ascending MHz values.
///
/// Each entry is 8 bytes; the first 4 are the frequency in hertz as a
/// little-endian u32, the rest is the matching voltage. Entries outside a
/// plausible clock range are dropped, which is what keeps the keys that hold
/// clock periods or bare state indices from being mistaken for frequency
/// tables.
fn parse_voltage_states_bytes(bytes: &[u8]) -> Vec<u32> {
    let len = bytes.len();
    let total_entries = (len / 8).min(MAX_FREQ_ENTRIES);
    let mut frequencies: Vec<u32> = Vec::with_capacity(total_entries);
    // Last accepted value, used to detect 32-bit wraparound. Rejected entries
    // deliberately do not advance it, so a table of non-frequency data never
    // arms the wrap correction.
    let mut prev_hz: u64 = 0;

    for i in 0..total_entries {
        let offset = i * 8;
        if offset + 4 > len {
            break;
        }

        // Read 4-byte little-endian frequency value in Hz
        let raw_hz = u32::from_le_bytes([
            bytes[offset],
            bytes[offset + 1],
            bytes[offset + 2],
            bytes[offset + 3],
        ]) as u64;

        // Clocks above 4.295 GHz do not fit the 32-bit hertz field and wrap to
        // a small value. These tables are ascending, so a value that drops
        // right after an entry already near the ceiling is a wrap, not a real
        // step down.
        let freq_hz = if prev_hz >= WRAP_GUARD_HZ && raw_hz < prev_hz {
            raw_hz + U32_SPAN_HZ
        } else {
            raw_hz
        };

        // Validate frequency is in a reasonable range; this filters out
        // invalid/corrupted data and non-frequency payloads.
        if !(MIN_FREQ_HZ..=MAX_FREQ_HZ).contains(&freq_hz) {
            continue;
        }

        prev_hz = freq_hz;
        frequencies.push((freq_hz / 1_000_000) as u32);
    }

    frequencies
}

/// Get the cached pmgr frequency tables, loading them if necessary
fn get_pmgr_voltage_states() -> &'static [(String, Vec<u32>)] {
    PMGR_VOLTAGE_STATES.get_or_init(load_pmgr_voltage_states)
}

/// Get cached GPU frequencies, loading them if necessary
pub fn get_gpu_frequencies() -> &'static [u32] {
    GPU_FREQUENCIES.get_or_init(|| select_gpu_frequency_table(get_pmgr_voltage_states()))
}

/// Pick the GPU performance-state table out of the pmgr tables.
fn select_gpu_frequency_table(tables: &[(String, Vec<u32>)]) -> Vec<u32> {
    for key in GPU_TABLE_KEYS {
        if let Some((_, frequencies)) = tables.iter().find(|(k, _)| k == key) {
            return frequencies.clone();
        }
    }

    // Fallback for chips that number their rails differently: GPU clocks are
    // the lowest of the published tables, so take the one with the smallest
    // maximum.
    tables
        .iter()
        .min_by_key(|(_, frequencies)| frequencies.iter().max().copied().unwrap_or(u32::MAX))
        .map(|(_, frequencies)| frequencies.clone())
        .unwrap_or_default()
}

/// Which CPU cluster an IOReport `CPU Core Performance States` channel belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CpuCluster {
    /// M5 Pro/Max "Super" cores.
    Super,
    Efficiency,
    Performance,
}

/// Strip the `DIE_<n>_` prefix that multi-die packages put on every channel.
///
/// M1/M2 Ultra fuse two dies into one package and report channels as
/// `DIE_0_ECPU_CPU0`, `DIE_1_PCPU1_CPU3`, and so on. Single-die parts report
/// the bare `ECPU0` / `PCPU1` names. Stripping the prefix lets one set of
/// classification rules cover both.
fn strip_die_prefix(channel: &str) -> &str {
    let Some(rest) = channel.strip_prefix("DIE_") else {
        return channel;
    };
    let digits_end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    if digits_end == 0 {
        return channel;
    }
    rest[digits_end..].strip_prefix('_').unwrap_or(channel)
}

/// Classify a `CPU Core Performance States` channel into its cluster.
///
/// Returns `None` for channels that do not name a CPU cluster, so unknown
/// channels are ignored rather than folded into the wrong average.
fn classify_cpu_channel(channel: &str) -> Option<CpuCluster> {
    let name = strip_die_prefix(channel);

    // M5 Pro/Max uses MCPU0/MCPU0x for Super cores, MCPU1/MCPU1x for
    // performance cluster 1, and PCPU/PCPUx for performance cluster 2.
    // M1-M4 uses ECPU/ECPUx for efficiency cores and PCPU/PCPUx for
    // performance cores.
    if name.starts_with("MCPU0") {
        Some(CpuCluster::Super)
    } else if name.starts_with("MCPU1") {
        Some(CpuCluster::Performance)
    } else if name.contains("ECPU") || name.starts_with('E') {
        Some(CpuCluster::Efficiency)
    } else if name.contains("PCPU") || name.starts_with('P') {
        Some(CpuCluster::Performance)
    } else {
        None
    }
}

/// Pick the frequency table for a CPU cluster out of the pmgr tables.
///
/// `active_states` is the number of non-idle entries in the channel's residency
/// histogram. Residency entry N maps to table entry N, so a table is only
/// trustworthy when it has exactly one entry per active state; that count is
/// also the most reliable way to identify the right table on chips whose pmgr
/// key numbering is not the documented one.
fn select_cpu_frequency_table(
    tables: &[(String, Vec<u32>)],
    cluster: CpuCluster,
    active_states: usize,
) -> Option<&[u32]> {
    let preferred: &[&str] = match cluster {
        CpuCluster::Efficiency => &E_CLUSTER_TABLE_KEYS,
        CpuCluster::Performance => &P_CLUSTER_TABLE_KEYS,
        // No documented key for the M5 Super cluster; fall through to the
        // length match below.
        CpuCluster::Super => &[],
    };

    for key in preferred {
        if let Some((_, frequencies)) = tables.iter().find(|(k, _)| k == key)
            && frequencies.len() == active_states
        {
            return Some(frequencies);
        }
    }

    if active_states > 0 {
        let matched = tables
            .iter()
            .find(|(k, f)| f.len() == active_states && k.ends_with("-sram"))
            .or_else(|| tables.iter().find(|(_, f)| f.len() == active_states));
        if let Some((_, frequencies)) = matched {
            return Some(frequencies);
        }
    }

    // Last resort: the documented key even though its length disagrees. A
    // partially mapped clock still beats reporting 0 MHz.
    preferred
        .iter()
        .find_map(|key| tables.iter().find(|(k, _)| k == key))
        .map(|(_, frequencies)| frequencies.as_slice())
}

/// Names IOReport uses for performance states in which the block is not running.
fn is_idle_state(name: &str) -> bool {
    name.contains("IDLE") || name.contains("OFF") || name.contains("DOWN")
}

// Core Foundation helper functions
fn cfstr_to_string(cfstr: CFStringRef) -> Option<String> {
    if cfstr.is_null() {
        return None;
    }
    unsafe {
        let cf_string = CFString::wrap_under_get_rule(cfstr);
        Some(cf_string.to_string())
    }
}

/// Get array of dictionaries from CFDictionary
fn get_io_channels(dict: CFDictionaryRef) -> Vec<CFDictionaryRef> {
    if dict.is_null() {
        return vec![];
    }

    // SAFETY: `dict` is a valid sample dictionary the caller keeps alive for
    // the duration of this call. `wrap_under_get_rule` and `CFDictionary::find`
    // follow the CF get-rule (no extra retain on values borrowed from `dict`).
    // The `IOReportChannels` value is null-checked and type-checked as a
    // `CFArray` before it is wrapped, and every element of that array is
    // null-checked and type-checked as a `CFDictionary` before being handed
    // back, so a sample whose structure does not match what IOReport
    // normally returns yields an empty or filtered list instead of driving a
    // CF accessor with the wrong type.
    unsafe {
        let cf_dict = CFDictionary::<CFType, CFType>::wrap_under_get_rule(dict);
        let key = CFString::new("IOReportChannels");

        if let Some(channels) = cf_dict.find(key.as_CFType().as_CFTypeRef()) {
            // The channels value is a CFArray - get its raw pointer
            let arr_ref = channels.as_CFTypeRef() as core_foundation::array::CFArrayRef;
            if arr_ref.is_null()
                || CFGetTypeID(arr_ref as CFTypeRef) != core_foundation::array::CFArrayGetTypeID()
            {
                return vec![];
            }

            let arr = core_foundation::array::CFArray::<CFType>::wrap_under_get_rule(arr_ref);
            let count = arr.len();

            (0..count)
                .filter_map(|i| arr.get(i).map(|v| v.as_CFTypeRef() as CFDictionaryRef))
                .filter(|d| !d.is_null() && CFGetTypeID(*d as CFTypeRef) == CFDictionaryGetTypeID())
                .collect()
        } else {
            vec![]
        }
    }
}

/// `mach_absolute_time` timebase as `(numer, denom)`, read once.
static MACH_TIMEBASE: OnceLock<(u32, u32)> = OnceLock::new();

fn mach_timebase() -> (u32, u32) {
    *MACH_TIMEBASE.get_or_init(|| {
        let mut info = MachTimebaseInfo::default();
        // SAFETY: `info` is a valid, writable `mach_timebase_info_data_t`.
        let status = unsafe { mach_timebase_info(&mut info) };
        // The call does not fail in practice; 1/1 only keeps the arithmetic
        // defined if it ever does.
        if status == 0 && info.numer != 0 && info.denom != 0 {
            (info.numer, info.denom)
        } else {
            (1, 1)
        }
    })
}

/// Scale mach ticks to nanoseconds with an explicit timebase.
///
/// Saturates to `u64::MAX` instead of wrapping when the result overflows a
/// `u64`, which an out-of-range tick count from garbage `RawElements` bytes
/// (or an extreme timebase) could otherwise turn into an arbitrary,
/// possibly earlier-looking time. A zero `denom` is read as 1 rather than
/// dividing by zero.
fn scale_mach_ticks(ticks: u64, numer: u32, denom: u32) -> u64 {
    let ns = u128::from(ticks) * u128::from(numer) / u128::from(denom.max(1));
    u64::try_from(ns).unwrap_or(u64::MAX)
}

/// Convert `mach_absolute_time` ticks to nanoseconds.
///
/// A tick is not a nanosecond on Apple Silicon: the timebase is 125/3, one
/// tick per 41.67 ns.
fn mach_ticks_to_ns(ticks: u64) -> u64 {
    let (numer, denom) = mach_timebase();
    scale_mach_ticks(ticks, numer, denom)
}

/// Now, on the clock IOReport stamps elements with, in nanoseconds.
fn mach_now_ns() -> u64 {
    // SAFETY: takes no arguments and has no preconditions.
    mach_ticks_to_ns(unsafe { mach_absolute_time() })
}

/// Offset of the publication timestamp inside an `IOReportElement`: the
/// `mach_absolute_time` of the driver's last update (see the module docs for
/// the whole layout).
const RAW_ELEMENT_TIMESTAMP_OFFSET: usize = 24;

/// Read the publication timestamp out of a channel's `RawElements` bytes.
///
/// Returns `None` when the data is too short to hold one.
fn parse_raw_element_timestamp(bytes: &[u8]) -> Option<u64> {
    let raw = bytes.get(RAW_ELEMENT_TIMESTAMP_OFFSET..RAW_ELEMENT_TIMESTAMP_OFFSET + 8)?;
    Some(u64::from_le_bytes(raw.try_into().ok()?))
}

/// The publication timestamp of one channel of a raw sample, in mach ticks.
fn raw_element_timestamp(item: CFDictionaryRef) -> Option<u64> {
    let key = get_cfstring_refs().raw_elements;
    // SAFETY: `item` is a channel dictionary borrowed from a raw sample the
    // caller keeps alive for the duration of this call. `CFDictionaryGetValue`
    // follows the get rule (no retain) and `key` is the process-lifetime
    // static `RawElements` CFString. The returned value is null-checked and
    // type-checked as CFData before `wrap_under_get_rule` retains it, so
    // `data.bytes()` borrows memory valid for the life of `data`.
    unsafe {
        let value = CFDictionaryGetValue(item, key as *const c_void);
        if value.is_null() || CFGetTypeID(value) != CFDataGetTypeID() {
            return None;
        }
        let data = CFData::wrap_under_get_rule(value as CFDataRef);
        parse_raw_element_timestamp(data.bytes())
    }
}

/// The channel's name, if it belongs to the `Energy Model` group.
///
/// The group is compared as a CFString, so a channel from another group costs
/// no string conversion.
fn energy_channel_name(item: CFDictionaryRef) -> Option<String> {
    let energy_model = get_cfstring_refs().energy_model as CFTypeRef;
    // SAFETY: `item` is a live channel dictionary from the caller's sample.
    // `IOReportChannelGetGroup` returns a get-rule reference, null-checked
    // before `CFEqual`, and `energy_model` is the process-lifetime static
    // `Energy Model` CFString, so both operands stay valid for the call.
    unsafe {
        let group = IOReportChannelGetGroup(item);
        if group.is_null() || CFEqual(group as CFTypeRef, energy_model) == 0 {
            return None;
        }
        cfstr_to_string(IOReportChannelGetChannelName(item))
    }
}

/// The channel's unit label (`mJ`, `uJ`, `nJ`, ...), empty when it has none.
fn channel_unit(item: CFDictionaryRef) -> String {
    // SAFETY: `item` is a live channel dictionary from the caller's sample,
    // owned and kept alive by the caller for the duration of this call.
    unsafe { cfstr_to_string(IOReportChannelGetUnitLabel(item)).unwrap_or_default() }
}

/// Read every tracked `Energy Model` channel out of a raw (non-delta) sample.
///
/// Only channels that classify into a rail are read past their name, so the
/// ~360 untracked channels of an M5 Max cost one name conversion each.
fn energy_observations(sample: CFDictionaryRef) -> Vec<EnergyObservation> {
    get_io_channels(sample)
        .into_iter()
        .filter_map(|item| {
            let channel = energy_channel_name(item)?;
            classify_energy_channel(&channel)?;
            Some(EnergyObservation {
                unit: channel_unit(item),
                // SAFETY: `item` is a live channel dictionary from `sample`,
                // which the caller keeps alive for the duration of this call.
                value: unsafe { IOReportSimpleGetIntegerValue(item, 0) },
                timestamp_ns: raw_element_timestamp(item).map(mach_ticks_to_ns),
                channel,
            })
        })
        .collect()
}

/// Item from IOReport iteration
#[derive(Debug, Clone)]
pub struct IOReportChannelItem {
    pub group: String,
    pub subgroup: String,
    pub channel: String,
    pub item: CFDictionaryRef,
}

impl IOReportChannelItem {
    /// Get state residencies as (name, residency) pairs
    pub fn get_residencies(&self) -> Vec<(String, i64)> {
        if self.item.is_null() {
            return vec![];
        }

        unsafe {
            let count = IOReportStateGetCount(self.item);
            (0..count)
                .filter_map(|i| {
                    let name_ref = IOReportStateGetNameForIndex(self.item, i);
                    let name = cfstr_to_string(name_ref)?;
                    let residency = IOReportStateGetResidency(self.item, i);
                    Some((name, residency))
                })
                .collect()
        }
    }
}

/// Iterator over IOReport sample channels
///
/// This struct takes ownership of the sample CFDictionaryRef and releases it
/// when the iterator is dropped, preventing memory leaks.
pub struct IOReportIterator {
    /// The sample CFDictionary that owns the channel data.
    /// Must be released when the iterator is dropped.
    sample: CFDictionaryRef,
    channels: Vec<CFDictionaryRef>,
    index: usize,
}

impl IOReportIterator {
    fn new(sample: CFDictionaryRef) -> Self {
        let channels = get_io_channels(sample);
        Self {
            sample,
            channels,
            index: 0,
        }
    }
}

impl Drop for IOReportIterator {
    fn drop(&mut self) {
        // Release the sample dictionary to prevent memory leaks
        if !self.sample.is_null() {
            unsafe {
                CFRelease(self.sample as *const c_void);
            }
        }
    }
}

impl Iterator for IOReportIterator {
    type Item = IOReportChannelItem;

    fn next(&mut self) -> Option<Self::Item> {
        if self.index >= self.channels.len() {
            return None;
        }

        let item = self.channels[self.index];
        self.index += 1;

        if item.is_null() {
            return self.next();
        }

        unsafe {
            let group = cfstr_to_string(IOReportChannelGetGroup(item)).unwrap_or_default();
            let subgroup = cfstr_to_string(IOReportChannelGetSubGroup(item)).unwrap_or_default();
            let channel = cfstr_to_string(IOReportChannelGetChannelName(item)).unwrap_or_default();

            Some(IOReportChannelItem {
                group,
                subgroup,
                channel,
                item,
            })
        }
    }
}

/// Channel groups to subscribe to
const ENERGY_MODEL: &str = "Energy Model";
const CPU_STATS: &str = "CPU Stats";
const CPU_PERF_STATES: &str = "CPU Core Performance States";
const GPU_STATS: &str = "GPU Stats";
const GPU_PERF_STATES: &str = "GPU Performance States";

/// Key of the raw element data inside each channel of a sample.
const RAW_ELEMENTS: &str = "RawElements";

/// Shortest interval [`IOReport::get_sample_since_last`] will turn into a
/// delta. Residency over a window this short is dominated by sampling jitter;
/// below it the call reports "no delta yet" instead of a wild rate.
const MIN_DELTA_WINDOW: std::time::Duration = std::time::Duration::from_millis(50);

/// IOReport subscription manager
pub struct IOReport {
    subscription: IOReportSubscriptionRef,
    channels: CFMutableDictionaryRef,
    /// Baseline for the residency delta.
    prev_sample: Option<(CFDictionaryRef, Instant)>,
    /// Per-channel energy baselines, fed by every raw sample taken here.
    energy: EnergyTracker,
}

impl IOReport {
    /// Create a new IOReport subscription for the specified channel groups
    ///
    /// The channel description this subscribes to is enumerated once per
    /// process and cached, so repeated construction (a library consumer
    /// building more than one client over the process's life) pays only for
    /// the subscription itself.
    pub fn new() -> Result<Self, &'static str> {
        // Borrowed from the process-wide cache, which retains it. Not ours to
        // release.
        let merged = merged_channels()?;

        unsafe {
            // Create mutable copy for subscription. The cached dictionary must
            // stay pristine, so the subscription always gets its own copy.
            let count = core_foundation::dictionary::CFDictionaryGetCount(merged) as isize;
            let channels = core_foundation::dictionary::CFDictionaryCreateMutableCopy(
                core_foundation::base::kCFAllocatorDefault,
                count,
                merged,
            );

            if channels.is_null() {
                return Err("Failed to create mutable channel dictionary");
            }

            // Create subscription
            let mut subscribed_channels: CFMutableDictionaryRef = ptr::null_mut();
            let subscription = IOReportCreateSubscription(
                ptr::null(),
                channels,
                &mut subscribed_channels,
                0,
                ptr::null(),
            );

            if subscription.is_null() {
                CFRelease(channels as *const c_void);
                return Err("Failed to create IOReport subscription");
            }

            Ok(Self {
                subscription,
                channels,
                prev_sample: None,
                energy: EnergyTracker::default(),
            })
        }
    }

    /// Get a delta sample over the specified duration.
    ///
    /// This blocks the calling thread for `duration_ms` to open the delta
    /// window, so it observes only that window out of however long the caller
    /// waits between calls. Prefer [`get_sample_since_last`], which deltas
    /// against the previous call and therefore covers the whole interval
    /// without sleeping. This variant remains for the first sample of a
    /// session, where there is no previous sample to delta against.
    ///
    /// Both samples feed the energy tracker, so power after this call is
    /// whatever the tracker holds, not energy over this window. On an M5 Max
    /// that is usually no reading yet, since the mJ counters publish every
    /// ~2.1 s and rarely inside a 100 ms window; an exact reading when a
    /// publication did land in it; and never the ~20x spike that dividing a
    /// whole 2.1 s batch by a 100 ms window used to produce. Channels stamped
    /// at sample time, such as `GPU Energy`, read normally.
    pub fn get_sample(&mut self, duration_ms: u64) -> Result<IOReportIterator, &'static str> {
        let sample1 = self.take_sample()?;

        std::thread::sleep(std::time::Duration::from_millis(duration_ms));

        let sample2 = match self.take_sample() {
            Ok(sample) => sample,
            Err(e) => {
                // SAFETY: `sample1` is the +1 sample taken above by
                // `take_sample`; this error path returns without using it
                // again, so releasing it here is the only release it gets.
                unsafe { CFRelease(sample1 as *const c_void) };
                return Err(e);
            }
        };

        // Calculate delta
        let delta = unsafe {
            let d = IOReportCreateSamplesDelta(sample1, sample2, ptr::null());
            CFRelease(sample1 as *const c_void);
            CFRelease(sample2 as *const c_void);
            d
        };

        if delta.is_null() {
            return Err("Failed to create sample delta");
        }

        Ok(IOReportIterator::new(delta))
    }

    /// Get a delta sample covering the time since the previous call, without
    /// blocking.
    ///
    /// The residency channels in the CPU/GPU stats groups are cumulative
    /// counters, so a delta between two arbitrary samples is exactly the
    /// activity that occurred between them. Retaining the newest sample and
    /// differencing the next call against it therefore yields a window equal
    /// to the caller's polling period, with no `sleep` and a single
    /// `IOReportCreateSamples` call per poll.
    ///
    /// Power does not come from this delta. The sample also feeds the energy
    /// tracker, read through [`energy_readings`], which times each Energy
    /// Model channel by its own publication timestamps (see the module docs).
    ///
    /// Returns `Ok(None)` when no usable delta is available, which happens in
    /// two cases: the first call of a session (nothing to delta against yet),
    /// and a call so soon after the previous one that the window would be
    /// below [`MIN_DELTA_WINDOW`]. Rates derived from a near-zero window are
    /// meaningless, so the baseline is left in place for the next call rather
    /// than consumed. Callers that need a value immediately can fall back to
    /// [`get_sample`].
    ///
    /// [`get_sample`]: Self::get_sample
    /// [`get_sample_since_last`]: Self::get_sample_since_last
    /// [`energy_readings`]: Self::energy_readings
    pub fn get_sample_since_last(&mut self) -> Result<Option<IOReportIterator>, &'static str> {
        let sample = self.take_sample()?;
        let now = Instant::now();

        // Peek before consuming: a too-short window must not discard a usable
        // baseline, or a caller polling faster than MIN_DELTA_WINDOW would
        // never accumulate one.
        if let Some((_, prev_at)) = self.prev_sample.as_ref()
            && now.duration_since(*prev_at) < MIN_DELTA_WINDOW
        {
            unsafe { CFRelease(sample as *const c_void) };
            return Ok(None);
        }

        let Some((prev, _)) = self.prev_sample.replace((sample, now)) else {
            // First call: `sample` is now retained as the baseline.
            return Ok(None);
        };

        // `IOReportCreateSamplesDelta` does not take ownership of either
        // argument. `sample` stays retained as the new baseline (it is already
        // stored in `prev_sample`), so only the old baseline is released here.
        let delta = unsafe {
            let d = IOReportCreateSamplesDelta(prev, sample, ptr::null());
            CFRelease(prev as *const c_void);
            d
        };

        if delta.is_null() {
            return Err("Failed to create sample delta");
        }

        Ok(Some(IOReportIterator::new(delta)))
    }

    /// Power per rail as of the most recent raw sample.
    pub fn energy_readings(&self) -> EnergyReadings {
        self.energy.readings()
    }

    /// Take a single raw sample and feed its energy channels to the tracker.
    ///
    /// Every sample this subscription takes passes through here, so the
    /// tracker sees all of them, in order, and reads its timestamps from raw
    /// samples rather than from a delta.
    fn take_sample(&mut self) -> Result<CFDictionaryRef, &'static str> {
        // SAFETY: `self.subscription` and `self.channels` are owned by
        // `self` and stay valid until `Drop`. The returned sample is a +1
        // reference; ownership passes to the caller, which is responsible
        // for releasing it (directly, or via `IOReportIterator`'s `Drop`).
        let sample =
            unsafe { IOReportCreateSamples(self.subscription, self.channels, ptr::null()) };
        if sample.is_null() {
            return Err("Failed to create IOReport sample");
        }
        let observed_at_ns = mach_now_ns();
        self.energy
            .observe_sample(observed_at_ns, energy_observations(sample));
        Ok(sample)
    }
}

impl Drop for IOReport {
    fn drop(&mut self) {
        unsafe {
            if let Some((prev, _)) = self.prev_sample.take()
                && !prev.is_null()
            {
                CFRelease(prev as *const c_void);
            }
            if !self.channels.is_null() {
                CFRelease(self.channels as *const c_void);
            }
            // Note: subscription cleanup is handled by the system
        }
    }
}

// Safety: IOReport is safe to send between threads
// The FFI calls are thread-safe and we don't share mutable state
unsafe impl Send for IOReport {}
unsafe impl Sync for IOReport {}

// Note: The old cfstring() helper was removed because it caused use-after-free.
// The CFString would be dropped immediately after as_concrete_TypeRef() was called,
// leaving a dangling pointer. Now we use static CFString constants that are kept
// alive for the lifetime of the application.

/// Collected metrics from IOReport
#[derive(Debug, Default, Clone)]
pub struct IOReportMetrics {
    // Power metrics (in watts)
    pub cpu_power: f64,
    pub gpu_power: f64,
    pub ane_power: f64,
    pub dram_power: f64,
    pub package_power: f64,

    // CPU frequency metrics (in MHz)
    pub s_cluster_freq: u32,
    pub e_cluster_freq: u32,
    pub p_cluster_freq: u32,
    pub s_cluster_residency: f64,
    pub e_cluster_residency: f64,
    pub p_cluster_residency: f64,

    // GPU metrics
    pub gpu_freq: u32,
    pub gpu_residency: f64,

    // Raw per-cluster data for Ultra chips
    pub s_cluster_data: Vec<(u32, f64)>, // (freq_mhz, residency_percent) - Super cores (M5)
    pub e_cluster_data: Vec<(u32, f64)>, // (freq_mhz, residency_percent)
    pub p_cluster_data: Vec<(u32, f64)>,
}

impl IOReportMetrics {
    /// Collect metrics from an IOReport delta sample and the subscription's
    /// current power readings.
    ///
    /// Residency and frequency come from `iterator`, the delta between two
    /// samples. Power comes from `power` ([`IOReport::energy_readings`]), not
    /// from the delta, because the Energy Model counters have to be timed by
    /// their own publication timestamps (see the module docs).
    pub fn from_sample(iterator: IOReportIterator, power: EnergyReadings) -> Self {
        let mut metrics = Self {
            cpu_power: power.cpu,
            gpu_power: power.gpu,
            ane_power: power.ane,
            dram_power: power.dram,
            package_power: power.package(),
            ..Self::default()
        };

        let mut s_cluster_freqs: Vec<(u32, f64)> = vec![];
        let mut e_cluster_freqs: Vec<(u32, f64)> = vec![];
        let mut p_cluster_freqs: Vec<(u32, f64)> = vec![];
        let mut gpu_freqs: Vec<(u32, f64)> = vec![];

        for item in iterator {
            match (item.group.as_str(), item.subgroup.as_str()) {
                ("CPU Stats", "CPU Core Performance States") => {
                    Self::process_cpu_channel(
                        &item,
                        &mut s_cluster_freqs,
                        &mut e_cluster_freqs,
                        &mut p_cluster_freqs,
                    );
                }
                ("GPU Stats", "GPU Performance States") if item.channel == "GPUPH" => {
                    Self::process_gpu_channel(&item, &mut gpu_freqs);
                }
                _ => {}
            }
        }

        // Calculate averages for clusters
        metrics.s_cluster_data = s_cluster_freqs.clone();
        metrics.e_cluster_data = e_cluster_freqs.clone();
        metrics.p_cluster_data = p_cluster_freqs.clone();

        if let Some((freq, residency)) = Self::calculate_cluster_average(&s_cluster_freqs) {
            metrics.s_cluster_freq = freq;
            metrics.s_cluster_residency = residency;
        }
        if let Some((freq, residency)) = Self::calculate_cluster_average(&e_cluster_freqs) {
            metrics.e_cluster_freq = freq;
            metrics.e_cluster_residency = residency;
        }
        if let Some((freq, residency)) = Self::calculate_cluster_average(&p_cluster_freqs) {
            metrics.p_cluster_freq = freq;
            metrics.p_cluster_residency = residency;
        }
        if let Some((freq, residency)) = Self::calculate_cluster_average(&gpu_freqs) {
            metrics.gpu_freq = freq;
            metrics.gpu_residency = residency;
        }

        metrics
    }

    fn process_cpu_channel(
        item: &IOReportChannelItem,
        s_cluster_freqs: &mut Vec<(u32, f64)>,
        e_cluster_freqs: &mut Vec<(u32, f64)>,
        p_cluster_freqs: &mut Vec<(u32, f64)>,
    ) {
        let residencies = item.get_residencies();
        if residencies.is_empty() {
            return;
        }

        let Some(cluster) = classify_cpu_channel(&item.channel) else {
            return;
        };

        // IOReport names CPU performance states symbolically (`IDLE`, `V0P14`,
        // ... `V14P0`), never in megahertz, so the clock has to come from the
        // pmgr voltage-states table for this cluster. Without that join every
        // CPU frequency reads 0.
        let active_states = residencies
            .iter()
            .filter(|(name, _)| !is_idle_state(name))
            .count();
        let table = select_cpu_frequency_table(get_pmgr_voltage_states(), cluster, active_states);

        let (freq, residency) = match table {
            Some(table) if !table.is_empty() => Self::calc_freq_with_table(&residencies, table),
            // No usable table. Residency is still meaningful, so keep
            // reporting it and let the frequency fall back to whatever the
            // state names themselves yield (nothing, on current chips).
            _ => Self::calc_freq_from_residencies(&residencies),
        };

        match cluster {
            CpuCluster::Super => s_cluster_freqs.push((freq, residency)),
            CpuCluster::Efficiency => e_cluster_freqs.push((freq, residency)),
            CpuCluster::Performance => p_cluster_freqs.push((freq, residency)),
        }
    }

    fn process_gpu_channel(item: &IOReportChannelItem, gpu_freqs: &mut Vec<(u32, f64)>) {
        let residencies = item.get_residencies();
        if residencies.is_empty() {
            return;
        }

        // Get pre-loaded GPU frequencies from IOKit pmgr device
        let gpu_freq_table = get_gpu_frequencies();

        // Use IOKit frequencies if available, otherwise fall back to parsing state names
        let (freq, residency) = if !gpu_freq_table.is_empty() {
            Self::calc_freq_with_table(&residencies, gpu_freq_table)
        } else {
            Self::calc_freq_from_residencies(&residencies)
        };
        gpu_freqs.push((freq, residency));
    }

    /// Calculate a block's frequency from its residency histogram and the
    /// pre-loaded pmgr frequency table.
    ///
    /// Used by both the GPU (`GPUPH`) and the CPU cluster channels, whose
    /// state names are symbolic in both cases:
    /// - Active states (non-OFF/IDLE/DOWN) are mapped to frequencies in order
    /// - The frequency table from the pmgr device provides accurate MHz values
    fn calc_freq_with_table(residencies: &[(String, i64)], freq_table: &[u32]) -> (u32, f64) {
        let mut total_residency: i64 = 0;
        let mut active_residency: i64 = 0;
        let mut weighted_freq: f64 = 0.0;
        let mut active_state_idx: usize = 0;

        for (name, residency) in residencies {
            total_residency += residency;

            // Skip idle/off states
            if is_idle_state(name) {
                continue;
            }

            active_residency += residency;

            // Map active state index to frequency from table
            if active_state_idx < freq_table.len() {
                weighted_freq += freq_table[active_state_idx] as f64 * *residency as f64;
            }
            active_state_idx += 1;
        }

        if total_residency == 0 {
            return (0, 0.0);
        }

        let avg_freq = if active_residency > 0 {
            (weighted_freq / active_residency as f64) as u32
        } else if !freq_table.is_empty() {
            // The block is present (total_residency > 0) but spent the entire
            // sampling window in IDLE/OFF/DOWN states. It is not running at
            // 0 Hz, it is parked at its lowest P-state. Report the lowest
            // entry of the IOKit pmgr frequency table (sorted ascending) so
            // the renderer shows a truthful idle clock instead of letting
            // the Freq field flicker between a real value and 0 every
            // refresh cycle.
            freq_table[0]
        } else {
            0
        };

        let residency_pct = (active_residency as f64 / total_residency as f64) * 100.0;

        (avg_freq, residency_pct)
    }

    /// Calculate frequency and residency from state residencies whose names are
    /// themselves megahertz values.
    ///
    /// This is the last-resort path used when no pmgr frequency table is
    /// available. No shipping Apple Silicon chip names its performance states
    /// numerically (CPU clusters use `V0P14`-style names, the GPU uses
    /// `P1`..`P15`), so in practice this yields residency only and the caller
    /// gets 0 MHz. Prefer [`calc_freq_with_table`].
    ///
    /// [`calc_freq_with_table`]: Self::calc_freq_with_table
    fn calc_freq_from_residencies(residencies: &[(String, i64)]) -> (u32, f64) {
        let mut total_residency: i64 = 0;
        let mut weighted_freq: i64 = 0;
        let mut active_residency: i64 = 0;
        // Track the lowest parseable active-state frequency seen during the
        // scan. Used as the idle fallback when the cluster is present
        // (total_residency > 0) but spent the entire window in IDLE/OFF/DOWN
        // states, which is its parked P-state, not 0 Hz.
        let mut min_active_freq: Option<i64> = None;

        for (name, residency) in residencies {
            total_residency += residency;

            // Skip idle/off states
            if is_idle_state(name) {
                continue;
            }

            active_residency += residency;

            // Parse frequency from state name (e.g., "2064" for 2064 MHz)
            if let Ok(freq) = name.trim().parse::<i64>() {
                weighted_freq += freq * residency;
                min_active_freq = Some(min_active_freq.map_or(freq, |m| m.min(freq)));
            }
        }

        if total_residency == 0 {
            return (0, 0.0);
        }

        let avg_freq = if active_residency > 0 {
            (weighted_freq / active_residency) as u32
        } else {
            // Cluster is present but parked at its lowest P-state for the
            // entire window. Report the minimum active-state frequency we
            // saw (the parked P-state) rather than masquerading as no data.
            // This applies to both the GPU fallback path (no IOKit pmgr
            // table available) and CPU clusters, which both call this
            // helper via process_cpu_channel.
            min_active_freq.unwrap_or(0) as u32
        };

        let residency_pct = (active_residency as f64 / total_residency as f64) * 100.0;

        (avg_freq, residency_pct)
    }

    fn calculate_cluster_average(data: &[(u32, f64)]) -> Option<(u32, f64)> {
        if data.is_empty() {
            return None;
        }

        let count = data.len() as f64;
        let avg_freq = data.iter().map(|(f, _)| *f as f64).sum::<f64>() / count;
        let avg_residency = data.iter().map(|(_, r)| *r).sum::<f64>() / count;

        Some((avg_freq as u32, avg_residency))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Opening a second subscription must not re-enumerate the channel groups.
    ///
    /// Enumerating them is the expensive half of `IOReport::new`, and a library
    /// consumer that constructs and drops several clients over a process's life
    /// used to pay it every time (issue #374). Pointer identity is the direct
    /// evidence that the second call reused the first call's work; a timing
    /// assertion would prove the same thing less reliably.
    ///
    /// Skipped where IOReport is unavailable (an Intel Mac, a VM, a sandboxed
    /// runner), which is not a failure.
    #[test]
    #[cfg(target_os = "macos")]
    fn channel_enumeration_happens_once_per_process() {
        let Ok(first) = merged_channels() else {
            return;
        };
        let second = merged_channels().expect("a cached lookup cannot start failing");
        assert_eq!(
            first, second,
            "the merged channel dictionary must be enumerated once and reused"
        );
    }

    /// Every subscription gets its own mutable copy, so nothing a subscription
    /// does can reach the cached description the next one is built from.
    #[test]
    #[cfg(target_os = "macos")]
    fn each_subscription_copies_the_cached_channels() {
        let Ok(cached) = merged_channels() else {
            return;
        };
        let Ok(report) = IOReport::new() else {
            return;
        };
        assert_ne!(
            report.channels as CFDictionaryRef, cached,
            "a subscription must not hold the cached dictionary itself"
        );
    }

    /// Two subscriptions can be open at once, which is what a second client
    /// constructed before the first is dropped amounts to.
    #[test]
    #[cfg(target_os = "macos")]
    fn two_subscriptions_can_coexist() {
        let Ok(first) = IOReport::new() else {
            return;
        };
        let Ok(second) = IOReport::new() else {
            return;
        };
        assert!(!first.channels.is_null());
        assert!(!second.channels.is_null());
        assert_ne!(
            first.channels, second.channels,
            "each subscription needs its own channel dictionary"
        );
    }

    #[test]
    fn raw_element_timestamp_is_read_at_offset_24() {
        // An `IOReportElement` laid out as the M5 Max reports one: provider
        // id, channel id, channel type, timestamp, value, then the rest of the
        // 64 bytes.
        let mut element = Vec::with_capacity(64);
        element.extend_from_slice(&0x1000_0abcu64.to_le_bytes());
        element.extend_from_slice(&7u64.to_le_bytes());
        element.extend_from_slice(&0x0001_0002u64.to_le_bytes());
        element.extend_from_slice(&123_456_789_012u64.to_le_bytes());
        element.extend_from_slice(&4_969i64.to_le_bytes());
        element.resize(64, 0);

        assert_eq!(parse_raw_element_timestamp(&element), Some(123_456_789_012));
        assert_eq!(
            parse_raw_element_timestamp(&element[..32]),
            Some(123_456_789_012)
        );
        assert_eq!(parse_raw_element_timestamp(&element[..31]), None);
        assert_eq!(parse_raw_element_timestamp(&[]), None);
    }

    #[test]
    fn mach_ticks_scale_by_the_timebase() {
        // Apple Silicon's 125/3 timebase is a 24 MHz tick.
        assert_eq!(scale_mach_ticks(24_000_000, 125, 3), 1_000_000_000);
        assert_eq!(scale_mach_ticks(3, 125, 3), 125);
        // Intel's 1/1: ticks are already nanoseconds.
        assert_eq!(scale_mach_ticks(42, 1, 1), 42);
        // The intermediate product must not overflow.
        let ticks = u64::MAX / 125 * 3;
        assert_eq!(scale_mach_ticks(ticks, 125, 3), u64::MAX / 125 * 125);
        // An out-of-range result saturates instead of wrapping.
        assert_eq!(scale_mach_ticks(u64::MAX, 125, 3), u64::MAX);
        // A zero denom is read as 1 rather than dividing by zero.
        assert_eq!(scale_mach_ticks(42, 1, 0), 42);
    }

    /// Hardware diagnostic for the Energy Model (issue #410). Prints this
    /// machine's channel inventory in the fixture format, then for six seconds
    /// at 100 ms every tracked channel's counter delta (`dv`), publication
    /// span (`dts`), and publication age, read from raw samples, next to the
    /// rail readings the tracker produced from them.
    ///
    /// Use it to record another chip family next to
    /// `tests/fixtures/ioreport/m5_max_energy_model.tsv` and to check that its
    /// publication cadence matches what the tracker assumes:
    ///
    /// ```text
    /// cargo test --lib ioreport_energy_diagnostics -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "hardware diagnostic; run by hand with --ignored --nocapture"]
    #[cfg(target_os = "macos")]
    fn ioreport_energy_diagnostics() {
        use std::collections::HashMap;

        let Ok(mut report) = IOReport::new() else {
            println!("IOReport is unavailable on this host; nothing to report");
            return;
        };
        let Ok(sample) = report.take_sample() else {
            println!("IOReport returned no sample; nothing to report");
            return;
        };

        let (numer, denom) = mach_timebase();
        println!("# mach timebase {numer}/{denom}");
        println!("# Energy Model inventory (name<TAB>unit)");
        let mut inventory = 0;
        for item in get_io_channels(sample) {
            if let Some(name) = energy_channel_name(item) {
                println!("{name}\t{}", channel_unit(item));
                inventory += 1;
            }
        }
        println!("# {inventory} channels");
        // SAFETY: `sample` is the +1 reference `take_sample` returns, released
        // exactly once here and not used afterwards.
        unsafe { CFRelease(sample as *const c_void) };

        println!("# per tracked channel: dv = counter delta, dts = publication span");
        let mut previous: HashMap<String, (i64, Option<u64>)> = HashMap::new();
        let start = Instant::now();
        for tick in 0..60 {
            std::thread::sleep(std::time::Duration::from_millis(100));
            let Ok(sample) = report.take_sample() else {
                continue;
            };
            let now_ns = mach_now_ns();
            let observations = energy_observations(sample);
            // SAFETY: `sample` is the +1 reference `take_sample` returns for
            // this iteration, released exactly once here; `energy_observations`
            // has already copied out everything it needs, so `sample` is not
            // used afterwards.
            unsafe { CFRelease(sample as *const c_void) };

            let ms = |ns: u64| format!("{:.1}ms", ns as f64 / 1e6);
            let fields: Vec<String> = observations
                .iter()
                .map(|obs| {
                    let (prev_value, prev_ts) = previous
                        .get(&obs.channel)
                        .copied()
                        .unwrap_or((obs.value, obs.timestamp_ns));
                    let dts = match (obs.timestamp_ns, prev_ts) {
                        (Some(ts), Some(prev)) => ms(ts.saturating_sub(prev)),
                        _ => "none".to_string(),
                    };
                    let age = obs
                        .timestamp_ns
                        .map_or("none".to_string(), |ts| ms(now_ns.saturating_sub(ts)));
                    format!(
                        "{} dv={}{} dts={dts} age={age}",
                        obs.channel,
                        obs.value.saturating_sub(prev_value),
                        obs.unit
                    )
                })
                .collect();
            for obs in observations {
                previous.insert(obs.channel, (obs.value, obs.timestamp_ns));
            }

            let rails = report.energy_readings();
            println!(
                "{tick:03} t={:.3}s {} || cpu={:.2}W gpu={:.2}W ane={:.2}W dram={:.2}W",
                start.elapsed().as_secs_f64(),
                fields.join(" | "),
                rails.cpu,
                rails.gpu,
                rails.ane,
                rails.dram
            );
        }
    }

    #[test]
    fn test_calc_freq_from_residencies() {
        let residencies = vec![
            ("IDLE".to_string(), 500),
            ("600".to_string(), 100),
            ("1200".to_string(), 200),
            ("2400".to_string(), 200),
        ];

        let (freq, residency) = IOReportMetrics::calc_freq_from_residencies(&residencies);

        // Active residency: 100 + 200 + 200 = 500 out of 1000 total = 50%
        assert!((residency - 50.0).abs() < 0.1);

        // Weighted freq: (600*100 + 1200*200 + 2400*200) / 500 = 1560
        assert_eq!(freq, 1560);
    }

    #[test]
    fn test_calculate_cluster_average() {
        let data = vec![(1000, 50.0), (2000, 60.0), (1500, 40.0)];

        let result = IOReportMetrics::calculate_cluster_average(&data);
        assert!(result.is_some());

        let (avg_freq, avg_residency) = result.unwrap();
        assert_eq!(avg_freq, 1500);
        assert!((avg_residency - 50.0).abs() < 0.1);
    }

    #[test]
    fn test_calculate_cluster_average_empty() {
        let result = IOReportMetrics::calculate_cluster_average(&[]);
        assert!(result.is_none());
    }

    #[test]
    fn test_calc_gpu_freq_with_table() {
        // Simulate GPUPH residencies: OFF, IDLE, then active states
        let residencies = vec![
            ("OFF".to_string(), 100),
            ("IDLE".to_string(), 400),
            ("state0".to_string(), 200), // Maps to freq_table[0]
            ("state1".to_string(), 200), // Maps to freq_table[1]
            ("state2".to_string(), 100), // Maps to freq_table[2]
        ];

        // GPU frequency table from IOKit (in MHz)
        let freq_table = [396, 720, 1398];

        let (freq, residency) = IOReportMetrics::calc_freq_with_table(&residencies, &freq_table);

        // Active residency: 200 + 200 + 100 = 500 out of 1000 total = 50%
        assert!((residency - 50.0).abs() < 0.1);

        // Weighted freq: (396*200 + 720*200 + 1398*100) / 500 = 725.2
        // (79200 + 144000 + 139800) / 500 = 726
        assert_eq!(freq, 726);
    }

    #[test]
    fn test_calc_gpu_freq_with_empty_table() {
        // When freq_table is empty, calc_gpu_freq_with_table should return 0 frequency
        // but still calculate residency correctly
        let residencies = vec![("OFF".to_string(), 100), ("state0".to_string(), 200)];

        let freq_table: [u32; 0] = [];

        let (freq, residency) = IOReportMetrics::calc_freq_with_table(&residencies, &freq_table);

        // Active residency: 200 out of 300 total = 66.67%
        assert!((residency - 66.67).abs() < 0.1);

        // No frequency data available
        assert_eq!(freq, 0);
    }

    #[test]
    fn test_calc_freq_from_residencies_all_idle() {
        // Cluster is present and idle, but there are no parseable active
        // state names available to fall back on. Expected: (0, 0.0) —
        // truly no usable signal.
        let residencies = vec![("IDLE".to_string(), 500)];

        let (freq, residency) = IOReportMetrics::calc_freq_from_residencies(&residencies);

        assert_eq!(freq, 0);
        assert!((residency - 0.0).abs() < 0.1);
    }

    #[test]
    fn test_calc_freq_from_residencies_idle_with_known_states() {
        // Cluster spent the whole window in IDLE, but the residency map
        // still enumerates the (zero-residency) active P-states. The
        // function should report the minimum parseable active frequency
        // as the parked clock with 0% active residency, rather than 0 Hz.
        let residencies = vec![
            ("IDLE".to_string(), 500),
            ("600".to_string(), 0),
            ("1200".to_string(), 0),
        ];

        let (freq, residency) = IOReportMetrics::calc_freq_from_residencies(&residencies);

        assert_eq!(freq, 600);
        assert!((residency - 0.0).abs() < 0.1);
    }

    #[test]
    fn test_calc_gpu_freq_with_table_idle() {
        // GPU is present (residency > 0) but every state is idle/off.
        // With a non-empty IOKit pmgr freq_table, the function should
        // return the lowest entry — the parked P-state — instead of 0.
        let residencies = vec![("OFF".to_string(), 200), ("IDLE".to_string(), 800)];

        let freq_table = [396, 720, 1398];

        let (freq, residency) = IOReportMetrics::calc_freq_with_table(&residencies, &freq_table);

        assert_eq!(freq, 396);
        assert!((residency - 0.0).abs() < 0.1);
    }

    #[test]
    fn test_calc_gpu_freq_with_table_empty_table_idle() {
        // GPU is present but idle, and the IOKit pmgr table is empty so
        // there is no idle-state P-state to fall back on. Expected:
        // (0, 0.0). The renderer will substitute N/A.
        let residencies = vec![("OFF".to_string(), 200), ("IDLE".to_string(), 800)];

        let freq_table: [u32; 0] = [];

        let (freq, residency) = IOReportMetrics::calc_freq_with_table(&residencies, &freq_table);

        assert_eq!(freq, 0);
        assert!((residency - 0.0).abs() < 0.1);
    }

    // ---------------------------------------------------------------------
    // Regression coverage for issue #314: CPU frequency metrics reported 0 on
    // Apple Silicon.
    //
    // Fixtures below are verbatim captures from an Apple M1 Ultra (macOS 26.6,
    // Darwin 25.6.0): the IOReport `CPU Core Performance States` residency
    // histograms and the IOKit `AppleARMIODevice` pmgr `voltage-states*`
    // tables. They pin down the fact that CPU performance states are named
    // symbolically (`V0P14`), never in megahertz, so the clock can only come
    // from a pmgr table join.
    // ---------------------------------------------------------------------

    /// Apple M1 Ultra `voltage-states5-sram`: performance-cluster clocks, MHz.
    const M1_ULTRA_P_TABLE: [u32; 15] = [
        600, 828, 1056, 1296, 1524, 1752, 1980, 2208, 2448, 2676, 2904, 3036, 3132, 3168, 3228,
    ];

    /// Apple M1 Ultra `voltage-states1-sram`: efficiency-cluster clocks, MHz.
    const M1_ULTRA_E_TABLE: [u32; 5] = [600, 972, 1332, 1704, 2064];

    /// Apple M1 Ultra `voltage-states9-sram`: GPU clocks, MHz.
    const M1_ULTRA_GPU_TABLE: [u32; 6] = [388, 486, 648, 777, 972, 1296];

    /// Encode MHz values the way a pmgr `voltage-states*` blob does: 8 bytes
    /// per entry, little-endian hertz in the first 4, voltage in the last 4.
    fn encode_voltage_states(freqs_hz: &[u64]) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(freqs_hz.len() * 8);
        for (i, hz) in freqs_hz.iter().enumerate() {
            bytes.extend_from_slice(&(*hz as u32).to_le_bytes());
            bytes.extend_from_slice(&(700 + i as u32).to_le_bytes());
        }
        bytes
    }

    fn residencies(pairs: &[(&str, i64)]) -> Vec<(String, i64)> {
        pairs.iter().map(|(n, r)| ((*n).to_string(), *r)).collect()
    }

    fn tables(entries: &[(&str, &[u32])]) -> Vec<(String, Vec<u32>)> {
        let mut out: Vec<(String, Vec<u32>)> = entries
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.to_vec()))
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// The M1 Ultra pmgr table set, reduced to the keys that matter here.
    fn m1_ultra_tables() -> Vec<(String, Vec<u32>)> {
        tables(&[
            ("voltage-states1-sram", &M1_ULTRA_E_TABLE),
            ("voltage-states5-sram", &M1_ULTRA_P_TABLE),
            ("voltage-states9-sram", &M1_ULTRA_GPU_TABLE),
            ("voltage-states9", &M1_ULTRA_GPU_TABLE),
            // Same length as the P table, different key. Present on real
            // hardware; the documented key must win over it.
            ("voltage-states13-sram", &M1_ULTRA_P_TABLE),
        ])
    }

    #[test]
    fn test_parse_voltage_states_bytes_reads_frequency_table() {
        let hz: Vec<u64> = M1_ULTRA_P_TABLE
            .iter()
            .map(|m| *m as u64 * 1_000_000)
            .collect();
        let parsed = parse_voltage_states_bytes(&encode_voltage_states(&hz));
        assert_eq!(parsed, M1_ULTRA_P_TABLE.to_vec());
    }

    #[test]
    fn test_parse_voltage_states_bytes_rejects_period_table() {
        // Verbatim `voltage-states5` (no `-sram`) from an M1 Ultra. Despite the
        // key name this holds clock *periods*, not hertz, and must not be
        // mistaken for a frequency table.
        let periods: Vec<u64> = vec![
            109226, 79149, 62060, 50567, 43002, 37406, 33098, 29681, 26771, 24490, 22567, 21586,
            20924, 20686, 20302,
        ];
        let parsed = parse_voltage_states_bytes(&encode_voltage_states(&periods));
        assert!(
            parsed.is_empty(),
            "period table must not parse as frequencies, got {parsed:?}"
        );
    }

    #[test]
    fn test_parse_voltage_states_bytes_undoes_u32_wraparound() {
        // Clocks above 4.295 GHz overflow the 32-bit hertz field. The stored
        // low bits must be lifted back over the wrap so later Apple Silicon
        // P-cores do not read as a few hundred megahertz.
        let hz: Vec<u64> = vec![3_000_000_000, 4_100_000_000, 4_512_000_000, 4_608_000_000];
        let parsed = parse_voltage_states_bytes(&encode_voltage_states(&hz));
        assert_eq!(parsed, vec![3000, 4100, 4512, 4608]);
    }

    #[test]
    fn test_parse_voltage_states_bytes_ignores_trailing_partial_entry() {
        let mut bytes = encode_voltage_states(&[600_000_000, 972_000_000]);
        bytes.extend_from_slice(&[0x11, 0x22]);
        assert_eq!(parse_voltage_states_bytes(&bytes), vec![600, 972]);
    }

    #[test]
    fn test_strip_die_prefix() {
        assert_eq!(strip_die_prefix("DIE_0_ECPU_CPU0"), "ECPU_CPU0");
        assert_eq!(strip_die_prefix("DIE_1_PCPU1_CPU3"), "PCPU1_CPU3");
        assert_eq!(strip_die_prefix("DIE_12_PCPU_CPU0"), "PCPU_CPU0");
        // Nothing to strip, or a shape that is not a die prefix.
        assert_eq!(strip_die_prefix("ECPU0"), "ECPU0");
        assert_eq!(strip_die_prefix("DIE_ECPU"), "DIE_ECPU");
    }

    #[test]
    fn test_classify_cpu_channel() {
        // Multi-die names as reported by an M1 Ultra.
        assert_eq!(
            classify_cpu_channel("DIE_0_ECPU_CPU0"),
            Some(CpuCluster::Efficiency)
        );
        assert_eq!(
            classify_cpu_channel("DIE_1_ECPU_CPU1"),
            Some(CpuCluster::Efficiency)
        );
        assert_eq!(
            classify_cpu_channel("DIE_0_PCPU_CPU0"),
            Some(CpuCluster::Performance)
        );
        assert_eq!(
            classify_cpu_channel("DIE_1_PCPU1_CPU3"),
            Some(CpuCluster::Performance)
        );

        // Single-die names.
        assert_eq!(classify_cpu_channel("ECPU"), Some(CpuCluster::Efficiency));
        assert_eq!(classify_cpu_channel("ECPU0"), Some(CpuCluster::Efficiency));
        assert_eq!(classify_cpu_channel("PCPU"), Some(CpuCluster::Performance));
        assert_eq!(classify_cpu_channel("PCPU1"), Some(CpuCluster::Performance));

        // M5 Pro/Max cluster naming, with and without a die prefix.
        assert_eq!(classify_cpu_channel("MCPU0"), Some(CpuCluster::Super));
        assert_eq!(classify_cpu_channel("MCPU05"), Some(CpuCluster::Super));
        assert_eq!(classify_cpu_channel("DIE_0_MCPU0"), Some(CpuCluster::Super));
        assert_eq!(classify_cpu_channel("MCPU1"), Some(CpuCluster::Performance));
        assert_eq!(
            classify_cpu_channel("DIE_1_MCPU15"),
            Some(CpuCluster::Performance)
        );

        // Unknown channels are dropped rather than folded into a cluster.
        assert_eq!(classify_cpu_channel("GPUPH"), None);
        assert_eq!(classify_cpu_channel(""), None);
    }

    #[test]
    fn test_select_cpu_frequency_table_prefers_documented_key() {
        let tables = m1_ultra_tables();

        let e = select_cpu_frequency_table(&tables, CpuCluster::Efficiency, 5).unwrap();
        assert_eq!(e, M1_ULTRA_E_TABLE);

        // voltage-states13-sram has the same length as the P table, so the
        // documented key has to be consulted first for the choice to be stable.
        let p = select_cpu_frequency_table(&tables, CpuCluster::Performance, 15).unwrap();
        assert_eq!(p, M1_ULTRA_P_TABLE);
    }

    #[test]
    fn test_select_cpu_frequency_table_falls_back_on_length_mismatch() {
        // A chip whose efficiency table lives under an undocumented key. The
        // active-state count is the only usable discriminator.
        let tables = tables(&[
            ("voltage-states2-sram", &M1_ULTRA_E_TABLE),
            ("voltage-states5-sram", &M1_ULTRA_P_TABLE),
        ]);

        let e = select_cpu_frequency_table(&tables, CpuCluster::Efficiency, 5).unwrap();
        assert_eq!(e, M1_ULTRA_E_TABLE);
    }

    #[test]
    fn test_select_cpu_frequency_table_super_cluster_uses_length_match() {
        // No documented pmgr key exists for the M5 Super cluster, so selection
        // rests entirely on the active-state count.
        let super_table: [u32; 4] = [800, 1600, 2800, 4000];
        let tables = tables(&[
            ("voltage-states1-sram", &M1_ULTRA_E_TABLE),
            ("voltage-states7-sram", &super_table),
        ]);

        let s = select_cpu_frequency_table(&tables, CpuCluster::Super, 4).unwrap();
        assert_eq!(s, super_table);

        // Nothing of that length: report no table rather than a wrong one.
        assert!(select_cpu_frequency_table(&tables, CpuCluster::Super, 9).is_none());
    }

    #[test]
    fn test_select_cpu_frequency_table_last_resort_uses_documented_key() {
        // Length disagrees with every table. A partially mapped clock from the
        // documented key still beats reporting 0 MHz.
        let tables = tables(&[("voltage-states5-sram", &M1_ULTRA_P_TABLE)]);
        let p = select_cpu_frequency_table(&tables, CpuCluster::Performance, 99).unwrap();
        assert_eq!(p, M1_ULTRA_P_TABLE);

        assert!(select_cpu_frequency_table(&[], CpuCluster::Performance, 15).is_none());
    }

    #[test]
    fn test_select_gpu_frequency_table() {
        assert_eq!(
            select_gpu_frequency_table(&m1_ultra_tables()),
            M1_ULTRA_GPU_TABLE.to_vec()
        );

        // No documented key: fall back to the table with the lowest maximum,
        // GPU clocks being the lowest the package publishes.
        let odd = tables(&[
            ("voltage-states1-sram", &M1_ULTRA_E_TABLE),
            ("voltage-states5-sram", &M1_ULTRA_P_TABLE),
            ("voltage-states22-sram", &M1_ULTRA_GPU_TABLE),
        ]);
        assert_eq!(
            select_gpu_frequency_table(&odd),
            M1_ULTRA_GPU_TABLE.to_vec()
        );

        assert!(select_gpu_frequency_table(&[]).is_empty());
    }

    #[test]
    fn test_cpu_performance_states_are_not_numeric() {
        // The heart of issue #314. Before the fix, CPU clusters went through
        // calc_freq_from_residencies, which reads the megahertz value out of
        // the state *name*. Apple Silicon names CPU states `V<volt>P<perf>`,
        // so every parse failed and the reported clock collapsed to 0 while
        // residency stayed correct.
        let res = residencies(&[
            ("IDLE", 2076468),
            ("V0P14", 0),
            ("V1P13", 0),
            ("V2P12", 1874),
            ("V3P11", 0),
            ("V4P10", 0),
            ("V5P9", 953),
            ("V6P8", 0),
            ("V7P7", 0),
            ("V8P6", 0),
            ("V9P5", 0),
            ("V10P4", 0),
            ("V11P3", 0),
            ("V12P2", 0),
            ("V13P1", 0),
            ("V14P0", 5363773),
        ]);

        let (freq, residency) = IOReportMetrics::calc_freq_from_residencies(&res);
        assert_eq!(freq, 0, "state names carry no megahertz value");
        assert!((residency - 72.10).abs() < 0.01);
    }

    #[test]
    fn test_p_cluster_frequency_from_real_m1_ultra_sample() {
        let res = residencies(&[
            ("IDLE", 2076468),
            ("V0P14", 0),
            ("V1P13", 0),
            ("V2P12", 1874),
            ("V3P11", 0),
            ("V4P10", 0),
            ("V5P9", 953),
            ("V6P8", 0),
            ("V7P7", 0),
            ("V8P6", 0),
            ("V9P5", 0),
            ("V10P4", 0),
            ("V11P3", 0),
            ("V12P2", 0),
            ("V13P1", 0),
            ("V14P0", 5363773),
        ]);

        let (freq, residency) = IOReportMetrics::calc_freq_with_table(&res, &M1_ULTRA_P_TABLE);

        // Residency-weighted over the three occupied states:
        // (1056*1874 + 1752*953 + 3228*5363773) / 5366600
        assert_eq!(freq, 3226);
        assert!((residency - 72.10).abs() < 0.01);
        assert!(freq <= *M1_ULTRA_P_TABLE.last().unwrap());
    }

    #[test]
    fn test_e_cluster_frequency_from_real_m1_ultra_sample() {
        let res = residencies(&[
            ("IDLE", 1999280),
            ("V0P4", 0),
            ("V1P3", 511855),
            ("V2P2", 576745),
            ("V3P1", 568251),
            ("V4P0", 3786908),
        ]);

        let (freq, residency) = IOReportMetrics::calc_freq_with_table(&res, &M1_ULTRA_E_TABLE);

        assert_eq!(freq, 1846);
        assert!((residency - 73.14).abs() < 0.01);
        assert!(freq >= M1_ULTRA_E_TABLE[0] && freq <= *M1_ULTRA_E_TABLE.last().unwrap());
    }

    #[test]
    fn test_idle_cpu_cluster_reports_parked_clock_not_zero() {
        // Every M1 Ultra core parks in IDLE when the machine is quiet. The
        // cluster is not stopped, it sits at its lowest P-state, so the
        // reported clock must be the bottom table entry rather than 0.
        let mut pairs = vec![("IDLE", 7478905)];
        for name in [
            "V0P14", "V1P13", "V2P12", "V3P11", "V4P10", "V5P9", "V6P8", "V7P7", "V8P6", "V9P5",
            "V10P4", "V11P3", "V12P2", "V13P1", "V14P0",
        ] {
            pairs.push((name, 0));
        }

        let (freq, residency) =
            IOReportMetrics::calc_freq_with_table(&residencies(&pairs), &M1_ULTRA_P_TABLE);

        assert_eq!(freq, 600);
        assert!((residency - 0.0).abs() < 0.01);
    }

    #[test]
    fn test_is_idle_state() {
        assert!(is_idle_state("IDLE"));
        assert!(is_idle_state("OFF"));
        assert!(is_idle_state("DOWN"));
        assert!(!is_idle_state("V0P14"));
        assert!(!is_idle_state("P1"));
    }
}
