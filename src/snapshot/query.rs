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

//! Dot-path query evaluator for snapshot JSON values.
//!
//! Resolves expressions like `memory.used` or `detail.cuda_version` against
//! a [`serde_json::Value`] and stringifies the result for CSV output.
//!
//! Design rules:
//!
//! * Missing keys MUST yield an empty string, never a panic.
//! * Numeric indices into JSON arrays are supported (e.g. `per_socket_info.0.cores`).
//! * `null` values render as empty strings so CSV output stays safe to pipe
//!   through `jq` and `awk` without quoting surprises.
//! * Strings render without surrounding JSON quotes, but any embedded quote,
//!   comma, CR, LF, or backslash causes RFC-4180 quoting to kick in.

use serde_json::Value;

/// Resolve a dot-separated path against a JSON value.
///
/// Returns `None` when the path does not exist. This is distinct from
/// resolving to `null`, which returns `Some(Value::Null)` — the CSV layer
/// turns both into an empty cell, but programmatic callers may want to tell
/// the two apart.
pub fn resolve<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    if path.is_empty() {
        return Some(value);
    }

    let mut current = value;
    for segment in path.split('.') {
        if segment.is_empty() {
            return None;
        }
        match current {
            Value::Object(map) => {
                current = map.get(segment)?;
            }
            Value::Array(arr) => {
                let idx: usize = segment.parse().ok()?;
                current = arr.get(idx)?;
            }
            _ => return None,
        }
    }
    Some(current)
}

/// Stringify a JSON value for CSV output.
///
/// Rules:
///
/// * `Null` -> empty string (distinguishable from the literal string `"null"`).
/// * `Bool` -> `"true"` / `"false"`.
/// * `Number` -> canonical serde_json rendering (no quotes, no trailing zeros).
/// * `String` -> the inner string, *without* JSON quotes.
/// * `Array` / `Object` -> compact JSON, so complex fields still round-trip
///   through a second `jq` pass.
pub fn stringify(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        Value::Array(_) | Value::Object(_) => value.to_string(),
    }
}

/// Quote a cell per RFC 4180 if necessary.
///
/// Called by the CSV serializer, exported here so the query tests can pin
/// the behaviour next to the dot-path evaluator. Bytes that trigger
/// quoting: `,`, `"`, `\n`, `\r`. Any embedded `"` is doubled.
pub fn csv_quote(cell: &str) -> String {
    let needs_quoting = cell
        .chars()
        .any(|c| c == ',' || c == '"' || c == '\n' || c == '\r');
    if !needs_quoting {
        return cell.to_string();
    }
    let escaped = cell.replace('"', "\"\"");
    format!("\"{escaped}\"")
}

/// Resolve a dot-path against a JSON value and stringify the result in one
/// step. Returns an empty string when the path does not exist.
pub fn resolve_as_string(value: &Value, path: &str) -> String {
    resolve(value, path).map(stringify).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn resolves_top_level_field() {
        let v = json!({ "name": "gpu0", "index": 0 });
        assert_eq!(resolve_as_string(&v, "name"), "gpu0");
        assert_eq!(resolve_as_string(&v, "index"), "0");
    }

    #[test]
    fn missing_path_is_empty() {
        let v = json!({ "name": "gpu0" });
        assert_eq!(resolve_as_string(&v, "bogus"), "");
        assert_eq!(resolve_as_string(&v, "name.deep"), "");
        assert_eq!(resolve_as_string(&v, ""), v.to_string());
    }

    #[test]
    fn resolves_nested_object() {
        let v = json!({ "detail": { "cuda_version": "12.4" } });
        assert_eq!(resolve_as_string(&v, "detail.cuda_version"), "12.4");
        assert_eq!(resolve_as_string(&v, "detail.missing"), "");
    }

    #[test]
    fn resolves_array_index() {
        let v = json!({ "cores": [1, 2, 3] });
        assert_eq!(resolve_as_string(&v, "cores.0"), "1");
        assert_eq!(resolve_as_string(&v, "cores.2"), "3");
        assert_eq!(resolve_as_string(&v, "cores.10"), "");
    }

    #[test]
    fn null_renders_as_empty() {
        let v = json!({ "optional": null });
        // Path resolves, but value is null -> empty string.
        assert_eq!(resolve_as_string(&v, "optional"), "");
        assert!(resolve(&v, "optional").is_some());
    }

    #[test]
    fn bool_and_number_render_without_quotes() {
        let v = json!({ "active": true, "temp": 42.5 });
        assert_eq!(resolve_as_string(&v, "active"), "true");
        assert_eq!(resolve_as_string(&v, "temp"), "42.5");
    }

    #[test]
    fn array_and_object_render_as_compact_json() {
        let v = json!({ "tags": ["a", "b"], "labels": { "k": "v" } });
        assert_eq!(resolve_as_string(&v, "tags"), "[\"a\",\"b\"]");
        assert_eq!(resolve_as_string(&v, "labels"), "{\"k\":\"v\"}");
    }

    #[test]
    fn csv_quote_passes_through_simple_strings() {
        assert_eq!(csv_quote("plain"), "plain");
        assert_eq!(csv_quote("42"), "42");
        assert_eq!(csv_quote(""), "");
    }

    #[test]
    fn csv_quote_wraps_and_escapes_special_chars() {
        assert_eq!(csv_quote("has,comma"), "\"has,comma\"");
        assert_eq!(csv_quote("has\"quote"), "\"has\"\"quote\"");
        assert_eq!(csv_quote("has\nnewline"), "\"has\nnewline\"");
        assert_eq!(csv_quote("has\rcr"), "\"has\rcr\"");
    }

    #[test]
    fn malformed_path_yields_empty() {
        let v = json!({ "x": 1 });
        // Trailing dot / double dot -> None path => empty.
        assert_eq!(resolve_as_string(&v, "x."), "");
        assert_eq!(resolve_as_string(&v, "x..y"), "");
    }
}
