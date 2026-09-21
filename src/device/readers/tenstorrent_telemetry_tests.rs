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

//! The reader's dynamic telemetry, end to end into the exposition (#425).
//!
//! `api::metrics::npu::tenstorrent` has always declared the
//! `all_smi_tenstorrent_*` telemetry gauges, and `API.md` has always
//! documented them, but none of them could fire: the exporter looks up
//! snake_case keys holding bare numbers, and the reader wrote Title Case
//! keys holding unit-suffixed strings (`"AI Clock"` => `"800MHz"`). The only
//! place a Tenstorrent clock, voltage or current reached the wire was as an
//! `all_smi_gpu_info` label that changed on nearly every scrape.
//!
//! These tests drive `build_device_details` and then the real exporter, so
//! the two halves cannot drift apart again without failing here.
//!
//! Linux-only, like the reader and its exporter, so they run in the CI Test
//! Suite job rather than on a macOS developer machine.

use super::*;
use crate::api::metrics::{MetricExporter, npu::NpuMetricExporter};
use crate::device::types::GpuInfo;
use luwen_def::Arch;

/// A Wormhole chip reporting the telemetry this module documents.
///
/// `Arch::Wormhole` is pinned deliberately: `Telemetry::asic_temperature`
/// decodes Blackhole as a 16.16 fixed-point value and everything else as a
/// 4-bit fraction, so the expected temperature depends on the arch.
fn wormhole_telemetry() -> Telemetry {
    Telemetry {
        arch: Arch::Wormhole,
        // 0.800 V, as millivolts.
        vcore: 800,
        // 12 A, low 16 bits of the TDC register.
        tdc: 12,
        // 45.0 degrees, as a 4-bit fraction.
        asic_temperature: 45 << 4,
        vreg_temperature: 45,
        // Inlet sits in the third byte of the board temperature register.
        board_temperature: 32 << 16,
        aiclk: 800,
        arcclk: 540,
        axiclk: 900,
        ..Default::default()
    }
}

fn detail_of(telem: &Telemetry) -> HashMap<String, String> {
    let static_info = DeviceStaticInfo::with_details(
        "Tenstorrent Wormhole n300".to_string(),
        Some("tt-0".to_string()),
        HashMap::new(),
    );
    let tenstorrent_info = TenstorrentStaticInfo {
        total_memory: 12 * 1024 * 1024 * 1024,
        tdp_limit: 300.0,
    };
    build_device_details(&static_info, &tenstorrent_info, telem)
}

fn device_with(detail: HashMap<String, String>) -> GpuInfo {
    GpuInfo {
        uuid: "tt-0".to_string(),
        time: "2026-09-20 10:00:00".to_string(),
        name: "Tenstorrent Wormhole n300".to_string(),
        device_type: "NPU".to_string(),
        host_id: "tt-node".to_string(),
        hostname: "tt-node".to_string(),
        instance: "tt-node".to_string(),
        utilization: 42.0,
        ane_utilization: 0.0,
        dla_utilization: None,
        tensorcore_utilization: None,
        temperature: 45,
        used_memory: 0,
        total_memory: 12 * 1024 * 1024 * 1024,
        frequency: 800,
        power_consumption: 120.0,
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
        detail,
    }
}

fn sample(exposition: &str, family: &str) -> Option<f64> {
    let prefix = format!("{family}{{");
    exposition
        .lines()
        .find(|line| line.starts_with(&prefix))
        .and_then(|line| line.rsplit(' ').next())
        .and_then(|value| value.parse().ok())
}

/// The reader writes the keys the exporter reads, holding bare numbers that
/// its strict `f64` parse accepts.
#[test]
fn dynamic_telemetry_uses_the_keys_the_exporter_reads() {
    let detail = detail_of(&wormhole_telemetry());

    for (key, expected) in [
        ("voltage", "0.800"),
        ("current", "12.00"),
        ("asic_temperature", "45.0"),
        ("vreg_temperature", "45.0"),
        ("inlet_temperature", "32.0"),
        ("aiclk_mhz", "800"),
        ("arcclk_mhz", "540"),
        ("axiclk_mhz", "900"),
    ] {
        assert_eq!(
            detail.get(key).map(String::as_str),
            Some(expected),
            "{key} is not the key/value shape the exporter parses"
        );
    }

    // The old Title Case spellings are gone, so nothing writes a unit-suffixed
    // string the exporter would reject and the label set would carry.
    for stale in [
        "VDD Voltage",
        "Current",
        "ASIC Temperature",
        "VR Temperature",
        "Inlet Temperature",
        "AI Clock",
        "ARC Clock",
        "AXI Clock",
    ] {
        assert!(!detail.contains_key(stale), "{stale} is still written");
    }
}

/// Every telemetry gauge the exporter declares now actually appears, with the
/// reading the chip reported. One assertion per family, so reverting a single
/// key's re-write fails on that family by name.
#[test]
fn telemetry_gauges_finally_reach_the_exposition() {
    let exposition =
        NpuMetricExporter::new(&[device_with(detail_of(&wormhole_telemetry()))]).export_metrics();

    for (family, expected) in [
        ("all_smi_tenstorrent_aiclk_mhz", 800.0),
        ("all_smi_tenstorrent_arcclk_mhz", 540.0),
        ("all_smi_tenstorrent_axiclk_mhz", 900.0),
        ("all_smi_tenstorrent_voltage_volts", 0.8),
        ("all_smi_tenstorrent_current_amperes", 12.0),
        ("all_smi_tenstorrent_asic_temperature_celsius", 45.0),
        ("all_smi_tenstorrent_vreg_temperature_celsius", 45.0),
        ("all_smi_tenstorrent_inlet_temperature_celsius", 32.0),
    ] {
        let value = sample(&exposition, family)
            .unwrap_or_else(|| panic!("{family} missing from the exposition:\n{exposition}"));
        assert!(
            (value - expected).abs() < 1e-9,
            "{family} reads {value}, expected {expected}"
        );
    }
}

/// The inlet sensor is optional, and the guard that skips it when the board
/// register reads zero survives the re-key.
#[test]
fn a_board_without_an_inlet_sensor_publishes_no_inlet_reading() {
    let telem = Telemetry {
        board_temperature: 0,
        ..wormhole_telemetry()
    };
    let detail = detail_of(&telem);
    assert!(!detail.contains_key("inlet_temperature"));

    let exposition = NpuMetricExporter::new(&[device_with(detail)]).export_metrics();
    assert!(
        !exposition.contains("all_smi_tenstorrent_inlet_temperature_celsius"),
        "an absent sensor must not publish 0 C:\n{exposition}"
    );
}

/// The readings stay out of the identity label set, which is what #425 is
/// about: eight labels per device used to churn on every poll.
#[test]
fn telemetry_never_reaches_the_identity_label_set() {
    use crate::api::metrics::gpu::GpuMetricExporter;

    let first = detail_of(&wormhole_telemetry());
    let second = detail_of(&Telemetry {
        vcore: 812,
        tdc: 13,
        asic_temperature: 47 << 4,
        vreg_temperature: 46,
        board_temperature: 33 << 16,
        aiclk: 1000,
        ..wormhole_telemetry()
    });
    assert_ne!(first, second, "the fixture polls must actually differ");

    let identity = |detail: HashMap<String, String>| -> String {
        GpuMetricExporter::new(&[device_with(detail)])
            .export_metrics()
            .lines()
            .find(|line| line.starts_with("all_smi_gpu_info{"))
            .expect("identity series")
            .to_string()
    };

    assert_eq!(
        identity(first),
        identity(second),
        "a telemetry poll moved the identity label set"
    );
}
