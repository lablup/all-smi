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

//! Performance Data Helper (PDH) sampling of the Windows GPU counter
//! families.
//!
//! Three counter families are read:
//!
//! | Counter | Shape | Gives us |
//! |---|---|---|
//! | `\GPU Engine(*)\Utilization Percentage` | rate | device utilization |
//! | `\GPU Adapter Memory(*)\Dedicated Usage` | gauge | system-wide used VRAM |
//! | `\GPU Process Memory(*)\Dedicated Usage` | gauge | per-process VRAM |
//!
//! This is the same data Task Manager's GPU pane shows, and it is
//! vendor-neutral: AMD, Intel, and NVIDIA adapters all publish it
//! through WDDM.
//!
//! ## Why the query is persistent
//!
//! `Utilization Percentage` is a rate counter. A single
//! `PdhCollectQueryData` call establishes a baseline and yields no
//! usable value; the rate only exists once there are two samples to
//! difference. Rather than sleeping inside the reader to manufacture a
//! second sample, the query is opened once and kept alive in a process
//! global, and each all-smi poll contributes one collection. The first
//! poll after start-up reports no utilization, and every poll after that
//! reports the rate over the real poll interval. That matches how
//! all-smi already drives its other rate-like sources and keeps
//! `get_gpu_info` non-blocking.
//!
//! The two memory counters are gauges and are valid from the very first
//! collection, so they are returned immediately.
//!
//! ## Locale
//!
//! Counters are added with `PdhAddEnglishCounterW`, not
//! `PdhAddCounterW`. Counter path components are localized on non-English
//! Windows installations, so the literal English path only resolves
//! through the English-specific entry point.

use super::ids::{
    AdapterLuid, GpuAdapterMemoryInstance, GpuEngineInstance, GpuProcessMemoryInstance,
    aggregate_adapter_memory, aggregate_engine_utilization, parse_gpu_adapter_memory_instance,
    parse_gpu_engine_instance, parse_gpu_process_memory_instance,
};
use once_cell::sync::OnceCell;
use std::collections::HashMap;
use std::sync::Mutex;
use windows::Win32::System::Performance::{
    PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE, PDH_HCOUNTER, PDH_HQUERY, PdhAddEnglishCounterW,
    PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterArrayW, PdhOpenQueryW,
};
use windows::core::{PCWSTR, w};

/// `PDH_CSTATUS_VALID_DATA`, the per-item success status.
const PDH_CSTATUS_VALID_DATA: u32 = 0;
/// `PDH_MORE_DATA`, returned by the buffer-sizing probe call.
const PDH_MORE_DATA: u32 = 0x8000_07D2;

/// What one poll of the GPU counters produced.
#[derive(Debug, Default)]
pub struct PdhSample {
    /// Device utilization per adapter, 0..=100. Empty on the first poll
    /// after start-up, and on hosts that publish no GPU Engine
    /// instances.
    pub utilization: HashMap<AdapterLuid, f64>,
    /// System-wide dedicated VRAM in use, per adapter, in bytes.
    pub adapter_memory: HashMap<AdapterLuid, u64>,
    /// Dedicated VRAM in use per (pid, adapter), in bytes.
    pub process_memory: Vec<(GpuProcessMemoryInstance, u64)>,
}

struct GpuCounterQuery {
    query: PDH_HQUERY,
    engine: PDH_HCOUNTER,
    adapter_memory: PDH_HCOUNTER,
    process_memory: PDH_HCOUNTER,
    /// Set once a collection has happened, which is when the rate
    /// counter starts producing values.
    primed: bool,
}

// PDH handles are opaque values, not thread-affine resources; the
// wrapping `Mutex` is what serialises access to the query.
unsafe impl Send for GpuCounterQuery {}

impl Drop for GpuCounterQuery {
    fn drop(&mut self) {
        // Unreachable while the sampler lives in a process global, but
        // correct if the storage is ever made per-reader.
        unsafe {
            let _ = PdhCloseQuery(self.query);
        }
    }
}

impl GpuCounterQuery {
    fn open() -> Option<Self> {
        let mut query = PDH_HQUERY::default();
        // A null data source means "live data from this machine".
        if unsafe { PdhOpenQueryW(PCWSTR::null(), 0, &mut query) } != 0 {
            return None;
        }

        let engine = match add_counter(query, w!("\\GPU Engine(*)\\Utilization Percentage")) {
            Some(counter) => counter,
            None => {
                unsafe {
                    let _ = PdhCloseQuery(query);
                }
                return None;
            }
        };
        // The two memory families are optional: a host can publish GPU
        // Engine without them, and losing them should cost only those
        // fields.
        let adapter_memory =
            add_counter(query, w!("\\GPU Adapter Memory(*)\\Dedicated Usage")).unwrap_or_default();
        let process_memory =
            add_counter(query, w!("\\GPU Process Memory(*)\\Dedicated Usage")).unwrap_or_default();

        Some(Self {
            query,
            engine,
            adapter_memory,
            process_memory,
            primed: false,
        })
    }

    fn collect(&mut self) -> PdhSample {
        if unsafe { PdhCollectQueryData(self.query) } != 0 {
            return PdhSample::default();
        }
        let was_primed = self.primed;
        self.primed = true;

        let mut sample = PdhSample::default();

        // Rate counter: skip entirely until a previous collection has
        // established the baseline.
        if was_primed {
            let engine_samples = read_counter_array(self.engine)
                .into_iter()
                .filter_map(|(name, value)| {
                    parse_gpu_engine_instance(&name).map(|instance| (instance, value))
                })
                .collect::<Vec<(GpuEngineInstance, f64)>>();
            sample.utilization = aggregate_engine_utilization(engine_samples);
        }

        if !self.adapter_memory.is_invalid() {
            let adapter_samples = read_counter_array(self.adapter_memory)
                .into_iter()
                .filter_map(|(name, value)| {
                    parse_gpu_adapter_memory_instance(&name).map(|instance| (instance, value))
                })
                .collect::<Vec<(GpuAdapterMemoryInstance, f64)>>();
            sample.adapter_memory = aggregate_adapter_memory(adapter_samples);
        }

        if !self.process_memory.is_invalid() {
            sample.process_memory = read_counter_array(self.process_memory)
                .into_iter()
                .filter_map(|(name, value)| {
                    if !value.is_finite() || value < 0.0 {
                        return None;
                    }
                    parse_gpu_process_memory_instance(&name)
                        .map(|instance| (instance, value as u64))
                })
                .collect();
        }

        sample
    }
}

fn add_counter(query: PDH_HQUERY, path: PCWSTR) -> Option<PDH_HCOUNTER> {
    let mut counter = PDH_HCOUNTER::default();
    if unsafe { PdhAddEnglishCounterW(query, path, 0, &mut counter) } != 0 {
        return None;
    }
    Some(counter)
}

/// Read every instance of a wildcard counter as `(instance name, value)`.
///
/// PDH wants the caller to size the buffer: the first call with a zero
/// size returns `PDH_MORE_DATA` and fills in the byte count, the second
/// call fills the buffer. The buffer holds the item array followed by
/// the instance-name strings the items point into, so it is allocated as
/// a `Vec` of items large enough to cover the whole byte count.
fn read_counter_array(counter: PDH_HCOUNTER) -> Vec<(String, f64)> {
    if counter.is_invalid() {
        return Vec::new();
    }

    let mut buffer_size = 0u32;
    let mut item_count = 0u32;
    let status = unsafe {
        PdhGetFormattedCounterArrayW(
            counter,
            PDH_FMT_DOUBLE,
            &mut buffer_size,
            &mut item_count,
            None,
        )
    };
    if status != PDH_MORE_DATA || buffer_size == 0 {
        // No instances at all is the normal case on a machine with no
        // WDDM GPU, and on GitHub-hosted Windows runners.
        return Vec::new();
    }

    let item_size = std::mem::size_of::<PDH_FMT_COUNTERVALUE_ITEM_W>();
    let capacity = (buffer_size as usize).div_ceil(item_size);
    let mut items: Vec<PDH_FMT_COUNTERVALUE_ITEM_W> = Vec::with_capacity(capacity);

    let status = unsafe {
        PdhGetFormattedCounterArrayW(
            counter,
            PDH_FMT_DOUBLE,
            &mut buffer_size,
            &mut item_count,
            Some(items.as_mut_ptr()),
        )
    };
    if status != 0 {
        return Vec::new();
    }

    // SAFETY: PDH reported `item_count` initialised items, and the
    // allocation covers `buffer_size` bytes which by PDH's own contract
    // is at least `item_count * item_size`.
    let item_count = (item_count as usize).min(capacity);
    unsafe { items.set_len(item_count) };

    items
        .iter()
        .filter_map(|item| {
            if item.FmtValue.CStatus != PDH_CSTATUS_VALID_DATA {
                return None;
            }
            // SAFETY: the array was requested with PDH_FMT_DOUBLE, so
            // the union holds the double variant.
            let value = unsafe { item.FmtValue.Anonymous.doubleValue };
            // SAFETY: szName points into the tail of the same buffer,
            // which is alive for the duration of this iteration.
            let name = unsafe { item.szName.to_string() }.ok()?;
            Some((name, value))
        })
        .collect()
}

/// Process-wide sampler. `None` once opening the query has failed, so a
/// host without GPU counters pays the failed-open cost exactly once.
static SAMPLER: OnceCell<Mutex<Option<GpuCounterQuery>>> = OnceCell::new();

/// Collect one sample of the GPU counter families.
///
/// Returns an all-empty sample when PDH is unavailable, when the host
/// publishes no GPU counter instances, or on the very first call (for
/// the utilization field only). Callers treat an empty field as "no
/// data" and keep whatever the WMI baseline provided.
pub fn sample() -> PdhSample {
    let cell = SAMPLER.get_or_init(|| Mutex::new(GpuCounterQuery::open()));
    let mut guard = match cell.lock() {
        Ok(guard) => guard,
        // A panic in another thread while holding the query lock leaves
        // the handle in an unknown state; recovering the guard is still
        // better than poisoning every later poll.
        Err(poisoned) => poisoned.into_inner(),
    };
    match guard.as_mut() {
        Some(query) => query.collect(),
        None => PdhSample::default(),
    }
}

/// Whether the GPU counter query could be opened at all. Used by the
/// doctor check to distinguish "PDH is broken" from "this machine has no
/// GPU counter instances".
pub fn query_available() -> bool {
    let cell = SAMPLER.get_or_init(|| Mutex::new(GpuCounterQuery::open()));
    match cell.lock() {
        Ok(guard) => guard.is_some(),
        Err(poisoned) => poisoned.into_inner().is_some(),
    }
}
