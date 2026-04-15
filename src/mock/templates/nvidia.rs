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

        // NVIDIA-specific: P-state metrics (legacy `all_smi_gpu_pstate` name
        // kept for backwards compatibility with older scrapers; the new
        // canonical name is emitted by `add_thermal_threshold_metrics`).
        self.add_pstate_metrics(&mut template, gpus);

        // NVIDIA-specific: Temperature thresholds + canonical P-state metric
        // (issue #130). Emitted with synthetic but realistic numbers so local
        // dev and `cargo test --features mock` see the feature populated.
        self.add_thermal_threshold_metrics(&mut template, gpus);

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
                    "gpu=\"{}\", instance=\"{}\", uuid=\"{}\", index=\"{i}\"",
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
                "gpu=\"{}\", instance=\"{}\", uuid=\"{}\", index=\"{i}\"",
                self.gpu_name, self.instance_name, gpu.uuid
            );
            template.push_str(&format!(
                "all_smi_gpu_performance_state{{{labels}}} {{{{PSTATE_{i}}}}}\n"
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
                    "gpu=\"{}\", instance=\"{}\", uuid=\"{}\", index=\"{i}\"",
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
                "gpu=\"{}\", instance=\"{}\", uuid=\"{}\", index=\"{i}\", \
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
                "gpu=\"{}\", instance=\"{}\", uuid=\"{}\", index=\"{i}\"",
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
                "gpu=\"{}\", instance=\"{}\", uuid=\"{}\", index=\"{i}\"",
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

    #[test]
    fn mock_template_includes_threshold_metrics() {
        let gen_ = NvidiaMockGenerator::new(None, "mock-node".to_string());
        let gpus = make_gpu_metrics();
        let tpl = gen_.build_nvidia_template(&gpus, &make_cpu_metrics(), &make_memory_metrics());

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
    }

    #[test]
    fn mock_render_resolves_pstate_placeholders() {
        let gen_ = NvidiaMockGenerator::new(None, "mock-node".to_string());
        let gpus = make_gpu_metrics();
        let tpl = gen_.build_nvidia_template(&gpus, &make_cpu_metrics(), &make_memory_metrics());
        let rendered =
            gen_.render_nvidia_response(&tpl, &gpus, &make_cpu_metrics(), &make_memory_metrics());
        // After rendering, no `{{PSTATE_...}}` placeholders should remain.
        assert!(
            !rendered.contains("{{PSTATE_"),
            "unresolved PSTATE placeholder in rendered output"
        );
    }
}
