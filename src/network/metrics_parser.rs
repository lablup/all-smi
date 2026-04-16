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

use std::collections::{HashMap, HashSet};

use crate::parsing::common::sanitize_label_value;
use chrono::Local;
use regex::Regex;

use crate::device::{
    AppleSiliconCpuInfo, CpuInfo, CpuPlatformType, GpmMetrics, GpuInfo, MemoryInfo, MigGpuInfo,
    MigInstanceInfo, NvLinkRemoteDevice, NvLinkRemoteType, VgpuHostInfo, VgpuInfo,
};
use crate::storage::info::StorageInfo;

pub struct MetricsParser;

impl MetricsParser {
    pub fn new() -> Self {
        Self
    }

    #[allow(clippy::type_complexity)]
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
        Vec<VgpuHostInfo>,
        Vec<MigGpuInfo>,
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
        // Keyed by (gpu_uuid, vgpu_id) for instance rows, and by (gpu_uuid, "__host__")
        // for host-scoped metrics.
        let mut vgpu_state = VgpuParseState::new();
        // MIG accumulator — keyed by gpu_uuid for parent host rows, and by
        // (gpu_uuid, mig_instance) for per-instance rows.
        let mut mig_state = MigParseState::new();
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

                // Process different metric types with size limits.
                // Route vGPU and MIG lines first so they aren't swallowed by
                // the broader `gpu_` prefix below. `nvlink_*` metrics from
                // issue #132 are also hardware-detail lines that must join
                // the per-GPU accumulator — they carry the same `uuid`
                // label set as the rest of the GPU rows.
                if metric_name.starts_with("vgpu_") {
                    vgpu_state.process(&metric_name, &labels, value, host);
                } else if metric_name == "gpu_mig_mode" || metric_name.starts_with("mig_instance_")
                {
                    mig_state.process(&metric_name, &labels, value, host);
                } else if metric_name.starts_with("gpu_")
                    || metric_name.starts_with("npu_")
                    || metric_name.starts_with("nvlink_")
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
            vgpu_state.finish(),
            mig_state.finish(),
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

        // Quote-aware splitting: only split on commas that appear outside of
        // a double-quoted value, so that a VM-owner-controlled label value
        // containing `",` cannot break out and inject fake labels.
        for label in split_labels_respecting_quotes(labels_str) {
            if label_count >= MAX_LABELS {
                break;
            }

            // Find the '=' separator without allocating a vector
            if let Some(eq_pos) = label.find('=') {
                let key = &label[..eq_pos];
                let value = &label[eq_pos + 1..];

                // Sanitize the key (trim whitespace + any stray quotes).
                let key_clean = sanitize_label_value(key);
                // For the value we strip the surrounding quotes ourselves
                // and un-escape the Prometheus exposition escape sequences
                // produced by `MetricBuilder::metric`. We never fall back to
                // the naive sanitizer for quoted values — doing so would
                // leave `\"` / `\\` / `\n` / `\r` escapes in place.
                let value_clean = unescape_label_value(value);

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
                // Populated by the new threshold / pstate metric handlers
                // below when the scraped exposition contains them. Remote
                // all-smi nodes exporting older builds will leave these None,
                // matching the local graceful-degradation contract.
                temperature_threshold_slowdown: None,
                temperature_threshold_shutdown: None,
                temperature_threshold_max_operating: None,
                temperature_threshold_acoustic: None,
                performance_state: None,
                // Hardware details (issue #132) land via dedicated
                // `all_smi_gpu_numa_node_id`, `all_smi_gpu_gsp_firmware_*`,
                // `all_smi_nvlink_remote_device_type`, and GPM metric
                // handlers further down. Until those lines are seen in
                // the scrape these fields stay at their "unavailable"
                // defaults, matching the local graceful-degradation
                // contract.
                numa_node_id: None,
                gsp_firmware_mode: None,
                gsp_firmware_version: None,
                nvlink_remote_devices: Vec::new(),
                gpm_metrics: None,
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
            // Thermal thresholds and P-state. Any round-number reading < 0 is
            // rejected via saturating_cast; the exporter only emits positive
            // u32 values, but we defend against malformed upstreams.
            "gpu_temperature_threshold_slowdown_celsius" => {
                gpu_info.temperature_threshold_slowdown = saturating_u32(value);
            }
            "gpu_temperature_threshold_shutdown_celsius" => {
                gpu_info.temperature_threshold_shutdown = saturating_u32(value);
            }
            "gpu_temperature_threshold_max_operating_celsius" => {
                gpu_info.temperature_threshold_max_operating = saturating_u32(value);
            }
            "gpu_temperature_threshold_acoustic_celsius" => {
                gpu_info.temperature_threshold_acoustic = saturating_u32(value);
            }
            "gpu_performance_state" => {
                // NVML defines exactly P0–P15 (0..=15). Accept only values
                // in that range so a malicious or buggy upstream cannot emit
                // an out-of-range value (e.g. 9999) that the TUI would
                // render as "P9999" and corrupt the secondary row layout.
                // Fractional inputs (e.g. 1.5) are also rejected — the
                // exporter only emits integer performance state indices.
                if (0.0..=15.0).contains(&value) && value.fract() == 0.0 {
                    gpu_info.performance_state = saturating_u32(value);
                }
            }
            "gpu_numa_node_id" => {
                // NUMA node ids are non-negative on every real system.
                // Cap at a paranoid ceiling so a hostile upstream cannot
                // inject huge values: no real machine exposes more than
                // ~256 NUMA nodes today. Negative inputs, NaN, and
                // fractional values (e.g. -0.5 → saturates to 0 without
                // this guard) are all dropped.
                if value >= 0.0
                    && value.fract() == 0.0
                    && let Some(node) = saturating_i32(value)
                    && (0..=MAX_NUMA_NODE_ID).contains(&node)
                {
                    gpu_info.numa_node_id = Some(node);
                }
            }
            "gpu_gsp_firmware_mode" => {
                // Exporter emits exactly 0/1/2. Accept only integer values
                // in that range so a malicious upstream cannot seed the UI
                // with a bogus code. Fractional inputs (e.g. 1.5) saturate
                // to 1 without this guard, producing a silently wrong code.
                if (0.0..=2.0).contains(&value) && value.fract() == 0.0 {
                    gpu_info.gsp_firmware_mode =
                        saturating_u32(value).and_then(|v| u8::try_from(v).ok());
                }
            }
            "gpu_gsp_firmware_version_info" => {
                // The numeric value is always 1 (info-style metric). The
                // payload is the `version` label.
                // Reject control characters (including ANSI escape sequences
                // like ESC[2J) to prevent TUI escape injection when the
                // version string is rendered in the hardware row. A remote
                // Prometheus endpoint could embed `\x1b[...` sequences that
                // would be executed by terminal emulators on display.
                if let Some(version) = labels.get("version") {
                    let trimmed = version.trim();
                    if !trimmed.is_empty()
                        && trimmed.len() <= MAX_GSP_VERSION_LEN
                        && trimmed.chars().all(|c| !c.is_control())
                    {
                        gpu_info.gsp_firmware_version = Some(trimmed.to_string());
                    }
                }
            }
            "nvlink_remote_device_type" => {
                // Info-style metric with `link_index` and `remote_type`
                // labels. Enforce a defensive per-GPU cap so a malicious
                // upstream cannot explode the `nvlink_remote_devices` vec
                // by emitting thousands of distinct link indices.
                let Some(link_index) = labels.get("link_index").and_then(|s| s.parse::<u32>().ok())
                else {
                    return;
                };
                if link_index >= MAX_NVLINK_PER_GPU {
                    return;
                }
                let remote_type = labels
                    .get("remote_type")
                    .map(|s| NvLinkRemoteType::from_label(s))
                    .unwrap_or_default();
                // Coalesce duplicate link_index emissions — most recent
                // sample wins — so a scrape that contains the same link
                // multiple times doesn't multiply the vector length.
                if let Some(existing) = gpu_info
                    .nvlink_remote_devices
                    .iter_mut()
                    .find(|l| l.link_index == link_index)
                {
                    existing.remote_type = remote_type;
                } else if gpu_info.nvlink_remote_devices.len() < MAX_NVLINK_PER_GPU as usize {
                    gpu_info.nvlink_remote_devices.push(NvLinkRemoteDevice {
                        link_index,
                        remote_type,
                    });
                }
            }
            "gpu_sm_occupancy" => {
                // GPM fractional utilization — expected in [0.0, 1.0].
                // Values outside that band come from a buggy upstream and
                // are dropped rather than clamped so dashboards can
                // distinguish "unavailable" from "definitely zero".
                if value.is_finite() && (0.0..=1.0).contains(&value) {
                    ensure_gpm_metrics(gpu_info).sm_occupancy = Some(value as f32);
                }
            }
            "gpu_memory_bandwidth_utilization" => {
                if value.is_finite() && (0.0..=1.0).contains(&value) {
                    ensure_gpm_metrics(gpu_info).memory_bandwidth_utilization = Some(value as f32);
                }
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
                    // Reject out-of-range core_ids to prevent OOM from a
                    // malicious upstream sending e.g. core_id="4294967295".
                    if core_id as usize >= MAX_CPU_CORES {
                        return;
                    }

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

/// Split a Prometheus label list on top-level commas only — commas inside a
/// double-quoted value are preserved as literal content. Backslash escapes
/// inside quoted values are honoured so an author cannot terminate the quote
/// by writing `\"`.
///
/// This is a security-critical counterpart to `MetricBuilder::metric`'s
/// escaping: if we split naively on `,` a malicious label value like
/// `vgpu_vm_id="evil\", fake=\"x"` breaks out and injects an attacker-
/// controlled label, enabling cross-host metric spoofing.
/// Saturating cast of a Prometheus `f64` value to `Option<u32>` for the
/// thermal-threshold / P-state fields.
///
/// * Negative values yield `None` (the exporter emits `-1` for "not
///   reported", and non-reading clients should not pretend otherwise).
/// * Values above `u32::MAX` saturate to `u32::MAX`.
/// * `NaN` yields `None`.
fn saturating_u32(value: f64) -> Option<u32> {
    if value.is_nan() || value < 0.0 {
        return None;
    }
    if value >= u32::MAX as f64 {
        return Some(u32::MAX);
    }
    Some(value as u32)
}

/// Saturating cast of a Prometheus `f64` value to `Option<i32>` for the
/// NUMA node id field.
///
/// * Values above `i32::MAX` saturate to `i32::MAX`.
/// * Values below `i32::MIN` saturate to `i32::MIN`.
/// * `NaN` yields `None`.
fn saturating_i32(value: f64) -> Option<i32> {
    if value.is_nan() {
        return None;
    }
    if value >= i32::MAX as f64 {
        return Some(i32::MAX);
    }
    if value <= i32::MIN as f64 {
        return Some(i32::MIN);
    }
    Some(value as i32)
}

/// Maximum CPU core_id accepted from a remote scrape. No real system
/// exposes more than a few hundred cores; 1024 provides generous headroom
/// while preventing OOM from a malicious upstream sending core_id=4294967295.
const MAX_CPU_CORES: usize = 1024;

/// Maximum NvLinks per GPU accepted from a remote scrape. Current NVIDIA
/// hardware caps at 18 physical links; 32 leaves headroom for future
/// generations while still rejecting absurd input.
pub(crate) const MAX_NVLINK_PER_GPU: u32 = 32;

/// Maximum NUMA node id accepted from a remote scrape. No real system
/// exposes more than a few hundred NUMA nodes; 4096 is paranoid.
const MAX_NUMA_NODE_ID: i32 = 4096;

/// Maximum GSP firmware version string length accepted from a remote
/// scrape. NVIDIA's GSP version strings are well under 32 bytes; 128
/// truncates any obviously pathological label.
const MAX_GSP_VERSION_LEN: usize = 128;

/// Lazily populate the GPM metrics slot on a [`GpuInfo`] so we do not
/// allocate an empty struct unless at least one GPM field was observed.
fn ensure_gpm_metrics(gpu_info: &mut GpuInfo) -> &mut GpmMetrics {
    if gpu_info.gpm_metrics.is_none() {
        gpu_info.gpm_metrics = Some(GpmMetrics::default());
    }
    gpu_info.gpm_metrics.as_mut().expect("just populated above")
}

fn split_labels_respecting_quotes(labels_str: &str) -> Vec<&str> {
    let bytes = labels_str.as_bytes();
    let mut out: Vec<&str> = Vec::with_capacity(16);
    let mut start = 0usize;
    let mut i = 0usize;
    let mut in_quotes = false;
    while i < bytes.len() {
        let b = bytes[i];
        if in_quotes {
            if b == b'\\' && i + 1 < bytes.len() {
                // Skip the escaped byte so `\"` doesn't terminate the quote.
                i += 2;
                continue;
            }
            if b == b'"' {
                in_quotes = false;
            }
            i += 1;
        } else {
            match b {
                b'"' => {
                    in_quotes = true;
                    i += 1;
                }
                b',' => {
                    out.push(&labels_str[start..i]);
                    i += 1;
                    start = i;
                }
                _ => {
                    i += 1;
                }
            }
        }
    }
    out.push(&labels_str[start..]);
    out
}

/// Strip the surrounding double quotes from a Prometheus label value and
/// un-escape the escape sequences emitted by `MetricBuilder::metric`
/// (`\\`, `\"`, `\n`, `\r`). If the value is not quoted, fall back to the
/// shared sanitizer so callers see consistent trimming behaviour for legacy
/// unquoted inputs.
fn unescape_label_value(raw: &str) -> String {
    let trimmed = raw.trim();
    let inner = match (trimmed.strip_prefix('"'), trimmed.ends_with('"')) {
        (Some(s), true) if trimmed.len() >= 2 => &s[..s.len() - 1],
        _ => return sanitize_label_value(raw),
    };

    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('\\') => out.push('\\'),
            Some('"') => out.push('"'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some(other) => {
                // Unknown escape: keep the backslash and the following char
                // verbatim so we do not silently lose data.
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }

    // Run through the shared sanitizer only for the length truncation
    // behaviour, since `inner` has already had its quotes removed and any
    // stray leading/trailing whitespace inside the quotes is intentional.
    const MAX_LABEL_VALUE_LENGTH: usize = 1024;
    if out.len() > MAX_LABEL_VALUE_LENGTH {
        let mut end = MAX_LABEL_VALUE_LENGTH;
        while !out.is_char_boundary(end) {
            end -= 1;
        }
        out.truncate(end);
    }
    out
}

/// Accumulator used while parsing vGPU Prometheus lines.
///
/// Per-instance metrics are keyed by `(gpu_uuid, vgpu_id)` so they merge into
/// a single [`VgpuInfo`] even if emitted across multiple metric families.
/// Host-scoped metrics (host mode, scheduler) populate the parent
/// [`VgpuHostInfo`] keyed solely by `gpu_uuid`.
struct VgpuParseState {
    /// `gpu_uuid -> VgpuHostInfo` being assembled.
    hosts: HashMap<String, VgpuHostInfo>,
    /// `(gpu_uuid, vgpu_id) -> VgpuInfo` instance accumulator.
    instances: HashMap<(String, u32), VgpuInfo>,
}

/// Per-scrape cap on distinct physical vGPU-capable GPUs. Matches the
/// `MAX_DEVICES_PER_TYPE` bound used for every other metric family so a
/// malicious remote exporter cannot exhaust memory by advertising unbounded
/// `gpu_uuid` values.
const MAX_VGPU_HOSTS: usize = 256;
/// Per-scrape cap on distinct `(gpu_uuid, vgpu_id)` tuples (~16 vGPUs * 256
/// hosts). New-key insertions past this limit are dropped; updates to
/// already-tracked instances proceed normally.
const MAX_VGPU_INSTANCES: usize = 4096;

impl VgpuParseState {
    fn new() -> Self {
        Self {
            hosts: HashMap::new(),
            instances: HashMap::new(),
        }
    }

    fn process(
        &mut self,
        metric_name: &str,
        labels: &HashMap<String, String>,
        value: f64,
        host: &str,
    ) {
        let gpu_uuid = labels.get("gpu_uuid").cloned().unwrap_or_default();
        if gpu_uuid.is_empty() {
            return;
        }

        // Drop samples for new hosts once the cap is reached. Updates to an
        // already-tracked host UUID always flow through.
        if !self.hosts.contains_key(&gpu_uuid) && self.hosts.len() >= MAX_VGPU_HOSTS {
            return;
        }

        // Ensure the host row exists before we touch either branch.
        let host_entry = self.hosts.entry(gpu_uuid.clone()).or_insert_with(|| {
            let gpu_index = labels
                .get("gpu_index")
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(0);
            let hostname = labels
                .get("host")
                .or_else(|| labels.get("instance"))
                .cloned()
                .unwrap_or_else(|| host.to_string());
            VgpuHostInfo {
                host_id: host.to_string(),
                hostname: hostname.clone(),
                instance: hostname,
                gpu_index,
                gpu_uuid: gpu_uuid.clone(),
                gpu_name: labels.get("gpu").cloned().unwrap_or_default(),
                host_mode: "Disabled".to_string(),
                scheduler_policy: 0,
                scheduler_arr_mode: 0,
                is_arr_supported: false,
                vgpus: Vec::new(),
                detail: HashMap::new(),
            }
        });

        match metric_name {
            "vgpu_host_mode" => {
                if let Some(mode) = labels.get("host_mode") {
                    host_entry.host_mode = mode.clone();
                }
            }
            "vgpu_scheduler_state" => {
                host_entry.scheduler_arr_mode = value as u32;
                if let Some(flag) = labels.get("arr_supported") {
                    host_entry.is_arr_supported = flag == "true";
                }
            }
            "vgpu_scheduler_policy" => {
                host_entry.scheduler_policy = value as u32;
            }
            _ => {
                // Per-instance metric families: require a vgpu_id label.
                let Some(vgpu_id) = labels.get("vgpu_id").and_then(|s| s.parse::<u32>().ok())
                else {
                    return;
                };
                let instance_key = (gpu_uuid.clone(), vgpu_id);
                // Drop new-instance samples once the cap is reached. Updates
                // to an already-tracked instance always flow through.
                if !self.instances.contains_key(&instance_key)
                    && self.instances.len() >= MAX_VGPU_INSTANCES
                {
                    return;
                }
                let entry = self
                    .instances
                    .entry(instance_key)
                    .or_insert_with(|| VgpuInfo {
                        instance_id: vgpu_id,
                        uuid: labels.get("vgpu_uuid").cloned().unwrap_or_default(),
                        // Keep vm_id round-tripping through the Prometheus
                        // exporter so remote mode can display the same `vm=`
                        // column as local mode.
                        vm_id: labels.get("vgpu_vm_id").cloned().unwrap_or_default(),
                        vgpu_type_name: labels.get("vgpu_type").cloned().unwrap_or_default(),
                        fb_used_bytes: 0,
                        fb_total_bytes: 0,
                        gpu_utilization: None,
                        memory_utilization: None,
                        is_active: false,
                    });

                match metric_name {
                    "vgpu_utilization" => entry.gpu_utilization = Some(value as u32),
                    "vgpu_memory_utilization" => entry.memory_utilization = Some(value as u32),
                    "vgpu_memory_used_bytes" => entry.fb_used_bytes = value as u64,
                    "vgpu_memory_total_bytes" => entry.fb_total_bytes = value as u64,
                    "vgpu_active" => entry.is_active = value > 0.0,
                    _ => {}
                }
            }
        }
    }

    fn finish(mut self) -> Vec<VgpuHostInfo> {
        // Attach instances to their owning host rows.
        for ((gpu_uuid, _vgpu_id), vgpu) in self.instances {
            if let Some(host) = self.hosts.get_mut(&gpu_uuid) {
                host.vgpus.push(vgpu);
            }
        }
        // Deterministic order: instance_id ascending inside each host.
        for host in self.hosts.values_mut() {
            host.vgpus.sort_by_key(|v| v.instance_id);
        }
        let mut out: Vec<VgpuHostInfo> = self.hosts.into_values().collect();
        out.sort_by_key(|h| h.gpu_index);
        out
    }
}

/// Accumulator used while parsing MIG Prometheus lines.
///
/// Per-instance metrics are keyed by `(gpu_uuid, mig_instance)` so they merge
/// into a single [`MigInstanceInfo`] even if emitted across multiple metric
/// families. Host-scoped metrics (`gpu_mig_mode`) populate the parent
/// [`MigGpuInfo`] keyed solely by `gpu_uuid`.
struct MigParseState {
    /// `gpu_uuid -> MigGpuInfo` being assembled.
    hosts: HashMap<String, MigGpuInfo>,
    /// `(gpu_uuid, mig_instance) -> MigInstanceInfo` accumulator.
    instances: HashMap<(String, u32), MigInstanceInfo>,
    /// UUIDs that received an explicit `gpu_mig_mode` line during this scrape.
    /// Used by `finish` to retain disabled-MIG rows (mode=0, zero instances)
    /// that were deliberately emitted by the remote exporter, while still
    /// dropping spurious hosts whose `gpu_uuid` appeared only in some other
    /// (non-MIG) family's labels by accident.
    hosts_with_mode: HashSet<String>,
}

/// Per-scrape cap on distinct MIG-capable GPUs. Matches the existing
/// `MAX_DEVICES_PER_TYPE` and `MAX_VGPU_HOSTS` ceilings so a malicious remote
/// exporter cannot exhaust memory by advertising unbounded `gpu_uuid` values.
const MAX_MIG_GPUS: usize = 256;
/// Per-scrape cap on distinct `(gpu_uuid, mig_instance)` tuples. A100/H100
/// support up to 7 instances per GPU, so 4096 leaves plenty of headroom for
/// large clusters without inviting an OOM via crafted input.
const MAX_MIG_INSTANCES: usize = 4096;
/// Per-instance cap on the `mig_instance` index value itself. MIG hardware
/// caps at 7 today; we accept up to 64 to leave headroom for future
/// architectures while still rejecting obviously bogus indices like
/// `mig_instance="9999"` from a hostile remote.
const MAX_MIG_INSTANCE_INDEX: u32 = 64;

impl MigParseState {
    fn new() -> Self {
        Self {
            hosts: HashMap::new(),
            instances: HashMap::new(),
            hosts_with_mode: HashSet::new(),
        }
    }

    fn process(
        &mut self,
        metric_name: &str,
        labels: &HashMap<String, String>,
        value: f64,
        host: &str,
    ) {
        let gpu_uuid = labels.get("gpu_uuid").cloned().unwrap_or_default();
        if gpu_uuid.is_empty() {
            return;
        }

        // Drop samples for new hosts once the cap is reached. Updates to an
        // already-tracked host UUID always flow through.
        if !self.hosts.contains_key(&gpu_uuid) && self.hosts.len() >= MAX_MIG_GPUS {
            return;
        }

        // Ensure the host row exists before we touch either branch.
        let host_entry = self.hosts.entry(gpu_uuid.clone()).or_insert_with(|| {
            let gpu_index = labels
                .get("gpu_index")
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(0);
            let hostname = labels
                .get("host")
                .or_else(|| labels.get("instance"))
                .cloned()
                .unwrap_or_else(|| host.to_string());
            MigGpuInfo {
                host_id: host.to_string(),
                hostname: hostname.clone(),
                instance: hostname,
                gpu_index,
                gpu_uuid: gpu_uuid.clone(),
                gpu_name: labels.get("gpu").cloned().unwrap_or_default(),
                mig_mode: false,
                instances: Vec::new(),
            }
        });

        match metric_name {
            "gpu_mig_mode" => {
                // Exporter encodes 1=enabled, 0=disabled. Treat any positive
                // value defensively as "enabled" rather than panicking on a
                // weird remote payload.
                host_entry.mig_mode = value > 0.0;
                // Remember that this host was explicitly observed via the
                // MIG-mode metric so `finish` retains it even with mode=0
                // and zero instances. Without this, disabled GPUs would be
                // silently dropped and the metric would be unobservable on
                // the consumer side.
                self.hosts_with_mode.insert(gpu_uuid.clone());
            }
            _ => {
                // Per-instance metric families: require a `mig_instance` label
                // and reject indices beyond the defensive cap.
                let Some(mig_instance) = labels
                    .get("mig_instance")
                    .and_then(|s| s.parse::<u32>().ok())
                else {
                    return;
                };
                if mig_instance > MAX_MIG_INSTANCE_INDEX {
                    return;
                }
                let instance_key = (gpu_uuid.clone(), mig_instance);

                // Drop new-instance samples once the cap is reached. Updates
                // to an already-tracked instance always flow through.
                if !self.instances.contains_key(&instance_key)
                    && self.instances.len() >= MAX_MIG_INSTANCES
                {
                    return;
                }

                let entry = self
                    .instances
                    .entry(instance_key)
                    .or_insert_with(|| MigInstanceInfo {
                        instance_id: mig_instance,
                        gpu_instance_id: labels
                            .get("gpu_instance_id")
                            .and_then(|s| s.parse::<u32>().ok()),
                        compute_instance_id: labels
                            .get("compute_instance_id")
                            .and_then(|s| s.parse::<u32>().ok()),
                        uuid: labels.get("mig_uuid").cloned().unwrap_or_default(),
                        profile_name: labels.get("mig_profile").cloned().unwrap_or_default(),
                        utilization_gpu: None,
                        utilization_memory: None,
                        memory_used_bytes: 0,
                        memory_total_bytes: 0,
                    });

                match metric_name {
                    "mig_instance_utilization_gpu" => {
                        entry.utilization_gpu = Some(value as u32);
                    }
                    "mig_instance_utilization_memory" => {
                        entry.utilization_memory = Some(value as u32);
                    }
                    "mig_instance_memory_used_bytes" => {
                        entry.memory_used_bytes = value as u64;
                    }
                    "mig_instance_memory_total_bytes" => {
                        entry.memory_total_bytes = value as u64;
                    }
                    _ => {}
                }
            }
        }
    }

    fn finish(mut self) -> Vec<MigGpuInfo> {
        // Attach instances to their owning host rows.
        for ((gpu_uuid, _mig_instance), instance) in self.instances {
            if let Some(host) = self.hosts.get_mut(&gpu_uuid) {
                host.instances.push(instance);
            }
        }
        // Deterministic order: instance_id ascending inside each host.
        for host in self.hosts.values_mut() {
            host.instances.sort_by_key(|i| i.instance_id);
        }
        // Retain a host when it either:
        //   * received an explicit `gpu_mig_mode` line (enabled or disabled),
        //     so disabled parent GPUs remain observable to consumers, or
        //   * has at least one MIG instance attached.
        // Hosts whose `gpu_uuid` appeared only incidentally on non-MIG
        // metrics still drop out here.
        let hosts_with_mode = &self.hosts_with_mode;
        self.hosts
            .retain(|uuid, h| hosts_with_mode.contains(uuid) || !h.instances.is_empty());
        // Enforce the invariant: mig_mode must be true whenever instances are
        // present. A remote feed may emit `all_smi_mig_instance_*` lines for a
        // UUID without a corresponding `gpu_mig_mode` line. The retain above
        // keeps such hosts alive via the `!h.instances.is_empty()` arm, but
        // they would carry `mig_mode=false`, which is a contradictory "ghost"
        // state that no real exporter produces. Infer the mode from presence.
        for host in self.hosts.values_mut() {
            if !host.instances.is_empty() {
                host.mig_mode = true;
            }
        }
        let mut out: Vec<MigGpuInfo> = self.hosts.into_values().collect();
        out.sort_by_key(|h| h.gpu_index);
        out
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

        let (gpu_info, _, _, _, _, _) = parser.parse_metrics(test_data, host, &re);

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
    fn test_parse_gpu_thermal_thresholds_and_pstate() {
        let parser = create_test_parser();
        let re = create_test_regex();
        let host = "127.0.0.1:10058";

        let test_data = r#"
all_smi_gpu_utilization{gpu="NVIDIA A100", instance="node-1", uuid="GPU-T", index="0"} 30
all_smi_gpu_temperature_celsius{gpu="NVIDIA A100", instance="node-1", uuid="GPU-T", index="0"} 65
all_smi_gpu_temperature_threshold_slowdown_celsius{gpu="NVIDIA A100", instance="node-1", uuid="GPU-T", index="0"} 90
all_smi_gpu_temperature_threshold_shutdown_celsius{gpu="NVIDIA A100", instance="node-1", uuid="GPU-T", index="0"} 95
all_smi_gpu_temperature_threshold_max_operating_celsius{gpu="NVIDIA A100", instance="node-1", uuid="GPU-T", index="0"} 85
all_smi_gpu_temperature_threshold_acoustic_celsius{gpu="NVIDIA A100", instance="node-1", uuid="GPU-T", index="0"} 77
all_smi_gpu_performance_state{gpu="NVIDIA A100", instance="node-1", uuid="GPU-T", index="0"} 2
"#;

        let (gpu_info, _, _, _, _, _) = parser.parse_metrics(test_data, host, &re);
        assert_eq!(gpu_info.len(), 1);
        let gpu = &gpu_info[0];
        assert_eq!(gpu.temperature_threshold_slowdown, Some(90));
        assert_eq!(gpu.temperature_threshold_shutdown, Some(95));
        assert_eq!(gpu.temperature_threshold_max_operating, Some(85));
        assert_eq!(gpu.temperature_threshold_acoustic, Some(77));
        assert_eq!(gpu.performance_state, Some(2));
    }

    #[test]
    fn test_parse_gpu_round_trip_preserves_absence() {
        // Round-trip: a scrape from an older all-smi node without the new
        // metrics must leave the new fields as `None`, not defaults.
        let parser = create_test_parser();
        let re = create_test_regex();
        let host = "127.0.0.1:10058";

        let test_data = r#"
all_smi_gpu_utilization{gpu="NVIDIA A100", instance="node-1", uuid="GPU-A", index="0"} 40
all_smi_gpu_temperature_celsius{gpu="NVIDIA A100", instance="node-1", uuid="GPU-A", index="0"} 60
"#;
        let (gpu_info, _, _, _, _, _) = parser.parse_metrics(test_data, host, &re);
        assert_eq!(gpu_info.len(), 1);
        let gpu = &gpu_info[0];
        assert!(gpu.temperature_threshold_slowdown.is_none());
        assert!(gpu.temperature_threshold_shutdown.is_none());
        assert!(gpu.performance_state.is_none());
    }

    #[test]
    fn parser_rejects_out_of_range_pstate() {
        // NVML defines P0–P15 only. Values outside [0, 15] from a
        // malicious or buggy upstream must be silently dropped so the TUI
        // never renders "P9999" or similar.
        let parser = create_test_parser();
        let re = create_test_regex();
        let host = "127.0.0.1:10058";

        for bad_value in [16.0_f64, 100.0, 9999.0, -1.0] {
            let test_data = format!(
                "all_smi_gpu_utilization{{gpu=\"GPU\", instance=\"n\", uuid=\"GPU-BAD\", index=\"0\"}} 0\n\
                 all_smi_gpu_performance_state{{gpu=\"GPU\", instance=\"n\", uuid=\"GPU-BAD\", index=\"0\"}} {bad_value}\n"
            );
            let (gpu_info, _, _, _, _, _) = parser.parse_metrics(&test_data, host, &re);
            assert_eq!(gpu_info.len(), 1);
            assert!(
                gpu_info[0].performance_state.is_none(),
                "expected None for out-of-range pstate {bad_value}, got {:?}",
                gpu_info[0].performance_state
            );
        }
    }

    #[test]
    fn test_saturating_u32_helper() {
        assert_eq!(saturating_u32(-1.0), None);
        assert_eq!(saturating_u32(f64::NAN), None);
        assert_eq!(saturating_u32(0.0), Some(0));
        assert_eq!(saturating_u32(93.0), Some(93));
        assert_eq!(saturating_u32(1e12), Some(u32::MAX));
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

        let (_, cpu_info, _, _, _, _) = parser.parse_metrics(test_data, host, &re);

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

        let (_, cpu_info, _, _, _, _) = parser.parse_metrics(test_data, host, &re);

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

        let (_, _, memory_info, _, _, _) = parser.parse_metrics(test_data, host, &re);

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

        let (_, _, _, storage_info, _, _) = parser.parse_metrics(test_data, host, &re);

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

        let (gpu_info, cpu_info, memory_info, storage_info, _, _) =
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

        let (gpu_info, cpu_info, memory_info, storage_info, _, _) =
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

        let (gpu_info, cpu_info, memory_info, storage_info, _, _) =
            parser.parse_metrics("", host, &re);

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

        let (gpu_info, cpu_info, _, _, _, _) = parser.parse_metrics(test_data, host, &re);

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

            let (_, cpu_info, _, _, _, _) = parser.parse_metrics(&test_data, host, &re);
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

        let (gpu_info, _, _, storage_info, _, _) = parser.parse_metrics(test_data, host, &re);

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

        let (_, cpu_info, _, _, _, _) = parser.parse_metrics(test_data, host, &re);

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

        let (_, cpu_info, _, _, _, _) = parser.parse_metrics(test_data, host, &re);

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

        let (_, cpu_info, _, _, _, _) = parser.parse_metrics(test_data, host, &re);

        assert_eq!(cpu_info.len(), 1);
        let cpu = &cpu_info[0];
        let apple_info = cpu.apple_silicon_info.as_ref().unwrap();

        // s_core_count defaults to 0 when not present in metrics
        assert_eq!(apple_info.s_core_count, 0);
        assert_eq!(apple_info.s_core_utilization, 0.0);
        assert_eq!(apple_info.p_core_count, 8);
        assert_eq!(apple_info.e_core_count, 4);
    }

    #[test]
    fn test_parse_vgpu_metrics_populates_host_and_instances() {
        let parser = create_test_parser();
        let re = create_test_regex();
        let host = "127.0.0.1:10100";

        let text = r#"
all_smi_vgpu_host_mode{gpu_index="0", gpu_uuid="GPU-A", gpu="NVIDIA A100", instance="node1", host="node1", host_mode="Sriov"} 1
all_smi_vgpu_scheduler_state{gpu_index="0", gpu_uuid="GPU-A", gpu="NVIDIA A100", instance="node1", host="node1", arr_supported="true"} 2
all_smi_vgpu_scheduler_policy{gpu_index="0", gpu_uuid="GPU-A", gpu="NVIDIA A100", instance="node1", host="node1"} 1
all_smi_vgpu_utilization{gpu_index="0", gpu_uuid="GPU-A", gpu="NVIDIA A100", instance="node1", host="node1", vgpu_id="0", vgpu_uuid="GRID-1", vgpu_type="GRID A100-8C"} 55
all_smi_vgpu_memory_used_bytes{gpu_index="0", gpu_uuid="GPU-A", gpu="NVIDIA A100", instance="node1", host="node1", vgpu_id="0", vgpu_uuid="GRID-1", vgpu_type="GRID A100-8C"} 8589934592
all_smi_vgpu_memory_total_bytes{gpu_index="0", gpu_uuid="GPU-A", gpu="NVIDIA A100", instance="node1", host="node1", vgpu_id="0", vgpu_uuid="GRID-1", vgpu_type="GRID A100-8C"} 17179869184
all_smi_vgpu_memory_utilization{gpu_index="0", gpu_uuid="GPU-A", gpu="NVIDIA A100", instance="node1", host="node1", vgpu_id="0", vgpu_uuid="GRID-1", vgpu_type="GRID A100-8C"} 30
all_smi_vgpu_active{gpu_index="0", gpu_uuid="GPU-A", gpu="NVIDIA A100", instance="node1", host="node1", vgpu_id="0", vgpu_uuid="GRID-1", vgpu_type="GRID A100-8C"} 1
all_smi_vgpu_utilization{gpu_index="0", gpu_uuid="GPU-A", gpu="NVIDIA A100", instance="node1", host="node1", vgpu_id="1", vgpu_uuid="GRID-2", vgpu_type="GRID A100-4C"} 10
"#;

        let (_gpu, _cpu, _mem, _storage, vgpu, _) = parser.parse_metrics(text, host, &re);

        assert_eq!(vgpu.len(), 1, "expected one host record");
        let host0 = &vgpu[0];
        assert_eq!(host0.host_mode, "Sriov");
        assert_eq!(host0.scheduler_policy, 1);
        assert_eq!(host0.scheduler_arr_mode, 2);
        assert!(host0.is_arr_supported);
        assert_eq!(host0.gpu_uuid, "GPU-A");
        assert_eq!(host0.gpu_name, "NVIDIA A100");
        assert_eq!(host0.vgpus.len(), 2);
        assert_eq!(host0.vgpus[0].instance_id, 0);
        assert_eq!(host0.vgpus[0].gpu_utilization, Some(55));
        assert_eq!(host0.vgpus[0].memory_utilization, Some(30));
        assert_eq!(host0.vgpus[0].fb_used_bytes, 8_589_934_592);
        assert_eq!(host0.vgpus[0].fb_total_bytes, 17_179_869_184);
        assert!(host0.vgpus[0].is_active);
        assert_eq!(host0.vgpus[1].instance_id, 1);
        assert_eq!(host0.vgpus[1].gpu_utilization, Some(10));
    }

    #[test]
    fn test_parse_vgpu_metrics_skips_rows_without_gpu_uuid() {
        let parser = create_test_parser();
        let re = create_test_regex();
        let host = "127.0.0.1:10101";
        let text = r#"
all_smi_vgpu_utilization{instance="node1", vgpu_id="0"} 55
"#;
        let (_, _, _, _, vgpu, _) = parser.parse_metrics(text, host, &re);
        assert!(vgpu.is_empty());
    }

    #[test]
    fn test_parse_non_vgpu_host_produces_no_vgpu_rows() {
        let parser = create_test_parser();
        let re = create_test_regex();
        let host = "127.0.0.1:10102";

        let text = r#"
all_smi_gpu_utilization{gpu="NVIDIA A100", instance="node1", uuid="GPU-X", index="0"} 50
all_smi_cpu_utilization{cpu_model="AMD", instance="node1", hostname="node1", index="0"} 20
"#;
        let (gpu, _cpu, _mem, _store, vgpu, _) = parser.parse_metrics(text, host, &re);
        assert_eq!(gpu.len(), 1);
        assert!(
            vgpu.is_empty(),
            "No vGPU rows must be emitted for a bare-metal host"
        );
    }

    #[test]
    fn vgpu_parser_caps_host_count() {
        let parser = create_test_parser();
        let re = create_test_regex();
        let host = "127.0.0.1:10103";

        // Feed 2 * MAX_VGPU_HOSTS unique host UUIDs. Only the first
        // MAX_VGPU_HOSTS may survive; the rest must be dropped silently.
        let mut text = String::new();
        for i in 0..(2 * MAX_VGPU_HOSTS) {
            text.push_str(&format!(
                r#"all_smi_vgpu_host_mode{{gpu_index="0", gpu_uuid="GPU-{i}", gpu="NVIDIA A100", instance="node1", host="node1", host_mode="Sriov"}} 1
"#
            ));
        }
        let (_gpu, _cpu, _mem, _storage, vgpu, _) = parser.parse_metrics(&text, host, &re);
        assert_eq!(vgpu.len(), MAX_VGPU_HOSTS, "host count must not exceed cap");
    }

    #[test]
    fn vgpu_parser_caps_instance_count() {
        let parser = create_test_parser();
        let re = create_test_regex();
        let host = "127.0.0.1:10104";

        // Generate a single host with 2 * MAX_VGPU_INSTANCES unique vGPU ids.
        // Each is emitted via a vgpu_utilization line — we pick a scrape-style
        // metric so the new-instance branch is the one being gated. After
        // parsing, at most MAX_VGPU_INSTANCES instances may appear.
        let mut text = String::new();
        // Establish the host first so the host row exists.
        text.push_str(
            r#"all_smi_vgpu_host_mode{gpu_index="0", gpu_uuid="GPU-A", gpu="NVIDIA A100", instance="node1", host="node1", host_mode="Sriov"} 1
"#,
        );
        for i in 0..(2 * MAX_VGPU_INSTANCES) {
            text.push_str(&format!(
                r#"all_smi_vgpu_utilization{{gpu_index="0", gpu_uuid="GPU-A", gpu="NVIDIA A100", instance="node1", host="node1", vgpu_id="{i}", vgpu_uuid="GRID-{i}", vgpu_type="GRID A100-8C"}} 1
"#
            ));
        }
        let (_gpu, _cpu, _mem, _storage, vgpu, _) = parser.parse_metrics(&text, host, &re);
        assert_eq!(vgpu.len(), 1, "single host must still exist");
        assert_eq!(
            vgpu[0].vgpus.len(),
            MAX_VGPU_INSTANCES,
            "instance count must not exceed cap"
        );
    }

    #[test]
    fn parse_labels_is_quote_aware_against_comma_injection() {
        let parser = create_test_parser();
        // A malicious VM-controlled value embeds `",` and `\"` so a naive
        // splitter would turn one label into three. The quote-aware
        // implementation must see exactly three labels.
        let labels_str = r#"gpu_uuid="a",vgpu_vm_id="pwned\", fake=\"evil",vgpu_id="0""#;
        let labels = parser.parse_labels(labels_str);
        assert_eq!(labels.len(), 3, "got labels: {labels:?}");
        assert_eq!(labels.get("gpu_uuid").unwrap(), "a");
        assert_eq!(labels.get("vgpu_id").unwrap(), "0");
        // vm_id must contain the attacker's payload verbatim, but never be
        // interpreted as additional labels.
        assert_eq!(labels.get("vgpu_vm_id").unwrap(), r#"pwned", fake="evil"#);
        assert!(!labels.contains_key("fake"), "no injected label allowed");
    }

    #[test]
    fn parse_labels_unescapes_newline_and_carriage_return() {
        let parser = create_test_parser();
        // Prometheus escape sequences must be reversed on ingest so the
        // parser sees the same bytes the exporter originally held.
        let labels_str = r#"key="line1\nline2\rend""#;
        let labels = parser.parse_labels(labels_str);
        assert_eq!(labels.get("key").unwrap(), "line1\nline2\rend");
    }

    #[test]
    fn test_parse_mig_metrics_populates_host_and_instances() {
        let parser = create_test_parser();
        let re = create_test_regex();
        let host = "127.0.0.1:10200";

        let text = r#"
all_smi_gpu_mig_mode{gpu_index="0", gpu_uuid="GPU-M", gpu="NVIDIA A100", instance="node1", host="node1"} 1
all_smi_mig_instance_utilization_gpu{gpu_index="0", gpu_uuid="GPU-M", gpu="NVIDIA A100", instance="node1", host="node1", mig_instance="0", mig_uuid="MIG-1", mig_profile="1g.5gb", gpu_instance_id="7", compute_instance_id="0"} 55
all_smi_mig_instance_utilization_memory{gpu_index="0", gpu_uuid="GPU-M", gpu="NVIDIA A100", instance="node1", host="node1", mig_instance="0", mig_uuid="MIG-1", mig_profile="1g.5gb", gpu_instance_id="7", compute_instance_id="0"} 30
all_smi_mig_instance_memory_used_bytes{gpu_index="0", gpu_uuid="GPU-M", gpu="NVIDIA A100", instance="node1", host="node1", mig_instance="0", mig_uuid="MIG-1", mig_profile="1g.5gb", gpu_instance_id="7", compute_instance_id="0"} 1073741824
all_smi_mig_instance_memory_total_bytes{gpu_index="0", gpu_uuid="GPU-M", gpu="NVIDIA A100", instance="node1", host="node1", mig_instance="0", mig_uuid="MIG-1", mig_profile="1g.5gb", gpu_instance_id="7", compute_instance_id="0"} 5368709120
all_smi_mig_instance_utilization_gpu{gpu_index="0", gpu_uuid="GPU-M", gpu="NVIDIA A100", instance="node1", host="node1", mig_instance="1", mig_uuid="MIG-2", mig_profile="2g.10gb", gpu_instance_id="2", compute_instance_id="0"} 10
"#;

        let (_gpu, _cpu, _mem, _storage, _vgpu, mig) = parser.parse_metrics(text, host, &re);

        assert_eq!(mig.len(), 1, "expected one MIG host record");
        let host0 = &mig[0];
        assert!(host0.mig_mode);
        assert_eq!(host0.gpu_uuid, "GPU-M");
        assert_eq!(host0.gpu_name, "NVIDIA A100");
        assert_eq!(host0.gpu_index, 0);
        assert_eq!(host0.instances.len(), 2);

        let inst0 = &host0.instances[0];
        assert_eq!(inst0.instance_id, 0);
        assert_eq!(inst0.uuid, "MIG-1");
        assert_eq!(inst0.profile_name, "1g.5gb");
        assert_eq!(inst0.gpu_instance_id, Some(7));
        assert_eq!(inst0.compute_instance_id, Some(0));
        assert_eq!(inst0.utilization_gpu, Some(55));
        assert_eq!(inst0.utilization_memory, Some(30));
        assert_eq!(inst0.memory_used_bytes, 1_073_741_824);
        assert_eq!(inst0.memory_total_bytes, 5_368_709_120);

        let inst1 = &host0.instances[1];
        assert_eq!(inst1.instance_id, 1);
        assert_eq!(inst1.utilization_gpu, Some(10));
    }

    #[test]
    fn test_parse_mig_metrics_skips_rows_without_gpu_uuid() {
        let parser = create_test_parser();
        let re = create_test_regex();
        let host = "127.0.0.1:10201";
        let text = r#"
all_smi_mig_instance_utilization_gpu{instance="node1", mig_instance="0"} 55
"#;
        let (_, _, _, _, _, mig) = parser.parse_metrics(text, host, &re);
        assert!(mig.is_empty());
    }

    #[test]
    fn test_parse_non_mig_host_produces_no_mig_rows() {
        let parser = create_test_parser();
        let re = create_test_regex();
        let host = "127.0.0.1:10202";

        let text = r#"
all_smi_gpu_utilization{gpu="NVIDIA A100", instance="node1", uuid="GPU-X", index="0"} 50
all_smi_cpu_utilization{cpu_model="AMD", instance="node1", hostname="node1", index="0"} 20
"#;
        let (gpu, _cpu, _mem, _store, _vgpu, mig) = parser.parse_metrics(text, host, &re);
        assert_eq!(gpu.len(), 1);
        assert!(
            mig.is_empty(),
            "No MIG rows must be emitted for a bare-metal host"
        );
    }

    #[test]
    fn mig_parser_records_disabled_mode_when_only_mode_metric_present() {
        // A parent GPU with MIG mode disabled and no instances must still
        // surface one row with `mig_mode=false`. Dashboards rely on this to
        // track runtime MIG toggles and cluster-wide MIG enablement rollout;
        // silently dropping disabled rows would make the metric unobservable.
        let parser = create_test_parser();
        let re = create_test_regex();
        let host = "127.0.0.1:10203";

        let text = r#"
all_smi_gpu_mig_mode{gpu_index="0", gpu_uuid="GPU-X", gpu="NVIDIA A100", instance="node1", host="node1"} 0
"#;
        let (_, _, _, _, _, mig) = parser.parse_metrics(text, host, &re);
        assert_eq!(
            mig.len(),
            1,
            "Disabled-MIG row must be retained so consumers can see mode=0"
        );
        assert!(!mig[0].mig_mode);
        assert_eq!(mig[0].gpu_uuid, "GPU-X");
        assert!(mig[0].instances.is_empty());
    }

    #[test]
    fn mig_parser_caps_host_count() {
        let parser = create_test_parser();
        let re = create_test_regex();
        let host = "127.0.0.1:10204";

        // Feed 2 * MAX_MIG_GPUS unique host UUIDs. Only the first
        // MAX_MIG_GPUS may survive; the rest must be dropped silently.
        let mut text = String::new();
        for i in 0..(2 * MAX_MIG_GPUS) {
            text.push_str(&format!(
                r#"all_smi_gpu_mig_mode{{gpu_index="0", gpu_uuid="GPU-{i}", gpu="NVIDIA A100", instance="node1", host="node1"}} 1
"#
            ));
        }
        let (_, _, _, _, _, mig) = parser.parse_metrics(&text, host, &re);
        assert_eq!(mig.len(), MAX_MIG_GPUS, "host count must not exceed cap");
    }

    #[test]
    fn mig_parser_caps_instance_count() {
        let parser = create_test_parser();
        let re = create_test_regex();
        let host = "127.0.0.1:10205";

        // Single host with 2 * MAX_MIG_INSTANCES unique mig_instance ids.
        // Indices beyond MAX_MIG_INSTANCE_INDEX (64) are also dropped by the
        // per-index defensive cap, so this test exercises both layers.
        let mut text = String::new();
        text.push_str(
            r#"all_smi_gpu_mig_mode{gpu_index="0", gpu_uuid="GPU-A", gpu="NVIDIA A100", instance="node1", host="node1"} 1
"#,
        );
        for i in 0..(2 * MAX_MIG_INSTANCES) {
            text.push_str(&format!(
                r#"all_smi_mig_instance_utilization_gpu{{gpu_index="0", gpu_uuid="GPU-A", gpu="NVIDIA A100", instance="node1", host="node1", mig_instance="{i}", mig_uuid="MIG-{i}", mig_profile="1g.5gb"}} 1
"#
            ));
        }
        let (_, _, _, _, _, mig) = parser.parse_metrics(&text, host, &re);
        assert_eq!(mig.len(), 1, "single host must still exist");
        // Per-index cap kicks in first (only IDs 0..=64 survive), so the
        // visible instance count is bounded by MAX_MIG_INSTANCE_INDEX + 1.
        assert!(
            mig[0].instances.len() <= (MAX_MIG_INSTANCE_INDEX as usize) + 1,
            "per-index cap must clamp instance count, got {}",
            mig[0].instances.len()
        );
    }

    #[test]
    fn mig_parser_drops_rows_with_oversized_instance_index() {
        let parser = create_test_parser();
        let re = create_test_regex();
        let host = "127.0.0.1:10206";

        // mig_instance="9999" exceeds the defensive cap (64) — must be
        // dropped silently. The parent host record only surfaces because of
        // the gpu_mig_mode line.
        let text = r#"
all_smi_gpu_mig_mode{gpu_index="0", gpu_uuid="GPU-A", gpu="NVIDIA A100", instance="node1", host="node1"} 1
all_smi_mig_instance_utilization_gpu{gpu_index="0", gpu_uuid="GPU-A", gpu="NVIDIA A100", instance="node1", host="node1", mig_instance="9999", mig_uuid="MIG-X", mig_profile="1g.5gb"} 50
"#;
        let (_, _, _, _, _, mig) = parser.parse_metrics(text, host, &re);
        assert_eq!(mig.len(), 1, "host record present");
        assert!(
            mig[0].instances.is_empty(),
            "oversized mig_instance must be rejected"
        );
    }

    #[test]
    fn mig_parser_handles_missing_optional_ids() {
        // Older drivers may not emit gpu_instance_id / compute_instance_id.
        // Empty values must round-trip as None rather than panic.
        let parser = create_test_parser();
        let re = create_test_regex();
        let host = "127.0.0.1:10207";

        let text = r#"
all_smi_gpu_mig_mode{gpu_index="0", gpu_uuid="GPU-A", gpu="NVIDIA A100", instance="node1", host="node1"} 1
all_smi_mig_instance_utilization_gpu{gpu_index="0", gpu_uuid="GPU-A", gpu="NVIDIA A100", instance="node1", host="node1", mig_instance="0", mig_uuid="MIG-X", mig_profile="1g.5gb", gpu_instance_id="", compute_instance_id=""} 25
"#;
        let (_, _, _, _, _, mig) = parser.parse_metrics(text, host, &re);
        assert_eq!(mig.len(), 1);
        assert_eq!(mig[0].instances.len(), 1);
        assert!(mig[0].instances[0].gpu_instance_id.is_none());
        assert!(mig[0].instances[0].compute_instance_id.is_none());
    }

    #[test]
    fn mig_parser_infers_mig_mode_from_instance_presence() {
        // When a remote feed emits `all_smi_mig_instance_*` lines for a UUID
        // without a `gpu_mig_mode` line, the host would survive the retain
        // (via the `!instances.is_empty()` arm) but carry `mig_mode=false` —
        // a contradictory "ghost" state. The parser must infer `mig_mode=true`
        // whenever instances are present.
        let parser = create_test_parser();
        let re = create_test_regex();
        let host = "127.0.0.1:10208";

        // No gpu_mig_mode line — only instance metrics.
        let text = r#"
all_smi_mig_instance_utilization_gpu{gpu_index="0", gpu_uuid="GPU-G", gpu="NVIDIA A100", instance="node1", host="node1", mig_instance="0", mig_uuid="MIG-G1", mig_profile="1g.5gb", gpu_instance_id="7", compute_instance_id="0"} 42
"#;
        let (_, _, _, _, _, mig) = parser.parse_metrics(text, host, &re);
        assert_eq!(mig.len(), 1, "host must survive via instance presence");
        assert!(
            mig[0].mig_mode,
            "mig_mode must be inferred as true when instances are present"
        );
        assert_eq!(mig[0].instances.len(), 1);
        assert_eq!(mig[0].instances[0].uuid, "MIG-G1");
    }

    #[test]
    fn parser_rejects_out_of_range_cpu_core_id() {
        let parser = create_test_parser();
        let re = create_test_regex();
        let host = "127.0.0.1:10058";

        // First emit a valid CPU metric so the cpu_info entry exists, then
        // inject an out-of-range core_id that must be silently dropped.
        let text = r#"
all_smi_cpu_utilization{cpu_model="Intel Xeon", instance="node-1", index="0"} 50
all_smi_cpu_core_utilization{cpu_model="Intel Xeon", instance="node-1", index="0", core_id="2000", core_type="S"} 99
"#;
        let (_, cpu_info, _, _, _, _) = parser.parse_metrics(text, host, &re);
        assert_eq!(cpu_info.len(), 1);
        // The out-of-range core_id=2000 must have been rejected — the
        // per_core_utilization vector must remain empty (or at least not
        // grow to 2001 elements).
        assert!(
            cpu_info[0].per_core_utilization.is_empty(),
            "per_core_utilization must be empty when core_id >= MAX_CPU_CORES, got len={}",
            cpu_info[0].per_core_utilization.len()
        );
    }
}
