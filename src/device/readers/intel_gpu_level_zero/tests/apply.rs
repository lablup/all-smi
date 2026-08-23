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

use super::*;
use crate::device::types::{GpuInfo, MAX_GPU_FAN_RPM};
use std::collections::HashMap;

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
        fan_speed_rpm: None,
        numa_node_id: None,
        gsp_firmware_mode: None,
        gsp_firmware_version: None,
        nvlink_remote_devices: Vec::new(),
        gpm_metrics: None,
        detail: HashMap::new(),
    }
}

#[test]
fn linux_fresh_sysman_overwrites_fields() {
    let mut gpu = make_baseline_gpu_info();
    gpu.utilization = 42.0;
    gpu.temperature = 60;
    gpu.frequency = 1900;
    gpu.power_consumption = 80.0;
    gpu.detail.insert(
        "Metrics Source".to_string(),
        "sysfs (engine counters)".to_string(),
    );
    // Mirror what the Linux sysfs baseline actually produces: the typed
    // field and the detail string are written together from one hwmon read.
    gpu.fan_speed_rpm = Some(1400);
    gpu.detail
        .insert("Fan Speed".to_string(), "1400 RPM".to_string());

    let readout = LevelZeroReadout {
        engines: vec![("compute (XMX)", 80.0), ("render", 30.0)],
        primary_engine_utilization: Some(FreshValue::level_zero(80.0)),
        power_watts: Some(FreshValue::level_zero(120.5)),
        temperature_celsius: Some(FreshValue::level_zero(72)),
        memory: Some(LevelZeroMemoryReadout {
            used_bytes: 4 * 1024 * 1024 * 1024,
            total_bytes: 12 * 1024 * 1024 * 1024,
            kind: LevelZeroMemoryKind::DedicatedLocal,
            source: "Level Zero Sysman",
        }),
        frequency_mhz: Some(FreshValue::level_zero(2300)),
        fan: Some(LevelZeroFanReadout {
            rpm: Some(1800),
            percent: None,
            source: "Level Zero Sysman",
        }),
        ..Default::default()
    };
    apply_to_gpu_info(&mut gpu, &readout, ApplyPlatform::Linux);

    assert_eq!(gpu.utilization, 80.0);
    assert_eq!(gpu.temperature, 72);
    assert_eq!(gpu.frequency, 2300);
    assert_eq!(gpu.used_memory, 4 * 1024 * 1024 * 1024);
    assert_eq!(gpu.total_memory, 12 * 1024 * 1024 * 1024);
    assert_eq!(
        gpu.detail.get("Power (L0)").map(String::as_str),
        Some("120.50 W")
    );
    // The sysfs qualifier survives. Level Zero used to assign this string
    // rather than append to it, so "(engine counters)" was lost the moment
    // Sysman produced a reading.
    assert_eq!(
        gpu.detail.get("Metrics Source").map(String::as_str),
        Some("sysfs (engine counters) + Level Zero Sysman")
    );
    assert_eq!(
        gpu.detail.get("Source: Utilization").map(String::as_str),
        Some("Level Zero Sysman")
    );
    assert_eq!(
        gpu.detail.get("Fan Speed").map(String::as_str),
        Some("1400 RPM"),
        "Linux hwmon fan must keep priority over L0 fan"
    );
    assert_eq!(
        gpu.fan_speed_rpm,
        Some(1400),
        "the typed field must follow the same priority as the detail string"
    );
}

#[test]
fn missing_sysman_fields_keep_linux_baseline() {
    let mut gpu = make_baseline_gpu_info();
    gpu.utilization = 42.0;
    gpu.temperature = 68;
    gpu.frequency = 1950;
    gpu.power_consumption = 150.0;
    let readout = LevelZeroReadout {
        engines: vec![("copy", 90.0)],
        power_watts: Some(FreshValue::level_zero(95.0)),
        ..Default::default()
    };
    apply_to_gpu_info(&mut gpu, &readout, ApplyPlatform::Linux);

    assert_eq!(gpu.utilization, 42.0, "no fresh L0 primary engine");
    assert_eq!(gpu.temperature, 68, "no L0 temperature");
    assert_eq!(gpu.frequency, 1950, "no L0 frequency");
    assert_eq!(gpu.power_consumption, 95.0, "fresh L0 power wins");
}

#[test]
fn shared_memory_does_not_fabricate_vram_budget() {
    let mut gpu = make_baseline_gpu_info();
    gpu.used_memory = 0;
    gpu.total_memory = 0;
    let readout = LevelZeroReadout {
        memory: Some(LevelZeroMemoryReadout {
            used_bytes: 0,
            total_bytes: 0,
            kind: LevelZeroMemoryKind::SharedSystem,
            source: "Level Zero Sysman",
        }),
        ..Default::default()
    };
    apply_to_gpu_info(&mut gpu, &readout, ApplyPlatform::Linux);

    assert_eq!(gpu.used_memory, 0);
    assert_eq!(gpu.total_memory, 0);
    assert_eq!(
        gpu.detail.get("Memory (L0)").map(String::as_str),
        Some("Shared/system memory; dedicated VRAM budget unavailable")
    );
}

#[test]
fn windows_overwrites_wmi_gaps() {
    let mut gpu = make_baseline_gpu_info();
    gpu.detail
        .insert("Metrics Source".to_string(), "WMI".to_string());
    let readout = LevelZeroReadout {
        engines: vec![("compute (XMX)", 65.0), ("render", 20.0)],
        primary_engine_utilization: Some(FreshValue::level_zero(65.0)),
        power_watts: Some(FreshValue::level_zero(95.0)),
        temperature_celsius: Some(FreshValue::level_zero(71)),
        frequency_mhz: Some(FreshValue::level_zero(2200)),
        fan: Some(LevelZeroFanReadout {
            rpm: Some(1600),
            percent: Some(40),
            source: "Level Zero Sysman",
        }),
        ..Default::default()
    };
    apply_to_gpu_info(&mut gpu, &readout, ApplyPlatform::Windows);

    assert!((gpu.utilization - 65.0).abs() < 1e-9);
    assert!((gpu.power_consumption - 95.0).abs() < 1e-9);
    assert_eq!(gpu.temperature, 71);
    assert_eq!(gpu.frequency, 2200);
    assert_eq!(
        gpu.detail.get("Fan Speed").map(String::as_str),
        Some("1600 RPM (40%)")
    );
    // The duty cycle only ever rides in the detail string; the typed field
    // carries the tachometer reading on its own.
    assert_eq!(gpu.fan_speed_rpm, Some(1600));
    assert_eq!(
        gpu.detail.get("Metrics Source").map(String::as_str),
        Some("WMI + Level Zero Sysman")
    );
}

#[test]
fn duty_cycle_only_fan_leaves_the_typed_field_unset() {
    // Some drivers report a fan percentage with no tachometer. A
    // percentage stored in a field named `_rpm` would be exported as a
    // wildly wrong RPM, so the field stays `None` while the percentage
    // still reaches snapshots through the detail string.
    let mut gpu = make_baseline_gpu_info();
    let readout = LevelZeroReadout {
        fan: Some(LevelZeroFanReadout {
            rpm: None,
            percent: Some(40),
            source: "Level Zero Sysman",
        }),
        ..Default::default()
    };
    apply_to_gpu_info(&mut gpu, &readout, ApplyPlatform::Windows);

    assert_eq!(gpu.detail.get("Fan Speed").map(String::as_str), Some("40%"));
    assert!(gpu.fan_speed_rpm.is_none());
}

#[test]
fn windows_duty_cycle_only_fan_clears_a_stale_tachometer_reading() {
    // `overwrite_existing` replaces the detail string unconditionally, so
    // the typed field has to follow it. Keeping an RPM from an earlier
    // sample would leave the exporter publishing a number the detail
    // string no longer agrees with.
    let mut gpu = make_baseline_gpu_info();
    gpu.fan_speed_rpm = Some(1450);
    gpu.detail
        .insert("Fan Speed".to_string(), "1450 RPM".to_string());
    let readout = LevelZeroReadout {
        fan: Some(LevelZeroFanReadout {
            rpm: None,
            percent: Some(40),
            source: "Level Zero Sysman",
        }),
        ..Default::default()
    };
    apply_to_gpu_info(&mut gpu, &readout, ApplyPlatform::Windows);

    assert_eq!(gpu.detail.get("Fan Speed").map(String::as_str), Some("40%"));
    assert!(gpu.fan_speed_rpm.is_none());
}

#[test]
fn linux_l0_fan_fills_a_gap_the_hwmon_baseline_left() {
    // No hwmon tachometer means no `Fan Speed` detail key, so the
    // overwrite guard does not fire and Level Zero supplies both
    // representations.
    let mut gpu = make_baseline_gpu_info();
    assert!(gpu.fan_speed_rpm.is_none());
    let readout = LevelZeroReadout {
        fan: Some(LevelZeroFanReadout {
            rpm: Some(1800),
            percent: None,
            source: "Level Zero Sysman",
        }),
        ..Default::default()
    };
    apply_to_gpu_info(&mut gpu, &readout, ApplyPlatform::Linux);

    assert_eq!(gpu.fan_speed_rpm, Some(1800));
    assert_eq!(
        gpu.detail.get("Fan Speed").map(String::as_str),
        Some("1800 RPM")
    );
}

#[test]
fn a_garbled_l0_fan_reading_is_clamped_before_either_write() {
    // A corrupted Sysman sample must never reach `GpuInfo::fan_speed_rpm`
    // or the `Fan Speed` detail string unclamped, and the two must keep
    // agreeing with each other after the clamp the same way they do for a
    // normal reading.
    let mut gpu = make_baseline_gpu_info();
    let readout = LevelZeroReadout {
        fan: Some(LevelZeroFanReadout {
            rpm: Some(u32::MAX),
            percent: None,
            source: "Level Zero Sysman",
        }),
        ..Default::default()
    };
    apply_to_gpu_info(&mut gpu, &readout, ApplyPlatform::Linux);

    assert_eq!(gpu.fan_speed_rpm, Some(MAX_GPU_FAN_RPM));
    assert_eq!(
        gpu.detail.get("Fan Speed").map(String::as_str),
        Some(format!("{MAX_GPU_FAN_RPM} RPM").as_str())
    );
}

#[test]
fn no_data_keeps_baseline() {
    let mut gpu = make_baseline_gpu_info();
    gpu.utilization = 42.0;
    gpu.detail
        .insert("Metrics Source".to_string(), "WMI".to_string());

    apply_to_gpu_info(
        &mut gpu,
        &LevelZeroReadout::default(),
        ApplyPlatform::Windows,
    );

    assert_eq!(gpu.utilization, 42.0);
    assert_eq!(
        gpu.detail.get("Metrics Source").map(String::as_str),
        Some("WMI")
    );
    assert!(!gpu.detail.contains_key("Power (L0)"));
}

// ---------------------------------------------------------------------
// Integrated Windows parts: the DXGI handoff (issue #364)
// ---------------------------------------------------------------------

/// Shape of an Intel iGPU after the vendor-neutral Windows layer has run:
/// DXGI resolved the capacity to the shared aperture and said so, and PDH
/// contributed a utilization figure.
fn integrated_after_dxgi() -> GpuInfo {
    let mut gpu = make_baseline_gpu_info();
    gpu.name = "Intel(R) Arc(TM) B390 GPU".to_string();
    gpu.total_memory = 19_202_415_943;
    gpu.detail
        .insert("Metrics Source".to_string(), "WMI".to_string());
    gpu.detail
        .insert("Source: Memory".to_string(), "DXGI (shared)".to_string());
    crate::device::readers::detail_keys::note_metrics_source(&mut gpu.detail, "DXGI");
    crate::device::readers::detail_keys::note_metrics_source(&mut gpu.detail, "PDH");
    gpu
}

/// A 128 MiB Sysman readout on an integrated part is the stolen-memory
/// carve-out, not the capacity. Letting it through is how the B390 was
/// reported with 128 MiB of VRAM against a real 17.88 GiB aperture.
#[test]
fn a_dedicated_carve_out_never_replaces_a_shared_aperture() {
    let mut gpu = integrated_after_dxgi();
    let readout = LevelZeroReadout {
        memory: Some(LevelZeroMemoryReadout {
            used_bytes: 64 * 1024 * 1024,
            total_bytes: 128 * 1024 * 1024,
            kind: LevelZeroMemoryKind::DedicatedLocal,
            source: "Level Zero Sysman",
        }),
        temperature_celsius: Some(FreshValue::level_zero(48)),
        ..Default::default()
    };
    apply_to_gpu_info(&mut gpu, &readout, ApplyPlatform::Windows);

    assert_eq!(gpu.total_memory, 19_202_415_943);
    assert_eq!(
        gpu.detail.get("Source: Memory").map(String::as_str),
        Some("DXGI (shared)")
    );
    // Not discarded: the carve-out is real, it is just not the capacity.
    assert_eq!(
        gpu.detail.get("VRAM Dedicated (L0)").map(String::as_str),
        Some("134217728 bytes")
    );
    assert!(!gpu.detail.contains_key("VRAM Total"));
    // The rest of the readout still lands.
    assert_eq!(gpu.temperature, 48);
}

/// The guard must not cost discrete cards their Sysman VRAM figure, which
/// is the only 64-bit-correct source when DXGI is unavailable.
#[test]
fn a_discrete_card_still_takes_its_sysman_total() {
    let mut gpu = make_baseline_gpu_info();
    gpu.detail
        .insert("Source: Memory".to_string(), "WMI".to_string());
    let readout = LevelZeroReadout {
        memory: Some(LevelZeroMemoryReadout {
            used_bytes: 2 * 1024 * 1024 * 1024,
            total_bytes: 12 * 1024 * 1024 * 1024,
            kind: LevelZeroMemoryKind::DedicatedLocal,
            source: "Level Zero Sysman",
        }),
        ..Default::default()
    };
    apply_to_gpu_info(&mut gpu, &readout, ApplyPlatform::Windows);

    assert_eq!(gpu.total_memory, 12 * 1024 * 1024 * 1024);
    assert_eq!(gpu.used_memory, 2 * 1024 * 1024 * 1024);
    assert_eq!(
        gpu.detail.get("Source: Memory").map(String::as_str),
        Some("Level Zero Sysman")
    );
}

/// Linux has no DXGI layer, so the marker cannot appear there and the
/// sysfs-derived total must keep yielding to Sysman as it always has.
#[test]
fn the_shared_aperture_guard_is_windows_only() {
    let mut gpu = make_baseline_gpu_info();
    gpu.detail
        .insert("Source: Memory".to_string(), "DXGI (shared)".to_string());
    let readout = LevelZeroReadout {
        memory: Some(LevelZeroMemoryReadout {
            used_bytes: 1024,
            total_bytes: 8 * 1024 * 1024 * 1024,
            kind: LevelZeroMemoryKind::DedicatedLocal,
            source: "Level Zero Sysman",
        }),
        ..Default::default()
    };
    apply_to_gpu_info(&mut gpu, &readout, ApplyPlatform::Linux);

    assert_eq!(gpu.total_memory, 8 * 1024 * 1024 * 1024);
}

/// A driver that reports a dedicated pool of zero must not zero a capacity
/// another layer already established.
#[test]
fn an_empty_dedicated_readout_leaves_the_total_alone() {
    let mut gpu = make_baseline_gpu_info();
    let readout = LevelZeroReadout {
        memory: Some(LevelZeroMemoryReadout {
            used_bytes: 0,
            total_bytes: 0,
            kind: LevelZeroMemoryKind::DedicatedLocal,
            source: "Level Zero Sysman",
        }),
        temperature_celsius: Some(FreshValue::level_zero(40)),
        ..Default::default()
    };
    apply_to_gpu_info(&mut gpu, &readout, ApplyPlatform::Windows);

    assert_eq!(gpu.total_memory, 12 * 1024 * 1024 * 1024);
    assert!(!gpu.detail.contains_key("VRAM Dedicated (L0)"));
}

/// Every layer that ran must still be named. Assigning here is what made a
/// fully-instrumented host report only "WMI + Level Zero Sysman".
#[test]
fn the_full_windows_stack_is_recorded_in_order() {
    let mut gpu = integrated_after_dxgi();
    let readout = LevelZeroReadout {
        temperature_celsius: Some(FreshValue::level_zero(48)),
        ..Default::default()
    };
    apply_to_gpu_info(&mut gpu, &readout, ApplyPlatform::Windows);

    assert_eq!(
        gpu.detail.get("Metrics Source").map(String::as_str),
        Some("WMI + DXGI + PDH + Level Zero Sysman")
    );
}

/// Polling repeatedly must not grow the string.
#[test]
fn repeated_polls_do_not_grow_the_metrics_source() {
    let mut gpu = integrated_after_dxgi();
    let readout = LevelZeroReadout {
        temperature_celsius: Some(FreshValue::level_zero(48)),
        ..Default::default()
    };
    for _ in 0..5 {
        apply_to_gpu_info(&mut gpu, &readout, ApplyPlatform::Windows);
    }
    assert_eq!(
        gpu.detail.get("Metrics Source").map(String::as_str),
        Some("WMI + DXGI + PDH + Level Zero Sysman")
    );
}
