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

//! Renderers for `all-smi config print`. Kept here so the command glue
//! and the schema stay co-located; serialization uses a minimal
//! hand-written projection that avoids polluting the `Settings` type
//! with `Serialize` derives.

use serde_json::{Value as JsonValue, json};
use std::fmt::Write;

use crate::common::config_file::{Settings, SocketSetting};

/// Convert a [`Settings`] into a TOML string that parses back into an
/// equivalent `Settings`. Redacts `webhook_url` unless `show_secrets`
/// is set.
pub fn render_toml(settings: &Settings, show_secrets: bool) -> std::io::Result<String> {
    let mut out = String::new();
    let webhook = if show_secrets || settings.alerts.webhook_url.is_empty() {
        settings.alerts.webhook_url.clone()
    } else {
        "<redacted>".to_string()
    };

    writeln!(
        &mut out,
        "schema_version = {}",
        crate::common::config_file::SUPPORTED_SCHEMA_VERSION
    )
    .ok();
    writeln!(&mut out).ok();

    writeln!(&mut out, "[general]").ok();
    writeln!(
        &mut out,
        "default_mode = \"{}\"",
        escape_toml(&settings.general.default_mode)
    )
    .ok();
    writeln!(
        &mut out,
        "theme = \"{}\"",
        escape_toml(&settings.general.theme)
    )
    .ok();
    writeln!(
        &mut out,
        "locale = \"{}\"",
        escape_toml(&settings.general.locale)
    )
    .ok();
    writeln!(&mut out).ok();

    writeln!(&mut out, "[local]").ok();
    if let Some(v) = settings.local.interval_secs {
        writeln!(&mut out, "interval_secs = {v}").ok();
    }
    writeln!(&mut out).ok();

    writeln!(&mut out, "[view]").ok();
    if let Some(h) = &settings.view.hostfile {
        writeln!(&mut out, "hostfile = \"{}\"", escape_toml(h)).ok();
    }
    if !settings.view.hosts.is_empty() {
        write!(&mut out, "hosts = [").ok();
        for (i, h) in settings.view.hosts.iter().enumerate() {
            if i > 0 {
                write!(&mut out, ", ").ok();
            }
            write!(&mut out, "\"{}\"", escape_toml(h)).ok();
        }
        writeln!(&mut out, "]").ok();
    }
    if let Some(v) = settings.view.interval_secs {
        writeln!(&mut out, "interval_secs = {v}").ok();
    }
    writeln!(&mut out).ok();

    writeln!(&mut out, "[api]").ok();
    writeln!(&mut out, "port = {}", settings.api.port).ok();
    write!(&mut out, "socket = ").ok();
    match &settings.api.socket {
        SocketSetting::Unset | SocketSetting::Bool(false) => writeln!(&mut out, "false").ok(),
        SocketSetting::Bool(true) => writeln!(&mut out, "true").ok(),
        SocketSetting::Path(p) => writeln!(&mut out, "\"{}\"", escape_toml(p)).ok(),
    };
    writeln!(&mut out, "processes = {}", settings.api.processes).ok();
    writeln!(&mut out, "interval_secs = {}", settings.api.interval_secs).ok();
    writeln!(&mut out).ok();

    writeln!(&mut out, "[alerts]").ok();
    writeln!(&mut out, "temp_warn_c = {}", settings.alerts.temp_warn_c).ok();
    writeln!(&mut out, "temp_crit_c = {}", settings.alerts.temp_crit_c).ok();
    writeln!(
        &mut out,
        "util_idle_pct = {}",
        settings.alerts.util_idle_pct
    )
    .ok();
    writeln!(
        &mut out,
        "util_idle_warn_mins = {}",
        settings.alerts.util_idle_warn_mins
    )
    .ok();
    writeln!(&mut out, "hysteresis_c = {}", settings.alerts.hysteresis_c).ok();
    writeln!(
        &mut out,
        "bell_on_critical = {}",
        settings.alerts.bell_on_critical
    )
    .ok();
    writeln!(&mut out, "power_crit_w = {}", settings.alerts.power_crit_w).ok();
    writeln!(&mut out, "webhook_url = \"{}\"", escape_toml(&webhook)).ok();
    writeln!(&mut out).ok();

    writeln!(&mut out, "[energy]").ok();
    writeln!(
        &mut out,
        "price_per_kwh = {}",
        format_float(settings.energy.price_per_kwh)
    )
    .ok();
    writeln!(
        &mut out,
        "currency = \"{}\"",
        escape_toml(&settings.energy.currency)
    )
    .ok();
    writeln!(&mut out, "show_cost = {}", settings.energy.show_cost).ok();
    writeln!(
        &mut out,
        "wal_path = \"{}\"",
        escape_toml(&settings.energy.wal_path)
    )
    .ok();
    writeln!(
        &mut out,
        "gap_interpolate_seconds = {}",
        settings.energy.gap_interpolate_seconds
    )
    .ok();
    writeln!(&mut out, "wal_enabled = {}", settings.energy.wal_enabled).ok();
    writeln!(&mut out).ok();

    writeln!(&mut out, "[display]").ok();
    writeln!(
        &mut out,
        "color_scheme = \"{}\"",
        escape_toml(&settings.display.color_scheme)
    )
    .ok();
    writeln!(
        &mut out,
        "gauge_style = \"{}\"",
        escape_toml(&settings.display.gauge_style)
    )
    .ok();
    writeln!(
        &mut out,
        "show_led_grid = {}",
        settings.display.show_led_grid
    )
    .ok();
    writeln!(&mut out).ok();

    writeln!(&mut out, "[record]").ok();
    writeln!(
        &mut out,
        "output_dir = \"{}\"",
        escape_toml(&settings.record.output_dir)
    )
    .ok();
    writeln!(
        &mut out,
        "compress = \"{}\"",
        escape_toml(&settings.record.compress)
    )
    .ok();
    writeln!(&mut out).ok();

    writeln!(&mut out, "[snapshot]").ok();
    writeln!(
        &mut out,
        "default_format = \"{}\"",
        escape_toml(&settings.snapshot.default_format)
    )
    .ok();
    writeln!(
        &mut out,
        "default_pretty = {}",
        settings.snapshot.default_pretty
    )
    .ok();

    Ok(out)
}

/// JSON projection for `config print --format json`. Mirrors the TOML
/// renderer structure but flattened so downstream `jq` pipelines are
/// predictable.
pub fn render_json(settings: &Settings, show_secrets: bool) -> std::io::Result<String> {
    let webhook = if show_secrets || settings.alerts.webhook_url.is_empty() {
        JsonValue::String(settings.alerts.webhook_url.clone())
    } else {
        JsonValue::String("<redacted>".to_string())
    };

    let socket_value = match &settings.api.socket {
        SocketSetting::Unset | SocketSetting::Bool(false) => JsonValue::Bool(false),
        SocketSetting::Bool(true) => JsonValue::Bool(true),
        SocketSetting::Path(p) => JsonValue::String(p.clone()),
    };

    let doc = json!({
        "schema_version": crate::common::config_file::SUPPORTED_SCHEMA_VERSION,
        "general": {
            "default_mode": settings.general.default_mode,
            "theme": settings.general.theme,
            "locale": settings.general.locale,
        },
        "local": {
            "interval_secs": settings.local.interval_secs,
        },
        "view": {
            "hostfile": settings.view.hostfile,
            "hosts": settings.view.hosts,
            "interval_secs": settings.view.interval_secs,
        },
        "api": {
            "port": settings.api.port,
            "socket": socket_value,
            "processes": settings.api.processes,
            "interval_secs": settings.api.interval_secs,
        },
        "alerts": {
            "temp_warn_c": settings.alerts.temp_warn_c,
            "temp_crit_c": settings.alerts.temp_crit_c,
            "util_idle_pct": settings.alerts.util_idle_pct,
            "util_idle_warn_mins": settings.alerts.util_idle_warn_mins,
            "hysteresis_c": settings.alerts.hysteresis_c,
            "bell_on_critical": settings.alerts.bell_on_critical,
            "power_crit_w": settings.alerts.power_crit_w,
            "webhook_url": webhook,
        },
        "energy": {
            "price_per_kwh": settings.energy.price_per_kwh,
            "currency": settings.energy.currency,
            "show_cost": settings.energy.show_cost,
            "wal_path": settings.energy.wal_path,
            "gap_interpolate_seconds": settings.energy.gap_interpolate_seconds,
            "wal_enabled": settings.energy.wal_enabled,
        },
        "display": {
            "color_scheme": settings.display.color_scheme,
            "gauge_style": settings.display.gauge_style,
            "show_led_grid": settings.display.show_led_grid,
        },
        "record": {
            "output_dir": settings.record.output_dir,
            "compress": settings.record.compress,
        },
        "snapshot": {
            "default_format": settings.snapshot.default_format,
            "default_pretty": settings.snapshot.default_pretty,
        },
        "source_path": settings.source_path.as_ref().map(|p| p.display().to_string()),
    });
    serde_json::to_string_pretty(&doc)
        .map_err(|e| std::io::Error::other(format!("json render: {e}")))
}

fn escape_toml(s: &str) -> String {
    // Minimal escape for the characters forbidden in basic TOML strings
    // (we never render multi-line strings). Matches the subset produced
    // by the `toml` crate's own serializer for our value set.
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn format_float(f: f64) -> String {
    if f.is_finite() {
        // `{}` prints integers without a decimal point; TOML requires at
        // least one digit after `.` for a float. Force a decimal.
        let s = format!("{f}");
        if s.contains('.') { s } else { format!("{s}.0") }
    } else {
        // Non-finite floats are not valid TOML; emit 0 instead of
        // producing an unparseable file.
        "0.0".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_toml_round_trips_through_loader() {
        let mut s = Settings::default();
        s.api.port = 9191;
        s.alerts.temp_warn_c = 77;
        s.alerts.webhook_url = "https://hooks.example/bot".to_string();

        let toml_str = render_toml(&s, true).unwrap();
        let (raw, unknown) = crate::common::config_file::parse_toml(&toml_str).unwrap();
        assert!(unknown.is_empty(), "rendered toml should be clean");
        assert_eq!(raw.api.as_ref().and_then(|a| a.port), Some(9191));
        assert_eq!(raw.alerts.as_ref().and_then(|a| a.temp_warn_c), Some(77));
        assert_eq!(
            raw.alerts.as_ref().and_then(|a| a.webhook_url.clone()),
            Some("https://hooks.example/bot".to_string())
        );
    }

    #[test]
    fn render_toml_redacts_webhook_by_default() {
        let mut s = Settings::default();
        s.alerts.webhook_url = "https://hooks.example/bot-secret".to_string();
        let toml_str = render_toml(&s, false).unwrap();
        assert!(toml_str.contains("<redacted>"));
        assert!(!toml_str.contains("bot-secret"));
    }

    #[test]
    fn render_json_outputs_valid_json() {
        let s = Settings::default();
        let json_str = render_json(&s, false).unwrap();
        let _: JsonValue = serde_json::from_str(&json_str).expect("valid JSON");
    }

    #[test]
    fn render_json_redacts_webhook_by_default() {
        let mut s = Settings::default();
        s.alerts.webhook_url = "https://hooks.example/secret".to_string();
        let json_str = render_json(&s, false).unwrap();
        assert!(json_str.contains("<redacted>"));
        assert!(!json_str.contains("secret"));
        let json_str = render_json(&s, true).unwrap();
        assert!(json_str.contains("secret"));
    }

    #[test]
    fn format_float_preserves_dot() {
        assert_eq!(format_float(0.12), "0.12");
        assert_eq!(format_float(1.0), "1.0");
        assert_eq!(format_float(42.0), "42.0");
    }

    #[test]
    fn format_float_handles_non_finite() {
        assert_eq!(format_float(f64::NAN), "0.0");
        assert_eq!(format_float(f64::INFINITY), "0.0");
    }
}
