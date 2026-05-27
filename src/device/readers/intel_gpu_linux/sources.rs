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

use crate::device::readers::intel_gpu_engine::{ENGINE_SEEDING_NOTE, EngineReadout};
use std::collections::HashMap;

pub(super) fn decorate_static_sources(
    detail: &mut HashMap<String, String>,
    total_memory: u64,
    temperature: u32,
    power_consumption: f64,
    frequency: u32,
    fan_rpm: Option<u32>,
) {
    if total_memory > 0 {
        detail.insert("VRAM Total".to_string(), format!("{total_memory} bytes"));
    }
    set_source(
        detail,
        "Memory",
        if total_memory > 0 {
            "DRM sysfs"
        } else {
            "unavailable/shared system memory"
        },
    );
    set_source(
        detail,
        "Temperature",
        if temperature > 0 {
            "hwmon"
        } else {
            "unavailable"
        },
    );
    set_source(
        detail,
        "Power",
        if power_consumption > 0.0 {
            "hwmon"
        } else {
            "unavailable"
        },
    );
    set_source(
        detail,
        "Frequency",
        if frequency > 0 {
            "DRM sysfs"
        } else {
            "unavailable"
        },
    );
    if let Some(rpm) = fan_rpm {
        detail.insert("Fan Speed".to_string(), format!("{rpm} RPM"));
        set_source(detail, "Fan", "hwmon");
    } else {
        set_source(detail, "Fan", "unavailable");
    }
}

pub(super) fn decorate_utilization_source(
    detail: &mut HashMap<String, String>,
    readout: &EngineReadout,
) {
    set_source(
        detail,
        "Utilization",
        if readout.status_note.is_none() && !readout.per_class.is_empty() {
            "DRM engine counters"
        } else if readout.status_note == Some(ENGINE_SEEDING_NOTE) {
            "DRM engine counters (seeded)"
        } else {
            "unavailable"
        },
    );
}

fn set_source(detail: &mut HashMap<String, String>, field: &str, source: &str) {
    detail.insert(format!("Source: {field}"), source.to_string());
}
