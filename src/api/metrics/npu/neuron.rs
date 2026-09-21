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

//! AWS Neuron (Trainium / Inferentia) Prometheus exporter.
//!
//! Emits the `all_smi_neuron_*` family. Utilization, memory, power and
//! temperature for these rows are published by the GPU exporter under the
//! `all_smi_gpu_*` names, because `render_prometheus_exposition` runs the
//! GPU and NPU exporters over the same rows; only Neuron-specific identity
//! and topology live here.
//!
//! Every series is conditional on the corresponding `detail` key being
//! present. A value the reader could not source is absent from `detail`
//! and therefore absent from the exposition — it is never exported as 0.

use super::common::CommonNpuExporter;
use super::exporter_trait::NpuExporter;
use crate::api::metrics::MetricBuilder;
use crate::device::GpuInfo;

pub struct NeuronExporter;

pub(crate) fn is_neuron_device(info: &GpuInfo) -> bool {
    if info
        .detail
        .get("lib_name")
        .is_some_and(|name| name.eq_ignore_ascii_case("Neuron"))
    {
        return true;
    }

    info.name
        .split(|character: char| !character.is_ascii_alphanumeric())
        .any(|token| {
            let token = token.to_ascii_uppercase();
            token == "NEURON" || token.starts_with("TRAINIUM") || token.starts_with("INFERENTIA")
        })
}

impl NeuronExporter {
    pub fn new() -> Self {
        Self
    }

    /// Base label set shared by every `all_smi_neuron_*` series.
    fn base_labels<'a>(info: &'a GpuInfo, index_str: &'a str) -> [(&'a str, &'a str); 4] {
        [
            ("npu", info.name.as_str()),
            ("instance", info.instance.as_str()),
            ("npu_uuid", info.uuid.as_str()),
            ("npu_index", index_str),
        ]
    }

    /// Device identity: PCI BDF, hardware serial, architecture, and the
    /// instance type the driver reports.
    fn export_identity_metrics(
        &self,
        builder: &mut MetricBuilder,
        info: &GpuInfo,
        index_str: &str,
    ) {
        const KEYS: &[(&str, &str, &str)] = &[
            ("pci_bdf", "bdf", "all_smi_neuron_device_info"),
            (
                "serial_number",
                "serial_number",
                "all_smi_neuron_serial_info",
            ),
            ("architecture", "architecture", "all_smi_neuron_arch_info"),
            (
                "instance_type",
                "instance_type",
                "all_smi_neuron_instance_info",
            ),
        ];

        for (detail_key, label_name, metric) in KEYS {
            let Some(value) = info.detail.get(*detail_key) else {
                continue;
            };
            let base = Self::base_labels(info, index_str);
            let labels = [
                base[0],
                base[1],
                base[2],
                base[3],
                (*label_name, value.as_str()),
            ];
            builder
                .help(metric, "AWS Neuron device identity")
                .type_(metric, "gauge")
                .metric(metric, &labels, 1);
        }
    }

    /// Driver version, reported by the `neuron` kernel module.
    fn export_driver_metrics(&self, builder: &mut MetricBuilder, info: &GpuInfo, index_str: &str) {
        let Some(version) = info.detail.get("driver_version") else {
            return;
        };
        let base = Self::base_labels(info, index_str);
        let labels = [
            base[0],
            base[1],
            base[2],
            base[3],
            ("version", version.as_str()),
        ];
        builder
            .help(
                "all_smi_neuron_driver_info",
                "AWS Neuron kernel driver version",
            )
            .type_("all_smi_neuron_driver_info", "gauge")
            .metric("all_smi_neuron_driver_info", &labels, 1);
    }

    /// NeuronCore topology: which device this row belongs to, its global
    /// flat NeuronCore index, and the device's core count.
    fn export_topology_metrics(
        &self,
        builder: &mut MetricBuilder,
        info: &GpuInfo,
        index_str: &str,
    ) {
        const GAUGES: &[(&str, &str, &str)] = &[
            (
                "neuron_device_index",
                "all_smi_neuron_device_index",
                "Driver enumeration index of the Neuron device",
            ),
            (
                "neuroncore_index",
                "all_smi_neuron_core_index",
                "Global flat NeuronCore index",
            ),
            (
                "neuroncore_count",
                "all_smi_neuron_core_count",
                "NeuronCores on the parent Neuron device",
            ),
            (
                "device_memory_bytes",
                "all_smi_neuron_device_memory_bytes",
                "HBM on the parent Neuron device in bytes",
            ),
        ];

        let labels = Self::base_labels(info, index_str);
        for (detail_key, metric, help) in GAUGES {
            if let Some(raw) = info.detail.get(*detail_key)
                && let Some(value) = CommonNpuExporter::parse_numeric_value(raw)
            {
                builder
                    .help(metric, help)
                    .type_(metric, "gauge")
                    .metric(metric, &labels, value);
            }
        }
    }
}

impl Default for NeuronExporter {
    fn default() -> Self {
        Self::new()
    }
}

impl NpuExporter for NeuronExporter {
    fn can_handle(&self, info: &GpuInfo) -> bool {
        is_neuron_device(info)
    }

    fn export_vendor_metrics(
        &self,
        builder: &mut MetricBuilder,
        info: &GpuInfo,
        _index: usize,
        index_str: &str,
    ) {
        if !self.can_handle(info) {
            return;
        }
        self.export_identity_metrics(builder, info, index_str);
        self.export_driver_metrics(builder, info, index_str);
        self.export_topology_metrics(builder, info, index_str);
    }
}
