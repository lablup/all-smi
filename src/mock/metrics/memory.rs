//! Memory metrics structures and utilities

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
pub struct MemoryMetrics {
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub available_bytes: u64,
    pub free_bytes: u64,
    pub buffers_bytes: u64,
    pub cached_bytes: u64,
    pub swap_total_bytes: u64,
    pub swap_used_bytes: u64,
    pub swap_free_bytes: u64,
    pub utilization: f32,
}

impl MemoryMetrics {
    /// Update memory metrics with realistic variations
    pub fn update(&mut self) {
        let mut rng = rng();

        // Update memory metrics with gradual fluctuation
        let memory_util_delta = rng.random_range(-2.0..2.0);
        // Allow memory utilization to occasionally reach 100% to trigger swap usage
        self.utilization = (self.utilization + memory_util_delta).clamp(30.0, 102.0);

        // Calculate memory usage, accounting for potential over-allocation
        let target_used_bytes = (self.total_bytes as f64 * self.utilization as f64 / 100.0) as u64;

        if target_used_bytes > self.total_bytes {
            // Memory usage exceeds physical memory - use swap
            self.used_bytes = self.total_bytes;
            self.available_bytes = 0;
            self.free_bytes = 0;

            // Calculate swap usage based on excess memory demand
            if self.swap_total_bytes > 0 {
                let excess_bytes = target_used_bytes - self.total_bytes;
                self.swap_used_bytes = excess_bytes.min(self.swap_total_bytes);
                self.swap_free_bytes = self.swap_total_bytes - self.swap_used_bytes;

                // Memory utilization should show 100% when physical memory is full
                self.utilization = 100.0;
            } else {
                // No swap available, cap at 100% physical memory
                self.utilization = 100.0;
            }
        } else {
            // Normal memory usage - no swap needed
            self.used_bytes = target_used_bytes;
            self.available_bytes = self.total_bytes - target_used_bytes;

            // Update free bytes (a portion of available bytes)
            let free_ratio = rng.random_range(0.3..0.8);
            self.free_bytes = (self.available_bytes as f64 * free_ratio) as u64;

            // No swap usage when memory is below 100%
            if self.swap_total_bytes > 0 {
                self.swap_used_bytes = 0;
                self.swap_free_bytes = self.swap_total_bytes;
            }
        }

        // Small fluctuations in buffers and cache
        if self.buffers_bytes > 0 {
            let buffer_delta =
                rng.random_range(-(self.total_bytes as i64 / 200)..(self.total_bytes as i64 / 200));
            self.buffers_bytes = self
                .buffers_bytes
                .saturating_add_signed(buffer_delta)
                .min(self.total_bytes / 20);
        }

        if self.cached_bytes > 0 {
            let cache_delta =
                rng.random_range(-(self.total_bytes as i64 / 100)..(self.total_bytes as i64 / 100));
            self.cached_bytes = self
                .cached_bytes
                .saturating_add_signed(cache_delta)
                .min(self.total_bytes / 5);
        }
    }
}
