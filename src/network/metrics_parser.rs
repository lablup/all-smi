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

use crate::parsing::common::sanitize_label_value;
use chrono::Local;
use regex::Regex;

use crate::device::{AppleSiliconCpuInfo, CpuInfo, CpuPlatformType, GpuInfo, MemoryInfo};
use crate::storage::info::StorageInfo;

pub struct MetricsParser;

impl MetricsParser {
    pub fn new() -> Self {
        Self
    }

    pub fn parse_metrics(
        &self,
        text: &str,
        host: &str,
        re: &Regex,
    ) -> (
        Vec<GpuInfo>,
        Vec<CpuInfo>,
        Vec<MemoryInfo>,
        Vec<StorageInfo>,
    ) {
        // Limit the maximum size of HashMaps to prevent memory exhaustion
        const MAX_DEVICES_PER_TYPE: usize = 256;
        const MAX_TEXT_SIZE: usize = 10_485_760; // 10MB max input

        // Validate input size
        if text.len() > MAX_TEXT_SIZE {
            eprintln!(
                "Warning: Metrics text too large ({}), truncating to 10MB",
                text.len()
            );
            let truncated = &text[..MAX_TEXT_SIZE];
            return self.parse_metrics(truncated, host, re);
        }

        let mut gpu_info_map: HashMap<String, GpuInfo> = HashMap::with_capacity(16);
        let mut cpu_info_map: HashMap<String, CpuInfo> = HashMap::with_capacity(8);
        let mut memory_info_map: HashMap<String, MemoryInfo> = HashMap::with_capacity(8);
        let mut storage_info_map: HashMap<String, StorageInfo> = HashMap::with_capacity(32);
        let mut host_instance_name: Option<String> = None;

        for line in text.lines() {
            if let Some((metric_name, labels_str, value)) = parse_prometheus!(line, re) {
                let labels = self.parse_labels(&labels_str);

                // Extract instance name from the first metric that has it
                if host_instance_name.is_none()
                    && let Some(instance) = labels.get("instance")
                {
                    host_instance_name = Some(instance.clone());
                }

                // Process different metric types with size limits
                if metric_name.starts_with("gpu_")
                    || metric_name.starts_with("npu_")
                    || metric_name == "ane_utilization"
                {
                    if gpu_info_map.len() < MAX_DEVICES_PER_TYPE {
                        self.process_gpu_metrics(
                            &mut gpu_info_map,
                            &metric_name,
                            &labels,
                            value,
                            host,
                        );
                    }
                } else if metric_name.starts_with("cpu_") {
                    if cpu_info_map.len() < MAX_DEVICES_PER_TYPE {
                        self.process_cpu_metrics(
                            &mut cpu_info_map,
                            &metric_name,
                            &labels,
                            value,
                            host,
                        );
                    }
                } else if metric_name.starts_with("memory_") {
                    if memory_info_map.len() < MAX_DEVICES_PER_TYPE {
                        self.process_memory_metrics(
                            &mut memory_info_map,
                            &metric_name,
                            &labels,
                            value,
                            host,
                        );
                    }
                } else if (metric_name.starts_with("storage_") || metric_name.starts_with("disk_"))
                    && storage_info_map.len() < MAX_DEVICES_PER_TYPE
                {
                    self.process_storage_metrics(
                        &mut storage_info_map,
                        &metric_name,
                        &labels,
                        value,
                        host,
                    );
                }
            }
        }

        // Store instance name in detail field if available, but keep host as the key
        if let Some(instance_name) = host_instance_name {
            self.update_instance_names(
                &mut gpu_info_map,
                &mut cpu_info_map,
                &mut memory_info_map,
                &mut storage_info_map,
                &instance_name,
            );
        }

        (
            gpu_info_map.into_values().collect(),
            cpu_info_map.into_values().collect(),
            memory_info_map.into_values().collect(),
            storage_info_map.into_values().collect(),
        )
    }

    fn parse_labels(&self, labels_str: &str) -> HashMap<String, String> {
        const MAX_LABELS: usize = 100; // Prevent unbounded growth
        const MAX_LABEL_LENGTH: usize = 1024; // Prevent large string allocations
        const MAX_INPUT_LENGTH: usize = 32768; // Maximum label string length to process

        // Limit input size to prevent DoS
        if labels_str.len() > MAX_INPUT_LENGTH {
            eprintln!("Warning: Label string too long, truncating to {MAX_INPUT_LENGTH} bytes");
            return HashMap::new();
        }

        let mut labels: HashMap<String, String> = HashMap::with_capacity(16);
        let mut label_count = 0;

        // Optimized parsing without intermediate allocations
        for label in labels_str.split(',') {
            if label_count >= MAX_LABELS {
                break;
            }

            // Find the '=' separator without allocating a vector
            if let Some(eq_pos) = label.find('=') {
                let key = &label[..eq_pos];
                let value = &label[eq_pos + 1..];

                // Sanitize and validate in one pass
                let key_clean = sanitize_label_value(key);
                let value_clean = sanitize_label_value(value);

                // Check lengths and insert
                if key_clean.len() <= MAX_LABEL_LENGTH && value_clean.len() <= MAX_LABEL_LENGTH {
                    labels.insert(key_clean, value_clean);
                    label_count += 1;
                }
            }
        }
        labels
    }

    fn process_gpu_metrics(
        &self,
        gpu_info_map: &mut HashMap<String, GpuInfo>,
        metric_name: &str,
        labels: &HashMap<String, String>,
        value: f64,
        host: &str,
    ) {
        let gpu_name = crate::get_label_or_default!(labels, "gpu");
        let gpu_uuid = crate::get_label_or_default!(labels, "uuid");
        let gpu_index = crate::get_label_or_default!(labels, "index");

        if gpu_name.is_empty() || gpu_uuid.is_empty() {
            return;
        }

        let gpu_info = gpu_info_map.entry(gpu_uuid.clone()).or_insert_with(|| {
            let mut detail = HashMap::new();
            detail.insert("index".to_string(), gpu_index.clone());
            GpuInfo {
                uuid: gpu_uuid.clone(),
                time: Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
                name: gpu_name,
                device_type: "GPU".to_string(), // Default to GPU, can be overridden by gpu_info metric
                host_id: host.to_string(),      // Host identifier (e.g., "10.82.128.41:9090")
                hostname: crate::get_label_or_default!(labels, "instance", host), // DNS hostname from instance label
                instance: crate::get_label_or_default!(labels, "instance", host),
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
                detail,
            }
        });

        crate::update_metric_field!(metric_name, value, gpu_info, {
            "gpu_utilization" => utilization as f64,
            "gpu_memory_used_bytes" => used_memory as u64,
            "gpu_memory_total_bytes" => total_memory as u64,
            "gpu_temperature_celsius" => temperature as u32,
            "gpu_power_consumption_watts" => power_consumption as f64,
            "gpu_frequency_mhz" => frequency as u32,
            "ane_utilization" => ane_utilization as f64
        });

        match metric_name {
            "gpu_power_limit_max_watts" => {
                gpu_info
                    .detail
                    .insert("power_limit_max".to_string(), value.to_string());
            }
            "gpu_info" => {
                // Extract device type
                if let Some(device_type) = labels.get("type") {
                    gpu_info.device_type = device_type.clone();
                }

                // Extract all GPU metadata labels in batch
                crate::extract_labels_batch!(
                    labels,
                    gpu_info.detail,
                    [
                        "cuda_version",
                        "driver_version",
                        "architecture",
                        "compute_capability",
                        "firmware",
                        "serial_number",
                        "pci_address",
                        "pci_device"
                    ]
                );
            }
            "npu_firmware_info" => {
                // Handle NPU-specific firmware info metric
                crate::extract_label_to_detail!(labels, "firmware", gpu_info.detail);
            }
            _ => {}
        }
    }

    fn process_cpu_metrics(
        &self,
        cpu_info_map: &mut HashMap<String, CpuInfo>,
        metric_name: &str,
        labels: &HashMap<String, String>,
        value: f64,
        host: &str,
    ) {
        let cpu_model = crate::get_label_or_default!(labels, "cpu_model");
        // Keep the full host address including port
        let cpu_index = crate::get_label_or_default!(labels, "index", "0");

        let cpu_key = format!("{host}:{cpu_index}");

        let cpu_info = cpu_info_map.entry(cpu_key).or_insert_with(|| {
            let platform_type = if cpu_model.contains("Apple") {
                CpuPlatformType::AppleSilicon
            } else if cpu_model.contains("Intel") {
                CpuPlatformType::Intel
            } else if cpu_model.contains("AMD") {
                CpuPlatformType::Amd
            } else {
                CpuPlatformType::Other("Unknown".to_string())
            };

            CpuInfo {
                host_id: host.to_string(), // Host identifier (e.g., "10.82.128.41:9090")
                hostname: crate::get_label_or_default!(labels, "instance", host), // DNS hostname from instance label
                instance: crate::get_label_or_default!(labels, "instance", host),
                cpu_model: cpu_model.clone(),
                architecture: "".to_string(),
                platform_type,
                socket_count: 1,
                total_cores: 0,
                total_threads: 0,
                base_frequency_mhz: 0,
                max_frequency_mhz: 0,
                cache_size_mb: 0,
                utilization: 0.0,
                temperature: None,
                power_consumption: None,
                per_socket_info: Vec::new(),
                apple_silicon_info: None,
                per_core_utilization: Vec::new(),
                time: Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            }
        });

        crate::update_metric_field!(metric_name, value, cpu_info, {
            "cpu_utilization" => utilization as f64,
            "cpu_socket_count" => socket_count as u32,
            "cpu_core_count" => total_cores as u32,
            "cpu_thread_count" => total_threads as u32
        });

        match metric_name {
            "cpu_model" => {
                // Handle all_smi_cpu_model info metric
                if let Some(model) = labels.get("model") {
                    cpu_info.cpu_model = model.clone();

                    // Update platform type based on new model info
                    cpu_info.platform_type = if model.contains("Apple") {
                        CpuPlatformType::AppleSilicon
                    } else if model.contains("Intel") {
                        CpuPlatformType::Intel
                    } else if model.contains("AMD")
                        || model.contains("EPYC")
                        || model.contains("Ryzen")
                    {
                        CpuPlatformType::Amd
                    } else {
                        CpuPlatformType::Other("Unknown".to_string())
                    };
                }
            }
            "cpu_frequency_mhz" => {
                cpu_info.base_frequency_mhz = value as u32;
                cpu_info.max_frequency_mhz = value as u32;
            }
            "cpu_temperature_celsius" => cpu_info.temperature = Some(value as u32),
            "cpu_power_consumption_watts" => cpu_info.power_consumption = Some(value),
            "cpu_s_core_count" => {
                self.ensure_apple_silicon_info(cpu_info);
                crate::update_optional_field!(
                    cpu_info,
                    apple_silicon_info,
                    s_core_count,
                    value as u32
                );
            }
            "cpu_p_core_count" => {
                self.ensure_apple_silicon_info(cpu_info);
                crate::update_optional_field!(
                    cpu_info,
                    apple_silicon_info,
                    p_core_count,
                    value as u32
                );
            }
            "cpu_e_core_count" => {
                self.ensure_apple_silicon_info(cpu_info);
                crate::update_optional_field!(
                    cpu_info,
                    apple_silicon_info,
                    e_core_count,
                    value as u32
                );
            }
            "cpu_s_core_utilization" => {
                self.ensure_apple_silicon_info(cpu_info);
                crate::update_optional_field!(
                    cpu_info,
                    apple_silicon_info,
                    s_core_utilization,
                    value
                );
            }
            "cpu_p_core_utilization" => {
                self.ensure_apple_silicon_info(cpu_info);
                crate::update_optional_field!(
                    cpu_info,
                    apple_silicon_info,
                    p_core_utilization,
                    value
                );
            }
            "cpu_e_core_utilization" => {
                self.ensure_apple_silicon_info(cpu_info);
                crate::update_optional_field!(
                    cpu_info,
                    apple_silicon_info,
                    e_core_utilization,
                    value
                );
            }
            "cpu_core_utilization" => {
                // Parse per-core utilization
                if let (Some(core_id_str), Some(core_type_str)) =
                    (labels.get("core_id"), labels.get("core_type"))
                    && let Ok(core_id) = core_id_str.parse::<u32>()
                {
                    let core_type = match core_type_str.as_str() {
                        "S" => crate::device::CoreType::Super,
                        "P" => crate::device::CoreType::Performance,
                        "E" => crate::device::CoreType::Efficiency,
                        _ => crate::device::CoreType::Standard,
                    };

                    // Ensure vector is large enough
                    while cpu_info.per_core_utilization.len() <= core_id as usize {
                        cpu_info
                            .per_core_utilization
                            .push(crate::device::CoreUtilization {
                                core_id: cpu_info.per_core_utilization.len() as u32,
                                core_type: crate::device::CoreType::Standard,
                                utilization: 0.0,
                            });
                    }

                    // Update the specific core
                    cpu_info.per_core_utilization[core_id as usize] =
                        crate::device::CoreUtilization {
                            core_id,
                            core_type,
                            utilization: value,
                        };
                }
            }
            "cpu_info" => {
                // Extract architecture and platform type from cpu_info metric
                if let Some(architecture) = labels.get("architecture") {
                    cpu_info.architecture = architecture.clone();
                }
                if let Some(platform_type_str) = labels.get("platform_type") {
                    // Parse the platform type from the Debug format
                    cpu_info.platform_type = if platform_type_str.contains("AppleSilicon") {
                        CpuPlatformType::AppleSilicon
                    } else if platform_type_str.contains("Intel") {
                        CpuPlatformType::Intel
                    } else if platform_type_str.contains("Amd") {
                        CpuPlatformType::Amd
                    } else {
                        CpuPlatformType::Other(platform_type_str.clone())
                    };
                }
            }
            _ => {}
        }
    }

    fn process_memory_metrics(
        &self,
        memory_info_map: &mut HashMap<String, MemoryInfo>,
        metric_name: &str,
        labels: &HashMap<String, String>,
        value: f64,
        host: &str,
    ) {
        // Keep the full host address including port
        let memory_index = crate::get_label_or_default!(labels, "index", "0");
        let memory_key = format!("{host}:{memory_index}");

        let memory_info = memory_info_map
            .entry(memory_key)
            .or_insert_with(|| MemoryInfo {
                host_id: host.to_string(), // Host identifier (e.g., "10.82.128.41:9090")
                hostname: crate::get_label_or_default!(labels, "instance", host), // DNS hostname from instance label
                instance: crate::get_label_or_default!(labels, "instance", host),
                total_bytes: 0,
                used_bytes: 0,
                available_bytes: 0,
                free_bytes: 0,
                buffers_bytes: 0,
                cached_bytes: 0,
                swap_total_bytes: 0,
                swap_used_bytes: 0,
                swap_free_bytes: 0,
                utilization: 0.0,
                time: Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            });

        crate::update_metric_field!(metric_name, value, memory_info, {
            "memory_total_bytes" => total_bytes as u64,
            "memory_used_bytes" => used_bytes as u64,
            "memory_available_bytes" => available_bytes as u64,
            "memory_buffers_bytes" => buffers_bytes as u64,
            "memory_cached_bytes" => cached_bytes as u64,
            "memory_utilization" => utilization as f64
        });
    }

    fn process_storage_metrics(
        &self,
        storage_info_map: &mut HashMap<String, StorageInfo>,
        metric_name: &str,
        labels: &HashMap<String, String>,
        value: f64,
        host: &str,
    ) {
        // Keep the full host address including port
        let mount_point = crate::get_label_or_default!(labels, "mount_point");
        let storage_index = crate::get_label_or_default!(labels, "index", "0");

        if mount_point.is_empty() {
            return;
        }

        let storage_key = format!("{host}:{mount_point}");
        let storage_info = storage_info_map
            .entry(storage_key)
            .or_insert_with(|| StorageInfo {
                host_id: host.to_string(), // Host identifier (e.g., "10.82.128.41:9090")
                hostname: labels
                    .get("instance")
                    .cloned()
                    .unwrap_or_else(|| host.to_string()), // DNS hostname from instance label
                mount_point: mount_point.clone(),
                total_bytes: 0,
                available_bytes: 0,
                index: storage_index.parse().unwrap_or(0),
            });

        crate::update_metric_field!(metric_name, value, storage_info, {
            "disk_total_bytes" => total_bytes as u64,
            "disk_available_bytes" => available_bytes as u64
        });
    }

    fn ensure_apple_silicon_info(&self, cpu_info: &mut CpuInfo) {
        if cpu_info.apple_silicon_info.is_none() {
            cpu_info.apple_silicon_info = Some(AppleSiliconCpuInfo {
                s_core_count: 0,
                p_core_count: 0,
                e_core_count: 0,
                gpu_core_count: 0,
                s_core_utilization: 0.0,
                p_core_utilization: 0.0,
                e_core_utilization: 0.0,
                ane_ops_per_second: None,
                s_cluster_frequency_mhz: None,
                p_cluster_frequency_mhz: None,
                e_cluster_frequency_mhz: None,
                s_core_l2_cache_mb: None,
                p_core_l2_cache_mb: None,
                e_core_l2_cache_mb: None,
            });
        }
    }

    fn update_instance_names(
        &self,
        gpu_info_map: &mut HashMap<String, GpuInfo>,
        cpu_info_map: &mut HashMap<String, CpuInfo>,
        memory_info_map: &mut HashMap<String, MemoryInfo>,
        storage_info_map: &mut HashMap<String, StorageInfo>,
        instance_name: &str,
    ) {
        // Store instance name in detail field but keep hostname as the host address
        for gpu_info in gpu_info_map.values_mut() {
            gpu_info
                .detail
                .insert("instance_name".to_string(), instance_name.to_string());
        }
        for _cpu_info in cpu_info_map.values_mut() {
            // For CPU info, we may want to store instance name differently
            // since it doesn't have a detail field by default
        }
        for _memory_info in memory_info_map.values_mut() {
            // Similarly for memory info
        }
        for _storage_info in storage_info_map.values_mut() {
            // And storage info
        }
    }
}

impl Default for MetricsParser {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use regex::Regex;

    fn create_test_parser() -> MetricsParser {
        MetricsParser::new()
    }

    fn create_test_regex() -> Regex {
        Regex::new(r"^all_smi_([^\{]+)\{([^}]+)\} ([\d\.]+)$").unwrap()
    }

    #[test]
    fn test_parse_labels() {
        let parser = create_test_parser();

        let labels = parser.parse_labels(r#"instance="node-0058", mount_point="/", index="0""#);
        assert_eq!(labels.get("instance").unwrap(), "node-0058");
        assert_eq!(labels.get("mount_point").unwrap(), "/");
        assert_eq!(labels.get("index").unwrap(), "0");

        let labels = parser.parse_labels(r#"gpu="NVIDIA H200 141GB HBM3", uuid="GPU-12345""#);
        assert_eq!(labels.get("gpu").unwrap(), "NVIDIA H200 141GB HBM3");
        assert_eq!(labels.get("uuid").unwrap(), "GPU-12345");

        let labels = parser.parse_labels("");
        assert!(labels.is_empty());

        let labels = parser.parse_labels("malformed");
        assert!(labels.is_empty());
    }

    #[test]
    fn test_parse_gpu_metrics() {
        let parser = create_test_parser();
        let re = create_test_regex();
        let host = "127.0.0.1:10058";

        let test_data = r#"
all_smi_gpu_utilization{gpu="NVIDIA H200 141GB HBM3", instance="node-0058", uuid="GPU-12345", index="0"} 25.5
all_smi_gpu_memory_used_bytes{gpu="NVIDIA H200 141GB HBM3", instance="node-0058", uuid="GPU-12345", index="0"} 8589934592
all_smi_gpu_memory_total_bytes{gpu="NVIDIA H200 141GB HBM3", instance="node-0058", uuid="GPU-12345", index="0"} 34359738368
all_smi_gpu_temperature_celsius{gpu="NVIDIA H200 141GB HBM3", instance="node-0058", uuid="GPU-12345", index="0"} 65
all_smi_gpu_power_consumption_watts{gpu="NVIDIA H200 141GB HBM3", instance="node-0058", uuid="GPU-12345", index="0"} 400.5
all_smi_ane_utilization{gpu="NVIDIA H200 141GB HBM3", instance="node-0058", uuid="GPU-12345", index="0"} 15.2
"#;

        let (gpu_info, _, _, _) = parser.parse_metrics(test_data, host, &re);

        assert_eq!(gpu_info.len(), 1);
        let gpu = &gpu_info[0];
        assert_eq!(gpu.uuid, "GPU-12345");
        assert_eq!(gpu.name, "NVIDIA H200 141GB HBM3");
        assert_eq!(gpu.host_id, host);
        assert_eq!(gpu.hostname, "node-0058");
        assert_eq!(gpu.instance, "node-0058");
        assert_eq!(gpu.utilization, 25.5);
        assert_eq!(gpu.used_memory, 8589934592);
        assert_eq!(gpu.total_memory, 34359738368);
        assert_eq!(gpu.temperature, 65);
        assert_eq!(gpu.power_consumption, 400.5);
        assert_eq!(gpu.ane_utilization, 15.2);
    }

    #[test]
    fn test_parse_cpu_metrics() {
        let parser = create_test_parser();
        let re = create_test_regex();
        let host = "127.0.0.1:10058";

        let test_data = r#"
all_smi_cpu_utilization{cpu_model="Intel Xeon", instance="node-0058", hostname="node-0058", index="0"} 45.2
all_smi_cpu_socket_count{cpu_model="Intel Xeon", instance="node-0058", hostname="node-0058", index="0"} 2
all_smi_cpu_core_count{cpu_model="Intel Xeon", instance="node-0058", hostname="node-0058", index="0"} 16
all_smi_cpu_thread_count{cpu_model="Intel Xeon", instance="node-0058", hostname="node-0058", index="0"} 32
all_smi_cpu_frequency_mhz{cpu_model="Intel Xeon", instance="node-0058", hostname="node-0058", index="0"} 2400
all_smi_cpu_temperature_celsius{cpu_model="Intel Xeon", instance="node-0058", hostname="node-0058", index="0"} 55
all_smi_cpu_power_consumption_watts{cpu_model="Intel Xeon", instance="node-0058", hostname="node-0058", index="0"} 125.5
"#;

        let (_, cpu_info, _, _) = parser.parse_metrics(test_data, host, &re);

        assert_eq!(cpu_info.len(), 1);
        let cpu = &cpu_info[0];
        assert_eq!(cpu.host_id, host);
        assert_eq!(cpu.hostname, "node-0058");
        assert_eq!(cpu.instance, "node-0058");
        assert_eq!(cpu.cpu_model, "Intel Xeon");
        assert_eq!(cpu.utilization, 45.2);
        assert_eq!(cpu.socket_count, 2);
        assert_eq!(cpu.total_cores, 16);
        assert_eq!(cpu.total_threads, 32);
        assert_eq!(cpu.base_frequency_mhz, 2400);
        assert_eq!(cpu.max_frequency_mhz, 2400);
        assert_eq!(cpu.temperature, Some(55));
        assert_eq!(cpu.power_consumption, Some(125.5));
        assert!(matches!(
            cpu.platform_type,
            crate::device::CpuPlatformType::Intel
        ));
    }

    #[test]
    fn test_parse_apple_silicon_cpu_metrics() {
        let parser = create_test_parser();
        let re = create_test_regex();
        let host = "127.0.0.1:10058";

        let test_data = r#"
all_smi_cpu_utilization{cpu_model="Apple M2 Max", instance="node-0058", hostname="node-0058", index="0"} 30.5
all_smi_cpu_p_core_count{cpu_model="Apple M2 Max", instance="node-0058", hostname="node-0058", index="0"} 8
all_smi_cpu_e_core_count{cpu_model="Apple M2 Max", instance="node-0058", hostname="node-0058", index="0"} 4
all_smi_cpu_p_core_utilization{cpu_model="Apple M2 Max", instance="node-0058", hostname="node-0058", index="0"} 25.2
all_smi_cpu_e_core_utilization{cpu_model="Apple M2 Max", instance="node-0058", hostname="node-0058", index="0"} 10.8
"#;

        let (_, cpu_info, _, _) = parser.parse_metrics(test_data, host, &re);

        assert_eq!(cpu_info.len(), 1);
        let cpu = &cpu_info[0];
        assert_eq!(cpu.cpu_model, "Apple M2 Max");
        assert_eq!(cpu.utilization, 30.5);
        assert!(matches!(
            cpu.platform_type,
            crate::device::CpuPlatformType::AppleSilicon
        ));

        let apple_info = cpu.apple_silicon_info.as_ref().unwrap();
        assert_eq!(apple_info.p_core_count, 8);
        assert_eq!(apple_info.e_core_count, 4);
        assert_eq!(apple_info.p_core_utilization, 25.2);
        assert_eq!(apple_info.e_core_utilization, 10.8);
    }

    #[test]
    fn test_parse_memory_metrics() {
        let parser = create_test_parser();
        let re = create_test_regex();
        let host = "127.0.0.1:10058";

        let test_data = r#"
all_smi_memory_total_bytes{instance="node-0058", hostname="node-0058", index="0"} 137438953472
all_smi_memory_used_bytes{instance="node-0058", hostname="node-0058", index="0"} 68719476736
all_smi_memory_available_bytes{instance="node-0058", hostname="node-0058", index="0"} 68719476736
all_smi_memory_utilization{instance="node-0058", hostname="node-0058", index="0"} 50.0
"#;

        let (_, _, memory_info, _) = parser.parse_metrics(test_data, host, &re);

        assert_eq!(memory_info.len(), 1);
        let memory = &memory_info[0];
        assert_eq!(memory.host_id, host);
        assert_eq!(memory.hostname, "node-0058");
        assert_eq!(memory.instance, "node-0058");
        assert_eq!(memory.total_bytes, 137438953472);
        assert_eq!(memory.used_bytes, 68719476736);
        assert_eq!(memory.available_bytes, 68719476736);
        assert_eq!(memory.utilization, 50.0);
    }

    #[test]
    fn test_parse_storage_metrics() {
        let parser = create_test_parser();
        let re = create_test_regex();
        let host = "127.0.0.1:10058";

        let test_data = r#"
all_smi_disk_total_bytes{instance="node-0058", mount_point="/", index="0"} 4398046511104
all_smi_disk_available_bytes{instance="node-0058", mount_point="/", index="0"} 891915494941
all_smi_disk_total_bytes{instance="node-0058", mount_point="/home", index="1"} 1099511627776
all_smi_disk_available_bytes{instance="node-0058", mount_point="/home", index="1"} 549755813888
"#;

        let (_, _, _, storage_info) = parser.parse_metrics(test_data, host, &re);

        assert_eq!(storage_info.len(), 2);

        let root_storage = storage_info.iter().find(|s| s.mount_point == "/").unwrap();
        assert_eq!(root_storage.host_id, host);
        assert_eq!(root_storage.hostname, "node-0058");
        assert_eq!(root_storage.total_bytes, 4398046511104);
        assert_eq!(root_storage.available_bytes, 891915494941);
        assert_eq!(root_storage.index, 0);

        let home_storage = storage_info
            .iter()
            .find(|s| s.mount_point == "/home")
            .unwrap();
        assert_eq!(home_storage.host_id, host);
        assert_eq!(home_storage.hostname, "node-0058");
        assert_eq!(home_storage.total_bytes, 1099511627776);
        assert_eq!(home_storage.available_bytes, 549755813888);
        assert_eq!(home_storage.index, 1);
    }

    #[test]
    fn test_parse_mixed_metrics() {
        let parser = create_test_parser();
        let re = create_test_regex();
        let host = "127.0.0.1:10058";

        let test_data = r#"
all_smi_gpu_utilization{gpu="NVIDIA RTX 4090", instance="node-0001", uuid="GPU-ABCDE", index="0"} 75.0
all_smi_cpu_utilization{cpu_model="AMD Ryzen", instance="node-0001", hostname="node-0001", index="0"} 60.0
all_smi_memory_total_bytes{instance="node-0001", hostname="node-0001", index="0"} 68719476736
all_smi_disk_total_bytes{instance="node-0001", mount_point="/", index="0"} 2199023255552
"#;

        let (gpu_info, cpu_info, memory_info, storage_info) =
            parser.parse_metrics(test_data, host, &re);

        assert_eq!(gpu_info.len(), 1);
        assert_eq!(cpu_info.len(), 1);
        assert_eq!(memory_info.len(), 1);
        assert_eq!(storage_info.len(), 1);

        assert_eq!(gpu_info[0].name, "NVIDIA RTX 4090");
        assert_eq!(gpu_info[0].utilization, 75.0);
        assert_eq!(gpu_info[0].host_id, host);
        assert_eq!(gpu_info[0].hostname, "node-0001");
        assert_eq!(gpu_info[0].instance, "node-0001");

        assert_eq!(cpu_info[0].cpu_model, "AMD Ryzen");
        assert_eq!(cpu_info[0].utilization, 60.0);
        assert!(matches!(
            cpu_info[0].platform_type,
            crate::device::CpuPlatformType::Amd
        ));

        assert_eq!(memory_info[0].total_bytes, 68719476736);
        assert_eq!(storage_info[0].total_bytes, 2199023255552);
    }

    #[test]
    fn test_invalid_metrics() {
        let parser = create_test_parser();
        let re = create_test_regex();
        let host = "127.0.0.1:10058";

        let test_data = r#"
invalid_metric_format
all_smi_gpu_utilization{malformed labels} invalid_value
all_smi_unknown_metric{instance="test"} 42.0
"#;

        let (gpu_info, cpu_info, memory_info, storage_info) =
            parser.parse_metrics(test_data, host, &re);

        assert!(gpu_info.is_empty());
        assert!(cpu_info.is_empty());
        assert!(memory_info.is_empty());
        assert!(storage_info.is_empty());
    }

    #[test]
    fn test_empty_metrics() {
        let parser = create_test_parser();
        let re = create_test_regex();
        let host = "127.0.0.1:10058";

        let (gpu_info, cpu_info, memory_info, storage_info) = parser.parse_metrics("", host, &re);

        assert!(gpu_info.is_empty());
        assert!(cpu_info.is_empty());
        assert!(memory_info.is_empty());
        assert!(storage_info.is_empty());
    }

    #[test]
    fn test_hostname_update() {
        let parser = create_test_parser();
        let re = create_test_regex();
        let host = "127.0.0.1:10058";

        let test_data = r#"
all_smi_gpu_utilization{gpu="Tesla V100", instance="production-node-42", uuid="GPU-XYZ", index="0"} 85.0
all_smi_cpu_utilization{cpu_model="Intel Xeon", instance="production-node-42", hostname="node-0058", index="0"} 55.0
"#;

        let (gpu_info, cpu_info, _, _) = parser.parse_metrics(test_data, host, &re);

        assert_eq!(gpu_info[0].host_id, host);
        assert_eq!(gpu_info[0].hostname, "production-node-42");
        assert_eq!(gpu_info[0].instance, "production-node-42");
        assert_eq!(cpu_info[0].host_id, host);
        assert_eq!(cpu_info[0].hostname, "production-node-42");
        assert_eq!(cpu_info[0].instance, "production-node-42");
    }

    #[test]
    fn test_cpu_platform_detection() {
        let parser = create_test_parser();
        let re = create_test_regex();
        let host = "127.0.0.1:10058";

        let test_cases = [
            ("Apple M1 Pro", crate::device::CpuPlatformType::AppleSilicon),
            ("Intel Core i9", crate::device::CpuPlatformType::Intel),
            ("AMD Ryzen 9", crate::device::CpuPlatformType::Amd),
            (
                "Unknown Processor",
                crate::device::CpuPlatformType::Other("Unknown".to_string()),
            ),
        ];

        for (cpu_model, expected_type) in test_cases {
            let test_data = format!(
                r#"all_smi_cpu_utilization{{cpu_model="{cpu_model}", instance="test", hostname="test", index="0"}} 50.0"#
            );

            let (_, cpu_info, _, _) = parser.parse_metrics(&test_data, host, &re);
            assert_eq!(cpu_info.len(), 1);

            match (&cpu_info[0].platform_type, &expected_type) {
                (
                    crate::device::CpuPlatformType::AppleSilicon,
                    crate::device::CpuPlatformType::AppleSilicon,
                ) => {}
                (crate::device::CpuPlatformType::Intel, crate::device::CpuPlatformType::Intel) => {}
                (crate::device::CpuPlatformType::Amd, crate::device::CpuPlatformType::Amd) => {}
                (
                    crate::device::CpuPlatformType::Other(actual),
                    crate::device::CpuPlatformType::Other(expected),
                ) => {
                    assert_eq!(actual, expected);
                }
                _ => panic!(
                    "Platform type mismatch for {cpu_model}: expected {expected_type:?}, got {:?}",
                    cpu_info[0].platform_type
                ),
            }
        }
    }

    #[test]
    fn test_missing_required_fields() {
        let parser = create_test_parser();
        let re = create_test_regex();
        let host = "127.0.0.1:10058";

        let test_data = r#"
all_smi_gpu_utilization{instance="node-0058", index="0"} 25.5
all_smi_disk_total_bytes{instance="node-0058", index="0"} 1000000000
"#;

        let (gpu_info, _, _, storage_info) = parser.parse_metrics(test_data, host, &re);

        assert!(gpu_info.is_empty());
        assert!(storage_info.is_empty());
    }

    #[test]
    fn test_parse_m5_super_core_metrics() {
        let parser = create_test_parser();
        let re = create_test_regex();
        let host = "127.0.0.1:10001";

        // M5 Max has 6 Super cores + 10 Performance cores, no Efficiency cores
        let test_data = r#"
all_smi_cpu_utilization{cpu_model="Apple M5 Max", instance="m5-node", hostname="m5-node", index="0"} 42.0
all_smi_cpu_s_core_count{cpu_model="Apple M5 Max", instance="m5-node", hostname="m5-node", index="0"} 6
all_smi_cpu_p_core_count{cpu_model="Apple M5 Max", instance="m5-node", hostname="m5-node", index="0"} 10
all_smi_cpu_e_core_count{cpu_model="Apple M5 Max", instance="m5-node", hostname="m5-node", index="0"} 0
all_smi_cpu_s_core_utilization{cpu_model="Apple M5 Max", instance="m5-node", hostname="m5-node", index="0"} 38.5
all_smi_cpu_p_core_utilization{cpu_model="Apple M5 Max", instance="m5-node", hostname="m5-node", index="0"} 44.2
all_smi_cpu_e_core_utilization{cpu_model="Apple M5 Max", instance="m5-node", hostname="m5-node", index="0"} 0.0
"#;

        let (_, cpu_info, _, _) = parser.parse_metrics(test_data, host, &re);

        assert_eq!(cpu_info.len(), 1);
        let cpu = &cpu_info[0];
        assert_eq!(cpu.cpu_model, "Apple M5 Max");
        assert_eq!(cpu.utilization, 42.0);
        assert!(matches!(
            cpu.platform_type,
            crate::device::CpuPlatformType::AppleSilicon
        ));

        let apple_info = cpu.apple_silicon_info.as_ref().unwrap();
        assert_eq!(apple_info.s_core_count, 6);
        assert_eq!(apple_info.p_core_count, 10);
        assert_eq!(apple_info.e_core_count, 0);
        assert_eq!(apple_info.s_core_utilization, 38.5);
        assert_eq!(apple_info.p_core_utilization, 44.2);
        assert_eq!(apple_info.e_core_utilization, 0.0);
    }

    #[test]
    fn test_parse_m5_per_core_super_type() {
        let parser = create_test_parser();
        let re = create_test_regex();
        let host = "127.0.0.1:10001";

        // Per-core metrics for M5 with S-type cores
        let test_data = r#"
all_smi_cpu_utilization{cpu_model="Apple M5 Pro", instance="m5pro-node", hostname="m5pro-node", index="0"} 35.0
all_smi_cpu_core_utilization{cpu_model="Apple M5 Pro", instance="m5pro-node", hostname="m5pro-node", core_id="0", core_type="S", index="0"} 50.0
all_smi_cpu_core_utilization{cpu_model="Apple M5 Pro", instance="m5pro-node", hostname="m5pro-node", core_id="1", core_type="S", index="0"} 40.0
all_smi_cpu_core_utilization{cpu_model="Apple M5 Pro", instance="m5pro-node", hostname="m5pro-node", core_id="2", core_type="P", index="0"} 30.0
all_smi_cpu_core_utilization{cpu_model="Apple M5 Pro", instance="m5pro-node", hostname="m5pro-node", core_id="3", core_type="P", index="0"} 25.0
"#;

        let (_, cpu_info, _, _) = parser.parse_metrics(test_data, host, &re);

        assert_eq!(cpu_info.len(), 1);
        let cpu = &cpu_info[0];
        assert_eq!(cpu.per_core_utilization.len(), 4);

        let core0 = &cpu.per_core_utilization[0];
        assert_eq!(core0.core_id, 0);
        assert_eq!(core0.core_type, crate::device::CoreType::Super);
        assert_eq!(core0.utilization, 50.0);

        let core1 = &cpu.per_core_utilization[1];
        assert_eq!(core1.core_id, 1);
        assert_eq!(core1.core_type, crate::device::CoreType::Super);
        assert_eq!(core1.utilization, 40.0);

        let core2 = &cpu.per_core_utilization[2];
        assert_eq!(core2.core_id, 2);
        assert_eq!(core2.core_type, crate::device::CoreType::Performance);
        assert_eq!(core2.utilization, 30.0);

        let core3 = &cpu.per_core_utilization[3];
        assert_eq!(core3.core_id, 3);
        assert_eq!(core3.core_type, crate::device::CoreType::Performance);
        assert_eq!(core3.utilization, 25.0);
    }

    #[test]
    fn test_m5_backward_compat_m1_m4_no_super_cores() {
        let parser = create_test_parser();
        let re = create_test_regex();
        let host = "127.0.0.1:10001";

        // M2 Max: no S-cores, standard P+E layout
        let test_data = r#"
all_smi_cpu_utilization{cpu_model="Apple M2 Max", instance="m2-node", hostname="m2-node", index="0"} 20.0
all_smi_cpu_p_core_count{cpu_model="Apple M2 Max", instance="m2-node", hostname="m2-node", index="0"} 8
all_smi_cpu_e_core_count{cpu_model="Apple M2 Max", instance="m2-node", hostname="m2-node", index="0"} 4
all_smi_cpu_p_core_utilization{cpu_model="Apple M2 Max", instance="m2-node", hostname="m2-node", index="0"} 18.0
all_smi_cpu_e_core_utilization{cpu_model="Apple M2 Max", instance="m2-node", hostname="m2-node", index="0"} 5.0
"#;

        let (_, cpu_info, _, _) = parser.parse_metrics(test_data, host, &re);

        assert_eq!(cpu_info.len(), 1);
        let cpu = &cpu_info[0];
        let apple_info = cpu.apple_silicon_info.as_ref().unwrap();

        // s_core_count defaults to 0 when not present in metrics
        assert_eq!(apple_info.s_core_count, 0);
        assert_eq!(apple_info.s_core_utilization, 0.0);
        assert_eq!(apple_info.p_core_count, 8);
        assert_eq!(apple_info.e_core_count, 4);
    }
}
