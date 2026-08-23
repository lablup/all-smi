// Copyright 2025 Lablup Inc., Jeongkyu Shin and DaeHyun Sung
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

//! Helpers for the shared `GpuInfo::detail` key conventions.
//!
//! Always compiled, on every platform. That is the point rather than an
//! accident: these keys are written by layers with disjoint `cfg` gates
//! (the vendor-neutral Windows DXGI/PDH layer, the Intel Level Zero
//! backend, the AMD ADL backend), so a helper living inside any one of
//! them cannot be called by the others. `note_metrics_source` previously
//! lived in `windows_gpu_perf`, which is unreachable from the Level Zero
//! backend on Linux, and that is how the Level Zero path came to assign
//! `Metrics Source` instead of appending to it.
//!
//! Being always compiled also means the Linux test runner exercises this,
//! which is the only runner this repository has.

use std::collections::HashMap;

/// Record that `source` contributed to this GPU's metrics.
///
/// `Metrics Source` is a human-readable composition of the layers that
/// produced a reading, in the order they ran. A Windows Intel GPU with
/// the full stack available reads `"WMI + DXGI + PDH + Level Zero
/// Sysman"`.
///
/// Appending rather than assigning is load-bearing. Each layer knows only
/// about itself, so a layer that *sets* the string erases the record of
/// everything beneath it. Idempotent, so repeated polls do not grow the
/// string.
//
// The binary crate re-declares these modules rather than importing the
// library, so a `pub` item with no compiled-in caller still reads as dead
// there. Every caller sits behind a per-OS or per-backend gate: the
// Windows DXGI/PDH and ADL layers, and the Level Zero backend.
#[cfg_attr(not(any(target_os = "windows", all_smi_level_zero)), allow(dead_code))]
pub fn note_metrics_source(detail: &mut HashMap<String, String>, source: &str) {
    let entry = detail.entry("Metrics Source".to_string()).or_default();
    if entry.is_empty() {
        *entry = source.to_string();
        return;
    }
    if entry.split(" + ").any(|part| part == source) {
        return;
    }
    entry.push_str(" + ");
    entry.push_str(source);
}

/// The subset of `fields` that no layer claimed, judged by their
/// `Source: <field>` keys.
///
/// A field counts as missing when its key is absent entirely (no layer
/// wrote it) or reads the sentinel `"unavailable"` (a layer looked and
/// found nothing). Order follows `fields`, so the caller controls how the
/// result reads in a message.
//
// Same reason as above; the only caller is the Windows Intel reader.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub fn missing_metric_sources<'a>(
    detail: &HashMap<String, String>,
    fields: &[&'a str],
) -> Vec<&'a str> {
    fields
        .iter()
        .copied()
        .filter(|field| {
            detail
                .get(&format!("Source: {field}"))
                .is_none_or(|source| source == "unavailable")
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_source_starts_clean_when_absent() {
        let mut detail = HashMap::new();
        note_metrics_source(&mut detail, "DXGI");
        assert_eq!(detail["Metrics Source"], "DXGI");
        note_metrics_source(&mut detail, "PDH");
        assert_eq!(detail["Metrics Source"], "DXGI + PDH");
    }

    #[test]
    fn appending_is_idempotent() {
        let mut detail = HashMap::new();
        for _ in 0..3 {
            note_metrics_source(&mut detail, "WMI");
            note_metrics_source(&mut detail, "DXGI");
        }
        assert_eq!(detail["Metrics Source"], "WMI + DXGI");
    }

    #[test]
    fn a_later_layer_never_erases_an_earlier_one() {
        // The regression this helper exists to prevent: the Level Zero
        // augmentation used to overwrite the string, losing DXGI and PDH
        // on a real Windows host.
        let mut detail = HashMap::new();
        note_metrics_source(&mut detail, "WMI");
        note_metrics_source(&mut detail, "DXGI");
        note_metrics_source(&mut detail, "PDH");
        note_metrics_source(&mut detail, "Level Zero Sysman");
        assert_eq!(
            detail["Metrics Source"],
            "WMI + DXGI + PDH + Level Zero Sysman"
        );
    }

    /// Deduplication is by part, not by substring: a layer whose name is a
    /// prefix of an already-recorded one still records separately.
    #[test]
    fn deduplication_compares_whole_parts() {
        let mut detail = HashMap::new();
        note_metrics_source(&mut detail, "Level Zero Sysman");
        note_metrics_source(&mut detail, "Level Zero");
        assert_eq!(detail["Metrics Source"], "Level Zero Sysman + Level Zero");
    }

    const METRIC_FIELDS: &[&str] = &["Temperature", "Power", "Frequency", "Utilization"];

    fn detail_with(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(field, source)| (format!("Source: {field}"), (*source).to_string()))
            .collect()
    }

    #[test]
    fn a_fully_sourced_device_is_missing_nothing() {
        let detail = detail_with(&[
            ("Temperature", "Level Zero Sysman"),
            ("Power", "Level Zero Sysman"),
            ("Frequency", "Level Zero Sysman"),
            ("Utilization", "PDH"),
        ]);
        assert!(missing_metric_sources(&detail, METRIC_FIELDS).is_empty());
    }

    #[test]
    fn the_unavailable_sentinel_counts_as_missing() {
        // The real Arc B390 shape: an Intel iGPU exposes no Sysman thermal
        // sensor, so temperature alone is absent.
        let detail = detail_with(&[
            ("Temperature", "unavailable"),
            ("Power", "Level Zero Sysman"),
            ("Frequency", "Level Zero Sysman"),
            ("Utilization", "PDH"),
        ]);
        assert_eq!(
            missing_metric_sources(&detail, METRIC_FIELDS),
            vec!["Temperature"]
        );
    }

    #[test]
    fn an_absent_key_also_counts_as_missing() {
        let detail = detail_with(&[("Utilization", "PDH")]);
        assert_eq!(
            missing_metric_sources(&detail, METRIC_FIELDS),
            vec!["Temperature", "Power", "Frequency"]
        );
    }

    #[test]
    fn the_result_follows_the_requested_order() {
        let detail = HashMap::new();
        assert_eq!(
            missing_metric_sources(&detail, &["Fan", "Power"]),
            vec!["Fan", "Power"]
        );
        assert_eq!(
            missing_metric_sources(&detail, &["Power", "Fan"]),
            vec!["Power", "Fan"]
        );
    }
}
