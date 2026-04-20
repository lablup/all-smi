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

//! JSON snapshot serializer.
//!
//! Output shape:
//!
//! * Single sample -> one top-level JSON object, schema-pinned as
//!   `{ "schema": 1, "timestamp": "...", "hostname": "...", "gpus": [...], ... }`.
//! * Multi-sample -> JSON array of the same object shape.
//!
//! Missing sections (not requested via `--include`) are omitted entirely
//! rather than serialized as empty arrays, per the issue spec.

use anyhow::{Context, Result};

use crate::snapshot::Snapshot;

/// Render a slice of snapshots to JSON.
///
/// When `snapshots.len() == 1`, the output is a single JSON object. When
/// greater, it is a JSON array so `--samples > 1` can be fed directly to
/// `jq -c '.[]'`. A newline is appended so piped output does not leave the
/// terminal prompt glued to the last `}`.
pub fn render(snapshots: &[Snapshot], pretty: bool) -> Result<String> {
    let mut out = if snapshots.len() == 1 {
        let value = &snapshots[0];
        if pretty {
            serde_json::to_string_pretty(value).context("failed to serialize snapshot to JSON")?
        } else {
            serde_json::to_string(value).context("failed to serialize snapshot to JSON")?
        }
    } else if pretty {
        serde_json::to_string_pretty(snapshots).context("failed to serialize snapshots to JSON")?
    } else {
        serde_json::to_string(snapshots).context("failed to serialize snapshots to JSON")?
    };
    out.push('\n');
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::Snapshot;
    use serde_json::Value;

    fn minimal_snapshot() -> Snapshot {
        Snapshot {
            schema: 1,
            timestamp: "2026-04-20T00:00:00Z".to_string(),
            hostname: "testhost".to_string(),
            gpus: None,
            cpus: Some(Vec::new()),
            memory: Some(Vec::new()),
            chassis: None,
            processes: None,
            storage: None,
            errors: Vec::new(),
        }
    }

    #[test]
    fn single_sample_is_object() {
        let snap = minimal_snapshot();
        let rendered = render(std::slice::from_ref(&snap), false).unwrap();
        let parsed: Value = serde_json::from_str(rendered.trim()).unwrap();
        assert!(parsed.is_object());
        assert_eq!(parsed["schema"], Value::from(1));
        assert_eq!(parsed["timestamp"], "2026-04-20T00:00:00Z");
        // Missing includes absent (not empty arrays).
        assert!(parsed.get("gpus").is_none());
        assert!(parsed.get("chassis").is_none());
        // Requested but empty sections still serialize as empty arrays.
        assert!(parsed.get("cpus").unwrap().is_array());
    }

    #[test]
    fn multi_sample_is_array() {
        let snaps = vec![minimal_snapshot(), minimal_snapshot()];
        let rendered = render(&snaps, false).unwrap();
        let parsed: Value = serde_json::from_str(rendered.trim()).unwrap();
        assert!(parsed.is_array());
        assert_eq!(parsed.as_array().unwrap().len(), 2);
    }

    #[test]
    fn pretty_output_has_newlines() {
        let snap = minimal_snapshot();
        let rendered = render(std::slice::from_ref(&snap), true).unwrap();
        assert!(rendered.contains('\n'));
        assert!(rendered.ends_with('\n'));
    }

    #[test]
    fn compact_output_ends_with_newline() {
        let snap = minimal_snapshot();
        let rendered = render(std::slice::from_ref(&snap), false).unwrap();
        assert!(rendered.ends_with('\n'));
    }
}
