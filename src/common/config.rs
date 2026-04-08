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
