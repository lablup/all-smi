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

use super::{MetricBuilder, MetricExporter};
use crate::device::CpuInfo;

pub struct CpuMetricExporter<'a> {
    pub cpu_info: &'a [CpuInfo],
}

impl<'a> CpuMetricExporter<'a> {
    pub fn new(cpu_info: &'a [CpuInfo]) -> Self {
        Self { cpu_info }
    }

    fn export_basic_metrics(&self, builder: &mut MetricBuilder, info: &CpuInfo, index: usize) {
        let base_labels = [
            ("cpu_model", info.cpu_model.as_str()),
            ("instance", info.instance.as_str()),
            ("hostname", info.hostname.as_str()),
            ("index", &index.to_string()),
        ];

        // CPU info metric with architecture
        let cpu_info_labels = [
            ("cpu_model", info.cpu_model.as_str()),
            ("instance", info.instance.as_str()),
            ("hostname", info.hostname.as_str()),
            ("index", &index.to_string()),
            ("architecture", info.architecture.as_str()),
            ("platform_type", &format!("{:?}", info.platform_type)),
        ];

        builder
            .help("all_smi_cpu_info", "CPU device information")
            .type_("all_smi_cpu_info", "gauge")
            .metric("all_smi_cpu_info", &cpu_info_labels, 1);

        // CPU utilization
        builder
            .help("all_smi_cpu_utilization", "CPU utilization percentage")
            .type_("all_smi_cpu_utilization", "gauge")
            .metric("all_smi_cpu_utilization", &base_labels, info.utilization);

        // Socket count
        builder
            .help("all_smi_cpu_socket_count", "Number of CPU sockets")
            .type_("all_smi_cpu_socket_count", "gauge")
            .metric("all_smi_cpu_socket_count", &base_labels, info.socket_count);

        // Core count
        builder
            .help("all_smi_cpu_core_count", "Total number of CPU cores")
            .type_("all_smi_cpu_core_count", "gauge")
            .metric("all_smi_cpu_core_count", &base_labels, info.total_cores);

        // Thread count
        builder
            .help("all_smi_cpu_thread_count", "Total number of CPU threads")
            .type_("all_smi_cpu_thread_count", "gauge")
            .metric("all_smi_cpu_thread_count", &base_labels, info.total_threads);

        // CPU frequency
        builder
            .help("all_smi_cpu_frequency_mhz", "CPU frequency in MHz")
            .type_("all_smi_cpu_frequency_mhz", "gauge")
            .metric(
                "all_smi_cpu_frequency_mhz",
                &base_labels,
                info.base_frequency_mhz,
            );

        // Temperature (if available)
        if let Some(temp) = info.temperature {
            builder
                .help(
                    "all_smi_cpu_temperature_celsius",
                    "CPU temperature in celsius",
                )
                .type_("all_smi_cpu_temperature_celsius", "gauge")
                .metric("all_smi_cpu_temperature_celsius", &base_labels, temp);
        }

        // Power consumption (if available)
        if let Some(power) = info.power_consumption {
            builder
                .help(
                    "all_smi_cpu_power_consumption_watts",
                    "CPU power consumption in watts",
                )
                .type_("all_smi_cpu_power_consumption_watts", "gauge")
                .metric("all_smi_cpu_power_consumption_watts", &base_labels, power);
        }
    }

    fn export_socket_metrics(&self, builder: &mut MetricBuilder, info: &CpuInfo, index: usize) {
        for socket_info in &info.per_socket_info {
            let socket_labels = [
                ("cpu_model", info.cpu_model.as_str()),
                ("instance", info.instance.as_str()),
                ("hostname", info.hostname.as_str()),
                ("cpu_index", &index.to_string()),
                ("socket_id", &socket_info.socket_id.to_string()),
            ];

            // Per-socket utilization
            builder
                .help(
                    "all_smi_cpu_socket_utilization",
                    "Per-socket CPU utilization percentage",
                )
                .type_("all_smi_cpu_socket_utilization", "gauge")
                .metric(
                    "all_smi_cpu_socket_utilization",
                    &socket_labels,
                    socket_info.utilization,
                );

            // Per-socket frequency
            builder
                .help(
                    "all_smi_cpu_socket_frequency_mhz",
                    "Per-socket CPU frequency in MHz",
                )
                .type_("all_smi_cpu_socket_frequency_mhz", "gauge")
                .metric(
                    "all_smi_cpu_socket_frequency_mhz",
                    &socket_labels,
                    socket_info.frequency_mhz,
                );

            // Per-socket temperature (if available)
            if let Some(socket_temp) = socket_info.temperature {
                builder
                    .help(
                        "all_smi_cpu_socket_temperature_celsius",
                        "Per-socket CPU temperature in celsius",
                    )
                    .type_("all_smi_cpu_socket_temperature_celsius", "gauge")
                    .metric(
                        "all_smi_cpu_socket_temperature_celsius",
                        &socket_labels,
                        socket_temp,
                    );
            }
        }
    }

    fn export_apple_silicon_metrics(
        &self,
        builder: &mut MetricBuilder,
        info: &CpuInfo,
        index: usize,
    ) {
        if let Some(apple_info) = &info.apple_silicon_info {
            let base_labels = [
                ("cpu_model", info.cpu_model.as_str()),
                ("instance", info.instance.as_str()),
                ("hostname", info.hostname.as_str()),
                ("index", &index.to_string()),
            ];

            // S-core count (M5 Pro/Max Super cores)
            if apple_info.s_core_count > 0 {
                builder
                    .help(
                        "all_smi_cpu_s_core_count",
                        "Apple Silicon S-core (Super) count",
                    )
                    .type_("all_smi_cpu_s_core_count", "gauge")
                    .metric(
                        "all_smi_cpu_s_core_count",
                        &base_labels,
                        apple_info.s_core_count,
                    );
            }

            // P-core count
            builder
                .help("all_smi_cpu_p_core_count", "Apple Silicon P-core count")
                .type_("all_smi_cpu_p_core_count", "gauge")
                .metric(
                    "all_smi_cpu_p_core_count",
                    &base_labels,
                    apple_info.p_core_count,
                );

            // E-core count
            builder
                .help("all_smi_cpu_e_core_count", "Apple Silicon E-core count")
                .type_("all_smi_cpu_e_core_count", "gauge")
                .metric(
                    "all_smi_cpu_e_core_count",
                    &base_labels,
                    apple_info.e_core_count,
                );

            // GPU core count
            builder
                .help("all_smi_cpu_gpu_core_count", "Apple Silicon GPU core count")
                .type_("all_smi_cpu_gpu_core_count", "gauge")
                .metric(
                    "all_smi_cpu_gpu_core_count",
                    &base_labels,
                    apple_info.gpu_core_count,
                );

            // S-core utilization (M5 Pro/Max)
            if apple_info.s_core_count > 0 {
                builder
                    .help(
                        "all_smi_cpu_s_core_utilization",
                        "Apple Silicon S-core (Super) utilization percentage",
                    )
                    .type_("all_smi_cpu_s_core_utilization", "gauge")
                    .metric(
                        "all_smi_cpu_s_core_utilization",
                        &base_labels,
                        apple_info.s_core_utilization,
                    );
            }

            // P-core utilization
            builder
                .help(
                    "all_smi_cpu_p_core_utilization",
                    "Apple Silicon P-core utilization percentage",
                )
                .type_("all_smi_cpu_p_core_utilization", "gauge")
                .metric(
                    "all_smi_cpu_p_core_utilization",
                    &base_labels,
                    apple_info.p_core_utilization,
                );

            // E-core utilization
            builder
                .help(
                    "all_smi_cpu_e_core_utilization",
                    "Apple Silicon E-core utilization percentage",
                )
                .type_("all_smi_cpu_e_core_utilization", "gauge")
                .metric(
                    "all_smi_cpu_e_core_utilization",
                    &base_labels,
                    apple_info.e_core_utilization,
                );

            // ANE operations per second (if available)
            if let Some(ane_ops) = apple_info.ane_ops_per_second {
                builder
                    .help(
                        "all_smi_cpu_ane_ops_per_second",
                        "Apple Neural Engine operations per second",
                    )
                    .type_("all_smi_cpu_ane_ops_per_second", "gauge")
                    .metric("all_smi_cpu_ane_ops_per_second", &base_labels, ane_ops);
            }

            // S-cluster frequency (M5 Pro/Max)
            if let Some(s_freq) = apple_info.s_cluster_frequency_mhz {
                builder
                    .help(
                        "all_smi_cpu_s_cluster_frequency_mhz",
                        "Apple Silicon S-cluster (Super) frequency in MHz",
                    )
                    .type_("all_smi_cpu_s_cluster_frequency_mhz", "gauge")
                    .metric("all_smi_cpu_s_cluster_frequency_mhz", &base_labels, s_freq);
            }

            // P-cluster frequency
            if let Some(p_freq) = apple_info.p_cluster_frequency_mhz {
                builder
                    .help(
                        "all_smi_cpu_p_cluster_frequency_mhz",
                        "Apple Silicon P-cluster frequency in MHz",
                    )
                    .type_("all_smi_cpu_p_cluster_frequency_mhz", "gauge")
                    .metric("all_smi_cpu_p_cluster_frequency_mhz", &base_labels, p_freq);
            }

            // E-cluster frequency
            if let Some(e_freq) = apple_info.e_cluster_frequency_mhz {
                builder
                    .help(
                        "all_smi_cpu_e_cluster_frequency_mhz",
                        "Apple Silicon E-cluster frequency in MHz",
                    )
                    .type_("all_smi_cpu_e_cluster_frequency_mhz", "gauge")
                    .metric("all_smi_cpu_e_cluster_frequency_mhz", &base_labels, e_freq);
            }
        }
    }

    /// Export one series per detected CPU tier (issue #473).
    ///
    /// Every series carries the canonical `cpu_tier` label. Frequency and L2
    /// cache appear only where the platform reports a per-tier value (Apple
    /// Silicon); Linux and Windows omit them while still exporting count and
    /// utilization.
    fn export_tier_metrics(&self, builder: &mut MetricBuilder, info: &CpuInfo, index: usize) {
        if info.cpu_tiers.is_empty() {
            return;
        }

        builder
            .help(
                "all_smi_cpu_tier_core_count",
                "Number of cores in a CPU tier",
            )
            .type_("all_smi_cpu_tier_core_count", "gauge")
            .help(
                "all_smi_cpu_tier_utilization",
                "CPU tier utilization percentage",
            )
            .type_("all_smi_cpu_tier_utilization", "gauge");

        // Frequency and cache are emitted only where the platform reports a
        // per-tier value, and the HELP/TYPE lines follow the samples so a
        // family with no value is entirely absent.
        if info
            .cpu_tiers
            .iter()
            .any(|tier| tier.frequency_mhz.is_some())
        {
            builder
                .help(
                    "all_smi_cpu_tier_frequency_mhz",
                    "CPU tier frequency in MHz (when the platform reports it)",
                )
                .type_("all_smi_cpu_tier_frequency_mhz", "gauge");
        }
        if info.cpu_tiers.iter().any(|tier| tier.l2_cache_mb.is_some()) {
            builder
                .help(
                    "all_smi_cpu_tier_l2_cache_mb",
                    "CPU tier L2 cache in MB (when the platform reports it)",
                )
                .type_("all_smi_cpu_tier_l2_cache_mb", "gauge");
        }

        let index_label = index.to_string();
        for tier in &info.cpu_tiers {
            let labels = [
                ("cpu_model", info.cpu_model.as_str()),
                ("instance", info.instance.as_str()),
                ("hostname", info.hostname.as_str()),
                ("index", index_label.as_str()),
                ("cpu_tier", tier.label.as_str()),
            ];

            builder.metric("all_smi_cpu_tier_core_count", &labels, tier.core_count);
            builder.metric("all_smi_cpu_tier_utilization", &labels, tier.utilization);
            if let Some(frequency) = tier.frequency_mhz {
                builder.metric("all_smi_cpu_tier_frequency_mhz", &labels, frequency);
            }
            if let Some(cache) = tier.l2_cache_mb {
                builder.metric("all_smi_cpu_tier_l2_cache_mb", &labels, cache);
            }
        }
    }

    fn export_per_core_metrics(&self, builder: &mut MetricBuilder, info: &CpuInfo, _index: usize) {
        if !info.per_core_utilization.is_empty() {
            // Help and type for per-core utilization
            builder
                .help(
                    "all_smi_cpu_core_utilization",
                    "Per-core CPU utilization percentage",
                )
                .type_("all_smi_cpu_core_utilization", "gauge");

            for core in &info.per_core_utilization {
                let core_type_str = match core.core_type {
                    crate::device::CoreType::Super => "S",
                    crate::device::CoreType::Performance => "P",
                    crate::device::CoreType::Efficiency => "E",
                    crate::device::CoreType::Standard => "C",
                };

                // Exact tier when the index resolves; the coarse core class
                // otherwise, so the label set stays uniform for every core.
                let cpu_tier = core
                    .tier_index
                    .and_then(|index| info.cpu_tiers.get(usize::from(index)))
                    .map_or_else(|| core.core_type.short_label(), |tier| tier.label.as_str());

                let core_labels = [
                    ("cpu_model", info.cpu_model.as_str()),
                    ("instance", info.instance.as_str()),
                    ("hostname", info.hostname.as_str()),
                    ("core_id", &core.core_id.to_string()),
                    ("core_type", core_type_str),
                    ("cpu_tier", cpu_tier),
                ];

                builder.metric(
                    "all_smi_cpu_core_utilization",
                    &core_labels,
                    core.utilization,
                );
            }
        }
    }
}

impl<'a> MetricExporter for CpuMetricExporter<'a> {
    fn export_metrics(&self) -> String {
        let mut builder = MetricBuilder::new();

        for (i, info) in self.cpu_info.iter().enumerate() {
            self.export_basic_metrics(&mut builder, info, i);
            self.export_socket_metrics(&mut builder, info, i);
            self.export_apple_silicon_metrics(&mut builder, info, i);
            self.export_tier_metrics(&mut builder, info, i);
            self.export_per_core_metrics(&mut builder, info, i);
        }

        builder.build()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::{CoreType, CoreUtilization, CpuPlatformType, CpuTier};

    fn cpu_with_tiers(tiers: Vec<CpuTier>, cores: Vec<CoreUtilization>) -> CpuInfo {
        CpuInfo {
            host_id: "host".to_string(),
            hostname: "host".to_string(),
            instance: "host".to_string(),
            cpu_model: "Test CPU".to_string(),
            architecture: "arm64".to_string(),
            platform_type: CpuPlatformType::Arm,
            socket_count: 1,
            total_cores: cores.len() as u32,
            total_threads: cores.len() as u32,
            base_frequency_mhz: 2000,
            max_frequency_mhz: 3000,
            cache_size_mb: 20,
            utilization: 30.0,
            cpu_tiers: tiers,
            per_core_utilization: cores,
            ..CpuInfo::test_default()
        }
    }

    fn tier(label: &str, core_type: CoreType, core_count: u32, utilization: f64) -> CpuTier {
        CpuTier {
            label: label.to_string(),
            core_type,
            core_count,
            utilization,
            frequency_mhz: None,
            l2_cache_mb: None,
        }
    }

    fn core(id: u32, tier_index: Option<u8>) -> CoreUtilization {
        CoreUtilization {
            core_id: id,
            core_type: CoreType::Standard,
            utilization: 10.0,
            tier_index,
        }
    }

    #[test]
    fn exports_tier_series_with_the_cpu_tier_label() {
        let cpu = cpu_with_tiers(
            vec![
                tier("P", CoreType::Performance, 2, 20.0),
                tier("M", CoreType::Efficiency, 2, 30.0),
                tier("E", CoreType::Efficiency, 2, 40.0),
            ],
            vec![core(0, Some(0)), core(1, Some(2))],
        );

        let output = CpuMetricExporter::new(std::slice::from_ref(&cpu)).export_metrics();

        assert!(output.contains("all_smi_cpu_tier_core_count"), "{output}");
        assert!(output.contains("all_smi_cpu_tier_utilization"), "{output}");
        assert!(output.contains("cpu_tier=\"P\""), "{output}");
        assert!(output.contains("cpu_tier=\"M\""), "{output}");
        assert!(output.contains("cpu_tier=\"E\""), "{output}");
        // Linux/Windows report no per-tier frequency or cache, so those
        // families must be absent.
        assert!(
            !output.contains("all_smi_cpu_tier_frequency_mhz"),
            "{output}"
        );
        assert!(!output.contains("all_smi_cpu_tier_l2_cache_mb"), "{output}");
        // The per-core series carries the exact tier from tier_index.
        let core_zero = output
            .lines()
            .find(|line| {
                line.starts_with("all_smi_cpu_core_utilization{") && line.contains("core_id=\"0\"")
            })
            .unwrap_or_default();
        assert!(core_zero.contains("cpu_tier=\"P\""), "{output}");
        let core_one = output
            .lines()
            .find(|line| {
                line.starts_with("all_smi_cpu_core_utilization{") && line.contains("core_id=\"1\"")
            })
            .unwrap_or_default();
        assert!(core_one.contains("cpu_tier=\"E\""), "{output}");
    }

    #[test]
    fn exports_tier_frequency_and_cache_only_when_present() {
        let mut first = tier("S", CoreType::Performance, 4, 15.0);
        first.frequency_mhz = Some(4200);
        first.l2_cache_mb = Some(16);
        let second = tier("P", CoreType::Efficiency, 6, 25.0);
        let cpu = cpu_with_tiers(vec![first, second], vec![core(0, Some(0))]);

        let output = CpuMetricExporter::new(std::slice::from_ref(&cpu)).export_metrics();
        assert!(
            output.contains("all_smi_cpu_tier_frequency_mhz"),
            "{output}"
        );
        assert!(output.contains("all_smi_cpu_tier_l2_cache_mb"), "{output}");
        // Exactly one sample per family: only the S tier reports a value.
        assert_eq!(
            output
                .lines()
                .filter(|line| line.starts_with("all_smi_cpu_tier_frequency_mhz{"))
                .count(),
            1,
            "{output}"
        );
        assert_eq!(
            output
                .lines()
                .filter(|line| line.starts_with("all_smi_cpu_tier_l2_cache_mb{"))
                .count(),
            1,
            "{output}"
        );
    }

    #[test]
    fn homogeneous_cpu_emits_no_tier_series_and_keeps_the_core_label_set() {
        let cpu = cpu_with_tiers(Vec::new(), vec![core(0, None)]);
        let output = CpuMetricExporter::new(std::slice::from_ref(&cpu)).export_metrics();

        assert!(!output.contains("all_smi_cpu_tier_core_count"), "{output}");
        assert!(!output.contains("all_smi_cpu_tier_utilization"), "{output}");
        // Falls back to the coarse class so the label set stays uniform.
        let core_line = output
            .lines()
            .find(|line| line.starts_with("all_smi_cpu_core_utilization{"))
            .unwrap_or_default();
        assert!(core_line.contains("core_type=\"C\""), "{output}");
        assert!(core_line.contains("cpu_tier=\"C\""), "{output}");
    }

    #[test]
    fn apple_series_remain_alongside_the_tier_series() {
        let mut cpu = cpu_with_tiers(
            vec![tier("S", CoreType::Performance, 4, 15.0)],
            vec![core(0, Some(0))],
        );
        cpu.apple_silicon_info = Some(crate::device::AppleSiliconCpuInfo {
            s_core_count: 4,
            p_core_count: 0,
            e_core_count: 6,
            gpu_core_count: 10,
            s_core_utilization: 15.0,
            p_core_utilization: 0.0,
            e_core_utilization: 25.0,
            ane_ops_per_second: None,
            s_cluster_frequency_mhz: None,
            p_cluster_frequency_mhz: None,
            e_cluster_frequency_mhz: None,
            s_core_l2_cache_mb: None,
            p_core_l2_cache_mb: None,
            e_core_l2_cache_mb: None,
        });

        let output = CpuMetricExporter::new(std::slice::from_ref(&cpu)).export_metrics();
        assert!(output.contains("all_smi_cpu_s_core_count"), "{output}");
        assert!(
            output.contains("all_smi_cpu_e_core_utilization"),
            "{output}"
        );
        assert!(output.contains("all_smi_cpu_tier_core_count"), "{output}");
    }
}
