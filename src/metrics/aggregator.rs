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

use std::collections::HashMap;

use crate::device::{CpuInfo, GpuInfo, MemoryInfo};

/// Metrics aggregation utilities for cluster-wide statistics
#[allow(dead_code)] // Used in coordinator.rs (metrics infrastructure)
pub struct MetricsAggregator;

#[allow(dead_code)] // Used in coordinator.rs (metrics infrastructure)
impl MetricsAggregator {
    /// Calculate cluster-wide GPU statistics
    pub fn aggregate_gpu_metrics(gpu_info: &[GpuInfo]) -> GpuClusterMetrics {
        if gpu_info.is_empty() {
            return GpuClusterMetrics::default();
        }

        let total_gpus = gpu_info.len();
        let total_memory_gb = gpu_info
            .iter()
            .map(|gpu| gpu.total_memory as f64 / (1024.0 * 1024.0 * 1024.0))
            .sum();

        let used_memory_gb = gpu_info
            .iter()
            .map(|gpu| gpu.used_memory as f64 / (1024.0 * 1024.0 * 1024.0))
            .sum();

        let total_power_watts = gpu_info.iter().map(|gpu| gpu.power_consumption).sum();

        let avg_utilization =
            gpu_info.iter().map(|gpu| gpu.utilization).sum::<f64>() / total_gpus as f64;

        let avg_temperature = gpu_info
            .iter()
            .map(|gpu| gpu.temperature as f64)
            .sum::<f64>()
            / total_gpus as f64;

        // Calculate temperature standard deviation
        let temp_variance = gpu_info
            .iter()
            .map(|gpu| {
                let diff = gpu.temperature as f64 - avg_temperature;
                diff * diff
            })
            .sum::<f64>()
            / (total_gpus - 1) as f64;
        let temp_std_dev = temp_variance.sqrt();

        let avg_power = total_power_watts / total_gpus as f64;

        GpuClusterMetrics {
            total_gpus,
            total_memory_gb,
            used_memory_gb,
            total_power_watts,
            avg_utilization,
            avg_temperature,
            temp_std_dev,
            avg_power,
        }
    }

    /// Calculate cluster-wide CPU statistics
    pub fn aggregate_cpu_metrics(cpu_info: &[CpuInfo]) -> CpuClusterMetrics {
        if cpu_info.is_empty() {
            return CpuClusterMetrics::default();
        }

        let total_cores = cpu_info
            .iter()
            .map(|cpu| {
                if let Some(apple_info) = &cpu.apple_silicon_info {
                    apple_info.p_core_count + apple_info.e_core_count
                } else {
                    cpu.total_cores
                }
            })
            .sum();

        let avg_utilization =
            cpu_info.iter().map(|cpu| cpu.utilization).sum::<f64>() / cpu_info.len() as f64;

        let total_power_watts = cpu_info
            .iter()
            .filter_map(|cpu| cpu.power_consumption)
            .sum();

        let avg_temperature = cpu_info
            .iter()
            .filter_map(|cpu| cpu.temperature)
            .map(|t| t as f64)
            .sum::<f64>()
            / cpu_info.len() as f64;

        CpuClusterMetrics {
            total_cores,
            avg_utilization,
            total_power_watts,
            avg_temperature,
        }
    }

    /// Calculate cluster-wide memory statistics
    pub fn aggregate_memory_metrics(memory_info: &[MemoryInfo]) -> MemoryClusterMetrics {
        if memory_info.is_empty() {
            return MemoryClusterMetrics::default();
        }

        let total_bytes: u64 = memory_info.iter().map(|mem| mem.total_bytes).sum();
        let used_bytes: u64 = memory_info.iter().map(|mem| mem.used_bytes).sum();
        let available_bytes: u64 = memory_info.iter().map(|mem| mem.available_bytes).sum();

        let total_gb = total_bytes as f64 / (1024.0 * 1024.0 * 1024.0);
        let used_gb = used_bytes as f64 / (1024.0 * 1024.0 * 1024.0);
        let available_gb = available_bytes as f64 / (1024.0 * 1024.0 * 1024.0);

        let avg_utilization =
            memory_info.iter().map(|mem| mem.utilization).sum::<f64>() / memory_info.len() as f64;

        MemoryClusterMetrics {
            total_gb,
            used_gb,
            available_gb,
            avg_utilization,
        }
    }

    /// Calculate per-host metrics for comparison
    pub fn aggregate_by_host(
        gpu_info: &[GpuInfo],
        cpu_info: &[CpuInfo],
        memory_info: &[MemoryInfo],
    ) -> HashMap<String, HostMetrics> {
        let mut host_metrics = HashMap::new();

        // Group by hostname
        let gpu_by_host = Self::group_by_hostname(gpu_info);
        let cpu_by_host = Self::group_by_hostname(cpu_info);
        let memory_by_host = Self::group_by_hostname(memory_info);

        // Get all unique hostnames
        let mut all_hosts = std::collections::HashSet::new();
        all_hosts.extend(gpu_by_host.keys());
        all_hosts.extend(cpu_by_host.keys());
        all_hosts.extend(memory_by_host.keys());

        for hostname in all_hosts {
            let gpu_metrics = gpu_by_host
                .get(hostname)
                .map(|gpus| Self::aggregate_gpu_metrics(gpus))
                .unwrap_or_default();

            let cpu_metrics = cpu_by_host
                .get(hostname)
                .map(|cpus| Self::aggregate_cpu_metrics(cpus))
                .unwrap_or_default();

            let memory_metrics = memory_by_host
                .get(hostname)
                .map(|mems| Self::aggregate_memory_metrics(mems))
                .unwrap_or_default();

            host_metrics.insert(
                hostname.clone(),
                HostMetrics {
                    hostname: hostname.clone(),
                    gpu_metrics,
                    cpu_metrics,
                    memory_metrics,
                },
            );
        }

        host_metrics
    }

    fn group_by_hostname<T>(items: &[T]) -> HashMap<String, Vec<T>>
    where
        T: Clone + HasHostname,
    {
        let mut grouped = HashMap::new();
        for item in items {
            grouped
                .entry(item.hostname().to_string())
                .or_insert_with(Vec::new)
                .push(item.clone());
        }
        grouped
    }
}

/// Trait for items that have a hostname
#[allow(dead_code)] // Used in coordinator.rs (metrics infrastructure)
pub trait HasHostname {
    fn hostname(&self) -> &str;
}

impl HasHostname for GpuInfo {
    fn hostname(&self) -> &str {
        &self.hostname
    }
}

impl HasHostname for CpuInfo {
    fn hostname(&self) -> &str {
        &self.hostname
    }
}

impl HasHostname for MemoryInfo {
    fn hostname(&self) -> &str {
        &self.hostname
    }
}

/// Aggregated GPU metrics for the cluster
#[derive(Debug, Default)]
#[allow(dead_code)] // Used in coordinator.rs (metrics infrastructure)
pub struct GpuClusterMetrics {
    pub total_gpus: usize,
    pub total_memory_gb: f64,
    pub used_memory_gb: f64,
    pub total_power_watts: f64,
    pub avg_utilization: f64,
    pub avg_temperature: f64,
    pub temp_std_dev: f64,
    pub avg_power: f64,
}

/// Aggregated CPU metrics for the cluster
#[derive(Debug, Default)]
#[allow(dead_code)] // Used in coordinator.rs (metrics infrastructure)
pub struct CpuClusterMetrics {
    pub total_cores: u32,
    pub avg_utilization: f64,
    pub total_power_watts: f64,
    pub avg_temperature: f64,
}

/// Aggregated memory metrics for the cluster
#[derive(Debug, Default)]
#[allow(dead_code)] // Used in coordinator.rs (metrics infrastructure)
pub struct MemoryClusterMetrics {
    pub total_gb: f64,
    pub used_gb: f64,
    pub available_gb: f64,
    pub avg_utilization: f64,
}

/// Combined metrics for a single host
#[derive(Debug)]
#[allow(dead_code)] // Used in coordinator.rs (metrics infrastructure)
pub struct HostMetrics {
    pub hostname: String,
    pub gpu_metrics: GpuClusterMetrics,
    pub cpu_metrics: CpuClusterMetrics,
    pub memory_metrics: MemoryClusterMetrics,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn create_test_gpu() -> GpuInfo {
        GpuInfo {
            uuid: "test-uuid".to_string(),
            time: "2024-01-01 00:00:00".to_string(),
            name: "Test GPU".to_string(),
            device_type: "GPU".to_string(),
            host_id: "test-host".to_string(),
            hostname: "test-host".to_string(),
            instance: "test-instance".to_string(),
            utilization: 75.0,
            ane_utilization: 0.0,
            dla_utilization: None,
            tensorcore_utilization: None,
            temperature: 80,
            used_memory: 8 * 1024 * 1024 * 1024,   // 8GB
            total_memory: 16 * 1024 * 1024 * 1024, // 16GB
            frequency: 1500,
            power_consumption: 250.0,
            gpu_core_count: None,
            temperature_threshold_slowdown: None,
            temperature_threshold_shutdown: None,
            temperature_threshold_max_operating: None,
            temperature_threshold_acoustic: None,
            performance_state: None,
            detail: HashMap::new(),
        }
    }

    #[test]
    fn test_aggregate_gpu_metrics() {
        let gpus = vec![create_test_gpu(), create_test_gpu()];
        let metrics = MetricsAggregator::aggregate_gpu_metrics(&gpus);

        assert_eq!(metrics.total_gpus, 2);
        assert_eq!(metrics.total_memory_gb, 32.0);
        assert_eq!(metrics.used_memory_gb, 16.0);
        assert_eq!(metrics.total_power_watts, 500.0);
        assert_eq!(metrics.avg_utilization, 75.0);
        assert_eq!(metrics.avg_temperature, 80.0);
        assert_eq!(metrics.avg_power, 250.0);
    }

    #[test]
    fn test_empty_metrics() {
        let metrics = MetricsAggregator::aggregate_gpu_metrics(&[]);
        assert_eq!(metrics.total_gpus, 0);
        assert_eq!(metrics.total_memory_gb, 0.0);
    }
}
