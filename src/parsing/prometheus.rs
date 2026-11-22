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

//! Allocation-free splitter for `all_smi_*` Prometheus exposition lines.
//!
//! Replaces the regex `^all_smi_([^\{]+)\{([^}]+)\} ([\d\.]+)$` in the remote
//! parser's hot path (issue #500). It must stay free of `cli`-gated
//! dependencies: `parsing` is also built by `--no-default-features`.

/// Split one exposition line into `(metric name without the all_smi_
/// prefix, label string inside the braces, value)`.
///
/// Accepts exactly the lines the regex
/// `^all_smi_([^\{]+)\{([^}]+)\} ([\d\.]+)$` accepts on the trimmed line and
/// whose value parses as `f64`:
///
/// - the line starts with `all_smi_`;
/// - the name is non-empty and runs up to the first `{`;
/// - the label string is non-empty and runs up to the first `}` after it;
/// - exactly one space follows the `}`;
/// - the value is one or more ASCII digits and dots and parses as `f64`
///   (so `1.2.3` and `..` are rejected rather than read as zero).
///
/// No length caps are applied here; callers enforce their own bounds.
///
/// ```
/// use all_smi::parsing::prometheus::split_prometheus_line;
///
/// let line = r#"all_smi_gpu_utilization{gpu="RTX", gpu_uuid="GPU-1"} 25.5"#;
/// assert_eq!(
///     split_prometheus_line(line),
///     Some(("gpu_utilization", r#"gpu="RTX", gpu_uuid="GPU-1""#, 25.5))
/// );
/// assert_eq!(split_prometheus_line("all_smi_gpu_utilization 25.5"), None);
/// ```
pub fn split_prometheus_line(line: &str) -> Option<(&str, &str, f64)> {
    let rest = line.trim().strip_prefix("all_smi_")?;

    let open = rest.find('{')?;
    if open == 0 {
        return None;
    }
    let name = &rest[..open];

    let after_open = &rest[open + 1..];
    let close = after_open.find('}')?;
    if close == 0 {
        return None;
    }
    let labels = &after_open[..close];

    let value_text = after_open[close + 1..].strip_prefix(' ')?;
    if value_text.is_empty()
        || !value_text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'.')
    {
        return None;
    }
    let value = value_text.parse::<f64>().ok()?;
    Some((name, labels, value))
}

#[cfg(test)]
mod tests {
    use regex::Regex;

    use super::*;

    const LEGACY_PATTERN: &str = r"^all_smi_([^\{]+)\{([^}]+)\} ([\d\.]+)$";

    /// What the previous regex path produced for one line, before any caps.
    fn legacy_split<'a>(re: &Regex, line: &'a str) -> Option<(&'a str, &'a str, f64)> {
        let trimmed = line.trim();
        let caps = re.captures(trimmed)?;
        let value = caps.get(3)?.as_str().parse::<f64>().ok()?;
        Some((caps.get(1)?.as_str(), caps.get(2)?.as_str(), value))
    }

    fn assert_equivalent(re: &Regex, line: &str) {
        let legacy = legacy_split(re, line);
        let ours = split_prometheus_line(line);
        match (legacy, ours) {
            (None, None) => {}
            (Some(a), Some(b)) => {
                assert_eq!((a.0, a.1), (b.0, b.1), "fields differ for {line:?}");
                assert!(
                    a.2 == b.2 || (a.2.is_nan() && b.2.is_nan()),
                    "value differs for {line:?}"
                );
            }
            _ => panic!("accept/reject differs for {line:?}: legacy {legacy:?}, ours {ours:?}"),
        }
    }

    #[test]
    fn matches_regex_on_edge_cases() {
        let re = Regex::new(LEGACY_PATTERN).unwrap();
        for line in [
            r#"all_smi_gpu_utilization{gpu="RTX", uuid="GPU-1"} 25.5"#,
            r#"all_smi_gpu_utilization{gpu="RTX"} 0"#,
            r#"all_smi_gpu_utilization{gpu="RTX"} 1.2.3"#,
            r#"all_smi_gpu_utilization{gpu="RTX"} .."#,
            r#"all_smi_gpu_utilization{gpu="RTX"} ."#,
            r#"all_smi_gpu_utilization{gpu="RTX"} 1."#,
            r#"all_smi_gpu_utilization{gpu="RTX"} .5"#,
            r#"all_smi_gpu_utilization{gpu="RTX"} 1e5"#,
            r#"all_smi_gpu_utilization{gpu="RTX"} -1"#,
            r#"all_smi_gpu_utilization{gpu="RTX"}  25"#,
            r#"all_smi_gpu_utilization{gpu="RTX"}25"#,
            r#"all_smi_gpu_utilization{gpu="RTX"} 25 "#,
            "all_smi_gpu_utilization{gpu=\"RTX\"} 25\r",
            "  all_smi_gpu_utilization{gpu=\"RTX\"} 25\t",
            r#"all_smi_gpu_utilization{gpu="a}b"} 25"#,
            r#"all_smi_gpu_utilization{gpu="a{b"} 25"#,
            r#"all_smi_gpu_utilization{} 25"#,
            r#"all_smi_{gpu="RTX"} 25"#,
            "all_smi_gpu_utilization 25",
            r#"all_smi_gpu utilization{gpu="RTX"} 25"#,
            r#"all_smi_gpu}x{gpu="RTX"} 25"#,
            r#"all_smi_gpu_utilization{gpu="RTX"} 25 # comment"#,
            r#"all_smi_gpu_utilization{gpu="RTX"} "#,
            r#"all_smi_gpu_utilization{gpu="RTX"}"#,
            r#"all_smi_gpu_utilization{gpu="RTX"} ١٢"#,
            r#"other_metric{gpu="RTX"} 1"#,
            "# HELP all_smi_gpu_utilization GPU utilization",
            "# TYPE all_smi_gpu_utilization gauge",
            "",
            "all_smi_",
            r#"all_smi_x{y} 00012.5000"#,
            r#"all_smi_x{y} 99999999999999999999999999999999"#,
        ] {
            assert_equivalent(&re, line);
        }
    }

    #[test]
    fn matches_regex_on_mock_and_exporter_fixtures() {
        let re = Regex::new(LEGACY_PATTERN).unwrap();
        let mut accepted = 0usize;
        for (name, text) in crate::parsing::prometheus_fixtures::ALL {
            for line in text.lines() {
                assert_equivalent(&re, line);
                accepted += usize::from(split_prometheus_line(line).is_some());
            }
            assert!(
                text.lines().any(|l| split_prometheus_line(l).is_some()),
                "fixture {name} has no metric lines"
            );
        }
        assert!(accepted > 1000, "only {accepted} lines exercised");
    }
}
