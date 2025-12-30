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
//! ## References
//! - macmon project by vladkens
//! - asitop project by tlkh
//! - OSXPrivateSDK IOReport.h

use core_foundation::base::{CFRelease, CFRetain, CFType, CFTypeRef, TCFType};
use core_foundation::data::CFData;
use core_foundation::dictionary::{CFDictionary, CFDictionaryRef, CFMutableDictionaryRef};
use core_foundation::string::{CFString, CFStringRef};
use std::ffi::c_void;
use std::marker::{PhantomData, PhantomPinned};
use std::ptr;
use std::sync::OnceLock;
use std::time::Instant;

/// Static CFStringRef constants for IOReport channel groups.
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

            Self {
                energy_model,
                cpu_stats,
                cpu_perf_states,
                gpu_stats,
                gpu_perf_states,
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

/// GPU frequency table loaded from IOKit pmgr device
///
/// This is loaded once at startup and cached for use when calculating
/// weighted average GPU frequency from GPUPH residencies.
static GPU_FREQUENCIES: OnceLock<Vec<u32>> = OnceLock::new();

/// Load GPU frequencies from IOKit pmgr/clpc device
///
/// Based on mactop's loadGpuFrequencies implementation:
/// - Matches AppleARMIODevice service
/// - Looks for pmgr or clpc device
/// - Reads voltage-states9-sram or voltage-states9 property
/// - Parses frequency values (first 4 bytes of each 8-byte entry in Hz)
fn load_gpu_frequencies() -> Vec<u32> {
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

        let mut frequencies: Vec<u32> = vec![];
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
                    frequencies = extract_gpu_frequencies_from_properties(properties);
                    CFRelease(properties as *const c_void);
                }
            }

            IOObjectRelease(entry);
            if !frequencies.is_empty() {
                break; // Found frequencies, stop searching
            }
            entry = IOIteratorNext(iterator);
        }

        IOObjectRelease(iterator);
        frequencies
    }
}

/// Extract GPU frequencies from pmgr device properties
fn extract_gpu_frequencies_from_properties(properties: CFMutableDictionaryRef) -> Vec<u32> {
    unsafe {
        let cf_dict = CFDictionary::<CFType, CFType>::wrap_under_get_rule(properties);

        // First try voltage-states9-sram or voltage-states9 (preferred keys)
        let preferred_keys = ["voltage-states9-sram", "voltage-states9"];
        for key_name in preferred_keys {
            let key = CFString::new(key_name);
            if let Some(value) = cf_dict.find(key.as_CFType().as_CFTypeRef()) {
                let data_ref = value.as_CFTypeRef() as core_foundation::data::CFDataRef;
                if !data_ref.is_null() {
                    let frequencies = parse_voltage_states_data(data_ref);
                    if !frequencies.is_empty() {
                        return frequencies;
                    }
                }
            }
        }

        // Fallback: search for any voltage-states* key with reasonable frequency range
        // Look for the one with the lowest max frequency (GPU frequencies are lower than CPU)
        let mut best_frequencies: Vec<u32> = vec![];
        let mut best_max_freq: u32 = u32::MAX;

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

            // Find max frequency in this set
            let max_freq = frequencies.iter().max().copied().unwrap_or(0);

            // GPU frequencies are typically lower than CPU frequencies
            // Select the set with the lowest maximum as GPU frequencies
            if max_freq > 0 && max_freq < best_max_freq {
                best_max_freq = max_freq;
                best_frequencies = frequencies;
            }
        }

        best_frequencies
    }
}

/// Minimum valid GPU frequency in Hz (100 MHz)
const MIN_GPU_FREQ_HZ: u32 = 100_000_000;
/// Maximum valid GPU frequency in Hz (4 GHz - fits in u32)
/// Apple Silicon GPUs typically max out around 1.4 GHz, so 4 GHz is generous
const MAX_GPU_FREQ_HZ: u32 = 4_000_000_000;
/// Maximum number of frequency entries to parse
const MAX_FREQ_ENTRIES: usize = 64;

/// Parse voltage-states data to extract frequencies in MHz
fn parse_voltage_states_data(data_ref: core_foundation::data::CFDataRef) -> Vec<u32> {
    unsafe {
        let data = CFData::wrap_under_get_rule(data_ref);
        let bytes = data.bytes();
        let len = bytes.len();

        // Each entry is 8 bytes: first 4 bytes are frequency in Hz
        let total_entries = len / 8;
        let mut frequencies: Vec<u32> = Vec::with_capacity(total_entries.min(MAX_FREQ_ENTRIES));

        for i in 0..total_entries.min(MAX_FREQ_ENTRIES) {
            let offset = i * 8;
            if offset + 4 > len {
                break;
            }

            // Read 4-byte little-endian frequency value in Hz
            let freq_hz = u32::from_le_bytes([
                bytes[offset],
                bytes[offset + 1],
                bytes[offset + 2],
                bytes[offset + 3],
            ]);

            // Validate frequency is in reasonable range (100 MHz - 4 GHz)
            // This filters out invalid/corrupted data
            if (MIN_GPU_FREQ_HZ..=MAX_GPU_FREQ_HZ).contains(&freq_hz) {
                // Convert to MHz
                let freq_mhz = freq_hz / 1_000_000;
                frequencies.push(freq_mhz);
            }
        }

        frequencies
    }
}

/// Get cached GPU frequencies, loading them if necessary
pub fn get_gpu_frequencies() -> &'static [u32] {
    GPU_FREQUENCIES.get_or_init(load_gpu_frequencies)
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

    unsafe {
        let cf_dict = CFDictionary::<CFType, CFType>::wrap_under_get_rule(dict);
        let key = CFString::new("IOReportChannels");

        if let Some(channels) = cf_dict.find(key.as_CFType().as_CFTypeRef()) {
            // The channels value is a CFArray - get its raw pointer
            let arr_ref = channels.as_CFTypeRef() as core_foundation::array::CFArrayRef;
            if arr_ref.is_null() {
                return vec![];
            }

            let arr = core_foundation::array::CFArray::<CFType>::wrap_under_get_rule(arr_ref);
            let count = arr.len();

            (0..count)
                .filter_map(|i| arr.get(i).map(|v| v.as_CFTypeRef() as CFDictionaryRef))
                .filter(|d| !d.is_null())
                .collect()
        } else {
            vec![]
        }
    }
}

/// Item from IOReport iteration
#[derive(Debug, Clone)]
pub struct IOReportChannelItem {
    pub group: String,
    pub subgroup: String,
    pub channel: String,
    pub unit: String,
    pub item: CFDictionaryRef,
}

impl IOReportChannelItem {
    /// Get simple integer value from this channel
    pub fn get_integer_value(&self) -> i64 {
        if self.item.is_null() {
            return 0;
        }
        unsafe { IOReportSimpleGetIntegerValue(self.item, 0) }
    }

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

    /// Calculate power consumption in watts from energy value
    pub fn calculate_watts(&self, duration_ns: u64) -> f64 {
        let value = self.get_integer_value();
        if value <= 0 || duration_ns == 0 {
            return 0.0;
        }

        // Determine conversion factor based on unit
        let unit_factor = match self.unit.as_str() {
            "mJ" => 1e-3, // millijoules to joules
            "uJ" => 1e-6, // microjoules to joules
            "nJ" => 1e-9, // nanojoules to joules
            _ => 1e-9,    // Default to nanojoules
        };

        // Convert energy to watts: W = J / s
        let energy_joules = value as f64 * unit_factor;
        let duration_secs = duration_ns as f64 / 1e9;
        energy_joules / duration_secs
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
            let unit = cfstr_to_string(IOReportChannelGetUnitLabel(item)).unwrap_or_default();

            Some(IOReportChannelItem {
                group,
                subgroup,
                channel,
                unit,
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

/// IOReport subscription manager
pub struct IOReport {
    subscription: IOReportSubscriptionRef,
    channels: CFMutableDictionaryRef,
    prev_sample: Option<(CFDictionaryRef, Instant)>,
}

impl IOReport {
    /// Create a new IOReport subscription for the specified channel groups
    pub fn new() -> Result<Self, &'static str> {
        unsafe {
            // Get static CFString refs to avoid use-after-free
            let refs = get_cfstring_refs();

            // Get channels for each group using static CFStrings
            let energy_channels =
                IOReportCopyChannelsInGroup(refs.energy_model, ptr::null(), 0, 0, 0);
            let cpu_channels =
                IOReportCopyChannelsInGroup(refs.cpu_stats, refs.cpu_perf_states, 0, 0, 0);
            let gpu_channels =
                IOReportCopyChannelsInGroup(refs.gpu_stats, refs.gpu_perf_states, 0, 0, 0);

            if energy_channels.is_null() {
                return Err("Failed to get Energy Model channels");
            }

            // Merge all channels into one dictionary
            if !cpu_channels.is_null() {
                IOReportMergeChannels(energy_channels, cpu_channels, ptr::null());
                CFRelease(cpu_channels as *const c_void);
            }
            if !gpu_channels.is_null() {
                IOReportMergeChannels(energy_channels, gpu_channels, ptr::null());
                CFRelease(gpu_channels as *const c_void);
            }

            // Create mutable copy for subscription
            let count = core_foundation::dictionary::CFDictionaryGetCount(energy_channels) as isize;
            let channels = core_foundation::dictionary::CFDictionaryCreateMutableCopy(
                core_foundation::base::kCFAllocatorDefault,
                count,
                energy_channels,
            );
            CFRelease(energy_channels as *const c_void);

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
            })
        }
    }

    /// Get a delta sample over the specified duration
    pub fn get_sample(
        &mut self,
        duration_ms: u64,
    ) -> Result<(IOReportIterator, u64), &'static str> {
        let sample1 = self.take_sample()?;

        let start = Instant::now();
        std::thread::sleep(std::time::Duration::from_millis(duration_ms));

        let sample2 = self.take_sample()?;
        let duration_ns = start.elapsed().as_nanos() as u64;

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

        Ok((IOReportIterator::new(delta), duration_ns))
    }

    /// Take a single sample
    fn take_sample(&self) -> Result<CFDictionaryRef, &'static str> {
        unsafe {
            let sample = IOReportCreateSamples(self.subscription, self.channels, ptr::null());
            if sample.is_null() {
                return Err("Failed to create IOReport sample");
            }
            Ok(sample)
        }
    }
}

impl Drop for IOReport {
    fn drop(&mut self) {
        unsafe {
            if let Some((prev, _)) = self.prev_sample.take() {
                if !prev.is_null() {
                    CFRelease(prev as *const c_void);
                }
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
    pub e_cluster_freq: u32,
    pub p_cluster_freq: u32,
    pub e_cluster_residency: f64,
    pub p_cluster_residency: f64,

    // GPU metrics
    pub gpu_freq: u32,
    pub gpu_residency: f64,

    // Raw per-cluster data for Ultra chips
    pub e_cluster_data: Vec<(u32, f64)>, // (freq_mhz, residency_percent)
    pub p_cluster_data: Vec<(u32, f64)>,
}

impl IOReportMetrics {
    /// Collect metrics from an IOReport sample
    pub fn from_sample(iterator: IOReportIterator, duration_ns: u64) -> Self {
        let mut metrics = Self::default();

        let mut e_cluster_freqs: Vec<(u32, f64)> = vec![];
        let mut p_cluster_freqs: Vec<(u32, f64)> = vec![];
        let mut gpu_freqs: Vec<(u32, f64)> = vec![];

        for item in iterator {
            match (item.group.as_str(), item.subgroup.as_str()) {
                ("Energy Model", _) => {
                    Self::process_energy_channel(&item, duration_ns, &mut metrics);
                }
                ("CPU Stats", "CPU Core Performance States") => {
                    Self::process_cpu_channel(&item, &mut e_cluster_freqs, &mut p_cluster_freqs);
                }
                ("GPU Stats", "GPU Performance States") => {
                    if item.channel == "GPUPH" {
                        Self::process_gpu_channel(&item, &mut gpu_freqs);
                    }
                }
                _ => {}
            }
        }

        // Calculate averages for clusters
        metrics.e_cluster_data = e_cluster_freqs.clone();
        metrics.p_cluster_data = p_cluster_freqs.clone();

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

    fn process_energy_channel(item: &IOReportChannelItem, duration_ns: u64, metrics: &mut Self) {
        let watts = item.calculate_watts(duration_ns);
        let channel = item.channel.as_str();

        // Match known energy channels
        if channel.contains("CPU") && !channel.contains("GPU") {
            metrics.cpu_power += watts;
        } else if channel.contains("GPU") && !channel.contains("CPU") {
            metrics.gpu_power += watts;
        } else if channel.contains("ANE") {
            metrics.ane_power += watts;
        } else if channel.contains("DRAM") {
            metrics.dram_power += watts;
        }

        // Track package power
        if channel == "CPU Energy" || channel.starts_with("CPU") {
            // Package includes CPU, GPU, ANE
            metrics.package_power = metrics.cpu_power + metrics.gpu_power + metrics.ane_power;
        }
    }

    fn process_cpu_channel(
        item: &IOReportChannelItem,
        e_cluster_freqs: &mut Vec<(u32, f64)>,
        p_cluster_freqs: &mut Vec<(u32, f64)>,
    ) {
        let residencies = item.get_residencies();
        if residencies.is_empty() {
            return;
        }

        let (freq, residency) = Self::calc_freq_from_residencies(&residencies);
        let channel = &item.channel;

        // Determine cluster type from channel name
        if channel.starts_with("E") || channel.contains("ECPU") {
            e_cluster_freqs.push((freq, residency));
        } else if channel.starts_with("P") || channel.contains("PCPU") {
            p_cluster_freqs.push((freq, residency));
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
            Self::calc_gpu_freq_with_table(&residencies, gpu_freq_table)
        } else {
            Self::calc_freq_from_residencies(&residencies)
        };
        gpu_freqs.push((freq, residency));
    }

    /// Calculate GPU frequency using pre-loaded frequency table from IOKit
    ///
    /// Based on mactop's approach:
    /// - Active states (non-OFF/IDLE/DOWN) are mapped to frequencies in order
    /// - The frequency table from pmgr device provides accurate MHz values
    fn calc_gpu_freq_with_table(residencies: &[(String, i64)], freq_table: &[u32]) -> (u32, f64) {
        let mut total_residency: i64 = 0;
        let mut active_residency: i64 = 0;
        let mut weighted_freq: f64 = 0.0;
        let mut active_state_idx: usize = 0;

        for (name, residency) in residencies {
            total_residency += residency;

            // Skip idle/off states
            if name.contains("IDLE") || name.contains("OFF") || name.contains("DOWN") {
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
        } else {
            0
        };

        let residency_pct = (active_residency as f64 / total_residency as f64) * 100.0;

        (avg_freq, residency_pct)
    }

    /// Calculate frequency and residency from state residencies
    fn calc_freq_from_residencies(residencies: &[(String, i64)]) -> (u32, f64) {
        let mut total_residency: i64 = 0;
        let mut weighted_freq: i64 = 0;
        let mut active_residency: i64 = 0;

        for (name, residency) in residencies {
            total_residency += residency;

            // Skip idle/off states
            if name.contains("IDLE") || name.contains("OFF") || name.contains("DOWN") {
                continue;
            }

            active_residency += residency;

            // Parse frequency from state name (e.g., "2064" for 2064 MHz)
            if let Ok(freq) = name.trim().parse::<i64>() {
                weighted_freq += freq * residency;
            }
        }

        if total_residency == 0 {
            return (0, 0.0);
        }

        let avg_freq = if active_residency > 0 {
            (weighted_freq / active_residency) as u32
        } else {
            0
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

        let (freq, residency) =
            IOReportMetrics::calc_gpu_freq_with_table(&residencies, &freq_table);

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

        let (freq, residency) =
            IOReportMetrics::calc_gpu_freq_with_table(&residencies, &freq_table);

        // Active residency: 200 out of 300 total = 66.67%
        assert!((residency - 66.67).abs() < 0.1);

        // No frequency data available
        assert_eq!(freq, 0);
    }
}
