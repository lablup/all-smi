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

//! AMD ADL sensor augmentation for the Windows GPU reader.
//!
//! The vendor-neutral DXGI and PDH layer added in #346 covers everything
//! WDDM publishes: VRAM size, utilization, per-process memory. It cannot
//! cover temperature, board power, fan speed, or clocks, because Windows
//! does not expose those at all. Those come from AMD's own library.
//!
//! `atiadlxx.dll` is loaded at runtime from an absolute System32 path,
//! so nothing is linked and a machine without AMD's driver simply gets
//! no ADL data. See [`loader`] for the loading and hijacking stance.
//!
//! ## Scope: modern parts only
//!
//! Sensors are read exclusively through `ADL2_New_QueryPMLogData_Get`,
//! the Overdrive 8 style sensor table that Vega and later expose. The
//! legacy Overdrive 5, 6, and 7 temperature entry points are
//! deliberately not implemented: they are three more ABI surfaces to get
//! right blind, for hardware that predates the datacentre and workstation
//! cards all-smi targets. A pre-Vega card keeps the WMI and PDH baseline.
//!
//! ## Scope: one AMD GPU
//!
//! ADL identifies adapters by an index that does not map to a card
//! without `AdapterInfo`, a 1568-byte struct whose layout this module
//! deliberately refuses to declare blind (see [`ffi`]). Worse, one card
//! exposes several adapter indices, one per display output, all
//! reporting identical telemetry, so an index cannot even be
//! deduplicated without it.
//!
//! Rather than guess, augmentation only runs when the reader found
//! exactly one AMD GPU. That covers the overwhelming majority of real
//! machines, and on a multi-AMD-GPU host the result is the honest DXGI
//! and PDH baseline instead of one card's temperature reported against
//! another. This mirrors the conclusion the #346 review reached about
//! adapter matching: declining to attribute beats attributing wrongly.
//!
//! ## Cost
//!
//! Nothing here submits GPU work; PMLog reads a telemetry block the
//! driver already maintains. The library load, context creation, and
//! capability scan each happen once for the process, so a steady-state
//! poll is a single call. Note that very aggressive sensor polling (at
//! the 100 ms rates desktop monitoring tools use) can hold an AMD GPU
//! out of its deepest idle power state; all-smi's intervals start at one
//! second, which is well clear of that.

pub mod ffi;
pub mod sensors;

#[cfg(target_os = "windows")]
pub mod loader;

use crate::device::readers::windows_gpu_perf::note_metrics_source;
use crate::device::types::GpuInfo;
use sensors::AdlReadout;

/// Whether an ADL readout can be attributed to a specific card.
///
/// See the module docs: without `AdapterInfo` an ADL adapter index
/// cannot be tied to a GPU, so attribution is only safe when there is
/// nothing to confuse it with.
pub fn can_attribute(amd_gpu_count: usize) -> bool {
    amd_gpu_count == 1
}

/// Layer an ADL readout onto a GPU that already carries the WMI, DXGI,
/// and PDH baseline.
///
/// ADL outranks the vendor-neutral layer: it reads the hardware's own
/// telemetry rather than the OS's accounting of it. Utilization is
/// therefore overwritten when ADL reports graphics activity, and
/// temperature, power, fan, and clocks are filled in for the first time.
///
/// Fields ADL did not produce are left exactly as they were, so a card
/// that publishes only some sensors keeps the baseline for the rest.
pub fn apply_to_gpu_info(gpu: &mut GpuInfo, readout: &AdlReadout) {
    if readout.is_empty() {
        return;
    }

    let mut applied: Vec<&str> = Vec::new();

    if let Some((temperature, source)) = readout.primary_temperature_c() {
        // `GpuInfo.temperature` is unsigned. A sub-zero die on a
        // cold-started machine is a real reading but cannot be
        // represented, so it floors at 0 and the true value stays
        // visible in the detail below.
        gpu.temperature = temperature.max(0) as u32;
        if temperature < 0 {
            // A sub-zero die is real on a cold-started machine but
            // cannot be represented in the unsigned field, which floors
            // at 0 above. Surface the true reading only in that case,
            // rather than adding a key that duplicates `temperature` on
            // every normal poll.
            gpu.detail
                .insert("Temperature".to_string(), format!("{temperature} C"));
        }
        // The label names which sensor was used. Edge, gfx, and hotspot
        // are not interchangeable (hotspot runs 15-30 C higher), and an
        // aggregated multi-host view would otherwise mix them with
        // nothing to tell them apart.
        gpu.detail
            .insert("Source: Temperature".to_string(), source.to_string());
        applied.push("temperature");
    }
    // Hotspot and memory temperatures have no dedicated `GpuInfo` field
    // but are the numbers that actually throttle a modern card, so they
    // are surfaced as details rather than dropped.
    if let Some(hotspot) = readout.temperature_hotspot_c {
        gpu.detail
            .insert("Hotspot Temperature".to_string(), format!("{hotspot} C"));
    }
    if let Some(memory) = readout.temperature_mem_c {
        gpu.detail
            .insert("Memory Temperature".to_string(), format!("{memory} C"));
    }

    if let Some(power) = readout.power_w {
        gpu.power_consumption = power;
        gpu.detail
            .insert("Source: Power".to_string(), "ADL".to_string());
        applied.push("power");
    }

    if let Some(clock) = readout.clock_gfx_mhz {
        gpu.frequency = clock;
        gpu.detail
            .insert("Source: Frequency".to_string(), "ADL".to_string());
        applied.push("clocks");
    }
    if let Some(clock) = readout.clock_mem_mhz {
        gpu.detail
            .insert("Memory Clock".to_string(), format!("{clock} MHz"));
    }

    if let Some(rpm) = readout.fan_rpm {
        // Written twice from one value: the typed field the TUI and the
        // Prometheus exporter read, and the `Fan Speed` detail string that
        // snapshots and the `contains_key("Fan Speed")` overwrite guard in
        // `intel_gpu_level_zero::apply_fan` still coordinate through. The
        // key and value format match `amd.rs`, `intel_gpu_linux`, and the
        // Level Zero reader exactly; a divergent key would sit outside that
        // guard.
        gpu.fan_speed_rpm = Some(rpm);
        gpu.detail
            .insert("Fan Speed".to_string(), format!("{rpm} RPM"));
        gpu.detail
            .insert("Source: Fan".to_string(), "ADL".to_string());
        applied.push("fan");
    }

    if let Some(activity) = readout.activity_gfx_pct {
        gpu.utilization = activity;
        gpu.detail
            .insert("Source: Utilization".to_string(), "ADL".to_string());
        applied.push("utilization");
    }
    if let Some(activity) = readout.activity_mem_pct {
        gpu.detail.insert(
            "Memory Controller Activity".to_string(),
            format!("{activity:.0}%"),
        );
    }

    note_metrics_source(&mut gpu.detail, "ADL");
    // Name only what ADL actually produced. A card publishing just one
    // sensor must not carry a Note claiming all four, which would
    // contradict the per-field `Source: *` keys sitting beside it.
    gpu.detail.insert(
        "Note".to_string(),
        format!("via AMD ADL (PMLog): {}", applied.join(", ")),
    );
}

/// Sample ADL and layer the result onto the reader's GPUs.
///
/// Called after the vendor-neutral layer so ADL's readings win. A no-op
/// when ADL is unavailable, when no adapter exposes PMLog, or when the
/// machine has more than one AMD GPU.
#[cfg(target_os = "windows")]
pub fn augment(gpus: &mut [GpuInfo]) {
    if !can_attribute(gpus.len()) {
        return;
    }
    let Some(output) = loader::sample() else {
        return;
    };
    let readout = sensors::extract(&output);
    apply_to_gpu_info(&mut gpus[0], &readout);
}

/// Non-Windows builds have no ADL. The stub keeps the surrounding logic
/// and its tests compiling everywhere.
#[cfg(not(target_os = "windows"))]
pub fn augment(_gpus: &mut [GpuInfo]) {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn baseline_gpu() -> GpuInfo {
        let mut detail = HashMap::new();
        // The state #346 leaves behind on a working Windows host.
        detail.insert("Metrics Source".to_string(), "WMI + DXGI + PDH".to_string());
        detail.insert("Source: Utilization".to_string(), "PDH".to_string());
        detail.insert("Source: Temperature".to_string(), "unavailable".to_string());
        detail.insert("Source: Power".to_string(), "unavailable".to_string());
        detail.insert("Source: Frequency".to_string(), "unavailable".to_string());
        detail.insert("Source: Fan".to_string(), "unavailable".to_string());
        detail.insert(
            "Note".to_string(),
            "Temperature, power, and fan need the AMD ADL library".to_string(),
        );
        GpuInfo {
            uuid: "PCI\\VEN_1002&DEV_744C".to_string(),
            time: String::new(),
            name: "AMD Radeon RX 7900 XTX".to_string(),
            device_type: "GPU".to_string(),
            host_id: String::new(),
            hostname: String::new(),
            instance: String::new(),
            utilization: 12.0,
            ane_utilization: 0.0,
            dla_utilization: None,
            tensorcore_utilization: None,
            temperature: 0,
            used_memory: 1_073_741_824,
            total_memory: 25_769_803_776,
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
            detail,
        }
    }

    fn full_readout() -> AdlReadout {
        AdlReadout {
            temperature_edge_c: Some(62),
            temperature_gfx_c: None,
            temperature_hotspot_c: Some(81),
            temperature_mem_c: Some(70),
            power_w: Some(310.0),
            fan_rpm: Some(1450),
            clock_gfx_mhz: Some(2400),
            clock_mem_mhz: Some(1250),
            activity_gfx_pct: Some(97.0),
            activity_mem_pct: Some(44.0),
        }
    }

    #[test]
    fn fills_in_everything_wddm_cannot_provide() {
        let mut gpu = baseline_gpu();
        apply_to_gpu_info(&mut gpu, &full_readout());

        assert_eq!(gpu.temperature, 62);
        assert_eq!(gpu.power_consumption, 310.0);
        assert_eq!(gpu.frequency, 2400);
        assert_eq!(gpu.detail["Hotspot Temperature"], "81 C");
        assert_eq!(gpu.detail["Memory Temperature"], "70 C");
        assert_eq!(gpu.fan_speed_rpm, Some(1450));
        assert_eq!(gpu.detail["Fan Speed"], "1450 RPM");
        assert_eq!(gpu.detail["Memory Clock"], "1250 MHz");
        assert_eq!(gpu.detail["Memory Controller Activity"], "44%");

        assert_eq!(gpu.detail["Source: Temperature"], "ADL (edge)");
        assert_eq!(gpu.detail["Source: Power"], "ADL");
        assert_eq!(gpu.detail["Source: Frequency"], "ADL");
        assert_eq!(gpu.detail["Source: Fan"], "ADL");
        assert_eq!(gpu.detail["Metrics Source"], "WMI + DXGI + PDH + ADL");
    }

    #[test]
    fn detail_keys_follow_the_shared_reader_convention() {
        // Every reader that publishes these quantities uses the same
        // key with the unit carried in the *value*, not the key:
        // `amd.rs` (Linux) writes `Fan Speed` = "1450 RPM" and
        // `Memory Clock` = "1250 MHz", and `intel_gpu_linux` and the
        // Level Zero reader match it.
        //
        // Two concrete costs of diverging, which is why this is locked
        // by a test rather than left to convention:
        //
        // 1. `intel_gpu_level_zero::apply_fan` guards an overwrite with
        //    `detail.contains_key("Fan Speed")`. A reader using a
        //    different key silently opts out of that coordination.
        // 2. The Prometheus exporter falls back to parsing this string
        //    when a payload predates `GpuInfo::fan_speed_rpm`, so a
        //    divergent spelling drops the metric for older nodes.
        let mut gpu = baseline_gpu();
        apply_to_gpu_info(&mut gpu, &full_readout());

        for (key, expected) in [
            ("Fan Speed", "1450 RPM"),
            ("Memory Clock", "1250 MHz"),
            ("Hotspot Temperature", "81 C"),
            ("Memory Temperature", "70 C"),
            ("Memory Controller Activity", "44%"),
        ] {
            assert_eq!(gpu.detail.get(key).map(String::as_str), Some(expected));
        }

        // The unit must not migrate back into the key.
        for stale in [
            "Fan Speed (RPM)",
            "Memory Clock (MHz)",
            "Hotspot Temperature (C)",
            "Memory Temperature (C)",
            "Memory Controller Activity (%)",
        ] {
            assert!(!gpu.detail.contains_key(stale), "{stale} should not exist");
        }
    }

    #[test]
    fn a_normal_temperature_adds_no_redundant_detail_key() {
        // `Temperature` exists only to preserve a sub-zero reading the
        // unsigned `GpuInfo.temperature` cannot hold. On every normal
        // poll it would just duplicate that field, so it is absent.
        let mut gpu = baseline_gpu();
        apply_to_gpu_info(&mut gpu, &full_readout());
        assert_eq!(gpu.temperature, 62);
        assert!(!gpu.detail.contains_key("Temperature"));
    }

    #[test]
    fn adl_utilization_outranks_the_pdh_figure() {
        // ADL reads the hardware's own activity counter; PDH reads the
        // OS's accounting. When both exist the vendor number wins.
        let mut gpu = baseline_gpu();
        assert_eq!(gpu.utilization, 12.0);
        apply_to_gpu_info(&mut gpu, &full_readout());
        assert_eq!(gpu.utilization, 97.0);
        assert_eq!(gpu.detail["Source: Utilization"], "ADL");
    }

    #[test]
    fn a_partial_readout_leaves_the_rest_of_the_baseline_alone() {
        // A card that publishes temperature but no activity counter must
        // not have its PDH utilization clobbered, nor be labelled as
        // sourcing utilization from ADL.
        let mut gpu = baseline_gpu();
        let readout = AdlReadout {
            temperature_edge_c: Some(55),
            ..Default::default()
        };
        apply_to_gpu_info(&mut gpu, &readout);

        assert_eq!(gpu.temperature, 55);
        assert_eq!(gpu.utilization, 12.0);
        assert_eq!(gpu.detail["Source: Utilization"], "PDH");
        assert_eq!(gpu.detail["Source: Power"], "unavailable");
        assert_eq!(gpu.power_consumption, 0.0);
        assert_eq!(gpu.detail["Metrics Source"], "WMI + DXGI + PDH + ADL");
    }

    #[test]
    fn the_note_names_only_the_fields_adl_actually_produced() {
        // A card publishing one sensor must not carry a Note claiming
        // all four, which would contradict the `Source: *` keys sitting
        // right beside it.
        let mut gpu = baseline_gpu();
        apply_to_gpu_info(
            &mut gpu,
            &AdlReadout {
                temperature_edge_c: Some(55),
                ..Default::default()
            },
        );
        assert_eq!(gpu.detail["Note"], "via AMD ADL (PMLog): temperature");
        assert_eq!(gpu.detail["Source: Power"], "unavailable");

        let mut full = baseline_gpu();
        apply_to_gpu_info(&mut full, &full_readout());
        assert_eq!(
            full.detail["Note"],
            "via AMD ADL (PMLog): temperature, power, clocks, fan, utilization"
        );
    }

    #[test]
    fn a_sub_zero_die_floors_the_unsigned_field_but_keeps_the_real_value() {
        let mut gpu = baseline_gpu();
        apply_to_gpu_info(
            &mut gpu,
            &AdlReadout {
                temperature_edge_c: Some(-8),
                ..Default::default()
            },
        );
        // `GpuInfo.temperature` is u32 and cannot hold it.
        assert_eq!(gpu.temperature, 0);
        // The true reading survives where it can be represented.
        assert_eq!(gpu.detail["Temperature"], "-8 C");
    }

    #[test]
    fn an_empty_readout_changes_nothing_at_all() {
        // A pre-Vega card, or one whose every sensor failed the range
        // guard, must leave no trace: claiming an ADL source for a
        // readout that produced nothing would be a lie in the output.
        let mut gpu = baseline_gpu();
        let before = gpu.clone();
        apply_to_gpu_info(&mut gpu, &AdlReadout::default());

        assert_eq!(gpu.temperature, before.temperature);
        assert_eq!(gpu.utilization, before.utilization);
        assert_eq!(gpu.detail["Metrics Source"], "WMI + DXGI + PDH");
        assert_eq!(gpu.detail["Source: Temperature"], "unavailable");
        assert!(!gpu.detail.contains_key("Hotspot Temperature"));
        // The typed field must stay unset too, so the exporter omits the
        // series rather than publishing a 0 RPM reading.
        assert!(gpu.fan_speed_rpm.is_none());
        assert!(!gpu.detail.contains_key("Fan Speed"));
    }

    #[test]
    fn applying_twice_does_not_grow_the_source_string() {
        let mut gpu = baseline_gpu();
        let readout = full_readout();
        apply_to_gpu_info(&mut gpu, &readout);
        apply_to_gpu_info(&mut gpu, &readout);
        assert_eq!(gpu.detail["Metrics Source"], "WMI + DXGI + PDH + ADL");
    }

    #[test]
    fn attribution_is_refused_for_anything_but_a_single_amd_gpu() {
        assert!(can_attribute(1));
        // Zero cards is nothing to attribute to; two or more cannot be
        // told apart without AdapterInfo.
        assert!(!can_attribute(0));
        assert!(!can_attribute(2));
        assert!(!can_attribute(4));
    }

    #[test]
    fn augment_is_inert_when_attribution_is_refused() {
        let mut gpus = vec![baseline_gpu(), baseline_gpu()];
        augment(&mut gpus);
        for gpu in &gpus {
            assert_eq!(gpu.detail["Metrics Source"], "WMI + DXGI + PDH");
            assert_eq!(gpu.temperature, 0);
        }

        // And on a non-Windows host it must be inert even for one GPU.
        #[cfg(not(target_os = "windows"))]
        {
            let mut single = vec![baseline_gpu()];
            augment(&mut single);
            assert_eq!(single[0].detail["Metrics Source"], "WMI + DXGI + PDH");
            assert_eq!(single[0].temperature, 0);
        }
    }
}
