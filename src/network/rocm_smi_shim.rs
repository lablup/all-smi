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

//! Agentless SSH `rocm-smi` fallback parser (issue #194).
//!
//! When a remote AMD host has `rocm-smi` but not `all-smi` installed,
//! we invoke `rocm-smi --showuse --showmemuse --showtemp --showpower
//! --json` and parse the resulting JSON into [`GpuInfo`] records. The
//! command is exported via [`ROCM_SMI_COMMAND`] so the parser and the
//! SSH client cannot drift out of sync.
//!
//! rocm-smi's JSON shape varies slightly across ROCm versions, so the
//! parser is intentionally lenient:
//!
//! * Top-level keys are `card0`, `card1`, ...  plus a `system` key
//!   carrying the driver version.  We iterate only `card*` entries.
//! * Field names are looked up with fallbacks because rocm-smi renames
//!   them between ROCm 5.x and 6.x (`GPU use (%)` vs
//!   `GPU use (%) sum`, for example).

use std::collections::HashMap;

use serde_json::Value;

use crate::device::GpuInfo;

/// `rocm-smi` command the SSH transport invokes on the remote host.
pub const ROCM_SMI_COMMAND: &str =
    "rocm-smi --showuse --showmemuse --showtemp --showpower --showproductname --json";

/// Errors surfaced by [`parse_rocm_smi_json`].
#[derive(Debug, thiserror::Error)]
pub enum RocmSmiParseError {
    #[error("rocm-smi JSON parse error: {0}")]
    Json(String),
    #[error("rocm-smi JSON top-level value is not an object")]
    NotObject,
}

/// Parse `rocm-smi ... --json` output.
///
/// See the module docstring for the exact command string and JSON
/// shape. A missing field inside a card entry is treated as "unknown"
/// (field becomes zero or `None`) rather than a hard error.
pub fn parse_rocm_smi_json(
    json: &str,
    host_id: &str,
    hostname: &str,
    timestamp: &str,
) -> Result<Vec<GpuInfo>, RocmSmiParseError> {
    let v: Value =
        serde_json::from_str(json).map_err(|e| RocmSmiParseError::Json(e.to_string()))?;
    let obj = v.as_object().ok_or(RocmSmiParseError::NotObject)?;

    let driver_version = obj
        .get("system")
        .and_then(|s| s.as_object())
        .and_then(|s| s.get("Driver version"))
        .and_then(|v| v.as_str())
        .map(str::to_string);

    let mut cards: Vec<(u32, &Value)> = obj
        .iter()
        .filter_map(|(k, v)| {
            k.strip_prefix("card")
                .and_then(|n| n.parse::<u32>().ok())
                .map(|idx| (idx, v))
        })
        .collect();
    cards.sort_by_key(|(i, _)| *i);

    let mut out = Vec::with_capacity(cards.len());
    for (idx, card_val) in cards {
        let Some(card) = card_val.as_object() else {
            continue;
        };
        let gpu = parse_card(
            idx,
            card,
            driver_version.as_deref(),
            host_id,
            hostname,
            timestamp,
        );
        out.push(gpu);
    }
    Ok(out)
}

fn parse_card(
    idx: u32,
    card: &serde_json::Map<String, Value>,
    driver_version: Option<&str>,
    host_id: &str,
    hostname: &str,
    timestamp: &str,
) -> GpuInfo {
    // rocm-smi reports GPU utilization in a few possible key names
    // depending on the ROCm version.
    let utilization = lookup_f64(
        card,
        &[
            "GPU use (%)",
            "GPU use %",
            "GPU Utilization",
            "GPU use (%) sum",
        ],
    )
    .unwrap_or(0.0);

    let memory_utilization_pct = lookup_f64(
        card,
        &[
            "GPU Memory Allocated (VRAM%)",
            "GPU memory use (%)",
            "Memory use (%)",
        ],
    );

    // Memory absolute values — rocm-smi splits across multiple fields.
    // Prefer absolute figures when present; otherwise derive used from
    // the percentage and total.
    let total_memory_bytes = lookup_u64(
        card,
        &[
            "VRAM Total Memory (B)",
            "GPU memory total (B)",
            "Total memory (B)",
        ],
    )
    .unwrap_or(0);
    let used_memory_bytes = lookup_u64(
        card,
        &[
            "VRAM Total Used Memory (B)",
            "GPU memory used (B)",
            "Used memory (B)",
        ],
    )
    .unwrap_or_else(|| {
        if let Some(pct) = memory_utilization_pct {
            ((pct / 100.0) * total_memory_bytes as f64) as u64
        } else {
            0
        }
    });

    // rocm-smi reports several temperatures; we pick the edge (junction)
    // temperature because that is what nvidia-smi's `temperature.gpu`
    // is comparable against. Fall back to sensor-less value of zero.
    let temperature = lookup_f64(
        card,
        &[
            "Temperature (Sensor edge) (C)",
            "Temperature (Sensor junction) (C)",
            "Temperature (Sensor memory) (C)",
            "Temperature (C)",
        ],
    )
    .unwrap_or(0.0) as u32;

    let power_consumption = lookup_f64(
        card,
        &[
            "Average Graphics Package Power (W)",
            "Current Socket Graphics Package Power (W)",
            "GPU Power (W)",
            "Power (W)",
        ],
    )
    .unwrap_or(0.0);

    let name = card
        .get("Card series")
        .or_else(|| card.get("Card model"))
        .or_else(|| card.get("GFX Version"))
        .and_then(|v| v.as_str())
        .unwrap_or("AMD GPU")
        .to_string();

    let uuid = card
        .get("Unique ID")
        .or_else(|| card.get("GUID"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("n/a"))
        .map(str::to_string)
        .unwrap_or_else(|| format!("rocm-{hostname}-{idx}"));

    let mut detail: HashMap<String, String> = HashMap::new();
    if let Some(dv) = driver_version {
        detail.insert("driver_version".to_string(), dv.to_string());
    }
    detail.insert("transport".to_string(), "ssh/rocm-smi".to_string());

    GpuInfo {
        uuid,
        time: timestamp.to_string(),
        name,
        device_type: "GPU".to_string(),
        host_id: host_id.to_string(),
        hostname: hostname.to_string(),
        instance: format!("{hostname}:{idx}"),
        utilization,
        ane_utilization: 0.0,
        dla_utilization: None,
        tensorcore_utilization: None,
        temperature,
        used_memory: used_memory_bytes,
        total_memory: total_memory_bytes,
        frequency: 0,
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
    }
}

fn lookup_f64(card: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<f64> {
    for key in keys {
        if let Some(val) = card.get(*key) {
            if let Some(n) = val.as_f64() {
                return Some(n);
            }
            // rocm-smi often stringifies numbers: "45.0", "23".
            if let Some(s) = val.as_str()
                && let Ok(n) = s.trim().parse::<f64>()
            {
                return Some(n);
            }
        }
    }
    None
}

fn lookup_u64(card: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<u64> {
    for key in keys {
        if let Some(val) = card.get(*key) {
            if let Some(n) = val.as_u64() {
                return Some(n);
            }
            if let Some(s) = val.as_str()
                && let Ok(n) = s.trim().parse::<u64>()
            {
                return Some(n);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOLDEN_ROCM6: &str = r#"{
        "card0": {
            "GPU use (%)": "68",
            "GPU Memory Allocated (VRAM%)": "42",
            "VRAM Total Memory (B)": "68719476736",
            "VRAM Total Used Memory (B)": "28823961600",
            "Temperature (Sensor edge) (C)": "72.0",
            "Temperature (Sensor junction) (C)": "78.0",
            "Average Graphics Package Power (W)": "235.5",
            "Card series": "Instinct MI250X",
            "Unique ID": "0x1234abcd"
        },
        "card1": {
            "GPU use (%)": "3",
            "GPU Memory Allocated (VRAM%)": "5",
            "VRAM Total Memory (B)": "68719476736",
            "VRAM Total Used Memory (B)": "3435973836",
            "Temperature (Sensor edge) (C)": "45.0",
            "Average Graphics Package Power (W)": "95.2",
            "Card series": "Instinct MI250X",
            "Unique ID": "0xbeefcafe"
        },
        "system": {
            "Driver version": "6.1.1"
        }
    }"#;

    #[test]
    fn parses_two_card_rocm6_sample() {
        let gpus =
            parse_rocm_smi_json(GOLDEN_ROCM6, "admin@amd01", "amd01", "2026-04-20T00:00:00Z")
                .expect("valid golden sample must parse");
        assert_eq!(gpus.len(), 2);

        let g0 = &gpus[0];
        assert_eq!(g0.uuid, "0x1234abcd");
        assert_eq!(g0.name, "Instinct MI250X");
        assert_eq!(g0.utilization, 68.0);
        assert_eq!(g0.total_memory, 68719476736);
        assert_eq!(g0.used_memory, 28823961600);
        assert_eq!(g0.temperature, 72);
        assert!((g0.power_consumption - 235.5).abs() < 1e-6);
        assert_eq!(g0.hostname, "amd01");
        assert_eq!(g0.host_id, "admin@amd01");
        assert_eq!(g0.instance, "amd01:0");
        assert_eq!(g0.detail.get("driver_version"), Some(&"6.1.1".to_string()));
        assert_eq!(
            g0.detail.get("transport"),
            Some(&"ssh/rocm-smi".to_string())
        );

        let g1 = &gpus[1];
        assert_eq!(g1.uuid, "0xbeefcafe");
        assert_eq!(g1.instance, "amd01:1");
    }

    #[test]
    fn parses_rocm5_alternate_key_names() {
        let csv = r#"{
            "card0": {
                "GPU use %": 22.5,
                "Used memory (B)": 12345,
                "Total memory (B)": 68719476736,
                "Temperature (C)": 60,
                "Power (W)": 150
            }
        }"#;
        let gpus = parse_rocm_smi_json(csv, "u@h", "h", "t").unwrap();
        assert_eq!(gpus.len(), 1);
        assert_eq!(gpus[0].utilization, 22.5);
        assert_eq!(gpus[0].total_memory, 68719476736);
        assert_eq!(gpus[0].used_memory, 12345);
        assert_eq!(gpus[0].temperature, 60);
        assert!((gpus[0].power_consumption - 150.0).abs() < 1e-6);
        // Without a `Unique ID` or `Card series`, we synthesize a uuid
        // and fall back to a generic name.
        assert_eq!(gpus[0].uuid, "rocm-h-0");
        assert_eq!(gpus[0].name, "AMD GPU");
    }

    #[test]
    fn empty_object_yields_empty_vec() {
        let gpus = parse_rocm_smi_json("{}", "u@h", "h", "t").unwrap();
        assert!(gpus.is_empty());
    }

    #[test]
    fn rejects_non_json_input() {
        let err = parse_rocm_smi_json("not-json", "u@h", "h", "t").unwrap_err();
        assert!(matches!(err, RocmSmiParseError::Json(_)));
    }

    #[test]
    fn rejects_non_object_root() {
        let err = parse_rocm_smi_json("[]", "u@h", "h", "t").unwrap_err();
        assert!(matches!(err, RocmSmiParseError::NotObject));
    }

    #[test]
    fn ignores_non_card_keys() {
        let json = r#"{
            "card0": { "GPU use (%)": "10" },
            "some other key": { "x": "y" }
        }"#;
        let gpus = parse_rocm_smi_json(json, "u@h", "h", "t").unwrap();
        assert_eq!(gpus.len(), 1);
    }

    #[test]
    fn derives_used_memory_from_percent() {
        // When absolute used memory is absent but percentage and
        // total are present, we should derive it.
        let json = r#"{
            "card0": {
                "GPU Memory Allocated (VRAM%)": "50",
                "VRAM Total Memory (B)": "1000000"
            }
        }"#;
        let gpus = parse_rocm_smi_json(json, "u@h", "h", "t").unwrap();
        assert_eq!(gpus[0].used_memory, 500_000);
    }
}
