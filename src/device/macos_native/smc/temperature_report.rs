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

//! SMC temperature key inventory for the IOReport hardware diagnostic
//! (issue #415).
//!
//! Temperatures are averaged from the static keys when any of them reads in
//! range and from the discovered keys otherwise, so which sensors feed a rail
//! differs by chip. This report prints, for the machine it runs on, which
//! static keys exist and read in range, what discovery finds, which path each
//! rail takes, and the value the getters return. It reads with the same keys,
//! range, and discovery the getters use and changes nothing about how they
//! aggregate.

use super::{
    CPU_STATIC_TEMP_KEYS, GPU_STATIC_TEMP_KEYS, PLAUSIBLE_TEMP_C, SMC, fourcc_to_string,
    str_to_fourcc,
};
use std::fmt::Write as _;

/// One key read the way the temperature getters read it.
enum KeyReading {
    /// The SMC does not have the key.
    Absent,
    /// The key exists but could not be read, or its key info could not be.
    Failed(&'static str),
    /// The key's value and data type.
    Value { value: f64, data_type: String },
}

impl KeyReading {
    fn read(smc: &mut SMC, key: &str) -> Self {
        let info = match smc.cached_key_info(str_to_fourcc(key)) {
            Ok(info) => info,
            Err("SMC key not found") => return Self::Absent,
            Err(error) => return Self::Failed(error),
        };
        match smc.read_value(key) {
            Ok(value) => Self::Value {
                value,
                data_type: fourcc_to_string(info.data_type),
            },
            Err(error) => Self::Failed(error),
        }
    }

    /// The value, when it is one the getters would average.
    fn in_range(&self) -> Option<f64> {
        match self {
            Self::Value { value, .. } if PLAUSIBLE_TEMP_C.contains(value) => Some(*value),
            _ => None,
        }
    }

    /// One static key per line.
    fn describe(&self) -> String {
        match self {
            Self::Absent => "absent".to_string(),
            Self::Failed(error) => format!("present, read failed: {error}"),
            Self::Value { value, data_type } => {
                let verdict = if PLAUSIBLE_TEMP_C.contains(value) {
                    "in range"
                } else {
                    "OUT OF RANGE"
                };
                format!("present type={data_type:?} {value:.2} C {verdict}")
            }
        }
    }

    /// Compact form for the discovered-key lists: `!` marks a value outside
    /// the range, `?` a key that could not be read.
    fn compact(&self, key: &str) -> String {
        match self {
            Self::Absent => format!("{key}=absent?"),
            Self::Failed(_) => format!("{key}=?"),
            Self::Value { value, .. } if PLAUSIBLE_TEMP_C.contains(value) => {
                format!("{key}={value:.1}")
            }
            Self::Value { value, .. } => format!("{key}={value:.1}!"),
        }
    }
}

fn mean(values: &[f64]) -> Option<f64> {
    (!values.is_empty()).then(|| values.iter().sum::<f64>() / values.len() as f64)
}

fn celsius(value: Option<f64>) -> String {
    value.map_or("None".to_string(), |v| format!("{v:.2} C"))
}

/// Read `keys` and append one line per key under `title`. Returns the
/// in-range values.
fn static_section(smc: &mut SMC, out: &mut String, title: &str, keys: &[&str]) -> Vec<f64> {
    let _ = writeln!(out, "# static {title} keys ({})", keys.join(" "));
    let mut in_range = Vec::new();
    for key in keys {
        let reading = KeyReading::read(smc, key);
        let _ = writeln!(out, "{key}\t{}", reading.describe());
        in_range.extend(reading.in_range());
    }
    in_range
}

/// Read every discovered key of one category and append them on one line.
/// Returns the in-range values.
fn discovered_section(smc: &mut SMC, out: &mut String, title: &str, keys: &[String]) -> Vec<f64> {
    let readings: Vec<(String, KeyReading)> = keys
        .iter()
        .map(|key| (key.clone(), KeyReading::read(smc, key)))
        .collect();
    let in_range: Vec<f64> = readings.iter().filter_map(|(_, r)| r.in_range()).collect();
    let listed: Vec<String> = readings.iter().map(|(key, r)| r.compact(key)).collect();
    let _ = writeln!(
        out,
        "# discovered {title} keys: {} found, {} in range: {}",
        keys.len(),
        in_range.len(),
        listed.join(" ")
    );
    in_range
}

/// Which path a rail's temperature takes, from the in-range readings of its
/// static and discovered keys, next to what the getter returned.
fn rail_line(
    title: &str,
    static_count: usize,
    static_in_range: &[f64],
    discovered_in_range: &[f64],
    getter: &str,
    returned: Option<f64>,
) -> String {
    let path = if !static_in_range.is_empty() {
        format!(
            "static keys ({} of {static_count} in range, mean {})",
            static_in_range.len(),
            celsius(mean(static_in_range))
        )
    } else if !discovered_in_range.is_empty() {
        format!(
            "discovered keys (no static key in range; {} discovered in range, mean {})",
            discovered_in_range.len(),
            celsius(mean(discovered_in_range))
        )
    } else {
        "none (no static or discovered key in range)".to_string()
    };
    format!("# {title} rail: {path}; {getter} = {}", celsius(returned))
}

/// The SMC temperature key inventory of this machine, one `#`-prefixed
/// section per question, ready to print next to the Energy Model diagnostic.
pub(crate) fn temperature_key_report(smc: &mut SMC) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "# SMC temperature keys (in range = {} to {} C)",
        PLAUSIBLE_TEMP_C.start(),
        PLAUSIBLE_TEMP_C.end()
    );

    let cpu_static = static_section(smc, &mut out, "CPU", &CPU_STATIC_TEMP_KEYS);
    let gpu_static = static_section(smc, &mut out, "GPU", &GPU_STATIC_TEMP_KEYS);

    let scan = smc.scan_temperature_keys();
    let _ = writeln!(
        out,
        "# scan_temperature_keys: {} CPU keys, {} GPU keys; scanned_keys={} total_keys={} used_sorted_range={}",
        scan.cpu_keys.len(),
        scan.gpu_keys.len(),
        scan.scanned_keys,
        scan.total_keys,
        scan.used_sorted_range
    );
    let _ = writeln!(
        out,
        "# discovered values in C; ! = outside the range, ? = unreadable"
    );
    let cpu_discovered = discovered_section(smc, &mut out, "CPU", &scan.cpu_keys);
    let gpu_discovered = discovered_section(smc, &mut out, "GPU", &scan.gpu_keys);

    let cpu = smc.get_cpu_temperature();
    let gpu = smc.get_gpu_temperature();
    let _ = writeln!(
        out,
        "{}",
        rail_line(
            "CPU",
            CPU_STATIC_TEMP_KEYS.len(),
            &cpu_static,
            &cpu_discovered,
            "get_cpu_temperature",
            cpu
        )
    );
    let _ = writeln!(
        out,
        "{}",
        rail_line(
            "GPU",
            GPU_STATIC_TEMP_KEYS.len(),
            &gpu_static,
            &gpu_discovered,
            "get_gpu_temperature",
            gpu
        )
    );
    out
}
