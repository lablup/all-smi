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

use crate::device::ChassisInfo;
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
}
