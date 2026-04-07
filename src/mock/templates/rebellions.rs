//! Rebellions NPU mock template generator

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

/// Rebellions NPU mock generator
pub struct RebellionsMockGenerator {
    gpu_name: String,
    instance_name: String,
}

impl RebellionsMockGenerator {
    pub fn new(gpu_name: Option<String>, instance_name: String) -> Self {
        Self {
            gpu_name: gpu_name.unwrap_or_else(|| "Rebellions ATOM".to_string()),
            instance_name,
        }
    }

    pub fn build_rebellions_template(&self, gpus: &[GpuMetrics]) -> String {
        let mut template = String::with_capacity(3072);

        // Basic GPU metrics
        super::common::add_basic_gpu_metrics(
            &mut template,
            &self.gpu_name,
            &self.instance_name,
            gpus,
        );

        // Rebellions-specific: NPU metrics
        self.add_npu_metrics(&mut template, gpus);

        // Rebellions-specific: Core status metrics
        self.add_core_metrics(&mut template, gpus);

        // System metrics
        super::common::add_system_metrics(&mut template, &self.instance_name);

        // Driver info
        self.add_driver_metrics(&mut template);

        template
    }

    fn add_npu_metrics(&self, template: &mut String, gpus: &[GpuMetrics]) {
        // NPU core utilization
        template.push_str("# HELP all_smi_npu_core_utilization NPU core utilization percentage\n");
        template.push_str("# TYPE all_smi_npu_core_utilization gauge\n");

        for (i, gpu) in gpus.iter().enumerate() {
            let labels = format!(
                "gpu=\"{}\", instance=\"{}\", uuid=\"{}\", index=\"{i}\"",
                self.gpu_name, self.instance_name, gpu.uuid
            );
            template.push_str(&format!(
                "all_smi_npu_core_utilization{{{labels}}} {{{{NPU_UTIL_{i}}}}}\n"
            ));
        }

        // NPU memory bandwidth utilization
        template.push_str("# HELP all_smi_npu_memory_bandwidth_percent NPU memory bandwidth utilization percentage\n");
        template.push_str("# TYPE all_smi_npu_memory_bandwidth_percent gauge\n");

        for (i, gpu) in gpus.iter().enumerate() {
            let labels = format!(
                "gpu=\"{}\", instance=\"{}\", uuid=\"{}\", index=\"{i}\"",
                self.gpu_name, self.instance_name, gpu.uuid
            );
            template.push_str(&format!(
                "all_smi_npu_memory_bandwidth_percent{{{labels}}} {{{{NPU_BW_{i}}}}}\n"
            ));
        }
    }

    fn add_core_metrics(&self, template: &mut String, gpus: &[GpuMetrics]) {
        // Core active status
        template.push_str("# HELP all_smi_npu_cores_active Number of active NPU cores\n");
        template.push_str("# TYPE all_smi_npu_cores_active gauge\n");

        for (i, gpu) in gpus.iter().enumerate() {
            let labels = format!(
                "gpu=\"{}\", instance=\"{}\", uuid=\"{}\", index=\"{i}\"",
                self.gpu_name, self.instance_name, gpu.uuid
            );
            template.push_str(&format!(
                "all_smi_npu_cores_active{{{labels}}} {{{{CORES_ACTIVE_{i}}}}}\n"
            ));
        }

        // Core total count
        template.push_str("# HELP all_smi_npu_cores_total Total number of NPU cores\n");
        template.push_str("# TYPE all_smi_npu_cores_total gauge\n");

        for (i, gpu) in gpus.iter().enumerate() {
            let labels = format!(
                "gpu=\"{}\", instance=\"{}\", uuid=\"{}\", index=\"{i}\"",
                self.gpu_name, self.instance_name, gpu.uuid
            );
            template.push_str(&format!(
                "all_smi_npu_cores_total{{{labels}}} 16\n" // ATOM has 16 cores
            ));
        }
    }

    fn add_driver_metrics(&self, template: &mut String) {
        template.push_str("# HELP all_smi_rebellions_driver_version Rebellions driver version\n");
        template.push_str("# TYPE all_smi_rebellions_driver_version gauge\n");
        template.push_str(&format!(
            "all_smi_rebellions_driver_version{{instance=\"{}\"}} 1\n",
            self.instance_name
        ));
    }

    pub fn render_rebellions_response(&self, template: &str, gpus: &[GpuMetrics]) -> String {
        let mut response = template.to_string();
        let mut rng = rng();

        // Render basic GPU metrics
        response = super::common::render_basic_gpu_metrics(response, gpus);

        // Render Rebellions-specific metrics
        for (i, gpu) in gpus.iter().enumerate() {
            // NPU core utilization (can differ slightly from GPU utilization)
            let npu_util = (gpu.utilization + rng.random_range(-5.0..5.0)).clamp(0.0, 100.0);
            response =
                response.replace(&format!("{{{{NPU_UTIL_{i}}}}}"), &format!("{npu_util:.2}"));

            // Memory bandwidth utilization
            let mem_bw = rng.random_range(20.0..95.0);
            response = response.replace(&format!("{{{{NPU_BW_{i}}}}}"), &format!("{mem_bw:.2}"));

            // Active cores (based on utilization)
            let cores_active = if gpu.utilization > 80.0 {
                16 // All cores active
            } else if gpu.utilization > 50.0 {
                12
            } else if gpu.utilization > 20.0 {
                8
            } else if gpu.utilization > 0.0 {
                4
            } else {
                0
            };
            response = response.replace(
                &format!("{{{{CORES_ACTIVE_{i}}}}}"),
                &cores_active.to_string(),
            );
        }

        // Render system metrics
        response = super::common::render_system_metrics(response);

        response
    }
}

impl MockGenerator for RebellionsMockGenerator {
    fn generate(&self, config: &MockConfig) -> MockResult<MockData> {
        self.validate_config(config)?;

        let gpus = super::common::generate_gpu_metrics(config.device_count, 24_000_000_000); // 24GB
        let template = self.build_rebellions_template(&gpus);
        let response = self.render_rebellions_response(&template, &gpus);

        Ok(MockData {
            response,
            content_type: "text/plain; version=0.0.4".to_string(),
            timestamp: chrono::Utc::now(),
            platform: MockPlatform::Custom("Rebellions".to_string()),
        })
    }

    fn generate_template(&self, config: &MockConfig) -> MockResult<String> {
        self.validate_config(config)?;
        let gpus = super::common::generate_empty_gpu_metrics(config.device_count, 24_000_000_000);
        Ok(self.build_rebellions_template(&gpus))
    }

    fn render(&self, template: &str, _config: &MockConfig) -> MockResult<String> {
        Ok(template.to_string())
    }

    fn platform(&self) -> MockPlatform {
        MockPlatform::Custom("Rebellions".to_string())
    }
}
