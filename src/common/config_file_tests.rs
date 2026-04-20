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

//! Unit tests for `common::config_file`. Kept in a sibling file so the
//! implementation stays under the 500-line soft limit.

use super::*;

/// Shared mutex used by env-var tests. Cargo runs tests on multiple
/// threads by default; without this lock the `set_var`/`remove_var`
/// calls from concurrent tests race each other.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn clear_env() {
    let keys = [
        "ALL_SMI_GENERAL_DEFAULT_MODE",
        "ALL_SMI_GENERAL_THEME",
        "ALL_SMI_GENERAL_LOCALE",
        "ALL_SMI_LOCAL_INTERVAL_SECS",
        "ALL_SMI_VIEW_HOSTFILE",
        "ALL_SMI_VIEW_HOSTS",
        "ALL_SMI_VIEW_INTERVAL_SECS",
        "ALL_SMI_API_PORT",
        "ALL_SMI_API_SOCKET",
        "ALL_SMI_API_PROCESSES",
        "ALL_SMI_API_INTERVAL_SECS",
        "ALL_SMI_ALERTS_TEMP_WARN_C",
        "ALL_SMI_ALERTS_TEMP_CRIT_C",
        "ALL_SMI_ALERTS_UTIL_IDLE_PCT",
        "ALL_SMI_ALERTS_UTIL_IDLE_WARN_MINS",
        "ALL_SMI_ALERTS_HYSTERESIS_C",
        "ALL_SMI_ALERTS_BELL_ON_CRITICAL",
        "ALL_SMI_ALERTS_WEBHOOK_URL",
        "ALL_SMI_ALERTS_POWER_CRIT_W",
        "ALL_SMI_ALERT_TEMP",
        "ALL_SMI_ALERT_UTIL_LOW_MINS",
        "ALL_SMI_ENERGY_PRICE",
        "ALL_SMI_ENERGY_PRICE_PER_KWH",
        "ALL_SMI_ENERGY_CURRENCY",
        "ALL_SMI_ENERGY_NO_COST",
        "ALL_SMI_ENERGY_SHOW_COST",
        "ALL_SMI_ENERGY_WAL_PATH",
        "ALL_SMI_ENERGY_NO_WAL",
        "ALL_SMI_ENERGY_WAL_ENABLED",
        "ALL_SMI_ENERGY_GAP_SECONDS",
        "ALL_SMI_ENERGY_GAP_INTERPOLATE_SECONDS",
        "ALL_SMI_DISPLAY_COLOR_SCHEME",
        "ALL_SMI_DISPLAY_GAUGE_STYLE",
        "ALL_SMI_DISPLAY_SHOW_LED_GRID",
        "ALL_SMI_RECORD_OUTPUT_DIR",
        "ALL_SMI_RECORD_COMPRESS",
        "ALL_SMI_SNAPSHOT_DEFAULT_FORMAT",
        "ALL_SMI_SNAPSHOT_DEFAULT_PRETTY",
    ];
    unsafe {
        for k in keys {
            std::env::remove_var(k);
        }
    }
}

#[test]
fn settings_default_has_expected_values() {
    let s = Settings::default();
    assert_eq!(s.api.port, 9090);
    assert_eq!(s.api.interval_secs, 3);
    assert!(!s.api.processes);
    assert_eq!(s.general.default_mode, "local");
    assert_eq!(s.general.theme, "auto");
    assert_eq!(s.general.locale, "en");
    assert_eq!(s.display.color_scheme, "default");
    assert_eq!(s.snapshot.default_format, "json");
    assert!(s.snapshot.default_pretty);
}

#[test]
fn parse_toml_accepts_valid_schema() {
    let toml_str = r#"
schema_version = 1

[api]
port = 9091
processes = true

[alerts]
temp_warn_c = 75
temp_crit_c = 95
"#;
    let (raw, unknown) = parse_toml(toml_str).expect("valid toml must parse");
    assert_eq!(raw.schema_version, Some(1));
    assert_eq!(raw.api.as_ref().and_then(|a| a.port), Some(9091));
    assert_eq!(raw.alerts.as_ref().and_then(|a| a.temp_warn_c), Some(75));
    assert!(
        unknown.is_empty(),
        "no unknown keys in a valid doc, got: {unknown:?}"
    );
}

#[test]
fn parse_toml_reports_unknown_top_level_keys() {
    let toml_str = r#"
schema_version = 1
future_feature = "whatever"

[api]
port = 9091
"#;
    let (_raw, unknown) = parse_toml(toml_str).expect("must parse");
    assert!(unknown.iter().any(|k| k == "future_feature"));
}

#[test]
fn parse_toml_reports_unknown_section_subkeys() {
    let toml_str = r#"
[api]
port = 9091
mystery_key = 42
"#;
    let (_raw, unknown) = parse_toml(toml_str).expect("must parse");
    assert!(
        unknown.iter().any(|k| k == "api.mystery_key"),
        "unknown keys: {unknown:?}"
    );
}

#[test]
fn parse_toml_fails_on_invalid_syntax() {
    let bad = "this is [ not valid toml";
    let result = parse_toml(bad);
    assert!(
        matches!(result, Err(ConfigError::Parse(_))),
        "got {result:?}"
    );
}

#[test]
fn load_with_missing_path_returns_error() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    clear_env();
    let path = std::path::Path::new("/nonexistent/all-smi/fake-test-config.toml");
    let result = load(Some(path));
    assert!(
        matches!(result, Err(ConfigError::Io { .. })),
        "explicit missing path must fail, got {result:?}"
    );
}

#[test]
fn load_without_path_uses_defaults_when_no_file() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    clear_env();
    // We pass `None` to `load`. If no candidate file exists we get
    // compiled defaults; if one happens to exist on the test host we
    // still get a valid outcome (just not the exact default).
    let outcome = load(None).expect("load without explicit must succeed");
    // The loader never crashes even if no file is present. That is
    // the only guarantee this test can make without a filesystem
    // fixture because candidate_config_paths is host-dependent.
    assert!(outcome.settings.api.port >= 1);
}

#[test]
fn load_with_explicit_file_applies_values() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    clear_env();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        r#"
schema_version = 1

[api]
port = 9191
processes = true
interval_secs = 7

[alerts]
temp_warn_c = 70
temp_crit_c = 95
util_idle_warn_mins = 20

[energy]
price_per_kwh = 0.18
currency = "EUR"
"#,
    )
    .unwrap();

    let outcome = load(Some(&path)).expect("must load");
    let s = outcome.settings;
    assert_eq!(s.api.port, 9191);
    assert!(s.api.processes);
    assert_eq!(s.api.interval_secs, 7);
    assert_eq!(s.alerts.temp_warn_c, 70);
    assert_eq!(s.alerts.temp_crit_c, 95);
    assert_eq!(s.alerts.util_idle_warn_mins, 20);
    assert!((s.energy.price_per_kwh - 0.18).abs() < 1e-9);
    assert_eq!(s.energy.currency, "EUR");
    assert_eq!(s.source_path.as_ref(), Some(&path));
}

#[test]
fn schema_version_mismatch_rejected() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    clear_env();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "schema_version = 2\n").unwrap();
    let result = load(Some(&path));
    assert!(
        matches!(
            result,
            Err(ConfigError::SchemaVersion {
                found: 2,
                supported: 1
            })
        ),
        "schema_version=2 must be rejected, got {result:?}"
    );
}

#[test]
fn semantic_error_on_invalid_theme() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    clear_env();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "[general]\ntheme = \"rainbow\"\n").unwrap();
    let result = load(Some(&path));
    assert!(
        matches!(result, Err(ConfigError::Semantic(_))),
        "invalid theme must fail, got {result:?}"
    );
}

#[test]
fn semantic_error_on_negative_price() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    clear_env();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "[energy]\nprice_per_kwh = -5.0\n").unwrap();
    let result = load(Some(&path));
    assert!(matches!(result, Err(ConfigError::Semantic(_))));
}

#[test]
fn validate_file_strict_rejects_unknown() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    clear_env();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "future_field = true\n[api]\nport = 9090\n").unwrap();
    let strict = validate_file(&path, true);
    assert!(matches!(strict, Err(ConfigError::UnknownKey(_))));
    let lenient = validate_file(&path, false);
    assert!(lenient.is_ok(), "non-strict must accept unknown keys");
}

#[test]
fn env_override_wins_over_file() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    clear_env();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "[api]\nport = 9090\n").unwrap();
    unsafe {
        std::env::set_var("ALL_SMI_API_PORT", "9191");
    }
    let outcome = load(Some(&path)).expect("must load");
    assert_eq!(
        outcome.settings.api.port, 9191,
        "env var must override file value"
    );
    unsafe {
        std::env::remove_var("ALL_SMI_API_PORT");
    }
}

#[test]
fn legacy_alert_env_alias_still_works() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    clear_env();
    unsafe {
        std::env::set_var("ALL_SMI_ALERT_TEMP", "77");
        std::env::set_var("ALL_SMI_ALERT_UTIL_LOW_MINS", "25");
    }
    let outcome = load(None).expect("must load");
    assert_eq!(outcome.settings.alerts.temp_warn_c, 77);
    assert_eq!(outcome.settings.alerts.util_idle_warn_mins, 25);
    // Auto-bumped crit: preserved backward-compat semantics.
    assert!(outcome.settings.alerts.temp_crit_c >= outcome.settings.alerts.temp_warn_c + 5);
    unsafe {
        std::env::remove_var("ALL_SMI_ALERT_TEMP");
        std::env::remove_var("ALL_SMI_ALERT_UTIL_LOW_MINS");
    }
}

#[test]
fn legacy_energy_env_alias_still_works() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    clear_env();
    unsafe {
        std::env::set_var("ALL_SMI_ENERGY_PRICE", "0.25");
        std::env::set_var("ALL_SMI_ENERGY_CURRENCY", "KRW");
    }
    let outcome = load(None).expect("must load");
    assert!((outcome.settings.energy.price_per_kwh - 0.25).abs() < 1e-9);
    assert_eq!(outcome.settings.energy.currency, "KRW");
    unsafe {
        std::env::remove_var("ALL_SMI_ENERGY_PRICE");
        std::env::remove_var("ALL_SMI_ENERGY_CURRENCY");
    }
}

#[test]
fn canonical_energy_env_wins_over_legacy() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    clear_env();
    unsafe {
        std::env::set_var("ALL_SMI_ENERGY_PRICE", "0.10"); // legacy
        std::env::set_var("ALL_SMI_ENERGY_PRICE_PER_KWH", "0.30"); // canonical
    }
    let outcome = load(None).expect("must load");
    assert!((outcome.settings.energy.price_per_kwh - 0.30).abs() < 1e-9);
    unsafe {
        std::env::remove_var("ALL_SMI_ENERGY_PRICE");
        std::env::remove_var("ALL_SMI_ENERGY_PRICE_PER_KWH");
    }
}

#[test]
fn unknown_keys_captured_in_settings() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    clear_env();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        "mystery_top = 1\n[api]\nport = 9090\nmystery_sub = 2\n",
    )
    .unwrap();
    let outcome = load(Some(&path)).expect("must load");
    assert!(
        outcome
            .settings
            .unknown_keys
            .iter()
            .any(|k| k == "mystery_top")
    );
    assert!(
        outcome
            .settings
            .unknown_keys
            .iter()
            .any(|k| k == "api.mystery_sub")
    );
}

#[test]
fn api_socket_as_bool_and_path() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    clear_env();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "[api]\nsocket = true\n").unwrap();
    let outcome = load(Some(&path)).expect("must load");
    assert!(matches!(
        outcome.settings.api.socket,
        SocketSetting::Bool(true)
    ));

    std::fs::write(&path, "[api]\nsocket = \"/tmp/test.sock\"\n").unwrap();
    let outcome = load(Some(&path)).expect("must load");
    match &outcome.settings.api.socket {
        SocketSetting::Path(p) => assert_eq!(p, "/tmp/test.sock"),
        other => panic!("expected Path, got {other:?}"),
    }
}

#[test]
fn malformed_toml_returns_parse_error() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    clear_env();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "this is not = valid = toml\n[[[").unwrap();
    let result = load(Some(&path));
    assert!(
        matches!(result, Err(ConfigError::Parse(_))),
        "malformed TOML must return ConfigError::Parse, got {result:?}"
    );
}
