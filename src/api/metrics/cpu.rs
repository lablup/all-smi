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
                    crate::device::CoreType::Performance => "P",
                    crate::device::CoreType::Efficiency => "E",
                    crate::device::CoreType::Standard => "C",
                };

                let core_labels = [
                    ("cpu_model", info.cpu_model.as_str()),
                    ("instance", info.instance.as_str()),
                    ("hostname", info.hostname.as_str()),
                    ("core_id", &core.core_id.to_string()),
                    ("core_type", core_type_str),
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

// Allow for Rust 1.85 compatibility (Launchpad)
#[allow(clippy::needless_lifetimes)]
impl<'a> MetricExporter for CpuMetricExporter<'a> {
    fn export_metrics(&self) -> String {
        let mut builder = MetricBuilder::new();

        for (i, info) in self.cpu_info.iter().enumerate() {
            self.export_basic_metrics(&mut builder, info, i);
            self.export_socket_metrics(&mut builder, info, i);
            self.export_apple_silicon_metrics(&mut builder, info, i);
            self.export_per_core_metrics(&mut builder, info, i);
        }

        builder.build()
    }
}
