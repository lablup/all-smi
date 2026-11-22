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

//! Tests for the label escaper and the family grouping of
//! [`MetricBuilder`](super::MetricBuilder) (issue #503).

use std::borrow::Cow;

use super::{MetricBuilder, escape_label_value, render_labels};

/// The implementation `MetricBuilder::metric` used before issue #503: four
/// `replace` passes and a filtering collect. Kept here as the reference the
/// single-pass escaper must agree with.
fn old_escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .chars()
        .filter(|c| !c.is_control())
        .collect()
}

#[test]
fn single_pass_escaper_matches_the_four_replace_implementation() {
    let atoms = [
        "",
        "a",
        "plain value",
        "\\",
        "\"",
        "\n",
        "\r",
        "\t",
        "\x1b",
        "\x1b[2J",
        "\x7f",
        "\u{85}",
        "\u{9f}",
        "\u{a0}",
        "°C",
        "한글",
        "é",
        "😀",
        "\\n",
        "\\\"",
        "C:\\Windows\\System32",
        "say \"hello\"",
        "line1\nline2\r\n",
    ];
    for a in atoms {
        assert_eq!(escape_label_value(a), old_escape(a), "input {a:?}");
        // Every pair, so an escape at the start, middle and end of a
        // longer value is covered.
        for b in atoms {
            let joined = format!("{a}{b}");
            assert_eq!(
                escape_label_value(&joined),
                old_escape(&joined),
                "input {joined:?}"
            );
            let padded = format!("x{a}y{b}z");
            assert_eq!(
                escape_label_value(&padded),
                old_escape(&padded),
                "input {padded:?}"
            );
        }
    }
}

#[test]
fn clean_input_is_returned_borrowed() {
    for clean in [
        "",
        "NVIDIA A100",
        "GPU-5f0e2c1a",
        "node-1.example.com",
        "한글 °C é",
    ] {
        assert!(
            matches!(escape_label_value(clean), Cow::Borrowed(_)),
            "{clean:?} needs no escaping and must not allocate"
        );
    }
    assert!(matches!(escape_label_value("a\"b"), Cow::Owned(_)));
}

#[test]
fn rendered_labels_match_the_per_sample_path() {
    let labels = [("gpu", "NVIDIA \"A100\""), ("path", "C:\\x"), ("n", "1")];
    let rendered = render_labels(&labels);

    let mut direct = MetricBuilder::new();
    direct.metric("m", &labels, 7);
    let mut prerendered = MetricBuilder::new();
    prerendered.metric_with_rendered_labels("m", &rendered, 7);
    assert_eq!(direct.build(), prerendered.build());

    let mut direct = MetricBuilder::new();
    direct.metric(
        "m",
        &[labels[0], labels[1], labels[2], ("engine", "3D\n")],
        1.5,
    );
    let mut prerendered = MetricBuilder::new();
    prerendered.metric_with_rendered_labels_and("m", &rendered, &[("engine", "3D\n")], 1.5);
    assert_eq!(direct.build(), prerendered.build());

    let mut no_labels = MetricBuilder::new();
    no_labels.metric_with_rendered_labels("m", "", 1);
    assert_eq!(no_labels.build(), "m 1\n");
}

#[test]
fn a_family_is_written_once_with_contiguous_samples() {
    let mut builder = MetricBuilder::new();
    // The call pattern every per-device exporter uses: help, type and one
    // sample per family, repeated for each device.
    for device in ["0", "1", "2"] {
        builder
            .help("a_total", "A")
            .type_("a_total", "counter")
            .metric("a_total", &[("d", device)], 1);
        builder
            .help("b", "B")
            .type_("b", "gauge")
            .metric("b", &[("d", device)], 2);
    }
    assert_eq!(
        builder.build(),
        "# HELP a_total A\n# TYPE a_total counter\n\
         a_total{d=\"0\"} 1\na_total{d=\"1\"} 1\na_total{d=\"2\"} 1\n\
         # HELP b B\n# TYPE b gauge\n\
         b{d=\"0\"} 2\nb{d=\"1\"} 2\nb{d=\"2\"} 2\n"
    );
}

#[test]
fn a_declared_family_without_samples_keeps_its_metadata_once() {
    // The vGPU, MIG and chassis exporters declare a family up front and may
    // then find no row with a reading. That stays valid and unchanged.
    let mut builder = MetricBuilder::new();
    for _ in 0..2 {
        builder.help("empty", "E").type_("empty", "gauge");
        builder
            .help("full", "F")
            .type_("full", "gauge")
            .metric("full", &[], 1);
    }
    assert_eq!(
        builder.build(),
        "# HELP empty E\n# TYPE empty gauge\n# HELP full F\n# TYPE full gauge\nfull 1\nfull 1\n"
    );
}

#[test]
fn histogram_samples_stay_with_their_family() {
    let mut builder = MetricBuilder::new();
    for device in ["0", "1"] {
        builder.help("lat", "L").type_("lat", "histogram");
        builder.metric("lat_bucket", &[("d", device), ("le", "+Inf")], 3);
        builder.metric("lat_sum", &[("d", device)], 1.5);
        builder.metric("lat_count", &[("d", device)], 3);
        builder.help("other", "O").type_("other", "gauge");
        builder.metric("other", &[("d", device)], 1);
    }
    let out = builder.build();
    assert_eq!(out.matches("# TYPE lat histogram").count(), 1);
    let lat_end = out.find("# HELP other").expect("other family");
    assert!(
        !out[lat_end..].contains("lat_"),
        "histogram samples must not trail another family:\n{out}"
    );
}
