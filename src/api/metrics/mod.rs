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
pub mod hardware;
pub mod memory;
pub mod mig;
pub mod npu;
pub mod process;
pub mod runtime;
pub mod vgpu;

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
                // backslash, double-quote, and newline must be escaped.
                // We also escape carriage returns so lines produced on
                // Windows-origin inputs cannot break the `\n`-delimited
                // exposition format downstream.
                // Finally, strip any remaining control characters to
                // defend against local NVML returning unexpected strings
                // that could inject ANSI escape sequences.
                let escaped_value: String = value
                    .replace('\\', "\\\\")
                    .replace('"', "\\\"")
                    .replace('\n', "\\n")
                    .replace('\r', "\\r")
                    .chars()
                    .filter(|c| !c.is_control())
                    .collect();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metric_builder_label_escaping_backslash() {
        let mut builder = MetricBuilder::new();
        builder.metric("test_metric", &[("path", "C:\\Windows\\System32")], "1");
        let output = builder.build();
        assert!(output.contains(r#"path="C:\\Windows\\System32""#));
    }

    #[test]
    fn test_metric_builder_label_escaping_double_quote() {
        let mut builder = MetricBuilder::new();
        builder.metric("test_metric", &[("label", r#"say "hello""#)], "1");
        let output = builder.build();
        assert!(output.contains(r#"label="say \"hello\"""#));
    }

    #[test]
    fn test_metric_builder_label_escaping_newline() {
        let mut builder = MetricBuilder::new();
        builder.metric("test_metric", &[("value", "line1\nline2")], "1");
        let output = builder.build();
        assert!(output.contains(r#"value="line1\nline2""#));
    }

    #[test]
    fn test_metric_builder_label_escaping_carriage_return() {
        let mut builder = MetricBuilder::new();
        builder.metric("test_metric", &[("value", "line1\rline2")], "1");
        let output = builder.build();
        assert!(output.contains(r#"value="line1\rline2""#));
    }

    #[test]
    fn test_metric_builder_label_strips_control_characters() {
        let mut builder = MetricBuilder::new();
        builder.metric("test_metric", &[("gpu", "NVIDIA\x1b[2JEvil")], "1");
        let output = builder.build();
        // The ESC control character is stripped; only the printable part of
        // the escape sequence remains.
        assert!(
            !output.contains('\x1b'),
            "control char leaked: {output}"
        );
        assert!(output.contains("NVIDIA[2JEvil"));
    }

    #[test]
    fn test_metric_builder_no_labels() {
        let mut builder = MetricBuilder::new();
        builder.metric("test_metric", &[], "42.5");
        let output = builder.build();
        assert_eq!(output, "test_metric 42.5\n");
    }

    #[test]
    fn test_metric_builder_help_and_type() {
        let mut builder = MetricBuilder::new();
        builder
            .help("my_metric", "A test metric")
            .type_("my_metric", "gauge")
            .metric("my_metric", &[("host", "server1")], "99");
        let output = builder.build();
        assert!(output.contains("# HELP my_metric A test metric\n"));
        assert!(output.contains("# TYPE my_metric gauge\n"));
        assert!(output.contains("my_metric{host=\"server1\"} 99\n"));
    }
}
