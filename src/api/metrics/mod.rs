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

pub mod chassis;
pub mod cpu;
pub mod disk;
pub mod gpu;
pub mod memory;
pub mod npu;
pub mod process;
pub mod runtime;

/// Trait for exporting metrics in Prometheus format
pub trait MetricExporter {
    /// Export metrics to Prometheus format string
    fn export_metrics(&self) -> String;
}

/// Helper struct to build Prometheus metrics
pub struct MetricBuilder {
    metrics: String,
}

impl MetricBuilder {
    pub fn new() -> Self {
        Self {
            metrics: String::new(),
        }
    }

    /// Add a comment line
    #[allow(dead_code)]
    pub fn comment(&mut self, text: &str) -> &mut Self {
        self.metrics.push_str("# ");
        self.metrics.push_str(text);
        self.metrics.push('\n');
        self
    }

    /// Add a HELP line
    pub fn help(&mut self, name: &str, description: &str) -> &mut Self {
        self.metrics
            .push_str(&format!("# HELP {name} {description}\n"));
        self
    }

    /// Add a TYPE line
    pub fn type_(&mut self, name: &str, metric_type: &str) -> &mut Self {
        self.metrics
            .push_str(&format!("# TYPE {name} {metric_type}\n"));
        self
    }

    /// Add a metric line with labels
    pub fn metric(
        &mut self,
        name: &str,
        labels: &[(&str, &str)],
        value: impl ToString,
    ) -> &mut Self {
        self.metrics.push_str(name);

        if !labels.is_empty() {
            self.metrics.push('{');
            for (i, (key, value)) in labels.iter().enumerate() {
                if i > 0 {
                    self.metrics.push_str(", ");
                }
                // Escape per Prometheus exposition format spec:
                // backslash, double-quote, and newline must be escaped
                let escaped_value = value
                    .replace('\\', "\\\\")
                    .replace('"', "\\\"")
                    .replace('\n', "\\n");
                self.metrics.push_str(&format!("{key}=\"{escaped_value}\""));
            }
            self.metrics.push('}');
        }

        self.metrics.push(' ');
        self.metrics.push_str(&value.to_string());
        self.metrics.push('\n');
        self
    }

    /// Build the final metric string
    pub fn build(self) -> String {
        self.metrics
    }
}

impl Default for MetricBuilder {
    fn default() -> Self {
        Self::new()
    }
}
