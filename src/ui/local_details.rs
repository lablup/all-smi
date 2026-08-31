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

//! Non-repeating local-mode hardware details.
//!
//! The local header owns live CPU/GPU/RAM/power/temperature values and the
//! Activity panel owns their history. These rows therefore contain only
//! identity, topology, frequency, runtime, and pressure details that add new
//! information instead of echoing the same sample in another gauge.

use std::io::Write;

use crossterm::{queue, style::Color, style::Print};

use crate::device::{ChassisInfo, CpuInfo, GpuInfo, MemoryInfo};
use crate::ui::text::{display_width, print_colored_text, truncate_to_width};

const SEPARATOR: &str = "  ";

/// Render the local CPU inventory row.
pub fn print_cpu_details<W: Write>(stdout: &mut W, info: &CpuInfo, width: usize) {
    let mut used = print_identity(stdout, "CPU", Color::Cyan, &info.cpu_model, width);

    push_field(
        stdout,
        &mut used,
        width,
        "Cores ",
        Color::Green,
        &format_cpu_cores(info),
    );
    if let Some(frequency) = format_cpu_frequency(info) {
        push_field(
            stdout,
            &mut used,
            width,
            "Freq ",
            Color::Magenta,
            &frequency,
        );
    }
    push_field(
        stdout,
        &mut used,
        width,
        "Arch ",
        Color::Yellow,
        &info.architecture,
    );
    if info.socket_count > 1 {
        push_field(
            stdout,
            &mut used,
            width,
            "Sockets ",
            Color::Yellow,
            &info.socket_count.to_string(),
        );
    }
    if let Some(cache) = format_cpu_cache(info) {
        push_field(stdout, &mut used, width, "Cache ", Color::Red, &cache);
    }

    queue!(stdout, Print("\r\n")).unwrap();
}

/// Render the local GPU/NPU inventory row without the live values already
/// present in the summary and Activity panel.
pub fn print_gpu_details<W: Write>(stdout: &mut W, info: &GpuInfo, width: usize) {
    let mut used = print_identity(stdout, &info.device_type, Color::Cyan, &info.name, width);

    if let Some(cores) = info.gpu_core_count {
        push_field(
            stdout,
            &mut used,
            width,
            "Cores ",
            Color::Green,
            &cores.to_string(),
        );
    }
    if let Some(frequency) = info.frequency_reading() {
        let display = if frequency >= 1000 {
            format!("{:.2}GHz", frequency as f64 / 1000.0)
        } else {
            format!("{frequency}MHz")
        };
        push_field(stdout, &mut used, width, "Freq ", Color::Magenta, &display);
    }

    if let (Some(name), Some(version)) =
        (info.detail.get("lib_name"), info.detail.get("lib_version"))
    {
        push_field(
            stdout,
            &mut used,
            width,
            &format!("{name} "),
            Color::Green,
            version,
        );
    } else if let Some(version) = info.detail.get("CUDA Version") {
        push_field(stdout, &mut used, width, "CUDA ", Color::Green, version);
    } else if let Some(version) = info.detail.get("ROCm Version") {
        push_field(stdout, &mut used, width, "ROCm ", Color::Green, version);
    }

    if let Some(version) = info.detail.get("Driver Version") {
        push_field(
            stdout,
            &mut used,
            width,
            "Driver ",
            Color::DarkGreen,
            version,
        );
    }

    queue!(stdout, Print("\r\n")).unwrap();
}

/// Render chassis-only details that are not already represented by the local
/// summary. Returns whether a row was emitted.
pub fn print_chassis_details<W: Write>(stdout: &mut W, info: &ChassisInfo, width: usize) -> bool {
    let cpu_power = detail_number(info, "cpu_power_watts");
    let gpu_power = detail_number(info, "gpu_power_watts");
    let ane_power = detail_number(info, "ane_power_watts");
    if info.thermal_pressure.is_none()
        && cpu_power.is_none()
        && gpu_power.is_none()
        && ane_power.is_none()
        && info.fan_speeds.is_empty()
        && info.psu_status.is_empty()
    {
        return false;
    }

    let mut used = print_label(stdout, "System", Color::Yellow, width);
    if let Some(pressure) = &info.thermal_pressure {
        push_field(
            stdout,
            &mut used,
            width,
            "Thermal ",
            Color::Magenta,
            pressure,
        );
    }
    if let Some(power) = cpu_power {
        push_field(
            stdout,
            &mut used,
            width,
            "CPU ",
            Color::Cyan,
            &format!("{power:.1}W"),
        );
    }
    if let Some(power) = gpu_power {
        push_field(
            stdout,
            &mut used,
            width,
            "GPU ",
            Color::Green,
            &format!("{power:.1}W"),
        );
    }
    if let Some(power) = ane_power {
        push_field(
            stdout,
            &mut used,
            width,
            "ANE ",
            Color::Blue,
            &format!("{power:.1}W"),
        );
    }
    if !info.fan_speeds.is_empty() {
        let average = info.fan_speeds.iter().map(|fan| fan.speed_rpm).sum::<u32>()
            / info.fan_speeds.len() as u32;
        push_field(
            stdout,
            &mut used,
            width,
            "Fans ",
            Color::Cyan,
            &format!("{average}RPM"),
        );
    }
    if !info.psu_status.is_empty() {
        let healthy = info
            .psu_status
            .iter()
            .filter(|psu| psu.status == crate::device::PsuStatus::Ok)
            .count();
        push_field(
            stdout,
            &mut used,
            width,
            "PSU ",
            Color::Yellow,
            &format!("{healthy}/{}", info.psu_status.len()),
        );
    }

    queue!(stdout, Print("\r\n")).unwrap();
    true
}

/// Swap is not represented in the top RAM summary, so surface it only when a
/// swap device actually exists. A compact ratio is enough; repeating another
/// full-width memory gauge would recreate the visual redundancy this module
/// exists to remove.
pub fn print_memory_details<W: Write>(stdout: &mut W, info: &MemoryInfo, width: usize) {
    if info.swap_total_bytes == 0 {
        return;
    }

    let gib = 1024.0 * 1024.0 * 1024.0;
    let used_gb = info.swap_used_bytes as f64 / gib;
    let total_gb = info.swap_total_bytes as f64 / gib;
    let utilization = info.swap_used_bytes as f64 / info.swap_total_bytes as f64 * 100.0;
    let mut used = print_label(stdout, "Swap", Color::Magenta, width);
    push_field(
        stdout,
        &mut used,
        width,
        "Used ",
        Color::Red,
        &format!("{used_gb:.1}/{total_gb:.1}GB"),
    );
    push_field(
        stdout,
        &mut used,
        width,
        "Util ",
        Color::Magenta,
        &format!("{utilization:.1}%"),
    );
    queue!(stdout, Print("\r\n")).unwrap();
}

fn print_identity<W: Write>(
    stdout: &mut W,
    label: &str,
    color: Color,
    name: &str,
    width: usize,
) -> usize {
    let mut used = print_label(stdout, label, color, width);
    if used >= width {
        return used;
    }

    print_colored_text(stdout, " ", Color::White, None, None);
    used += 1;
    let name_budget = width.saturating_sub(used).min(24);
    let display = truncate_to_width(name, name_budget);
    print_colored_text(stdout, &display, Color::White, None, None);
    used + display_width(&display)
}

fn print_label<W: Write>(stdout: &mut W, label: &str, color: Color, width: usize) -> usize {
    let display = truncate_to_width(label, width);
    print_colored_text(stdout, &display, color, None, None);
    display_width(&display)
}

fn push_field<W: Write>(
    stdout: &mut W,
    used: &mut usize,
    width: usize,
    label: &str,
    color: Color,
    value: &str,
) {
    let required = display_width(SEPARATOR) + display_width(label) + display_width(value);
    if used.saturating_add(required) > width {
        return;
    }

    print_colored_text(stdout, SEPARATOR, Color::DarkGrey, None, None);
    print_colored_text(stdout, label, color, None, None);
    print_colored_text(stdout, value, Color::White, None, None);
    *used += required;
}

fn format_cpu_cores(info: &CpuInfo) -> String {
    if let Some(apple) = &info.apple_silicon_info {
        if apple.s_core_count > 0 {
            format!("{}S+{}P", apple.s_core_count, apple.p_core_count)
        } else {
            format!("{}P+{}E", apple.p_core_count, apple.e_core_count)
        }
    } else {
        info.total_cores.to_string()
    }
}

fn format_cpu_frequency(info: &CpuInfo) -> Option<String> {
    if let Some(apple) = &info.apple_silicon_info {
        let (high, low) = if apple.s_core_count > 0 {
            (apple.s_cluster_frequency_mhz, apple.p_cluster_frequency_mhz)
        } else {
            (apple.p_cluster_frequency_mhz, apple.e_cluster_frequency_mhz)
        };
        if let (Some(high), Some(low)) = (high, low) {
            return Some(format_frequency_pair(high, low));
        }
    }

    (info.max_frequency_mhz > 0)
        .then(|| format!("{:.1}GHz", info.max_frequency_mhz as f64 / 1000.0))
}

fn format_frequency_pair(high: u32, low: u32) -> String {
    let format_one = |mhz: u32| {
        if mhz >= 1000 {
            format!("{:.2}GHz", mhz as f64 / 1000.0)
        } else {
            format!("{mhz}MHz")
        }
    };
    format!("{}+{}", format_one(high), format_one(low))
}

fn format_cpu_cache(info: &CpuInfo) -> Option<String> {
    if let Some(apple) = &info.apple_silicon_info {
        let (high, low) = if apple.s_core_count > 0 {
            (apple.s_core_l2_cache_mb, apple.p_core_l2_cache_mb)
        } else {
            (apple.p_core_l2_cache_mb, apple.e_core_l2_cache_mb)
        };
        if let (Some(high), Some(low)) = (high, low) {
            return Some(format!("L2 {high}+{low}MB"));
        }
    }

    (info.cache_size_mb > 0).then(|| format!("{}MB", info.cache_size_mb))
}

fn detail_number(info: &ChassisInfo, key: &str) -> Option<f64> {
    info.detail.get(key).and_then(|value| value.parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    use crate::device::{CpuPlatformType, GPU_METRIC_UNAVAILABLE};
    use crate::ui::text::ansi_display_width;

    fn gpu() -> GpuInfo {
        GpuInfo {
            uuid: "gpu-0".to_string(),
            time: String::new(),
            name: "A GPU With An Extremely Long Product Name".to_string(),
            device_type: "GPU".to_string(),
            host_id: "localhost".to_string(),
            hostname: "localhost".to_string(),
            instance: "localhost".to_string(),
            utilization: 50.0,
            ane_utilization: GPU_METRIC_UNAVAILABLE,
            dla_utilization: None,
            tensorcore_utilization: None,
            temperature: 55,
            used_memory: 8,
            total_memory: 16,
            frequency: 1500,
            power_consumption: 100.0,
            gpu_core_count: Some(80),
            temperature_threshold_slowdown: None,
            temperature_threshold_shutdown: None,
            temperature_threshold_max_operating: None,
            temperature_threshold_acoustic: None,
            performance_state: None,
            fan_speed_rpm: None,
            numa_node_id: None,
            gsp_firmware_mode: None,
            gsp_firmware_version: None,
            nvlink_remote_devices: Vec::new(),
            gpm_metrics: None,
            detail: HashMap::from([
                ("lib_name".to_string(), "CUDA".to_string()),
                ("lib_version".to_string(), "13.0".to_string()),
            ]),
        }
    }

    fn cpu() -> CpuInfo {
        CpuInfo {
            index: 0,
            host_id: "localhost".to_string(),
            hostname: "localhost".to_string(),
            instance: "localhost".to_string(),
            cpu_model: "A CPU With An Extremely Long Product Name".to_string(),
            architecture: "aarch64".to_string(),
            platform_type: CpuPlatformType::AppleSilicon,
            socket_count: 1,
            total_cores: 32,
            total_threads: 32,
            base_frequency_mhz: 0,
            max_frequency_mhz: 3500,
            cache_size_mb: 16,
            utilization: 50.0,
            temperature: Some(55),
            power_consumption: Some(20.0),
            per_socket_info: Vec::new(),
            apple_silicon_info: None,
            per_core_utilization: Vec::new(),
            time: String::new(),
        }
    }

    fn visible_lines(rendered: &[u8]) -> Vec<String> {
        String::from_utf8_lossy(rendered)
            .split("\r\n")
            .filter(|line| !line.is_empty())
            .map(ToString::to_string)
            .collect()
    }

    #[test]
    fn detail_rows_never_exceed_their_width() {
        for width in [20, 40, 80, 120] {
            let mut rendered = Vec::new();
            print_cpu_details(&mut rendered, &cpu(), width);
            print_gpu_details(&mut rendered, &gpu(), width);
            for line in visible_lines(&rendered) {
                assert!(
                    ansi_display_width(&line) <= width,
                    "line exceeded width {width}: {line:?}"
                );
            }
        }
    }

    #[test]
    fn local_gpu_details_omit_summary_metrics() {
        let mut rendered = Vec::new();
        print_gpu_details(&mut rendered, &gpu(), 120);
        let visible = String::from_utf8_lossy(&rendered);

        assert!(visible.contains("Freq"));
        assert!(visible.contains("CUDA"));
        for repeated in ["Util:", "VRAM:", "Temp:", "Pwr:"] {
            assert!(
                !visible.contains(repeated),
                "unexpected {repeated}: {visible}"
            );
        }
    }
}
