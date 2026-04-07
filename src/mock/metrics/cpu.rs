//! CPU metrics structures and utilities

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

use rand::{RngExt, rng};

#[derive(Clone)]
pub struct CpuMetrics {
    #[allow(dead_code)]
    pub model: String,
    pub utilization: f32,
    #[allow(dead_code)]
    pub socket_count: u32,
    pub core_count: u32,
    #[allow(dead_code)]
    pub thread_count: u32,
    #[allow(dead_code)]
    pub frequency_mhz: u32,
    pub temperature_celsius: Option<u32>,
    pub power_consumption_watts: Option<f32>,
    // Per-socket utilization for multi-socket systems
    pub socket_utilizations: Vec<f32>,
    // Apple Silicon specific fields
    pub p_core_count: Option<u32>,
    pub e_core_count: Option<u32>,
    #[allow(dead_code)]
    pub gpu_core_count: Option<u32>,
    pub p_core_utilization: Option<f32>,
    pub e_core_utilization: Option<f32>,
    pub p_cluster_frequency_mhz: Option<u32>,
    pub e_cluster_frequency_mhz: Option<u32>,
    // Per-core utilization metrics
    pub per_core_utilization: Vec<f32>,
}

impl CpuMetrics {
    /// Update CPU metrics with realistic variations
    pub fn update(&mut self) {
        let mut rng = rng();

        // Update CPU utilization
        let cpu_utilization_delta = rng.random_range(-3.0..3.0);
        self.utilization = (self.utilization + cpu_utilization_delta).clamp(0.0, 100.0);

        // Update per-socket utilizations
        for socket_util in &mut self.socket_utilizations {
            let socket_delta = rng.random_range(-3.0..3.0);
            *socket_util = (*socket_util + socket_delta).clamp(0.0, 100.0);
        }

        // Update CPU temperature if available
        if let Some(ref mut temp) = self.temperature_celsius {
            let temp_delta = rng.random_range(-2..3);
            *temp = (*temp as i32 + temp_delta).clamp(35, 85) as u32;
        }

        // Update CPU power consumption if available
        if let Some(ref mut power) = self.power_consumption_watts {
            let power_delta = rng.random_range(-10.0..10.0);
            *power = (*power + power_delta).clamp(10.0, 500.0);
        }

        // Update Apple Silicon specific metrics
        if let (Some(ref mut p_util), Some(ref mut e_util)) =
            (&mut self.p_core_utilization, &mut self.e_core_utilization)
        {
            let p_delta = rng.random_range(-4.0..4.0);
            let e_delta = rng.random_range(-2.0..2.0);
            *p_util = (*p_util + p_delta).clamp(0.0, 100.0);
            *e_util = (*e_util + e_delta).clamp(0.0, 100.0);
        }

        // Update P/E cluster frequencies for Apple Silicon
        if let (Some(ref mut p_freq), Some(ref mut e_freq)) = (
            &mut self.p_cluster_frequency_mhz,
            &mut self.e_cluster_frequency_mhz,
        ) {
            // P-cluster: high performance, varies between 2500-3500 MHz
            let p_delta = rng.random_range(-50..50) as i32;
            *p_freq = ((*p_freq as i32 + p_delta).clamp(2500, 3500)) as u32;

            // E-cluster: efficiency, varies between 600-2000 MHz
            let e_delta = rng.random_range(-30..30) as i32;
            *e_freq = ((*e_freq as i32 + e_delta).clamp(600, 2000)) as u32;
        }

        // Update per-core utilization
        for core_util in &mut self.per_core_utilization {
            let core_delta = rng.random_range(-5.0..5.0);
            *core_util = (*core_util + core_delta).clamp(0.0, 100.0);
        }

        // Recalculate overall utilization from per-core values
        if !self.per_core_utilization.is_empty() {
            self.utilization = self.per_core_utilization.iter().sum::<f32>()
                / self.per_core_utilization.len() as f32;

            // Update socket utilizations to reflect new overall utilization
            for socket_util in &mut self.socket_utilizations {
                *socket_util = self.utilization + rng.random_range(-2.0..2.0);
            }

            // Update P-core and E-core utilization for Apple Silicon
            if let (Some(p_count), Some(e_count), Some(ref mut p_util), Some(ref mut e_util)) = (
                self.p_core_count,
                self.e_core_count,
                &mut self.p_core_utilization,
                &mut self.e_core_utilization,
            ) {
                if p_count > 0
                    && e_count > 0
                    && self.per_core_utilization.len() >= (p_count + e_count) as usize
                {
                    *p_util = self.per_core_utilization[..p_count as usize]
                        .iter()
                        .sum::<f32>()
                        / p_count as f32;
                    *e_util = self.per_core_utilization
                        [p_count as usize..(p_count + e_count) as usize]
                        .iter()
                        .sum::<f32>()
                        / e_count as f32;
                }
            }
        }
    }
}
