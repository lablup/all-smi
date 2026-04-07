//! NVIDIA Jetson mock template generator

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

/// Jetson GPU mock generator
pub struct JetsonMockGenerator {
    gpu_name: String,
    instance_name: String,
}

impl JetsonMockGenerator {
    pub fn new(gpu_name: Option<String>, instance_name: String) -> Self {
        Self {
            gpu_name: gpu_name.unwrap_or_else(|| "NVIDIA Jetson AGX Orin".to_string()),
            instance_name,
        }
    }

    pub fn build_jetson_template(&self, gpus: &[GpuMetrics]) -> String {
        let mut template = String::with_capacity(3072);

        // Basic GPU metrics
        super::common::add_basic_gpu_metrics(
            &mut template,
            &self.gpu_name,
            &self.instance_name,
            gpus,
        );

        // Add GPU info metric with lib_name and lib_version
        self.add_gpu_info_metric(&mut template, gpus);

        // Jetson-specific: DLA metrics
        self.add_dla_metrics(&mut template, gpus);

        // Jetson-specific: System metrics with ARM CPU
        self.add_jetson_system_metrics(&mut template);

        template
    }

    fn add_gpu_info_metric(&self, template: &mut String, gpus: &[GpuMetrics]) {
        use crate::mock::constants::DEFAULT_CUDA_VERSION;

        template.push_str("# HELP all_smi_gpu_info GPU device information\n");
        template.push_str("# TYPE all_smi_gpu_info gauge\n");

        for (i, gpu) in gpus.iter().enumerate() {
            let labels = format!(
                "gpu=\"{}\", instance=\"{}\", uuid=\"{}\", index=\"{i}\", \
                 cuda_version=\"{DEFAULT_CUDA_VERSION}\", \
                 lib_name=\"CUDA\", lib_version=\"{DEFAULT_CUDA_VERSION}\"",
                self.gpu_name, self.instance_name, gpu.uuid
            );
            template.push_str(&format!("all_smi_gpu_info{{{labels}}} 1\n"));
        }
    }

    fn add_jetson_system_metrics(&self, template: &mut String) {
        // CPU metrics - Jetson AGX Orin has 12-core ARM Cortex-A78AE CPU
        template.push_str("# HELP all_smi_cpu_utilization CPU utilization percentage\n");
        template.push_str("# TYPE all_smi_cpu_utilization gauge\n");
        template.push_str(&format!(
            "all_smi_cpu_utilization{{instance=\"{}\"}} {{{{CPU_UTIL}}}}\n",
            self.instance_name
        ));

        template.push_str("# HELP all_smi_cpu_cores Number of CPU cores\n");
        template.push_str("# TYPE all_smi_cpu_cores gauge\n");
        template.push_str(&format!(
            "all_smi_cpu_cores{{instance=\"{}\"}} 12\n", // Jetson AGX Orin: 12 cores
            self.instance_name
        ));

        // CPU frequency - ARM cores run at lower frequencies
        template.push_str("# HELP all_smi_cpu_frequency_mhz CPU frequency in MHz\n");
        template.push_str("# TYPE all_smi_cpu_frequency_mhz gauge\n");
        template.push_str(&format!(
            "all_smi_cpu_frequency_mhz{{instance=\"{}\"}} {{{{CPU_FREQ}}}}\n",
            self.instance_name
        ));

        // Memory metrics - Jetson typically has 32GB or 64GB unified memory
        template.push_str("# HELP all_smi_memory_used_bytes System memory used in bytes\n");
        template.push_str("# TYPE all_smi_memory_used_bytes gauge\n");
        template.push_str(&format!(
            "all_smi_memory_used_bytes{{instance=\"{}\"}} {{{{MEM_USED}}}}\n",
            self.instance_name
        ));

        template.push_str("# HELP all_smi_memory_total_bytes System memory total in bytes\n");
        template.push_str("# TYPE all_smi_memory_total_bytes gauge\n");
        template.push_str(&format!(
            "all_smi_memory_total_bytes{{instance=\"{}\"}} 34359738368\n", // 32GB
            self.instance_name
        ));
    }

    fn add_dla_metrics(&self, template: &mut String, gpus: &[GpuMetrics]) {
        // DLA utilization (Deep Learning Accelerator)
        template.push_str("# HELP all_smi_dla_utilization DLA utilization percentage\n");
        template.push_str("# TYPE all_smi_dla_utilization gauge\n");

        for (i, gpu) in gpus.iter().enumerate() {
            for dla_idx in 0..2 {
                // Jetson has 2 DLA cores
                let labels = format!(
                    "gpu=\"{}\", instance=\"{}\", uuid=\"{}\", index=\"{i}\", dla=\"{dla_idx}\"",
                    self.gpu_name, self.instance_name, gpu.uuid
                );
                template.push_str(&format!(
                    "all_smi_dla_utilization{{{labels}}} {{{{DLA_{i}_{dla_idx}}}}}\n"
                ));
            }
        }

        // DLA frequency
        template.push_str("# HELP all_smi_dla_frequency_mhz DLA frequency in MHz\n");
        template.push_str("# TYPE all_smi_dla_frequency_mhz gauge\n");

        for (i, gpu) in gpus.iter().enumerate() {
            for dla_idx in 0..2 {
                let labels = format!(
                    "gpu=\"{}\", instance=\"{}\", uuid=\"{}\", index=\"{i}\", dla=\"{dla_idx}\"",
                    self.gpu_name, self.instance_name, gpu.uuid
                );
                template.push_str(&format!(
                    "all_smi_dla_frequency_mhz{{{labels}}} {{{{DLA_FREQ_{i}_{dla_idx}}}}}\n"
                ));
            }
        }
    }

    pub fn render_jetson_response(&self, template: &str, gpus: &[GpuMetrics]) -> String {
        let mut response = template.to_string();

        // Render basic GPU metrics
        response = super::common::render_basic_gpu_metrics(response, gpus);

        // Render DLA metrics
        use rand::{RngExt, rng};
        let mut rng = rng();
        for (i, _gpu) in gpus.iter().enumerate() {
            for dla_idx in 0..2 {
                let dla_util = rng.random_range(0.0..100.0);
                let dla_freq = rng.random_range(600..1400);

                response = response
                    .replace(
                        &format!("{{{{DLA_{i}_{dla_idx}}}}}"),
                        &format!("{dla_util:.2}"),
                    )
                    .replace(
                        &format!("{{{{DLA_FREQ_{i}_{dla_idx}}}}}"),
                        &dla_freq.to_string(),
                    );
            }
        }

        // Render Jetson-specific system metrics
        // Note: CPU cores are hardcoded in template, only need to render dynamic values
        response = response
            .replace(
                "{{CPU_UTIL}}",
                &format!("{:.2}", rng.random_range(10.0..70.0)),
            ) // ARM CPUs typically run cooler
            .replace("{{CPU_FREQ}}", &rng.random_range(1200..2200).to_string()) // ARM frequency range
            .replace(
                "{{MEM_USED}}",
                &rng.random_range(2_000_000_000u64..30_000_000_000u64)
                    .to_string(),
            );

        response
    }
}

impl MockGenerator for JetsonMockGenerator {
    fn generate(&self, config: &MockConfig) -> MockResult<MockData> {
        self.validate_config(config)?;

        let gpus = super::common::generate_gpu_metrics(config.device_count, 32_000_000_000); // 32GB
        let template = self.build_jetson_template(&gpus);
        let response = self.render_jetson_response(&template, &gpus);

        Ok(MockData {
            response,
            content_type: "text/plain; version=0.0.4".to_string(),
            timestamp: chrono::Utc::now(),
            platform: MockPlatform::Jetson,
        })
    }

    fn generate_template(&self, config: &MockConfig) -> MockResult<String> {
        self.validate_config(config)?;
        let gpus = super::common::generate_empty_gpu_metrics(config.device_count, 32_000_000_000);
        Ok(self.build_jetson_template(&gpus))
    }

    fn render(&self, template: &str, _config: &MockConfig) -> MockResult<String> {
        Ok(template.to_string())
    }

    fn platform(&self) -> MockPlatform {
        MockPlatform::Jetson
    }
}
