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

use crate::device::GpuInfo;
use crate::ui::text::print_colored_text;
use crate::ui::widgets::draw_bar;

/// GPU renderer struct implementing the DeviceRenderer trait
#[allow(dead_code)]
pub struct GpuRenderer;

impl Default for GpuRenderer {
    fn default() -> Self {
        Self::new()
    }
}

#[allow(dead_code)]
impl GpuRenderer {
    pub fn new() -> Self {
        Self
    }
}

/// Helper function to format hostname with scrolling.
///
/// For short hostnames (<= 9 chars) this returns a padded view without
/// allocating an extended scroll string. For long hostnames the scrolling
/// window is computed with a single allocation.
///
/// The byte-level fast path is used only for ASCII hostnames (the common
/// case per RFC 952). Non-ASCII hostnames fall back to char iteration.
pub(crate) fn format_hostname_with_scroll(hostname: &str, scroll_offset: usize) -> String {
    if hostname.len() > 9 {
        let scroll_len = hostname.len() + 3;
        let start_pos = scroll_offset % scroll_len;

        if hostname.is_ascii() {
            // Fast path for ASCII hostnames: byte indexing is safe and
            // byte length equals character count.
            let mut result = String::with_capacity(9);
            let extended_len = hostname.len() * 2 + 3;
            let mut idx = start_pos;
            while result.len() < 9 && idx < extended_len {
                let effective_idx = idx % extended_len;
                let ch = if effective_idx < hostname.len() {
                    hostname.as_bytes()[effective_idx] as char
                } else if effective_idx < hostname.len() + 3 {
                    ' '
                } else {
                    hostname.as_bytes()[effective_idx - hostname.len() - 3] as char
                };
                result.push(ch);
                idx += 1;
            }
            result
        } else {
            // Safe fallback for non-ASCII hostnames: use char iteration
            // to avoid splitting multibyte UTF-8 sequences.
            let extended_hostname = format!("{hostname}   {hostname}");
            extended_hostname
                .chars()
                .skip(start_pos)
                .take(9)
                .collect::<String>()
        }
    } else {
        // Always return 9 characters, left-aligned with space padding
        format!("{hostname:<9}")
    }
}

/// Render GPU information including utilization, memory, temperature, and power
pub fn print_gpu_info<W: Write>(
    stdout: &mut W,
    _index: usize,
    info: &GpuInfo,
    width: usize,
    device_name_scroll_offset: usize,
    hostname_scroll_offset: usize,
) {
    // Format device name with scrolling if needed
    let device_name = if info.name.len() > 15 {
        let scroll_len = info.name.len() + 3;
        let start_pos = device_name_scroll_offset % scroll_len;
        let extended_name = format!("{}   {}", info.name, info.name);

        extended_name
            .chars()
            .skip(start_pos)
            .take(15)
            .collect::<String>()
    } else {
        format!("{:<15}", info.name)
    };

    // Format hostname with scrolling if needed
    let hostname_display = format_hostname_with_scroll(&info.hostname, hostname_scroll_offset);

    // Calculate values
    let memory_gb = info.used_memory as f64 / (1024.0 * 1024.0 * 1024.0);
    let total_memory_gb = info.total_memory as f64 / (1024.0 * 1024.0 * 1024.0);
    let memory_percent = if info.total_memory > 0 {
        (info.used_memory as f64 / info.total_memory as f64) * 100.0
    } else {
        0.0
    };

    // Print info line: <device_type> <name> @ <hostname> Util:4.0% Mem:25.2/128GB Temp:0°C Pwr:0.0W
    print_colored_text(
        stdout,
        &format!("{:<5}", info.device_type),
        Color::Cyan,
        None,
        None,
    );
    print_colored_text(stdout, &device_name, Color::White, None, None);
    print_colored_text(stdout, " @ ", Color::DarkGreen, None, None);
    print_colored_text(stdout, &hostname_display, Color::White, None, None);
    print_colored_text(stdout, " Util:", Color::Yellow, None, None);
    let util_display = if info.utilization < 0.0 {
        format!("{:>6}", "N/A")
    } else {
        format!("{:>5.1}%", info.utilization)
    };
    print_colored_text(stdout, &util_display, Color::White, None, None);
    print_colored_text(stdout, " VRAM:", Color::Blue, None, None);
    let vram_display = if info.detail.get("metrics_available") == Some(&"false".to_string()) {
        format!("{:>11}", "N/A")
    } else {
        // Format total memory with proper precision: 1 decimal for sub-GB, 0 decimal for GB+
        let total_fmt = if total_memory_gb < 1.0 {
            format!("{total_memory_gb:.1}")
        } else {
            format!("{total_memory_gb:.0}")
        };
        format!("{:>11}", format!("{memory_gb:.1}/{total_fmt}GB"))
    };
    print_colored_text(stdout, &vram_display, Color::White, None, None);
    print_colored_text(stdout, " Temp:", Color::Magenta, None, None);

    // Display real GPU die temperature on every platform. Apple Silicon used
    // to fall back to the qualitative thermal pressure text because SMC float
    // decoding was broken; with the SMC `flt ` little-endian fix in place the
    // Tg* sensors return real die temperatures (~50 °C idle), so the numeric
    // reading is now meaningful and consistent with other platforms.
    let temp_display = if info.detail.get("metrics_available") == Some(&"false".to_string()) {
        format!("{:>7}", "N/A")
    } else if info.temperature == 0 {
        // SMC didn't yield a usable reading and we have no fallback — show N/A
        // rather than a misleading "0 °C".
        format!("{:>7}", "N/A")
    } else {
        format!("{:>4}°C", info.temperature)
    };

    print_colored_text(stdout, &temp_display, Color::White, None, None);

    // Display GPU frequency
    if info.frequency > 0 {
        print_colored_text(stdout, " Freq:", Color::Magenta, None, None);
        if info.frequency >= 1000 {
            print_colored_text(
                stdout,
                &format!("{:.2}GHz", info.frequency as f64 / 1000.0),
                Color::White,
                None,
                None,
            );
        } else {
            print_colored_text(
                stdout,
                &format!("{}MHz", info.frequency),
                Color::White,
                None,
                None,
            );
        }
    }

    print_colored_text(stdout, " Pwr:", Color::Red, None, None);

    // Check if power_limit_max is available and display as current/max
    // For Apple Silicon, info.power_consumption contains GPU power only
    let is_apple_silicon = info.name.contains("Apple") || info.name.contains("Metal");
    let power_display = if info.power_consumption < 0.0 {
        "N/A".to_string()
    } else if is_apple_silicon {
        // Apple Silicon GPU uses very little power, show 2 decimal places
        // Use fixed width formatting to prevent trailing characters
        format!("{:5.2}W", info.power_consumption)
    } else if let Some(power_max_str) = info.detail.get("power_limit_max") {
        if let Ok(power_max) = power_max_str.parse::<f64>() {
            format!("{:.0}/{power_max:.0}W", info.power_consumption)
        } else {
            format!("{:.0}W", info.power_consumption)
        }
    } else {
        format!("{:.0}W", info.power_consumption)
    };

    // Dynamically adjust width based on content, with minimum of 8 chars
    let display_width = power_display.len().max(8);
    print_colored_text(
        stdout,
        &format!("{power_display:>display_width$}"),
        Color::White,
        None,
        None,
    );

    // Display HLO Queue Size for TPU devices (show 0 if not available)
    if info.device_type == "TPU" {
        let hlo_queue_size = info
            .detail
            .get("HLO Queue Size")
            .map(|s| s.as_str())
            .unwrap_or("0");
        print_colored_text(stdout, " HLO Q:", Color::Cyan, None, None);
        print_colored_text(
            stdout,
            &format!("{hlo_queue_size:>3}"),
            Color::White,
            None,
            None,
        );
    }

    // Display driver version if available
    if let Some(driver_version) = info.detail.get("Driver Version") {
        print_colored_text(stdout, " Drv:", Color::Green, None, None);
        print_colored_text(stdout, driver_version, Color::White, None, None);
    }

    // Display AI library name and version using unified fields
    // Falls back to platform-specific fields for backward compatibility
    if let Some(lib_name) = info.detail.get("lib_name") {
        if let Some(lib_version) = info.detail.get("lib_version") {
            print_colored_text(stdout, &format!(" {lib_name}:"), Color::Green, None, None);
            print_colored_text(stdout, lib_version, Color::White, None, None);
        }
    } else {
        // Backward compatibility: try platform-specific fields
        if let Some(cuda_version) = info.detail.get("CUDA Version") {
            print_colored_text(stdout, " CUDA:", Color::Green, None, None);
            print_colored_text(stdout, cuda_version, Color::White, None, None);
        } else if let Some(rocm_version) = info.detail.get("ROCm Version") {
            print_colored_text(stdout, " ROCm:", Color::Green, None, None);
            print_colored_text(stdout, rocm_version, Color::White, None, None);
        }
    }

    queue!(stdout, Print("\r\n")).unwrap();

    // Calculate gauge widths with 5 char padding on each side and 2 space separation
    let available_width = width.saturating_sub(10); // 5 padding each side
    let is_apple_silicon = info.name.contains("Apple") || info.name.contains("Metal");
    let has_tensorcore = info.device_type == "TPU" && info.tensorcore_utilization.is_some();
    let num_gauges = if is_apple_silicon || has_tensorcore {
        3
    } else {
        2
    }; // Util, Mem, (ANE for Apple Silicon, TensorCore for TPU)
    let gauge_width = (available_width - (num_gauges - 1) * 2) / num_gauges; // 2 spaces between gauges

    // Calculate actual space used and dynamic right padding
    let total_gauge_width = gauge_width * num_gauges + (num_gauges - 1) * 2;
    let left_padding = 5;
    let right_padding = width - left_padding - total_gauge_width;

    // Print gauges on one line with proper spacing
    print_colored_text(stdout, "     ", Color::White, None, None); // 5 char left padding

    // Util gauge
    draw_bar(
        stdout,
        "Util",
        info.utilization,
        100.0,
        gauge_width,
        Some(format!("{:.1}%", info.utilization)),
    );
    print_colored_text(stdout, "  ", Color::White, None, None); // 2 space separator

    // Memory gauge
    draw_bar(
        stdout,
        "Mem",
        memory_percent,
        100.0,
        gauge_width,
        Some(format!("{memory_gb:.1}GB")),
    );

    // ANE gauge only for Apple Silicon (in Watts)
    if is_apple_silicon {
        print_colored_text(stdout, "  ", Color::White, None, None); // 2 space separator

        // Determine max ANE power based on die count (Ultra = 2 dies = 12W, others = 6W)
        let is_ultra = info.name.contains("Ultra");
        let max_ane_power = if is_ultra { 12.0 } else { 6.0 };

        // Convert mW to W and cap at max
        let ane_power_w = (info.ane_utilization / 1000.0).min(max_ane_power);
        let ane_percent = (ane_power_w / max_ane_power) * 100.0;

        draw_bar(
            stdout,
            "ANE",
            ane_percent,
            100.0,
            gauge_width,
            Some(format!("{ane_power_w:.1}W")),
        );
    }

    // TensorCore gauge for TPU
    if has_tensorcore {
        print_colored_text(stdout, "  ", Color::White, None, None); // 2 space separator

        let tc_util = info.tensorcore_utilization.unwrap_or(0.0);
        draw_bar(
            stdout,
            "TC",
            tc_util,
            100.0,
            gauge_width,
            Some(format!("{tc_util:.1}%")),
        );
    }

    print_colored_text(stdout, &" ".repeat(right_padding), Color::White, None, None); // dynamic right padding
    queue!(stdout, Print("\r\n")).unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_hostname_with_scroll() {
        // Test short hostname (no scrolling needed)
        assert_eq!(format_hostname_with_scroll("host", 0), "host     ");
        assert_eq!(format_hostname_with_scroll("host", 5), "host     ");

        // Test exact 9 characters
        assert_eq!(format_hostname_with_scroll("localhost", 0), "localhost");

        // Test long hostname with scrolling
        let long_hostname = "very-long-hostname";
        assert_eq!(format_hostname_with_scroll(long_hostname, 0).len(), 9);
        assert_eq!(format_hostname_with_scroll(long_hostname, 0), "very-long");
        assert_eq!(format_hostname_with_scroll(long_hostname, 5), "long-host");
        assert_eq!(format_hostname_with_scroll(long_hostname, 10), "hostname ");

        // Test scrolling wraps around
        let scroll_len = long_hostname.len() + 3;
        assert_eq!(
            format_hostname_with_scroll(long_hostname, scroll_len),
            format_hostname_with_scroll(long_hostname, 0)
        );
    }

    #[test]
    fn test_gpu_renderer_new() {
        let renderer = GpuRenderer::new();
        // Just verify it can be created
        let _ = renderer;
    }
}
