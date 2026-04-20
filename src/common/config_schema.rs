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

//! TOML on-disk schema definitions for `config.toml` (issue #192).
//!
//! Kept separate from the runtime [`Settings`] struct so the serde-
//! derived layer stays a pure data projection of the file format while
//! [`crate::common::config_file`] owns the merge + validation logic.

use serde::{Deserialize, Serialize};

/// On-disk schema for `config.toml`. Fields are optional so omitted
/// keys silently fall through to compiled defaults. Unknown keys are
/// captured separately during parse for the `print`/`validate`
/// workflow.
#[derive(Default, Debug, Clone, Deserialize, Serialize)]
pub struct RawConfig {
    /// Schema version — `1` for the shape documented in issue #192.
    #[serde(default)]
    pub schema_version: Option<u32>,
    #[serde(default)]
    pub general: Option<GeneralSection>,
    #[serde(default)]
    pub local: Option<LocalSection>,
    #[serde(default)]
    pub view: Option<ViewSection>,
    #[serde(default)]
    pub api: Option<ApiSection>,
    #[serde(default)]
    pub alerts: Option<AlertsSection>,
    #[serde(default)]
    pub energy: Option<EnergySection>,
    #[serde(default)]
    pub display: Option<DisplaySection>,
    #[serde(default)]
    pub record: Option<RecordSection>,
    #[serde(default)]
    pub snapshot: Option<SnapshotSection>,
}

/// `[general]` section — cross-mode defaults.
#[derive(Default, Debug, Clone, Deserialize, Serialize)]
pub struct GeneralSection {
    pub default_mode: Option<String>,
    pub theme: Option<String>,
    pub locale: Option<String>,
}

/// `[local]` section — options for `all-smi local`.
#[derive(Default, Debug, Clone, Deserialize, Serialize)]
pub struct LocalSection {
    pub interval_secs: Option<u64>,
}

/// `[view]` section — options for `all-smi view` (remote mode).
#[derive(Default, Debug, Clone, Deserialize, Serialize)]
pub struct ViewSection {
    pub hostfile: Option<String>,
    pub hosts: Option<Vec<String>>,
    pub interval_secs: Option<u64>,
    // Agentless SSH transport (issue #194). A missing section produces
    // `None` which the CLI layer then ignores — the defaults baked into
    // `clap` still apply.
    pub ssh: Option<Vec<String>>,
    pub ssh_hostfile: Option<String>,
    pub ssh_key: Option<String>,
    pub ssh_config: Option<String>,
    pub ssh_strict_host_key: Option<String>,
    pub ssh_timeout_secs: Option<u64>,
    pub ssh_fallback: Option<String>,
    pub ssh_known_hosts: Option<String>,
    pub ssh_concurrency: Option<usize>,
}

/// `[api]` section — options for `all-smi api`.
#[derive(Default, Debug, Clone, Deserialize, Serialize)]
pub struct ApiSection {
    pub port: Option<u16>,
    /// Accept either `socket = false`/`true` or `socket = "/path"`. TOML's
    /// typed deserializer does not allow sum types natively; we route
    /// through an untagged enum to accept either shape.
    #[serde(default)]
    pub socket: SocketSetting,
    pub processes: Option<bool>,
    pub interval_secs: Option<u64>,
}

/// Tri-state representation of `[api].socket`.
#[derive(Default, Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum SocketSetting {
    /// Caller did not set the key — leave the existing value alone.
    #[default]
    Unset,
    /// `socket = true` means default platform socket path.
    Bool(bool),
    /// `socket = "/path"` means explicit socket path.
    Path(String),
}

/// `[alerts]` section — maps to [`crate::common::config::AlertConfig`].
#[derive(Default, Debug, Clone, Deserialize, Serialize)]
pub struct AlertsSection {
    pub enabled: Option<bool>,
    pub temp_warn_c: Option<u32>,
    pub temp_crit_c: Option<u32>,
    pub util_idle_pct: Option<u32>,
    pub util_idle_warn_mins: Option<u32>,
    pub hysteresis_c: Option<u32>,
    pub bell_on_critical: Option<bool>,
    pub webhook_url: Option<String>,
    pub power_crit_w: Option<u32>,
}

/// `[energy]` section — maps to [`crate::common::config::EnergyConfig`].
#[derive(Default, Debug, Clone, Deserialize, Serialize)]
pub struct EnergySection {
    pub price_per_kwh: Option<f64>,
    pub currency: Option<String>,
    pub show_cost: Option<bool>,
    pub wal_path: Option<String>,
    pub gap_interpolate_seconds: Option<u64>,
    pub wal_enabled: Option<bool>,
}

/// `[display]` section — TUI cosmetics.
#[derive(Default, Debug, Clone, Deserialize, Serialize)]
pub struct DisplaySection {
    pub color_scheme: Option<String>,
    pub gauge_style: Option<String>,
    pub show_led_grid: Option<bool>,
}

/// `[record]` section — defaults for `all-smi record`.
#[derive(Default, Debug, Clone, Deserialize, Serialize)]
pub struct RecordSection {
    pub output_dir: Option<String>,
    pub compress: Option<String>,
}

/// `[snapshot]` section — defaults for `all-smi snapshot`.
#[derive(Default, Debug, Clone, Deserialize, Serialize)]
pub struct SnapshotSection {
    pub default_format: Option<String>,
    pub default_pretty: Option<bool>,
}

/// Known top-level section names. Anything outside this set in the raw
/// TOML table is reported as "unknown".
pub const KNOWN_TOP_LEVEL: &[&str] = &[
    "schema_version",
    "general",
    "local",
    "view",
    "api",
    "alerts",
    "energy",
    "display",
    "record",
    "snapshot",
];
