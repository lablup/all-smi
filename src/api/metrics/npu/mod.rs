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

pub mod common;
pub mod exporter_trait;
pub mod furiosa;
pub mod gaudi;
pub mod google_tpu;
pub mod rebellions;
#[cfg(target_os = "linux")]
pub mod tenstorrent;

use crate::api::metrics::{MetricBuilder, MetricExporter};
use crate::device::GpuInfo;
use exporter_trait::{CommonNpuMetrics, NpuExporter};
use std::sync::OnceLock;

/// Static pool of vendor exporters to avoid repeated allocations
static EXPORTER_POOL: OnceLock<Vec<Box<dyn NpuExporter + Send + Sync>>> = OnceLock::new();

/// Main NPU metric exporter that coordinates between different vendor-specific exporters
pub struct NpuMetricExporter<'a> {
    pub npu_info: &'a [GpuInfo],
    common: common::CommonNpuExporter,
}

impl<'a> NpuMetricExporter<'a> {
    pub fn new(npu_info: &'a [GpuInfo]) -> Self {
        // Initialize the exporter pool once
        EXPORTER_POOL.get_or_init(|| {
            #[allow(unused_mut)]
            let mut exporters: Vec<Box<dyn NpuExporter + Send + Sync>> = vec![
                Box::new(gaudi::GaudiExporter::new()),
                Box::new(rebellions::RebellionsExporter::new()),
                Box::new(furiosa::FuriosaExporter::new()),
                Box::new(google_tpu::GoogleTpuExporter::new()),
            ];
            #[cfg(target_os = "linux")]
            exporters.insert(0, Box::new(tenstorrent::TenstorrentExporter::new()));
            exporters
        });

        Self {
            npu_info,
            common: common::CommonNpuExporter::new(),
        }
    }

    /// Find the appropriate exporter for a given NPU device
    /// Optimized with early pattern matching to avoid linear search
    fn find_exporter(&self, info: &GpuInfo) -> Option<&(dyn NpuExporter + Send + Sync)> {
        EXPORTER_POOL.get().and_then(|exporters| {
            // Fast path: match common vendor patterns first
            let name = &info.name;

            // Direct index access for known vendors (most common first)
            #[cfg(target_os = "linux")]
            if name.contains("Tenstorrent") {
                return Some(exporters[0].as_ref());
            }

            // Index mapping based on platform
            // Linux: [Tenstorrent, Gaudi, Rebellions, Furiosa, Google TPU]
            // Other: [Gaudi, Rebellions, Furiosa, Google TPU]
            #[cfg(target_os = "linux")]
            let (gaudi_idx, rebellions_idx, furiosa_idx, tpu_idx) = (1, 2, 3, 4);
            #[cfg(not(target_os = "linux"))]
            let (gaudi_idx, rebellions_idx, furiosa_idx, tpu_idx) = (0, 1, 2, 3);

            if name.contains("Gaudi") || name.contains("HL-") {
                return Some(exporters[gaudi_idx].as_ref());
            } else if name.contains("Rebellions") {
                return Some(exporters[rebellions_idx].as_ref());
            } else if name.contains("Furiosa") || name.contains("RNGD") || name.contains("Warboy") {
                return Some(exporters[furiosa_idx].as_ref());
            } else if name.contains("TPU") || name.contains("Google") {
                return Some(exporters[tpu_idx].as_ref());
            }

            // Fallback to dynamic check for unknown patterns
            exporters
                .iter()
                .find(|exporter| exporter.can_handle(info))
                .map(|b| b.as_ref())
        })
    }

    /// Export generic NPU metrics that are common across all vendors
    #[allow(dead_code)]
    fn export_generic_npu_metrics(
        &self,
        builder: &mut MetricBuilder,
        info: &GpuInfo,
        index: usize,
    ) {
        // Device type check removed - caller already filters NPU devices
        // Always export common metrics first
        self.common.export_generic_npu_metrics(builder, info, index);
    }

    /// Export vendor-specific metrics using the appropriate exporter
    fn export_vendor_metrics(
        &self,
        builder: &mut MetricBuilder,
        info: &GpuInfo,
        index: usize,
        index_str: &str,
    ) {
        if let Some(exporter) = self.find_exporter(info) {
            exporter.export_vendor_metrics(builder, info, index, index_str);
        }
    }

    /// Export all NPU metrics for a single device
    fn export_device_metrics(&self, builder: &mut MetricBuilder, info: &GpuInfo, index: usize) {
        // Pre-allocate index string once per device
        let index_str = index.to_string();

        // Export generic metrics first
        self.export_generic_npu_metrics_with_str(builder, info, &index_str);

        // Then export vendor-specific metrics
        self.export_vendor_metrics(builder, info, index, &index_str);
    }

    /// Export generic NPU metrics with pre-allocated index string
    fn export_generic_npu_metrics_with_str(
        &self,
        builder: &mut MetricBuilder,
        info: &GpuInfo,
        index_str: &str,
    ) {
        // Device type check removed - caller already filters NPU devices
        // Always export common metrics first
        self.common
            .export_generic_npu_metrics_str(builder, info, index_str);
    }
}

// Allow for Rust 1.85 compatibility (Launchpad)
#[allow(clippy::needless_lifetimes)]
impl<'a> MetricExporter for NpuMetricExporter<'a> {
    fn export_metrics(&self) -> String {
        let mut builder = MetricBuilder::new();

        // Filter NPU devices and export metrics
        for (i, info) in self.npu_info.iter().enumerate() {
            // Only process NPU or TPU devices
            if info.device_type == "NPU" || info.device_type == "TPU" {
                self.export_device_metrics(&mut builder, info, i);
            }
        }

        builder.build()
    }
}
