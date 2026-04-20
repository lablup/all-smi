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

//! CSV snapshot serializer.
//!
//! Design:
//!
//! * One row per device, one CSV table for the whole snapshot.
//! * Column set comes from `--query` if provided, otherwise from
//!   [`crate::snapshot::default_csv_columns`]. The dot-path evaluator lets
//!   any column resolve across every section — missing paths yield empty
//!   cells, consistent with `nvidia-smi --query-gpu` behaviour.
//! * When `--samples > 1`, a `timestamp` column is prepended so rows can
//!   be disambiguated across samples.
//! * Reader failures are emitted on `stderr` because a dedicated `errors`
//!   column would either pollute every row or require a second table — the
//!   issue spec explicitly permits stderr for this case.
//! * Rows are written by hand (no `csv` crate dependency) per the issue's
//!   "keep deps light" directive. RFC-4180 quoting is handled inline.

use std::fmt::Write as _;

use anyhow::Result;

use crate::cli::SnapshotIncludes;
use crate::snapshot::{
    Snapshot, SnapshotOptions, buckets_for_csv, effective_csv_columns,
    query::{csv_quote, resolve_as_string},
};

/// Render snapshots to CSV text.
pub fn render(opts: &SnapshotOptions, snapshots: &[Snapshot]) -> Result<String> {
    let columns = effective_csv_columns(opts);
    let includes = opts.includes;
    let emit_timestamp = snapshots.len() > 1;

    let mut out = String::new();
    write_header(&mut out, &columns, emit_timestamp);

    for snap in snapshots {
        write_snapshot_rows(&mut out, snap, &columns, &includes, emit_timestamp);
        // Surface reader errors on stderr rather than injecting them into
        // the CSV stream. The spec explicitly allows this for CSV mode.
        for err in &snap.errors {
            eprintln!(
                "snapshot: {section} reader {kind}: {message}",
                section = err.section,
                kind = err.kind,
                message = err.message
            );
        }
    }

    Ok(out)
}

fn write_header(out: &mut String, columns: &[String], emit_timestamp: bool) {
    if emit_timestamp {
        out.push_str("timestamp");
        if !columns.is_empty() {
            out.push(',');
        }
    }
    for (i, col) in columns.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&csv_quote(col));
    }
    out.push('\n');
}

fn write_snapshot_rows(
    out: &mut String,
    snap: &Snapshot,
    columns: &[String],
    includes: &SnapshotIncludes,
    emit_timestamp: bool,
) {
    let buckets = buckets_for_csv(snap, includes);

    for (_section, devices) in &buckets {
        for device_value in devices {
            let mut row = String::new();
            if emit_timestamp {
                row.push_str(&csv_quote(&snap.timestamp));
                if !columns.is_empty() {
                    row.push(',');
                }
            }
            for (i, col) in columns.iter().enumerate() {
                if i > 0 {
                    row.push(',');
                }
                let cell = resolve_as_string(device_value, col);
                row.push_str(&csv_quote(&cell));
            }
            row.push('\n');
            // Writing straight to `String` cannot fail — `write!` returns
            // `Result<(), fmt::Error>` which only surfaces in fmt impls.
            let _ = out.write_str(&row);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{SnapshotFormat, SnapshotIncludes};
    use crate::device::GpuInfo;
    use crate::snapshot::{Snapshot, SnapshotOptions};
    use std::collections::HashMap;

    fn gpu(name: &str, util: f64, temp: u32) -> GpuInfo {
        GpuInfo {
            uuid: format!("{name}-uuid"),
            time: "2026-04-20T00:00:00Z".to_string(),
            name: name.to_string(),
            device_type: "GPU".to_string(),
            host_id: "host0".to_string(),
            hostname: "host0".to_string(),
            instance: "host0".to_string(),
            utilization: util,
            ane_utilization: 0.0,
            dla_utilization: None,
            tensorcore_utilization: None,
            temperature: temp,
            used_memory: 1024,
            total_memory: 8192,
            frequency: 1500,
            power_consumption: 250.0,
            gpu_core_count: None,
            temperature_threshold_slowdown: None,
            temperature_threshold_shutdown: None,
            temperature_threshold_max_operating: None,
            temperature_threshold_acoustic: None,
            performance_state: None,
            numa_node_id: None,
            gsp_firmware_mode: None,
            gsp_firmware_version: None,
            nvlink_remote_devices: Vec::new(),
            gpm_metrics: None,
            detail: HashMap::new(),
        }
    }

    fn snap_with_gpus(gpus: Vec<GpuInfo>) -> Snapshot {
        Snapshot {
            schema: 1,
            timestamp: "2026-04-20T00:00:00Z".to_string(),
            hostname: "host0".to_string(),
            gpus: Some(gpus),
            cpus: None,
            memory: None,
            chassis: None,
            processes: None,
            storage: None,
            errors: Vec::new(),
        }
    }

    #[test]
    fn query_columns_become_header_exactly() {
        let opts = SnapshotOptions {
            format: SnapshotFormat::Csv,
            query: vec![
                "index".to_string(),
                "name".to_string(),
                "utilization".to_string(),
                "temperature".to_string(),
            ],
            includes: SnapshotIncludes {
                gpu: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let snap = snap_with_gpus(vec![gpu("A100", 80.0, 60), gpu("H100", 92.0, 65)]);
        let rendered = render(&opts, &[snap]).unwrap();
        let mut lines = rendered.lines();
        assert_eq!(lines.next(), Some("index,name,utilization,temperature"));
        // serde_json renders f64 with a fractional part, so `80` becomes
        // `80.0`. Downstream consumers that expect integers can cast.
        assert_eq!(lines.next(), Some("0,A100,80.0,60"));
        assert_eq!(lines.next(), Some("1,H100,92.0,65"));
        assert_eq!(lines.next(), None);
    }

    #[test]
    fn samples_multi_prepends_timestamp_column() {
        let opts = SnapshotOptions {
            format: SnapshotFormat::Csv,
            query: vec!["name".to_string(), "utilization".to_string()],
            includes: SnapshotIncludes {
                gpu: true,
                ..Default::default()
            },
            samples: 2,
            ..Default::default()
        };
        let s1 = snap_with_gpus(vec![gpu("A100", 50.0, 50)]);
        let mut s2 = snap_with_gpus(vec![gpu("A100", 60.0, 52)]);
        s2.timestamp = "2026-04-20T00:00:01Z".to_string();
        let rendered = render(&opts, &[s1, s2]).unwrap();
        let mut lines = rendered.lines();
        assert_eq!(lines.next(), Some("timestamp,name,utilization"));
        assert_eq!(lines.next(), Some("2026-04-20T00:00:00Z,A100,50.0"));
        assert_eq!(lines.next(), Some("2026-04-20T00:00:01Z,A100,60.0"));
    }

    #[test]
    fn missing_paths_yield_empty_cells() {
        let opts = SnapshotOptions {
            format: SnapshotFormat::Csv,
            query: vec![
                "name".to_string(),
                "detail.cuda_version".to_string(),
                "bogus_field".to_string(),
            ],
            includes: SnapshotIncludes {
                gpu: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let snap = snap_with_gpus(vec![gpu("A100", 80.0, 60)]);
        let rendered = render(&opts, &[snap]).unwrap();
        let mut lines = rendered.lines();
        assert_eq!(lines.next(), Some("name,detail.cuda_version,bogus_field"));
        assert_eq!(lines.next(), Some("A100,,"));
    }

    #[test]
    fn default_columns_when_no_query() {
        let opts = SnapshotOptions {
            format: SnapshotFormat::Csv,
            includes: SnapshotIncludes {
                gpu: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let snap = snap_with_gpus(vec![gpu("A100", 80.0, 60)]);
        let rendered = render(&opts, &[snap]).unwrap();
        let header = rendered.lines().next().unwrap();
        assert!(header.starts_with("section,index,hostname,name"));
        assert!(header.contains("temperature"));
    }

    #[test]
    fn empty_snapshot_still_prints_header() {
        let opts = SnapshotOptions {
            format: SnapshotFormat::Csv,
            query: vec!["name".to_string()],
            includes: SnapshotIncludes {
                gpu: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let snap = snap_with_gpus(vec![]);
        let rendered = render(&opts, &[snap]).unwrap();
        assert_eq!(rendered, "name\n");
    }
}
