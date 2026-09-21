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

use crate::parsing::common::sanitize_label_name;
use std::collections::HashMap;
use std::sync::LazyLock;

/// Power of the physical board a device sits on, in watts, as a bare number
/// (`"42.80"`).
///
/// Carried on every device of a board that exposes several devices, such as
/// the four dies of a Rebellions ATOM Max card, whose tool reports one power
/// figure per board and repeats it on every die. The board's power is counted
/// exactly once, in `power_consumption` of one of its devices; the others
/// read unavailable, so summing `power_consumption` over a host gives the
/// real draw. This key keeps the board value visible on the rows that no
/// longer show power. A board with a single device does not carry it: that
/// device's `power_consumption` already is the board value.
///
/// snake_case on purpose, so the key a local reader writes and the key the
/// remote parser stores are the same string (the same pattern as
/// `power_limit_max`). The value travels between nodes as the
/// `all_smi_gpu_card_power_watts` gauge rather than as a label: it is a live
/// reading, so it is listed in [`VOLATILE_DETAIL_KEYS`] and never reaches the
/// `all_smi_gpu_info` label set. The key must not be `power` or `power_draw`:
/// those spellings belong to per-device power readings, and a board figure
/// under them reads as one power figure per die (issue #431 removed the dead
/// generic NPU exporter that turned them into per-die series, but the
/// spelling distinction stays load-bearing).
pub const CARD_POWER_WATTS_DETAIL_KEY: &str = "card_power_watts";

/// Current PCIe link generation of a device, as a bare decimal number
/// (`"4"`) with no `Gen` prefix and no formatting.
///
/// Written by the NVIDIA NVML reader and by the Linux AMD plugin's live
/// link reading, both through [`insert_pcie_details`]. The value travels as
/// the `all_smi_gpu_pcie_gen_current` gauge rather than as a label: it is a
/// moving reading on AMD, whose links retrain with the power state, so it
/// is listed in [`VOLATILE_DETAIL_KEYS`]. On NVIDIA the reading is the link
/// state seen when the reader initialised (it lives in the startup-cached
/// static detail map, the same contract `pcie_gen_max` and
/// `power_limit_current` already carry). The value must be a bare number:
/// the exporter parses the whole value with `parse::<f64>()`, so a
/// formatted value such as `"Gen4"` silently omits the gauge.
pub const PCIE_GEN_CURRENT_DETAIL_KEY: &str = "pcie_gen_current";

/// Current PCIe link width of a device, as a bare decimal number (`"16"`)
/// with no `x` prefix.
///
/// Same writer and same freshness contract as [`PCIE_GEN_CURRENT_DETAIL_KEY`],
/// travelling as the `all_smi_gpu_pcie_width_current` gauge. The value must
/// not carry the legacy `"x16"` spelling: `parse::<f64>()` rejects it and
/// the gauge would be omitted even though the reading arrived.
pub const PCIE_WIDTH_CURRENT_DETAIL_KEY: &str = "pcie_width_current";

/// Current memory clock of a device, as a bare decimal number of MHz
/// (`"1249"`).
///
/// Named to pair with the static `clock_memory_max`, whose gauge
/// (`all_smi_gpu_clock_memory_max_mhz`) reports NVIDIA's maximum. This key
/// holds the live reading, written by the Linux AMD plugin on every poll
/// and travelling as the `all_smi_gpu_clock_memory_current_mhz` gauge, so
/// it is listed in [`VOLATILE_DETAIL_KEYS`]. The value must be a bare
/// number: the gauge is omitted for anything `parse::<f64>()` rejects.
pub const CLOCK_MEMORY_CURRENT_DETAIL_KEY: &str = "clock_memory_current";

/// Legacy `detail` key every reader used before `GpuInfo::fan_speed_rpm`
/// existed, and still writes alongside it.
///
/// Written by the Linux AMD plugin, AMD ADL, the Intel sysfs reader and the
/// Intel Level Zero backend, all from the same clamped tachometer value
/// they assign to `GpuInfo::fan_speed_rpm`. It lives here rather than in
/// `api::metrics::gpu` because the registry matches by key and cannot
/// import from `api` without inverting the module dependency.
///
/// The reading travels as the `all_smi_gpu_fan_speed_rpm` gauge, which
/// reads the typed field first and falls back to this string, so the key is
/// listed in [`VOLATILE_DETAIL_KEYS`] and never reaches the
/// `all_smi_gpu_info` label set. The string stays in `detail`: snapshots
/// and the cross-reader overwrite guard in `intel_gpu_level_zero::apply_fan`
/// still depend on it. The Level Zero exception is a duty-cycle-only
/// percentage (`"40%"`, with `fan_speed_rpm` left `None`), which has no
/// series of its own and stays in `detail` for the TUI and the snapshot
/// writers.
pub const FAN_SPEED_DETAIL_KEY: &str = "Fan Speed";

/// Detail keys whose value is a continuously varying measurement, and which
/// therefore must not become labels on `all_smi_gpu_info`.
///
/// Prometheus identifies a series by its full label set. `all_smi_gpu_info`
/// exports every `detail` entry as a label, so a key whose value changes
/// between polls starts a brand new series on each scrape and leaves the
/// previous one stale. Series and index cardinality then scale with the
/// number of scrapes rather than with the number of devices, range queries
/// and `label_values` return thousands of dead series, and `group_left` joins
/// over a range fragment across them.
///
/// This list is the single place a reader registers such a key. A key belongs
/// here when its value is a measurement that moves on its own: a power,
/// current, voltage, temperature, clock or byte count. A key does not belong
/// here when it holds a discrete state (`Status`, `Performance State`), a
/// settable limit or mode (`power_limit_max`, `ecc_mode_current`), or a
/// provenance string (`Metrics Source`, `Source: *`): those are identity, and
/// a change in them is a change a dashboard wants to see. The taxonomy has one
/// edge: link generation and width are discrete, like `Performance State`,
/// which the registry keeps as a label, but an AMD link changes on every
/// idle-to-load transition, so [`PCIE_GEN_CURRENT_DETAIL_KEY`] and
/// [`PCIE_WIDTH_CURRENT_DETAIL_KEY`] are registered as moving values. The
/// carve-out is
/// narrower than it looks, though: a Tenstorrent status register
/// (`pcie_status`, `eth_status0`, `eth_status1`, `ddr_status`) is discrete in
/// the sense that it holds a register word, but its raw hex dump is not
/// identity an operator joins on, and each one already ships as the label
/// value of its own `all_smi_tenstorrent_*` series, so registering it costs
/// the identity series nothing. The rule the Tenstorrent batch applies: a key
/// that has its own `all_smi_tenstorrent_*` series and is not stable device
/// identity does not belong in the `all_smi_gpu_info` label set.
///
/// Registering a key removes its only route onto the wire unless the same
/// reading is already published as a dedicated series, so every entry below
/// names the series that carries it, or says why it needs none:
///
/// * `card_power_watts` (Rebellions): `all_smi_gpu_card_power_watts`.
/// * `pcie_gen_current`, `pcie_width_current` (NVIDIA, AMD): the matching
///   `all_smi_gpu_pcie_gen_current` / `all_smi_gpu_pcie_width_current`
///   gauges. Both readers write the pair through [`insert_pcie_details`],
///   NVIDIA at reader initialisation and the Linux AMD plugin live.
/// * `Fan Speed` (AMD, Intel): `all_smi_gpu_fan_speed_rpm`, which reads the
///   typed `GpuInfo::fan_speed_rpm` field first and falls back to this
///   string. The exception is the Level Zero duty cycle: a device reporting
///   only a percentage has no fan series, and that reading stays in
///   `detail` for the TUI and the snapshot writers (#434 covers giving it
///   one). The entry also catches the snake_case `fan_speed` key the
///   Tenstorrent reader writes, through the sanitizer, which already ships
///   as its own `all_smi_tenstorrent_*` series.
/// * `clock_memory_current` (AMD): `all_smi_gpu_clock_memory_current_mhz`.
/// * `voltage`, `current`, `asic_temperature`, `vreg_temperature`,
///   `inlet_temperature`, `aiclk_mhz`, `arcclk_mhz`, `axiclk_mhz`
///   (Tenstorrent): the matching `all_smi_tenstorrent_*` gauges.
/// * `faults`, `throttler`, `arc0_health`, `arc3_health`, `pcie_status`,
///   `eth_status0`, `eth_status1`, `ddr_status`, `fan_speed`, `fan_rpm`,
///   `heartbeat` (Tenstorrent): the matching `all_smi_tenstorrent_*` series
///   — the two ethernet statuses and PCIe status as the label value of
///   `all_smi_tenstorrent_eth_status_info` / `pcie_status_info`, the rest as
///   their own gauge or counter. The `fan_speed` entry also catches the
///   legacy Title Case `Fan Speed` key the AMD / Intel readers still write,
///   through the sanitizer: that label value churned with the fan, and the
///   tachometer reading already ships as `all_smi_gpu_fan_speed_rpm` (the
///   typed field, or the exporter's legacy detail fallback), so dropping the
///   label costs the wire only a churning display string. A duty-cycle-only
///   Level Zero percentage keeps no series of its own and stays in `detail`
///   for the TUI and the snapshot writers.
/// * `combined_power_mw` (Apple Silicon): `all_smi_combined_power_watts`,
///   which reads this very key out of `detail`. Filtering removes labels
///   only, never `detail` entries, which is what keeps that gauge alive.
/// * `cpu_temperature` (Apple Silicon): `all_smi_cpu_temperature_celsius`.
/// * `gpu_temperature` (Apple Silicon): `all_smi_gpu_temperature_celsius`.
/// * `frequency` (Furiosa): `all_smi_gpu_frequency_mhz`.
/// * `Current Power` (Gaudi, Google TPU): `all_smi_gpu_power_consumption_watts`.
/// * `Used Memory` (Gaudi, Google TPU): `all_smi_gpu_memory_used_bytes`.
/// * `HLO Queue Size`, `HLO Exec Mean`, `HLO Exec P50`, `HLO Exec P90`,
///   `HLO Exec P95`, `HLO Exec P99.9` (Google TPU): the matching
///   `all_smi_tpu_hlo_*` gauges. That exporter reads these very strings out
///   of `detail` and parses the leading number itself, so it keeps working
///   with the unit suffix the reader writes (`"125.5 µs"`).
/// * `power_utilization_raw` (AWS Neuron): no series, and it needs none. It
///   is the raw `stats/power/utilization` line rather than a reading, nothing
///   parses it, and its second field is a sampling timestamp that advances on
///   its own, so it started a new series on every scrape even on an idle
///   device. Filtering removes labels only, so the entry stays in `detail`
///   for the TUI and the snapshot writers, and issue #434 covers giving its
///   three utilization floats a series if that is ever wanted.
///
/// The Title Case entries are matched through `sanitize_label_name` (see
/// [`is_volatile_detail_key`]); they are spelled here as their readers write
/// them so a reader author can find the key by grepping for the string they
/// typed.
pub const VOLATILE_DETAIL_KEYS: &[&str] = &[
    CARD_POWER_WATTS_DETAIL_KEY,
    "voltage",
    "current",
    "asic_temperature",
    "vreg_temperature",
    "inlet_temperature",
    "aiclk_mhz",
    "arcclk_mhz",
    "axiclk_mhz",
    "faults",
    "throttler",
    "arc0_health",
    "arc3_health",
    "pcie_status",
    "eth_status0",
    "eth_status1",
    "ddr_status",
    "fan_speed",
    "fan_rpm",
    "heartbeat",
    "combined_power_mw",
    "cpu_temperature",
    "gpu_temperature",
    "frequency",
    "Current Power",
    "Used Memory",
    "HLO Queue Size",
    "HLO Exec Mean",
    "HLO Exec P50",
    "HLO Exec P90",
    "HLO Exec P95",
    "HLO Exec P99.9",
    "power_utilization_raw",
    PCIE_GEN_CURRENT_DETAIL_KEY,
    PCIE_WIDTH_CURRENT_DETAIL_KEY,
    FAN_SPEED_DETAIL_KEY,
    CLOCK_MEMORY_CURRENT_DETAIL_KEY,
];

/// [`VOLATILE_DETAIL_KEYS`] as the label names they sanitize to, computed
/// once rather than on every detail entry of every device of every scrape.
static VOLATILE_LABEL_NAMES: LazyLock<Vec<String>> = LazyLock::new(|| {
    VOLATILE_DETAIL_KEYS
        .iter()
        .map(|key| sanitize_label_name(key))
        .collect()
});

/// Whether `key` names a continuously varying measurement, and so must be
/// kept out of the `all_smi_gpu_info` label set.
///
/// A key matches when it equals a [`VOLATILE_DETAIL_KEYS`] entry verbatim, or
/// when `sanitize_label_name` maps the two to the same label name. Sanitizing
/// both sides is what makes the Title Case entries (`Current Power`,
/// `Used Memory`) match the label they would have produced, and what stops a
/// respelling of a registered key from slipping a live reading back onto the
/// identity series.
///
/// It is not a spelling-independent guard. `sanitize_label_name` lowercases
/// and replaces every non-alphanumeric character with `_`, so `"AI Clock"`
/// becomes `"ai_clock"` and does not match the entry `aiclk_mhz`. The
/// Tenstorrent reader writes the snake_case keys its own exporter reads, and
/// that agreement, not this function, is what keeps its clocks off the
/// identity series.
///
/// It also matches whole keys only, so a key a reader builds at runtime
/// cannot be registered by spelling. The Intel GPU readers write one
/// `format!`-built entry per engine class and per clock domain
/// (`"Engine: render"`, `"Frequency: gpu (L0)"`), which no fixed entry here
/// can name; issue #434 covers giving those readings a series and deciding
/// how the registry should express a key family.
pub fn is_volatile_detail_key(key: &str) -> bool {
    if VOLATILE_DETAIL_KEYS.contains(&key) {
        return true;
    }
    let label = sanitize_label_name(key);
    VOLATILE_LABEL_NAMES.contains(&label)
}

/// The board power carried under [`CARD_POWER_WATTS_DETAIL_KEY`], or `None`
/// when the key is absent or does not hold a finite, non-negative number.
///
/// The value can arrive from a remote node's `all_smi_gpu_info` labels, so
/// it is validated here rather than trusted.
pub fn card_power_watts(detail: &HashMap<String, String>) -> Option<f64> {
    detail
        .get(CARD_POWER_WATTS_DETAIL_KEY)?
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|watts| watts.is_finite() && *watts >= 0.0)
}

/// Write the four PCIe link readings into `detail` in the exporter's
/// snake_case bare-number convention.
///
/// Writes `pcie_gen_current`, `pcie_width_current`, `pcie_gen_max` and
/// `pcie_width_max` as bare decimal numbers (`"4"`, `"16"` — no `Gen` or
/// `x` prefix, the same convention the NVIDIA reader already used for the
/// two maximums), omits any key whose value is `None`, and writes no Title
/// Case key. The current pair is what the `all_smi_gpu_pcie_gen_current` /
/// `all_smi_gpu_pcie_width_current` gauges read out of `detail`, and the
/// maximums stay identity labels, so the key spellings here and at the
/// exporter must not drift; the gauge lookups use
/// [`PCIE_GEN_CURRENT_DETAIL_KEY`] / [`PCIE_WIDTH_CURRENT_DETAIL_KEY`] so
/// the contract is compiler-checked instead of string-matched.
///
/// This lives in `detail_keys.rs` rather than inside the NVIDIA reader
/// because its callers sit behind disjoint `cfg` gates: the NVIDIA reader
/// on every platform, and the Linux AMD plugin, which is a separate cdylib
/// crate (`crates/all-smi-amd-plugin`) that depends on `all-smi` and cannot
/// call a `pub(crate)` item. Taking plain `Option<u32>` is what makes the
/// helper testable without an NVML or amdgpu device.
pub fn insert_pcie_details(
    detail: &mut HashMap<String, String>,
    gen_current: Option<u32>,
    width_current: Option<u32>,
    gen_max: Option<u32>,
    width_max: Option<u32>,
) {
    if let Some(link_gen) = gen_current {
        detail.insert(
            PCIE_GEN_CURRENT_DETAIL_KEY.to_string(),
            link_gen.to_string(),
        );
    }
    if let Some(link_width) = width_current {
        detail.insert(
            PCIE_WIDTH_CURRENT_DETAIL_KEY.to_string(),
            link_width.to_string(),
        );
    }
    if let Some(max_gen) = gen_max {
        detail.insert("pcie_gen_max".to_string(), max_gen.to_string());
    }
    if let Some(max_width) = width_max {
        detail.insert("pcie_width_max".to_string(), max_width.to_string());
    }
}

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

    /// Every registered key is recognised on its own, so a regression that
    /// drops one entry fails on that entry rather than hiding behind a
    /// neighbour.
    #[test]
    fn every_registered_key_is_volatile() {
        for key in VOLATILE_DETAIL_KEYS {
            assert!(
                is_volatile_detail_key(key),
                "{key} fell out of the registry"
            );
        }
    }

    /// The Title Case entries are matched through the label name they would
    /// have produced, which is the whole reason both sides are sanitized.
    #[test]
    fn title_case_entries_match_through_the_sanitizer() {
        assert!(is_volatile_detail_key("Current Power"));
        assert!(is_volatile_detail_key("current_power"));
        assert!(is_volatile_detail_key("Used Memory"));
        assert!(is_volatile_detail_key("used_memory"));
        // Same key, different spelling of the separator.
        assert!(is_volatile_detail_key("Current-Power"));
    }

    /// The Tenstorrent `fan_speed` entry also catches the legacy Title Case
    /// `Fan Speed` key the AMD / Intel readers write, through the sanitizer.
    /// That is intended: the label value churned with the fan, and the
    /// tachometer reading already ships as the structured
    /// `all_smi_gpu_fan_speed_rpm` series (the typed field, or the exporter's
    /// legacy detail fallback), so removing the label costs the wire only a
    /// churning display string.
    #[test]
    fn fan_speed_registration_also_filters_the_legacy_title_case_key() {
        assert!(is_volatile_detail_key("fan_speed"));
        assert!(is_volatile_detail_key("Fan Speed"));
        assert!(is_volatile_detail_key("FAN_SPEED"));
    }

    /// Identity, discrete state and settable limits stay on the series: they
    /// are what an operator joins and filters on.
    #[test]
    fn identity_and_state_keys_are_not_volatile() {
        for key in [
            "Status",
            "Performance State",
            "power_limit_current",
            "power_limit_max",
            "ecc_mode_current",
            "mig_mode_current",
            "Metrics Source",
            "Source: Power",
            "serial_number",
            "lib_name",
            "Utilization",
            // A near miss: the sanitizer is not a fuzzy matcher, so the old
            // Tenstorrent spelling is not caught here. The reader writing
            // `aiclk_mhz` is what keeps it off the label set.
            "AI Clock",
        ] {
            assert!(!is_volatile_detail_key(key), "{key} must stay a label");
        }
    }

    #[test]
    fn card_power_accepts_only_finite_non_negative_watts() {
        let with = |value: &str| {
            HashMap::from([(CARD_POWER_WATTS_DETAIL_KEY.to_string(), value.to_string())])
        };
        assert_eq!(card_power_watts(&with("42.80")), Some(42.8));
        assert_eq!(card_power_watts(&with(" 0 ")), Some(0.0));
        for rejected in ["-1", "-0.5", "NaN", "inf", "abc", ""] {
            assert_eq!(card_power_watts(&with(rejected)), None, "{rejected:?}");
        }
        assert_eq!(card_power_watts(&HashMap::new()), None);
    }

    /// The writer must produce the exporter's snake_case bare-number
    /// convention exactly: the four keys the gauges and the identity labels
    /// read, with no leftover Title Case key.
    #[test]
    fn insert_pcie_details_writes_the_snake_case_bare_number_convention() {
        let mut detail = HashMap::new();
        insert_pcie_details(&mut detail, Some(4), Some(16), Some(5), Some(16));
        assert_eq!(
            detail,
            HashMap::from([
                (PCIE_GEN_CURRENT_DETAIL_KEY.to_string(), "4".to_string()),
                (PCIE_WIDTH_CURRENT_DETAIL_KEY.to_string(), "16".to_string()),
                ("pcie_gen_max".to_string(), "5".to_string()),
                ("pcie_width_max".to_string(), "16".to_string()),
            ])
        );
        assert!(!detail.contains_key("PCIe Generation"));
        assert!(!detail.contains_key("PCIe Width"));
    }

    /// A `None` argument leaves its key absent rather than writing `"0"`:
    /// absence is this exporter's "no data" convention, and a link that
    /// reported nothing must not look like a Gen-0 link.
    #[test]
    fn insert_pcie_details_omits_none_arguments() {
        let mut detail = HashMap::new();
        insert_pcie_details(&mut detail, Some(4), None, None, Some(16));
        assert_eq!(detail[PCIE_GEN_CURRENT_DETAIL_KEY], "4");
        assert_eq!(detail["pcie_width_max"], "16");
        assert_eq!(detail.len(), 2);

        let mut empty = HashMap::new();
        insert_pcie_details(&mut empty, None, None, None, None);
        assert!(empty.is_empty());
    }
}
