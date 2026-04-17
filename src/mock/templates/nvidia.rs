//! NVIDIA GPU mock template generator

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

use crate::mock::metrics::{CpuMetrics, GpuMetrics, MemoryMetrics};
use all_smi::traits::mock_generator::{
    MockConfig, MockData, MockGenerator, MockPlatform, MockResult,
};

/// Environment variable that gates hardware-detail metric emission in NVIDIA
/// mock responses.
///
/// When set to any non-empty value the mock will emit:
/// * NUMA node id
/// * GSP firmware mode and version
/// * NvLink remote endpoint classification per active link
/// * GPM SM occupancy and memory bandwidth utilization (Hopper+)
/// * Thermal thresholds (slowdown, shutdown, max-operating, acoustic)
/// * Canonical P-state gauge (`all_smi_gpu_performance_state`)
///
/// When unset (the default) only basic GPU metrics are emitted, simulating an
/// older driver or a node where NVML does not expose these extended APIs.
pub const HARDWARE_DETAILS_ENV_VAR: &str = "ALL_SMI_MOCK_HARDWARE_DETAILS";

/// `true` when the hardware-detail mock mode is enabled via env var.
pub fn is_hardware_details_enabled() -> bool {
    std::env::var(HARDWARE_DETAILS_ENV_VAR)
        .map(|v| !v.is_empty())
        .unwrap_or(false)
}

/// NVIDIA GPU mock generator
pub struct NvidiaMockGenerator {
    gpu_name: String,
    instance_name: String,
}

impl NvidiaMockGenerator {
    pub fn new(gpu_name: Option<String>, instance_name: String) -> Self {
        Self {
            gpu_name: gpu_name.unwrap_or_else(|| "NVIDIA H100 80GB HBM3".to_string()),
            instance_name,
        }
    }

    /// Build NVIDIA-specific template
    pub fn build_nvidia_template(
        &self,
        gpus: &[GpuMetrics],
        cpu: &CpuMetrics,
        memory: &MemoryMetrics,
    ) -> String {
        let mut template = String::with_capacity(4096);

        // Basic GPU metrics
        self.add_gpu_metrics(&mut template, gpus);

        // NVIDIA-specific extended hardware detail metrics — gated by
        // ALL_SMI_MOCK_HARDWARE_DETAILS so the default bare-metal mock
        // simulates a minimal/older driver that does not expose these APIs.
        //
        // Gated together under one flag because they all originate from the
        // same family of extended NVML calls (thermal thresholds, P-state,
        // NUMA, GSP, NvLink, GPM) and a consumer either has access to all of
        // them or none. Splitting into per-feature flags would add complexity
        // without a clear use case.
        if is_hardware_details_enabled() {
            // Legacy `all_smi_gpu_pstate` gauge — kept for backwards
            // compatibility with scrapers predating issue #130.
            self.add_pstate_metrics(&mut template, gpus);

            // Temperature thresholds + canonical P-state metric (issue #130).
            // Fixed synthetic values that match typical H100/A100 limits.
            self.add_thermal_threshold_metrics(&mut template, gpus);

            // NUMA node id, GSP firmware mode + version, NvLink topology,
            // GPM gauges (issue #132). Representative synthetic values so the
            // TUI, exporter, and remote-parser round-trip paths all compile.
            self.add_hardware_detail_metrics(&mut template, gpus);
        }

        // NVIDIA-specific: Process metrics
        self.add_process_metrics(&mut template, gpus);

        // NVIDIA-specific: Driver metrics
        self.add_driver_metrics(&mut template);

        // CPU and memory metrics
        self.add_system_metrics(&mut template, cpu, memory);

        // Chassis metrics (total power)
        crate::mock::templates::common::add_chassis_metrics(&mut template, &self.instance_name);

        // Optional vGPU metrics — gated by the ALL_SMI_MOCK_VGPU env var so
        // the NVIDIA bare-metal behaviour is unchanged by default.
        crate::mock::templates::vgpu::maybe_add_vgpu_template(
            &mut template,
            &self.instance_name,
            &self.gpu_name,
            gpus,
        );

        // Optional MIG metrics — gated by the ALL_SMI_MOCK_MIG env var.
        // Same default-off contract as vGPU above.
        crate::mock::templates::mig::maybe_add_mig_template(
            &mut template,
            &self.instance_name,
            &self.gpu_name,
            gpus,
        );

        template
    }

    /// Synthesize the new NVML extended-temperature / P-state metrics in the
    /// mock output. Values are fixed-synthetic, not randomized, because
    /// thresholds never change on real hardware.
    fn add_thermal_threshold_metrics(&self, template: &mut String, gpus: &[GpuMetrics]) {
        // Constants chosen to match typical H100 / A100 datacenter values.
        const SLOWDOWN: u32 = 90;
        const SHUTDOWN: u32 = 95;
        const MAX_OPERATING: u32 = 87;
        const ACOUSTIC: u32 = 77;

        for (metric_name, help_text, value) in [
            (
                "all_smi_gpu_temperature_threshold_slowdown_celsius",
                "GPU slowdown temperature threshold in Celsius",
                SLOWDOWN,
            ),
            (
                "all_smi_gpu_temperature_threshold_shutdown_celsius",
                "GPU shutdown temperature threshold in Celsius",
                SHUTDOWN,
            ),
            (
                "all_smi_gpu_temperature_threshold_max_operating_celsius",
                "GPU maximum operating temperature threshold in Celsius",
                MAX_OPERATING,
            ),
            (
                "all_smi_gpu_temperature_threshold_acoustic_celsius",
                "GPU acoustic (noise) temperature threshold in Celsius",
                ACOUSTIC,
            ),
        ] {
            template.push_str(&format!("# HELP {metric_name} {help_text}\n"));
            template.push_str(&format!("# TYPE {metric_name} gauge\n"));
            for (i, gpu) in gpus.iter().enumerate() {
                let labels = format!(
                    "gpu=\"{}\", instance=\"{}\", gpu_uuid=\"{}\", gpu_index=\"{i}\"",
                    self.gpu_name, self.instance_name, gpu.uuid
                );
                template.push_str(&format!("{metric_name}{{{labels}}} {value}\n"));
            }
        }

        // Canonical P-state metric (issue #130). Reuses the same
        // placeholder substitution as the legacy pstate metric so
        // `render_nvidia_response` only needs one replace pass.
        template.push_str(
            "# HELP all_smi_gpu_performance_state GPU performance state \
             (0=P0 fastest, 15=P15 idlest; metric is omitted when the device does not report a P-state)\n",
        );
        template.push_str("# TYPE all_smi_gpu_performance_state gauge\n");
        for (i, gpu) in gpus.iter().enumerate() {
            let labels = format!(
                "gpu=\"{}\", instance=\"{}\", gpu_uuid=\"{}\", gpu_index=\"{i}\"",
                self.gpu_name, self.instance_name, gpu.uuid
            );
            template.push_str(&format!(
                "all_smi_gpu_performance_state{{{labels}}} {{{{PSTATE_{i}}}}}\n"
            ));
        }
    }

    /// Synthesize extended hardware-detail metrics (issue #132):
    ///   * NUMA node id alternating between 0 and 1 across GPUs
    ///   * GSP firmware mode `1=enabled` for every GPU
    ///   * GSP firmware version string `"550.54.15"` for every GPU
    ///   * 6 active NvLinks per GPU, 5 remote=gpu + 1 remote=switch
    ///   * GPM gauges at fixed-plausible values in the 0.45-0.88 band
    ///
    /// Fixed values rather than random numbers keep the mock output stable
    /// across scrapes — hardware details never change at runtime on real
    /// devices, and stable mock values simplify test assertions.
    fn add_hardware_detail_metrics(&self, template: &mut String, gpus: &[GpuMetrics]) {
        const GSP_MODE: u8 = 1;
        const GSP_VERSION: &str = "550.54.15";
        const NVLINK_COUNT: u32 = 6;
        const SM_OCCUPANCY: f32 = 0.67;
        const MEMORY_BW_UTIL: f32 = 0.42;

        // --- NUMA node id ---
        template.push_str(
            "# HELP all_smi_gpu_numa_node_id NUMA node the GPU is attached to \
             (metric is omitted when the host has no NUMA topology or the driver does not report one)\n",
        );
        template.push_str("# TYPE all_smi_gpu_numa_node_id gauge\n");
        for (i, gpu) in gpus.iter().enumerate() {
            let numa = (i as u32) % 2;
            let labels = format!(
                "gpu=\"{}\", instance=\"{}\", gpu_uuid=\"{}\", gpu_index=\"{i}\"",
                self.gpu_name, self.instance_name, gpu.uuid
            );
            template.push_str(&format!("all_smi_gpu_numa_node_id{{{labels}}} {numa}\n"));
        }

        // --- GSP firmware mode ---
        template.push_str(
            "# HELP all_smi_gpu_gsp_firmware_mode GSP firmware mode \
             (0=disabled, 1=enabled, 2=default); omitted when the driver does not expose the GSP firmware API\n",
        );
        template.push_str("# TYPE all_smi_gpu_gsp_firmware_mode gauge\n");
        for (i, gpu) in gpus.iter().enumerate() {
            let labels = format!(
                "gpu=\"{}\", instance=\"{}\", gpu_uuid=\"{}\", gpu_index=\"{i}\"",
                self.gpu_name, self.instance_name, gpu.uuid
            );
            template.push_str(&format!(
                "all_smi_gpu_gsp_firmware_mode{{{labels}}} {GSP_MODE}\n"
            ));
        }

        // --- GSP firmware version info ---
        template.push_str(
            "# HELP all_smi_gpu_gsp_firmware_version_info GSP firmware version, encoded as a constant 1 \
             with the version in a `version` label; omitted when unsupported\n",
        );
        template.push_str("# TYPE all_smi_gpu_gsp_firmware_version_info gauge\n");
        for (i, gpu) in gpus.iter().enumerate() {
            let labels = format!(
                "gpu=\"{}\", instance=\"{}\", gpu_uuid=\"{}\", gpu_index=\"{i}\", version=\"{GSP_VERSION}\"",
                self.gpu_name, self.instance_name, gpu.uuid
            );
            template.push_str(&format!(
                "all_smi_gpu_gsp_firmware_version_info{{{labels}}} 1\n"
            ));
        }

        // --- NvLink remote device types ---
        template.push_str(
            "# HELP all_smi_nvlink_remote_device_type NvLink remote endpoint classification per active link. \
             Value is always 1; classification is carried in the `remote_type` label (gpu / switch / ibmnpu / unknown).\n",
        );
        template.push_str("# TYPE all_smi_nvlink_remote_device_type gauge\n");
        for (i, gpu) in gpus.iter().enumerate() {
            // 5 remote=gpu, 1 remote=switch — typical NVLink topology on
            // HGX-style 8-GPU boards.
            for link in 0..NVLINK_COUNT {
                let remote_type = if link == NVLINK_COUNT - 1 {
                    "switch"
                } else {
                    "gpu"
                };
                let labels = format!(
                    "gpu=\"{}\", instance=\"{}\", gpu_uuid=\"{}\", gpu_index=\"{i}\", link_index=\"{link}\", remote_type=\"{remote_type}\"",
                    self.gpu_name, self.instance_name, gpu.uuid
                );
                template.push_str(&format!(
                    "all_smi_nvlink_remote_device_type{{{labels}}} 1\n"
                ));
            }
        }

        // --- GPM metrics (Hopper+ only in reality; the mock pretends
        // every GPU is GPM-capable so the TUI / exporter paths see data).
        template.push_str(
            "# HELP all_smi_gpu_sm_occupancy GPM-reported SM occupancy fraction (0.0-1.0); \
             omitted on devices that do not support GPM (pre-Hopper)\n",
        );
        template.push_str("# TYPE all_smi_gpu_sm_occupancy gauge\n");
        for (i, gpu) in gpus.iter().enumerate() {
            let labels = format!(
                "gpu=\"{}\", instance=\"{}\", gpu_uuid=\"{}\", gpu_index=\"{i}\"",
                self.gpu_name, self.instance_name, gpu.uuid
            );
            template.push_str(&format!(
                "all_smi_gpu_sm_occupancy{{{labels}}} {SM_OCCUPANCY:.2}\n"
            ));
        }

        template.push_str(
            "# HELP all_smi_gpu_memory_bandwidth_utilization GPM-reported memory bandwidth utilization fraction (0.0-1.0); \
             omitted on devices that do not support GPM (pre-Hopper)\n",
        );
        template.push_str("# TYPE all_smi_gpu_memory_bandwidth_utilization gauge\n");
        for (i, gpu) in gpus.iter().enumerate() {
            let labels = format!(
                "gpu=\"{}\", instance=\"{}\", gpu_uuid=\"{}\", gpu_index=\"{i}\"",
                self.gpu_name, self.instance_name, gpu.uuid
            );
            template.push_str(&format!(
                "all_smi_gpu_memory_bandwidth_utilization{{{labels}}} {MEMORY_BW_UTIL:.2}\n"
            ));
        }
    }

    fn add_gpu_metrics(&self, template: &mut String, gpus: &[GpuMetrics]) {
        let gpu_metrics = [
            ("all_smi_gpu_utilization", "GPU utilization percentage"),
            ("all_smi_gpu_memory_used_bytes", "GPU memory used in bytes"),
            (
                "all_smi_gpu_memory_total_bytes",
                "GPU memory total in bytes",
            ),
            (
                "all_smi_gpu_temperature_celsius",
                "GPU temperature in celsius",
            ),
            (
                "all_smi_gpu_power_consumption_watts",
                "GPU power consumption in watts",
            ),
            ("all_smi_gpu_frequency_mhz", "GPU frequency in MHz"),
        ];

        for (metric_name, help_text) in gpu_metrics {
            template.push_str(&format!("# HELP {metric_name} {help_text}\n"));
            template.push_str(&format!("# TYPE {metric_name} gauge\n"));

            for (i, gpu) in gpus.iter().enumerate() {
                let labels = format!(
                    "gpu=\"{}\", instance=\"{}\", gpu_uuid=\"{}\", gpu_index=\"{i}\"",
                    self.gpu_name, self.instance_name, gpu.uuid
                );

                let placeholder = match metric_name {
                    "all_smi_gpu_utilization" => format!("{{{{UTIL_{i}}}}}"),
                    "all_smi_gpu_memory_used_bytes" => format!("{{{{MEM_USED_{i}}}}}"),
                    "all_smi_gpu_memory_total_bytes" => format!("{{{{MEM_TOTAL_{i}}}}}"),
                    "all_smi_gpu_temperature_celsius" => format!("{{{{TEMP_{i}}}}}"),
                    "all_smi_gpu_power_consumption_watts" => format!("{{{{POWER_{i}}}}}"),
                    "all_smi_gpu_frequency_mhz" => format!("{{{{FREQ_{i}}}}}"),
                    _ => "0".to_string(),
                };

                template.push_str(&format!("{metric_name}{{{labels}}} {placeholder}\n"));
            }
        }

        // Add GPU info metric with driver and CUDA version
        self.add_gpu_info_metric(template, gpus);
    }

    fn add_gpu_info_metric(&self, template: &mut String, gpus: &[GpuMetrics]) {
        use crate::mock::constants::{DEFAULT_CUDA_VERSION, DEFAULT_NVIDIA_DRIVER_VERSION};

        template.push_str("# HELP all_smi_gpu_info GPU device information\n");
        template.push_str("# TYPE all_smi_gpu_info gauge\n");

        for (i, gpu) in gpus.iter().enumerate() {
            let labels = format!(
                "gpu=\"{}\", instance=\"{}\", gpu_uuid=\"{}\", gpu_index=\"{i}\", \
                 driver_version=\"{DEFAULT_NVIDIA_DRIVER_VERSION}\", cuda_version=\"{DEFAULT_CUDA_VERSION}\", \
                 lib_name=\"CUDA\", lib_version=\"{DEFAULT_CUDA_VERSION}\"",
                self.gpu_name, self.instance_name, gpu.uuid
            );
            template.push_str(&format!("all_smi_gpu_info{{{labels}}} 1\n"));
        }
    }

    fn add_pstate_metrics(&self, template: &mut String, gpus: &[GpuMetrics]) {
        template.push_str("# HELP all_smi_gpu_pstate GPU performance state\n");
        template.push_str("# TYPE all_smi_gpu_pstate gauge\n");

        for (i, gpu) in gpus.iter().enumerate() {
            let labels = format!(
                "gpu=\"{}\", instance=\"{}\", gpu_uuid=\"{}\", gpu_index=\"{i}\"",
                self.gpu_name, self.instance_name, gpu.uuid
            );
            template.push_str(&format!(
                "all_smi_gpu_pstate{{{labels}}} {{{{PSTATE_{i}}}}}\n"
            ));
        }
    }

    fn add_process_metrics(&self, template: &mut String, gpus: &[GpuMetrics]) {
        // Process count
        template.push_str("# HELP all_smi_gpu_process_count Number of processes running on GPU\n");
        template.push_str("# TYPE all_smi_gpu_process_count gauge\n");

        for (i, gpu) in gpus.iter().enumerate() {
            let labels = format!(
                "gpu=\"{}\", instance=\"{}\", gpu_uuid=\"{}\", gpu_index=\"{i}\"",
                self.gpu_name, self.instance_name, gpu.uuid
            );
            template.push_str(&format!(
                "all_smi_gpu_process_count{{{labels}}} {{{{PROC_COUNT_{i}}}}}\n"
            ));
        }
    }

    fn add_driver_metrics(&self, template: &mut String) {
        // NVIDIA driver version
        template.push_str("# HELP all_smi_nvidia_driver_version NVIDIA driver version\n");
        template.push_str("# TYPE all_smi_nvidia_driver_version gauge\n");
        template.push_str(&format!(
            "all_smi_nvidia_driver_version{{instance=\"{}\"}} 1\n",
            self.instance_name
        ));
    }

    fn add_system_metrics(&self, template: &mut String, cpu: &CpuMetrics, memory: &MemoryMetrics) {
        // CPU metrics
        template.push_str("# HELP all_smi_cpu_utilization CPU utilization percentage\n");
        template.push_str("# TYPE all_smi_cpu_utilization gauge\n");
        template.push_str(&format!(
            "all_smi_cpu_utilization{{instance=\"{}\"}} {{{{CPU_UTIL}}}}\n",
            self.instance_name
        ));

        template.push_str("# HELP all_smi_cpu_core_count Total number of CPU cores\n");
        template.push_str("# TYPE all_smi_cpu_core_count gauge\n");
        template.push_str(&format!(
            "all_smi_cpu_core_count{{instance=\"{}\"}} {}\n",
            self.instance_name, cpu.core_count
        ));

        template.push_str("# HELP all_smi_cpu_model CPU model name\n");
        template.push_str("# TYPE all_smi_cpu_model info\n");
        template.push_str(&format!(
            "all_smi_cpu_model{{instance=\"{}\", model=\"{}\"}} 1\n",
            self.instance_name, cpu.model
        ));

        template.push_str("# HELP all_smi_cpu_frequency_mhz CPU frequency in MHz\n");
        template.push_str("# TYPE all_smi_cpu_frequency_mhz gauge\n");
        template.push_str(&format!(
            "all_smi_cpu_frequency_mhz{{instance=\"{}\"}} {}\n",
            self.instance_name, cpu.frequency_mhz
        ));

        template.push_str("# HELP all_smi_cpu_temperature_celsius CPU temperature in celsius\n");
        template.push_str("# TYPE all_smi_cpu_temperature_celsius gauge\n");
        if let Some(temp) = cpu.temperature_celsius {
            template.push_str(&format!(
                "all_smi_cpu_temperature_celsius{{instance=\"{}\"}} {temp}\n",
                self.instance_name
            ));
        }

        // Memory metrics
        template.push_str("# HELP all_smi_memory_used_bytes System memory used in bytes\n");
        template.push_str("# TYPE all_smi_memory_used_bytes gauge\n");
        template.push_str(&format!(
            "all_smi_memory_used_bytes{{instance=\"{}\"}} {{{{MEM_USED}}}}\n",
            self.instance_name
        ));

        template.push_str("# HELP all_smi_memory_total_bytes System memory total in bytes\n");
        template.push_str("# TYPE all_smi_memory_total_bytes gauge\n");
        template.push_str(&format!(
            "all_smi_memory_total_bytes{{instance=\"{}\"}} {}\n",
            self.instance_name, memory.total_bytes
        ));
    }

    /// Render dynamic values for NVIDIA GPUs
    pub fn render_nvidia_response(
        &self,
        template: &str,
        gpus: &[GpuMetrics],
        cpu: &CpuMetrics,
        memory: &MemoryMetrics,
    ) -> String {
        let mut response = template.to_string();

        // Replace GPU metrics
        for (i, gpu) in gpus.iter().enumerate() {
            response = response
                .replace(
                    &format!("{{{{UTIL_{i}}}}}"),
                    &format!("{:.2}", gpu.utilization),
                )
                .replace(
                    &format!("{{{{MEM_USED_{i}}}}}"),
                    &gpu.memory_used_bytes.to_string(),
                )
                .replace(
                    &format!("{{{{MEM_TOTAL_{i}}}}}"),
                    &gpu.memory_total_bytes.to_string(),
                )
                .replace(
                    &format!("{{{{TEMP_{i}}}}}"),
                    &gpu.temperature_celsius.to_string(),
                )
                .replace(
                    &format!("{{{{POWER_{i}}}}}"),
                    &format!("{:.3}", gpu.power_consumption_watts),
                )
                .replace(&format!("{{{{FREQ_{i}}}}}"), &gpu.frequency_mhz.to_string());

            // Replace P-state based on utilization
            let pstate = if gpu.utilization > 80.0 {
                0 // P0 - Maximum performance
            } else if gpu.utilization > 50.0 {
                2 // P2 - Balanced
            } else if gpu.utilization > 20.0 {
                5 // P5 - Auto
            } else if gpu.utilization > 0.0 {
                8 // P8 - Adaptive
            } else {
                12 // P12 - Idle
            };
            response = response.replace(&format!("{{{{PSTATE_{i}}}}}"), &pstate.to_string());

            // Process metrics (simplified for now - no actual processes)
            response = response.replace(&format!("{{{{PROC_COUNT_{i}}}}}"), "0");
        }

        // Replace CPU and memory metrics
        response = response
            .replace("{{CPU_UTIL}}", &format!("{:.2}", cpu.utilization))
            .replace("{{MEM_USED}}", &memory.used_bytes.to_string());

        // Replace chassis metrics
        response = crate::mock::templates::common::render_chassis_metrics(response, gpus);

        // Replace vGPU placeholders when the mock mode is enabled. No-op when
        // the env var is unset.
        response = crate::mock::templates::vgpu::maybe_render_vgpu_response(response, gpus);

        // Replace MIG placeholders when the mock mode is enabled. No-op when
        // the env var is unset.
        response = crate::mock::templates::mig::maybe_render_mig_response(response, gpus);

        response
    }
}

impl MockGenerator for NvidiaMockGenerator {
    fn generate(&self, config: &MockConfig) -> MockResult<MockData> {
        self.validate_config(config)?;

        // Generate initial GPU metrics
        // Create a single RNG instance outside the loop for better performance
        use rand::{RngExt, rng};
        let mut rng = rng();

        let gpus: Vec<GpuMetrics> = (0..config.device_count)
            .map(|_| {
                GpuMetrics {
                    uuid: crate::mock::metrics::gpu::generate_uuid_with_rng(&mut rng),
                    utilization: rng.random_range(0.0..100.0),
                    memory_used_bytes: rng.random_range(1_000_000_000..80_000_000_000),
                    memory_total_bytes: 85_899_345_920, // 80GB
                    temperature_celsius: rng.random_range(35..75),
                    power_consumption_watts: rng.random_range(100.0..450.0),
                    frequency_mhz: rng.random_range(1200..1980),
                    ane_utilization_watts: 0.0,
                    thermal_pressure_level: None,
                }
            })
            .collect();

        // Generate CPU and memory metrics
        // Reuse the existing RNG instance
        let cpu = CpuMetrics {
            model: "Intel Xeon Platinum".to_string(),
            utilization: rng.random_range(10.0..90.0),
            socket_count: 2,
            core_count: 128,
            thread_count: 256,
            frequency_mhz: 2400,
            temperature_celsius: Some(65),
            power_consumption_watts: Some(250.0),
            socket_utilizations: vec![rng.random_range(10.0..90.0), rng.random_range(10.0..90.0)],
            p_core_count: None,
            e_core_count: None,
            gpu_core_count: None,
            p_core_utilization: None,
            e_core_utilization: None,
            p_cluster_frequency_mhz: None,
            e_cluster_frequency_mhz: None,
            per_core_utilization: vec![],
        };

        let memory = MemoryMetrics {
            total_bytes: 1099511627776, // 1TB
            used_bytes: rng.random_range(10_000_000_000..500_000_000_000),
            available_bytes: rng.random_range(100_000_000_000..600_000_000_000),
            free_bytes: rng.random_range(50_000_000_000..400_000_000_000),
            cached_bytes: rng.random_range(10_000_000_000..100_000_000_000),
            buffers_bytes: rng.random_range(1_000_000_000..10_000_000_000),
            swap_total_bytes: 0,
            swap_used_bytes: 0,
            swap_free_bytes: 0,
            utilization: rng.random_range(10.0..90.0),
        };

        // Build and render template
        let template = self.build_nvidia_template(&gpus, &cpu, &memory);
        let response = self.render_nvidia_response(&template, &gpus, &cpu, &memory);

        Ok(MockData {
            response,
            content_type: "text/plain; version=0.0.4".to_string(),
            timestamp: chrono::Utc::now(),
            platform: MockPlatform::Nvidia,
        })
    }

    fn generate_template(&self, config: &MockConfig) -> MockResult<String> {
        self.validate_config(config)?;

        // Generate sample metrics for template
        let gpus: Vec<GpuMetrics> = (0..config.device_count)
            .map(|i| GpuMetrics {
                uuid: format!("GPU-{:08x}", i as u32),
                utilization: 0.0,
                memory_used_bytes: 0,
                memory_total_bytes: 85_899_345_920,
                temperature_celsius: 0,
                power_consumption_watts: 0.0,
                frequency_mhz: 0,
                ane_utilization_watts: 0.0,
                thermal_pressure_level: None,
            })
            .collect();

        let cpu = CpuMetrics {
            model: "Intel Xeon Platinum".to_string(),
            utilization: 0.0,
            socket_count: 2,
            core_count: 128,
            thread_count: 256,
            frequency_mhz: 2400,
            temperature_celsius: Some(65),
            power_consumption_watts: Some(250.0),
            socket_utilizations: vec![0.0, 0.0],
            p_core_count: None,
            e_core_count: None,
            gpu_core_count: None,
            p_core_utilization: None,
            e_core_utilization: None,
            p_cluster_frequency_mhz: None,
            e_cluster_frequency_mhz: None,
            per_core_utilization: vec![],
        };

        let memory = MemoryMetrics {
            total_bytes: 1099511627776,
            used_bytes: 0,
            available_bytes: 1099511627776,
            free_bytes: 1099511627776,
            cached_bytes: 0,
            buffers_bytes: 0,
            swap_total_bytes: 0,
            swap_used_bytes: 0,
            swap_free_bytes: 0,
            utilization: 0.0,
        };

        Ok(self.build_nvidia_template(&gpus, &cpu, &memory))
    }

    fn render(&self, template: &str, config: &MockConfig) -> MockResult<String> {
        self.validate_config(config)?;

        // This would use actual dynamic values in production
        Ok(template.to_string())
    }

    fn platform(&self) -> MockPlatform {
        MockPlatform::Nvidia
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_gpu_metrics() -> Vec<GpuMetrics> {
        vec![GpuMetrics {
            uuid: "GPU-0".to_string(),
            utilization: 50.0,
            memory_used_bytes: 1024,
            memory_total_bytes: 8192,
            temperature_celsius: 65,
            power_consumption_watts: 200.0,
            frequency_mhz: 1500,
            ane_utilization_watts: 0.0,
            thermal_pressure_level: None,
        }]
    }

    fn make_cpu_metrics() -> CpuMetrics {
        CpuMetrics {
            model: "Intel Xeon".to_string(),
            utilization: 10.0,
            socket_count: 1,
            core_count: 8,
            thread_count: 16,
            frequency_mhz: 2400,
            temperature_celsius: Some(50),
            power_consumption_watts: Some(100.0),
            socket_utilizations: vec![10.0],
            p_core_count: None,
            e_core_count: None,
            gpu_core_count: None,
            p_core_utilization: None,
            e_core_utilization: None,
            p_cluster_frequency_mhz: None,
            e_cluster_frequency_mhz: None,
            per_core_utilization: vec![],
        }
    }

    fn make_memory_metrics() -> MemoryMetrics {
        MemoryMetrics {
            total_bytes: 1024,
            used_bytes: 512,
            available_bytes: 512,
            free_bytes: 512,
            cached_bytes: 0,
            buffers_bytes: 0,
            swap_total_bytes: 0,
            swap_used_bytes: 0,
            swap_free_bytes: 0,
            utilization: 50.0,
        }
    }

    // Helper: set HARDWARE_DETAILS_ENV_VAR for the duration of a closure, then
    // restore the prior value. Uses unsafe env mutation the same way the vGPU
    // and MIG test helpers do.
    fn with_hardware_details_enabled<F: FnOnce()>(f: F) {
        let prior = std::env::var(HARDWARE_DETAILS_ENV_VAR).ok();
        // SAFETY: this helper is called from single-threaded test functions
        // that do not spawn additional threads. Env mutation is restored before
        // returning, matching the pattern used by is_vgpu_enabled_reflects_env_var
        // and is_mig_enabled_reflects_env_var in the sibling mock modules.
        unsafe {
            std::env::set_var(HARDWARE_DETAILS_ENV_VAR, "1");
        }
        f();
        unsafe {
            match prior {
                Some(v) => std::env::set_var(HARDWARE_DETAILS_ENV_VAR, v),
                None => std::env::remove_var(HARDWARE_DETAILS_ENV_VAR),
            }
        }
    }

    // --- default (env var unset) behaviour ---

    #[test]
    fn mock_template_omits_hardware_details_by_default() {
        // Ensure the env var is absent for this test.
        let prior = std::env::var(HARDWARE_DETAILS_ENV_VAR).ok();
        unsafe {
            std::env::remove_var(HARDWARE_DETAILS_ENV_VAR);
        }

        let gen_ = NvidiaMockGenerator::new(None, "mock-node".to_string());
        let gpus = make_gpu_metrics();
        let tpl = gen_.build_nvidia_template(&gpus, &make_cpu_metrics(), &make_memory_metrics());

        // All extended-detail metrics must be absent in the minimal output.
        for metric in &[
            "all_smi_gpu_temperature_threshold_slowdown_celsius",
            "all_smi_gpu_temperature_threshold_shutdown_celsius",
            "all_smi_gpu_temperature_threshold_max_operating_celsius",
            "all_smi_gpu_temperature_threshold_acoustic_celsius",
            "all_smi_gpu_performance_state{",
            "all_smi_gpu_pstate{",
            "all_smi_gpu_numa_node_id{",
            "all_smi_gpu_gsp_firmware_mode{",
            "all_smi_gpu_gsp_firmware_version_info{",
            "all_smi_nvlink_remote_device_type{",
            "all_smi_gpu_sm_occupancy{",
            "all_smi_gpu_memory_bandwidth_utilization{",
        ] {
            assert!(
                !tpl.contains(metric),
                "metric {metric:?} should be absent when ALL_SMI_MOCK_HARDWARE_DETAILS is unset, but was found in:\n{tpl}"
            );
        }

        // Basic metrics must still be present.
        assert!(
            tpl.contains("all_smi_gpu_utilization{"),
            "basic GPU metric absent"
        );

        // Restore prior state.
        unsafe {
            match prior {
                Some(v) => std::env::set_var(HARDWARE_DETAILS_ENV_VAR, v),
                None => std::env::remove_var(HARDWARE_DETAILS_ENV_VAR),
            }
        }
    }

    // --- is_hardware_details_enabled reflects env var ---

    #[test]
    fn is_hardware_details_enabled_reflects_env_var() {
        let prior = std::env::var(HARDWARE_DETAILS_ENV_VAR).ok();
        // SAFETY: See with_hardware_details_enabled docstring.
        unsafe {
            std::env::remove_var(HARDWARE_DETAILS_ENV_VAR);
        }
        assert!(!is_hardware_details_enabled());

        unsafe {
            std::env::set_var(HARDWARE_DETAILS_ENV_VAR, "1");
        }
        assert!(is_hardware_details_enabled());

        // Empty value treated as disabled.
        unsafe {
            std::env::set_var(HARDWARE_DETAILS_ENV_VAR, "");
        }
        assert!(!is_hardware_details_enabled());

        // Restore prior state.
        unsafe {
            match prior {
                Some(v) => std::env::set_var(HARDWARE_DETAILS_ENV_VAR, v),
                None => std::env::remove_var(HARDWARE_DETAILS_ENV_VAR),
            }
        }
    }

    // --- extended-detail tests (require env var set) ---

    #[test]
    fn mock_template_includes_threshold_metrics() {
        with_hardware_details_enabled(|| {
            let gen_ = NvidiaMockGenerator::new(None, "mock-node".to_string());
            let gpus = make_gpu_metrics();
            let tpl =
                gen_.build_nvidia_template(&gpus, &make_cpu_metrics(), &make_memory_metrics());

            assert!(
                tpl.contains("all_smi_gpu_temperature_threshold_slowdown_celsius"),
                "mock template missing slowdown metric:\n{tpl}"
            );
            assert!(
                tpl.contains("all_smi_gpu_temperature_threshold_shutdown_celsius"),
                "mock template missing shutdown metric:\n{tpl}"
            );
            assert!(
                tpl.contains("all_smi_gpu_temperature_threshold_max_operating_celsius"),
                "mock template missing max_operating metric:\n{tpl}"
            );
            assert!(
                tpl.contains("all_smi_gpu_temperature_threshold_acoustic_celsius"),
                "mock template missing acoustic metric:\n{tpl}"
            );
            assert!(
                tpl.contains("all_smi_gpu_performance_state{"),
                "mock template missing canonical pstate metric:\n{tpl}"
            );
        });
    }

    #[test]
    fn mock_render_resolves_pstate_placeholders() {
        with_hardware_details_enabled(|| {
            let gen_ = NvidiaMockGenerator::new(None, "mock-node".to_string());
            let gpus = make_gpu_metrics();
            let tpl =
                gen_.build_nvidia_template(&gpus, &make_cpu_metrics(), &make_memory_metrics());
            let rendered = gen_.render_nvidia_response(
                &tpl,
                &gpus,
                &make_cpu_metrics(),
                &make_memory_metrics(),
            );
            // After rendering, no `{{PSTATE_...}}` placeholders should remain.
            assert!(
                !rendered.contains("{{PSTATE_"),
                "unresolved PSTATE placeholder in rendered output"
            );
        });
    }

    // --- hardware-detail mock template tests (issue #132) ---

    fn make_multi_gpu_metrics(count: usize) -> Vec<GpuMetrics> {
        (0..count)
            .map(|i| GpuMetrics {
                uuid: format!("GPU-{i}"),
                utilization: 50.0,
                memory_used_bytes: 1024,
                memory_total_bytes: 8192,
                temperature_celsius: 65,
                power_consumption_watts: 200.0,
                frequency_mhz: 1500,
                ane_utilization_watts: 0.0,
                thermal_pressure_level: None,
            })
            .collect()
    }

    #[test]
    fn mock_template_includes_all_hardware_detail_metrics() {
        with_hardware_details_enabled(|| {
            let gen_ = NvidiaMockGenerator::new(None, "mock-node".to_string());
            let gpus = make_gpu_metrics();
            let tpl =
                gen_.build_nvidia_template(&gpus, &make_cpu_metrics(), &make_memory_metrics());

            assert!(
                tpl.contains("all_smi_gpu_numa_node_id{"),
                "mock template missing NUMA metric:\n{tpl}"
            );
            assert!(
                tpl.contains("all_smi_gpu_gsp_firmware_mode{"),
                "mock template missing GSP firmware mode metric:\n{tpl}"
            );
            assert!(
                tpl.contains("all_smi_gpu_gsp_firmware_version_info{"),
                "mock template missing GSP firmware version info metric:\n{tpl}"
            );
            assert!(
                tpl.contains("all_smi_nvlink_remote_device_type{"),
                "mock template missing NvLink metric:\n{tpl}"
            );
            assert!(
                tpl.contains("all_smi_gpu_sm_occupancy{"),
                "mock template missing SM occupancy metric:\n{tpl}"
            );
            assert!(
                tpl.contains("all_smi_gpu_memory_bandwidth_utilization{"),
                "mock template missing memory bandwidth utilization metric:\n{tpl}"
            );
        });
    }

    #[test]
    fn mock_template_emits_six_nvlinks_per_gpu() {
        with_hardware_details_enabled(|| {
            let gen_ = NvidiaMockGenerator::new(None, "mock-node".to_string());
            let gpus = make_multi_gpu_metrics(2);
            let tpl =
                gen_.build_nvidia_template(&gpus, &make_cpu_metrics(), &make_memory_metrics());
            let nvlink_lines: Vec<_> = tpl
                .lines()
                .filter(|l| l.starts_with("all_smi_nvlink_remote_device_type{"))
                .collect();
            // 2 GPUs * 6 links = 12 lines.
            assert_eq!(
                nvlink_lines.len(),
                12,
                "expected 12 NvLink rows (2 GPUs x 6 links):\n{}",
                nvlink_lines.join("\n")
            );
            // 5 gpu + 1 switch per GPU = 10 gpu + 2 switch total.
            let gpu_remote_count = nvlink_lines
                .iter()
                .filter(|l| l.contains(r#"remote_type="gpu""#))
                .count();
            let switch_remote_count = nvlink_lines
                .iter()
                .filter(|l| l.contains(r#"remote_type="switch""#))
                .count();
            assert_eq!(gpu_remote_count, 10);
            assert_eq!(switch_remote_count, 2);
        });
    }

    #[test]
    fn mock_template_numa_alternates_between_zero_and_one() {
        with_hardware_details_enabled(|| {
            let gen_ = NvidiaMockGenerator::new(None, "mock-node".to_string());
            let gpus = make_multi_gpu_metrics(4);
            let tpl =
                gen_.build_nvidia_template(&gpus, &make_cpu_metrics(), &make_memory_metrics());
            // Extract just the NUMA metric lines.
            let numa_lines: Vec<_> = tpl
                .lines()
                .filter(|l| l.starts_with("all_smi_gpu_numa_node_id{"))
                .collect();
            assert_eq!(numa_lines.len(), 4);
            // GPU 0 → 0, GPU 1 → 1, GPU 2 → 0, GPU 3 → 1.
            for (i, line) in numa_lines.iter().enumerate() {
                let expected = (i as u32) % 2;
                assert!(
                    line.ends_with(&format!(" {expected}")),
                    "GPU {i} NUMA line expected to end with ' {expected}', got: {line}"
                );
            }
        });
    }

    #[test]
    fn mock_template_emits_gsp_firmware_version_label() {
        with_hardware_details_enabled(|| {
            let gen_ = NvidiaMockGenerator::new(None, "mock-node".to_string());
            let gpus = make_gpu_metrics();
            let tpl =
                gen_.build_nvidia_template(&gpus, &make_cpu_metrics(), &make_memory_metrics());
            assert!(
                tpl.contains(r#"version="550.54.15""#),
                "mock template missing GSP firmware version label:\n{tpl}"
            );
        });
    }

    #[test]
    fn mock_template_gpm_values_are_in_0_to_1_range() {
        with_hardware_details_enabled(|| {
            let gen_ = NvidiaMockGenerator::new(None, "mock-node".to_string());
            let gpus = make_gpu_metrics();
            let tpl =
                gen_.build_nvidia_template(&gpus, &make_cpu_metrics(), &make_memory_metrics());
            // Parse the SM occupancy line and sanity-check the value band.
            let sm_line = tpl
                .lines()
                .find(|l| l.starts_with("all_smi_gpu_sm_occupancy{"))
                .expect("SM occupancy line");
            let value: f32 = sm_line
                .rsplit(' ')
                .next()
                .and_then(|s| s.parse().ok())
                .expect("SM occupancy value");
            assert!(
                (0.0..=1.0).contains(&value),
                "SM occupancy out of range: {value}"
            );
        });
    }
}
