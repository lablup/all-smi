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
use crate::device::types::GpuInfo;
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
    assert_eq!(
        gpu.detail.get("Metrics Source").map(String::as_str),
        Some("sysfs + Level Zero Sysman")
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
    assert_eq!(
        gpu.detail.get("Metrics Source").map(String::as_str),
        Some("WMI + Level Zero Sysman")
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
