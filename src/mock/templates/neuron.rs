//! AWS Neuron (Trainium / Inferentia) mock metric generator

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

/// HBM per NeuronCore on a Trainium device: `neuron-ls` reports
/// 34359738368 bytes (32 GiB) per device across 2 NeuronCores, and the
/// reader charges each core an even share.
const NEURONCORE_MEMORY_BYTES: u64 = 16 * 1024 * 1024 * 1024;

/// NeuronCores per Trainium device, as reported by `nc_count`.
const NEURONCORES_PER_DEVICE: usize = 2;

/// AWS Neuron mock generator.
///
/// Deliberately emits **no** temperature, power, or frequency series:
/// Trainium exposes none of the three, and the real exporter omits the
/// series rather than publishing a zero. A mock that invented them would
/// make a consumer look correct against data the hardware never produces.
pub struct NeuronMockGenerator {
    gpu_name: String,
    instance_name: String,
}

impl NeuronMockGenerator {
    pub fn new(gpu_name: Option<String>, instance_name: String) -> Self {
        Self {
            gpu_name: gpu_name.unwrap_or_else(|| "AWS Trainium1".to_string()),
            instance_name,
        }
    }

    pub fn build_neuron_template(&self, gpus: &[GpuMetrics]) -> String {
        let mut template = String::with_capacity(4096);
        self.add_core_metrics(&mut template, gpus);
        self.add_topology_metrics(&mut template, gpus);
        self.add_driver_metrics(&mut template, gpus);
        super::common::add_system_metrics(&mut template, &self.instance_name);
        template
    }

    /// Shared `all_smi_gpu_*` family. Utilization and memory only — the
    /// absent-on-real-hardware metrics are left out on purpose.
    fn add_core_metrics(&self, template: &mut String, gpus: &[GpuMetrics]) {
        let metrics = [
            ("all_smi_gpu_utilization", "GPU utilization percentage"),
            ("all_smi_gpu_memory_used_bytes", "GPU memory used in bytes"),
            (
                "all_smi_gpu_memory_total_bytes",
                "GPU memory total in bytes",
            ),
        ];

        for (metric_name, help_text) in metrics {
            template.push_str(&format!("# HELP {metric_name} {help_text}\n"));
            template.push_str(&format!("# TYPE {metric_name} gauge\n"));
            for (i, gpu) in gpus.iter().enumerate() {
                let labels = self.gpu_labels(i, &gpu.uuid);
                let placeholder = match metric_name {
                    "all_smi_gpu_utilization" => format!("{{{{UTIL_{i}}}}}"),
                    "all_smi_gpu_memory_used_bytes" => format!("{{{{MEM_USED_{i}}}}}"),
                    "all_smi_gpu_memory_total_bytes" => format!("{{{{MEM_TOTAL_{i}}}}}"),
                    _ => "0".to_string(),
                };
                template.push_str(&format!("{metric_name}{{{labels}}} {placeholder}\n"));
            }
        }
    }

    /// `all_smi_neuron_*` topology series, mirroring the real exporter:
    /// one row per NeuronCore, keyed by the *global flat* core index.
    fn add_topology_metrics(&self, template: &mut String, gpus: &[GpuMetrics]) {
        let gauges = [
            (
                "all_smi_neuron_device_index",
                "Driver enumeration index of the Neuron device",
            ),
            ("all_smi_neuron_core_index", "Global flat NeuronCore index"),
            (
                "all_smi_neuron_core_count",
                "NeuronCores on the parent Neuron device",
            ),
            (
                "all_smi_neuron_device_memory_bytes",
                "HBM on the parent Neuron device in bytes",
            ),
        ];

        for (metric_name, help_text) in gauges {
            template.push_str(&format!("# HELP {metric_name} {help_text}\n"));
            template.push_str(&format!("# TYPE {metric_name} gauge\n"));
            for (i, gpu) in gpus.iter().enumerate() {
                let labels = self.npu_labels(i, &gpu.uuid);
                let value = match metric_name {
                    "all_smi_neuron_device_index" => (i / NEURONCORES_PER_DEVICE).to_string(),
                    "all_smi_neuron_core_index" => i.to_string(),
                    "all_smi_neuron_core_count" => NEURONCORES_PER_DEVICE.to_string(),
                    _ => (NEURONCORE_MEMORY_BYTES * NEURONCORES_PER_DEVICE as u64).to_string(),
                };
                template.push_str(&format!("{metric_name}{{{labels}}} {value}\n"));
            }
        }
    }

    fn add_driver_metrics(&self, template: &mut String, gpus: &[GpuMetrics]) {
        template.push_str("# HELP all_smi_neuron_driver_info AWS Neuron kernel driver version\n");
        template.push_str("# TYPE all_smi_neuron_driver_info gauge\n");
        for (i, gpu) in gpus.iter().enumerate() {
            let labels = self.npu_labels(i, &gpu.uuid);
            template.push_str(&format!(
                "all_smi_neuron_driver_info{{{labels}, version=\"2.26.5.0\"}} 1\n"
            ));
        }
    }

    fn gpu_labels(&self, index: usize, uuid: &str) -> String {
        format!(
            "gpu=\"{}\", instance=\"{}\", gpu_uuid=\"{uuid}\", gpu_index=\"{index}\"",
            self.gpu_name, self.instance_name
        )
    }

    fn npu_labels(&self, index: usize, uuid: &str) -> String {
        format!(
            "npu=\"{}\", instance=\"{}\", npu_uuid=\"{uuid}\", npu_index=\"{index}\"",
            self.gpu_name, self.instance_name
        )
    }

    pub fn render_neuron_response(&self, template: &str, gpus: &[GpuMetrics]) -> String {
        let response = super::common::render_basic_gpu_metrics(template.to_string(), gpus);
        super::common::render_system_metrics(response)
    }
}

impl MockGenerator for NeuronMockGenerator {
    fn generate(&self, config: &MockConfig) -> MockResult<MockData> {
        self.validate_config(config)?;
        let gpus =
            super::common::generate_gpu_metrics(config.device_count, NEURONCORE_MEMORY_BYTES);
        let template = self.build_neuron_template(&gpus);
        let response = self.render_neuron_response(&template, &gpus);

        Ok(MockData {
            response,
            content_type: "text/plain; version=0.0.4".to_string(),
            timestamp: chrono::Utc::now(),
            platform: MockPlatform::Custom("AWS Neuron".to_string()),
        })
    }

    fn generate_template(&self, config: &MockConfig) -> MockResult<String> {
        self.validate_config(config)?;
        let gpus =
            super::common::generate_empty_gpu_metrics(config.device_count, NEURONCORE_MEMORY_BYTES);
        Ok(self.build_neuron_template(&gpus))
    }

    fn render(&self, template: &str, _config: &MockConfig) -> MockResult<String> {
        Ok(template.to_string())
    }

    fn platform(&self) -> MockPlatform {
        MockPlatform::Custom("AWS Neuron".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn generator() -> NeuronMockGenerator {
        NeuronMockGenerator::new(None, "node-0001".to_string())
    }

    #[test]
    fn template_numbers_cores_globally_and_groups_them_by_device() {
        let gpus = super::super::common::generate_empty_gpu_metrics(4, NEURONCORE_MEMORY_BYTES);
        let template = generator().build_neuron_template(&gpus);

        // 4 NeuronCores == 2 Trainium devices of 2 cores each.
        assert!(template.contains("all_smi_neuron_core_index"));
        assert!(template.contains("npu_index=\"3\"} 3"));
        assert!(template.contains("npu_index=\"2\"} 1"));
        assert!(template.contains("all_smi_neuron_device_memory_bytes"));
        assert!(template.contains("34359738368"));
    }

    #[test]
    fn template_omits_metrics_trainium_cannot_report() {
        let gpus = super::super::common::generate_empty_gpu_metrics(2, NEURONCORE_MEMORY_BYTES);
        let template = generator().build_neuron_template(&gpus);
        for absent in [
            "all_smi_gpu_temperature_celsius",
            "all_smi_gpu_power_consumption_watts",
            "all_smi_gpu_frequency_mhz",
        ] {
            assert!(
                !template.contains(absent),
                "{absent} must not be mocked: Trainium exposes no such reading"
            );
        }
    }
}
