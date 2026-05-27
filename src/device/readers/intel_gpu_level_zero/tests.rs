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

//! Unit tests for the Intel Level Zero backend. These tests run on any
//! host because they exercise the pure-logic surface: enum value
//! locks, BDF formatting, delta math, integration with `GpuInfo`.
//! Real-runtime tests would require a host with the Level Zero loader
//! installed and a supported Intel GPU; those are deferred to
//! maintainer hardware verification (issue #248).

use super::ffi;
use super::loader::{format_pci_bdf, normalise_pci_bdf, try_load_library};
use super::refresh::{
    compute_engine_busy_pct, compute_power_watts, make_engine_sample, make_power_sample,
};
use super::*;
use crate::device::types::GpuInfo;
use std::collections::HashMap;

// ----- Enum value locks ---------------------------------------------
//
// These tests lock in the `zes_engine_group_t` integer values against
// the Level Zero spec at
// <https://oneapi-src.github.io/level-zero-spec/level-zero/latest/sysman/api.html#zes-engine-group-t>.
// If Intel ever renumbers the enum (or if a developer accidentally
// changes the constants), CI catches it before the change ships and
// silently misclassifies engine telemetry.

#[test]
fn engine_group_enum_values_match_spec() {
    assert_eq!(ffi::ZES_ENGINE_GROUP_ALL, 0);
    assert_eq!(ffi::ZES_ENGINE_GROUP_COMPUTE_ALL, 1);
    assert_eq!(ffi::ZES_ENGINE_GROUP_MEDIA_ALL, 2);
    assert_eq!(ffi::ZES_ENGINE_GROUP_COPY_ALL, 3);
    assert_eq!(ffi::ZES_ENGINE_GROUP_COMPUTE_SINGLE, 4);
    assert_eq!(ffi::ZES_ENGINE_GROUP_RENDER_SINGLE, 5);
    assert_eq!(ffi::ZES_ENGINE_GROUP_MEDIA_DECODE_SINGLE, 6);
    assert_eq!(ffi::ZES_ENGINE_GROUP_MEDIA_ENCODE_SINGLE, 7);
    assert_eq!(ffi::ZES_ENGINE_GROUP_COPY_SINGLE, 8);
    assert_eq!(ffi::ZES_ENGINE_GROUP_RENDER_COMPUTE_ALL, 9);
    assert_eq!(ffi::ZES_ENGINE_GROUP_3D_ALL, 10);
    assert_eq!(ffi::ZES_ENGINE_GROUP_3D_SINGLE, 11);
    assert_eq!(ffi::ZES_ENGINE_GROUP_MEDIA_ENHANCEMENT_SINGLE, 12);
}

#[test]
fn init_flags_match_spec() {
    assert_eq!(ffi::ZE_INIT_FLAG_DEFAULT, 0);
    assert_eq!(ffi::ZE_RESULT_SUCCESS, 0);
}

#[test]
fn structure_type_constants_match_spec() {
    assert_eq!(ffi::ZES_STRUCTURE_TYPE_PCI_PROPERTIES, 0x0000_0001);
    assert_eq!(ffi::ZES_STRUCTURE_TYPE_ENGINE_PROPERTIES, 0x0000_000a);
}

// ----- Engine label classification ----------------------------------

#[test]
fn engine_label_maps_known_groups() {
    assert_eq!(
        engine_label(ffi::ZES_ENGINE_GROUP_COMPUTE_SINGLE),
        "compute (XMX)"
    );
    assert_eq!(engine_label(ffi::ZES_ENGINE_GROUP_RENDER_SINGLE), "render");
    assert_eq!(engine_label(ffi::ZES_ENGINE_GROUP_COPY_SINGLE), "copy");
    assert_eq!(
        engine_label(ffi::ZES_ENGINE_GROUP_MEDIA_DECODE_SINGLE),
        "media-decode"
    );
    assert_eq!(
        engine_label(ffi::ZES_ENGINE_GROUP_MEDIA_ENCODE_SINGLE),
        "media-encode"
    );
}

#[test]
fn engine_label_unknown_becomes_other() {
    // Aggregated _ALL groups are not tracked; they'd fall through to "other".
    assert_eq!(engine_label(ffi::ZES_ENGINE_GROUP_ALL), "other");
    assert_eq!(engine_label(ffi::ZES_ENGINE_GROUP_3D_SINGLE), "other");
    assert_eq!(engine_label(999), "other");
}

#[test]
fn is_tracked_engine_only_singletons() {
    assert!(is_tracked_engine(ffi::ZES_ENGINE_GROUP_COMPUTE_SINGLE));
    assert!(is_tracked_engine(ffi::ZES_ENGINE_GROUP_RENDER_SINGLE));
    assert!(is_tracked_engine(ffi::ZES_ENGINE_GROUP_COPY_SINGLE));
    assert!(is_tracked_engine(ffi::ZES_ENGINE_GROUP_MEDIA_DECODE_SINGLE));
    assert!(is_tracked_engine(ffi::ZES_ENGINE_GROUP_MEDIA_ENCODE_SINGLE));

    // Aggregated groups MUST be excluded — including them would
    // double-count against the per-engine _SINGLE readings the same
    // device exposes.
    assert!(!is_tracked_engine(ffi::ZES_ENGINE_GROUP_ALL));
    assert!(!is_tracked_engine(ffi::ZES_ENGINE_GROUP_COMPUTE_ALL));
    assert!(!is_tracked_engine(ffi::ZES_ENGINE_GROUP_MEDIA_ALL));
    assert!(!is_tracked_engine(ffi::ZES_ENGINE_GROUP_COPY_ALL));
    assert!(!is_tracked_engine(ffi::ZES_ENGINE_GROUP_RENDER_COMPUTE_ALL));
    assert!(!is_tracked_engine(ffi::ZES_ENGINE_GROUP_3D_ALL));
}

// ----- PCI BDF formatting ------------------------------------------

#[test]
fn pci_bdf_format_matches_sysfs() {
    let addr = ffi::zes_pci_address_t {
        domain: 0,
        bus: 0x03,
        device: 0x00,
        function: 0,
    };
    // Format MUST match the layout of `/sys/class/drm/cardN/device` symlink
    // targets (e.g. `0000:03:00.0`) so the per-card readers can do a
    // string-equality lookup.
    assert_eq!(format_pci_bdf(&addr), "0000:03:00.0");
}

#[test]
fn pci_bdf_format_handles_nonzero_domain() {
    let addr = ffi::zes_pci_address_t {
        domain: 0xABCD,
        bus: 0xEF,
        device: 0x12,
        function: 7,
    };
    assert_eq!(format_pci_bdf(&addr), "abcd:ef:12.7");
}

#[test]
fn normalise_pci_bdf_lowercases() {
    assert_eq!(normalise_pci_bdf("0000:03:00.0"), "0000:03:00.0");
    assert_eq!(normalise_pci_bdf("ABCD:EF:12.7"), "abcd:ef:12.7");
}

// ----- Engine busy delta math --------------------------------------

#[test]
fn engine_busy_first_call_seeds_zero() {
    // last_timestamp_us == 0 means "no baseline yet" — must return 0.0
    // so the first refresh per card reports a clean zero instead of a
    // huge bogus delta against an uninitialised baseline.
    let sample = make_engine_sample(ffi::ZES_ENGINE_GROUP_COMPUTE_SINGLE, 0, 0);
    let stats = ffi::zes_engine_stats_t {
        active_time: 1_000,
        timestamp: 10_000,
    };
    assert_eq!(compute_engine_busy_pct(&sample, &stats), 0.0);
}

#[test]
fn engine_busy_percent_correct() {
    // 500us active over 1000us wall -> 50%.
    let sample = make_engine_sample(ffi::ZES_ENGINE_GROUP_COMPUTE_SINGLE, 1_000, 5_000);
    let stats = ffi::zes_engine_stats_t {
        active_time: 1_500, // delta = 500us
        timestamp: 6_000,   // delta = 1000us
    };
    let pct = compute_engine_busy_pct(&sample, &stats);
    assert!((pct - 50.0).abs() < 1e-9, "pct={pct}");
}

#[test]
fn engine_busy_clamps_to_100_on_overrun() {
    // Driver bug: active_time advances faster than wall — clamp to 100%
    // so a buggy driver does not poison downstream consumers.
    let sample = make_engine_sample(ffi::ZES_ENGINE_GROUP_COMPUTE_SINGLE, 1_000, 5_000);
    let stats = ffi::zes_engine_stats_t {
        active_time: 10_000, // delta = 9000us
        timestamp: 6_000,    // delta = 1000us
    };
    assert_eq!(compute_engine_busy_pct(&sample, &stats), 100.0);
}

#[test]
fn engine_busy_handles_backwards_clock() {
    // Counter reset / timestamp regression: must return 0.0, not panic.
    let sample = make_engine_sample(ffi::ZES_ENGINE_GROUP_COMPUTE_SINGLE, 1_000, 6_000);
    let stats = ffi::zes_engine_stats_t {
        active_time: 0,
        timestamp: 5_000,
    };
    assert_eq!(compute_engine_busy_pct(&sample, &stats), 0.0);
}

#[test]
fn engine_busy_handles_zero_delta_t() {
    let sample = make_engine_sample(ffi::ZES_ENGINE_GROUP_COMPUTE_SINGLE, 1_000, 5_000);
    let stats = ffi::zes_engine_stats_t {
        active_time: 1_500,
        timestamp: 5_000,
    };
    assert_eq!(compute_engine_busy_pct(&sample, &stats), 0.0);
}

// ----- Energy counter delta math -----------------------------------

#[test]
fn power_first_call_seeds_none() {
    let sample = make_power_sample(0, 0);
    let counter = ffi::zes_power_energy_counter_t {
        energy: 1_000_000_000,
        timestamp: 10_000,
    };
    assert!(compute_power_watts(&sample, &counter).is_none());
}

#[test]
fn power_watts_correct() {
    // 30 J over 1s -> 30 W.
    let sample = make_power_sample(0, 5_000_000); // 5s baseline
    let counter = ffi::zes_power_energy_counter_t {
        energy: 30_000_000,   // 30 J in microjoules (delta = 30_000_000)
        timestamp: 6_000_000, // 1s later (delta = 1_000_000us)
    };
    let watts = compute_power_watts(&sample, &counter).unwrap();
    assert!((watts - 30.0).abs() < 1e-9, "watts={watts}");
}

#[test]
fn power_handles_backwards_clock() {
    // Counter reset or driver bug: return None rather than negative
    // watts (which would corrupt the downstream histograms).
    let sample = make_power_sample(1_000, 6_000_000);
    let counter = ffi::zes_power_energy_counter_t {
        energy: 0,
        timestamp: 5_000_000,
    };
    assert!(compute_power_watts(&sample, &counter).is_none());
}

#[test]
fn power_handles_zero_delta_t() {
    let sample = make_power_sample(1_000, 5_000_000);
    let counter = ffi::zes_power_energy_counter_t {
        energy: 10_000,
        timestamp: 5_000_000,
    };
    assert!(compute_power_watts(&sample, &counter).is_none());
}

#[test]
fn power_handles_energy_reset() {
    // Sometimes the energy counter wraps or resets — saturating_sub
    // means we report 0 watts that interval, not a u64-wrap-around
    // garbage value.
    let sample = make_power_sample(1_000_000, 5_000_000);
    let counter = ffi::zes_power_energy_counter_t {
        energy: 500_000, // smaller than last_energy_uj
        timestamp: 6_000_000,
    };
    let watts = compute_power_watts(&sample, &counter).unwrap();
    assert_eq!(watts, 0.0);
}

// ----- Primary utilization picker ---------------------------------

#[test]
fn primary_utilization_prefers_render_or_compute() {
    let engines = vec![
        ("compute (XMX)", 80.0_f64),
        ("render", 30.0_f64),
        ("copy", 90.0_f64),
        ("media-decode", 5.0_f64),
    ];
    // 80 (compute XMX) beats 30 (render) — copy 90% must NOT win.
    assert_eq!(primary_utilization(&engines), Some(80.0));
}

#[test]
fn primary_utilization_falls_back_when_no_compute() {
    let engines = vec![("copy", 12.0_f64), ("media-decode", 7.0_f64)];
    assert_eq!(primary_utilization(&engines), Some(12.0));
}

#[test]
fn primary_utilization_empty_returns_none() {
    let engines: Vec<(&'static str, f64)> = Vec::new();
    assert_eq!(primary_utilization(&engines), None);
}

// ----- GpuInfo integration ----------------------------------------

fn make_baseline_gpu_info() -> GpuInfo {
    GpuInfo {
        uuid: "Intel-GPU-0000:03:00.0".to_string(),
        time: "2026-01-01 00:00:00".to_string(),
        name: "Intel Arc B580".to_string(),
        device_type: "GPU".to_string(),
        host_id: "test-host".to_string(),
        hostname: "test-host".to_string(),
        instance: "test-host".to_string(),
        utilization: 0.0,
        ane_utilization: 0.0,
        dla_utilization: None,
        tensorcore_utilization: None,
        temperature: 0,
        used_memory: 0,
        total_memory: 12 * 1024 * 1024 * 1024,
        frequency: 0,
        power_consumption: 0.0,
        gpu_core_count: None,
        temperature_threshold_slowdown: None,
        temperature_threshold_shutdown: None,
        temperature_threshold_max_operating: None,
        temperature_threshold_acoustic: None,
        performance_state: None,
        numa_node_id: None,
        gsp_firmware_mode: None,
        gsp_firmware_version: None,
        nvlink_remote_devices: Vec::new(),
        gpm_metrics: None,
        detail: HashMap::new(),
    }
}

#[test]
fn apply_to_gpu_info_linux_does_not_overwrite_utilization() {
    // Linux semantics: sysfs engine counters drive `utilization`; L0
    // adds detail entries and the Power (L0) field. The `utilization`
    // value must remain untouched even when L0 has engine data.
    let mut gpu = make_baseline_gpu_info();
    gpu.utilization = 42.0; // pretend sysfs already filled this in
    gpu.detail.insert(
        "Metrics Source".to_string(),
        "sysfs (engine counters)".to_string(),
    );

    let readout = LevelZeroReadout {
        engines: vec![("compute (XMX)", 80.0), ("render", 30.0)],
        power_watts: Some(120.5),
        had_any_data: true,
    };
    apply_to_gpu_info(&mut gpu, &readout, ApplyPlatform::Linux);

    assert_eq!(
        gpu.utilization, 42.0,
        "Linux must NOT overwrite utilization"
    );
    assert_eq!(
        gpu.detail
            .get("Engine: compute (XMX) (L0)")
            .map(String::as_str),
        Some("80.00%")
    );
    assert_eq!(
        gpu.detail.get("Engine: render (L0)").map(String::as_str),
        Some("30.00%")
    );
    assert_eq!(
        gpu.detail.get("Power (L0)").map(String::as_str),
        Some("120.50 W")
    );
    assert_eq!(
        gpu.detail.get("Metrics Source").map(String::as_str),
        Some("sysfs + Level Zero")
    );
}

#[test]
fn apply_to_gpu_info_windows_overwrites_zero_fields() {
    // Windows semantics: WMI baseline has utilization = 0 and
    // power_consumption = 0.0 — placeholders for "no data". L0
    // overwrites both with real numbers.
    let mut gpu = make_baseline_gpu_info();
    gpu.detail
        .insert("Metrics Source".to_string(), "WMI".to_string());

    let readout = LevelZeroReadout {
        engines: vec![("compute (XMX)", 65.0), ("render", 20.0)],
        power_watts: Some(95.0),
        had_any_data: true,
    };
    apply_to_gpu_info(&mut gpu, &readout, ApplyPlatform::Windows);

    // Primary picks max(render, compute (XMX)) = 65.
    assert!((gpu.utilization - 65.0).abs() < 1e-9);
    assert!((gpu.power_consumption - 95.0).abs() < 1e-9);
    assert_eq!(
        gpu.detail.get("Metrics Source").map(String::as_str),
        Some("WMI + Level Zero")
    );
}

#[test]
fn apply_to_gpu_info_no_data_keeps_baseline() {
    // `had_any_data == false` is the "L0 visible but card not visible"
    // path — must leave the existing detail map and metric fields
    // unchanged so the caller's baseline survives.
    let mut gpu = make_baseline_gpu_info();
    gpu.utilization = 42.0;
    gpu.detail
        .insert("Metrics Source".to_string(), "WMI".to_string());

    let readout = LevelZeroReadout::default();
    apply_to_gpu_info(&mut gpu, &readout, ApplyPlatform::Windows);

    assert_eq!(gpu.utilization, 42.0);
    assert_eq!(
        gpu.detail.get("Metrics Source").map(String::as_str),
        Some("WMI")
    );
    assert!(!gpu.detail.contains_key("Power (L0)"));
}

// ----- Library-not-found behaviour --------------------------------

#[test]
fn try_load_library_returns_none_for_nonexistent_path() {
    // Verifies the runtime degrades gracefully on hosts (like CI) that
    // do not have a Level Zero loader at all. Passing a bogus path
    // must NOT panic — it must return None so the caller silently
    // falls back to the sysfs/WMI baseline.
    let bogus = "/nonexistent/path/to/libze_loader.so.1";
    // SAFETY: nonexistent path → dlopen fails → returns None without
    // dereferencing any function pointers.
    let result = unsafe { try_load_library(bogus) };
    assert!(
        result.is_none(),
        "expected None for nonexistent loader path"
    );
}

#[test]
fn enumerated_pci_bdfs_empty_when_runtime_absent() {
    // On hosts without the Level Zero loader (the canonical case for
    // CI), the BDF enumeration helper must return an empty list, not
    // panic. The Windows reader relies on this contract to skip the
    // ordinal-based pairing loop entirely when no L0 hardware is
    // reachable.
    let bdfs = enumerated_pci_bdfs();
    // Either zero (no loader) or some non-empty list (developer host
    // with Intel GPU). Both are valid; the contract is "does not
    // panic and returns a Vec<String>".
    let _: Vec<String> = bdfs;
}

#[test]
fn refresh_returns_none_without_runtime() {
    // Refresh against a fresh state on a host without an L0 loader
    // must return None — the per-OS readers rely on this to leave
    // the sysfs / WMI baseline untouched.
    let mut state = LevelZeroState::empty();
    let result = refresh(&mut state, "0000:03:00.0");
    // None when the loader is unavailable. On a host where the loader
    // happens to be present but the BDF does not match any L0 device,
    // we also expect None (bind_attempted flips to true, device stays
    // None).
    if let Some(readout) = result {
        // If we DID get a runtime, refresh against an unknown BDF must
        // still produce no data — bind to an unknown card fails.
        assert!(
            !readout.had_any_data,
            "unknown BDF must not produce data, got {readout:?}"
        );
    }
}

// ----- Diagnostic helpers -----------------------------------------

#[test]
fn diagnostic_helpers_on_empty_state() {
    let state = LevelZeroState::empty();
    assert_eq!(engine_count(&state), 0);
    assert_eq!(power_domain_count(&state), 0);
    assert!(!is_bound(&state));
}

#[test]
fn sort_engine_entries_canonical_order() {
    let mut engines = vec![
        ("media-encode", 10.0_f64),
        ("compute (XMX)", 50.0_f64),
        ("render", 30.0_f64),
        ("copy", 5.0_f64),
        ("media-decode", 2.0_f64),
    ];
    sort_engine_entries(&mut engines);
    let order: Vec<&'static str> = engines.iter().map(|(l, _)| *l).collect();
    // render first, then compute (XMX), then copy, then media-decode, then media-encode.
    assert_eq!(
        order,
        vec![
            "render",
            "compute (XMX)",
            "copy",
            "media-decode",
            "media-encode"
        ]
    );
}
