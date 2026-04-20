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

//! Threshold alerting with hysteresis.
//!
//! The alerter runs once per collection cycle, diffs the new state against
//! the per-device/per-rule state machines kept between ticks, and emits
//! [`AlertTransition`] records for any `ok → warn`, `warn → crit`, etc.
//!
//! The UI layer consumes transitions to:
//! - push a 5-second toast via the notification manager,
//! - flash the affected GPU card border,
//! - append to a ring buffer rendered by the `A` panel,
//! - optionally POST to a webhook (see `network/webhook.rs`).

use std::collections::HashMap;
use std::time::Instant;

use chrono::{DateTime, Local};

use crate::common::config::AlertConfig;
use crate::device::GpuInfo;

/// Alert severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AlertLevel {
    Ok,
    Warn,
    Crit,
}

impl AlertLevel {
    pub fn as_label(self) -> &'static str {
        match self {
            AlertLevel::Ok => "ok",
            AlertLevel::Warn => "warn",
            AlertLevel::Crit => "crit",
        }
    }
}

/// Per-rule identifier used as part of the state-machine key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RuleKind {
    /// Temperature crossing `temp_warn_c` / `temp_crit_c`.
    Temperature,
    /// Idle utilization sustained for `util_idle_warn_mins`.
    IdleUtilization,
    /// Power consumption exceeding `power_crit_w`.
    Power,
}

impl RuleKind {
    pub fn as_label(self) -> &'static str {
        match self {
            RuleKind::Temperature => "temperature",
            RuleKind::IdleUtilization => "idle_utilization",
            RuleKind::Power => "power",
        }
    }
}

/// Identifier of a single device/rule pair tracked by the alerter.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct RuleKey {
    device_id: String,
    rule: RuleKind,
}

/// State machine per-device per-rule. Hysteresis prevents flapping by
/// requiring the observed value to drop below `threshold - hysteresis_c`
/// before the state machine is allowed to leave `warn`/`crit`.
#[derive(Debug, Clone)]
struct RuleState {
    level: AlertLevel,
    /// First time we entered `warn`/`crit` during an idle-utilization sweep.
    idle_since: Option<Instant>,
}

impl Default for RuleState {
    fn default() -> Self {
        Self {
            level: AlertLevel::Ok,
            idle_since: None,
        }
    }
}

/// A single transition produced by [`Alerter::evaluate`].
///
/// The UI turns each transition into a toast, a border flash, a
/// ring-buffer entry and (optionally) a webhook POST.
#[derive(Debug, Clone)]
pub struct AlertTransition {
    pub timestamp: DateTime<Local>,
    pub host: String,
    pub gpu_index: Option<i32>,
    pub rule: RuleKind,
    pub from: AlertLevel,
    pub to: AlertLevel,
    pub value: f64,
    pub threshold: f64,
    pub message: String,
    /// Unique "card key" used by the renderer to decide which GPU tile
    /// should flash. For GPU alerts this is the UUID; for future CPU /
    /// memory alerts it can be extended by building a different key.
    #[allow(dead_code)] // Consumed by the binary-side border-flash renderer
    pub card_key: String,
}

/// The alerter owned by `AppState`. Reset between mode switches is safe
/// (construct a fresh [`Alerter::new`]).
#[derive(Debug, Clone, Default)]
pub struct Alerter {
    config: AlertConfig,
    states: HashMap<RuleKey, RuleState>,
    /// Active border-flash deadlines keyed by `card_key`. Render path reads
    /// this to decide if the border should blink.
    flashing: HashMap<String, Instant>,
}

impl Alerter {
    pub fn new(config: AlertConfig) -> Self {
        Self {
            config,
            states: HashMap::new(),
            flashing: HashMap::new(),
        }
    }

    /// Replace the active config. Existing state machines are kept so that
    /// a running hysteresis window is not lost on reload.
    #[allow(dead_code)] // Future config-reload wiring (issue #192)
    pub fn set_config(&mut self, config: AlertConfig) {
        self.config = config;
    }

    /// Borrow the currently active config. Used by the renderer to decide
    /// whether to play a bell and by tests to confirm defaults.
    pub fn config(&self) -> &AlertConfig {
        &self.config
    }

    /// Mark a card to flash for [`AlertConfig::flash_duration_secs`]
    /// seconds. Called internally on every transition; exposed for tests.
    pub fn mark_flash(&mut self, card_key: &str) {
        self.flashing.insert(
            card_key.to_string(),
            Instant::now() + std::time::Duration::from_secs(self.config.flash_duration_secs),
        );
    }

    /// Query whether a given card is currently flashing. The renderer
    /// alternates the border color at 1 Hz while this is true.
    pub fn is_flashing(&self, card_key: &str) -> bool {
        match self.flashing.get(card_key) {
            Some(deadline) => *deadline > Instant::now(),
            None => false,
        }
    }

    /// Run a single evaluation pass against the current GPU snapshot.
    /// Returns the set of transitions that the caller must forward to the
    /// notification manager / webhook queue.
    pub fn evaluate(&mut self, gpus: &[GpuInfo]) -> Vec<AlertTransition> {
        let mut transitions = Vec::new();
        let now = Instant::now();
        for gpu in gpus {
            self.evaluate_temperature(gpu, &mut transitions);
            self.evaluate_idle_utilization(gpu, now, &mut transitions);
            self.evaluate_power(gpu, &mut transitions);
        }
        // Garbage-collect expired flash deadlines so the map doesn't grow
        // without bound across long sessions.
        let now_cmp = Instant::now();
        self.flashing.retain(|_, d| *d > now_cmp);
        transitions
    }

    fn evaluate_temperature(&mut self, gpu: &GpuInfo, out: &mut Vec<AlertTransition>) {
        let temp = gpu.temperature as f64;
        if gpu.temperature == 0 {
            return; // N/A — don't alert on missing data.
        }
        let warn_on = self.config.temp_warn_c as f64;
        let crit_on = self.config.temp_crit_c as f64;
        let warn_off = warn_on - self.config.hysteresis_c as f64;
        let crit_off = crit_on - self.config.hysteresis_c as f64;

        let key = RuleKey {
            device_id: device_id(gpu),
            rule: RuleKind::Temperature,
        };
        let current = self.states.entry(key).or_default().level;

        // Pick the target level with hysteresis. When the device is
        // already at a given level, we require a drop of `hysteresis_c`
        // below the entry point before stepping down.
        let target = match current {
            AlertLevel::Crit => {
                if temp <= crit_off {
                    if temp <= warn_off {
                        AlertLevel::Ok
                    } else {
                        AlertLevel::Warn
                    }
                } else {
                    AlertLevel::Crit
                }
            }
            AlertLevel::Warn => {
                if temp >= crit_on && crit_on > 0.0 {
                    AlertLevel::Crit
                } else if temp <= warn_off {
                    AlertLevel::Ok
                } else {
                    AlertLevel::Warn
                }
            }
            AlertLevel::Ok => {
                if crit_on > 0.0 && temp >= crit_on {
                    AlertLevel::Crit
                } else if warn_on > 0.0 && temp >= warn_on {
                    AlertLevel::Warn
                } else {
                    AlertLevel::Ok
                }
            }
        };

        if target != current {
            let key = RuleKey {
                device_id: device_id(gpu),
                rule: RuleKind::Temperature,
            };
            if let Some(state) = self.states.get_mut(&key) {
                state.level = target;
            }
            let threshold = if target == AlertLevel::Crit {
                crit_on
            } else {
                warn_on
            };
            let message =
                build_message(gpu, RuleKind::Temperature, current, target, temp, threshold);
            let card_key = device_id(gpu);
            self.mark_flash(&card_key);
            out.push(AlertTransition {
                timestamp: Local::now(),
                host: gpu.hostname.clone(),
                gpu_index: gpu.detail.get("index").and_then(|s| s.parse().ok()),
                rule: RuleKind::Temperature,
                from: current,
                to: target,
                value: temp,
                threshold,
                message,
                card_key,
            });
        }
    }

    fn evaluate_idle_utilization(
        &mut self,
        gpu: &GpuInfo,
        now: Instant,
        out: &mut Vec<AlertTransition>,
    ) {
        if self.config.util_idle_warn_mins == 0 {
            return;
        }
        let util = gpu.utilization;
        if util < 0.0 {
            return;
        }
        let threshold_pct = self.config.util_idle_pct as f64;
        let warn_after =
            std::time::Duration::from_secs(self.config.util_idle_warn_mins as u64 * 60);

        let key = RuleKey {
            device_id: device_id(gpu),
            rule: RuleKind::IdleUtilization,
        };
        let state = self.states.entry(key).or_default();
        let prev_level = state.level;

        if util <= threshold_pct {
            // Device is idle. Start or continue timing.
            let start = state.idle_since.get_or_insert(now);
            let elapsed = now.duration_since(*start);
            let target = if elapsed >= warn_after {
                AlertLevel::Warn
            } else {
                AlertLevel::Ok
            };
            if target != prev_level {
                state.level = target;
                let threshold = self.config.util_idle_warn_mins as f64;
                let message = build_message(
                    gpu,
                    RuleKind::IdleUtilization,
                    prev_level,
                    target,
                    util,
                    threshold,
                );
                let card_key = device_id(gpu);
                self.mark_flash(&card_key);
                out.push(AlertTransition {
                    timestamp: Local::now(),
                    host: gpu.hostname.clone(),
                    gpu_index: gpu.detail.get("index").and_then(|s| s.parse().ok()),
                    rule: RuleKind::IdleUtilization,
                    from: prev_level,
                    to: target,
                    value: util,
                    threshold,
                    message,
                    card_key,
                });
            }
        } else if state.idle_since.is_some() || prev_level != AlertLevel::Ok {
            // Device no longer idle — reset timing and emit recovery if needed.
            state.idle_since = None;
            if prev_level != AlertLevel::Ok {
                state.level = AlertLevel::Ok;
                let threshold = self.config.util_idle_warn_mins as f64;
                let message = build_message(
                    gpu,
                    RuleKind::IdleUtilization,
                    prev_level,
                    AlertLevel::Ok,
                    util,
                    threshold,
                );
                let card_key = device_id(gpu);
                self.mark_flash(&card_key);
                out.push(AlertTransition {
                    timestamp: Local::now(),
                    host: gpu.hostname.clone(),
                    gpu_index: gpu.detail.get("index").and_then(|s| s.parse().ok()),
                    rule: RuleKind::IdleUtilization,
                    from: prev_level,
                    to: AlertLevel::Ok,
                    value: util,
                    threshold,
                    message,
                    card_key,
                });
            }
        }
    }

    fn evaluate_power(&mut self, gpu: &GpuInfo, out: &mut Vec<AlertTransition>) {
        let limit = self.config.power_crit_w as f64;
        if limit <= 0.0 {
            return;
        }
        let value = gpu.power_consumption;
        let off = limit - self.config.hysteresis_c as f64;

        let key = RuleKey {
            device_id: device_id(gpu),
            rule: RuleKind::Power,
        };
        let current = self.states.entry(key).or_default().level;

        let target = match current {
            AlertLevel::Crit => {
                if value <= off {
                    AlertLevel::Ok
                } else {
                    AlertLevel::Crit
                }
            }
            _ => {
                if value >= limit {
                    AlertLevel::Crit
                } else {
                    AlertLevel::Ok
                }
            }
        };
        if target != current {
            let key = RuleKey {
                device_id: device_id(gpu),
                rule: RuleKind::Power,
            };
            if let Some(state) = self.states.get_mut(&key) {
                state.level = target;
            }
            let message = build_message(gpu, RuleKind::Power, current, target, value, limit);
            let card_key = device_id(gpu);
            self.mark_flash(&card_key);
            out.push(AlertTransition {
                timestamp: Local::now(),
                host: gpu.hostname.clone(),
                gpu_index: gpu.detail.get("index").and_then(|s| s.parse().ok()),
                rule: RuleKind::Power,
                from: current,
                to: target,
                value,
                threshold: limit,
                message,
                card_key,
            });
        }
    }
}

/// Produce a short human-readable summary for a transition. Kept separate
/// so snapshot tests can re-use the same formatter.
fn build_message(
    gpu: &GpuInfo,
    rule: RuleKind,
    from: AlertLevel,
    to: AlertLevel,
    value: f64,
    threshold: f64,
) -> String {
    let ix = gpu.detail.get("index").map(|s| s.as_str()).unwrap_or("?");
    let hn = &gpu.hostname;
    let label = rule.as_label();
    let from_s = from.as_label();
    let to_s = to.as_label();
    match rule {
        RuleKind::Temperature => {
            format!("{hn} gpu{ix} {label}: {from_s}->{to_s} ({value:.0}C / thr {threshold:.0}C)",)
        }
        RuleKind::IdleUtilization => {
            format!("{hn} gpu{ix} idle: {from_s}->{to_s} ({value:.0}% for >= {threshold:.0}m)",)
        }
        RuleKind::Power => {
            format!("{hn} gpu{ix} power: {from_s}->{to_s} ({value:.0}W / thr {threshold:.0}W)",)
        }
    }
}

/// Build a stable per-device key used for both the state machine and the
/// border-flash registry.
fn device_id(gpu: &GpuInfo) -> String {
    if !gpu.uuid.is_empty() {
        gpu.uuid.clone()
    } else {
        format!(
            "{}@{}",
            gpu.detail.get("index").map(|s| s.as_str()).unwrap_or("?"),
            gpu.hostname
        )
    }
}

/// JSON body shape posted to the webhook. Exposed so the webhook module
/// and its tests can share the same serialisation.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WebhookPayload {
    pub timestamp: String,
    pub host: String,
    pub gpu_index: Option<i32>,
    pub rule: String,
    pub from: String,
    pub to: String,
    pub value: f64,
    pub threshold: f64,
}

impl From<&AlertTransition> for WebhookPayload {
    fn from(t: &AlertTransition) -> Self {
        Self {
            timestamp: t.timestamp.to_rfc3339(),
            host: t.host.clone(),
            gpu_index: t.gpu_index,
            rule: t.rule.as_label().to_string(),
            from: t.from.as_label().to_string(),
            to: t.to.as_label().to_string(),
            value: t.value,
            threshold: t.threshold,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn gpu(temp: u32, util: f64, power: f64) -> GpuInfo {
        let mut detail = HashMap::new();
        detail.insert("index".to_string(), "0".to_string());
        GpuInfo {
            uuid: "GPU-0".to_string(),
            time: String::new(),
            name: "TestGPU".to_string(),
            device_type: "GPU".to_string(),
            host_id: "h".to_string(),
            hostname: "n01".to_string(),
            instance: String::new(),
            utilization: util,
            ane_utilization: 0.0,
            dla_utilization: None,
            tensorcore_utilization: None,
            temperature: temp,
            used_memory: 0,
            total_memory: 0,
            frequency: 0,
            power_consumption: power,
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

    fn default_cfg() -> AlertConfig {
        AlertConfig {
            temp_warn_c: 80,
            temp_crit_c: 90,
            util_idle_pct: 5,
            util_idle_warn_mins: 0, // disabled for most tests
            power_crit_w: 0,        // disabled for most tests
            bell_on_critical: false,
            webhook_url: String::new(),
            hysteresis_c: 2,
            flash_duration_secs: 2,
        }
    }

    #[test]
    fn no_transition_when_below_warn() {
        let mut a = Alerter::new(default_cfg());
        let t = a.evaluate(&[gpu(70, 50.0, 0.0)]);
        assert!(t.is_empty());
    }

    #[test]
    fn ok_to_warn_emits_one_transition() {
        let mut a = Alerter::new(default_cfg());
        let t = a.evaluate(&[gpu(81, 50.0, 0.0)]);
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].rule, RuleKind::Temperature);
        assert_eq!(t[0].from, AlertLevel::Ok);
        assert_eq!(t[0].to, AlertLevel::Warn);
    }

    #[test]
    fn warn_to_crit_emits_transition() {
        let mut a = Alerter::new(default_cfg());
        a.evaluate(&[gpu(81, 50.0, 0.0)]); // OK -> warn
        let t = a.evaluate(&[gpu(91, 50.0, 0.0)]);
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].from, AlertLevel::Warn);
        assert_eq!(t[0].to, AlertLevel::Crit);
    }

    #[test]
    fn hysteresis_keeps_crit_until_drop() {
        // temp_crit=90, hysteresis=2 → must drop to <=88 to leave crit.
        let mut a = Alerter::new(default_cfg());
        a.evaluate(&[gpu(91, 0.0, 0.0)]); // -> crit
        // Dropping to 89 must NOT leave crit.
        let t = a.evaluate(&[gpu(89, 0.0, 0.0)]);
        assert!(t.is_empty(), "expected no transition at 89°C, got {t:?}");
        // Dropping to 88 leaves crit (enters warn, not ok, because 88 > 80-2=78).
        let t = a.evaluate(&[gpu(88, 0.0, 0.0)]);
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].from, AlertLevel::Crit);
        assert_eq!(t[0].to, AlertLevel::Warn);
    }

    #[test]
    fn hysteresis_boundary_exactly_at_crit_off() {
        // crit=90, hysteresis=2, so crit_off=88. Temp==88 should trigger transition.
        let mut a = Alerter::new(default_cfg());
        a.evaluate(&[gpu(91, 0.0, 0.0)]);
        let t = a.evaluate(&[gpu(88, 0.0, 0.0)]);
        assert_eq!(t.len(), 1);
    }

    #[test]
    fn recovery_to_ok_emits_transition() {
        let mut a = Alerter::new(default_cfg());
        a.evaluate(&[gpu(85, 0.0, 0.0)]); // OK -> warn
        // warn_off = warn(80) - hysteresis(2) = 78. Drop to 77 recovers.
        let t = a.evaluate(&[gpu(77, 0.0, 0.0)]);
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].to, AlertLevel::Ok);
    }

    #[test]
    fn zero_thresholds_disable_rule() {
        let mut cfg = default_cfg();
        cfg.temp_warn_c = 0;
        cfg.temp_crit_c = 0;
        let mut a = Alerter::new(cfg);
        let t = a.evaluate(&[gpu(95, 0.0, 0.0)]);
        assert!(t.is_empty());
    }

    #[test]
    fn zero_temperature_is_treated_as_absent() {
        let mut a = Alerter::new(default_cfg());
        let t = a.evaluate(&[gpu(0, 50.0, 0.0)]);
        assert!(t.is_empty());
    }

    #[test]
    fn power_rule_disabled_when_zero() {
        let mut a = Alerter::new(default_cfg());
        let t = a.evaluate(&[gpu(60, 50.0, 500.0)]);
        assert!(t.is_empty());
    }

    #[test]
    fn power_rule_triggers_when_enabled() {
        let mut cfg = default_cfg();
        cfg.power_crit_w = 400;
        let mut a = Alerter::new(cfg);
        let t = a.evaluate(&[gpu(60, 50.0, 450.0)]);
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].rule, RuleKind::Power);
        assert_eq!(t[0].to, AlertLevel::Crit);
    }

    #[test]
    fn flash_is_registered_on_transition() {
        let mut a = Alerter::new(default_cfg());
        a.evaluate(&[gpu(91, 0.0, 0.0)]);
        assert!(a.is_flashing("GPU-0"));
    }

    #[test]
    fn webhook_payload_contains_expected_fields() {
        let mut a = Alerter::new(default_cfg());
        let trans = a.evaluate(&[gpu(95, 0.0, 0.0)]);
        assert_eq!(trans.len(), 1);
        let payload = WebhookPayload::from(&trans[0]);
        assert_eq!(payload.rule, "temperature");
        assert_eq!(payload.to, "crit");
        assert_eq!(payload.value, 95.0);
        assert_eq!(payload.threshold, 90.0);
        assert_eq!(payload.host, "n01");
        assert_eq!(payload.gpu_index, Some(0));
        // RFC3339 roundtrips as JSON
        let serialised = serde_json::to_string(&payload).unwrap();
        assert!(serialised.contains("\"rule\":\"temperature\""));
        assert!(serialised.contains("\"from\":\"ok\""));
        assert!(serialised.contains("\"to\":\"crit\""));
    }

    #[test]
    fn config_update_preserves_states() {
        let mut a = Alerter::new(default_cfg());
        a.evaluate(&[gpu(85, 0.0, 0.0)]); // OK -> warn
        let mut cfg = default_cfg();
        cfg.temp_warn_c = 70; // narrower
        a.set_config(cfg);
        // Already in warn; stay warn (no transition emitted) and also no
        // duplicate ok->warn.
        let t = a.evaluate(&[gpu(85, 0.0, 0.0)]);
        assert!(t.is_empty());
    }
}
