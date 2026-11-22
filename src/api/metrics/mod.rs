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

use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt::Write as _;

pub mod chassis;
pub mod cpu;
pub mod disk;
pub mod energy;
/// Baseline `all_smi_up` / `all_smi_build_info` families (issue #324).
pub mod exporter_status;
pub mod gpu;
pub mod hardware;
pub mod memory;
pub mod mig;
pub mod npu;
pub mod process;
pub mod render;
pub mod runtime;
pub mod vgpu;

/// Trait for exporting metrics in Prometheus format
pub trait MetricExporter {
    /// Write this exporter's samples into a shared [`MetricBuilder`].
    ///
    /// The whole exposition is rendered through one builder so a family that
    /// two exporters both emit is still written once, with one `# HELP`, one
    /// `# TYPE`, and contiguous samples (issue #503).
    fn export_into(&self, builder: &mut MetricBuilder);

    /// Export metrics to Prometheus format string.
    ///
    /// Standalone form for one exporter. The live render path goes through
    /// [`MetricExporter::export_into`], so the binary target itself never
    /// calls this; library callers and the tests do.
    #[allow(dead_code)]
    fn export_metrics(&self) -> String {
        let mut builder = MetricBuilder::new();
        self.export_into(&mut builder);
        builder.build()
    }
}

/// Escape a label value for the Prometheus text exposition format.
///
/// Backslash, double-quote, and newline must be escaped per the spec. Carriage
/// returns are escaped too so lines produced on Windows-origin inputs cannot
/// break the `\n`-delimited format downstream, and every other control
/// character is dropped to defend against local NVML returning strings that
/// could inject ANSI escape sequences.
///
/// Single pass: a value with nothing to escape is returned borrowed, so the
/// common case (device names, UUIDs, hostnames) allocates nothing.
pub fn escape_label_value(value: &str) -> Cow<'_, str> {
    let Some(first) = value
        .char_indices()
        .find(|(_, c)| needs_escape(*c))
        .map(|(i, _)| i)
    else {
        return Cow::Borrowed(value);
    };

    let mut escaped = String::with_capacity(value.len() + 8);
    escaped.push_str(&value[..first]);
    for c in value[first..].chars() {
        match c {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            c if c.is_control() => {}
            c => escaped.push(c),
        }
    }
    Cow::Owned(escaped)
}

fn needs_escape(c: char) -> bool {
    c == '\\' || c == '"' || c.is_control()
}

/// Append `key="escaped value"` pairs, comma separated, without braces.
fn write_label_pairs(out: &mut String, labels: &[(&str, &str)]) {
    for (i, (key, value)) in labels.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str(key);
        out.push_str("=\"");
        out.push_str(&escape_label_value(value));
        out.push('"');
    }
}

/// Render a label set once so it can be reused for every family of a row.
///
/// The result is the content between the braces (`a="1", b="2"`), which is
/// what [`MetricBuilder::metric_with_rendered_labels`] expects. Escaping a
/// row's labels once instead of once per family is the point: a GPU row
/// feeds dozens of families with the same four labels.
pub fn render_labels(labels: &[(&str, &str)]) -> String {
    let mut out = String::new();
    write_label_pairs(&mut out, labels);
    out
}

/// One metric family: its metadata and every sample line gathered so far.
struct Family {
    name: String,
    help: Option<String>,
    metric_type: Option<String>,
    samples: String,
}

/// Helper struct to build Prometheus metrics.
///
/// The builder groups output by metric family. `help`, `type_` and `metric`
/// can be called in any order and any number of times (the exporters call
/// them once per device), and [`MetricBuilder::build`] writes each family
/// once: one `# HELP`, one `# TYPE`, then all of its samples, in the order
/// the family was first seen. The text format requires exactly that, and
/// strict parsers (`promtool check metrics`, OpenMetrics) reject a body that
/// repeats the metadata per device or interleaves families (issue #503).
///
/// A family that was declared but never received a sample keeps its
/// metadata lines, which the format allows. Exporters that follow the
/// "absence means no data" convention (issue #325) only declare a family
/// when a row has a reading, so those still emit nothing at all.
pub struct MetricBuilder {
    comments: String,
    families: Vec<Family>,
    index: HashMap<String, usize>,
    /// Family touched by the previous call. Exporters call `help`, `type_`
    /// and `metric` for the same name back to back, so this skips the map
    /// lookup most of the time.
    last: usize,
}

impl MetricBuilder {
    pub fn new() -> Self {
        Self {
            comments: String::new(),
            families: Vec::new(),
            index: HashMap::new(),
            last: 0,
        }
    }

    /// Index of the family called `name`, creating it when it is new.
    fn family(&mut self, name: &str) -> usize {
        if self.families.get(self.last).is_some_and(|f| f.name == name) {
            return self.last;
        }
        let idx = match self.index.get(name) {
            Some(idx) => *idx,
            None => {
                let idx = self.families.len();
                self.families.push(Family {
                    name: name.to_string(),
                    help: None,
                    metric_type: None,
                    samples: String::new(),
                });
                self.index.insert(name.to_string(), idx);
                idx
            }
        };
        self.last = idx;
        idx
    }

    /// Index of the family a sample called `name` belongs to.
    ///
    /// Histogram and summary samples carry a `_bucket`, `_sum` or `_count`
    /// suffix on top of the family name, so those resolve to the declared
    /// histogram or summary family when there is one.
    fn sample_family(&mut self, name: &str) -> usize {
        if self.families.get(self.last).is_some_and(|f| f.name == name) {
            return self.last;
        }
        if !self.index.contains_key(name) {
            for suffix in ["_bucket", "_sum", "_count"] {
                let Some(base) = name.strip_suffix(suffix) else {
                    continue;
                };
                if let Some(idx) = self.index.get(base).copied()
                    && matches!(
                        self.families[idx].metric_type.as_deref(),
                        Some("histogram" | "summary")
                    )
                {
                    self.last = idx;
                    return idx;
                }
            }
        }
        self.family(name)
    }

    /// Add a comment line. Comments are written before every family.
    #[allow(dead_code)]
    pub fn comment(&mut self, text: &str) -> &mut Self {
        self.comments.push_str("# ");
        self.comments.push_str(text);
        self.comments.push('\n');
        self
    }

    /// Declare a family's HELP text. The first description for a name wins.
    pub fn help(&mut self, name: &str, description: &str) -> &mut Self {
        let idx = self.family(name);
        let family = &mut self.families[idx];
        if family.help.is_none() {
            family.help = Some(description.to_string());
        }
        self
    }

    /// Declare a family's TYPE. The first type for a name wins.
    pub fn type_(&mut self, name: &str, metric_type: &str) -> &mut Self {
        let idx = self.family(name);
        let family = &mut self.families[idx];
        if family.metric_type.is_none() {
            family.metric_type = Some(metric_type.to_string());
        }
        self
    }

    /// Add a metric line with labels
    pub fn metric(
        &mut self,
        name: &str,
        labels: &[(&str, &str)],
        value: impl std::fmt::Display,
    ) -> &mut Self {
        let idx = self.sample_family(name);
        let out = &mut self.families[idx].samples;
        out.push_str(name);
        if !labels.is_empty() {
            out.push('{');
            write_label_pairs(out, labels);
            out.push('}');
        }
        // Writing to a `String` cannot fail.
        let _ = writeln!(out, " {value}");
        self
    }

    /// Add a metric line whose label block was rendered by [`render_labels`].
    pub fn metric_with_rendered_labels(
        &mut self,
        name: &str,
        rendered_labels: &str,
        value: impl std::fmt::Display,
    ) -> &mut Self {
        self.metric_with_rendered_labels_and(name, rendered_labels, &[], value)
    }

    /// Like [`MetricBuilder::metric_with_rendered_labels`], with extra labels
    /// appended after the pre-rendered block (a per-engine or per-domain
    /// label on top of a row's shared identity labels).
    pub fn metric_with_rendered_labels_and(
        &mut self,
        name: &str,
        rendered_labels: &str,
        extra_labels: &[(&str, &str)],
        value: impl std::fmt::Display,
    ) -> &mut Self {
        let idx = self.sample_family(name);
        let out = &mut self.families[idx].samples;
        out.push_str(name);
        if !rendered_labels.is_empty() || !extra_labels.is_empty() {
            out.push('{');
            out.push_str(rendered_labels);
            if !rendered_labels.is_empty() && !extra_labels.is_empty() {
                out.push_str(", ");
            }
            write_label_pairs(out, extra_labels);
            out.push('}');
        }
        // Writing to a `String` cannot fail.
        let _ = writeln!(out, " {value}");
        self
    }

    /// Build the final metric string, one contiguous block per family.
    pub fn build(self) -> String {
        let capacity = self.comments.len()
            + self
                .families
                .iter()
                .map(|f| f.samples.len() + 2 * f.name.len() + 64)
                .sum::<usize>();
        let mut out = String::with_capacity(capacity);
        out.push_str(&self.comments);
        for family in &self.families {
            if let Some(help) = &family.help {
                out.push_str("# HELP ");
                out.push_str(&family.name);
                out.push(' ');
                out.push_str(help);
                out.push('\n');
            }
            if let Some(metric_type) = &family.metric_type {
                out.push_str("# TYPE ");
                out.push_str(&family.name);
                out.push(' ');
                out.push_str(metric_type);
                out.push('\n');
            }
            out.push_str(&family.samples);
        }
        out
    }
}

impl Default for MetricBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[path = "builder_tests.rs"]
mod builder_tests;

#[cfg(test)]
#[path = "exposition_validity_tests.rs"]
mod exposition_validity_tests;

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
        assert!(!output.contains('\x1b'), "control char leaked: {output}");
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
