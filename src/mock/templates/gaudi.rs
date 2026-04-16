//! Intel Gaudi NPU mock template generator

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
    MockConfig, MockData, MockError, MockGenerator, MockPlatform, MockResult,
};
use rand::{RngExt, rng};

/// Intel Gaudi NPU mock generator
pub struct GaudiMockGenerator {
    gpu_name: String,
    instance_name: String,
}

impl GaudiMockGenerator {
    pub fn new(gpu_name: Option<String>, instance_name: String) -> Self {
        Self {
            gpu_name: gpu_name.unwrap_or_else(|| "Intel Gaudi 3".to_string()),
            instance_name,
        }
    }

    pub fn build_gaudi_template(&self, gpus: &[GpuMetrics]) -> String {
        let mut template = String::with_capacity(4096);

        // Basic GPU metrics
        super::common::add_basic_gpu_metrics(
            &mut template,
            &self.gpu_name,
            &self.instance_name,
            gpus,
        );

        // Gaudi-specific: AIP (AI Processor) utilization
        self.add_aip_metrics(&mut template, gpus);

        // Gaudi-specific: Power metrics
        self.add_power_metrics(&mut template, gpus);

        // System metrics
        super::common::add_system_metrics(&mut template, &self.instance_name);

        // Driver info
        self.add_driver_metrics(&mut template);

        template
    }

    fn add_aip_metrics(&self, template: &mut String, gpus: &[GpuMetrics]) {
        // AIP (AI Processor) utilization
        template
            .push_str("# HELP all_smi_gaudi_aip_utilization Gaudi AIP utilization percentage\n");
        template.push_str("# TYPE all_smi_gaudi_aip_utilization gauge\n");

        for (i, gpu) in gpus.iter().enumerate() {
            let labels = format!(
                "gpu=\"{}\", instance=\"{}\", gpu_uuid=\"{}\", gpu_index=\"{i}\"",
                self.gpu_name, self.instance_name, gpu.uuid
            );
            template.push_str(&format!(
                "all_smi_gaudi_aip_utilization{{{labels}}} {{{{AIP_UTIL_{i}}}}}\n"
            ));
        }

        // AIP memory bandwidth utilization
        template.push_str("# HELP all_smi_gaudi_memory_bandwidth_percent Gaudi memory bandwidth utilization percentage\n");
        template.push_str("# TYPE all_smi_gaudi_memory_bandwidth_percent gauge\n");

        for (i, gpu) in gpus.iter().enumerate() {
            let labels = format!(
                "gpu=\"{}\", instance=\"{}\", gpu_uuid=\"{}\", gpu_index=\"{i}\"",
                self.gpu_name, self.instance_name, gpu.uuid
            );
            template.push_str(&format!(
                "all_smi_gaudi_memory_bandwidth_percent{{{labels}}} {{{{MEM_BW_{i}}}}}\n"
            ));
        }
    }

    fn add_power_metrics(&self, template: &mut String, gpus: &[GpuMetrics]) {
        // Maximum power limit
        template
            .push_str("# HELP all_smi_gaudi_power_max_watts Gaudi maximum power limit in watts\n");
        template.push_str("# TYPE all_smi_gaudi_power_max_watts gauge\n");

        for (i, gpu) in gpus.iter().enumerate() {
            let labels = format!(
                "gpu=\"{}\", instance=\"{}\", gpu_uuid=\"{}\", gpu_index=\"{i}\"",
                self.gpu_name, self.instance_name, gpu.uuid
            );
            template.push_str(&format!(
                "all_smi_gaudi_power_max_watts{{{labels}}} 850\n" // Gaudi 3 max 850W
            ));
        }

        // Power efficiency (performance per watt)
        template.push_str("# HELP all_smi_gaudi_power_efficiency Performance per watt\n");
        template.push_str("# TYPE all_smi_gaudi_power_efficiency gauge\n");

        for (i, gpu) in gpus.iter().enumerate() {
            let labels = format!(
                "gpu=\"{}\", instance=\"{}\", gpu_uuid=\"{}\", gpu_index=\"{i}\"",
                self.gpu_name, self.instance_name, gpu.uuid
            );
            template.push_str(&format!(
                "all_smi_gaudi_power_efficiency{{{labels}}} {{{{PWR_EFF_{i}}}}}\n"
            ));
        }
    }

    fn add_driver_metrics(&self, template: &mut String) {
        template
            .push_str("# HELP all_smi_gaudi_driver_version Intel Gaudi (Habana) driver version\n");
        template.push_str("# TYPE all_smi_gaudi_driver_version gauge\n");
        template.push_str(&format!(
            "all_smi_gaudi_driver_version{{instance=\"{}\"}} 1\n",
            self.instance_name
        ));
    }

    pub fn render_gaudi_response(&self, template: &str, gpus: &[GpuMetrics]) -> String {
        let mut response = template.to_string();
        let mut rng = rng();

        // Render basic GPU metrics
        response = super::common::render_basic_gpu_metrics(response, gpus);

        // Render Gaudi-specific metrics
        for (i, gpu) in gpus.iter().enumerate() {
            // AIP utilization (same as GPU utilization for simplicity)
            let aip_util = gpu.utilization;
            response =
                response.replace(&format!("{{{{AIP_UTIL_{i}}}}}"), &format!("{aip_util:.2}"));

            // Memory bandwidth utilization (varies with workload)
            let mem_bw = if gpu.utilization > 80.0 {
                rng.random_range(85.0..98.0)
            } else if gpu.utilization > 50.0 {
                rng.random_range(60.0..85.0)
            } else if gpu.utilization > 20.0 {
                rng.random_range(30.0..60.0)
            } else {
                rng.random_range(5.0..30.0)
            };
            response = response.replace(&format!("{{{{MEM_BW_{i}}}}}"), &format!("{mem_bw:.2}"));

            // Power efficiency (utilization / power draw)
            let power_efficiency = if gpu.power_consumption_watts > 0.0 {
                gpu.utilization / gpu.power_consumption_watts
            } else {
                0.0
            };
            response = response.replace(
                &format!("{{{{PWR_EFF_{i}}}}}"),
                &format!("{power_efficiency:.4}"),
            );
        }

        // Render system metrics
        response = super::common::render_system_metrics(response);

        response
    }
}

impl MockGenerator for GaudiMockGenerator {
    fn generate(&self, config: &MockConfig) -> MockResult<MockData> {
        self.validate_config(config)?;

        // Gaudi 3 has 128GB HBM2e memory
        let gpus = super::common::generate_gpu_metrics(config.device_count, 128_000_000_000);
        let template = self.build_gaudi_template(&gpus);
        let response = self.render_gaudi_response(&template, &gpus);

        Ok(MockData {
            response,
            content_type: "text/plain; version=0.0.4".to_string(),
            timestamp: chrono::Utc::now(),
            platform: MockPlatform::Custom("Intel Gaudi".to_string()),
        })
    }

    fn generate_template(&self, config: &MockConfig) -> MockResult<String> {
        self.validate_config(config)?;
        let gpus = super::common::generate_empty_gpu_metrics(config.device_count, 128_000_000_000);
        Ok(self.build_gaudi_template(&gpus))
    }

    fn render(&self, template: &str, _config: &MockConfig) -> MockResult<String> {
        Ok(template.to_string())
    }

    fn platform(&self) -> MockPlatform {
        MockPlatform::Custom("Intel Gaudi".to_string())
    }

    fn validate_config(&self, config: &MockConfig) -> MockResult<()> {
        if config.device_count == 0 {
            return Err(MockError::ConfigError(
                "Device count must be greater than 0".to_string(),
            ));
        }
        if config.device_count > 8 {
            return Err(MockError::ConfigError(
                "Intel Gaudi mock supports up to 8 devices per node".to_string(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gaudi_generator_creation() {
        let generator = GaudiMockGenerator::new(None, "test-node".to_string());
        assert_eq!(generator.gpu_name, "Intel Gaudi 3");
        assert_eq!(generator.instance_name, "test-node");
    }

    #[test]
    fn test_gaudi_generator_custom_name() {
        let generator =
            GaudiMockGenerator::new(Some("Gaudi 2".to_string()), "test-node".to_string());
        assert_eq!(generator.gpu_name, "Gaudi 2");
    }

    #[test]
    fn test_gaudi_template_generation() {
        let generator = GaudiMockGenerator::new(None, "test-node".to_string());
        let config = MockConfig {
            device_count: 2,
            ..Default::default()
        };

        let result = generator.generate_template(&config);
        assert!(result.is_ok());

        let template = result.unwrap();
        assert!(template.contains("all_smi_gaudi_aip_utilization"));
        assert!(template.contains("all_smi_gaudi_memory_bandwidth_percent"));
        assert!(template.contains("all_smi_gaudi_power_max_watts"));
    }

    #[test]
    fn test_gaudi_validation() {
        let generator = GaudiMockGenerator::new(None, "test-node".to_string());

        let valid_config = MockConfig {
            device_count: 8,
            ..Default::default()
        };
        assert!(generator.validate_config(&valid_config).is_ok());

        let invalid_config = MockConfig {
            device_count: 0,
            ..Default::default()
        };
        assert!(generator.validate_config(&invalid_config).is_err());

        let too_many_devices = MockConfig {
            device_count: 16,
            ..Default::default()
        };
        assert!(generator.validate_config(&too_many_devices).is_err());
    }
}
