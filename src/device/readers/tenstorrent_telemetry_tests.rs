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

//! The reader's dynamic telemetry, end to end into the exposition (#425,
//! #430).
//!
//! `api::metrics::npu::tenstorrent` has always declared the
//! `all_smi_tenstorrent_*` telemetry gauges, and `API.md` has always
//! documented them, but none of them could fire: the exporter looks up
//! snake_case keys holding bare numbers, and the reader wrote Title Case
//! keys holding unit-suffixed strings (`"AI Clock"` => `"800MHz"`). The only
//! place a Tenstorrent clock, voltage or current reached the wire was as an
//! `all_smi_gpu_info` label that changed on nearly every scrape. #425
//! re-keyed the churning readings; #430 extends the map to the counters,
//! status registers and limits luwen already carried, re-keys the static
//! half to the same convention, and deletes the exporters of values no
//! source ever provided.
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
        // Health and status registers, as the #430 exporter sites read them.
        faults: 0x0000_00a5,
        throttler: 1,
        ddr_status: 0x0000_1234,
        pcie_status: 7,
        eth_status0: 0x11,
        eth_status1: 0x22,
        arc0_health: 1234,
        arc3_health: 5678,
        // The raw heartbeat counter, deliberately distinct from
        // `arc0_health` so the test can tell the two sources apart.
        timer_heartbeat: 42,
        fan_speed: 60,
        fan_rpm: 6000,
        tdp: 300,
        thm_limits: 90,
        ddr_speed: Some(1200),
        ..Default::default()
    }
}

/// The reader's static half, hand-built from the snake_case keys
/// `extract_static_info` writes. `extract_static_info` needs a real `Chip`
/// and cannot be called from a test, so this map is what holds the test in
/// step with the reader; the on-card scrape in `API.md` is the backstop.
fn static_detail() -> HashMap<String, String> {
    DetailBuilder::new()
        .insert("board_type", "n300")
        .insert("board_id", "001a2b3c4d5e6f70")
        .insert("arc_fw_version", "6.9.0")
        .insert("eth_fw_version", "6.9.0")
        .insert("fw_date", "2026-08-01")
        .insert("ddr_fw_version", "1.2.3")
        .insert("spibootrom_fw_version", "1.0.0")
        .insert("pcie_address", "0000:03:00.0")
        .insert("pcie_vendor_id", "0x1e52")
        .insert("pcie_device_id", "0xb140")
        .insert("pci_bus_id", "0000:03:00.0")
        .insert("pcie_link_gen", "4")
        .insert("pcie_link_width", "16")
        .build()
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

/// A device whose static detail is the reader's new snake_case map, so the
/// board, firmware and PCIe exporter sites all fire.
fn full_device() -> GpuInfo {
    let static_info = DeviceStaticInfo::with_details(
        "Tenstorrent Wormhole n300".to_string(),
        Some("tt-0".to_string()),
        static_detail(),
    );
    let tenstorrent_info = TenstorrentStaticInfo {
        total_memory: 12 * 1024 * 1024 * 1024,
        tdp_limit: 300.0,
    };
    let detail = build_device_details(&static_info, &tenstorrent_info, &wormhole_telemetry());
    device_with(detail)
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
/// about: eight labels per device used to churn on every poll. The #430
/// registers (`faults`, `throttler`, the health counters, the statuses, the
/// fan readings, `heartbeat`) stay out of it too, and the only label name
/// that moves is `pcie_generation` becoming `pcie_link_gen`.
#[test]
fn telemetry_never_reaches_the_identity_label_set() {
    use crate::api::metrics::gpu::GpuMetricExporter;

    let first = detail_of(&wormhole_telemetry());
    let second = detail_of(&Telemetry {
        vcore: 812,
        asic_temperature: 47 << 4,
        vreg_temperature: 46,
        board_temperature: 33 << 16,
        aiclk: 1000,
        arcclk: 545,
        axiclk: 905,
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

/// The live #430 registers keep their own series and never appear as
/// `all_smi_gpu_info` labels, and the reader's static re-key moves the PCIe
/// labels exactly as intended: `pcie_generation` becomes `pcie_link_gen`,
/// and the width value loses its old `x` prefix while the label name stays.
#[test]
fn live_registers_stay_out_of_the_identity_label_set() {
    use crate::api::metrics::gpu::GpuMetricExporter;

    let line = GpuMetricExporter::new(&[full_device()])
        .export_metrics()
        .lines()
        .find(|line| line.starts_with("all_smi_gpu_info{"))
        .expect("identity series")
        .to_string();

    for label in [
        "faults=",
        "throttler=",
        "heartbeat=",
        "arc0_health=",
        "arc3_health=",
        "fan_speed=",
        "fan_rpm=",
        "ddr_status=",
        "pcie_status=",
    ] {
        assert!(
            !line.contains(label),
            "the identity series carries {label}:\n{line}"
        );
    }

    assert!(
        line.contains("pcie_link_gen=\"4\""),
        "the re-keyed generation label is missing:\n{line}"
    );
    assert!(
        !line.contains("pcie_generation="),
        "the old generation label is still written:\n{line}"
    );
    assert!(
        line.contains("pcie_link_width=\"16\""),
        "the width label must hold a bare lane count:\n{line}"
    );
    assert!(
        !line.contains("pcie_link_width=\"x16\""),
        "the width value still carries its old x prefix:\n{line}"
    );
}

/// The static and register families the reader now populates all reach the
/// exposition through the real pipeline. One assertion per family, so
/// reverting a single key or export block fails on that family by name.
#[test]
fn static_and_register_families_reach_the_exposition() {
    let exposition = NpuMetricExporter::new(&[full_device()]).export_metrics();

    // Board identity and firmware, from the re-keyed static keys.
    assert!(
        exposition.contains("all_smi_tenstorrent_board_info{"),
        "board info missing from the exposition:\n{exposition}"
    );
    assert!(
        exposition.contains("all_smi_tenstorrent_arc_firmware_info{"),
        "ARC firmware info missing from the exposition:\n{exposition}"
    );

    // Registers and counters, in the shape each exporter site parses.
    for (family, expected) in [
        // 0x000000a5 = 165: proves the written hex form is one
        // `parse_hex_register` accepts.
        ("all_smi_tenstorrent_faults", 165.0),
        ("all_smi_tenstorrent_throttler", 1.0),
        ("all_smi_tenstorrent_arc0_health", 1234.0),
        ("all_smi_tenstorrent_heartbeat", 42.0),
        ("all_smi_tenstorrent_fan_rpm", 6000.0),
        ("all_smi_tenstorrent_tdp_limit_watts", 300.0),
        ("all_smi_tenstorrent_pcie_generation", 4.0),
        ("all_smi_tenstorrent_pcie_width", 16.0),
    ] {
        let value = sample(&exposition, family)
            .unwrap_or_else(|| panic!("{family} missing from the exposition:\n{exposition}"));
        assert!(
            (value - expected).abs() < 1e-9,
            "{family} reads {value}, expected {expected}"
        );
    }
}

/// The series whose export blocks were deleted stay gone, both as samples
/// and as `# HELP` declarations.
#[test]
fn deleted_series_are_declared_nowhere() {
    let exposition = NpuMetricExporter::new(&[full_device()]).export_metrics();

    for family in [
        "all_smi_tenstorrent_outlet1_temperature_celsius",
        "all_smi_tenstorrent_outlet2_temperature_celsius",
        "all_smi_tenstorrent_power_limit_tdp_watts",
        "all_smi_tenstorrent_power_limit_tdc_amperes",
        "all_smi_tenstorrent_power_raw_watts",
        "all_smi_tenstorrent_collection_method_info",
    ] {
        assert!(
            !exposition
                .lines()
                .any(|line| line.starts_with(&format!("{family}{{"))),
            "{family} still has a sample:\n{exposition}"
        );
        assert!(
            !exposition.contains(&format!("# HELP {family} ")),
            "{family} is still declared:\n{exposition}"
        );
    }
}

/// A board without the DRAM speed sensor carries no `dram_speed` key, so
/// the DRAM info series is omitted rather than exported with a `0` speed.
#[test]
fn a_board_without_a_dram_speed_sensor_publishes_no_dram_info() {
    let static_info = DeviceStaticInfo::with_details(
        "Tenstorrent Wormhole n300".to_string(),
        Some("tt-0".to_string()),
        static_detail(),
    );
    let tenstorrent_info = TenstorrentStaticInfo {
        total_memory: 12 * 1024 * 1024 * 1024,
        tdp_limit: 300.0,
    };
    let telem = Telemetry {
        ddr_speed: None,
        ..wormhole_telemetry()
    };
    let detail = build_device_details(&static_info, &tenstorrent_info, &telem);
    assert!(!detail.contains_key("dram_speed"));

    let exposition = NpuMetricExporter::new(&[device_with(detail)]).export_metrics();
    assert!(
        !exposition.contains("all_smi_tenstorrent_dram_info"),
        "an absent sensor must not publish a DRAM speed:\n{exposition}"
    );
}
