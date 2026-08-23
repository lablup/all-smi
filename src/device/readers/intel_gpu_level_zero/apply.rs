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

use super::{LevelZeroFanReadout, LevelZeroMemoryKind, LevelZeroReadout};
use crate::device::readers::detail_keys::note_metrics_source;
use crate::device::types::{GpuInfo, MAX_GPU_FAN_RPM};

#[derive(Debug, Clone, Copy)]
pub enum ApplyPlatform {
    Linux,
    Windows,
}

pub fn apply_to_gpu_info(
    gpu_info: &mut GpuInfo,
    readout: &LevelZeroReadout,
    platform: ApplyPlatform,
) {
    if !readout.has_fresh_data() {
        return;
    }

    for (label, pct) in &readout.engines {
        gpu_info
            .detail
            .insert(format!("Engine: {label} (L0)"), format!("{pct:.2}%"));
    }
    if let Some(watts) = readout.power_watts {
        gpu_info
            .detail
            .insert("Power (L0)".to_string(), format!("{:.2} W", watts.value));
    }

    if let Some(temp) = readout.temperature_celsius {
        gpu_info.temperature = temp.value;
        set_source(gpu_info, "Temperature", temp.source);
    }
    if let Some(watts) = readout.power_watts {
        gpu_info.power_consumption = watts.value.clamp(0.0, 750.0);
        set_source(gpu_info, "Power", watts.source);
    }
    if let Some(memory) = readout.memory {
        match memory.kind {
            LevelZeroMemoryKind::DedicatedLocal
                if memory.total_bytes > 0
                    && !dxgi_resolved_a_shared_aperture(gpu_info, platform) =>
            {
                gpu_info.total_memory = memory.total_bytes;
                gpu_info.used_memory = memory.used_bytes.min(memory.total_bytes);
                set_source(gpu_info, "Memory", memory.source);
                gpu_info.detail.insert(
                    "VRAM Total".to_string(),
                    format!("{} bytes", memory.total_bytes),
                );
            }
            LevelZeroMemoryKind::DedicatedLocal => {
                // Either an empty readout, or an integrated part whose
                // dedicated pool is the small stolen carve-out that DXGI
                // already looked past. Overwriting here is how a 17.88 GiB
                // Arc B390 became a 128 MiB one (issue #364): Sysman
                // reports the carve-out perfectly correctly, it is just not
                // the capacity the device can address. Keep the number
                // visible without letting it replace the total.
                if memory.total_bytes > 0 {
                    gpu_info.detail.insert(
                        "VRAM Dedicated (L0)".to_string(),
                        format!("{} bytes", memory.total_bytes),
                    );
                }
            }
            LevelZeroMemoryKind::SharedSystem => {
                gpu_info.detail.insert(
                    "Memory (L0)".to_string(),
                    "Shared/system memory; dedicated VRAM budget unavailable".to_string(),
                );
            }
        }
    }
    if let Some(freq) = readout.frequency_mhz {
        gpu_info.frequency = freq.value;
        set_source(gpu_info, "Frequency", freq.source);
    }
    for (domain, mhz) in &readout.frequency_domains {
        gpu_info
            .detail
            .insert(format!("Frequency: {domain} (L0)"), format!("{mhz} MHz"));
    }

    match platform {
        ApplyPlatform::Linux => {
            if let Some(primary) = readout.primary_engine_utilization {
                gpu_info.utilization = primary.value.clamp(0.0, 100.0);
                set_source(gpu_info, "Utilization", primary.source);
                gpu_info.detail.remove("Utilization");
            }
            apply_fan(gpu_info, readout.fan, false);
            // Append rather than assign. Assigning erased whatever the
            // sysfs layer had recorded: a card whose kernel exposes engine
            // counters reports `"sysfs (engine counters)"`, and that
            // qualifier disappeared the moment Level Zero produced a
            // reading. Name the generic baseline only when no layer
            // claimed one, so the string still reads "sysfs + Level Zero
            // Sysman" on a card without engine counters.
            if !gpu_info.detail.contains_key("Metrics Source") {
                note_metrics_source(&mut gpu_info.detail, "sysfs");
            }
            note_metrics_source(&mut gpu_info.detail, "Level Zero Sysman");
        }
        ApplyPlatform::Windows => {
            if let Some(primary) = readout.primary_engine_utilization {
                gpu_info.utilization = primary.value.clamp(0.0, 100.0);
                set_source(gpu_info, "Utilization", primary.source);
            }
            apply_fan(gpu_info, readout.fan, true);
            // Appending is the whole point: assigning here erased the DXGI
            // and PDH contributions that ran before this layer, so a host
            // with the full stack reported "WMI + Level Zero Sysman" and
            // hid where its memory and utilization figures came from.
            note_metrics_source(&mut gpu_info.detail, "Level Zero Sysman");
        }
    }
}

/// Whether the vendor-neutral DXGI layer already resolved this adapter's
/// capacity to a shared aperture rather than a dedicated pool.
///
/// `windows_gpu_perf::apply_to_gpu_info` writes `"DXGI (shared)"` exactly
/// when it took that branch, which makes the detail map the handoff between
/// two layers that cannot see each other: this one is compiled on Linux too
/// and must not reach into a Windows-only module.
///
/// Linux never reaches this. There is no DXGI, and the sysfs reader
/// publishes a real dedicated pool for the parts that have one.
fn dxgi_resolved_a_shared_aperture(gpu_info: &GpuInfo, platform: ApplyPlatform) -> bool {
    matches!(platform, ApplyPlatform::Windows)
        && gpu_info
            .detail
            .get("Source: Memory")
            .is_some_and(|source| source.contains("shared"))
}

fn set_source(gpu_info: &mut GpuInfo, field: &str, source: &str) {
    gpu_info
        .detail
        .insert(format!("Source: {field}"), source.to_string());
}

fn apply_fan(gpu_info: &mut GpuInfo, fan: Option<LevelZeroFanReadout>, overwrite_existing: bool) {
    let Some(fan) = fan else {
        return;
    };
    if !overwrite_existing && gpu_info.detail.contains_key("Fan Speed") {
        return;
    }
    // Clamped so a garbled Sysman sample can never propagate `u32::MAX`
    // into the exporter or the TUI; see the sysfs readers for the same
    // defence-in-depth pattern and `MAX_GPU_FAN_RPM` for the shared bound.
    let rpm = fan.rpm.map(|rpm| rpm.min(MAX_GPU_FAN_RPM));
    let value = match (rpm, fan.percent) {
        (Some(rpm), Some(percent)) => format!("{rpm} RPM ({percent}%)"),
        (Some(rpm), None) => format!("{rpm} RPM"),
        (None, Some(percent)) => format!("{percent}%"),
        (None, None) => return,
    };
    gpu_info.detail.insert("Fan Speed".to_string(), value);
    // The typed field only ever carries a tachometer reading. A
    // duty-cycle-only readout (`rpm == None`) clears it rather than storing
    // a percentage in a field named `_rpm`; the percentage still reaches
    // snapshots through the detail string above. The assignment is
    // unconditional precisely so the field cannot keep an RPM from an
    // earlier sample that the detail string just replaced on the
    // `overwrite_existing` path, which would leave the two describing
    // different samples and the exporter publishing the stale number.
    gpu_info.fan_speed_rpm = rpm;
    set_source(gpu_info, "Fan", fan.source);
}
