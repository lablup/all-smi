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

//! Tests for the N1X / GB10 SoC helpers, using the values probed on an
//! ASUS ProArt P14 (RTX Spark N1X, driver 616.62).

use super::*;
use crate::metrics::gpu_readings::mean_utilization;
use std::collections::HashMap;

const N1X_GPU_NAME: &str = "NVIDIA RTX Spark N1X (6144-core Blackwell RTX GPU)";
const N1X_GPU_UUID: &str = "GPU-1e1f295a-4e4f-b9ef-ff91-2226a89c45aa";
const N1X_NPU_UUID: &str = "DLA-28469e90-187e-afc7-84f8-35a8f9c308d3";
const GIB: u64 = 1024 * 1024 * 1024;

fn gpu_row(name: &str, utilization: f64) -> GpuInfo {
    GpuInfo {
        uuid: N1X_GPU_UUID.to_string(),
        time: String::new(),
        name: name.to_string(),
        device_type: "GPU".to_string(),
        host_id: "spark".to_string(),
        hostname: "spark".to_string(),
        instance: "spark".to_string(),
        utilization,
        ane_utilization: 0.0,
        dla_utilization: None,
        tensorcore_utilization: None,
        temperature: 44,
        used_memory: GIB,
        total_memory: 62 * GIB,
        frequency: 2865,
        power_consumption: 9.57,
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

// ---------- classification ----------

#[test]
fn n1x_gpu_handle_is_a_gpu() {
    assert_eq!(
        classify_nvml_device(N1X_GPU_NAME, Some(N1X_GPU_UUID), Some(5)),
        NvmlDeviceKind::Gpu
    );
}

#[test]
fn n1x_npu_handle_is_a_dla_from_every_signal() {
    // All three signals as the driver reports them.
    assert_eq!(
        classify_nvml_device("NVIDIA NPU", Some(N1X_NPU_UUID), Some(NVML_BRAND_NPU_RAW)),
        NvmlDeviceKind::Dla
    );
    // Each signal alone is enough.
    assert_eq!(
        classify_nvml_device("Renamed", Some(N1X_NPU_UUID), None),
        NvmlDeviceKind::Dla
    );
    assert_eq!(
        classify_nvml_device("Renamed", Some("XYZ-1"), Some(NVML_BRAND_NPU_RAW)),
        NvmlDeviceKind::Dla
    );
    assert_eq!(
        classify_nvml_device("nvidia npu", None, None),
        NvmlDeviceKind::Dla
    );
    // Lowercase UUID prefix.
    assert_eq!(
        classify_nvml_device("x", Some("dla-0000"), None),
        NvmlDeviceKind::Dla
    );
}

#[test]
fn discrete_gpus_are_not_dlas() {
    for (name, uuid) in [
        ("NVIDIA GeForce RTX 4090", "GPU-aaaa"),
        ("NVIDIA H100 80GB HBM3", "GPU-bbbb"),
        ("NVIDIA GB10", "GPU-cccc"),
    ] {
        assert_eq!(
            classify_nvml_device(name, Some(uuid), Some(2)),
            NvmlDeviceKind::Gpu,
            "{name}"
        );
    }
    // Short UUIDs must not panic on the prefix slice.
    assert_eq!(
        classify_nvml_device("x", Some("DL"), None),
        NvmlDeviceKind::Gpu
    );
}

#[test]
fn soc_names_cover_gb10_and_n1x() {
    assert!(is_soc_device_name(N1X_GPU_NAME));
    assert!(is_soc_device_name("NVIDIA GB10"));
    assert!(is_soc_device_name("NVIDIA DGX Spark"));
    assert!(!is_soc_device_name("NVIDIA GeForce RTX 5090"));
    assert!(!is_soc_device_name("NVIDIA GH200 480GB"));
}

// ---------- memory class ----------

#[test]
fn n1x_carve_out_is_a_soc_even_with_memory() {
    // 63,936 MiB framebuffer carve-out of the 128 GiB package.
    let class = classify_memory(67_041_755_136, true, N1X_GPU_NAME, true);
    assert_eq!(class, MemoryClass::UnifiedCarveOut);
    assert!(class.is_soc());
    // The name alone is enough when the C2C query is unavailable.
    assert_eq!(
        classify_memory(67_041_755_136, true, N1X_GPU_NAME, false),
        MemoryClass::UnifiedCarveOut
    );
}

#[test]
fn c2c_on_windows_marks_an_unnamed_soc() {
    assert_eq!(
        classify_memory(32 * GIB, true, "NVIDIA RTX Future SoC", true),
        MemoryClass::UnifiedCarveOut
    );
}

#[test]
fn gb10_without_framebuffer_reads_system_memory() {
    assert_eq!(
        classify_memory(0, true, "NVIDIA GB10", false),
        MemoryClass::UnifiedSystem
    );
    // Blackwell with no framebuffer, unknown name: the pre-#459 rule.
    assert_eq!(
        classify_memory(0, true, "Unknown", false),
        MemoryClass::UnifiedSystem
    );
}

#[test]
fn discrete_cards_stay_discrete() {
    assert_eq!(
        classify_memory(32 * GIB, true, "NVIDIA GeForce RTX 5090", false),
        MemoryClass::Discrete
    );
    // Grace Hopper enables C2C on Linux, which the caller does not pass on.
    assert_eq!(
        classify_memory(96 * GIB, false, "NVIDIA GH200 480GB", false),
        MemoryClass::Discrete
    );
    assert_eq!(
        classify_memory(0, false, "Tesla T4", false),
        MemoryClass::Discrete
    );
    assert!(!MemoryClass::Discrete.is_soc());
}

// ---------- thresholds ----------

#[test]
fn zero_threshold_sentinel_is_dropped() {
    assert_eq!(valid_threshold(Some(0)), None);
    assert_eq!(valid_threshold(None), None);
    assert_eq!(valid_threshold(Some(99)), Some(99));
}

// ---------- DLA pairing and folding ----------

#[test]
fn dla_pairs_with_the_soc_gpu() {
    assert_eq!(pair_dla_with_gpu(&[(0, true)]), Some(0));
    assert_eq!(pair_dla_with_gpu(&[(0, false), (1, true)]), Some(1));
}

#[test]
fn dla_pairs_with_a_lone_gpu() {
    assert_eq!(pair_dla_with_gpu(&[(0, false)]), Some(0));
}

#[test]
fn dla_is_dropped_without_a_clear_host() {
    assert_eq!(pair_dla_with_gpu(&[]), None);
    assert_eq!(pair_dla_with_gpu(&[(0, false), (1, false)]), None);
}

#[test]
fn folded_dla_fills_the_gpu_row_and_keeps_mean_utilization() {
    let mut gpu = gpu_row(N1X_GPU_NAME, 8.0);
    let reading = DlaReading {
        utilization: Some(0.0),
        power_watts: Some(0.882),
        clock_mhz: None,
        memory_clock_mhz: Some(9400),
        temperature: Some(46),
    };
    apply_dla_reading(&mut gpu, &reading, Some(N1X_NPU_UUID));

    assert_eq!(gpu.dla_utilization, Some(0.0));
    assert_eq!(
        gpu.detail
            .get(detail_keys::DLA_POWER_WATTS_DETAIL_KEY)
            .map(String::as_str),
        Some("0.88")
    );
    assert_eq!(
        gpu.detail
            .get(detail_keys::DLA_CLOCK_MEMORY_DETAIL_KEY)
            .map(String::as_str),
        Some("9400")
    );
    assert_eq!(
        gpu.detail
            .get(detail_keys::DLA_TEMPERATURE_DETAIL_KEY)
            .map(String::as_str),
        Some("46")
    );
    assert!(!gpu.detail.contains_key(detail_keys::DLA_CLOCK_DETAIL_KEY));
    assert_eq!(
        gpu.detail.get("dla_uuid").map(String::as_str),
        Some(N1X_NPU_UUID)
    );

    // The idle NPU used to be a second row averaging 0 % into the mean,
    // halving the GPU's 8 %. Folded, there is one row and the mean is 8 %.
    let rows = vec![gpu];
    assert_eq!(rows.len(), 1);
    assert_eq!(mean_utilization(&rows), Some(8.0));
}

#[test]
fn dla_readings_merge() {
    let a = DlaReading {
        utilization: Some(40.0),
        power_watts: Some(1.0),
        clock_mhz: Some(1105),
        memory_clock_mhz: None,
        temperature: Some(40),
    };
    let b = DlaReading {
        utilization: Some(20.0),
        power_watts: Some(0.5),
        clock_mhz: Some(1562),
        memory_clock_mhz: Some(9400),
        temperature: None,
    };
    let merged = a.merge(b);
    assert_eq!(merged.utilization, Some(30.0));
    assert_eq!(merged.power_watts, Some(1.5));
    assert_eq!(merged.clock_mhz, Some(1562));
    assert_eq!(merged.memory_clock_mhz, Some(9400));
    assert_eq!(merged.temperature, Some(40));
}

#[test]
fn dla_detail_keys_are_volatile() {
    for key in [
        detail_keys::DLA_POWER_WATTS_DETAIL_KEY,
        detail_keys::DLA_CLOCK_DETAIL_KEY,
        detail_keys::DLA_CLOCK_MEMORY_DETAIL_KEY,
        detail_keys::DLA_TEMPERATURE_DETAIL_KEY,
    ] {
        assert!(detail_keys::is_volatile_detail_key(key), "{key}");
    }
}

#[test]
fn memory_status_saturates() {
    assert_eq!(
        memory_status_to_total_used(64 * GIB, 40 * GIB),
        (64 * GIB, 24 * GIB)
    );
    assert_eq!(memory_status_to_total_used(1, 2), (1, 0));
}
