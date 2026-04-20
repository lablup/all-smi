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

use std::time::Instant;

use crate::app_state::AppState;
use crate::common::config::AppConfig;
use crate::metrics::energy::EnergyKey;

/// Aggregates data from multiple sources and manages history tracking
pub struct DataAggregator;

impl DataAggregator {
    pub fn new() -> Self {
        Self
    }

    /// Update utilization history for all metrics
    pub fn update_utilization_history(&self, state: &mut AppState) {
        // Always collect CPU statistics if available
        self.update_cpu_history(state);

        // Update GPU history if we have GPU data OR if we're on Apple Silicon
        self.update_gpu_history(state);
    }

    /// Feed the current `gpu_info`, `cpu_info`, and `chassis_info`
    /// samples into the energy integrator (issue #191).
    ///
    /// Devices that do not report power are silently skipped — the
    /// integrator's "has this device ever been seen?" check is what
    /// drives the Prometheus exporter's "no data → omit metric"
    /// behavior, so this preserves the contract that a zero counter
    /// is distinct from a missing counter.
    ///
    /// On the first sample for each `(host, device)` pair the method
    /// consults `state.energy_wal_replay`, and if the pair was
    /// previously written to the WAL, seeds the integrator's
    /// lifetime counter with the replayed value. This preserves
    /// Prometheus counter monotonicity across restarts.
    pub fn update_energy_counters(&self, state: &mut AppState) {
        let now = Instant::now();

        // Collect `(key, watts)` pairs first so we do not hold an
        // immutable borrow over `state.*_info` while taking the
        // mutable borrow on `state.energy`.
        let mut samples: Vec<(EnergyKey, f64)> = Vec::with_capacity(
            state.gpu_info.len() + state.cpu_info.len() + state.chassis_info.len(),
        );

        // Per-GPU samples. The `hostname` field is the label that ends
        // up on the Prometheus metric and the TUI top-consumer view,
        // so we use it as the host component of the key (same as
        // other per-GPU exporters).
        for gpu in &state.gpu_info {
            let key = EnergyKey::gpu(gpu.hostname.clone(), gpu.uuid.clone());
            samples.push((key, gpu.power_consumption));
        }

        // Per-CPU samples (Apple Silicon, some Intel/AMD chipsets).
        for cpu in &state.cpu_info {
            if let Some(power) = cpu.power_consumption {
                let key = EnergyKey::cpu(cpu.hostname.clone());
                samples.push((key, power));
            }
        }

        // Per-chassis samples.
        for chassis in &state.chassis_info {
            if let Some(power) = chassis.total_power_watts {
                let key = EnergyKey::chassis(chassis.hostname.clone());
                samples.push((key, power));
            }
        }

        // First-sample WAL seeding. The index is populated by
        // `replay_from_path` at startup and shrinks as matches are
        // applied, so this runs in amortized O(1) per device.
        let wal_index = &mut state.energy_wal_replay;
        let integrator = state.energy.integrator_mut();
        for (key, watts) in samples {
            if !integrator.has_samples(&key) && !wal_index.is_empty() {
                wal_index.seed_if_matches(&key, integrator);
            }
            integrator.record_sample(key, now, watts);
        }
    }

    fn update_cpu_history(&self, state: &mut AppState) {
        if state.cpu_info.is_empty() {
            return;
        }

        let avg_cpu_utilization = state
            .cpu_info
            .iter()
            .map(|cpu| cpu.utilization)
            .sum::<f64>()
            / state.cpu_info.len() as f64;

        let avg_system_memory_usage = if !state.memory_info.is_empty() {
            state
                .memory_info
                .iter()
                .map(|mem| {
                    if mem.total_bytes > 0 {
                        (mem.used_bytes as f64 / mem.total_bytes as f64) * 100.0
                    } else {
                        0.0
                    }
                })
                .sum::<f64>()
                / state.memory_info.len() as f64
        } else {
            0.0
        };

        let cpu_temps: Vec<f64> = state
            .cpu_info
            .iter()
            .filter_map(|cpu| cpu.temperature.map(|t| t as f64))
            .collect();
        let avg_cpu_temperature = if !cpu_temps.is_empty() {
            cpu_temps.iter().sum::<f64>() / cpu_temps.len() as f64
        } else {
            0.0
        };

        state.cpu_utilization_history.push_back(avg_cpu_utilization);
        state
            .system_memory_history
            .push_back(avg_system_memory_usage);
        state.cpu_temperature_history.push_back(avg_cpu_temperature);

        // Keep only last N entries
        if state.cpu_utilization_history.len() > AppConfig::HISTORY_MAX_ENTRIES {
            state.cpu_utilization_history.pop_front();
        }
        if state.system_memory_history.len() > AppConfig::HISTORY_MAX_ENTRIES {
            state.system_memory_history.pop_front();
        }
        if state.cpu_temperature_history.len() > AppConfig::HISTORY_MAX_ENTRIES {
            state.cpu_temperature_history.pop_front();
        }
    }

    fn update_gpu_history(&self, state: &mut AppState) {
        let has_gpu_data = !state.gpu_info.is_empty();
        let is_apple_silicon = state.gpu_info.iter().any(|gpu| {
            gpu.detail
                .get("architecture")
                .map(|arch| arch == "Apple Silicon")
                .unwrap_or(false)
        });

        if has_gpu_data
            && (state.gpu_info.iter().any(|gpu| gpu.total_memory > 0) || is_apple_silicon)
        {
            let avg_utilization = state
                .gpu_info
                .iter()
                .map(|gpu| gpu.utilization)
                .sum::<f64>()
                / state.gpu_info.len() as f64;

            let avg_memory = state
                .gpu_info
                .iter()
                .map(|gpu| {
                    if gpu.total_memory > 0 {
                        (gpu.used_memory as f64 / gpu.total_memory as f64) * 100.0
                    } else {
                        0.0
                    }
                })
                .sum::<f64>()
                / state.gpu_info.len() as f64;

            let avg_temperature = state
                .gpu_info
                .iter()
                .map(|gpu| gpu.temperature as f64)
                .sum::<f64>()
                / state.gpu_info.len() as f64;

            state.utilization_history.push_back(avg_utilization);
            state.memory_history.push_back(avg_memory);
            state.temperature_history.push_back(avg_temperature);
            state
                .package_power_history
                .push_back(current_package_power_watts(state));

            if detect_apple_silicon(state) {
                state
                    .ane_power_history
                    .push_back(current_ane_power_watts(state));
            }

            // Keep only last N entries as configured
            if state.utilization_history.len() > AppConfig::HISTORY_MAX_ENTRIES {
                state.utilization_history.pop_front();
            }
            if state.memory_history.len() > AppConfig::HISTORY_MAX_ENTRIES {
                state.memory_history.pop_front();
            }
            if state.temperature_history.len() > AppConfig::HISTORY_MAX_ENTRIES {
                state.temperature_history.pop_front();
            }
            if state.package_power_history.len() > AppConfig::HISTORY_MAX_ENTRIES {
                state.package_power_history.pop_front();
            }
            if state.ane_power_history.len() > AppConfig::HISTORY_MAX_ENTRIES {
                state.ane_power_history.pop_front();
            }
        } else if !state.cpu_info.is_empty() {
            // Fallback to CPU-based statistics when no GPU is available
            self.update_fallback_history(state);
        }
    }

    fn update_fallback_history(&self, state: &mut AppState) {
        let avg_cpu_utilization = state
            .cpu_info
            .iter()
            .map(|cpu| cpu.utilization)
            .sum::<f64>()
            / state.cpu_info.len() as f64;

        let avg_memory_usage = if !state.memory_info.is_empty() {
            state
                .memory_info
                .iter()
                .map(|mem| {
                    if mem.total_bytes > 0 {
                        (mem.used_bytes as f64 / mem.total_bytes as f64) * 100.0
                    } else {
                        0.0
                    }
                })
                .sum::<f64>()
                / state.memory_info.len() as f64
        } else {
            0.0
        };

        // Use CPU temperature if available, otherwise use a placeholder
        let cpu_temps: Vec<f64> = state
            .cpu_info
            .iter()
            .filter_map(|cpu| cpu.temperature.map(|t| t as f64))
            .collect();
        let avg_temperature = if !cpu_temps.is_empty() {
            cpu_temps.iter().sum::<f64>() / cpu_temps.len() as f64
        } else {
            0.0
        };

        state.utilization_history.push_back(avg_cpu_utilization);
        state.memory_history.push_back(avg_memory_usage);
        state.temperature_history.push_back(avg_temperature);
        state.package_power_history.push_back(0.0);

        // Keep only last N entries as configured
        if state.utilization_history.len() > AppConfig::HISTORY_MAX_ENTRIES {
            state.utilization_history.pop_front();
        }
        if state.memory_history.len() > AppConfig::HISTORY_MAX_ENTRIES {
            state.memory_history.pop_front();
        }
        if state.temperature_history.len() > AppConfig::HISTORY_MAX_ENTRIES {
            state.temperature_history.pop_front();
        }
        if state.package_power_history.len() > AppConfig::HISTORY_MAX_ENTRIES {
            state.package_power_history.pop_front();
        }
    }

    /// Calculate average GPU utilization
    #[allow(dead_code)]
    pub fn calculate_avg_gpu_utilization(state: &AppState) -> f64 {
        if state.gpu_info.is_empty() {
            return 0.0;
        }

        state
            .gpu_info
            .iter()
            .map(|gpu| gpu.utilization)
            .sum::<f64>()
            / state.gpu_info.len() as f64
    }

    /// Calculate average GPU memory usage
    #[allow(dead_code)]
    pub fn calculate_avg_gpu_memory(state: &AppState) -> f64 {
        if state.gpu_info.is_empty() {
            return 0.0;
        }

        state
            .gpu_info
            .iter()
            .map(|gpu| {
                if gpu.total_memory > 0 {
                    (gpu.used_memory as f64 / gpu.total_memory as f64) * 100.0
                } else {
                    0.0
                }
            })
            .sum::<f64>()
            / state.gpu_info.len() as f64
    }

    /// Calculate average CPU utilization
    #[allow(dead_code)]
    pub fn calculate_avg_cpu_utilization(state: &AppState) -> f64 {
        if state.cpu_info.is_empty() {
            return 0.0;
        }

        state
            .cpu_info
            .iter()
            .map(|cpu| cpu.utilization)
            .sum::<f64>()
            / state.cpu_info.len() as f64
    }

    /// Calculate average system memory usage
    #[allow(dead_code)]
    pub fn calculate_avg_system_memory(state: &AppState) -> f64 {
        if state.memory_info.is_empty() {
            return 0.0;
        }

        state
            .memory_info
            .iter()
            .map(|mem| {
                if mem.total_bytes > 0 {
                    (mem.used_bytes as f64 / mem.total_bytes as f64) * 100.0
                } else {
                    0.0
                }
            })
            .sum::<f64>()
            / state.memory_info.len() as f64
    }
}

fn detect_apple_silicon(state: &AppState) -> bool {
    state.gpu_info.iter().any(|gpu| {
        gpu.detail
            .get("architecture")
            .map(|arch| arch == "Apple Silicon")
            .unwrap_or(false)
    })
}

fn current_package_power_watts(state: &AppState) -> f64 {
    if detect_apple_silicon(state) {
        state
            .gpu_info
            .iter()
            .find_map(|gpu| {
                gpu.detail
                    .get("combined_power_mw")
                    .and_then(|value| value.parse::<f64>().ok())
                    .map(|mw| mw / 1000.0)
            })
            .unwrap_or_else(|| state.gpu_info.iter().map(|gpu| gpu.power_consumption).sum())
    } else {
        state.gpu_info.iter().map(|gpu| gpu.power_consumption).sum()
    }
}

fn current_ane_power_watts(state: &AppState) -> f64 {
    state
        .gpu_info
        .first()
        .map(|gpu| gpu.ane_utilization / 1000.0)
        .unwrap_or(0.0)
}

impl Default for DataAggregator {
    fn default() -> Self {
        Self::new()
    }
}
