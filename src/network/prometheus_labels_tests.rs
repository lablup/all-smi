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

use std::collections::HashMap;

use super::*;
use crate::parsing::prometheus::split_prometheus_line;

/// The `HashMap` label parser this module replaced, kept verbatim as the
/// reference for equivalence tests.
pub(crate) fn legacy_parse_labels(labels_str: &str) -> HashMap<String, String> {
    if labels_str.len() > MAX_INPUT_LENGTH {
        return HashMap::new();
    }
    let mut labels = HashMap::new();
    let mut label_count = 0;
    for label in legacy_split(labels_str) {
        if label_count >= MAX_LABELS {
            break;
        }
        if let Some(eq_pos) = label.find('=') {
            let key_clean = sanitize_label_value(&label[..eq_pos]);
            let value_clean = unescape_label_value_owned(&label[eq_pos + 1..]);
            if key_clean.len() <= MAX_LABEL_LENGTH && value_clean.len() <= MAX_LABEL_LENGTH {
                labels.insert(key_clean, value_clean);
                label_count += 1;
            }
        }
    }
    labels
}

fn legacy_split(labels_str: &str) -> Vec<&str> {
    let bytes = labels_str.as_bytes();
    let mut out = Vec::new();
    let (mut start, mut i, mut in_quotes) = (0usize, 0usize, false);
    while i < bytes.len() {
        let b = bytes[i];
        if in_quotes {
            if b == b'\\' && i + 1 < bytes.len() {
                i += 2;
                continue;
            }
            if b == b'"' {
                in_quotes = false;
            }
            i += 1;
        } else {
            match b {
                b'"' => in_quotes = true,
                b',' => {
                    out.push(&labels_str[start..i]);
                    start = i + 1;
                }
                _ => {}
            }
            i += 1;
        }
    }
    out.push(&labels_str[start..]);
    out
}

fn parsed(labels_str: &str) -> HashMap<String, String> {
    let mut labels = Labels::new();
    labels.parse_into(labels_str);
    labels.to_map()
}

const EDGE_CASES: &[&str] = &[
    r#"gpu="RTX", uuid="GPU-1""#,
    r#"a="1",a="2""#,
    r#"  key  =  "  spaced  " "#,
    r#"x="has \"quote\" and \\ slash""#,
    r#"x="line\nbreak\rret""#,
    "x=\"bell\u{7}\u{1b}[31m\"",
    r#"x=unquoted, y="half"#,
    r#"x=""#,
    r#"x="""#,
    r#""q"="v""#,
    r#"noeq, k="v""#,
    r#"a="x,y",b="p\",q",c=plain"#,
    r#"x="unknown \t escape""#,
    r#"x="trailing \"#,
    "",
    ",,,",
    r#"ключ="значение""#,
];

#[test]
fn split_iterator_matches_legacy_vec() {
    for case in EDGE_CASES {
        let ours: Vec<&str> = QuoteAwareSplit::new(case).collect();
        assert_eq!(ours, legacy_split(case), "{case:?}");
    }
}

#[test]
fn labels_match_legacy_parser_on_edge_cases() {
    for case in EDGE_CASES {
        assert_eq!(parsed(case), legacy_parse_labels(case), "{case:?}");
    }
    let long_value = format!(r#"k="{}""#, "v".repeat(MAX_LABEL_LENGTH + 10));
    assert_eq!(parsed(&long_value), legacy_parse_labels(&long_value));
    let many: String = (0..MAX_LABELS + 20)
        .map(|i| format!(r#"k{i}="{i}""#))
        .collect::<Vec<_>>()
        .join(",");
    assert_eq!(parsed(&many).len(), MAX_LABELS);
    assert_eq!(parsed(&many), legacy_parse_labels(&many));
    let huge = format!(r#"k="{}""#, "v".repeat(MAX_INPUT_LENGTH));
    assert!(parsed(&huge).is_empty());
}

#[test]
fn labels_match_legacy_parser_on_fixtures() {
    for (name, text) in crate::parsing::prometheus_fixtures::ALL {
        for line in text.lines() {
            if let Some((_, labels_str, _)) = split_prometheus_line(line) {
                assert_eq!(
                    parsed(labels_str),
                    legacy_parse_labels(labels_str),
                    "{name}: {line}"
                );
            }
        }
    }
}

#[test]
fn plain_labels_borrow_from_the_input() {
    let mut labels = Labels::new();
    labels.parse_into(r#"gpu="RTX 4090", gpu_uuid="GPU-1", gpu_index="0""#);
    assert!(
        labels
            .0
            .iter()
            .all(|(k, v)| matches!(k, Cow::Borrowed(_)) && matches!(v, Cow::Borrowed(_)))
    );
    assert_eq!(labels.get("gpu"), Some("RTX 4090"));
    assert_eq!(labels.get("missing"), None);

    labels.parse_into(r#"x="a\"b""#);
    assert!(matches!(labels.0[0].1, Cow::Owned(_)));
    assert_eq!(labels.get("x"), Some("a\"b"));
}

#[test]
fn duplicate_key_returns_last_value() {
    let mut labels = Labels::new();
    labels.parse_into(r#"a="1",b="2",a="3""#);
    assert_eq!(labels.get("a"), Some("3"));
}
