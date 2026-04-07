//! Mock node representing a single server with multiple GPUs

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

use crate::mock::generator::{
    generate_cpu_metrics, generate_disk_metrics, generate_gpus, generate_memory_metrics,
};
use crate::mock::metrics::{CpuMetrics, GpuMetrics, MemoryMetrics, PlatformType};
use crate::mock::template_engine::{build_response_template, render_response};
use rand::{RngExt, rng};

/// High-performance template-based mock node
#[allow(dead_code)]
pub struct MockNode {
    pub instance_name: String,
    pub gpu_name: String,
    pub gpus: Vec<GpuMetrics>,
    pub cpu: CpuMetrics,
    pub memory: MemoryMetrics,
    pub platform_type: PlatformType,
    pub disk_available_bytes: u64,
    pub disk_total_bytes: u64,
    response_template: String,
    rendered_response: String,
    pub is_responding: bool, // Whether this node should respond to requests
}

impl MockNode {
    pub fn new(instance_name: String, gpu_name: String, platform: PlatformType) -> Self {
        // Initialize all metrics
        let gpus = generate_gpus(&gpu_name, &platform);
        let cpu = generate_cpu_metrics(&platform);
        let memory = generate_memory_metrics();
        let (disk_total_bytes, disk_available_bytes) = generate_disk_metrics();

        // Build response template once during initialization
        let response_template =
            build_response_template(&instance_name, &gpu_name, &gpus, &cpu, &memory, &platform);

        let mut node = Self {
            instance_name,
            gpu_name,
            gpus,
            cpu,
            memory,
            platform_type: platform,
            disk_available_bytes,
            disk_total_bytes,
            response_template,
            rendered_response: String::new(),
            is_responding: true, // Start with all nodes responding
        };

        // Render initial response
        node.render_response();
        node
    }

    /// Update all metrics with realistic variations
    pub fn update(&mut self) {
        let mut rng = rng();

        // Update GPU metrics
        for gpu in &mut self.gpus {
            gpu.update(&self.platform_type);
        }

        // Update CPU metrics
        self.cpu.update();

        // Update memory metrics
        self.memory.update();

        // Change disk available bytes by a small amount, up to 1 GiB
        let delta = rng.random_range(-(1024 * 1024 * 1024)..(1024 * 1024 * 1024));
        self.disk_available_bytes = self
            .disk_available_bytes
            .saturating_add_signed(delta)
            .min(self.disk_total_bytes);

        // Re-render response with new values
        self.render_response();
    }

    /// Fast response rendering using string replacement (called every 3 seconds)
    fn render_response(&mut self) {
        self.rendered_response = render_response(
            &self.response_template,
            &self.gpus,
            &self.cpu,
            &self.memory,
            self.disk_available_bytes,
            self.disk_total_bytes,
            &self.platform_type,
        );
    }

    /// Instant response serving (no processing, just return pre-rendered string)
    pub fn get_response(&self) -> &str {
        &self.rendered_response
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mock_node_creation() {
        let node = MockNode::new(
            "test-node".to_string(),
            "Test GPU".to_string(),
            PlatformType::Nvidia,
        );

        assert_eq!(node.instance_name, "test-node");
        assert_eq!(node.gpu_name, "Test GPU");
        assert_eq!(node.gpus.len(), crate::mock::constants::NUM_GPUS);
        assert!(node.is_responding);

        // Check GPU metrics are initialized with reasonable values
        for gpu in &node.gpus {
            assert!(gpu.utilization >= 0.0 && gpu.utilization <= 100.0);
            assert!(gpu.memory_used_bytes <= gpu.memory_total_bytes);
            assert!(gpu.temperature_celsius >= 20 && gpu.temperature_celsius <= 90);
            assert!(gpu.power_consumption_watts >= 0.0);
            assert!(!gpu.uuid.is_empty());
        }
    }

    #[test]
    fn test_platform_specific_cpu_metrics() {
        // Test NVIDIA platform
        let nvidia_node = MockNode::new(
            "nvidia-node".to_string(),
            "GPU".to_string(),
            PlatformType::Nvidia,
        );
        assert!(nvidia_node.cpu.socket_count >= 1);
        assert!(nvidia_node.cpu.thread_count >= nvidia_node.cpu.core_count);
        assert!(nvidia_node.cpu.p_core_count.is_none());
        assert!(nvidia_node.cpu.e_core_count.is_none());

        // Test Apple platform
        let apple_node = MockNode::new(
            "apple-node".to_string(),
            "GPU".to_string(),
            PlatformType::Apple,
        );
        assert_eq!(apple_node.cpu.socket_count, 1);
        assert!(apple_node.cpu.p_core_count.is_some());
        assert!(apple_node.cpu.e_core_count.is_some());
        assert!(apple_node.cpu.gpu_core_count.is_some());
    }

    #[test]
    fn test_node_update() {
        let mut node = MockNode::new(
            "test-node".to_string(),
            "Test GPU".to_string(),
            PlatformType::Nvidia,
        );

        // Capture initial values
        let initial_gpu_util = node.gpus[0].utilization;
        let initial_cpu_util = node.cpu.utilization;
        let initial_memory_used = node.memory.used_bytes;

        // Update the node
        node.update();

        // Check that values have potentially changed (within bounds)
        assert!(node.gpus[0].utilization >= 0.0 && node.gpus[0].utilization <= 100.0);
        assert!(node.cpu.utilization >= 0.0 && node.cpu.utilization <= 100.0);
        assert!(node.memory.used_bytes <= node.memory.total_bytes);

        // Values should be different (not guaranteed but highly likely with random changes)
        // The update function uses random changes, so we can't guarantee values will change
        // This test mainly verifies that update() doesn't crash and maintains valid ranges
        let _values_changed = node.gpus[0].utilization != initial_gpu_util
            || node.cpu.utilization != initial_cpu_util
            || node.memory.used_bytes != initial_memory_used;
    }

    #[test]
    fn test_response_template_contains_required_metrics() {
        let node = MockNode::new(
            "test-node".to_string(),
            "Test GPU".to_string(),
            PlatformType::Nvidia,
        );
        let response = node.get_response();

        // Check for required GPU metrics
        assert!(response.contains("all_smi_gpu_utilization"));
        assert!(response.contains("all_smi_gpu_memory_used_bytes"));
        assert!(response.contains("all_smi_gpu_memory_total_bytes"));
        assert!(response.contains("all_smi_gpu_temperature_celsius"));
        assert!(response.contains("all_smi_gpu_power_consumption_watts"));
        assert!(response.contains("all_smi_gpu_frequency_mhz"));

        // Check for CPU metrics
        assert!(response.contains("all_smi_cpu_utilization"));
        // Note: socket_count is stored in CpuMetrics but not exported as a metric
        assert!(response.contains("all_smi_cpu_core_count"));

        // Check for memory metrics
        assert!(response.contains("all_smi_memory_total_bytes"));
        assert!(response.contains("all_smi_memory_used_bytes"));

        // Check for disk metrics
        assert!(response.contains("all_smi_disk_total_bytes"));
        assert!(response.contains("all_smi_disk_available_bytes"));

        // Check instance label
        assert!(response.contains("instance=\"test-node\""));
    }

    #[test]
    fn test_apple_platform_specific_metrics() {
        let node = MockNode::new(
            "apple-node".to_string(),
            "Apple M1".to_string(),
            PlatformType::Apple,
        );
        let response = node.get_response();

        // Check for Apple-specific metrics
        assert!(response.contains("all_smi_ane_utilization"));
        assert!(response.contains("all_smi_ane_power_watts"));
        assert!(response.contains("all_smi_thermal_pressure_level"));
        // Apple CPU metrics are generated in the template
        assert!(response.contains("core_type=\"performance\""));
        assert!(response.contains("core_type=\"efficiency\""));
        assert!(response.contains("all_smi_cpu_utilization"));
    }

    #[test]
    fn test_failure_simulation() {
        let mut node = MockNode::new(
            "test-node".to_string(),
            "Test GPU".to_string(),
            PlatformType::Nvidia,
        );

        // Node should start as responding
        assert!(node.is_responding);

        // Simulate failure
        node.is_responding = false;
        assert!(!node.is_responding);

        // Simulate recovery
        node.is_responding = true;
        assert!(node.is_responding);
    }

    #[test]
    fn test_disk_size_variations() {
        let node = MockNode::new(
            "test-node".to_string(),
            "Test GPU".to_string(),
            PlatformType::Nvidia,
        );

        // Check that disk sizes are one of the expected values
        use crate::mock::constants::{DISK_SIZE_1TB, DISK_SIZE_4TB, DISK_SIZE_12TB};
        let valid_sizes = [DISK_SIZE_1TB, DISK_SIZE_4TB, DISK_SIZE_12TB];
        assert!(valid_sizes.contains(&node.disk_total_bytes));
        assert!(node.disk_available_bytes <= node.disk_total_bytes);
    }

    #[test]
    fn test_response_performance() {
        let node = MockNode::new(
            "perf-test".to_string(),
            "GPU".to_string(),
            PlatformType::Nvidia,
        );

        // Measure multiple renders to ensure template-based approach is fast
        let start = std::time::Instant::now();
        for _ in 0..1000 {
            let _ = node.get_response();
        }
        let duration = start.elapsed();

        // Should be able to render 1000 responses in under 100ms
        assert!(
            duration.as_millis() < 100,
            "Response rendering too slow: {duration:?}"
        );
    }
}
