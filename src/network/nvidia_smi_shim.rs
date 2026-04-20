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

//! Agentless SSH `nvidia-smi` fallback parser (issue #194).
//!
//! When a remote host does not have `all-smi` installed but does have a
//! working NVIDIA driver, the SSH transport can still render per-GPU
//! metrics by running a fixed `nvidia-smi --query-gpu` command and
//! parsing the CSV output into [`GpuInfo`] records. The command the
//! caller should execute is exported via [`NVIDIA_SMI_COMMAND`] so the
//! client and the parser stay in lockstep.
//!
//! CSV assumptions (`--format=csv,noheader,nounits`):
//!
//! * No header row, no unit suffixes.
//! * Field separator is `, ` (comma + space) in practice; we split on
//!   `,` and trim whitespace so we tolerate either flavour.
//! * Unsupported fields render as `[Not Supported]` or `[N/A]` — both
//!   are treated as zero / absent rather than as hard parse errors.
//! * `N/A` in the `uuid` field is extremely rare; we synthesize a
//!   stable `gpu{index}@{host}` UUID so [`crate::device::GpuInfo`]
//!   consumers that key on UUID still work.

use std::collections::HashMap;

use crate::device::GpuInfo;

/// The exact `nvidia-smi` command string the SSH transport should
/// execute on a remote host. Kept as a single `&'static str` so the
/// parser and the client cannot drift. Matches the `--query-gpu` list
/// documented in issue #194.
pub const NVIDIA_SMI_COMMAND: &str = "nvidia-smi --query-gpu=index,uuid,name,driver_version,utilization.gpu,memory.used,memory.total,temperature.gpu,clocks.current.graphics,power.draw --format=csv,noheader,nounits";

/// Number of columns [`parse_nvidia_smi_csv`] expects per row. Enforced
/// as an exact match so a malicious or broken remote cannot silently
/// misalign field-to-metric mapping by emitting a truncated row.
pub const EXPECTED_COLUMN_COUNT: usize = 10;

/// Errors surfaced by [`parse_nvidia_smi_csv`]. Every error variant
/// carries the raw offending line so the caller (the SSH strategy) can
/// log it against the host that produced it without re-stitching the
/// original input.
#[derive(Debug, thiserror::Error)]
pub enum NvidiaSmiParseError {
    #[error(
        "unexpected column count (want {expected}, got {got}) in line: {line}",
        expected = EXPECTED_COLUMN_COUNT
    )]
    ColumnCount { got: usize, line: String },
}

/// Parse `nvidia-smi --query-gpu=index,uuid,name,driver_version,\
/// utilization.gpu,memory.used,memory.total,temperature.gpu,\
/// clocks.current.graphics,power.draw --format=csv,noheader,nounits`
/// output into a vector of [`GpuInfo`] records.
///
/// * `host_id` is the logical host identifier (e.g. `user@dgx-01:22`)
///   — the value stored on every produced [`GpuInfo`].
/// * `hostname` is the human-readable hostname (`dgx-01`), used for
///   the per-tab label.
/// * `timestamp` is an RFC3339 timestamp string captured by the caller
///   at the moment the SSH `exec` returned.
///
/// Malformed rows are **skipped** with a logged warning rather than
/// failing the whole parse — a single bad row on one GPU must not blind
/// the operator to the rest of the fleet. The function returns an error
/// only when the CSV has zero parseable rows AND at least one malformed
/// row was encountered (i.e. we saw output that looked like CSV but
/// failed every parse attempt).
pub fn parse_nvidia_smi_csv(
    csv: &str,
    host_id: &str,
    hostname: &str,
    timestamp: &str,
) -> Result<Vec<GpuInfo>, NvidiaSmiParseError> {
    let mut out = Vec::new();
    let mut malformed = 0usize;
    let mut last_error: Option<NvidiaSmiParseError> = None;

    for raw_line in csv.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }

        match parse_row(line, host_id, hostname, timestamp) {
            Ok(gpu) => out.push(gpu),
            Err(e) => {
                malformed += 1;
                last_error = Some(e);
            }
        }
    }

    if out.is_empty()
        && malformed > 0
        && let Some(e) = last_error
    {
        return Err(e);
    }

    Ok(out)
}

fn parse_row(
    line: &str,
    host_id: &str,
    hostname: &str,
    timestamp: &str,
) -> Result<GpuInfo, NvidiaSmiParseError> {
    // Columns: 0=index, 1=uuid, 2=name, 3=driver_version,
    // 4=util.gpu, 5=mem.used, 6=mem.total, 7=temp.gpu,
    // 8=clocks.graphics, 9=power.draw
    //
    // Require EXACTLY the expected number of columns. A permissive
    // ">= N" check would let a malicious remote emit a shortened row
    // that silently aliases later metrics onto earlier columns (e.g.
    // making temperature-like values appear as power draw).
    let cols: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
    if cols.len() != EXPECTED_COLUMN_COUNT {
        return Err(NvidiaSmiParseError::ColumnCount {
            got: cols.len(),
            line: line.to_string(),
        });
    }

    let get = |i: usize| -> &str { cols.get(i).copied().unwrap_or("") };

    let index_str = get(0);
    let uuid_raw = get(1);
    let name = get(2).to_string();
    let driver_version = get(3).to_string();
    let utilization = parse_f64(get(4)).unwrap_or(0.0);
    let used_memory_mib = parse_f64(get(5)).unwrap_or(0.0);
    let total_memory_mib = parse_f64(get(6)).unwrap_or(0.0);
    let temperature = parse_f64(get(7)).unwrap_or(0.0) as u32;
    let frequency_mhz = parse_f64(get(8)).unwrap_or(0.0) as u32;
    let power_consumption = parse_f64(get(9)).unwrap_or(0.0);

    let uuid = if is_missing(uuid_raw) {
        format!("gpu{index_str}@{host_id}")
    } else {
        uuid_raw.to_string()
    };

    // nvidia-smi reports memory in MiB with the nounits flag; convert
    // to bytes (×1024×1024) to match the internal `GpuInfo` contract
    // used by the rest of the codebase (bytes, not MiB).
    let used_memory = (used_memory_mib * 1024.0 * 1024.0) as u64;
    let total_memory = (total_memory_mib * 1024.0 * 1024.0) as u64;

    let mut detail: HashMap<String, String> = HashMap::new();
    if !driver_version.is_empty() {
        detail.insert("driver_version".to_string(), driver_version);
    }
    detail.insert("transport".to_string(), "ssh/nvidia-smi".to_string());

    // Build a unique instance string so per-GPU rendering identifies
    // the correct remote slot. The existing remote collector uses
    // `hostname:index`-style labels; follow that.
    let instance = format!("{hostname}:{index_str}");

    Ok(GpuInfo {
        uuid,
        time: timestamp.to_string(),
        name,
        device_type: "GPU".to_string(),
        host_id: host_id.to_string(),
        hostname: hostname.to_string(),
        instance,
        utilization,
        ane_utilization: 0.0,
        dla_utilization: None,
        tensorcore_utilization: None,
        temperature,
        used_memory,
        total_memory,
        frequency: frequency_mhz,
        power_consumption,
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
        detail,
    })
}

/// `nvidia-smi` emits `[Not Supported]`, `[N/A]`, or `N/A` for fields the
/// hardware does not report. We treat all of them as "missing".
fn is_missing(s: &str) -> bool {
    let lo = s.to_ascii_lowercase();
    matches!(
        lo.as_str(),
        "" | "[not supported]" | "[n/a]" | "n/a" | "not supported"
    )
}

fn parse_f64(s: &str) -> Option<f64> {
    if is_missing(s) {
        return None;
    }
    s.parse::<f64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOLDEN_TWO_GPU: &str = "\
0, GPU-abc123, NVIDIA A100-SXM4-80GB, 550.54.15, 45, 12288, 81920, 58, 1410, 285.5
1, GPU-def456, NVIDIA A100-SXM4-80GB, 550.54.15, 12, 1024, 81920, 42, 210, 120.0
";

    #[test]
    fn parses_two_gpu_golden_sample() {
        let gpus = parse_nvidia_smi_csv(
            GOLDEN_TWO_GPU,
            "user@dgx-01:22",
            "dgx-01",
            "2026-04-20T00:00:00Z",
        )
        .expect("valid golden sample must parse");

        assert_eq!(gpus.len(), 2, "two rows -> two GpuInfo records");

        let g0 = &gpus[0];
        assert_eq!(g0.uuid, "GPU-abc123");
        assert_eq!(g0.name, "NVIDIA A100-SXM4-80GB");
        assert_eq!(g0.utilization, 45.0);
        assert_eq!(g0.used_memory, 12288 * 1024 * 1024);
        assert_eq!(g0.total_memory, 81920 * 1024 * 1024);
        assert_eq!(g0.temperature, 58);
        assert_eq!(g0.frequency, 1410);
        assert!((g0.power_consumption - 285.5).abs() < 1e-6);
        assert_eq!(g0.hostname, "dgx-01");
        assert_eq!(g0.host_id, "user@dgx-01:22");
        assert_eq!(g0.instance, "dgx-01:0");
        assert_eq!(g0.device_type, "GPU");
        assert_eq!(
            g0.detail.get("driver_version"),
            Some(&"550.54.15".to_string())
        );
        assert_eq!(
            g0.detail.get("transport"),
            Some(&"ssh/nvidia-smi".to_string())
        );

        assert_eq!(gpus[1].uuid, "GPU-def456");
        assert_eq!(gpus[1].instance, "dgx-01:1");
    }

    #[test]
    fn tolerates_not_supported_sentinels() {
        // Older drivers or vGPU slices often emit [Not Supported] for
        // power.draw or clocks.current.graphics. Those fields must
        // downgrade to zero, not poison the entire row.
        let csv = "0, GPU-xyz, Quadro K2200, 470.82.01, 3, 512, 4096, 45, [Not Supported], [Not Supported]\n";
        let gpus = parse_nvidia_smi_csv(csv, "user@box", "box", "t").unwrap();
        assert_eq!(gpus.len(), 1);
        assert_eq!(gpus[0].frequency, 0);
        assert_eq!(gpus[0].power_consumption, 0.0);
    }

    #[test]
    fn empty_input_yields_empty_vec() {
        let gpus = parse_nvidia_smi_csv("", "u@h", "h", "t").unwrap();
        assert!(gpus.is_empty());
    }

    #[test]
    fn skips_blank_lines() {
        let csv = "\n\n0, GPU-a, N1, 1, 1, 1, 1, 1, 1, 1\n\n";
        let gpus = parse_nvidia_smi_csv(csv, "u@h", "h", "t").unwrap();
        assert_eq!(gpus.len(), 1);
    }

    #[test]
    fn malformed_row_alone_surfaces_error() {
        // If every input row is malformed, surface the last observed
        // error so callers can report it in the TUI.
        let csv = "this is not csv\nstill not csv\n";
        let err = parse_nvidia_smi_csv(csv, "u@h", "h", "t").unwrap_err();
        match err {
            NvidiaSmiParseError::ColumnCount { got, line } => {
                assert_eq!(got, 1);
                assert_eq!(line, "still not csv");
            }
        }
    }

    #[test]
    fn mixed_good_and_bad_rows_keep_good() {
        // A single bad row among good rows must not kill the whole
        // parse. Known-good rows still land in the output.
        let csv = "broken row\n0, GPU-a, N1, 1, 1, 1, 1, 1, 1, 1\n";
        let gpus = parse_nvidia_smi_csv(csv, "u@h", "h", "t").unwrap();
        assert_eq!(gpus.len(), 1);
        assert_eq!(gpus[0].uuid, "GPU-a");
    }

    #[test]
    fn missing_uuid_is_synthesized() {
        // Some driver versions return "N/A" for uuid on very old GPUs;
        // the parser must synthesize a stable UUID so the UI layer's
        // UUID-keyed GPU dedup keeps working.
        let csv = "0, N/A, Tesla K40, 1, 1, 1, 1, 1, 1, 1\n";
        let gpus = parse_nvidia_smi_csv(csv, "u@h", "h", "t").unwrap();
        assert_eq!(gpus.len(), 1);
        assert_eq!(gpus[0].uuid, "gpu0@u@h");
    }

    #[test]
    fn fewer_than_expected_columns_is_error() {
        // A row with fewer than 10 columns cannot be meaningfully
        // rendered — callers must see the error surface.
        let csv = "0, GPU-a, only three\n";
        let err = parse_nvidia_smi_csv(csv, "u@h", "h", "t").unwrap_err();
        match err {
            NvidiaSmiParseError::ColumnCount { got, .. } => assert_eq!(got, 3),
        }
    }

    #[test]
    fn seven_column_row_is_rejected_not_misaligned() {
        // Defense-in-depth for the case where a remote (malicious or
        // buggy) emits a short row that would otherwise silently
        // alias later fields onto earlier columns. We now require
        // EXACTLY 10 columns.
        let csv = "0, GPU-a, N1, drv, 1, 2, 3\n";
        let err = parse_nvidia_smi_csv(csv, "u@h", "h", "t").unwrap_err();
        match err {
            NvidiaSmiParseError::ColumnCount { got, .. } => assert_eq!(got, 7),
        }
    }

    #[test]
    fn nine_column_row_is_rejected() {
        // A 9-column row is the subtle case: previously accepted by
        // the `>= 7` guard; now rejected so metrics cannot drift.
        let csv = "0, GPU-a, N1, 1.0, 10, 20, 30, 40, 50\n";
        let err = parse_nvidia_smi_csv(csv, "u@h", "h", "t").unwrap_err();
        match err {
            NvidiaSmiParseError::ColumnCount { got, line } => {
                assert_eq!(got, 9);
                assert!(line.contains("GPU-a"));
            }
        }
    }

    #[test]
    fn eleven_column_row_is_rejected() {
        // More columns than expected is also suspicious — reject so
        // attackers cannot append bogus data beyond the parser's
        // schema.
        let csv = "0, GPU-a, N1, 1.0, 10, 20, 30, 40, 50, 60, extra\n";
        let err = parse_nvidia_smi_csv(csv, "u@h", "h", "t").unwrap_err();
        match err {
            NvidiaSmiParseError::ColumnCount { got, .. } => assert_eq!(got, 11),
        }
    }

    #[test]
    fn command_string_matches_issue_spec() {
        // Guard against accidental edits to the command string — the
        // column order is hardcoded in parse_row() and a drift would
        // silently mis-attribute fields.
        assert!(NVIDIA_SMI_COMMAND.contains("index,uuid,name,driver_version"));
        assert!(NVIDIA_SMI_COMMAND.contains("utilization.gpu,memory.used,memory.total"));
        assert!(NVIDIA_SMI_COMMAND.contains("temperature.gpu,clocks.current.graphics,power.draw"));
        assert!(NVIDIA_SMI_COMMAND.contains("--format=csv,noheader,nounits"));
    }
}
