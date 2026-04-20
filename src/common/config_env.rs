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

//! Environment-variable overlay for [`crate::common::config_file::Settings`]
//! (issue #192).
//!
//! Canonical naming: `ALL_SMI_<SECTION>_<KEY>` upper-snake. Legacy
//! aliases introduced by earlier issues are also recognised here so
//! existing operator muscle memory continues to work:
//!
//! - `ALL_SMI_ALERT_TEMP` → `alerts.temp_warn_c` (issue #186)
//! - `ALL_SMI_ALERT_UTIL_LOW_MINS` → `alerts.util_idle_warn_mins`
//!   (issue #186)
//! - `ALL_SMI_ENERGY_PRICE` → `energy.price_per_kwh` (issue #191)
//! - `ALL_SMI_ENERGY_NO_COST` → `energy.show_cost = false` (#191)
//! - `ALL_SMI_ENERGY_WAL_PATH` → `energy.wal_path` (#191)
//! - `ALL_SMI_ENERGY_NO_WAL` → `energy.wal_enabled = false` (#191)
//! - `ALL_SMI_ENERGY_GAP_SECONDS` → `energy.gap_interpolate_seconds`
//!   (#191)
//! - `ALL_SMI_ENERGY_CURRENCY` → `energy.currency` (#191)
//!
//! Invalid values are silently dropped so a typo cannot brick the TUI;
//! surface warnings via the returned `warnings` vector which `config
//! print` shows to the user.

use crate::common::config_file::{Settings, SocketSetting};

/// Apply environment-variable overrides on top of `settings`.
pub(super) fn apply_env(settings: &mut Settings, warnings: &mut Vec<String>) {
    apply_env_general(settings, warnings);
    apply_env_local(settings);
    apply_env_view(settings);
    apply_env_api(settings);
    apply_env_alerts(settings);
    apply_env_energy(settings);
    apply_env_display(settings);
    apply_env_record(settings, warnings);
    apply_env_snapshot(settings, warnings);
}

fn apply_env_general(settings: &mut Settings, warnings: &mut Vec<String>) {
    use std::env;
    if let Ok(v) = env::var("ALL_SMI_GENERAL_DEFAULT_MODE") {
        match v.as_str() {
            "local" | "view" | "api" => settings.general.default_mode = v,
            other => warnings.push(format!(
                "env ALL_SMI_GENERAL_DEFAULT_MODE: ignored invalid value `{other}`"
            )),
        }
    }
    if let Ok(v) = env::var("ALL_SMI_GENERAL_THEME") {
        match v.as_str() {
            "auto" | "light" | "dark" | "high-contrast" | "mono" => settings.general.theme = v,
            other => warnings.push(format!(
                "env ALL_SMI_GENERAL_THEME: ignored invalid value `{other}`"
            )),
        }
    }
    if let Ok(v) = env::var("ALL_SMI_GENERAL_LOCALE") {
        settings.general.locale = v;
    }
}

fn apply_env_local(settings: &mut Settings) {
    use std::env;
    if let Ok(v) = env::var("ALL_SMI_LOCAL_INTERVAL_SECS")
        && let Ok(n) = v.parse::<u64>()
    {
        settings.local.interval_secs = Some(n);
    }
}

fn apply_env_view(settings: &mut Settings) {
    use std::env;
    if let Ok(v) = env::var("ALL_SMI_VIEW_HOSTFILE")
        && !v.trim().is_empty()
    {
        settings.view.hostfile = Some(v);
    }
    if let Ok(v) = env::var("ALL_SMI_VIEW_HOSTS")
        && !v.trim().is_empty()
    {
        settings.view.hosts = v
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
    }
    if let Ok(v) = env::var("ALL_SMI_VIEW_INTERVAL_SECS")
        && let Ok(n) = v.parse::<u64>()
    {
        settings.view.interval_secs = Some(n);
    }
}

fn apply_env_api(settings: &mut Settings) {
    use std::env;
    if let Ok(v) = env::var("ALL_SMI_API_PORT")
        && let Ok(n) = v.parse::<u16>()
    {
        settings.api.port = n;
    }
    if let Ok(v) = env::var("ALL_SMI_API_SOCKET") {
        if v.eq_ignore_ascii_case("false") || v == "0" {
            settings.api.socket = SocketSetting::Bool(false);
        } else if v.eq_ignore_ascii_case("true") || v == "1" {
            settings.api.socket = SocketSetting::Bool(true);
        } else {
            settings.api.socket = SocketSetting::Path(v);
        }
    }
    if let Ok(v) = env::var("ALL_SMI_API_PROCESSES") {
        settings.api.processes = matches!(v.as_str(), "1" | "true" | "TRUE" | "True");
    }
    if let Ok(v) = env::var("ALL_SMI_API_INTERVAL_SECS")
        && let Ok(n) = v.parse::<u64>()
    {
        settings.api.interval_secs = n;
    }
}

/// Apply both the canonical `ALL_SMI_ALERTS_*` names and the legacy
/// `ALL_SMI_ALERT_*` aliases introduced by issue #186.
///
/// Application order: **legacy first, canonical second.** This matches
/// the pattern used by [`apply_env_energy`]: when an operator has both
/// the old name and the new name set (e.g. during a rollout), the
/// canonical one wins. Without this ordering a stale shell dotfile
/// carrying `ALL_SMI_ALERT_TEMP` would silently clobber a freshly-set
/// `ALL_SMI_ALERTS_TEMP_WARN_C`, making the documented canonical name
/// a no-op.
fn apply_env_alerts(settings: &mut Settings) {
    use std::env;

    // Legacy aliases (issue #186) first. The old alias auto-bumped
    // crit to keep `crit > warn`; reproduce that behaviour so
    // backward-compat scripts stay faithful.
    if let Ok(v) = env::var("ALL_SMI_ALERT_TEMP")
        && let Ok(n) = v.parse::<u32>()
    {
        settings.alerts.temp_warn_c = n;
        if settings.alerts.temp_crit_c < n + 5 {
            settings.alerts.temp_crit_c = n + 10;
        }
    }
    if let Ok(v) = env::var("ALL_SMI_ALERT_UTIL_LOW_MINS")
        && let Ok(n) = v.parse::<u32>()
    {
        settings.alerts.util_idle_warn_mins = n;
    }

    // Canonical names second — they override the legacy values when
    // both are present.
    if let Ok(v) = env::var("ALL_SMI_ALERTS_TEMP_WARN_C")
        && let Ok(n) = v.parse::<u32>()
    {
        settings.alerts.temp_warn_c = n;
    }
    if let Ok(v) = env::var("ALL_SMI_ALERTS_TEMP_CRIT_C")
        && let Ok(n) = v.parse::<u32>()
    {
        settings.alerts.temp_crit_c = n;
    }
    if let Ok(v) = env::var("ALL_SMI_ALERTS_UTIL_IDLE_PCT")
        && let Ok(n) = v.parse::<u32>()
    {
        settings.alerts.util_idle_pct = n;
    }
    if let Ok(v) = env::var("ALL_SMI_ALERTS_UTIL_IDLE_WARN_MINS")
        && let Ok(n) = v.parse::<u32>()
    {
        settings.alerts.util_idle_warn_mins = n;
    }
    if let Ok(v) = env::var("ALL_SMI_ALERTS_HYSTERESIS_C")
        && let Ok(n) = v.parse::<u32>()
    {
        settings.alerts.hysteresis_c = n;
    }
    if let Ok(v) = env::var("ALL_SMI_ALERTS_BELL_ON_CRITICAL") {
        settings.alerts.bell_on_critical = matches!(v.as_str(), "1" | "true" | "TRUE" | "True");
    }
    if let Ok(v) = env::var("ALL_SMI_ALERTS_WEBHOOK_URL") {
        settings.alerts.webhook_url = v;
    }
    if let Ok(v) = env::var("ALL_SMI_ALERTS_POWER_CRIT_W")
        && let Ok(n) = v.parse::<u32>()
    {
        settings.alerts.power_crit_w = n;
    }
}

fn apply_env_energy(settings: &mut Settings) {
    use std::env;
    // Delegate the existing legacy-alias handling to EnergyConfig.
    settings.energy = settings.energy.clone().with_env_overrides();
    // Canonical names introduced by issue #192 (additional to the legacy
    // aliases handled by `with_env_overrides`).
    if let Ok(v) = env::var("ALL_SMI_ENERGY_PRICE_PER_KWH")
        && let Ok(p) = v.parse::<f64>()
        && p.is_finite()
        && p >= 0.0
    {
        settings.energy.price_per_kwh = p;
    }
    if let Ok(v) = env::var("ALL_SMI_ENERGY_SHOW_COST") {
        settings.energy.show_cost = matches!(v.as_str(), "1" | "true" | "TRUE" | "True");
    }
    if let Ok(v) = env::var("ALL_SMI_ENERGY_WAL_ENABLED") {
        settings.energy.wal_enabled = matches!(v.as_str(), "1" | "true" | "TRUE" | "True");
    }
    if let Ok(v) = env::var("ALL_SMI_ENERGY_GAP_INTERPOLATE_SECONDS")
        && let Ok(n) = v.parse::<u64>()
        && (1..=3600).contains(&n)
    {
        settings.energy.gap_interpolate_seconds = n;
    }
}

fn apply_env_display(settings: &mut Settings) {
    use std::env;
    if let Ok(v) = env::var("ALL_SMI_DISPLAY_COLOR_SCHEME") {
        settings.display.color_scheme = v;
    }
    if let Ok(v) = env::var("ALL_SMI_DISPLAY_GAUGE_STYLE") {
        settings.display.gauge_style = v;
    }
    if let Ok(v) = env::var("ALL_SMI_DISPLAY_SHOW_LED_GRID") {
        settings.display.show_led_grid = matches!(v.as_str(), "1" | "true" | "TRUE" | "True");
    }
}

fn apply_env_record(settings: &mut Settings, warnings: &mut Vec<String>) {
    use std::env;
    if let Ok(v) = env::var("ALL_SMI_RECORD_OUTPUT_DIR")
        && !v.trim().is_empty()
    {
        settings.record.output_dir = v;
    }
    if let Ok(v) = env::var("ALL_SMI_RECORD_COMPRESS") {
        match v.as_str() {
            "zstd" | "gzip" | "none" => settings.record.compress = v,
            other => warnings.push(format!(
                "env ALL_SMI_RECORD_COMPRESS: ignored invalid value `{other}`"
            )),
        }
    }
}

fn apply_env_snapshot(settings: &mut Settings, warnings: &mut Vec<String>) {
    use std::env;
    if let Ok(v) = env::var("ALL_SMI_SNAPSHOT_DEFAULT_FORMAT") {
        match v.as_str() {
            "json" | "csv" | "prometheus" => settings.snapshot.default_format = v,
            other => warnings.push(format!(
                "env ALL_SMI_SNAPSHOT_DEFAULT_FORMAT: ignored invalid value `{other}`"
            )),
        }
    }
    if let Ok(v) = env::var("ALL_SMI_SNAPSHOT_DEFAULT_PRETTY") {
        settings.snapshot.default_pretty = matches!(v.as_str(), "1" | "true" | "TRUE" | "True");
    }
}
