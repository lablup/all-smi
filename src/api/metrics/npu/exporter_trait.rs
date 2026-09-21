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

use crate::api::metrics::MetricBuilder;
use crate::device::GpuInfo;

/// Trait for NPU vendor-specific metric exporters
/// This trait defines the interface that all NPU vendor implementations must follow
pub trait NpuExporter: Send + Sync {
    /// Check if this exporter can handle the given NPU device
    fn can_handle(&self, info: &GpuInfo) -> bool;

    /// Export vendor-specific metrics for a single NPU device
    fn export_vendor_metrics(
        &self,
        builder: &mut MetricBuilder,
        info: &GpuInfo,
        index: usize,
        index_str: &str,
    );

    /// Get the vendor name for identification purposes
    fn vendor_name(&self) -> &'static str;
}
