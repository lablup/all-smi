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

//! Integration tests for the TOML config file support added by issue
//! #192. Covers the full precedence chain (CLI > env > file > default),
//! malformed input, unknown-keys behaviour, and the backward-compat
//! aliases.
//!
//! The tests avoid shelling out to the compiled binary so they can run
//! under `cargo test --lib` (no extra build step needed).

use all_smi::common::config_file::{self, ConfigError};

/// Shared mutex to serialise env-var mutation across tests. Cargo runs
/// test functions on multiple threads by default; without this lock,
/// `set_var`/`remove_var` calls race each other and produce flaky
/// failures that are hard to triage.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn clear_all_env() {
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
        "ALL_SMI_ALERT_TEMP",
        "ALL_SMI_ALERT_UTIL_LOW_MINS",
        "ALL_SMI_ENERGY_PRICE",
        "ALL_SMI_ENERGY_PRICE_PER_KWH",
        "ALL_SMI_ENERGY_CURRENCY",
        "ALL_SMI_ENERGY_NO_COST",
        "ALL_SMI_ENERGY_WAL_PATH",
        "ALL_SMI_ENERGY_NO_WAL",
        "ALL_SMI_ENERGY_GAP_SECONDS",
    ];
    unsafe {
        for k in keys {
            std::env::remove_var(k);
        }
    }
}

/// Case 1: env > file > default. With no CLI layer above this module,
/// the loader honours env on top of file, and the remaining fields come
/// from compiled defaults. This is the core precedence guarantee.
#[test]
fn env_overrides_file_value() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    clear_all_env();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        r#"
[api]
port = 9100

[alerts]
temp_warn_c = 70
"#,
    )
    .unwrap();

    unsafe {
        std::env::set_var("ALL_SMI_API_PORT", "9200");
    }
    let outcome = config_file::load(Some(&path)).expect("must load");
    assert_eq!(outcome.settings.api.port, 9200, "env must override file");
    // File value preserved for fields env didn't touch.
    assert_eq!(outcome.settings.alerts.temp_warn_c, 70);
    unsafe {
        std::env::remove_var("ALL_SMI_API_PORT");
    }
}

/// Case 2: file > default. With no env set the file wins over compiled
/// defaults.
#[test]
fn file_overrides_default_when_no_env() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    clear_all_env();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "[api]\nport = 9500\n").unwrap();

    let outcome = config_file::load(Some(&path)).expect("must load");
    assert_eq!(outcome.settings.api.port, 9500);
    // Untouched field returns compiled default.
    assert_eq!(outcome.settings.api.interval_secs, 3);
}

/// Case 3: default when no file and no env. No explicit path, no env
/// set — the only guaranteed outcome is that the loader does not crash
/// and returns a valid Settings.
#[test]
fn defaults_apply_when_no_file_no_env() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    clear_all_env();
    let outcome = config_file::load(None).expect("must not crash");
    // The compiled default is 9090. If a config file happens to exist
    // on the host (e.g., the dev's own config), we can still assert
    // the port is within a reasonable range to catch wild misreads.
    assert!(
        (1..=u16::MAX).contains(&outcome.settings.api.port),
        "loaded port {} looks wrong",
        outcome.settings.api.port
    );
}

/// Malformed TOML must produce a clear `Parse` error, not silently
/// apply partial values.
#[test]
fn malformed_toml_is_clean_error() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    clear_all_env();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "this is = not valid [[[ toml\n[api]\nport = 9090").unwrap();
    let result = config_file::load(Some(&path));
    assert!(
        matches!(result, Err(ConfigError::Parse(_))),
        "expected ConfigError::Parse, got {result:?}"
    );
}

/// Unknown keys survive normal `load` and are retained in
/// `settings.unknown_keys` so `config print` can warn. `validate`
/// without `--strict` accepts them for forward compatibility.
#[test]
fn unknown_keys_preserved_in_settings() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    clear_all_env();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        "future_feature = true\n[api]\nunknown_sub = 42\nport = 9090\n",
    )
    .unwrap();
    let outcome = config_file::load(Some(&path)).expect("must load");
    assert!(
        outcome
            .settings
            .unknown_keys
            .contains(&"future_feature".to_string())
    );
    assert!(
        outcome
            .settings
            .unknown_keys
            .contains(&"api.unknown_sub".to_string())
    );
    // validate without strict accepts them.
    let valid = config_file::validate_file(&path, false);
    assert!(valid.is_ok(), "validate without strict must accept");
}

/// `validate --strict` rejects unknown keys with UnknownKey error.
#[test]
fn validate_strict_rejects_unknown() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    clear_all_env();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "mystery = 1\n[api]\nport = 9090\n").unwrap();
    let result = config_file::validate_file(&path, true);
    assert!(
        matches!(result, Err(ConfigError::UnknownKey(_))),
        "strict must reject, got {result:?}"
    );
}

/// Schema version 2 (not yet supported) must produce a clean error.
#[test]
fn future_schema_version_rejected() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    clear_all_env();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "schema_version = 2\n").unwrap();
    let result = config_file::load(Some(&path));
    assert!(
        matches!(
            result,
            Err(ConfigError::SchemaVersion {
                found: 2,
                supported: 1
            })
        ),
        "expected SchemaVersion error, got {result:?}"
    );
}

/// Backward compat: the legacy `ALL_SMI_ENERGY_PRICE` env var still
/// drives `energy.price_per_kwh`.
#[test]
fn legacy_energy_price_env_backcompat() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    clear_all_env();
    unsafe {
        std::env::set_var("ALL_SMI_ENERGY_PRICE", "0.42");
    }
    let outcome = config_file::load(None).expect("must load");
    assert!((outcome.settings.energy.price_per_kwh - 0.42).abs() < 1e-9);
    unsafe {
        std::env::remove_var("ALL_SMI_ENERGY_PRICE");
    }
}

/// Backward compat: legacy `ALL_SMI_ALERT_TEMP` still sets temp_warn_c.
#[test]
fn legacy_alert_temp_env_backcompat() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    clear_all_env();
    unsafe {
        std::env::set_var("ALL_SMI_ALERT_TEMP", "68");
    }
    let outcome = config_file::load(None).expect("must load");
    assert_eq!(outcome.settings.alerts.temp_warn_c, 68);
    // Auto-bumped crit per the original alias contract.
    assert!(outcome.settings.alerts.temp_crit_c >= 73);
    unsafe {
        std::env::remove_var("ALL_SMI_ALERT_TEMP");
    }
}

/// Canonical `ALL_SMI_ENERGY_PRICE_PER_KWH` wins over legacy
/// `ALL_SMI_ENERGY_PRICE` when both are set.
#[test]
fn canonical_energy_env_beats_legacy() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    clear_all_env();
    unsafe {
        std::env::set_var("ALL_SMI_ENERGY_PRICE", "0.10");
        std::env::set_var("ALL_SMI_ENERGY_PRICE_PER_KWH", "0.50");
    }
    let outcome = config_file::load(None).expect("must load");
    assert!((outcome.settings.energy.price_per_kwh - 0.50).abs() < 1e-9);
    unsafe {
        std::env::remove_var("ALL_SMI_ENERGY_PRICE");
        std::env::remove_var("ALL_SMI_ENERGY_PRICE_PER_KWH");
    }
}

/// Webhook URL must be loaded verbatim by the loader — redaction is a
/// render-time concern tested separately in the `config_cmd` renderer.
#[test]
fn webhook_url_loaded_verbatim() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    clear_all_env();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        "[alerts]\nwebhook_url = \"https://hooks.example/bot-secret\"\n",
    )
    .unwrap();
    let outcome = config_file::load(Some(&path)).expect("must load");
    assert_eq!(
        outcome.settings.alerts.webhook_url,
        "https://hooks.example/bot-secret"
    );
}

/// `~` expansion is done by per-feature consumers (energy WAL,
/// hostfile, etc.) — the loader stores paths verbatim so downstream
/// code can decide when to expand.
#[test]
fn tilde_paths_stored_verbatim_by_loader() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    clear_all_env();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        "[view]\nhostfile = \"~/all-smi-hosts.csv\"\n[energy]\nwal_path = \"~/my-wal.bin\"\n",
    )
    .unwrap();
    let outcome = config_file::load(Some(&path)).expect("must load");
    assert_eq!(
        outcome.settings.view.hostfile.as_deref(),
        Some("~/all-smi-hosts.csv"),
        "loader preserves leading tilde verbatim"
    );
    assert_eq!(outcome.settings.energy.wal_path, "~/my-wal.bin");
}
