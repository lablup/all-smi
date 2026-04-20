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

use std::io::Write;

use crossterm::{queue, style::Color, style::Print};

use crate::common::config::EnergyConfig;
use crate::device::ChassisInfo;
use crate::metrics::energy::{
    EnergyKey, EnergyScope, PowerIntegrator, joules_to_cost, joules_to_kwh,
};
use crate::ui::text::print_colored_text;
use crate::ui::widgets::draw_bar;

use super::gpu_renderer::format_hostname_with_scroll;

/// Chassis renderer struct
#[allow(dead_code)]
pub struct ChassisRenderer;

impl Default for ChassisRenderer {
    fn default() -> Self {
        Self::new()
    }
}

#[allow(dead_code)]
impl ChassisRenderer {
    pub fn new() -> Self {
        Self
    }
}

/// Render the "Energy session" row below the chassis header (issue #191).
///
/// Emits nothing when no energy has accumulated yet — the renderer is
/// indifferent to "device does not report power" vs "power is zero",
/// which is the right behavior here because both collapse to "there
/// is nothing meaningful to show".
///
/// `price_per_kwh` comes from the runtime [`EnergyConfig`].  When
/// `cost_visible()` is false the renderer drops the `|  $cost` half
/// of the line.
pub fn print_chassis_energy_row<W: Write>(
    stdout: &mut W,
    info: &ChassisInfo,
    integrator: &PowerIntegrator,
    energy_config: &EnergyConfig,
) {
    let key = EnergyKey::chassis(info.hostname.clone());
    let stats = integrator
        .iter_stats()
        .find(|s| s.key.scope == EnergyScope::Chassis && s.key.host == info.hostname);
    let joules = match stats {
        Some(s) if s.session_joules > 0.0 => s.session_joules,
        _ => return, // No session energy — render nothing.
    };
    let _ = key; // suppress unused warning if future callers want to
    // look up the key directly.

    let kwh = joules_to_kwh(joules);
    let kwh_display = if kwh >= 0.001 {
        format!("{kwh:.3} kWh")
    } else {
        // Fall back to Joules for very early samples so the row does
        // not print "0.000 kWh" for the first few integration cycles.
        format!("{joules:.1} J")
    };

    print_colored_text(stdout, "     ", Color::White, None, None);
    print_colored_text(stdout, "Energy session: ", Color::Yellow, None, None);
    print_colored_text(stdout, &kwh_display, Color::White, None, None);

    if energy_config.cost_visible() {
        let cost = joules_to_cost(joules, energy_config.price_per_kwh);
        let cost_display = format_cost(&energy_config.currency, cost);
        let price_display = format!(
            " (at {}/kWh)",
            format_cost(&energy_config.currency, energy_config.price_per_kwh),
        );
        print_colored_text(stdout, "  |  ", Color::DarkGrey, None, None);
        print_colored_text(stdout, &cost_display, Color::Green, None, None);
        print_colored_text(stdout, &price_display, Color::DarkGrey, None, None);
    }

    queue!(stdout, Print("\r\n")).unwrap();
}

/// Pretty-print a monetary amount using a minimal currency-symbol
/// table.  Unknown currency codes are printed as-is to the right of
/// the amount — e.g. `0.35 GBP` — which is valid for display but keeps
/// the renderer from shipping a full FX table.
fn format_cost(currency: &str, amount: f64) -> String {
    let trimmed = currency.trim();
    match trimmed.to_ascii_uppercase().as_str() {
        "USD" => format!("${amount:.2}"),
        "EUR" => format!("\u{20AC}{amount:.2}"),
        "GBP" => format!("\u{00A3}{amount:.2}"),
        "JPY" => format!("\u{00A5}{amount:.0}"),
        "KRW" => format!("\u{20A9}{amount:.0}"),
        _ if trimmed.is_empty() => format!("{amount:.2}"),
        _ => format!("{amount:.2} {trimmed}"),
    }
}

/// Render chassis/node-level information including total power, thermal data
pub fn print_chassis_info<W: Write>(
    stdout: &mut W,
    _index: usize,
    info: &ChassisInfo,
    width: usize,
    hostname_scroll_offset: usize,
) {
    // Format hostname with scrolling if needed
    let hostname_display = format_hostname_with_scroll(&info.hostname, hostname_scroll_offset);

    // Print chassis info line: NODE <hostname> Pwr:<power>W Thermal:<status> [CPU:<x>W GPU:<y>W ANE:<z>W]
    print_colored_text(stdout, "NODE ", Color::Yellow, None, None);
    print_colored_text(stdout, &hostname_display, Color::White, None, None);

    // Total Power
    print_colored_text(stdout, " Pwr:", Color::Red, None, None);
    let power_display = if let Some(power) = info.total_power_watts {
        format!("{power:>6.1}W")
    } else {
        format!("{:>7}", "N/A")
    };
    print_colored_text(stdout, &power_display, Color::White, None, None);

    // Thermal pressure (Apple Silicon) or temperatures
    if let Some(ref pressure) = info.thermal_pressure {
        print_colored_text(stdout, " Thermal:", Color::Magenta, None, None);
        print_colored_text(stdout, &format!("{pressure:>8}"), Color::White, None, None);
    } else {
        // Show inlet/outlet temperatures if available
        if let Some(inlet) = info.inlet_temperature {
            print_colored_text(stdout, " Inlet:", Color::Magenta, None, None);
            print_colored_text(stdout, &format!("{inlet:>4.0}°C"), Color::White, None, None);
        }
        if let Some(outlet) = info.outlet_temperature {
            print_colored_text(stdout, " Outlet:", Color::Magenta, None, None);
            print_colored_text(
                stdout,
                &format!("{outlet:>4.0}°C"),
                Color::White,
                None,
                None,
            );
        }
    }

    // Power breakdown from detail (Apple Silicon: CPU, GPU, ANE)
    let has_power_breakdown = info.detail.contains_key("cpu_power_watts")
        || info.detail.contains_key("gpu_power_watts")
        || info.detail.contains_key("ane_power_watts");

    if has_power_breakdown {
        print_colored_text(stdout, " │", Color::DarkGrey, None, None);

        if let Some(cpu_power) = info.detail.get("cpu_power_watts")
            && let Ok(power) = cpu_power.parse::<f64>()
        {
            print_colored_text(stdout, " CPU:", Color::Cyan, None, None);
            print_colored_text(stdout, &format!("{power:>5.1}W"), Color::White, None, None);
        }

        if let Some(gpu_power) = info.detail.get("gpu_power_watts")
            && let Ok(power) = gpu_power.parse::<f64>()
        {
            print_colored_text(stdout, " GPU:", Color::Green, None, None);
            print_colored_text(stdout, &format!("{power:>5.1}W"), Color::White, None, None);
        }

        if let Some(ane_power) = info.detail.get("ane_power_watts")
            && let Ok(power) = ane_power.parse::<f64>()
        {
            print_colored_text(stdout, " ANE:", Color::Blue, None, None);
            print_colored_text(stdout, &format!("{power:>5.1}W"), Color::White, None, None);
        }
    }

    // Fan speeds if available
    if !info.fan_speeds.is_empty() {
        print_colored_text(stdout, " Fans:", Color::Cyan, None, None);
        let avg_rpm: u32 =
            info.fan_speeds.iter().map(|f| f.speed_rpm).sum::<u32>() / info.fan_speeds.len() as u32;
        print_colored_text(
            stdout,
            &format!("{avg_rpm:>5}RPM"),
            Color::White,
            None,
            None,
        );
    }

    // PSU status if available
    if !info.psu_status.is_empty() {
        let ok_count = info
            .psu_status
            .iter()
            .filter(|p| p.status == crate::device::PsuStatus::Ok)
            .count();
        let total = info.psu_status.len();
        print_colored_text(stdout, " PSU:", Color::Yellow, None, None);
        let psu_color = if ok_count == total {
            Color::Green
        } else {
            Color::Red
        };
        print_colored_text(
            stdout,
            &format!("{ok_count}/{total}"),
            psu_color,
            None,
            None,
        );
    }

    queue!(stdout, Print("\r\n")).unwrap();

    // Power gauge bar (if power data available)
    if let Some(power) = info.total_power_watts {
        // Calculate gauge width with 5 char padding on each side
        let available_width = width.saturating_sub(10);
        let gauge_width = available_width;

        // Determine max power for gauge based on platform
        // Apple Silicon: ~150W max, Server: ~1000W max
        let is_apple_silicon = info.detail.get("platform") == Some(&"Apple Silicon".to_string());
        let max_power = if is_apple_silicon { 150.0 } else { 1000.0 };

        let power_percent = (power / max_power * 100.0).min(100.0);

        let left_padding = 5;
        let right_padding = width - left_padding - gauge_width;

        print_colored_text(stdout, "     ", Color::White, None, None); // 5 char left padding

        draw_bar(
            stdout,
            "Power",
            power_percent,
            100.0,
            gauge_width,
            Some(format!("{power:.1}W")),
        );

        print_colored_text(stdout, &" ".repeat(right_padding), Color::White, None, None);
        queue!(stdout, Print("\r\n")).unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::ChassisInfo;
    use crate::metrics::energy::{EnergyKey, PowerIntegrator};
    use std::time::{Duration, Instant};

    #[test]
    fn test_chassis_renderer_new() {
        let renderer = ChassisRenderer::new();
        let _ = renderer;
    }

    #[test]
    fn test_print_chassis_info_basic() {
        let mut buffer = Vec::new();
        let chassis = ChassisInfo {
            hostname: "test-host".to_string(),
            total_power_watts: Some(45.5),
            thermal_pressure: Some("Nominal".to_string()),
            ..Default::default()
        };

        print_chassis_info(&mut buffer, 0, &chassis, 80, 0);
        let output = String::from_utf8(buffer).unwrap();

        assert!(output.contains("NODE"));
        assert!(output.contains("test-host"));
    }

    #[test]
    fn energy_row_emits_nothing_when_no_samples() {
        let mut buffer = Vec::new();
        let chassis = ChassisInfo {
            hostname: "dgx-01".to_string(),
            total_power_watts: Some(300.0),
            ..Default::default()
        };
        let integrator = PowerIntegrator::default();
        let cfg = EnergyConfig::default();
        print_chassis_energy_row(&mut buffer, &chassis, &integrator, &cfg);
        let output = String::from_utf8(buffer).unwrap();
        assert!(output.is_empty(), "expected empty output, got: {output:?}");
    }

    #[test]
    fn energy_row_shows_kwh_and_cost_when_enabled() {
        let mut buffer = Vec::new();
        let chassis = ChassisInfo {
            hostname: "dgx-01".to_string(),
            total_power_watts: Some(300.0),
            ..Default::default()
        };
        let mut integrator = PowerIntegrator::default();
        let key = EnergyKey::chassis("dgx-01");
        let origin = Instant::now();
        integrator.record_sample(key.clone(), origin, 300.0);
        integrator.record_sample(key.clone(), origin + Duration::from_secs(600), 300.0);
        // 300 W * 600 s = 180 000 J = 0.05 kWh
        let cfg = EnergyConfig::default();
        print_chassis_energy_row(&mut buffer, &chassis, &integrator, &cfg);
        let output = String::from_utf8(buffer).unwrap();
        assert!(output.contains("Energy session:"));
        assert!(output.contains("0.050 kWh"));
        // Default USD, default price 0.12 → cost $0.006 which is
        // rounded to $0.01 in the renderer's two-digit format.
        assert!(output.contains("$0.01"));
        assert!(output.contains("$0.12/kWh"));
    }

    #[test]
    fn energy_row_hides_cost_when_show_cost_false() {
        let mut buffer = Vec::new();
        let chassis = ChassisInfo {
            hostname: "dgx-01".to_string(),
            total_power_watts: Some(300.0),
            ..Default::default()
        };
        let mut integrator = PowerIntegrator::default();
        let key = EnergyKey::chassis("dgx-01");
        let origin = Instant::now();
        integrator.record_sample(key.clone(), origin, 300.0);
        integrator.record_sample(key, origin + Duration::from_secs(600), 300.0);
        let cfg = EnergyConfig {
            show_cost: false,
            ..EnergyConfig::default()
        };
        print_chassis_energy_row(&mut buffer, &chassis, &integrator, &cfg);
        let output = String::from_utf8(buffer).unwrap();
        assert!(output.contains("kWh"));
        assert!(!output.contains('$'), "cost should be hidden: {output}");
    }

    #[test]
    fn format_cost_respects_currency_code() {
        assert_eq!(format_cost("USD", 1.234), "$1.23");
        assert_eq!(format_cost("EUR", 1.234), "\u{20AC}1.23");
        assert_eq!(format_cost("KRW", 1234.5), "\u{20A9}1234");
        assert_eq!(format_cost("UNKNOWN", 1.234), "1.23 UNKNOWN");
        assert_eq!(format_cost("", 1.234), "1.23");
    }
}
