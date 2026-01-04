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
use crate::storage::info::StorageInfo;

/// Disk metric exporter that uses cached StorageInfo from AppState
/// This avoids expensive disk collection on every metrics request
pub struct DiskMetricExporter<'a> {
    storage_info: &'a [StorageInfo],
}

impl<'a> DiskMetricExporter<'a> {
    pub fn new(storage_info: &'a [StorageInfo]) -> Self {
        Self { storage_info }
    }

    fn export_disk_metrics(&self, builder: &mut MetricBuilder, info: &StorageInfo) {
        let labels = [
            ("instance", info.hostname.as_str()),
            ("mount_point", &info.mount_point),
            ("index", &info.index.to_string()),
        ];

        // Total disk space
        builder
            .help("all_smi_disk_total_bytes", "Total disk space in bytes")
            .type_("all_smi_disk_total_bytes", "gauge")
            .metric("all_smi_disk_total_bytes", &labels, info.total_bytes);

        // Available disk space
        builder
            .help(
                "all_smi_disk_available_bytes",
                "Available disk space in bytes",
            )
            .type_("all_smi_disk_available_bytes", "gauge")
            .metric(
                "all_smi_disk_available_bytes",
                &labels,
                info.available_bytes,
            );
    }
}

// Allow for Rust 1.85 compatibility (Launchpad)
#[allow(clippy::needless_lifetimes)]
impl<'a> MetricExporter for DiskMetricExporter<'a> {
    fn export_metrics(&self) -> String {
        let mut builder = MetricBuilder::new();

        for info in self.storage_info {
            self.export_disk_metrics(&mut builder, info);
        }

        builder.build()
    }
}
