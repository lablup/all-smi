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
use crate::device::types::GpuInfo;

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
            LevelZeroMemoryKind::DedicatedLocal => {
                gpu_info.total_memory = memory.total_bytes;
                gpu_info.used_memory = memory.used_bytes.min(memory.total_bytes);
                set_source(gpu_info, "Memory", memory.source);
                gpu_info.detail.insert(
                    "VRAM Total".to_string(),
                    format!("{} bytes", memory.total_bytes),
                );
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
            gpu_info.detail.insert(
                "Metrics Source".to_string(),
                "sysfs + Level Zero Sysman".to_string(),
            );
        }
        ApplyPlatform::Windows => {
            if let Some(primary) = readout.primary_engine_utilization {
                gpu_info.utilization = primary.value.clamp(0.0, 100.0);
                set_source(gpu_info, "Utilization", primary.source);
            }
            apply_fan(gpu_info, readout.fan, true);
            gpu_info.detail.insert(
                "Metrics Source".to_string(),
                "WMI + Level Zero Sysman".to_string(),
            );
        }
    }
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
    let value = match (fan.rpm, fan.percent) {
        (Some(rpm), Some(percent)) => format!("{rpm} RPM ({percent}%)"),
        (Some(rpm), None) => format!("{rpm} RPM"),
        (None, Some(percent)) => format!("{percent}%"),
        (None, None) => return,
    };
    gpu_info.detail.insert("Fan Speed".to_string(), value);
    set_source(gpu_info, "Fan", fan.source);
}
