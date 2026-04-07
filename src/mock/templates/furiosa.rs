//! Furiosa NPU mock template generator

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

use crate::mock::metrics::GpuMetrics;
use all_smi::traits::mock_generator::{
    MockConfig, MockData, MockGenerator, MockPlatform, MockResult,
};
use rand::{RngExt, rng};

/// Furiosa NPU mock generator
pub struct FuriosaMockGenerator {
    gpu_name: String,
    instance_name: String,
}

impl FuriosaMockGenerator {
    pub fn new(gpu_name: Option<String>, instance_name: String) -> Self {
        Self {
            gpu_name: gpu_name.unwrap_or_else(|| "FuriosaAI Warboy RNGD".to_string()),
            instance_name,
        }
    }

    pub fn build_furiosa_template(&self, gpus: &[GpuMetrics]) -> String {
        let mut template = String::with_capacity(4096);

        // Basic GPU metrics (with Furiosa-specific labeling)
        self.add_furiosa_gpu_metrics(&mut template, gpus);

        // Furiosa-specific: ANE metrics (always 0 for Furiosa)
        self.add_ane_metrics(&mut template, gpus);

        // Furiosa-specific: NPU engine metrics
        self.add_npu_engine_metrics(&mut template, gpus);

        // Furiosa-specific: NPU status metrics
        self.add_npu_status_metrics(&mut template, gpus);

        // System metrics
        super::common::add_system_metrics(&mut template, &self.instance_name);

        // Driver info
        self.add_driver_metrics(&mut template);

        template
    }

    fn add_furiosa_gpu_metrics(&self, template: &mut String, gpus: &[GpuMetrics]) {
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
                // Furiosa uses "npu{i}" instead of instance name
                let labels = format!(
                    "gpu=\"{}\", instance=\"npu{i}\", uuid=\"{}\", index=\"{i}\"",
                    self.gpu_name, gpu.uuid
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
    }

    fn add_ane_metrics(&self, template: &mut String, gpus: &[GpuMetrics]) {
        // ANE metrics (Furiosa always returns 0)
        template.push_str("# HELP all_smi_ane_utilization ANE utilization in mW\n");
        template.push_str("# TYPE all_smi_ane_utilization gauge\n");

        for (i, gpu) in gpus.iter().enumerate() {
            let labels = format!(
                "gpu=\"{}\", instance=\"npu{i}\", uuid=\"{}\", index=\"{i}\"",
                self.gpu_name, gpu.uuid
            );
            template.push_str(&format!(
                "all_smi_ane_utilization{{{labels}}} 0\n" // Always 0 for Furiosa
            ));
        }

        template.push_str("# HELP all_smi_ane_power_watts ANE power consumption in watts\n");
        template.push_str("# TYPE all_smi_ane_power_watts gauge\n");

        for (i, gpu) in gpus.iter().enumerate() {
            let labels = format!(
                "gpu=\"{}\", instance=\"npu{i}\", uuid=\"{}\", index=\"{i}\"",
                self.gpu_name, gpu.uuid
            );
            template.push_str(&format!(
                "all_smi_ane_power_watts{{{labels}}} 0\n" // Always 0 for Furiosa
            ));
        }
    }

    fn add_npu_engine_metrics(&self, template: &mut String, gpus: &[GpuMetrics]) {
        // NPU computation engine utilization
        template.push_str("# HELP all_smi_npu_computation_utilization NPU computation engine utilization percentage\n");
        template.push_str("# TYPE all_smi_npu_computation_utilization gauge\n");

        for (i, gpu) in gpus.iter().enumerate() {
            let labels = format!(
                "gpu=\"{}\", instance=\"npu{i}\", uuid=\"{}\", index=\"{i}\"",
                self.gpu_name, gpu.uuid
            );
            template.push_str(&format!(
                "all_smi_npu_computation_utilization{{{labels}}} {{{{NPU_COMP_{i}}}}}\n"
            ));
        }

        // NPU copy engine utilization
        template.push_str(
            "# HELP all_smi_npu_copy_utilization NPU copy engine utilization percentage\n",
        );
        template.push_str("# TYPE all_smi_npu_copy_utilization gauge\n");

        for (i, gpu) in gpus.iter().enumerate() {
            let labels = format!(
                "gpu=\"{}\", instance=\"npu{i}\", uuid=\"{}\", index=\"{i}\"",
                self.gpu_name, gpu.uuid
            );
            template.push_str(&format!(
                "all_smi_npu_copy_utilization{{{labels}}} {{{{NPU_COPY_{i}}}}}\n"
            ));
        }
    }

    fn add_npu_status_metrics(&self, template: &mut String, gpus: &[GpuMetrics]) {
        // NPU status (idle/running)
        template.push_str("# HELP all_smi_npu_status NPU status (0=idle, 1=running)\n");
        template.push_str("# TYPE all_smi_npu_status gauge\n");

        for (i, gpu) in gpus.iter().enumerate() {
            let labels = format!(
                "gpu=\"{}\", instance=\"npu{i}\", uuid=\"{}\", index=\"{i}\"",
                self.gpu_name, gpu.uuid
            );
            template.push_str(&format!(
                "all_smi_npu_status{{{labels}}} {{{{NPU_STATUS_{i}}}}}\n"
            ));
        }

        // NPU error count
        template.push_str("# HELP all_smi_npu_error_count NPU error count\n");
        template.push_str("# TYPE all_smi_npu_error_count counter\n");

        for (i, gpu) in gpus.iter().enumerate() {
            let labels = format!(
                "gpu=\"{}\", instance=\"npu{i}\", uuid=\"{}\", index=\"{i}\"",
                self.gpu_name, gpu.uuid
            );
            template.push_str(&format!(
                "all_smi_npu_error_count{{{labels}}} 0\n" // Always 0 for mock
            ));
        }
    }

    fn add_driver_metrics(&self, template: &mut String) {
        template.push_str("# HELP all_smi_furiosa_driver_version Furiosa driver version\n");
        template.push_str("# TYPE all_smi_furiosa_driver_version gauge\n");
        template.push_str(&format!(
            "all_smi_furiosa_driver_version{{instance=\"{}\"}} 1\n",
            self.instance_name
        ));
    }

    pub fn render_furiosa_response(&self, template: &str, gpus: &[GpuMetrics]) -> String {
        let mut response = template.to_string();
        let mut rng = rng();

        // Render basic GPU metrics
        response = super::common::render_basic_gpu_metrics(response, gpus);

        // Render Furiosa-specific metrics
        for (i, gpu) in gpus.iter().enumerate() {
            // NPU engine metrics
            let npu_comp = if gpu.utilization > 0.0 {
                (gpu.utilization + rng.random_range(-10.0..10.0)).clamp(1.0, 100.0)
            } else {
                0.0
            };
            let npu_copy = if gpu.utilization > 0.0 {
                rng.random_range(5.0..30.0)
            } else {
                0.0
            };

            response = response
                .replace(&format!("{{{{NPU_COMP_{i}}}}}"), &format!("{npu_comp:.2}"))
                .replace(&format!("{{{{NPU_COPY_{i}}}}}"), &format!("{npu_copy:.2}"));

            // NPU status (based on utilization)
            let npu_status = if gpu.utilization > 0.0 { 1 } else { 0 };
            response =
                response.replace(&format!("{{{{NPU_STATUS_{i}}}}}"), &npu_status.to_string());
        }

        // Render system metrics
        response = super::common::render_system_metrics(response);

        response
    }
}

impl MockGenerator for FuriosaMockGenerator {
    fn generate(&self, config: &MockConfig) -> MockResult<MockData> {
        self.validate_config(config)?;

        // Furiosa RNGD has 64GB memory
        let gpus = super::common::generate_gpu_metrics(config.device_count, 64_000_000_000);
        let template = self.build_furiosa_template(&gpus);
        let response = self.render_furiosa_response(&template, &gpus);

        Ok(MockData {
            response,
            content_type: "text/plain; version=0.0.4".to_string(),
            timestamp: chrono::Utc::now(),
            platform: MockPlatform::Custom("Furiosa".to_string()),
        })
    }

    fn generate_template(&self, config: &MockConfig) -> MockResult<String> {
        self.validate_config(config)?;
        let gpus = super::common::generate_empty_gpu_metrics(config.device_count, 64_000_000_000);
        Ok(self.build_furiosa_template(&gpus))
    }

    fn render(&self, template: &str, _config: &MockConfig) -> MockResult<String> {
        Ok(template.to_string())
    }

    fn platform(&self) -> MockPlatform {
        MockPlatform::Custom("Furiosa".to_string())
    }
}
