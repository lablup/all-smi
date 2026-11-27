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

// Common JSON and text parsing utilities for device modules.

use crate::device::common::{DeviceError, DeviceResult};
use serde::de::DeserializeOwned;
use serde_json::Value;

/// Split a CSV line by comma, trimming whitespace around each field.
pub fn parse_csv_line(line: &str) -> Vec<String> {
    line.split(',')
        .map(|s| s.trim().to_string())
        .collect::<Vec<_>>()
}

/// Parse u64 from string with error context.
#[allow(dead_code)] // Library helper (tests/device_tests.rs); the binary does not call it.
pub fn parse_u64(s: &str) -> DeviceResult<u64> {
    s.trim()
        .parse::<u64>()
        .map_err(|e| DeviceError::ParseError(format!("Failed to parse u64 from '{s}': {e}")))
}

/// Parse u32 from string with error context.
#[allow(dead_code)] // Library helper (tests/device_tests.rs); the binary does not call it.
pub fn parse_u32(s: &str) -> DeviceResult<u32> {
    s.trim()
        .parse::<u32>()
        .map_err(|e| DeviceError::ParseError(format!("Failed to parse u32 from '{s}': {e}")))
}

/// Parse f64 from string with error context.
#[allow(dead_code)] // Library helper (tests/device_tests.rs); the binary does not call it.
pub fn parse_f64(s: &str) -> DeviceResult<f64> {
    s.trim()
        .parse::<f64>()
        .map_err(|e| DeviceError::ParseError(format!("Failed to parse f64 from '{s}': {e}")))
}

/// Get a JSON field by key, returning a DeviceError if not found.
#[allow(dead_code)] // Library helper (tests/device_tests.rs); the binary does not call it.
pub fn json_get<'a>(v: &'a Value, key: &str) -> DeviceResult<&'a Value> {
    v.get(key)
        .ok_or_else(|| DeviceError::ParseError(format!("Missing JSON key '{key}'")))
}

/// Parse a JSON field into a concrete type using Serde.
#[allow(dead_code)] // Library helper (tests/device_tests.rs); the binary does not call it.
pub fn json_parse<T: DeserializeOwned>(v: &Value, key: &str) -> DeviceResult<T> {
    let field = json_get(v, key)?;
    serde_json::from_value(field.clone()).map_err(|e| {
        DeviceError::ParseError(format!(
            "Failed to deserialize JSON key '{key}' into target type: {e}"
        ))
    })
}

/// Extract string from JSON, returning ParseError on failure.
#[allow(dead_code)] // Library helper (tests/device_tests.rs); the binary does not call it.
pub fn json_string(v: &Value, key: &str) -> DeviceResult<String> {
    match json_get(v, key)? {
        Value::String(s) => Ok(s.clone()),
        other => Err(DeviceError::ParseError(format!(
            "Expected string at key '{key}', found: {other}"
        ))),
    }
}

/// Extract u64 from JSON number or string.
#[allow(dead_code)] // Library helper (tests/device_tests.rs); the binary does not call it.
pub fn json_u64(v: &Value, key: &str) -> DeviceResult<u64> {
    match json_get(v, key)? {
        Value::Number(n) => n
            .as_u64()
            .ok_or_else(|| DeviceError::ParseError(format!("JSON number at '{key}' not u64"))),
        Value::String(s) => parse_u64(s),
        other => Err(DeviceError::ParseError(format!(
            "Expected u64 (number/string) at key '{key}', found: {other}"
        ))),
    }
}

/// Extract f64 from JSON number or string.
#[allow(dead_code)] // Library helper (tests/device_tests.rs); the binary does not call it.
pub fn json_f64(v: &Value, key: &str) -> DeviceResult<f64> {
    match json_get(v, key)? {
        Value::Number(n) => n
            .as_f64()
            .ok_or_else(|| DeviceError::ParseError(format!("JSON number at '{key}' not f64"))),
        Value::String(s) => parse_f64(s),
        other => Err(DeviceError::ParseError(format!(
            "Expected f64 (number/string) at key '{key}', found: {other}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_parse_csv_line() {
        let line = " 1, 2 ,3 , four ";
        let parts = parse_csv_line(line);
        assert_eq!(parts, vec!["1", "2", "3", "four"]);
    }

    #[test]
    fn test_parse_numbers() {
        assert_eq!(parse_u64("42").unwrap(), 42);
        assert!(parse_u64("x").is_err());

        assert_eq!(parse_u32("7").unwrap(), 7);
        assert!(parse_u32("7.2").is_err());

        assert!((parse_f64("2.34").unwrap() - 2.34).abs() < 1e-9);
        assert!(parse_f64("pi").is_err());
    }

    #[test]
    fn test_json_extractors() {
        let v = json!({
            "a": "hello",
            "b": 123,
            "c": "456",
            "d": 1.5,
            "e": "2.5"
        });

        assert_eq!(json_string(&v, "a").unwrap(), "hello");
        assert_eq!(json_u64(&v, "b").unwrap(), 123);
        assert_eq!(json_u64(&v, "c").unwrap(), 456);
        assert!((json_f64(&v, "d").unwrap() - 1.5).abs() < 1e-9);
        assert!((json_f64(&v, "e").unwrap() - 2.5).abs() < 1e-9);

        assert!(json_get(&v, "missing").is_err());
    }

    #[test]
    fn test_json_parse_generic() {
        let v = json!({ "obj": { "x": 1, "y": 2 } });
        #[derive(serde::Deserialize, Debug, PartialEq)]
        struct Obj {
            x: u32,
            y: u32,
        }
        let obj: Obj = json_parse(&v, "obj").unwrap();
        assert_eq!(obj, Obj { x: 1, y: 2 });
    }
}
