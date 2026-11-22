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

//! Borrowed label set for one Prometheus line (issue #500).
//!
//! Replaces the per-line `HashMap<String, String>`: keys and values borrow
//! from the scraped text unless sanitizing actually changes them (an escape
//! sequence, a control character, or an over-long value), so a typical line
//! allocates nothing once the reused vector has grown.

use std::borrow::Cow;

use crate::parsing::common::sanitize_label_value;

/// Most labels kept from one line.
pub(crate) const MAX_LABELS: usize = 100;
/// Longest label key or value kept, in bytes.
pub(crate) const MAX_LABEL_LENGTH: usize = 1024;
/// Longest label string (the text between the braces) processed, in bytes.
pub(crate) const MAX_INPUT_LENGTH: usize = 32768;

/// Labels of one line. Lookup is a linear scan from the end, which is
/// faster than hashing for the 5-20 labels a line carries and gives the
/// same "last duplicate wins" result the previous `HashMap` insert did.
#[derive(Debug, Default)]
pub(crate) struct Labels<'a>(Vec<(Cow<'a, str>, Cow<'a, str>)>);

impl<'a> Labels<'a> {
    pub(crate) fn new() -> Self {
        Self(Vec::with_capacity(16))
    }

    /// Value of `key`, if present.
    pub(crate) fn get(&self, key: &str) -> Option<&str> {
        self.0
            .iter()
            .rev()
            .find(|(k, _)| k.as_ref() == key)
            .map(|(_, v)| v.as_ref())
    }

    /// Replace the contents with the labels parsed from `labels_str`,
    /// keeping the vector's allocation for the next line.
    ///
    /// Splitting is quote-aware: only commas outside a double-quoted value
    /// separate labels, so a label value containing `",` cannot inject fake
    /// labels. Every bound of the previous parser is kept: at most
    /// [`MAX_LABELS`] labels, keys and values over [`MAX_LABEL_LENGTH`]
    /// bytes dropped, and an input over [`MAX_INPUT_LENGTH`] bytes yields no
    /// labels.
    pub(crate) fn parse_into(&mut self, labels_str: &'a str) {
        self.0.clear();
        if labels_str.len() > MAX_INPUT_LENGTH {
            tracing::warn!(
                bytes = labels_str.len(),
                "label string longer than {MAX_INPUT_LENGTH} bytes ignored"
            );
            return;
        }
        let mut label_count = 0usize;
        for label in QuoteAwareSplit::new(labels_str) {
            if label_count >= MAX_LABELS {
                break;
            }
            let Some(eq_pos) = label.find('=') else {
                continue;
            };
            let key = sanitize_key(&label[..eq_pos]);
            let value = unescape_label_value(&label[eq_pos + 1..]);
            if key.len() <= MAX_LABEL_LENGTH && value.len() <= MAX_LABEL_LENGTH {
                self.0.push((key, value));
                label_count += 1;
            }
        }
    }

    /// Owned copy of the labels for tests that compare against a map.
    #[cfg(test)]
    pub(crate) fn from_pairs<I, K, V>(pairs: I) -> Labels<'static>
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        Labels(
            pairs
                .into_iter()
                .map(|(k, v)| (Cow::Owned(k.into()), Cow::Owned(v.into())))
                .collect(),
        )
    }

    #[cfg(test)]
    pub(crate) fn to_map(&self) -> std::collections::HashMap<String, String> {
        self.0
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }
}

/// Iterator over the comma-separated labels of a label string, ignoring
/// commas inside double-quoted values (with `\"` escapes honoured).
pub(crate) struct QuoteAwareSplit<'a> {
    text: &'a str,
    start: usize,
    done: bool,
}

impl<'a> QuoteAwareSplit<'a> {
    pub(crate) fn new(text: &'a str) -> Self {
        Self {
            text,
            start: 0,
            done: false,
        }
    }
}

impl<'a> Iterator for QuoteAwareSplit<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<&'a str> {
        if self.done {
            return None;
        }
        let bytes = self.text.as_bytes();
        let mut i = self.start;
        let mut in_quotes = false;
        while i < bytes.len() {
            let b = bytes[i];
            if in_quotes {
                if b == b'\\' && i + 1 < bytes.len() {
                    // Skip the escaped byte so `\"` doesn't terminate the quote.
                    i += 2;
                    continue;
                }
                if b == b'"' {
                    in_quotes = false;
                }
            } else if b == b'"' {
                in_quotes = true;
            } else if b == b',' {
                let item = &self.text[self.start..i];
                self.start = i + 1;
                return Some(item);
            }
            i += 1;
        }
        self.done = true;
        Some(&self.text[self.start..])
    }
}

fn has_control_chars(s: &str) -> bool {
    s.chars().any(char::is_control)
}

/// Borrowing form of [`sanitize_label_value`]: identical output, allocated
/// only when stripping or truncation changes the text.
fn sanitize_borrowed(raw: &str) -> Cow<'_, str> {
    let cleaned = raw.trim().trim_matches('"');
    if cleaned.len() <= MAX_LABEL_LENGTH && !has_control_chars(cleaned) {
        Cow::Borrowed(cleaned)
    } else {
        Cow::Owned(sanitize_label_value(raw))
    }
}

/// Trim whitespace and stray quotes from a label key.
fn sanitize_key(raw: &str) -> Cow<'_, str> {
    sanitize_borrowed(raw)
}

/// Strip the surrounding double quotes from a Prometheus label value and
/// un-escape the sequences emitted by `MetricBuilder::metric` (`\\`, `\"`,
/// `\n`, `\r`). Borrows unless the value holds an escape, a control
/// character, or more than [`MAX_LABEL_LENGTH`] bytes; the result always
/// equals [`unescape_label_value_owned`].
pub(crate) fn unescape_label_value(raw: &str) -> Cow<'_, str> {
    let trimmed = raw.trim();
    let inner = match (trimmed.strip_prefix('"'), trimmed.ends_with('"')) {
        (Some(s), true) if trimmed.len() >= 2 => &s[..s.len() - 1],
        _ => return sanitize_borrowed(raw),
    };
    if inner.len() <= MAX_LABEL_LENGTH && !inner.contains('\\') && !has_control_chars(inner) {
        Cow::Borrowed(inner)
    } else {
        Cow::Owned(unescape_label_value_owned(raw))
    }
}

/// Allocating unescape: the reference behaviour [`unescape_label_value`]
/// reproduces. If the value is not quoted, fall back to the shared
/// sanitizer so legacy unquoted inputs see the same trimming.
pub(crate) fn unescape_label_value_owned(raw: &str) -> String {
    let trimmed = raw.trim();
    let inner = match (trimmed.strip_prefix('"'), trimmed.ends_with('"')) {
        (Some(s), true) if trimmed.len() >= 2 => &s[..s.len() - 1],
        _ => return sanitize_label_value(raw),
    };

    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('\\') => out.push('\\'),
            Some('"') => out.push('"'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some(other) => {
                // Unknown escape: keep the backslash and the following char
                // verbatim so we do not silently lose data.
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }

    // Strip control characters (including ANSI escape sequences) to defend
    // against TUI escape injection from compromised remote endpoints. This
    // mirrors the stripping done in `sanitize_label_value` for unquoted
    // values, ensuring ALL label values are safe regardless of quoting.
    let out = crate::parsing::common::strip_control_chars(&out);

    // Run through length truncation (the shared sanitizer also does this,
    // but `inner` has already had its quotes removed and any stray
    // leading/trailing whitespace inside the quotes is intentional).
    if out.len() > MAX_LABEL_LENGTH {
        let mut end = MAX_LABEL_LENGTH;
        while !out.is_char_boundary(end) {
            end -= 1;
        }
        out[..end].to_string()
    } else {
        out
    }
}

#[cfg(test)]
#[path = "prometheus_labels_tests.rs"]
pub(crate) mod tests;
