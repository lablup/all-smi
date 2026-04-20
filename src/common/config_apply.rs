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

//! File-level merge helpers: project a parsed [`RawConfig`] onto a
//! mutable [`Settings`]. Broken out so `config_file.rs` stays under the
//! 500-line soft cap and each section-level projection is easy to read
//! and test in isolation.

use crate::common::config_file::{ConfigError, Settings};
use crate::common::config_schema::{RawConfig, SocketSetting};

/// Entry point called by [`crate::common::config_file::load`] and
/// [`crate::common::config_file::validate_file`].
pub(super) fn apply_file(raw: &RawConfig, settings: &mut Settings) -> Result<(), ConfigError> {
    apply_file_general(raw, settings)?;
    apply_file_local(raw, settings);
    apply_file_view(raw, settings);
    apply_file_api(raw, settings);
    apply_file_alerts(raw, settings);
    apply_file_energy(raw, settings)?;
    apply_file_display(raw, settings);
    apply_file_record(raw, settings)?;
    apply_file_snapshot(raw, settings)?;
    Ok(())
}

fn apply_file_general(raw: &RawConfig, settings: &mut Settings) -> Result<(), ConfigError> {
    let Some(g) = &raw.general else {
        return Ok(());
    };
    if let Some(m) = &g.default_mode {
        match m.as_str() {
            "local" | "view" | "api" => settings.general.default_mode = m.clone(),
            _ => {
                return Err(ConfigError::Semantic(format!(
                    "general.default_mode must be one of local/view/api, got `{m}`"
                )));
            }
        }
    }
    if let Some(t) = &g.theme {
        match t.as_str() {
            "auto" | "light" | "dark" | "high-contrast" | "mono" => {
                settings.general.theme = t.clone();
            }
            _ => {
                return Err(ConfigError::Semantic(format!(
                    "general.theme must be auto/light/dark/high-contrast/mono, got `{t}`"
                )));
            }
        }
    }
    if let Some(l) = &g.locale {
        settings.general.locale = l.clone();
    }
    Ok(())
}

fn apply_file_local(raw: &RawConfig, settings: &mut Settings) {
    if let Some(l) = &raw.local
        && l.interval_secs.is_some()
    {
        settings.local.interval_secs = l.interval_secs;
    }
}

fn apply_file_view(raw: &RawConfig, settings: &mut Settings) {
    let Some(v) = &raw.view else { return };
    if v.hostfile.is_some() {
        settings.view.hostfile = v.hostfile.clone();
    }
    if let Some(hosts) = &v.hosts {
        settings.view.hosts = hosts.clone();
    }
    if v.interval_secs.is_some() {
        settings.view.interval_secs = v.interval_secs;
    }
    // Agentless SSH transport keys (issue #194). Each `.is_some()`
    // guard lets the CLI layer distinguish "config set this" from
    // "config left it unset".
    if let Some(ssh) = &v.ssh {
        settings.view.ssh = ssh.clone();
    }
    if v.ssh_hostfile.is_some() {
        settings.view.ssh_hostfile = v.ssh_hostfile.clone();
    }
    if v.ssh_key.is_some() {
        settings.view.ssh_key = v.ssh_key.clone();
    }
    if v.ssh_config.is_some() {
        settings.view.ssh_config = v.ssh_config.clone();
    }
    if v.ssh_strict_host_key.is_some() {
        settings.view.ssh_strict_host_key = v.ssh_strict_host_key.clone();
    }
    if v.ssh_timeout_secs.is_some() {
        settings.view.ssh_timeout_secs = v.ssh_timeout_secs;
    }
    if v.ssh_fallback.is_some() {
        settings.view.ssh_fallback = v.ssh_fallback.clone();
    }
    if v.ssh_known_hosts.is_some() {
        settings.view.ssh_known_hosts = v.ssh_known_hosts.clone();
    }
    if v.ssh_concurrency.is_some() {
        settings.view.ssh_concurrency = v.ssh_concurrency;
    }
}

fn apply_file_api(raw: &RawConfig, settings: &mut Settings) {
    let Some(a) = &raw.api else { return };
    if let Some(p) = a.port {
        settings.api.port = p;
    }
    match &a.socket {
        SocketSetting::Unset => {}
        s => settings.api.socket = s.clone(),
    }
    if let Some(p) = a.processes {
        settings.api.processes = p;
    }
    if let Some(i) = a.interval_secs {
        settings.api.interval_secs = i;
    }
}

fn apply_file_alerts(raw: &RawConfig, settings: &mut Settings) {
    let Some(al) = &raw.alerts else { return };
    if let Some(t) = al.temp_warn_c {
        settings.alerts.temp_warn_c = t;
    }
    if let Some(t) = al.temp_crit_c {
        settings.alerts.temp_crit_c = t;
    }
    if let Some(v) = al.util_idle_pct {
        settings.alerts.util_idle_pct = v;
    }
    if let Some(v) = al.util_idle_warn_mins {
        settings.alerts.util_idle_warn_mins = v;
    }
    if let Some(v) = al.hysteresis_c {
        settings.alerts.hysteresis_c = v;
    }
    if let Some(v) = al.bell_on_critical {
        settings.alerts.bell_on_critical = v;
    }
    if let Some(url) = &al.webhook_url {
        settings.alerts.webhook_url = url.clone();
    }
    if let Some(v) = al.power_crit_w {
        settings.alerts.power_crit_w = v;
    }
    // `enabled` currently acts as a no-op; wire-up pending a future
    // global disable switch on the alerter.
    let _ = al.enabled;
}

fn apply_file_energy(raw: &RawConfig, settings: &mut Settings) -> Result<(), ConfigError> {
    let Some(e) = &raw.energy else {
        return Ok(());
    };
    if let Some(p) = e.price_per_kwh {
        if !p.is_finite() || p < 0.0 {
            return Err(ConfigError::Semantic(format!(
                "energy.price_per_kwh must be a non-negative finite number, got {p}"
            )));
        }
        settings.energy.price_per_kwh = p;
    }
    if let Some(c) = &e.currency {
        settings.energy.currency = c.clone();
    }
    if let Some(s) = e.show_cost {
        settings.energy.show_cost = s;
    }
    if let Some(w) = &e.wal_path {
        settings.energy.wal_path = w.clone();
    }
    if let Some(g) = e.gap_interpolate_seconds {
        if !(1..=3600).contains(&g) {
            return Err(ConfigError::Semantic(format!(
                "energy.gap_interpolate_seconds must be in [1, 3600], got {g}"
            )));
        }
        settings.energy.gap_interpolate_seconds = g;
    }
    if let Some(w) = e.wal_enabled {
        settings.energy.wal_enabled = w;
    }
    Ok(())
}

fn apply_file_display(raw: &RawConfig, settings: &mut Settings) {
    let Some(d) = &raw.display else { return };
    if let Some(c) = &d.color_scheme {
        settings.display.color_scheme = c.clone();
    }
    if let Some(g) = &d.gauge_style {
        settings.display.gauge_style = g.clone();
    }
    if let Some(v) = d.show_led_grid {
        settings.display.show_led_grid = v;
    }
}

fn apply_file_record(raw: &RawConfig, settings: &mut Settings) -> Result<(), ConfigError> {
    let Some(r) = &raw.record else {
        return Ok(());
    };
    if let Some(o) = &r.output_dir {
        settings.record.output_dir = o.clone();
    }
    if let Some(c) = &r.compress {
        match c.as_str() {
            "zstd" | "gzip" | "none" => settings.record.compress = c.clone(),
            _ => {
                return Err(ConfigError::Semantic(format!(
                    "record.compress must be zstd/gzip/none, got `{c}`"
                )));
            }
        }
    }
    Ok(())
}

fn apply_file_snapshot(raw: &RawConfig, settings: &mut Settings) -> Result<(), ConfigError> {
    let Some(s) = &raw.snapshot else {
        return Ok(());
    };
    if let Some(f) = &s.default_format {
        match f.as_str() {
            "json" | "csv" | "prometheus" => settings.snapshot.default_format = f.clone(),
            _ => {
                return Err(ConfigError::Semantic(format!(
                    "snapshot.default_format must be json/csv/prometheus, got `{f}`"
                )));
            }
        }
    }
    if let Some(v) = s.default_pretty {
        settings.snapshot.default_pretty = v;
    }
    Ok(())
}
