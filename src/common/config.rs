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

/// Application configuration constants
#[allow(dead_code)] // Many constants used across modules but clippy may not detect cross-module usage
pub struct AppConfig;

impl AppConfig {
    // UI Rendering Constants
    // Optimized for CPU efficiency: 10 FPS is sufficient for monitoring tools
    // This significantly reduces CPU usage while maintaining smooth visuals
    pub const MIN_RENDER_INTERVAL_MS: u64 = 100; // ~10 FPS (was 33ms/30 FPS)
    #[allow(dead_code)] // Retained for configuration reference; replaced by TERMINAL_READER_POLL_MS in event-driven model
    pub const EVENT_POLL_TIMEOUT_MS: u64 = 100; // Poll every 100ms (was 50ms)
    pub const SCROLL_UPDATE_FREQUENCY: u64 = 2; // Every N frames for text scrolling (2 = every 200ms at 10 FPS)

    // Event-driven UI constants
    /// Animation tick interval in milliseconds (loading indicator, marquee scroll)
    pub const ANIMATION_TICK_MS: u64 = 200;
    /// Refresh tick interval when no animations are active (clock update only)
    pub const REFRESH_TICK_MS: u64 = 1000;
    /// Poll timeout for the dedicated terminal reader task (ms).
    /// Short enough to detect shutdown promptly, long enough to avoid busy-spinning.
    pub const TERMINAL_READER_POLL_MS: u64 = 50;

    // Network Configuration
    pub const BACKEND_AI_DEFAULT_PORT: u16 = 9090;
    pub const MAX_CONCURRENT_CONNECTIONS: usize = 128;
    pub const CONNECTION_TIMEOUT_SECS: u64 = 5;
    pub const POOL_IDLE_TIMEOUT_SECS: u64 = 60;
    pub const POOL_MAX_IDLE_PER_HOST: usize = 200;
    pub const TCP_KEEPALIVE_SECS: u64 = 30;
    pub const HTTP2_KEEPALIVE_SECS: u64 = 30;
    pub const RETRY_ATTEMPTS: u32 = 3;
    pub const RETRY_BASE_DELAY_MS: u64 = 50;

    // Data Collection
    #[allow(dead_code)] // Future configuration option
    pub const DEFAULT_UPDATE_INTERVAL_SECS: u64 = 2;
    pub const HISTORY_MAX_ENTRIES: usize = 100;
    pub const CONNECTION_STAGGER_BASE_MS: u64 = 500;

    // UI Layout Constants
    pub const PROGRESS_BAR_LABEL_WIDTH: usize = 5;
    pub const PROGRESS_BAR_BRACKET_WIDTH: usize = 4; // ": [" + "]"
    pub const PROGRESS_BAR_TEXT_WIDTH: usize = 8;
    #[allow(dead_code)] // Future UI configuration
    pub const DASHBOARD_ITEM_WIDTH: usize = 15;
    #[allow(dead_code)] // Default terminal fallback values for future use
    pub const DEFAULT_TERMINAL_WIDTH: u16 = 80;
    #[allow(dead_code)] // Default terminal fallback values for future use
    pub const DEFAULT_TERMINAL_HEIGHT: u16 = 24;

    // Memory and Performance
    #[allow(dead_code)] // Future Linux-specific calculations
    pub const LINUX_PAGE_SIZE_BYTES: u64 = 4096;
    #[allow(dead_code)] // Future Linux-specific calculations
    pub const LINUX_JIFFIES_PER_SECOND: u64 = 100;
    #[allow(dead_code)] // Future notification system
    pub const NOTIFICATION_DURATION_SECS: u64 = 5;

    // Color Thresholds
    pub const CRITICAL_THRESHOLD: f64 = 0.8;
    pub const WARNING_THRESHOLD: f64 = 0.7;
    pub const NORMAL_THRESHOLD: f64 = 0.25;
    pub const LOW_THRESHOLD: f64 = 0.05;
}

/// Threshold alert configuration used by the TUI alerter.
///
/// The defaults mirror the example `[alerts]` section in the issue so that
/// both local and remote modes produce useful transitions out of the box
/// when no config file is present. When the companion config-file issue
/// lands, a loader can overwrite these values from TOML; the CLI already
/// exposes `--alert-temp` / `--alert-util-low-mins` overrides in the
/// meantime.
#[derive(Clone, Debug)]
pub struct AlertConfig {
    /// Temperature in Celsius at which GPUs move from `ok` to `warn`.
    /// `0` disables the rule entirely.
    pub temp_warn_c: u32,
    /// Temperature in Celsius at which GPUs move from `warn` to `crit`.
    /// `0` disables the rule entirely.
    pub temp_crit_c: u32,
    /// Utilization percentage at or below which the GPU is considered idle.
    pub util_idle_pct: u32,
    /// Minutes of sustained idle before we emit an `ok → warn` transition.
    /// `0` disables the rule.
    pub util_idle_warn_mins: u32,
    /// Power consumption in Watts at which the GPU moves to `crit`. `0`
    /// disables the rule.
    pub power_crit_w: u32,
    /// When true, a `\u{7}` bell is emitted on any `→ crit` transition.
    pub bell_on_critical: bool,
    /// Destination URL for fire-and-forget webhook POSTs. Empty disables
    /// the feature.
    pub webhook_url: String,
    /// Hysteresis (in the rule's native unit, °C for temperature and W for
    /// power) applied to every rule. A device currently in `crit` must
    /// drop this many units below the `crit` threshold before moving out.
    pub hysteresis_c: u32,
    /// How long a card's border should flash after any transition.
    pub flash_duration_secs: u64,
}

impl Default for AlertConfig {
    fn default() -> Self {
        Self {
            temp_warn_c: 80,
            temp_crit_c: 90,
            util_idle_pct: 5,
            util_idle_warn_mins: 15,
            power_crit_w: 0,
            bell_on_critical: false,
            webhook_url: String::new(),
            hysteresis_c: 2,
            flash_duration_secs: 2,
        }
    }
}

impl AlertConfig {
    /// Apply CLI overrides on top of the defaults. Each override is
    /// `Some(_)` when the operator explicitly passed the flag.
    pub fn with_cli_overrides(
        mut self,
        alert_temp: Option<u32>,
        alert_util_low_mins: Option<u32>,
    ) -> Self {
        if let Some(t) = alert_temp {
            // A single `--alert-temp` flag sets both thresholds, mirroring
            // Slurm / Prom alert operators. `crit` stays 10 °C above warn
            // unless the operator explicitly raises the crit-only value.
            self.temp_warn_c = t;
            if self.temp_crit_c < t + 5 {
                self.temp_crit_c = t + 10;
            }
        }
        if let Some(m) = alert_util_low_mins {
            self.util_idle_warn_mins = m;
        }
        self
    }
}

/// Energy accounting configuration (issue #191).
///
/// Built from defaults, then overlaid with the following sources in
/// order of precedence (lowest to highest):
///
/// 1. `[energy]` section of the TOML config file (added by companion
///    issue #192; loader is a no-op until that lands).
/// 2. Environment-variable overrides (see
///    [`EnergyConfig::with_env_overrides`]).
///
/// The TUI cost display is suppressed whenever `show_cost` is `false`
/// or `price_per_kwh` is not a finite positive number. The Prometheus
/// counter is exported unconditionally so downstream tooling can
/// compute its own cost estimate.
#[derive(Clone, Debug)]
pub struct EnergyConfig {
    /// Electricity price per kilowatt-hour in `currency` units.
    pub price_per_kwh: f64,
    /// Display-only currency code (e.g. `"USD"`, `"KRW"`, `"EUR"`).
    pub currency: String,
    /// When `false`, the TUI renders the kWh total but NOT the
    /// monetary cost. Ignored when `price_per_kwh == 0` (there is no
    /// cost to show in that case).
    pub show_cost: bool,
    /// Path to the WAL file. Tilde expansion is applied at open time.
    pub wal_path: String,
    /// Threshold (in seconds) above which the integrator switches from
    /// trapezoidal to hold-last integration across sample gaps.
    pub gap_interpolate_seconds: u64,
    /// When `false`, no WAL is opened — counters are in-memory only.
    /// Set via env-var `ALL_SMI_ENERGY_NO_WAL=1` for ephemeral hosts.
    pub wal_enabled: bool,
}

impl Default for EnergyConfig {
    fn default() -> Self {
        Self {
            price_per_kwh: 0.12,
            currency: "USD".to_string(),
            show_cost: true,
            wal_path: "~/.cache/all-smi/energy-wal.bin".to_string(),
            gap_interpolate_seconds: 10,
            wal_enabled: true,
        }
    }
}

impl EnergyConfig {
    /// Overlay env-var overrides.
    ///
    /// Recognised variables:
    /// - `ALL_SMI_ENERGY_PRICE`: override `price_per_kwh` (invalid
    ///   values are silently ignored so a typo cannot brick the TUI).
    /// - `ALL_SMI_ENERGY_CURRENCY`: override `currency`.
    /// - `ALL_SMI_ENERGY_NO_COST`: when set (any value), unsets
    ///   `show_cost`.
    /// - `ALL_SMI_ENERGY_WAL_PATH`: override `wal_path`.
    /// - `ALL_SMI_ENERGY_NO_WAL`: when set (any value), disables the
    ///   disk-backed WAL.
    /// - `ALL_SMI_ENERGY_GAP_SECONDS`: override
    ///   `gap_interpolate_seconds`.
    pub fn with_env_overrides(mut self) -> Self {
        if let Ok(v) = std::env::var("ALL_SMI_ENERGY_PRICE")
            && let Ok(price) = v.parse::<f64>()
            && price.is_finite()
            && price >= 0.0
        {
            self.price_per_kwh = price;
        }
        if let Ok(v) = std::env::var("ALL_SMI_ENERGY_CURRENCY")
            && !v.trim().is_empty()
        {
            self.currency = v.trim().to_string();
        }
        if std::env::var("ALL_SMI_ENERGY_NO_COST").is_ok() {
            self.show_cost = false;
        }
        if let Ok(v) = std::env::var("ALL_SMI_ENERGY_WAL_PATH")
            && !v.trim().is_empty()
        {
            self.wal_path = v.trim().to_string();
        }
        if std::env::var("ALL_SMI_ENERGY_NO_WAL").is_ok() {
            self.wal_enabled = false;
        }
        if let Ok(v) = std::env::var("ALL_SMI_ENERGY_GAP_SECONDS")
            && let Ok(secs) = v.parse::<u64>()
            && secs > 0
        {
            self.gap_interpolate_seconds = secs;
        }
        self
    }

    /// `true` iff the TUI should render the monetary cost column.
    ///
    /// Returns `false` for non-positive or non-finite prices so a
    /// mis-configured `ALL_SMI_ENERGY_PRICE=abc` quietly hides the
    /// column instead of surfacing `$NaN`.
    pub fn cost_visible(&self) -> bool {
        self.show_cost && self.price_per_kwh.is_finite() && self.price_per_kwh > 0.0
    }
}

/// Environment-specific configuration
#[allow(dead_code)] // Functions used across modules but clippy may not detect cross-module usage
pub struct EnvConfig;

impl EnvConfig {
    pub fn adaptive_interval(node_count: usize) -> u64 {
        match node_count {
            0 => {
                // Local monitoring only (no remote nodes)
                // Use 1 second interval for Apple Silicon local monitoring
                if cfg!(target_os = "macos") && crate::device::is_apple_silicon() {
                    1
                } else {
                    2
                }
            }
            1..=10 => 3, // 1-10 remote nodes: 3 seconds
            11..=50 => 4,
            51..=100 => 5,
            _ => 6,
        }
    }

    #[allow(dead_code)] // Future connection management
    pub fn max_concurrent_connections(total_hosts: usize) -> usize {
        std::cmp::min(total_hosts, AppConfig::MAX_CONCURRENT_CONNECTIONS)
    }

    pub fn connection_stagger_delay(host_index: usize, total_hosts: usize) -> u64 {
        (host_index as u64 * AppConfig::CONNECTION_STAGGER_BASE_MS) / total_hosts as u64
    }

    pub fn retry_delay(attempt: u32) -> u64 {
        AppConfig::RETRY_BASE_DELAY_MS * attempt as u64
    }
}

/// UI Theme configuration (requires `cli` feature for crossterm color support)
#[cfg(feature = "cli")]
pub struct ThemeConfig;

#[cfg(feature = "cli")]
impl ThemeConfig {
    pub fn accent_color() -> crossterm::style::Color {
        crossterm::style::Color::Cyan
    }

    pub fn cpu_color() -> crossterm::style::Color {
        crossterm::style::Color::Cyan
    }

    pub fn gpu_color() -> crossterm::style::Color {
        crossterm::style::Color::Blue
    }

    pub fn memory_color() -> crossterm::style::Color {
        crossterm::style::Color::Green
    }

    pub fn power_color() -> crossterm::style::Color {
        crossterm::style::Color::Red
    }

    pub fn thermal_color() -> crossterm::style::Color {
        crossterm::style::Color::Magenta
    }

    pub fn accelerator_color() -> crossterm::style::Color {
        crossterm::style::Color::Yellow
    }

    pub fn progress_bar_color(fill_ratio: f64) -> crossterm::style::Color {
        use crossterm::style::Color;

        if fill_ratio > AppConfig::CRITICAL_THRESHOLD {
            Color::Red
        } else if fill_ratio > AppConfig::WARNING_THRESHOLD {
            Color::Yellow
        } else if fill_ratio > AppConfig::NORMAL_THRESHOLD {
            Color::Green
        } else if fill_ratio > AppConfig::LOW_THRESHOLD {
            Color::DarkGreen
        } else {
            Color::DarkGrey
        }
    }

    pub fn utilization_color(utilization: f64) -> crossterm::style::Color {
        use crossterm::style::Color;

        if utilization > 80.0 {
            Color::Red
        } else if utilization > 50.0 {
            Color::Yellow
        } else if utilization > 20.0 {
            Color::Green
        } else {
            Color::DarkGrey
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_adaptive_interval() {
        // Test accounts for Apple Silicon returning 1 second for local monitoring
        let expected_local = if cfg!(target_os = "macos") && crate::device::is_apple_silicon() {
            1
        } else {
            2
        };
        assert_eq!(EnvConfig::adaptive_interval(0), expected_local);
        assert_eq!(EnvConfig::adaptive_interval(1), 3); // 1 remote node: 3 seconds
        assert_eq!(EnvConfig::adaptive_interval(2), 3);
        assert_eq!(EnvConfig::adaptive_interval(5), 3);
        assert_eq!(EnvConfig::adaptive_interval(10), 3);
        assert_eq!(EnvConfig::adaptive_interval(11), 4);
        assert_eq!(EnvConfig::adaptive_interval(25), 4);
        assert_eq!(EnvConfig::adaptive_interval(50), 4);
        assert_eq!(EnvConfig::adaptive_interval(51), 5);
        assert_eq!(EnvConfig::adaptive_interval(75), 5);
        assert_eq!(EnvConfig::adaptive_interval(100), 5);
        assert_eq!(EnvConfig::adaptive_interval(101), 6);
        assert_eq!(EnvConfig::adaptive_interval(200), 6);
        assert_eq!(EnvConfig::adaptive_interval(500), 6);
        assert_eq!(EnvConfig::adaptive_interval(1000), 6);
    }

    #[test]
    fn test_max_concurrent_connections() {
        assert_eq!(EnvConfig::max_concurrent_connections(10), 10);
        assert_eq!(EnvConfig::max_concurrent_connections(50), 50);
        assert_eq!(EnvConfig::max_concurrent_connections(64), 64);
        assert_eq!(EnvConfig::max_concurrent_connections(100), 100);
        assert_eq!(EnvConfig::max_concurrent_connections(128), 128);
        assert_eq!(EnvConfig::max_concurrent_connections(200), 128);
    }

    #[test]
    fn test_connection_stagger_delay() {
        assert_eq!(EnvConfig::connection_stagger_delay(0, 10), 0);
        assert_eq!(EnvConfig::connection_stagger_delay(1, 10), 50);
        assert_eq!(EnvConfig::connection_stagger_delay(5, 10), 250);
        assert_eq!(EnvConfig::connection_stagger_delay(9, 10), 450);
        assert_eq!(EnvConfig::connection_stagger_delay(0, 1), 0);
        assert_eq!(EnvConfig::connection_stagger_delay(10, 20), 250);
    }

    #[test]
    fn test_retry_delay() {
        assert_eq!(EnvConfig::retry_delay(1), 50);
        assert_eq!(EnvConfig::retry_delay(2), 100);
        assert_eq!(EnvConfig::retry_delay(3), 150);
        assert_eq!(EnvConfig::retry_delay(5), 250);
        assert_eq!(EnvConfig::retry_delay(0), 0);
    }

    #[test]
    #[cfg(feature = "cli")]
    fn test_progress_bar_color_thresholds() {
        use crossterm::style::Color;

        assert_eq!(ThemeConfig::progress_bar_color(0.0), Color::DarkGrey);
        assert_eq!(ThemeConfig::progress_bar_color(0.03), Color::DarkGrey);
        assert_eq!(ThemeConfig::progress_bar_color(0.05), Color::DarkGrey);
        assert_eq!(ThemeConfig::progress_bar_color(0.06), Color::DarkGreen);
        assert_eq!(ThemeConfig::progress_bar_color(0.1), Color::DarkGreen);
        assert_eq!(ThemeConfig::progress_bar_color(0.25), Color::DarkGreen);
        assert_eq!(ThemeConfig::progress_bar_color(0.26), Color::Green);
        assert_eq!(ThemeConfig::progress_bar_color(0.5), Color::Green);
        assert_eq!(ThemeConfig::progress_bar_color(0.7), Color::Green);
        assert_eq!(ThemeConfig::progress_bar_color(0.71), Color::Yellow);
        assert_eq!(ThemeConfig::progress_bar_color(0.75), Color::Yellow);
        assert_eq!(ThemeConfig::progress_bar_color(0.8), Color::Yellow);
        assert_eq!(ThemeConfig::progress_bar_color(0.81), Color::Red);
        assert_eq!(ThemeConfig::progress_bar_color(0.9), Color::Red);
        assert_eq!(ThemeConfig::progress_bar_color(1.0), Color::Red);
    }

    #[test]
    #[cfg(feature = "cli")]
    fn test_utilization_color_thresholds() {
        use crossterm::style::Color;

        assert_eq!(ThemeConfig::utilization_color(0.0), Color::DarkGrey);
        assert_eq!(ThemeConfig::utilization_color(10.0), Color::DarkGrey);
        assert_eq!(ThemeConfig::utilization_color(20.0), Color::DarkGrey);
        assert_eq!(ThemeConfig::utilization_color(20.1), Color::Green);
        assert_eq!(ThemeConfig::utilization_color(30.0), Color::Green);
        assert_eq!(ThemeConfig::utilization_color(50.0), Color::Green);
        assert_eq!(ThemeConfig::utilization_color(50.1), Color::Yellow);
        assert_eq!(ThemeConfig::utilization_color(70.0), Color::Yellow);
        assert_eq!(ThemeConfig::utilization_color(80.0), Color::Yellow);
        assert_eq!(ThemeConfig::utilization_color(80.1), Color::Red);
        assert_eq!(ThemeConfig::utilization_color(90.0), Color::Red);
        assert_eq!(ThemeConfig::utilization_color(100.0), Color::Red);
    }

    #[test]
    fn test_alert_config_defaults() {
        let cfg = AlertConfig::default();
        assert_eq!(cfg.temp_warn_c, 80);
        assert_eq!(cfg.temp_crit_c, 90);
        assert_eq!(cfg.util_idle_pct, 5);
        assert_eq!(cfg.util_idle_warn_mins, 15);
        assert_eq!(cfg.power_crit_w, 0);
        assert!(!cfg.bell_on_critical);
        assert!(cfg.webhook_url.is_empty());
        assert_eq!(cfg.hysteresis_c, 2);
        assert_eq!(cfg.flash_duration_secs, 2);
    }

    #[test]
    fn test_alert_config_cli_overrides_apply() {
        let cfg = AlertConfig::default().with_cli_overrides(Some(70), Some(30));
        assert_eq!(cfg.temp_warn_c, 70);
        // Auto-adjusted to keep crit above warn.
        assert!(cfg.temp_crit_c > cfg.temp_warn_c);
        assert_eq!(cfg.util_idle_warn_mins, 30);
    }

    #[test]
    fn test_alert_config_cli_overrides_no_change_when_none() {
        let original = AlertConfig::default();
        let cfg = original.clone().with_cli_overrides(None, None);
        assert_eq!(cfg.temp_warn_c, original.temp_warn_c);
        assert_eq!(cfg.temp_crit_c, original.temp_crit_c);
        assert_eq!(cfg.util_idle_warn_mins, original.util_idle_warn_mins);
    }

    #[test]
    fn test_app_config_constants() {
        assert_eq!(AppConfig::MIN_RENDER_INTERVAL_MS, 100);
        assert_eq!(AppConfig::EVENT_POLL_TIMEOUT_MS, 100);
        assert_eq!(AppConfig::MAX_CONCURRENT_CONNECTIONS, 128);
        assert_eq!(AppConfig::CONNECTION_TIMEOUT_SECS, 5);
        assert_eq!(AppConfig::RETRY_ATTEMPTS, 3);
        assert_eq!(AppConfig::RETRY_BASE_DELAY_MS, 50);
        assert_eq!(AppConfig::DEFAULT_UPDATE_INTERVAL_SECS, 2);
        assert_eq!(AppConfig::CONNECTION_STAGGER_BASE_MS, 500);
        assert_eq!(AppConfig::CRITICAL_THRESHOLD, 0.8);
        assert_eq!(AppConfig::WARNING_THRESHOLD, 0.7);
        assert_eq!(AppConfig::NORMAL_THRESHOLD, 0.25);
        assert_eq!(AppConfig::LOW_THRESHOLD, 0.05);
    }

    #[test]
    fn test_boundary_conditions() {
        let expected_local = if cfg!(target_os = "macos") && crate::device::is_apple_silicon() {
            1
        } else {
            2
        };
        assert_eq!(EnvConfig::adaptive_interval(0), expected_local);
        assert_eq!(EnvConfig::adaptive_interval(usize::MAX), 6);
    }

    #[test]
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    fn test_apple_silicon_adaptive_interval() {
        // On Apple Silicon Macs, local monitoring should use 1 second interval
        assert_eq!(EnvConfig::adaptive_interval(0), 1);
        // Remote monitoring should use 3 seconds for 1 node
        assert_eq!(EnvConfig::adaptive_interval(1), 3);
        // Remote monitoring should follow standard intervals
        assert_eq!(EnvConfig::adaptive_interval(2), 3);
        assert_eq!(EnvConfig::adaptive_interval(10), 3);
    }

    #[test]
    #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
    fn test_non_apple_silicon_adaptive_interval() {
        // On non-Apple Silicon systems, use standard intervals
        assert_eq!(EnvConfig::adaptive_interval(0), 2);
        // Remote monitoring should use 3 seconds for 1 node
        assert_eq!(EnvConfig::adaptive_interval(1), 3);
        assert_eq!(EnvConfig::adaptive_interval(2), 3);
        assert_eq!(EnvConfig::adaptive_interval(10), 3);

        assert_eq!(EnvConfig::connection_stagger_delay(0, 1), 0);
        assert_eq!(EnvConfig::connection_stagger_delay(1000, 1000), 500);

        assert_eq!(EnvConfig::retry_delay(0), 0);
        assert_eq!(EnvConfig::retry_delay(1000), 50000);
    }

    #[test]
    fn energy_config_defaults_match_issue_spec() {
        let cfg = EnergyConfig::default();
        assert!((cfg.price_per_kwh - 0.12).abs() < 1e-9);
        assert_eq!(cfg.currency, "USD");
        assert!(cfg.show_cost);
        assert_eq!(cfg.wal_path, "~/.cache/all-smi/energy-wal.bin");
        assert_eq!(cfg.gap_interpolate_seconds, 10);
        assert!(cfg.wal_enabled);
        assert!(cfg.cost_visible());
    }

    #[test]
    fn energy_config_cost_hidden_when_price_zero_or_invalid() {
        let cfg = EnergyConfig {
            price_per_kwh: 0.0,
            ..EnergyConfig::default()
        };
        assert!(!cfg.cost_visible(), "zero price must hide cost");
        let cfg = EnergyConfig {
            price_per_kwh: -0.5,
            ..EnergyConfig::default()
        };
        assert!(!cfg.cost_visible(), "negative price must hide cost");
        let cfg = EnergyConfig {
            price_per_kwh: f64::NAN,
            ..EnergyConfig::default()
        };
        assert!(!cfg.cost_visible(), "NaN price must hide cost");
        let cfg = EnergyConfig {
            show_cost: false,
            ..EnergyConfig::default()
        };
        assert!(!cfg.cost_visible(), "show_cost=false must hide cost");
    }

    /// Shared mutex used by the env-var tests to serialize
    /// mutations of `ALL_SMI_ENERGY_*`. Cargo runs test fns on
    /// multiple threads by default; without this lock, the `set_var`
    /// / `remove_var` calls from two tests would race and either
    /// observe each other's partial state or observe the value the
    /// OTHER test was mid-clearing.
    static ENERGY_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn energy_config_env_overrides_apply() {
        let _guard = ENERGY_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let keys = [
            "ALL_SMI_ENERGY_PRICE",
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
            std::env::set_var("ALL_SMI_ENERGY_PRICE", "0.30");
            std::env::set_var("ALL_SMI_ENERGY_CURRENCY", "KRW");
            std::env::set_var("ALL_SMI_ENERGY_WAL_PATH", "/tmp/unit-wal.bin");
            std::env::set_var("ALL_SMI_ENERGY_GAP_SECONDS", "15");
        }
        let cfg = EnergyConfig::default().with_env_overrides();
        assert!((cfg.price_per_kwh - 0.30).abs() < 1e-9);
        assert_eq!(cfg.currency, "KRW");
        assert_eq!(cfg.wal_path, "/tmp/unit-wal.bin");
        assert_eq!(cfg.gap_interpolate_seconds, 15);
        unsafe {
            for k in keys {
                std::env::remove_var(k);
            }
        }
    }

    #[test]
    fn energy_config_invalid_price_env_ignored() {
        let _guard = ENERGY_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            std::env::remove_var("ALL_SMI_ENERGY_PRICE");
            std::env::set_var("ALL_SMI_ENERGY_PRICE", "not-a-number");
        }
        let cfg = EnergyConfig::default().with_env_overrides();
        assert!((cfg.price_per_kwh - 0.12).abs() < 1e-9);
        unsafe {
            std::env::remove_var("ALL_SMI_ENERGY_PRICE");
        }
    }

    #[test]
    #[cfg(all(
        feature = "cli",
        not(all(target_os = "macos", target_arch = "aarch64"))
    ))]
    fn test_non_apple_silicon_theme_boundary_values() {
        use crossterm::style::Color;
        assert_eq!(
            ThemeConfig::progress_bar_color(f64::NEG_INFINITY),
            Color::DarkGrey
        );
        assert_eq!(ThemeConfig::progress_bar_color(f64::INFINITY), Color::Red);
        assert_eq!(
            ThemeConfig::utilization_color(f64::NEG_INFINITY),
            Color::DarkGrey
        );
        assert_eq!(ThemeConfig::utilization_color(f64::INFINITY), Color::Red);

        assert_eq!(ThemeConfig::progress_bar_color(-1.0), Color::DarkGrey);
        assert_eq!(ThemeConfig::progress_bar_color(2.0), Color::Red);
        assert_eq!(ThemeConfig::utilization_color(-10.0), Color::DarkGrey);
        assert_eq!(ThemeConfig::utilization_color(200.0), Color::Red);
    }
}
