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
use super::loader::{
    MAX_L0_HANDLES, cap_handle_count, format_pci_bdf, normalise_pci_bdf, try_load_library,
};
use super::refresh::{
    compute_engine_busy_pct, compute_power_watts, make_engine_sample, make_power_sample,
};
use super::*;

#[path = "tests/apply.rs"]
mod apply;
#[path = "tests/ffi_layout.rs"]
mod ffi_layout;
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
    // Values cross-checked against the upstream Sysman header at
    // <https://github.com/oneapi-src/level-zero/blob/master/include/zes_api.h>
    // (`typedef enum _zes_engine_group_t`). Several of the 9..=14 slots
    // are marked DEPRECATED in the spec but we still lock their numeric
    // values to detect any accidental renumbering during future updates.
    assert_eq!(ffi::ZES_ENGINE_GROUP_ALL, 0);
    assert_eq!(ffi::ZES_ENGINE_GROUP_COMPUTE_ALL, 1);
    assert_eq!(ffi::ZES_ENGINE_GROUP_MEDIA_ALL, 2);
    assert_eq!(ffi::ZES_ENGINE_GROUP_COPY_ALL, 3);
    assert_eq!(ffi::ZES_ENGINE_GROUP_COMPUTE_SINGLE, 4);
    assert_eq!(ffi::ZES_ENGINE_GROUP_RENDER_SINGLE, 5);
    assert_eq!(ffi::ZES_ENGINE_GROUP_MEDIA_DECODE_SINGLE, 6);
    assert_eq!(ffi::ZES_ENGINE_GROUP_MEDIA_ENCODE_SINGLE, 7);
    assert_eq!(ffi::ZES_ENGINE_GROUP_COPY_SINGLE, 8);
    assert_eq!(ffi::ZES_ENGINE_GROUP_MEDIA_ENHANCEMENT_SINGLE, 9);
    assert_eq!(ffi::ZES_ENGINE_GROUP_3D_SINGLE, 10);
    assert_eq!(ffi::ZES_ENGINE_GROUP_3D_RENDER_COMPUTE_ALL, 11);
    assert_eq!(ffi::ZES_ENGINE_GROUP_RENDER_ALL, 12);
    assert_eq!(ffi::ZES_ENGINE_GROUP_3D_ALL, 13);
    assert_eq!(ffi::ZES_ENGINE_GROUP_MEDIA_CODEC_SINGLE, 14);
}

#[test]
fn init_flags_match_spec() {
    assert_eq!(ffi::ZE_INIT_FLAG_DEFAULT, 0);
    assert_eq!(ffi::ZE_RESULT_SUCCESS, 0);
}

#[test]
fn structure_type_constants_match_spec() {
    assert_eq!(ffi::ZES_STRUCTURE_TYPE_PCI_PROPERTIES, 0x0000_0002);
    assert_eq!(ffi::ZES_STRUCTURE_TYPE_ENGINE_PROPERTIES, 0x0000_0005);
}

// ----- FFI struct ABI locks -----------------------------------------
//
// The Level Zero driver writes into Rust-allocated buffers using the C
// struct layout from `ze_api.h` / `zes_api.h`. If the Rust struct size
// drifts from the C struct size, the driver either over- or under-writes
// our buffer and we read garbage (or worse). These tests measure the
// Rust struct sizes against the spec-documented C sizes on x86_64 LP64,
// the only platform combination the L0 backend currently targets.
//
// Values verified against the upstream level-zero header at
// <https://github.com/oneapi-src/level-zero/blob/master/include/zes_api.h>
// compiled with the system C compiler on x86_64 Linux.

#[cfg(target_pointer_width = "64")]
#[test]
fn zes_pci_address_t_size_matches_spec() {
    assert_eq!(std::mem::size_of::<ffi::zes_pci_address_t>(), 16);
}

#[cfg(target_pointer_width = "64")]
#[test]
fn zes_pci_speed_t_size_matches_spec() {
    assert_eq!(std::mem::size_of::<ffi::zes_pci_speed_t>(), 16);
}

#[cfg(target_pointer_width = "64")]
#[test]
fn zes_pci_properties_t_size_matches_spec() {
    // C spec on x86_64 LP64: stype (4) + pad (4) + pNext (8) +
    // address (16) + maxSpeed (16) + 3x ze_bool_t (3) + trailing pad
    // (5) = 56 bytes. A previous version of this file declared the
    // three booleans as u32 each, inflating the Rust struct to 64
    // bytes; this assertion guards against a regression.
    assert_eq!(std::mem::size_of::<ffi::zes_pci_properties_t>(), 56);
}

#[cfg(target_pointer_width = "64")]
#[test]
fn zes_engine_properties_t_size_matches_spec() {
    // C spec on x86_64 LP64: stype (4) + pad (4) + pNext (8) +
    // type (4) + onSubdevice (1) + pad (3) + subdeviceId (4) = 28,
    // with trailing pad to the 8-byte struct alignment = 32 bytes.
    assert_eq!(std::mem::size_of::<ffi::zes_engine_properties_t>(), 32);
}

#[cfg(target_pointer_width = "64")]
#[test]
fn zes_engine_stats_t_size_matches_spec() {
    // Two u64 fields = 16 bytes.
    assert_eq!(std::mem::size_of::<ffi::zes_engine_stats_t>(), 16);
}

#[cfg(target_pointer_width = "64")]
#[test]
fn zes_power_energy_counter_t_size_matches_spec() {
    // Two u64 fields = 16 bytes.
    assert_eq!(std::mem::size_of::<ffi::zes_power_energy_counter_t>(), 16);
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
    assert!(!is_tracked_engine(
        ffi::ZES_ENGINE_GROUP_3D_RENDER_COMPUTE_ALL
    ));
    assert!(!is_tracked_engine(ffi::ZES_ENGINE_GROUP_3D_ALL));
    assert!(!is_tracked_engine(ffi::ZES_ENGINE_GROUP_RENDER_ALL));
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
    // Sometimes the energy counter wraps or resets. That interval is
    // not fresh enough to override a fallback power reading.
    let sample = make_power_sample(1_000_000, 5_000_000);
    let counter = ffi::zes_power_energy_counter_t {
        energy: 500_000, // smaller than last_energy_uj
        timestamp: 6_000_000,
    };
    assert!(compute_power_watts(&sample, &counter).is_none());
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
            !readout.has_fresh_data(),
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

// ----- MAX_L0_HANDLES cap arithmetic ----------------------------------
//
// `cap_handle_count` guards every Vec allocation against a buggy or
// hostile driver that reports a giant count (DoS via OOM). These tests
// exercise the cap math directly without requiring a real L0 runtime.

#[test]
fn cap_handle_count_passes_through_when_under_cap() {
    // Normal hardware: a few engines plus a couple of power domains.
    // The count must pass through unchanged so the Vec is sized exactly.
    let (safe, capped_u32) = cap_handle_count(6, "engine groups");
    assert_eq!(safe, 6);
    assert_eq!(capped_u32, 6);
}

#[test]
fn cap_handle_count_clamps_when_over_cap() {
    // Driver reports u32::MAX — without the cap this would attempt a
    // ~32 GiB Vec allocation and OOM the process. The cap must reduce
    // both the usize (for Vec::with_capacity) and the u32 (for the
    // second "fill" call of the count-then-buffer idiom) to MAX_L0_HANDLES.
    let giant: u32 = u32::MAX;
    let (safe, capped_u32) = cap_handle_count(giant, "drivers");
    assert_eq!(safe, MAX_L0_HANDLES);
    assert_eq!(capped_u32, MAX_L0_HANDLES as u32);
    // The capped values must agree with each other.
    assert_eq!(safe, capped_u32 as usize);
}

#[test]
fn cap_handle_count_at_exact_boundary() {
    // Exactly MAX_L0_HANDLES — must pass through, not be clamped.
    let at_limit = MAX_L0_HANDLES as u32;
    let (safe, capped_u32) = cap_handle_count(at_limit, "devices");
    assert_eq!(safe, MAX_L0_HANDLES);
    assert_eq!(capped_u32, at_limit);
}

#[test]
fn cap_handle_count_one_over_boundary_is_clamped() {
    // MAX_L0_HANDLES + 1 must be clamped to MAX_L0_HANDLES.
    let one_over = (MAX_L0_HANDLES + 1) as u32;
    let (safe, capped_u32) = cap_handle_count(one_over, "power domains");
    assert_eq!(safe, MAX_L0_HANDLES);
    assert_eq!(capped_u32, MAX_L0_HANDLES as u32);
}
