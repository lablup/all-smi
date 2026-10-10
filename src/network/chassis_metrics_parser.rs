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

//! Remote parser for the chassis metric families emitted by `all-smi api`.

use std::collections::HashMap;

use chrono::Local;

use crate::device::{ChassisInfo, FanInfo};
use crate::metrics::energy::MAX_POWER_WATTS;

const MAX_CHASSIS_PER_SCRAPE: usize = 256;
const MAX_FANS_PER_CHASSIS: usize = 256;
const MAX_FAN_RPM: f64 = 1_000_000.0;
const MAX_ABS_TEMPERATURE_CELSIUS: f64 = 1_000.0;

/// Per-scrape chassis accumulator. A scrape can theoretically carry more
/// than one chassis row, so entries are keyed by the exporter's stable
/// `(hostname, instance)` label pair rather than collapsed into one value.
pub(super) struct ChassisParseState {
    entries: HashMap<(String, String), ChassisInfo>,
}

impl ChassisParseState {
    pub(super) fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    pub(super) fn process(
        &mut self,
        metric_name: &str,
        labels: &HashMap<String, String>,
        value: f64,
        host: &str,
    ) {
        if !is_chassis_metric(metric_name) {
            return;
        }

        let hostname = labels
            .get("hostname")
            .or_else(|| labels.get("host"))
            .or_else(|| labels.get("instance"))
            .cloned()
            .unwrap_or_else(|| host.to_string());
        let instance = labels
            .get("instance")
            .cloned()
            .unwrap_or_else(|| hostname.clone());
        let key = (hostname.clone(), instance.clone());

        if !self.entries.contains_key(&key) && self.entries.len() >= MAX_CHASSIS_PER_SCRAPE {
            return;
        }

        let chassis = self.entries.entry(key).or_insert_with(|| ChassisInfo {
            host_id: host.to_string(),
            hostname,
            instance,
            time: Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            ..Default::default()
        });

        match metric_name {
            "chassis_info" => copy_identity_details(chassis, labels),
            "chassis_power_watts" if value.is_finite() && value >= 0.0 => {
                chassis.total_power_watts = Some(value.min(MAX_POWER_WATTS));
            }
            "chassis_thermal_pressure_info" => {
                if let Some(level) = labels.get("level").filter(|level| !level.is_empty()) {
                    chassis.thermal_pressure = Some(level.clone());
                }
            }
            "chassis_cpu_power_watts" if value.is_finite() && value >= 0.0 => {
                chassis.detail.insert(
                    "cpu_power_watts".to_string(),
                    value.min(MAX_POWER_WATTS).to_string(),
                );
            }
            "chassis_gpu_power_watts" if value.is_finite() && value >= 0.0 => {
                chassis.detail.insert(
                    "gpu_power_watts".to_string(),
                    value.min(MAX_POWER_WATTS).to_string(),
                );
            }
            "chassis_ane_power_watts" if value.is_finite() && value >= 0.0 => {
                chassis.detail.insert(
                    "ane_power_watts".to_string(),
                    value.min(MAX_POWER_WATTS).to_string(),
                );
            }
            "chassis_inlet_temperature_celsius"
                if value.is_finite() && value.abs() <= MAX_ABS_TEMPERATURE_CELSIUS =>
            {
                chassis.inlet_temperature = Some(value);
            }
            "chassis_outlet_temperature_celsius"
                if value.is_finite() && value.abs() <= MAX_ABS_TEMPERATURE_CELSIUS =>
            {
                chassis.outlet_temperature = Some(value);
            }
            "chassis_fan_speed_rpm"
                if value.is_finite()
                    && (0.0..=MAX_FAN_RPM).contains(&value)
                    && value.fract() == 0.0 =>
            {
                update_fan(chassis, labels, value as u32);
            }
            _ => {}
        }
    }

    pub(super) fn finish(self) -> Vec<ChassisInfo> {
        let mut chassis: Vec<_> = self.entries.into_values().collect();
        chassis.sort_by(|a, b| {
            a.hostname
                .cmp(&b.hostname)
                .then_with(|| a.instance.cmp(&b.instance))
        });
        chassis
    }
}

fn is_chassis_metric(metric_name: &str) -> bool {
    matches!(
        metric_name,
        "chassis_info"
            | "chassis_power_watts"
            | "chassis_thermal_pressure_info"
            | "chassis_cpu_power_watts"
            | "chassis_gpu_power_watts"
            | "chassis_ane_power_watts"
            | "chassis_inlet_temperature_celsius"
            | "chassis_outlet_temperature_celsius"
            | "chassis_fan_speed_rpm"
    )
}

fn copy_identity_details(chassis: &mut ChassisInfo, labels: &HashMap<String, String>) {
    for (label, detail_key) in [
        ("product_name", "Product Name"),
        ("vendor", "Vendor"),
        ("board", "Board"),
        ("version", "Version"),
        ("bios_version", "BIOS Version"),
        ("platform", "platform"),
    ] {
        if let Some(value) = labels.get(label).filter(|value| !value.is_empty()) {
            chassis.detail.insert(detail_key.to_string(), value.clone());
        }
    }
}

fn update_fan(chassis: &mut ChassisInfo, labels: &HashMap<String, String>, speed_rpm: u32) {
    let Some(id) = labels.get("fan_id").and_then(|id| id.parse::<u32>().ok()) else {
        return;
    };
    let name = labels
        .get("fan_name")
        .cloned()
        .unwrap_or_else(|| format!("Fan {id}"));

    if let Some(fan) = chassis.fan_speeds.iter_mut().find(|fan| fan.id == id) {
        fan.name = name;
        fan.speed_rpm = speed_rpm;
    } else if chassis.fan_speeds.len() < MAX_FANS_PER_CHASSIS {
        chassis.fan_speeds.push(FanInfo {
            id,
            name,
            speed_rpm,
            // The current exporter does not expose a maximum RPM series.
            max_rpm: 0,
        });
        chassis.fan_speeds.sort_by_key(|fan| fan.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(hostname: &str, instance: &str) -> HashMap<String, String> {
        HashMap::from([
            ("hostname".to_string(), hostname.to_string()),
            ("instance".to_string(), instance.to_string()),
        ])
    }

    #[test]
    fn invalid_readings_are_ignored_and_power_is_bounded() {
        let mut state = ChassisParseState::new();
        let mut metric_labels = labels("reported-host", "reported-instance");

        state.process(
            "chassis_power_watts",
            &metric_labels,
            MAX_POWER_WATTS * 2.0,
            "endpoint:9090",
        );
        state.process(
            "chassis_inlet_temperature_celsius",
            &metric_labels,
            f64::INFINITY,
            "endpoint:9090",
        );
        metric_labels.insert("fan_id".to_string(), "1".to_string());
        state.process(
            "chassis_fan_speed_rpm",
            &metric_labels,
            1_234.5,
            "endpoint:9090",
        );

        let chassis = state.finish().pop().expect("one chassis record");
        assert_eq!(chassis.host_id, "endpoint:9090");
        assert_eq!(chassis.total_power_watts, Some(MAX_POWER_WATTS));
        assert_eq!(chassis.inlet_temperature, None);
        assert!(chassis.fan_speeds.is_empty());
    }

    #[test]
    fn scrape_cardinality_limits_chassis_and_fans() {
        let mut state = ChassisParseState::new();

        for index in 0..=MAX_CHASSIS_PER_SCRAPE {
            state.process(
                "chassis_info",
                &labels(&format!("host-{index}"), &format!("instance-{index}")),
                1.0,
                "endpoint:9090",
            );
        }

        let mut chassis = state.finish();
        assert_eq!(chassis.len(), MAX_CHASSIS_PER_SCRAPE);

        let mut fan_state = ChassisParseState::new();
        for id in 0..=MAX_FANS_PER_CHASSIS {
            let mut metric_labels = labels("reported-host", "reported-instance");
            metric_labels.insert("fan_id".to_string(), id.to_string());
            fan_state.process(
                "chassis_fan_speed_rpm",
                &metric_labels,
                1_000.0,
                "endpoint:9090",
            );
        }

        chassis = fan_state.finish();
        assert_eq!(chassis[0].fan_speeds.len(), MAX_FANS_PER_CHASSIS);
    }
}
