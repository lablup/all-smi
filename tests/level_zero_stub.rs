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

// The backend does not exist on targets `build.rs` does not emit the cfg
// for, macOS among them, so this whole binary compiles away there rather
// than failing to resolve the module.
#![cfg(all_smi_level_zero)]

//! Drive the Level Zero backend against the synthetic loader in
//! `tests/fixtures/level_zero/stub_ze_loader.c`.
//!
//! ## Why this is a separate test binary
//!
//! `LZ_RUNTIME` is a process-wide `OnceCell`, latched by whichever caller
//! reaches `ensure_runtime` first, and two unit tests in the library's own
//! test binary already latch it. Whoever wins decides which loader the whole
//! binary sees, so a stub test living beside them would be a race. Cargo
//! compiles each file under `tests/` into its own binary, hence its own
//! process, which is what makes the `LD_LIBRARY_PATH` binding deterministic.
//!
//! ## How the stub is bound
//!
//! `LIBZE_PATHS[0]` is the bare SONAME `libze_loader.so.1`, and `dlopen` on
//! a bare SONAME searches `LD_LIBRARY_PATH` before the default paths. A
//! stub built under that name, in a directory placed first, therefore wins
//! over the real loader the CI job also installs. No production code knows
//! this file exists: no path was added to `LIBZE_PATHS`, no environment
//! override was added to the loader, and nothing here is compiled into the
//! `all-smi` binary.
//!
//! ## Failing loudly
//!
//! Every test returns early unless `ALL_SMI_LEVEL_ZERO_STUB=1`, so a
//! developer without the stub is not bothered. That makes a green run
//! ambiguous on its own, which is the same trap the real-loader check in
//! #365 had, so the same three-part answer applies: the env key arms the
//! assertions, a marker is printed only after they pass, and the CI step
//! greps for the marker.

use all_smi::device::readers::intel_gpu_level_zero as l0;
use all_smi::device::types::GpuInfo;
use std::collections::HashMap;

/// Set by CI once the stub is built and `LD_LIBRARY_PATH` points at it.
const STUB_ENV: &str = "ALL_SMI_LEVEL_ZERO_STUB";

/// Printed only after a test has actually asserted. See the module note.
const MARKER: &str = "all-smi: level-zero-stub-assertions-ran";

/// The addresses `stub_ze_loader.c` reports, in the order
/// `enumerated_pci_bdfs` must sort them.
const BDF_A: &str = "0000:03:00.0";
const BDF_B: &str = "0000:af:00.0";

fn armed() -> bool {
    std::env::var_os(STUB_ENV).is_some()
}

fn blank_gpu_info() -> GpuInfo {
    GpuInfo {
        uuid: "stub-0".to_string(),
        time: "2026-01-01 00:00:00".to_string(),
        name: "Stub Intel GPU".to_string(),
        device_type: "GPU".to_string(),
        host_id: "stub-host".to_string(),
        hostname: "stub-host".to_string(),
        instance: "stub-host".to_string(),
        utilization: 0.0,
        ane_utilization: 0.0,
        dla_utilization: None,
        tensorcore_utilization: None,
        temperature: 0,
        used_memory: 0,
        total_memory: 0,
        frequency: 0,
        power_consumption: 0.0,
        gpu_core_count: None,
        temperature_threshold_slowdown: None,
        temperature_threshold_shutdown: None,
        temperature_threshold_max_operating: None,
        temperature_threshold_acoustic: None,
        performance_state: None,
        fan_speed_rpm: None,
        numa_node_id: None,
        gsp_firmware_mode: None,
        gsp_firmware_version: None,
        nvlink_remote_devices: Vec::new(),
        gpm_metrics: None,
        detail: HashMap::new(),
    }
}

/// Take the two samples the delta families need, and return the second
/// readout. The first is only a seed by design.
fn second_readout(bdf: &str) -> l0::LevelZeroReadout {
    let mut state = l0::LevelZeroState::empty();
    let first = l0::refresh(&mut state, bdf)
        .unwrap_or_else(|| panic!("{STUB_ENV} is set but the stub produced no readout for {bdf}"));
    // Seeded, not fresh: a delta needs two samples.
    assert!(
        first.primary_engine_utilization.is_none(),
        "the first refresh must only seed the delta counters, got {:?}",
        first.primary_engine_utilization
    );
    l0::refresh(&mut state, bdf).expect("second refresh must produce a readout")
}

/// The full chain `zeDriverGet` then `zeDeviceGet` then
/// `zesDevicePciGetProperties` then `format_pci_bdf`, end to end. Nothing
/// short of a real driver has exercised it before.
#[test]
fn the_stub_devices_enumerate_in_sorted_bdf_order() {
    if !armed() {
        return;
    }
    let bdfs = l0::enumerated_pci_bdfs();
    assert_eq!(
        bdfs,
        vec![BDF_A.to_string(), BDF_B.to_string()],
        "the stub reports bus 0x03 and 0xaf; a wrong offset in zes_pci_address_t \
         changes these strings"
    );
    println!("{MARKER}");
}

/// Exact constants, not ranges. The stub advances `activeTime` by 250000 and
/// `timestamp` by 1000000 microseconds per call, so the second sample is
/// 25.00% by construction, and energy by 45000000 microjoules over the same
/// tick, which is 45.00 W.
#[test]
fn the_delta_families_produce_the_exact_synthetic_values() {
    if !armed() {
        return;
    }
    let readout = second_readout(BDF_A);

    let engine = readout
        .primary_engine_utilization
        .expect("engine activity must be fresh on the second sample");
    assert!(
        (engine.value - 25.0).abs() < 1e-9,
        "expected exactly 25.00% compute busy, got {}",
        engine.value
    );

    let power = readout
        .power_watts
        .expect("power must be fresh on the second sample");
    assert!(
        (power.value - 45.0).abs() < 1e-9,
        "expected exactly 45.00 W, got {}",
        power.value
    );

    // Both engines are tracked, and the primary is the larger of the two.
    let render = readout
        .engines
        .iter()
        .find(|(label, _)| *label == "render")
        .map(|(_, pct)| *pct)
        .expect("the render engine must appear in the per-engine list");
    assert!(
        (render - 10.0).abs() < 1e-9,
        "expected exactly 10.00% render, got {render}"
    );
    println!("{MARKER}");
}

/// Point-in-time families, and the reason the stub is compiled against the
/// vendor headers: each value below is read back through a different
/// `#[repr(C)]` struct, so a field at the wrong offset changes an
/// observable number rather than passing silently.
#[test]
fn the_point_in_time_families_land_in_gpu_info() {
    if !armed() {
        return;
    }
    let readout = second_readout(BDF_A);
    let mut gpu = blank_gpu_info();
    l0::apply_to_gpu_info(&mut gpu, &readout, l0::ApplyPlatform::Linux);

    // zes_temp_properties_t + zesTemperatureGetState
    assert_eq!(gpu.temperature, 61, "temperature came back wrong");
    // zes_freq_state_t: `actual` sits after four other f64 fields, so a
    // one-field slip reports 1200, 2200, 2300, or a voltage as a frequency.
    assert_eq!(gpu.frequency, 2100, "frequency came back wrong");
    // zes_mem_state_t: 12 GiB total, 4 GiB free, so 8 GiB used.
    assert_eq!(gpu.total_memory, 12 * 1024 * 1024 * 1024);
    assert_eq!(gpu.used_memory, 8 * 1024 * 1024 * 1024);
    // zes_fan_properties_t: RPM is the only advertised unit, so the
    // percent reading must be absent rather than invented.
    assert_eq!(gpu.fan_speed_rpm, Some(1800));
    assert_eq!(
        gpu.detail.get("Fan Speed").map(String::as_str),
        Some("1800 RPM"),
        "a percent reading must not appear: the stub advertises RPM only"
    );
    println!("{MARKER}");
}

/// Device B advertises 4096 engines, above the `MAX_L0_HANDLES` cap of 256,
/// then fills fewer than requested. Both the clamp and the post-fill
/// truncate have to hold or this panics or reads past the buffer.
#[test]
fn an_over_cap_count_and_a_short_fill_are_survived() {
    if !armed() {
        return;
    }
    let mut state = l0::LevelZeroState::empty();
    let readout = l0::refresh(&mut state, BDF_B).expect("device B must still bind");
    assert!(
        l0::is_bound(&state),
        "device B must bind even though its enumerator misbehaves"
    );
    // One engine survived the clamp and the truncate.
    assert_eq!(l0::engine_count(&state), 1);
    // Device B has no power domain, and a family the driver does not offer
    // must degrade only itself.
    assert_eq!(l0::power_domain_count(&state), 0);
    assert!(readout.power_watts.is_none());
    println!("{MARKER}");
}

/// A BDF the stub does not know must produce nothing, which is the same
/// contract that holds when no runtime is present at all.
#[test]
fn an_unknown_bdf_binds_to_nothing() {
    if !armed() {
        return;
    }
    let mut state = l0::LevelZeroState::empty();
    assert!(l0::refresh(&mut state, "0000:ff:00.0").is_none());
    assert!(!l0::is_bound(&state));
    println!("{MARKER}");
}
