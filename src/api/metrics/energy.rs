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

//! Prometheus exporter for energy counters (issue #191).
//!
//! Exports a single counter family,
//! `all_smi_energy_consumed_joules_total`, with two label shapes:
//!
//! ```text
//! # TYPE all_smi_energy_consumed_joules_total counter
//! all_smi_energy_consumed_joules_total{host="dgx-01",scope="gpu",gpu_index="0",gpu_uuid="GPU-..."} 8.43e6
//! all_smi_energy_consumed_joules_total{host="dgx-01",scope="chassis"} 6.13e7
//! all_smi_energy_consumed_joules_total{host="dgx-01",scope="cpu"} 1.02e6
//! ```
//!
//! The counter reflects the integrator's *lifetime* value (WAL seed +
//! live samples), so it stays monotonic across `R` session resets.
//!
//! Devices that have never reported power produce no line — callers
//! should rely on metric *absence* rather than zero to detect
//! "unsupported on this host", per Prometheus convention.

use super::{MetricBuilder, MetricExporter};
use crate::device::GpuInfo;
use crate::metrics::energy::{EnergyScope, PowerIntegrator};

/// Exporter for the `all_smi_energy_consumed_joules_total` counter.
///
/// Holds a reference to the integrator plus the current `gpu_info`
/// slice so we can attach the stable `gpu_index` label to per-GPU
/// rows. The index is the position of the GPU within the current
/// scrape's `gpu_info`, matching [`crate::api::metrics::gpu`].
pub struct EnergyMetricExporter<'a> {
    integrator: &'a PowerIntegrator,
    gpu_info: &'a [GpuInfo],
}

impl<'a> EnergyMetricExporter<'a> {
    pub fn new(integrator: &'a PowerIntegrator, gpu_info: &'a [GpuInfo]) -> Self {
        Self {
            integrator,
            gpu_info,
        }
    }
}

impl<'a> MetricExporter for EnergyMetricExporter<'a> {
    fn export_metrics(&self) -> String {
        let mut builder = MetricBuilder::new();

        // Collect stats eagerly so we can detect "nothing to emit" and
        // skip the HELP / TYPE header entirely. An empty metric family
        // pollutes the exposition without buying anything.
        let stats: Vec<_> = self.integrator.iter_stats().collect();
        if stats.is_empty() {
            return builder.build();
        }

        builder
            .help(
                "all_smi_energy_consumed_joules_total",
                "Cumulative energy consumption in Joules.",
            )
            .type_("all_smi_energy_consumed_joules_total", "counter");

        for stat in stats {
            // Skip entries with zero lifetime and no active samples —
            // this covers WAL placeholder seeds that never resolved
            // into a live device during this session.
            if stat.lifetime_joules <= 0.0 {
                continue;
            }
            match stat.key.scope {
                EnergyScope::Gpu => {
                    // Look up the matching GPU to attach `gpu_index` so
                    // dashboards can correlate with
                    // `all_smi_gpu_power_consumption_watts`. The lookup
                    // is linear but `gpu_info` stays small (single-
                    // digit on local hosts, low-thousands on the
                    // largest clusters); O(n²) over scrape cadence is
                    // still trivial compared to network.
                    let (host, uuid) = (stat.key.host.as_str(), stat.key.device.as_str());
                    let gpu_index = self
                        .gpu_info
                        .iter()
                        .position(|g| g.hostname == host && g.uuid == uuid)
                        .map(|i| i.to_string())
                        .unwrap_or_else(|| "0".to_string());
                    let labels = [
                        ("host", host),
                        ("scope", "gpu"),
                        ("gpu_index", gpu_index.as_str()),
                        ("gpu_uuid", uuid),
                    ];
                    builder.metric(
                        "all_smi_energy_consumed_joules_total",
                        &labels,
                        format!("{:.3}", stat.lifetime_joules),
                    );
                }
                EnergyScope::Cpu => {
                    let labels = [("host", stat.key.host.as_str()), ("scope", "cpu")];
                    builder.metric(
                        "all_smi_energy_consumed_joules_total",
                        &labels,
                        format!("{:.3}", stat.lifetime_joules),
                    );
                }
                EnergyScope::Chassis => {
                    let labels = [("host", stat.key.host.as_str()), ("scope", "chassis")];
                    builder.metric(
                        "all_smi_energy_consumed_joules_total",
                        &labels,
                        format!("{:.3}", stat.lifetime_joules),
                    );
                }
            }
        }

        builder.build()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::energy::EnergyKey;
    use std::collections::HashMap;
    use std::time::{Duration, Instant};

    fn make_gpu(hostname: &str, uuid: &str) -> GpuInfo {
        GpuInfo {
            uuid: uuid.to_string(),
            time: String::new(),
            name: "Mock GPU".to_string(),
            device_type: "GPU".to_string(),
            host_id: hostname.to_string(),
            hostname: hostname.to_string(),
            instance: hostname.to_string(),
            utilization: 0.0,
            ane_utilization: 0.0,
            dla_utilization: None,
            tensorcore_utilization: None,
            temperature: 0,
            used_memory: 0,
            total_memory: 0,
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
    fn empty_integrator_emits_nothing() {
        let integ = PowerIntegrator::default();
        let out = EnergyMetricExporter::new(&integ, &[]).export_metrics();
        assert!(out.is_empty(), "expected empty output, got:\n{out}");
    }

    #[test]
    fn emits_gpu_and_chassis_rows_with_expected_labels() {
        let mut integ = PowerIntegrator::default();
        let origin = Instant::now();
        let gpu_key = EnergyKey::gpu("dgx-01", "GPU-AAA");
        integ.record_sample(gpu_key.clone(), origin, 300.0);
        integ.record_sample(gpu_key.clone(), origin + Duration::from_secs(10), 300.0);

        let chassis_key = EnergyKey::chassis("dgx-01");
        integ.record_sample(chassis_key.clone(), origin, 450.0);
        integ.record_sample(chassis_key.clone(), origin + Duration::from_secs(10), 450.0);

        let gpus = vec![make_gpu("dgx-01", "GPU-AAA")];
        let out = EnergyMetricExporter::new(&integ, &gpus).export_metrics();

        assert!(
            out.contains("# TYPE all_smi_energy_consumed_joules_total counter"),
            "exposition header missing:\n{out}"
        );
        assert!(out.contains(r#"scope="gpu""#), "gpu row missing:\n{out}");
        assert!(
            out.contains(r#"gpu_uuid="GPU-AAA""#),
            "gpu_uuid label missing:\n{out}"
        );
        assert!(
            out.contains(r#"gpu_index="0""#),
            "gpu_index label missing:\n{out}"
        );
        assert!(
            out.contains(r#"scope="chassis""#),
            "chassis row missing:\n{out}"
        );
    }

    /// Counter is monotonic across scrapes: re-rendering after more
    /// samples must produce a value that is not lower.
    #[test]
    fn counter_is_monotonic_across_scrapes() {
        let mut integ = PowerIntegrator::default();
        let origin = Instant::now();
        let key = EnergyKey::gpu("host", "uuid");
        integ.record_sample(key.clone(), origin, 100.0);
        integ.record_sample(key.clone(), origin + Duration::from_secs(10), 100.0);

        let gpus = vec![make_gpu("host", "uuid")];
        let scrape1 = EnergyMetricExporter::new(&integ, &gpus).export_metrics();

        integ.record_sample(key.clone(), origin + Duration::from_secs(20), 100.0);
        let scrape2 = EnergyMetricExporter::new(&integ, &gpus).export_metrics();

        // Extract numeric value from each scrape and compare.
        fn last_value(exposition: &str) -> f64 {
            exposition
                .lines()
                .filter(|l| l.starts_with("all_smi_energy_consumed_joules_total"))
                .filter_map(|l| l.rsplit(' ').next())
                .filter_map(|s| s.parse::<f64>().ok())
                .next_back()
                .expect("expected at least one counter line")
        }
        assert!(
            last_value(&scrape2) >= last_value(&scrape1),
            "counter regressed across scrapes: {} -> {}",
            last_value(&scrape1),
            last_value(&scrape2)
        );
    }

    /// A session reset (the `R` hotkey) must NOT rewind the exported
    /// counter — the exporter reports lifetime joules, not session.
    #[test]
    fn session_reset_does_not_rewind_exported_counter() {
        let mut integ = PowerIntegrator::default();
        let origin = Instant::now();
        let key = EnergyKey::gpu("host", "uuid");
        integ.record_sample(key.clone(), origin, 100.0);
        integ.record_sample(key.clone(), origin + Duration::from_secs(10), 100.0);
        let lifetime_before = integ.lifetime_joules(&key);
        integ.reset_session();
        assert_eq!(integ.session_joules(&key), 0.0);
        assert!((integ.lifetime_joules(&key) - lifetime_before).abs() < 1e-9);
    }
}
