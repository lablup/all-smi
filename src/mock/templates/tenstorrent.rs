//! Tenstorrent NPU mock template generator

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

/// Tenstorrent NPU mock generator
pub struct TenstorrentMockGenerator {
    gpu_name: String,
    instance_name: String,
}

impl TenstorrentMockGenerator {
    pub fn new(gpu_name: Option<String>, instance_name: String) -> Self {
        Self {
            gpu_name: gpu_name.unwrap_or_else(|| "Tenstorrent Wormhole n150s".to_string()),
            instance_name,
        }
    }

    pub fn build_tenstorrent_template(&self, gpus: &[GpuMetrics]) -> String {
        let mut template = String::with_capacity(4096);

        // Basic GPU metrics
        super::common::add_basic_gpu_metrics(
            &mut template,
            &self.gpu_name,
            &self.instance_name,
            gpus,
        );

        // Tenstorrent-specific: SoC utilization metrics
        self.add_soc_metrics(&mut template, gpus);

        // Tenstorrent-specific: Temperature sensors
        self.add_temperature_sensors(&mut template, gpus);

        // Tenstorrent-specific: Clock frequencies
        self.add_clock_metrics(&mut template, gpus);

        // Tenstorrent-specific: Voltage and current
        self.add_power_metrics(&mut template, gpus);

        // System metrics
        super::common::add_system_metrics(&mut template, &self.instance_name);

        // Driver info
        self.add_driver_metrics(&mut template);

        template
    }

    fn add_soc_metrics(&self, template: &mut String, gpus: &[GpuMetrics]) {
        // NPU SoC utilization
        template.push_str("# HELP all_smi_npu_soc_utilization NPU SoC utilization percentage\n");
        template.push_str("# TYPE all_smi_npu_soc_utilization gauge\n");

        for (i, gpu) in gpus.iter().enumerate() {
            let labels = format!(
                "npu=\"{}\", instance=\"{}\", npu_uuid=\"{}\", npu_index=\"{i}\"",
                self.gpu_name, self.instance_name, gpu.uuid
            );
            template.push_str(&format!(
                "all_smi_npu_soc_utilization{{{labels}}} {{{{SOC_UTIL_{i}}}}}\n"
            ));
        }
    }

    fn add_temperature_sensors(&self, template: &mut String, gpus: &[GpuMetrics]) {
        // Multiple temperature sensors
        let sensors = [
            ("asic", "ASIC temperature"),
            ("vreg", "Voltage regulator temperature"),
            ("inlet", "Inlet temperature"),
        ];

        for (sensor_name, description) in sensors {
            template.push_str(&format!(
                "# HELP all_smi_temperature_{sensor_name}_celsius {description} in celsius\n"
            ));
            template.push_str(&format!(
                "# TYPE all_smi_temperature_{sensor_name}_celsius gauge\n"
            ));

            for (i, gpu) in gpus.iter().enumerate() {
                let labels = format!(
                    "npu=\"{}\", instance=\"{}\", npu_uuid=\"{}\", npu_index=\"{i}\"",
                    self.gpu_name, self.instance_name, gpu.uuid
                );
                let placeholder = format!("{{{{{}_TEMP_{i}}}}}", sensor_name.to_uppercase());
                template.push_str(&format!(
                    "all_smi_temperature_{sensor_name}_celsius{{{labels}}} {placeholder}\n"
                ));
            }
        }
    }

    fn add_clock_metrics(&self, template: &mut String, gpus: &[GpuMetrics]) {
        // Multiple clock domains
        let clocks = [
            ("aiclk", "AI clock frequency"),
            ("axiclk", "AXI clock frequency"),
            ("arcclk", "ARC clock frequency"),
        ];

        for (clock_name, description) in clocks {
            template.push_str(&format!(
                "# HELP all_smi_{clock_name}_mhz {description} in MHz\n"
            ));
            template.push_str(&format!("# TYPE all_smi_{clock_name}_mhz gauge\n"));

            for (i, gpu) in gpus.iter().enumerate() {
                let labels = format!(
                    "npu=\"{}\", instance=\"{}\", npu_uuid=\"{}\", npu_index=\"{i}\"",
                    self.gpu_name, self.instance_name, gpu.uuid
                );
                let placeholder = format!("{{{{{}_{i}}}}}", clock_name.to_uppercase());
                template.push_str(&format!(
                    "all_smi_{clock_name}_mhz{{{labels}}} {placeholder}\n"
                ));
            }
        }
    }

    fn add_power_metrics(&self, template: &mut String, gpus: &[GpuMetrics]) {
        // Voltage
        template.push_str("# HELP all_smi_voltage_volts Core voltage in volts\n");
        template.push_str("# TYPE all_smi_voltage_volts gauge\n");

        for (i, gpu) in gpus.iter().enumerate() {
            let labels = format!(
                "npu=\"{}\", instance=\"{}\", npu_uuid=\"{}\", npu_index=\"{i}\"",
                self.gpu_name, self.instance_name, gpu.uuid
            );
            template.push_str(&format!(
                "all_smi_voltage_volts{{{labels}}} {{{{VOLTAGE_{i}}}}}\n"
            ));
        }

        // Current
        template.push_str("# HELP all_smi_current_amperes Core current in amperes\n");
        template.push_str("# TYPE all_smi_current_amperes gauge\n");

        for (i, gpu) in gpus.iter().enumerate() {
            let labels = format!(
                "npu=\"{}\", instance=\"{}\", npu_uuid=\"{}\", npu_index=\"{i}\"",
                self.gpu_name, self.instance_name, gpu.uuid
            );
            template.push_str(&format!(
                "all_smi_current_amperes{{{labels}}} {{{{CURRENT_{i}}}}}\n"
            ));
        }
    }

    fn add_driver_metrics(&self, template: &mut String) {
        template.push_str("# HELP all_smi_tenstorrent_driver_version Tenstorrent driver version\n");
        template.push_str("# TYPE all_smi_tenstorrent_driver_version gauge\n");
        template.push_str(&format!(
            "all_smi_tenstorrent_driver_version{{instance=\"{}\"}} 1\n",
            self.instance_name
        ));
    }

    pub fn render_tenstorrent_response(&self, template: &str, gpus: &[GpuMetrics]) -> String {
        let mut response = template.to_string();
        let mut rng = rng();

        // Render basic GPU metrics
        response = super::common::render_basic_gpu_metrics(response, gpus);

        // Render Tenstorrent-specific metrics
        for (i, gpu) in gpus.iter().enumerate() {
            // SoC utilization (similar to GPU utilization but can differ)
            let soc_util = (gpu.utilization + rng.random_range(-10.0..10.0)).clamp(0.0, 100.0);
            response =
                response.replace(&format!("{{{{SOC_UTIL_{i}}}}}"), &format!("{soc_util:.2}"));

            // Temperature sensors (variations from main temp)
            let asic_temp = gpu.temperature_celsius;
            let vreg_temp =
                (asic_temp as f32 + rng.random_range(-5.0..5.0)).clamp(30.0, 90.0) as u32;
            let inlet_temp =
                (asic_temp as f32 - rng.random_range(10.0..20.0)).clamp(20.0, 60.0) as u32;

            response = response
                .replace(&format!("{{{{ASIC_TEMP_{i}}}}}"), &asic_temp.to_string())
                .replace(&format!("{{{{VREG_TEMP_{i}}}}}"), &vreg_temp.to_string())
                .replace(&format!("{{{{INLET_TEMP_{i}}}}}"), &inlet_temp.to_string());

            // Clock frequencies
            let ai_clk = gpu.frequency_mhz;
            let axi_clk = rng.random_range(800..1200);
            let arc_clk = rng.random_range(500..800);

            response = response
                .replace(&format!("{{{{AICLK_{i}}}}}"), &ai_clk.to_string())
                .replace(&format!("{{{{AXICLK_{i}}}}}"), &axi_clk.to_string())
                .replace(&format!("{{{{ARCCLK_{i}}}}}"), &arc_clk.to_string());

            // Voltage and current (derived from power)
            let voltage = rng.random_range(0.85..0.95);
            let current = gpu.power_consumption_watts / voltage;

            response = response
                .replace(&format!("{{{{VOLTAGE_{i}}}}}"), &format!("{voltage:.3}"))
                .replace(&format!("{{{{CURRENT_{i}}}}}"), &format!("{current:.1}"));
        }

        // Render system metrics
        response = super::common::render_system_metrics(response);

        response
    }
}

impl MockGenerator for TenstorrentMockGenerator {
    fn generate(&self, config: &MockConfig) -> MockResult<MockData> {
        self.validate_config(config)?;

        let gpus = super::common::generate_gpu_metrics(config.device_count, 32_000_000_000); // 32GB
        let template = self.build_tenstorrent_template(&gpus);
        let response = self.render_tenstorrent_response(&template, &gpus);

        Ok(MockData {
            response,
            content_type: "text/plain; version=0.0.4".to_string(),
            timestamp: chrono::Utc::now(),
            platform: MockPlatform::Custom("Tenstorrent".to_string()),
        })
    }

    fn generate_template(&self, config: &MockConfig) -> MockResult<String> {
        self.validate_config(config)?;
        let gpus = super::common::generate_empty_gpu_metrics(config.device_count, 32_000_000_000);
        Ok(self.build_tenstorrent_template(&gpus))
    }

    fn render(&self, template: &str, _config: &MockConfig) -> MockResult<String> {
        Ok(template.to_string())
    }

    fn platform(&self) -> MockPlatform {
        MockPlatform::Custom("Tenstorrent".to_string())
    }
}
