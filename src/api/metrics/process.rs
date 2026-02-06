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
use crate::device::ProcessInfo;

pub struct ProcessMetricExporter<'a> {
    pub process_info: &'a [ProcessInfo],
}

impl<'a> ProcessMetricExporter<'a> {
    pub fn new(process_info: &'a [ProcessInfo]) -> Self {
        Self { process_info }
    }

    fn export_process_metrics(&self, builder: &mut MetricBuilder, process: &ProcessInfo) {
        let pid_str = process.pid.to_string();
        let device_id_str = process.device_id.to_string();

        let labels = [
            ("pid", pid_str.as_str()),
            ("name", process.process_name.as_str()),
            ("device_id", device_id_str.as_str()),
            ("device_uuid", process.device_uuid.as_str()),
        ];

        // Process memory usage
        builder
            .help(
                "all_smi_process_memory_used_bytes",
                "Process memory used in bytes",
            )
            .type_("all_smi_process_memory_used_bytes", "gauge")
            .metric(
                "all_smi_process_memory_used_bytes",
                &labels,
                process.used_memory,
            );
    }
}

// Allow for Rust 1.85 compatibility (Launchpad)
#[allow(clippy::needless_lifetimes)]
impl<'a> MetricExporter for ProcessMetricExporter<'a> {
    fn export_metrics(&self) -> String {
        if self.process_info.is_empty() {
            return String::new();
        }

        let mut builder = MetricBuilder::new();

        for process in self.process_info {
            self.export_process_metrics(&mut builder, process);
        }

        builder.build()
    }
}
