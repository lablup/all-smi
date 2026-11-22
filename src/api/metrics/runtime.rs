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

use crate::utils::RuntimeEnvironment;

use super::{MetricBuilder, MetricExporter};

pub struct RuntimeMetricExporter<'a> {
    runtime_env: &'a RuntimeEnvironment,
    hostname: String,
}

impl<'a> RuntimeMetricExporter<'a> {
    pub fn new(runtime_env: &'a RuntimeEnvironment) -> Self {
        Self {
            runtime_env,
            hostname: crate::utils::get_hostname(),
        }
    }
}

impl<'a> MetricExporter for RuntimeMetricExporter<'a> {
    fn export_into(&self, builder: &mut MetricBuilder) {
        // The label blocks below are written verbatim (comma separated with
        // no space) to keep these families byte-identical to what this
        // exporter has always produced.

        // Container environment metrics
        if self.runtime_env.container.is_containerized() {
            let runtime_name = self.runtime_env.container.runtime.as_str();

            // Container runtime info metric
            let labels = format!(
                "hostname=\"{}\",runtime=\"{runtime_name}\",container_id=\"{}\"",
                self.hostname,
                self.runtime_env
                    .container
                    .container_id
                    .as_deref()
                    .unwrap_or("unknown")
            );
            builder
                .help(
                    "all_smi_container_runtime_info",
                    "Container runtime environment information",
                )
                .type_("all_smi_container_runtime_info", "gauge")
                .metric_with_rendered_labels("all_smi_container_runtime_info", &labels, 1);

            // Additional Kubernetes-specific metrics
            if let crate::utils::ContainerRuntime::Kubernetes = self.runtime_env.container.runtime
                && let Some(pod_name) = &self.runtime_env.container.pod_name
            {
                let labels = format!(
                    "hostname=\"{}\",pod_name=\"{pod_name}\",namespace=\"{}\"",
                    self.hostname,
                    self.runtime_env
                        .container
                        .namespace
                        .as_deref()
                        .unwrap_or("default")
                );
                builder
                    .help("all_smi_kubernetes_pod_info", "Kubernetes pod information")
                    .type_("all_smi_kubernetes_pod_info", "gauge")
                    .metric_with_rendered_labels("all_smi_kubernetes_pod_info", &labels, 1);
            }
        }

        // Virtualization environment metrics
        if self.runtime_env.virtualization.is_virtual {
            let vm_type = self.runtime_env.virtualization.vm_type.as_str();

            let labels = format!(
                "hostname=\"{}\",vm_type=\"{vm_type}\",hypervisor=\"{}\"",
                self.hostname,
                self.runtime_env
                    .virtualization
                    .hypervisor
                    .as_deref()
                    .unwrap_or(vm_type)
            );
            builder
                .help(
                    "all_smi_virtualization_info",
                    "Virtualization environment information",
                )
                .type_("all_smi_virtualization_info", "gauge")
                .metric_with_rendered_labels("all_smi_virtualization_info", &labels, 1);
        }

        // Combined runtime environment metric (what would be displayed in UI)
        if let Some((name, _color)) = self.runtime_env.display_info() {
            let labels = format!("hostname=\"{}\",environment=\"{name}\"", self.hostname);
            builder
                .help(
                    "all_smi_runtime_environment",
                    "Current runtime environment (container or VM)",
                )
                .type_("all_smi_runtime_environment", "gauge")
                .metric_with_rendered_labels("all_smi_runtime_environment", &labels, 1);
        }
    }
}
