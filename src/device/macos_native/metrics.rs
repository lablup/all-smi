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

//! Unified metrics data structure for native macOS APIs
//!
//! This module provides a unified data structure for Apple Silicon metrics
//! populated using native macOS APIs (IOReport, SMC, NSProcessInfo).

use super::ioreport::IOReportMetrics;
use super::smc::SMCMetrics;
use super::thermal::ThermalState;

/// Core types for Apple Silicon cluster identification
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CoreType {
    Super,
    Efficiency,
    Performance,
}

/// Unified metrics data from native macOS APIs
///
/// This structure provides comprehensive Apple Silicon metrics
/// collected from IOReport, SMC, and NSProcessInfo APIs.
#[derive(Debug, Default, Clone)]
pub struct NativeMetricsData {
    // CPU cluster metrics
    pub s_cluster_active_residency: f64,
    pub e_cluster_active_residency: f64,
    pub p_cluster_active_residency: f64,
    pub s_cluster_frequency: u32,
    pub e_cluster_frequency: u32,
    pub p_cluster_frequency: u32,
    pub cpu_power_mw: f64,

    // Per-core metrics (optional, for detailed reporting)
    #[allow(dead_code)]
    pub core_active_residencies: Vec<f64>,
    #[allow(dead_code)]
    pub core_frequencies: Vec<u32>,
    #[allow(dead_code)]
    pub core_cluster_types: Vec<CoreType>,

    // GPU metrics
    pub gpu_active_residency: f64,
    pub gpu_frequency: u32,
    pub gpu_power_mw: f64,

    // ANE metrics
    pub ane_power_mw: f64,

    // Combined metrics
    pub combined_power_mw: f64,

    // Thermal
    pub thermal_pressure_level: Option<String>,

    // Additional metrics from SMC sensors
    pub cpu_temperature: Option<f64>,
    pub gpu_temperature: Option<f64>,
    #[allow(dead_code)]
    pub system_power_watts: Option<f64>,
    #[allow(dead_code)]
    pub fan_speeds: Vec<(String, u32)>,
}

impl NativeMetricsData {
    /// Create from IOReport and SMC metrics
    pub fn from_components(
        ioreport: IOReportMetrics,
        smc: SMCMetrics,
        thermal: ThermalState,
    ) -> Self {
        let combined_power_mw =
            (ioreport.cpu_power + ioreport.gpu_power + ioreport.ane_power) * 1000.0;

        Self {
            // CPU cluster metrics from IOReport
            s_cluster_active_residency: ioreport.s_cluster_residency,
            e_cluster_active_residency: ioreport.e_cluster_residency,
            p_cluster_active_residency: ioreport.p_cluster_residency,
            s_cluster_frequency: ioreport.s_cluster_freq,
            e_cluster_frequency: ioreport.e_cluster_freq,
            p_cluster_frequency: ioreport.p_cluster_freq,
            cpu_power_mw: ioreport.cpu_power * 1000.0, // Convert W to mW

            // Per-core metrics from cluster data
            core_active_residencies: Self::extract_core_residencies(&ioreport),
            core_frequencies: Self::extract_core_frequencies(&ioreport),
            core_cluster_types: Self::extract_core_types(&ioreport),

            // GPU metrics
            gpu_active_residency: ioreport.gpu_residency,
            gpu_frequency: ioreport.gpu_freq,
            gpu_power_mw: ioreport.gpu_power * 1000.0,

            // ANE metrics
            ane_power_mw: ioreport.ane_power * 1000.0,

            // Combined power
            combined_power_mw,

            // Thermal from NSProcessInfo
            thermal_pressure_level: Some(thermal.to_string()),

            // SMC metrics
            cpu_temperature: smc.cpu_temperature,
            gpu_temperature: smc.gpu_temperature,
            system_power_watts: smc.system_power,
            fan_speeds: smc.fan_speeds,
        }
    }

    fn extract_core_residencies(ioreport: &IOReportMetrics) -> Vec<f64> {
        let mut residencies = Vec::new();

        // Add S-cluster cores (M5 Pro/Max Super cores)
        for (_, residency) in &ioreport.s_cluster_data {
            residencies.push(*residency);
        }

        // Add E-cluster cores
        for (_, residency) in &ioreport.e_cluster_data {
            residencies.push(*residency);
        }

        // Add P-cluster cores
        for (_, residency) in &ioreport.p_cluster_data {
            residencies.push(*residency);
        }

        residencies
    }

    fn extract_core_frequencies(ioreport: &IOReportMetrics) -> Vec<u32> {
        let mut frequencies = Vec::new();

        // Add S-cluster cores (M5 Pro/Max Super cores)
        for (freq, _) in &ioreport.s_cluster_data {
            frequencies.push(*freq);
        }

        // Add E-cluster cores
        for (freq, _) in &ioreport.e_cluster_data {
            frequencies.push(*freq);
        }

        // Add P-cluster cores
        for (freq, _) in &ioreport.p_cluster_data {
            frequencies.push(*freq);
        }

        frequencies
    }

    fn extract_core_types(ioreport: &IOReportMetrics) -> Vec<CoreType> {
        let mut types = Vec::new();

        // Add S-cluster cores (M5 Pro/Max Super cores)
        for _ in &ioreport.s_cluster_data {
            types.push(CoreType::Super);
        }

        // Add E-cluster cores
        for _ in &ioreport.e_cluster_data {
            types.push(CoreType::Efficiency);
        }

        // Add P-cluster cores
        for _ in &ioreport.p_cluster_data {
            types.push(CoreType::Performance);
        }

        types
    }

    /// Get CPU utilization as a percentage (0-100)
    /// Uses weighted average of cluster utilization
    #[allow(dead_code)]
    pub fn cpu_utilization(&self) -> f64 {
        if self.s_cluster_active_residency > 0.0 {
            // M5 Pro/Max: Super + Performance (no Efficiency)
            // Weight Super cores more heavily as they handle the most intensive tasks
            self.s_cluster_active_residency * 0.4 + self.p_cluster_active_residency * 0.6
        } else {
            // M1-M4: Performance + Efficiency
            // Weight P-cores more heavily as they handle more intensive tasks
            self.e_cluster_active_residency * 0.3 + self.p_cluster_active_residency * 0.7
        }
    }

    /// Get GPU utilization as a percentage (0-100)
    #[allow(dead_code)]
    pub fn gpu_utilization(&self) -> f64 {
        self.gpu_active_residency
    }

    /// Check if native APIs returned valid data
    #[allow(dead_code)]
    pub fn is_valid(&self) -> bool {
        // Check that we got some meaningful power data
        self.combined_power_mw > 0.0 || self.cpu_power_mw > 0.0 || self.gpu_power_mw > 0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cpu_utilization() {
        let data = NativeMetricsData {
            e_cluster_active_residency: 30.0,
            p_cluster_active_residency: 70.0,
            ..Default::default()
        };

        // Weighted: 30 * 0.3 + 70 * 0.7 = 9 + 49 = 58
        assert!((data.cpu_utilization() - 58.0).abs() < 0.1);
    }

    #[test]
    fn test_gpu_utilization() {
        let data = NativeMetricsData {
            gpu_active_residency: 45.5,
            ..Default::default()
        };

        assert!((data.gpu_utilization() - 45.5).abs() < 0.1);
    }

    #[test]
    fn test_is_valid() {
        let empty_data = NativeMetricsData::default();
        assert!(!empty_data.is_valid());

        let valid_data = NativeMetricsData {
            cpu_power_mw: 1000.0,
            ..Default::default()
        };
        assert!(valid_data.is_valid());
    }

    #[test]
    fn test_core_type_conversion() {
        assert_eq!(CoreType::Efficiency, CoreType::Efficiency);
        assert_eq!(CoreType::Performance, CoreType::Performance);
        assert_ne!(CoreType::Efficiency, CoreType::Performance);
    }
}
