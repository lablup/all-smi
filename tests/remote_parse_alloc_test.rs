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

//! Issue #500: parsing a GPU line whose labels hold no escapes and whose
//! device is already known allocates nothing.
//!
//! A counting global allocator measures one parse of a body with a single
//! device, then the same body with 2000 more value lines for that device.
//! The second parse may not allocate more than a small constant on top of
//! the first. This file holds a single test so no other test thread
//! allocates while it counts.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use all_smi::network::metrics_parser::MetricsParser;
use regex::Regex;

struct CountingAllocator;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

// SAFETY: every call is forwarded unchanged to the system allocator; the
// wrapper only counts allocation calls.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: same contract as the caller's.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: same contract as the caller's.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: same contract as the caller's.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: CountingAllocator = CountingAllocator;

const BASE: &str = r#"all_smi_gpu_info{gpu="Test GPU", instance="node-a", gpu_uuid="GPU-A", gpu_index="0", type="GPU", driver_version="580.82.07"} 1
all_smi_gpu_memory_total_bytes{gpu="Test GPU", instance="node-a", gpu_uuid="GPU-A", gpu_index="0"} 17179869184
"#;

const VALUE_LINE: &str = r#"all_smi_gpu_utilization{gpu="Test GPU", instance="node-a", gpu_uuid="GPU-A", gpu_index="0"} 42.5
"#;

fn allocations_for(parser: &MetricsParser, text: &str, re: &Regex) -> usize {
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    let parsed = parser.parse_metrics(text, "node-a:9090", re);
    let after = ALLOCATIONS.load(Ordering::Relaxed);
    assert_eq!(parsed.gpu_info.len(), 1);
    assert_eq!(parsed.gpu_info[0].utilization, 42.5);
    drop(parsed);
    after - before
}

#[test]
fn known_device_value_lines_allocate_nothing() {
    let re = Regex::new(r"^all_smi_([^\{]+)\{([^}]+)\} ([\d\.]+)$").unwrap();
    let parser = MetricsParser::new();
    let small = format!("{BASE}{VALUE_LINE}");
    let large = format!("{BASE}{}", VALUE_LINE.repeat(2001));

    // Warm up lazily initialised state (tracing callsites, time zone data).
    allocations_for(&parser, &small, &re);

    let small_allocs = allocations_for(&parser, &small, &re);
    let large_allocs = allocations_for(&parser, &large, &re);
    assert!(
        large_allocs <= small_allocs + 4,
        "2000 extra value lines for a known device allocated {} times ({small_allocs} -> {large_allocs})",
        large_allocs - small_allocs
    );
}
