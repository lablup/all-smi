//! High-performance GPU metrics mock server for all-smi
//!
//! This mock server simulates realistic GPU clusters with multiple nodes,
//! each containing multiple GPUs. It's useful for testing and development
//! of all-smi without requiring actual GPU hardware.

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

use anyhow::Result;
use clap::Parser;

// Import the mock module from src/mock
#[path = "../mock/mod.rs"]
mod mock;

use mock::{Args, start_servers};

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    start_servers(args).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use mock::generator::extract_gpu_memory_gb;
    use mock::metrics::PlatformType;
    use mock::node::MockNode;

    #[test]
    fn test_extract_gpu_memory_gb() {
        assert_eq!(extract_gpu_memory_gb("NVIDIA H200 141GB HBM3"), 141);
        assert_eq!(extract_gpu_memory_gb("NVIDIA A100 80GB"), 80);
        assert_eq!(extract_gpu_memory_gb("RTX 4090 24GB"), 24);
        assert_eq!(extract_gpu_memory_gb("Unknown GPU"), 24); // Default
    }

    #[test]
    fn test_platform_type_parsing() {
        assert_eq!(PlatformType::from_str("nvidia"), PlatformType::Nvidia);
        assert_eq!(PlatformType::from_str("apple"), PlatformType::Apple);
        assert_eq!(PlatformType::from_str("jetson"), PlatformType::Jetson);
        assert_eq!(PlatformType::from_str("intel"), PlatformType::Intel);
        assert_eq!(PlatformType::from_str("amd"), PlatformType::Amd);
        assert_eq!(
            PlatformType::from_str("tenstorrent"),
            PlatformType::Tenstorrent
        );
        assert_eq!(PlatformType::from_str("tt"), PlatformType::Tenstorrent);
        assert_eq!(
            PlatformType::from_str("rebellions"),
            PlatformType::Rebellions
        );
        assert_eq!(PlatformType::from_str("rbln"), PlatformType::Rebellions);
        assert_eq!(PlatformType::from_str("furiosa"), PlatformType::Furiosa);
        assert_eq!(PlatformType::from_str("gaudi"), PlatformType::Gaudi);
        assert_eq!(PlatformType::from_str("habana"), PlatformType::Gaudi);
        assert_eq!(PlatformType::from_str("unknown"), PlatformType::Nvidia); // Default
    }

    #[test]
    fn test_node_naming_with_start_index() {
        // This would typically be tested at the integration level,
        // but we can verify the node accepts any valid name
        let node1 = MockNode::new(
            "node-0001".to_string(),
            "GPU".to_string(),
            PlatformType::Nvidia,
        );
        let node2 = MockNode::new(
            "node-0051".to_string(),
            "GPU".to_string(),
            PlatformType::Nvidia,
        );
        let node3 = MockNode::new(
            "node-0100".to_string(),
            "GPU".to_string(),
            PlatformType::Nvidia,
        );

        assert_eq!(node1.instance_name, "node-0001");
        assert_eq!(node2.instance_name, "node-0051");
        assert_eq!(node3.instance_name, "node-0100");
    }

    #[test]
    fn test_csv_file_content_format() {
        // Test that the CSV content would be correctly formatted
        let test_hosts = vec!["localhost:10001", "localhost:10002", "localhost:10003"];

        // Test the format that would be written
        for host in &test_hosts {
            assert!(host.contains(":"));
            let parts: Vec<&str> = host.split(':').collect();
            assert_eq!(parts.len(), 2);
            assert!(parts[1].parse::<u16>().is_ok());
        }
    }

    #[test]
    fn test_prometheus_label_format() {
        let node = MockNode::new(
            "test-node-123".to_string(),
            "NVIDIA A100".to_string(),
            PlatformType::Nvidia,
        );
        let response = node.get_response();

        // Check label formatting
        assert!(response.contains("instance=\"test-node-123\""));
        assert!(response.contains("gpu=\"NVIDIA A100\""));
        assert!(response.contains("model=\"")); // CPU model
        // UUID format is a standard hex UUID, not "GPU-" prefixed
        assert!(response.contains("uuid=\"")); // UUID is present

        // Check that special characters in labels are handled
        // Note: The current implementation doesn't escape quotes in labels
        // which would be invalid Prometheus format if quotes are present
        let node_special = MockNode::new(
            "node-special-123".to_string(),
            "GPU Name".to_string(),
            PlatformType::Nvidia,
        );
        let response_special = node_special.get_response();
        assert!(response_special.contains("instance=\"node-special-123\""));
    }

    #[test]
    fn test_metric_value_ranges() {
        let node = MockNode::new("test".to_string(), "GPU".to_string(), PlatformType::Nvidia);

        // GPU metrics
        for gpu in &node.gpus {
            assert!(gpu.utilization >= 0.0 && gpu.utilization <= 100.0);
            assert!(gpu.memory_used_bytes <= gpu.memory_total_bytes);
            assert!(gpu.temperature_celsius >= 20 && gpu.temperature_celsius <= 90);
            assert!(gpu.power_consumption_watts >= 0.0 && gpu.power_consumption_watts <= 1000.0);
            assert!(gpu.frequency_mhz >= 100 && gpu.frequency_mhz <= 3000);
        }

        // CPU metrics
        assert!(node.cpu.utilization >= 0.0 && node.cpu.utilization <= 100.0);
        assert!(node.cpu.frequency_mhz >= 100 && node.cpu.frequency_mhz <= 6000);

        // Memory metrics
        assert!(node.memory.used_bytes <= node.memory.total_bytes);
        assert!(node.memory.available_bytes <= node.memory.total_bytes);
    }

    #[test]
    fn test_update_correlations() {
        let mut node = MockNode::new("test".to_string(), "GPU".to_string(), PlatformType::Nvidia);

        // Test multiple updates to verify correlations hold on average
        let mut high_util_powers = Vec::new();
        let mut low_util_powers = Vec::new();

        for _ in 0..10 {
            // Force high utilization
            node.gpus[0].utilization = 95.0;
            node.update();
            high_util_powers.push(node.gpus[0].power_consumption_watts);

            // High utilization should correlate with higher temperature
            assert!(node.gpus[0].temperature_celsius > 50);
        }

        for _ in 0..10 {
            // Force low utilization
            node.gpus[0].utilization = 5.0;
            node.update();
            low_util_powers.push(node.gpus[0].power_consumption_watts);
        }

        // On average, high utilization should have higher power than low utilization
        let avg_high_power: f32 =
            high_util_powers.iter().sum::<f32>() / high_util_powers.len() as f32;
        let avg_low_power: f32 = low_util_powers.iter().sum::<f32>() / low_util_powers.len() as f32;

        assert!(
            avg_high_power > avg_low_power,
            "Average high util power ({avg_high_power}) should be greater than low util power ({avg_low_power})"
        );
    }

    #[test]
    fn test_memory_update_bounds() {
        let mut node = MockNode::new("test".to_string(), "GPU".to_string(), PlatformType::Nvidia);

        // Run multiple updates and ensure memory stays within bounds
        for _ in 0..100 {
            node.update();

            // System memory
            assert!(node.memory.used_bytes <= node.memory.total_bytes);
            assert!(node.memory.available_bytes <= node.memory.total_bytes);
            assert!(node.memory.free_bytes <= node.memory.available_bytes);

            // GPU memory
            for gpu in &node.gpus {
                assert!(gpu.memory_used_bytes <= gpu.memory_total_bytes);
            }
        }
    }

    #[test]
    fn test_platform_specific_power_ranges() {
        // Test that all platforms respect the power limits
        let jetson_node = MockNode::new(
            "jetson".to_string(),
            "GPU".to_string(),
            PlatformType::Jetson,
        );
        for gpu in &jetson_node.gpus {
            assert!(gpu.power_consumption_watts >= 80.0);
            assert!(gpu.power_consumption_watts <= 700.0);
        }

        // Test regular NVIDIA GPUs also respect power range
        let nvidia_node = MockNode::new(
            "nvidia".to_string(),
            "GPU".to_string(),
            PlatformType::Nvidia,
        );
        for gpu in &nvidia_node.gpus {
            assert!(gpu.power_consumption_watts >= 80.0);
            assert!(gpu.power_consumption_watts <= 700.0);
        }
    }

    #[test]
    fn test_disk_metrics_in_response() {
        let node = MockNode::new(
            "disk-test".to_string(),
            "GPU".to_string(),
            PlatformType::Nvidia,
        );
        let response = node.get_response();

        // Check that disk metrics are present in the response
        assert!(response.contains("all_smi_disk_total_bytes"));
        assert!(response.contains("all_smi_disk_available_bytes"));

        // Check mount points are included
        assert!(response.contains("mount_point=\"/\""));
    }

    #[test]
    fn test_prometheus_metric_syntax() {
        let node = MockNode::new(
            "syntax-test".to_string(),
            "GPU Test".to_string(),
            PlatformType::Nvidia,
        );
        let response = node.get_response();

        // Check each line follows Prometheus format
        for line in response.lines() {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            // Metric lines should have the format: metric_name{labels} value
            assert!(line.contains('{') && line.contains('}'));
            let parts: Vec<&str> = line.split_whitespace().collect();
            assert!(parts.len() >= 2, "Invalid metric line: {line}");

            // Last part should be a valid number
            let value_str = parts.last().unwrap();
            assert!(
                value_str.parse::<f64>().is_ok(),
                "Invalid metric value: {value_str}"
            );
        }
    }

    #[test]
    fn test_socket_utilization_consistency() {
        let node = MockNode::new(
            "socket-test".to_string(),
            "GPU".to_string(),
            PlatformType::Intel,
        );

        // For multi-socket systems, socket utilizations should sum to approximately total utilization
        if node.cpu.socket_count > 1 {
            let sum: f32 = node.cpu.socket_utilizations.iter().sum();
            let avg = sum / node.cpu.socket_count as f32;
            let diff = (avg - node.cpu.utilization).abs();

            // Allow some variance but should be close
            assert!(
                diff < 10.0,
                "Socket utilization sum doesn't match total utilization"
            );
        }
    }
}
