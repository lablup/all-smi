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
use crate::device::MemoryInfo;

pub struct MemoryMetricExporter<'a> {
    pub memory_info: &'a [MemoryInfo],
}

impl<'a> MemoryMetricExporter<'a> {
    pub fn new(memory_info: &'a [MemoryInfo]) -> Self {
        Self { memory_info }
    }

    fn export_basic_metrics(&self, builder: &mut MetricBuilder, info: &MemoryInfo, index: usize) {
        let base_labels = [
            ("instance", info.instance.as_str()),
            ("hostname", info.hostname.as_str()),
            ("index", &index.to_string()),
        ];

        // Total memory
        builder
            .help("all_smi_memory_total_bytes", "Total system memory in bytes")
            .type_("all_smi_memory_total_bytes", "gauge")
            .metric("all_smi_memory_total_bytes", &base_labels, info.total_bytes);

        // Used memory
        builder
            .help("all_smi_memory_used_bytes", "Used system memory in bytes")
            .type_("all_smi_memory_used_bytes", "gauge")
            .metric("all_smi_memory_used_bytes", &base_labels, info.used_bytes);

        // Available memory
        builder
            .help(
                "all_smi_memory_available_bytes",
                "Available system memory in bytes",
            )
            .type_("all_smi_memory_available_bytes", "gauge")
            .metric(
                "all_smi_memory_available_bytes",
                &base_labels,
                info.available_bytes,
            );

        // Free memory
        builder
            .help("all_smi_memory_free_bytes", "Free system memory in bytes")
            .type_("all_smi_memory_free_bytes", "gauge")
            .metric("all_smi_memory_free_bytes", &base_labels, info.free_bytes);

        // Memory utilization
        builder
            .help(
                "all_smi_memory_utilization",
                "Memory utilization percentage",
            )
            .type_("all_smi_memory_utilization", "gauge")
            .metric("all_smi_memory_utilization", &base_labels, info.utilization);
    }

    fn export_swap_metrics(&self, builder: &mut MetricBuilder, info: &MemoryInfo, index: usize) {
        if info.swap_total_bytes == 0 {
            return;
        }

        let base_labels = [
            ("instance", info.instance.as_str()),
            ("hostname", info.hostname.as_str()),
            ("index", &index.to_string()),
        ];

        // Total swap
        builder
            .help("all_smi_swap_total_bytes", "Total swap space in bytes")
            .type_("all_smi_swap_total_bytes", "gauge")
            .metric(
                "all_smi_swap_total_bytes",
                &base_labels,
                info.swap_total_bytes,
            );

        // Used swap
        builder
            .help("all_smi_swap_used_bytes", "Used swap space in bytes")
            .type_("all_smi_swap_used_bytes", "gauge")
            .metric(
                "all_smi_swap_used_bytes",
                &base_labels,
                info.swap_used_bytes,
            );

        // Free swap
        builder
            .help("all_smi_swap_free_bytes", "Free swap space in bytes")
            .type_("all_smi_swap_free_bytes", "gauge")
            .metric(
                "all_smi_swap_free_bytes",
                &base_labels,
                info.swap_free_bytes,
            );
    }

    fn export_linux_specific_metrics(
        &self,
        builder: &mut MetricBuilder,
        info: &MemoryInfo,
        index: usize,
    ) {
        let base_labels = [
            ("instance", info.instance.as_str()),
            ("hostname", info.hostname.as_str()),
            ("index", &index.to_string()),
        ];

        // Buffers (Linux specific)
        if info.buffers_bytes > 0 {
            builder
                .help(
                    "all_smi_memory_buffers_bytes",
                    "Memory used for buffers in bytes",
                )
                .type_("all_smi_memory_buffers_bytes", "gauge")
                .metric(
                    "all_smi_memory_buffers_bytes",
                    &base_labels,
                    info.buffers_bytes,
                );
        }

        // Cached (Linux specific)
        if info.cached_bytes > 0 {
            builder
                .help(
                    "all_smi_memory_cached_bytes",
                    "Memory used for cache in bytes",
                )
                .type_("all_smi_memory_cached_bytes", "gauge")
                .metric(
                    "all_smi_memory_cached_bytes",
                    &base_labels,
                    info.cached_bytes,
                );
        }
    }
}

// Allow for Rust 1.85 compatibility (Launchpad)
#[allow(clippy::needless_lifetimes)]
impl<'a> MetricExporter for MemoryMetricExporter<'a> {
    fn export_metrics(&self) -> String {
        let mut builder = MetricBuilder::new();

        for (i, info) in self.memory_info.iter().enumerate() {
            self.export_basic_metrics(&mut builder, info, i);
            self.export_swap_metrics(&mut builder, info, i);
            self.export_linux_specific_metrics(&mut builder, info, i);
        }

        builder.build()
    }
}
