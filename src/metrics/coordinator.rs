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

use std::collections::VecDeque;
use std::sync::Arc;

use tokio::sync::Mutex;

use crate::app_state::AppState;
use crate::common::config::AppConfig;
use crate::metrics::aggregator::{
    CpuClusterMetrics, GpuClusterMetrics, MemoryClusterMetrics, MetricsAggregator,
};

/// Coordinates metrics collection, aggregation, and history management
#[allow(dead_code)] // Future metrics coordination architecture
pub struct MetricsCoordinator {
    app_state: Arc<Mutex<AppState>>,
}

#[allow(dead_code)] // Future metrics coordination architecture
impl MetricsCoordinator {
    pub fn new(app_state: Arc<Mutex<AppState>>) -> Self {
        Self { app_state }
    }

    /// Update cluster metrics and history
    pub async fn update_cluster_metrics(&self) {
        let mut state = self.app_state.lock().await;

        // Calculate cluster-wide metrics
        let gpu_metrics = MetricsAggregator::aggregate_gpu_metrics(&state.gpu_info);
        let _cpu_metrics = MetricsAggregator::aggregate_cpu_metrics(&state.cpu_info);
        let memory_metrics = MetricsAggregator::aggregate_memory_metrics(&state.memory_info);

        // Update history with new values
        self.update_history(&mut state, &gpu_metrics, &memory_metrics);

        // Store current cluster metrics for dashboard
        // Note: This would require adding cluster metrics to AppState
    }

    /// Update per-host metrics for comparison
    pub async fn update_host_metrics(
        &self,
    ) -> std::collections::HashMap<String, crate::metrics::aggregator::HostMetrics> {
        let state = self.app_state.lock().await;
        MetricsAggregator::aggregate_by_host(&state.gpu_info, &state.cpu_info, &state.memory_info)
    }

    /// Get historical trend analysis
    pub async fn get_trend_analysis(&self) -> TrendAnalysis {
        let state = self.app_state.lock().await;

        let utilization_trend = self.calculate_trend(&state.utilization_history);
        let memory_trend = self.calculate_trend(&state.memory_history);
        let temperature_trend = self.calculate_trend(&state.temperature_history);

        TrendAnalysis {
            utilization_trend,
            memory_trend,
            temperature_trend,
            data_points: state.utilization_history.len(),
        }
    }

    /// Get cluster health status
    pub async fn get_cluster_health(&self) -> ClusterHealth {
        let state = self.app_state.lock().await;

        if state.gpu_info.is_empty() {
            return ClusterHealth::NoData;
        }

        let gpu_metrics = MetricsAggregator::aggregate_gpu_metrics(&state.gpu_info);
        let cpu_metrics = MetricsAggregator::aggregate_cpu_metrics(&state.cpu_info);
        let memory_metrics = MetricsAggregator::aggregate_memory_metrics(&state.memory_info);

        // Determine health based on thresholds
        let critical_issues =
            self.check_critical_issues(&gpu_metrics, &cpu_metrics, &memory_metrics);
        let warning_issues = self.check_warning_issues(&gpu_metrics, &cpu_metrics, &memory_metrics);

        if !critical_issues.is_empty() {
            ClusterHealth::Critical(critical_issues)
        } else if !warning_issues.is_empty() {
            ClusterHealth::Warning(warning_issues)
        } else {
            ClusterHealth::Healthy
        }
    }

    /// Calculate performance baselines for anomaly detection
    pub async fn calculate_baselines(&self) -> PerformanceBaselines {
        let state = self.app_state.lock().await;

        let utilization_baseline = self.calculate_baseline(&state.utilization_history);
        let memory_baseline = self.calculate_baseline(&state.memory_history);
        let temperature_baseline = self.calculate_baseline(&state.temperature_history);

        PerformanceBaselines {
            utilization_baseline,
            memory_baseline,
            temperature_baseline,
            confidence: self.calculate_confidence(state.utilization_history.len()),
        }
    }

    fn update_history(
        &self,
        state: &mut AppState,
        gpu_metrics: &GpuClusterMetrics,
        memory_metrics: &MemoryClusterMetrics,
    ) {
        // Add new data points. A cycle where no GPU reported contributes no
        // sample rather than a fabricated 0, which would draw a dip in the
        // history graph that never happened (issue #325).
        if let Some(util) = gpu_metrics.avg_utilization {
            state.utilization_history.push_back(util);
        }
        state
            .memory_history
            .push_back(memory_metrics.avg_utilization);
        if let Some(temp) = gpu_metrics.avg_temperature {
            state.temperature_history.push_back(temp);
        }

        // Maintain history size limits
        self.trim_history(&mut state.utilization_history);
        self.trim_history(&mut state.memory_history);
        self.trim_history(&mut state.temperature_history);
    }

    fn trim_history(&self, history: &mut VecDeque<f64>) {
        while history.len() > AppConfig::HISTORY_MAX_ENTRIES {
            history.pop_front();
        }
    }

    fn calculate_trend(&self, history: &VecDeque<f64>) -> Trend {
        if history.len() < 2 {
            return Trend::Insufficient;
        }

        let recent_points = history.len().min(10); // Use last 10 points for trend
        let start_idx = history.len().saturating_sub(recent_points);
        let recent: Vec<f64> = history.iter().skip(start_idx).copied().collect();

        // Simple linear regression
        let n = recent.len() as f64;
        let sum_x: f64 = (0..recent.len()).map(|i| i as f64).sum();
        let sum_y: f64 = recent.iter().sum();
        let sum_xy: f64 = recent.iter().enumerate().map(|(i, &y)| i as f64 * y).sum();
        let sum_x2: f64 = (0..recent.len()).map(|i| (i * i) as f64).sum();

        let slope = (n * sum_xy - sum_x * sum_y) / (n * sum_x2 - sum_x * sum_x);

        if slope > 0.5 {
            Trend::Increasing
        } else if slope < -0.5 {
            Trend::Decreasing
        } else {
            Trend::Stable
        }
    }

    fn calculate_baseline(&self, history: &VecDeque<f64>) -> Baseline {
        if history.len() < 10 {
            return Baseline::Insufficient;
        }

        let values: Vec<f64> = history.iter().copied().collect();
        let mean = values.iter().sum::<f64>() / values.len() as f64;

        let variance =
            values.iter().map(|&x| (x - mean).powi(2)).sum::<f64>() / values.len() as f64;
        let std_dev = variance.sqrt();

        Baseline::Available {
            mean,
            std_dev,
            min: values.iter().fold(f64::INFINITY, |a, &b| a.min(b)),
            max: values.iter().fold(f64::NEG_INFINITY, |a, &b| a.max(b)),
        }
    }

    fn calculate_confidence(&self, data_points: usize) -> f64 {
        // Confidence increases with more data points, plateaus at 95%
        let max_confidence = 0.95;
        let required_points = 100.0;

        (data_points as f64 / required_points).min(1.0) * max_confidence
    }

    fn check_critical_issues(
        &self,
        gpu_metrics: &GpuClusterMetrics,
        _cpu_metrics: &CpuClusterMetrics,
        memory_metrics: &MemoryClusterMetrics,
    ) -> Vec<String> {
        let mut issues = Vec::new();

        if gpu_metrics.avg_utilization.is_some_and(|u| u > 95.0) {
            issues.push("GPU utilization critically high (>95%)".to_string());
        }

        if gpu_metrics.avg_temperature.is_some_and(|t| t > 90.0) {
            issues.push("GPU temperature critically high (>90°C)".to_string());
        }

        if memory_metrics.avg_utilization > 95.0 {
            issues.push("Memory utilization critically high (>95%)".to_string());
        }

        issues
    }

    fn check_warning_issues(
        &self,
        gpu_metrics: &GpuClusterMetrics,
        _cpu_metrics: &CpuClusterMetrics,
        memory_metrics: &MemoryClusterMetrics,
    ) -> Vec<String> {
        let mut issues = Vec::new();

        if gpu_metrics.avg_utilization.is_some_and(|u| u > 85.0) {
            issues.push("GPU utilization high (>85%)".to_string());
        }

        if gpu_metrics.avg_temperature.is_some_and(|t| t > 80.0) {
            issues.push("GPU temperature high (>80°C)".to_string());
        }

        if memory_metrics.avg_utilization > 85.0 {
            issues.push("Memory utilization high (>85%)".to_string());
        }

        if gpu_metrics.temp_std_dev.is_some_and(|sd| sd > 10.0) {
            issues.push("High temperature variance across GPUs".to_string());
        }

        issues
    }
}

/// Trend analysis results
#[derive(Debug)]
#[allow(dead_code)] // Future metrics coordination architecture
pub struct TrendAnalysis {
    pub utilization_trend: Trend,
    pub memory_trend: Trend,
    pub temperature_trend: Trend,
    pub data_points: usize,
}

/// Trend direction
#[derive(Debug, PartialEq)]
#[allow(dead_code)] // Future metrics coordination architecture
pub enum Trend {
    Increasing,
    Decreasing,
    Stable,
    Insufficient,
}

/// Cluster health status
#[derive(Debug)]
#[allow(dead_code)] // Future metrics coordination architecture
pub enum ClusterHealth {
    Healthy,
    Warning(Vec<String>),
    Critical(Vec<String>),
    NoData,
}

/// Performance baseline information
#[derive(Debug)]
#[allow(dead_code)] // Future metrics coordination architecture
pub struct PerformanceBaselines {
    pub utilization_baseline: Baseline,
    pub memory_baseline: Baseline,
    pub temperature_baseline: Baseline,
    pub confidence: f64,
}

/// Statistical baseline
#[derive(Debug)]
#[allow(dead_code)] // Future metrics coordination architecture
pub enum Baseline {
    Available {
        mean: f64,
        std_dev: f64,
        min: f64,
        max: f64,
    },
    Insufficient,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    #[test]
    fn test_calculate_trend() {
        let state = Arc::new(Mutex::new(AppState::new()));
        let coordinator = MetricsCoordinator::new(state);

        // Test increasing trend
        let mut increasing = VecDeque::new();
        for i in 0..10 {
            increasing.push_back(i as f64 * 2.0); // Clear upward trend
        }
        assert_eq!(coordinator.calculate_trend(&increasing), Trend::Increasing);

        // Test stable trend
        let mut stable = VecDeque::new();
        for _ in 0..10 {
            stable.push_back(50.0); // Constant value
        }
        assert_eq!(coordinator.calculate_trend(&stable), Trend::Stable);

        // Test insufficient data
        let insufficient = VecDeque::new();
        assert_eq!(
            coordinator.calculate_trend(&insufficient),
            Trend::Insufficient
        );
    }

    #[test]
    fn test_calculate_confidence() {
        let state = Arc::new(Mutex::new(AppState::new()));
        let coordinator = MetricsCoordinator::new(state);

        assert_eq!(coordinator.calculate_confidence(0), 0.0);
        assert_eq!(coordinator.calculate_confidence(50), 0.475); // 50% of required points * 95%
        assert!(coordinator.calculate_confidence(200) >= 0.94); // Should be close to max
    }
}
