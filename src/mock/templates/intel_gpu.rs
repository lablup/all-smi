//! Intel client GPU mock template generator (issue #244).

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
use rand::{RngExt, rng};

/// Intel client GPU mock generator. Modelled on
/// [`super::amd_gpu::AmdGpuMockGenerator`] — same metric set, same
/// template shape — with an `all_smi_intel_driver_version` info metric
/// replacing the AMD `all_smi_amd_rocm_version` analogue, and a memory
/// table sized for client SKUs (4–16 GB) instead of HBM stacks.
pub struct IntelGpuMockGenerator {
    gpu_name: String,
    instance_name: String,
}

impl IntelGpuMockGenerator {
    /// Create a new Intel client GPU mock generator. Supported `--gpu-name`
    /// values include discrete Arc (e.g. `Intel Arc B580 12GB`,
    /// `Intel Arc A770 16GB`, `Intel Arc A750 8GB`) and integrated
    /// families (`Intel Arc Graphics` on Core Ultra, `Intel Iris Xe
    /// Graphics`, `Intel UHD Graphics 770`). Integrated names yield
    /// `0` total memory to mirror the production reader semantics.
    pub fn new(gpu_name: Option<String>, instance_name: String) -> Self {
        Self {
            gpu_name: gpu_name
                .unwrap_or_else(|| crate::mock::constants::DEFAULT_INTEL_GPU_NAME.to_string()),
            instance_name,
        }
    }

    /// Parse memory size from GPU name. Returns 0 for integrated GPUs
    /// — they have no dedicated VRAM and the reader on a real Intel
    /// host reports `0` total with a `detail["Memory"]` note. We mirror
    /// that semantics here so library consumers see the same shape from
    /// the mock as they would from a live integrated Intel host.
    fn get_gpu_memory_bytes(&self) -> u64 {
        // Match the actual production reader: integrated GPUs report 0.
        if is_integrated_name(&self.gpu_name) {
            return 0;
        }
        // Discrete Arc: scan for the `NNGB` token.
        const GB: u64 = 1024 * 1024 * 1024;
        if self.gpu_name.contains("16GB") {
            16 * GB
        } else if self.gpu_name.contains("12GB") {
            12 * GB
        } else if self.gpu_name.contains("10GB") {
            10 * GB
        } else if self.gpu_name.contains("8GB") {
            8 * GB
        } else if self.gpu_name.contains("6GB") {
            6 * GB
        } else if self.gpu_name.contains("4GB") {
            4 * GB
        } else if self.gpu_name.to_lowercase().contains("a770") {
            16 * GB
        } else if self.gpu_name.to_lowercase().contains("a750") {
            8 * GB
        } else if self.gpu_name.to_lowercase().contains("b580") {
            12 * GB
        } else if self.gpu_name.to_lowercase().contains("b570") {
            10 * GB
        } else {
            // Default to 12 GB (Arc B580) when no size hint is present.
            12 * GB
        }
    }

    /// Build the Intel-specific template (metric names + placeholders).
    pub fn build_intel_template(
        &self,
        gpus: &[GpuMetrics],
        cpu: &CpuMetrics,
        memory: &MemoryMetrics,
    ) -> String {
        let mut template = String::with_capacity(4096);
        self.add_gpu_metrics(&mut template, gpus);
        self.add_intel_driver_info(&mut template);
        self.add_system_metrics(&mut template, cpu, memory);
        template
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

        self.add_gpu_info_metric(template, gpus);
    }

    fn add_gpu_info_metric(&self, template: &mut String, gpus: &[GpuMetrics]) {
        use crate::mock::constants::DEFAULT_INTEL_DRIVER_VERSION;

        template.push_str("# HELP all_smi_gpu_info GPU device information\n");
        template.push_str("# TYPE all_smi_gpu_info gauge\n");

        let variant = if is_integrated_name(&self.gpu_name) {
            "Integrated"
        } else {
            "Discrete"
        };

        for (i, gpu) in gpus.iter().enumerate() {
            let labels = format!(
                "gpu=\"{}\", instance=\"{}\", gpu_uuid=\"{}\", gpu_index=\"{i}\", type=\"GPU\", \
                 driver_version=\"{DEFAULT_INTEL_DRIVER_VERSION}\", variant=\"{variant}\", \
                 lib_name=\"Intel Graphics Driver\", lib_version=\"{DEFAULT_INTEL_DRIVER_VERSION}\"",
                self.gpu_name, self.instance_name, gpu.uuid
            );
            template.push_str(&format!("all_smi_gpu_info{{{labels}}} 1\n"));
        }
    }

    fn add_intel_driver_info(&self, template: &mut String) {
        use crate::mock::constants::DEFAULT_INTEL_DRIVER_VERSION;

        template.push_str("# HELP all_smi_intel_driver_version Intel Graphics Driver version\n");
        template.push_str("# TYPE all_smi_intel_driver_version gauge\n");
        template.push_str(&format!(
            "all_smi_intel_driver_version{{instance=\"{}\", version=\"{DEFAULT_INTEL_DRIVER_VERSION}\"}} 1\n",
            self.instance_name
        ));
    }

    fn add_system_metrics(&self, template: &mut String, cpu: &CpuMetrics, memory: &MemoryMetrics) {
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

    /// Render dynamic values for Intel GPUs into a previously built template.
    pub fn render_intel_response(
        &self,
        template: &str,
        gpus: &[GpuMetrics],
        cpu: &CpuMetrics,
        memory: &MemoryMetrics,
    ) -> String {
        let mut response = template.to_string();

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
        }

        response = response
            .replace("{{CPU_UTIL}}", &format!("{:.2}", cpu.utilization))
            .replace("{{MEM_USED}}", &memory.used_bytes.to_string());

        response
    }
}

/// Heuristic: a GPU name represents an integrated Intel GPU when it
/// lacks an Arc model number (A770 / B580 / etc.) AND contains one of
/// the integrated-family tokens. Matches the production reader's
/// classification so the mock and the real reader agree on `Variant`.
fn is_integrated_name(name: &str) -> bool {
    let lower = name.to_lowercase();
    let has_arc_model = lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|token| {
            let bytes = token.as_bytes();
            if bytes.len() < 4 {
                return false;
            }
            let first = bytes[0] as char;
            matches!(first, 'a' | 'b' | 'c' | 'd') && bytes[1..].iter().all(|b| b.is_ascii_digit())
        });
    if has_arc_model {
        return false;
    }
    lower.contains("iris")
        || lower.contains("uhd")
        || lower.contains("hd graphics")
        || lower.contains("xe graphics")
        || (lower.contains("arc") && lower.contains("graphics"))
        || lower.contains("intel graphics")
}

impl MockGenerator for IntelGpuMockGenerator {
    fn generate(&self, config: &MockConfig) -> MockResult<MockData> {
        self.validate_config(config)?;

        let mut rng = rng();
        let memory_total_bytes = self.get_gpu_memory_bytes();
        // Cap the random memory-used draw at 80% of total. Integrated
        // GPUs (memory_total_bytes == 0) get a flat zero, mirroring the
        // real reader.
        let memory_used_max = (memory_total_bytes / 10).saturating_mul(8);

        let gpus: Vec<GpuMetrics> = (0..config.device_count)
            .map(|_| {
                let memory_used_bytes = if memory_total_bytes == 0 {
                    0
                } else {
                    rng.random_range(memory_total_bytes / 10..memory_used_max.max(2_000_000_000))
                };
                GpuMetrics {
                    uuid: crate::mock::metrics::gpu::generate_uuid_with_rng(&mut rng),
                    utilization: rng.random_range(0.0..100.0),
                    memory_used_bytes,
                    memory_total_bytes,
                    temperature_celsius: rng.random_range(40..78),
                    // Discrete Arc client cards sit in 110–225W
                    // depending on SKU; integrated Intel GPUs are
                    // package-shared and report low single-digit
                    // watts. Pick a representative draw.
                    power_consumption_watts: if memory_total_bytes == 0 {
                        rng.random_range(2.0..15.0)
                    } else {
                        rng.random_range(80.0..225.0)
                    },
                    frequency_mhz: rng.random_range(1100..2400),
                    ane_utilization_watts: 0.0,
                    thermal_pressure_level: None,
                }
            })
            .collect();

        let cpu = CpuMetrics {
            // Intel client GPUs (Arc + iGPU) typically live in Intel
            // Core / Core Ultra hosts. Pick a representative model.
            model: "Intel Core Ultra 7 165H".to_string(),
            utilization: rng.random_range(10.0..90.0),
            socket_count: 1,
            core_count: 16,
            thread_count: 22,
            frequency_mhz: 3800,
            temperature_celsius: Some(60),
            power_consumption_watts: Some(45.0),
            socket_utilizations: vec![rng.random_range(10.0..90.0)],
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
            total_bytes: 64 * 1024 * 1024 * 1024, // 64 GiB
            used_bytes: rng.random_range(8_000_000_000..40_000_000_000),
            available_bytes: rng.random_range(20_000_000_000..50_000_000_000),
            free_bytes: rng.random_range(10_000_000_000..30_000_000_000),
            cached_bytes: rng.random_range(2_000_000_000..10_000_000_000),
            buffers_bytes: rng.random_range(100_000_000..1_000_000_000),
            swap_total_bytes: 0,
            swap_used_bytes: 0,
            swap_free_bytes: 0,
            utilization: rng.random_range(10.0..80.0),
        };

        let template = self.build_intel_template(&gpus, &cpu, &memory);
        let response = self.render_intel_response(&template, &gpus, &cpu, &memory);

        Ok(MockData {
            response,
            content_type: "text/plain; version=0.0.4".to_string(),
            timestamp: chrono::Utc::now(),
            platform: MockPlatform::IntelGpu,
        })
    }

    fn generate_template(&self, config: &MockConfig) -> MockResult<String> {
        self.validate_config(config)?;

        let memory_total_bytes = self.get_gpu_memory_bytes();
        let gpus: Vec<GpuMetrics> = (0..config.device_count)
            .map(|i| GpuMetrics {
                uuid: format!("GPU-{:08x}", i as u32),
                utilization: 0.0,
                memory_used_bytes: 0,
                memory_total_bytes,
                temperature_celsius: 0,
                power_consumption_watts: 0.0,
                frequency_mhz: 0,
                ane_utilization_watts: 0.0,
                thermal_pressure_level: None,
            })
            .collect();

        let cpu = CpuMetrics {
            model: "Intel Core Ultra 7 165H".to_string(),
            utilization: 0.0,
            socket_count: 1,
            core_count: 16,
            thread_count: 22,
            frequency_mhz: 3800,
            temperature_celsius: Some(60),
            power_consumption_watts: Some(45.0),
            socket_utilizations: vec![0.0],
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
            total_bytes: 64 * 1024 * 1024 * 1024,
            used_bytes: 0,
            available_bytes: 64 * 1024 * 1024 * 1024,
            free_bytes: 64 * 1024 * 1024 * 1024,
            cached_bytes: 0,
            buffers_bytes: 0,
            swap_total_bytes: 0,
            swap_used_bytes: 0,
            swap_free_bytes: 0,
            utilization: 0.0,
        };

        Ok(self.build_intel_template(&gpus, &cpu, &memory))
    }

    fn render(&self, template: &str, config: &MockConfig) -> MockResult<String> {
        self.validate_config(config)?;
        Ok(template.to_string())
    }

    fn platform(&self) -> MockPlatform {
        MockPlatform::IntelGpu
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_contains_canonical_metric_names() {
        let g = IntelGpuMockGenerator::new(
            Some("Intel Arc B580 12GB".to_string()),
            "intel-test".to_string(),
        );
        let cfg = MockConfig {
            platform: MockPlatform::IntelGpu,
            device_count: 2,
            ..MockConfig::default()
        };
        let template = g.generate_template(&cfg).expect("template builds");
        // Core metric families.
        assert!(template.contains("all_smi_gpu_utilization"));
        assert!(template.contains("all_smi_gpu_memory_used_bytes"));
        assert!(template.contains("all_smi_gpu_memory_total_bytes"));
        assert!(template.contains("all_smi_gpu_temperature_celsius"));
        assert!(template.contains("all_smi_gpu_power_consumption_watts"));
        assert!(template.contains("all_smi_gpu_frequency_mhz"));
        // Intel-specific info metric — analogue of AMD's
        // `all_smi_amd_rocm_version`.
        assert!(template.contains("all_smi_intel_driver_version"));
        // GPU info metric carries variant/driver labels.
        assert!(template.contains("all_smi_gpu_info"));
        assert!(template.contains("variant=\"Discrete\""));
        // CPU + memory.
        assert!(template.contains("all_smi_cpu_utilization"));
        assert!(template.contains("all_smi_memory_total_bytes"));
    }

    #[test]
    fn discrete_arc_b580_reports_12gb() {
        let g =
            IntelGpuMockGenerator::new(Some("Intel Arc B580 12GB".to_string()), "i".to_string());
        assert_eq!(g.get_gpu_memory_bytes(), 12 * 1024 * 1024 * 1024);
    }

    #[test]
    fn discrete_arc_a770_reports_16gb() {
        let g =
            IntelGpuMockGenerator::new(Some("Intel Arc A770 16GB".to_string()), "i".to_string());
        assert_eq!(g.get_gpu_memory_bytes(), 16 * 1024 * 1024 * 1024);
    }

    #[test]
    fn arc_a750_defaults_to_8gb() {
        let g = IntelGpuMockGenerator::new(Some("Intel Arc A750".to_string()), "i".to_string());
        assert_eq!(g.get_gpu_memory_bytes(), 8 * 1024 * 1024 * 1024);
    }

    #[test]
    fn integrated_reports_zero_memory() {
        let g =
            IntelGpuMockGenerator::new(Some("Intel Iris Xe Graphics".to_string()), "i".to_string());
        assert_eq!(g.get_gpu_memory_bytes(), 0);

        let g2 =
            IntelGpuMockGenerator::new(Some("Intel UHD Graphics 770".to_string()), "i".to_string());
        assert_eq!(g2.get_gpu_memory_bytes(), 0);
    }

    #[test]
    fn meteor_lake_arc_igpu_classified_as_integrated() {
        // The Meteor Lake iGPU ships as "Intel Arc Graphics" with no
        // model number — must be classified as integrated.
        assert!(is_integrated_name("Intel(R) Arc(TM) Graphics"));
        let g = IntelGpuMockGenerator::new(
            Some("Intel(R) Arc(TM) Graphics".to_string()),
            "i".to_string(),
        );
        assert_eq!(g.get_gpu_memory_bytes(), 0);
    }

    #[test]
    fn discrete_arc_not_classified_as_integrated() {
        assert!(!is_integrated_name("Intel Arc A770 16GB"));
        assert!(!is_integrated_name("Intel Arc B580 12GB"));
    }

    #[test]
    fn generate_leaves_no_unreplaced_placeholders() {
        // `generate_template` intentionally contains `{{UTIL_N}}` etc.;
        // `generate` must replace every placeholder before returning the
        // response. Any leftover `{{` would be emitted verbatim to the
        // Prometheus scraper, breaking numeric-value parsing.
        let g = IntelGpuMockGenerator::new(
            Some("Intel Arc B580 12GB".to_string()),
            "intel-test".to_string(),
        );
        let cfg = MockConfig {
            platform: MockPlatform::IntelGpu,
            device_count: 2,
            ..MockConfig::default()
        };
        let data = g.generate(&cfg).expect("generate succeeds");
        assert!(
            !data.response.contains("{{"),
            "unreplaced placeholder in output: {}",
            // Show the first occurrence for easier diagnosis.
            data.response
                .lines()
                .find(|l| l.contains("{{"))
                .unwrap_or("<not found>")
        );
        // Verify the output is Prometheus text format: every metric line
        // must be preceded by # HELP and # TYPE comment blocks.
        assert!(data.response.contains("# HELP "), "missing # HELP block");
        assert!(data.response.contains("# TYPE "), "missing # TYPE block");
        assert_eq!(data.content_type, "text/plain; version=0.0.4");
    }
}
