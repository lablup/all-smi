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

use std::collections::HashMap;
use std::io::Write;

use crossterm::{queue, style::Color, style::Print};

use crate::device::GpuInfo;
use crate::device::MigGpuInfo;
use crate::device::VgpuHostInfo;
use crate::device::types::{NvLinkRemoteType, ThermalProximity, ThermalProximityConfig};
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

/// Compute the number of terminal lines [`print_gpu_info`] will emit for a
/// single GPU, including any optional thermal/P-state row, the extended
/// hardware-details row (issue #132), and any nested vGPU / MIG section.
///
/// Layout pieces, in render order:
///   1. The base info line (always 1 line).
///   2. The optional thermal/P-state row (1 line when any of the 5 thermal
///      fields or the P-state is populated; 0 otherwise).
///   3. The optional hardware-details row (1 line when NUMA, GSP firmware,
///      or NvLink topology is populated; 0 otherwise).
///   4. The gauge row (always 1 line).
///   5. A nested vGPU section, when a matching [`VgpuHostInfo`] is present
///      and the host is "active": 1 header line + 1 line per vGPU instance.
///   6. A nested MIG section, when a matching [`MigGpuInfo`] is present and
///      the host is "active": 1 header line + 1 line per MIG instance.
///
/// The vGPU/MIG matching here mirrors `find_matching_vgpu_host` /
/// `find_matching_mig_gpu` in `view::frame_renderer` (UUID first, then
/// hostname + gpu_name fallback) so that layout math agrees with what is
/// actually rendered.
///
/// Used by the layout calculator and the PgUp/PgDn handlers to size the
/// scrollable GPU area correctly when optional rows are present, since they
/// can grow rows from 2 to 4 lines apiece.
pub fn gpu_render_line_count(
    gpu: &GpuInfo,
    vgpu_info: &[VgpuHostInfo],
    mig_info: &[MigGpuInfo],
) -> usize {
    // Build single-use lookup maps. When called from `max_gpu_lines_over`
    // for many GPUs, prefer `gpu_render_line_count_with_lookup` instead.
    let vgpu_lookup = build_vgpu_uuid_lookup(vgpu_info);
    let mig_lookup = build_mig_uuid_lookup(mig_info);
    gpu_render_line_count_with_lookup(gpu, vgpu_info, mig_info, &vgpu_lookup, &mig_lookup)
}

/// Same as [`gpu_render_line_count`] but accepts pre-built UUID lookup maps
/// so that callers iterating over many GPUs (e.g. `max_gpu_lines_over`) pay
/// O(V+M) map construction once rather than O(G*(V+M)) linear scans per GPU.
pub fn gpu_render_line_count_with_lookup(
    gpu: &GpuInfo,
    vgpu_info: &[VgpuHostInfo],
    mig_info: &[MigGpuInfo],
    vgpu_lookup: &HashMap<&str, usize>,
    mig_lookup: &HashMap<&str, usize>,
) -> usize {
    // Base: info line + gauges line.
    let mut lines: usize = 2;

    // Optional thermal-threshold / P-state row.
    let has_thermal_or_pstate = gpu.temperature_threshold_slowdown.is_some()
        || gpu.temperature_threshold_shutdown.is_some()
        || gpu.temperature_threshold_max_operating.is_some()
        || gpu.temperature_threshold_acoustic.is_some()
        || gpu.performance_state.is_some();
    if has_thermal_or_pstate {
        lines += 1;
    }

    // Optional hardware-details row (issue #132). Rendered when NUMA,
    // GSP firmware, or NvLink topology is present. GPM metrics are shown
    // only inline on the same row if NUMA/GSP/NvLink is also present, so
    // they don't by themselves trigger the row.
    if gpu_has_hardware_details_row(gpu) {
        lines += 1;
    }

    // Optional vGPU section: header + one row per instance. O(1) UUID
    // lookup with hostname+gpu_name fallback for remote-mode data.
    let matched = vgpu_lookup
        .get(gpu.uuid.as_str())
        .map(|&idx| &vgpu_info[idx])
        .or_else(|| {
            vgpu_info
                .iter()
                .find(|v| v.hostname == gpu.hostname && v.gpu_name == gpu.name)
        });
    if let Some(host) = matched
        && host.is_vgpu_active()
    {
        lines += 1 + host.vgpus.len();
    }

    // Optional MIG section: header + one row per instance. O(1) UUID
    // lookup with hostname+gpu_name fallback for remote-mode data.
    let mig_matched = mig_lookup
        .get(gpu.uuid.as_str())
        .map(|&idx| &mig_info[idx])
        .or_else(|| {
            mig_info
                .iter()
                .find(|m| m.hostname == gpu.hostname && m.gpu_name == gpu.name)
        });
    if let Some(host) = mig_matched
        && host.is_mig_active()
    {
        lines += 1 + host.instances.len();
    }

    lines
}

/// Build a `gpu_uuid -> index` lookup map for vGPU host info. O(V) build
/// time, O(1) per-GPU lookup.
pub fn build_vgpu_uuid_lookup(vgpu_info: &[VgpuHostInfo]) -> HashMap<&str, usize> {
    let mut map = HashMap::with_capacity(vgpu_info.len());
    for (i, host) in vgpu_info.iter().enumerate() {
        map.entry(host.gpu_uuid.as_str()).or_insert(i);
    }
    map
}

/// Build a `gpu_uuid -> index` lookup map for MIG GPU info. O(M) build
/// time, O(1) per-GPU lookup.
pub fn build_mig_uuid_lookup(mig_info: &[MigGpuInfo]) -> HashMap<&str, usize> {
    let mut map = HashMap::with_capacity(mig_info.len());
    for (i, host) in mig_info.iter().enumerate() {
        map.entry(host.gpu_uuid.as_str()).or_insert(i);
    }
    map
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
    show_hostname: bool,
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

    // Calculate values
    let memory_gb = info.used_memory as f64 / (1024.0 * 1024.0 * 1024.0);
    let total_memory_gb = info.total_memory as f64 / (1024.0 * 1024.0 * 1024.0);
    let memory_percent = if info.total_memory > 0 {
        (info.used_memory as f64 / info.total_memory as f64) * 100.0
    } else {
        0.0
    };

    // Print info line: <device_type> <name> [@ <hostname>] Util:4.0% Mem:25.2/128GB Temp:0°C Pwr:0.0W
    // (The info / gauges / thermal / HW rows are produced further below.)
    print_colored_text(
        stdout,
        &format!("{:<5}", info.device_type),
        Color::Cyan,
        None,
        None,
    );
    print_colored_text(stdout, &device_name, Color::White, None, None);
    if show_hostname {
        let hostname_display = format_hostname_with_scroll(&info.hostname, hostname_scroll_offset);
        print_colored_text(stdout, " @ ", Color::DarkGreen, None, None);
        print_colored_text(stdout, &hostname_display, Color::White, None, None);
    }
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
    let (temp_display, temp_color) =
        if info.detail.get("metrics_available") == Some(&"false".to_string()) {
            (format!("{:>7}", "N/A"), Color::White)
        } else if info.temperature == 0 {
            // SMC didn't yield a usable reading and we have no fallback — show N/A
            // rather than a misleading "0 °C".
            (format!("{:>7}", "N/A"), Color::White)
        } else {
            // Highlight the current temperature when it is within the
            // configured margin of the slowdown/shutdown thresholds reported
            // by NVML. `thermal_proximity` returns `Normal` (→ white) when no
            // thresholds are available, so non-NVIDIA paths are unaffected.
            let colour = match info.thermal_proximity(ThermalProximityConfig::default()) {
                ThermalProximity::Shutdown => Color::Red,
                ThermalProximity::Slowdown => Color::Yellow,
                ThermalProximity::Normal => Color::White,
            };
            (format!("{:>4}°C", info.temperature), colour)
        };

    print_colored_text(stdout, &temp_display, temp_color, None, None);

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

    // Optional secondary row: thermal thresholds + current P-state.
    //
    // Only rendered when at least one piece of threshold/P-state data is
    // available, so Apple Silicon / AMD / Jetson rows that never populate
    // these fields keep their current two-row layout. The row is indented
    // to line up under the device name so it visually hangs off the GPU.
    render_thermal_pstate_row(stdout, info);

    // Optional tertiary row: extended hardware details (issue #132).
    //
    // Rendered when NUMA placement, GSP firmware, or NvLink topology is
    // reported. Keeps the same 5-column indent as the thermal row above.
    render_hardware_details_row(stdout, info);

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

/// Render the compact thermal-threshold / P-state row beneath a GPU. No-op
/// when the GPU reports none of the new NVML fields — so non-NVIDIA rows
/// and older drivers skip the row entirely and the TUI keeps its historical
/// two-line layout.
fn render_thermal_pstate_row<W: Write>(stdout: &mut W, info: &GpuInfo) {
    let has_any_threshold = info.temperature_threshold_slowdown.is_some()
        || info.temperature_threshold_shutdown.is_some()
        || info.temperature_threshold_max_operating.is_some()
        || info.temperature_threshold_acoustic.is_some();
    if !has_any_threshold && info.performance_state.is_none() {
        return;
    }

    // 5-char indent aligns with the gauge row below.
    print_colored_text(stdout, "     ", Color::White, None, None);

    let proximity = info.thermal_proximity(ThermalProximityConfig::default());
    let warn_color = match proximity {
        ThermalProximity::Shutdown => Some(Color::Red),
        ThermalProximity::Slowdown => Some(Color::Yellow),
        ThermalProximity::Normal => None,
    };

    // Track whether any field has already been written so that only the
    // second and subsequent fields get a leading separator space. Without
    // this, a partial report (e.g. only P-State populated) would begin the
    // row with two spaces — one from the indent above, one from the field
    // itself — corrupting alignment.
    let mut emitted_any = false;

    // Slowdown threshold — colour it yellow when the current temperature is
    // bumping up against it, red when shutdown is imminent. When no warning
    // is active, render neutrally.
    if let Some(slowdown) = info.temperature_threshold_slowdown {
        emitted_any = true;
        print_colored_text(stdout, "Slowdown:", Color::DarkYellow, None, None);
        let color = warn_color.unwrap_or(Color::White);
        print_colored_text(stdout, &format!("{slowdown}°C"), color, None, None);
    }

    if let Some(shutdown) = info.temperature_threshold_shutdown {
        if emitted_any {
            print_colored_text(stdout, " ", Color::White, None, None);
        }
        emitted_any = true;
        print_colored_text(stdout, "Shutdown:", Color::DarkRed, None, None);
        let color = match proximity {
            ThermalProximity::Shutdown => Color::Red,
            _ => Color::White,
        };
        print_colored_text(stdout, &format!("{shutdown}°C"), color, None, None);
    }

    if let Some(gpu_max) = info.temperature_threshold_max_operating {
        if emitted_any {
            print_colored_text(stdout, " ", Color::White, None, None);
        }
        emitted_any = true;
        print_colored_text(stdout, "MaxOp:", Color::DarkGreen, None, None);
        print_colored_text(stdout, &format!("{gpu_max}°C"), Color::White, None, None);
    }

    if let Some(acoustic) = info.temperature_threshold_acoustic {
        if emitted_any {
            print_colored_text(stdout, " ", Color::White, None, None);
        }
        emitted_any = true;
        print_colored_text(stdout, "Acoustic:", Color::DarkCyan, None, None);
        print_colored_text(stdout, &format!("{acoustic}°C"), Color::White, None, None);
    }

    if let Some(pstate) = info.performance_state {
        if emitted_any {
            print_colored_text(stdout, " ", Color::White, None, None);
        }
        print_colored_text(stdout, "P-State:", Color::DarkBlue, None, None);
        // Highlight P0 (maximum performance) green and P15 (idle) dim; mid
        // states render neutrally. Helps spot a throttled GPU at a glance.
        let color = match pstate {
            0 => Color::Green,
            15 => Color::DarkGrey,
            _ => Color::White,
        };
        print_colored_text(stdout, &format!("P{pstate}"), color, None, None);
    }

    queue!(stdout, Print("\r\n")).unwrap();
}

/// Return true when a GPU carries at least one hardware-detail field that
/// the TUI cares about surfacing: NUMA node, GSP firmware mode/version,
/// or NvLink topology. GPM metrics alone do NOT trigger the row since
/// they render alongside the other details when present.
fn gpu_has_hardware_details_row(gpu: &GpuInfo) -> bool {
    gpu.numa_node_id.is_some()
        || gpu.gsp_firmware_mode.is_some()
        || gpu.gsp_firmware_version.is_some()
        || !gpu.nvlink_remote_devices.is_empty()
}

/// Human-readable label for the GSP firmware mode gauge, used in the TUI
/// hardware-details row. Mirrors the `all_smi_gpu_gsp_firmware_mode` code
/// emitted by the exporter (`0=disabled`, `1=enabled`, `2=default`).
fn gsp_firmware_mode_label(code: u8) -> &'static str {
    match code {
        0 => "disabled",
        1 => "enabled",
        2 => "default",
        _ => "unknown",
    }
}

/// Render the compact hardware-details row beneath a GPU (issue #132).
///
/// Example output (trailing spaces / ANSI colour codes omitted for
/// readability):
///
/// ```text
///      HW  NUMA:0  GSP:enabled v550.54.15  NVLink:6x(gpu=5,sw=1)  GPM:SM=0.67 MemBW=0.42
/// ```
///
/// No-op when none of the issue-#132 fields are populated, so non-NVIDIA
/// rows and older drivers keep their historical layout.
fn render_hardware_details_row<W: Write>(stdout: &mut W, info: &GpuInfo) {
    if !gpu_has_hardware_details_row(info) {
        return;
    }

    // 5-char indent aligns with the other secondary rows.
    print_colored_text(stdout, "     ", Color::White, None, None);
    print_colored_text(stdout, "HW", Color::DarkMagenta, None, None);

    if let Some(numa) = info.numa_node_id {
        print_colored_text(stdout, " NUMA:", Color::DarkYellow, None, None);
        print_colored_text(stdout, &format!("{numa}"), Color::White, None, None);
    }

    if let Some(mode_code) = info.gsp_firmware_mode {
        print_colored_text(stdout, " GSP:", Color::DarkGreen, None, None);
        print_colored_text(
            stdout,
            gsp_firmware_mode_label(mode_code),
            Color::White,
            None,
            None,
        );
    }
    if let Some(ref version) = info.gsp_firmware_version {
        // Terse prefix to signal "firmware version" without stealing a
        // whole column. The value is rendered in parentheses when the
        // mode is also shown to visually group the two.
        print_colored_text(stdout, " v", Color::DarkGrey, None, None);
        print_colored_text(stdout, version, Color::White, None, None);
    }

    if !info.nvlink_remote_devices.is_empty() {
        let total = info.nvlink_remote_devices.len();
        let (gpu_count, switch_count, ibmnpu_count, unknown_count) =
            count_nvlink_remote_types(&info.nvlink_remote_devices);
        print_colored_text(stdout, " NVLink:", Color::DarkCyan, None, None);
        // Summary: "6x(gpu=5,sw=1)" — compact and still machine-parseable
        // if someone wants to grep.
        let mut parts: Vec<String> = Vec::with_capacity(4);
        if gpu_count > 0 {
            parts.push(format!("gpu={gpu_count}"));
        }
        if switch_count > 0 {
            parts.push(format!("sw={switch_count}"));
        }
        if ibmnpu_count > 0 {
            parts.push(format!("ibmnpu={ibmnpu_count}"));
        }
        if unknown_count > 0 {
            parts.push(format!("?={unknown_count}"));
        }
        let summary = if parts.is_empty() {
            format!("{total}x")
        } else {
            format!("{total}x({})", parts.join(","))
        };
        print_colored_text(stdout, &summary, Color::White, None, None);
    }

    // GPM metrics render last so they appear after the topology info.
    // Only surfaces when at least one scalar is populated — a supported-
    // but-unsampled snapshot (`Some(GpmMetrics::default())`) produces
    // nothing so the TUI never shows stale zeros.
    if let Some(ref gpm) = info.gpm_metrics
        && (gpm.sm_occupancy.is_some() || gpm.memory_bandwidth_utilization.is_some())
    {
        print_colored_text(stdout, " GPM:", Color::DarkBlue, None, None);
        if let Some(sm) = gpm.sm_occupancy {
            print_colored_text(stdout, "SM=", Color::DarkGrey, None, None);
            print_colored_text(stdout, &format!("{sm:.2}"), Color::White, None, None);
        }
        if let Some(mem) = gpm.memory_bandwidth_utilization {
            if gpm.sm_occupancy.is_some() {
                print_colored_text(stdout, " ", Color::White, None, None);
            }
            print_colored_text(stdout, "MemBW=", Color::DarkGrey, None, None);
            print_colored_text(stdout, &format!("{mem:.2}"), Color::White, None, None);
        }
    }

    queue!(stdout, Print("\r\n")).unwrap();
}

/// Tally NvLinks by remote-type classification for the summary column.
/// Returns `(gpu, switch, ibmnpu, unknown)` counts.
fn count_nvlink_remote_types(
    links: &[crate::device::NvLinkRemoteDevice],
) -> (usize, usize, usize, usize) {
    let mut gpu_count = 0;
    let mut switch_count = 0;
    let mut ibmnpu_count = 0;
    let mut unknown_count = 0;
    for link in links {
        match link.remote_type {
            NvLinkRemoteType::Gpu => gpu_count += 1,
            NvLinkRemoteType::Switch => switch_count += 1,
            NvLinkRemoteType::IbmNpu => ibmnpu_count += 1,
            NvLinkRemoteType::Unknown => unknown_count += 1,
        }
    }
    (gpu_count, switch_count, ibmnpu_count, unknown_count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn make_gpu(temp: u32) -> GpuInfo {
        GpuInfo {
            uuid: "gpu-0".to_string(),
            time: String::new(),
            name: "Test GPU".to_string(),
            device_type: "GPU".to_string(),
            host_id: "h".to_string(),
            hostname: "h".to_string(),
            instance: "h".to_string(),
            utilization: 0.0,
            ane_utilization: 0.0,
            dla_utilization: None,
            tensorcore_utilization: None,
            temperature: temp,
            used_memory: 0,
            total_memory: 0,
            frequency: 0,
            power_consumption: 0.0,
            gpu_core_count: None,
            temperature_threshold_slowdown: Some(93),
            temperature_threshold_shutdown: Some(98),
            temperature_threshold_max_operating: Some(87),
            temperature_threshold_acoustic: None,
            performance_state: Some(2),
            numa_node_id: None,
            gsp_firmware_mode: None,
            gsp_firmware_version: None,
            nvlink_remote_devices: Vec::new(),
            gpm_metrics: None,
            detail: HashMap::new(),
        }
    }

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

    // --- thermal proximity classification ---

    #[test]
    fn thermal_proximity_normal_when_far_from_thresholds() {
        let gpu = make_gpu(60);
        assert_eq!(
            gpu.thermal_proximity(ThermalProximityConfig::default()),
            ThermalProximity::Normal
        );
    }

    #[test]
    fn thermal_proximity_slowdown_within_margin() {
        // Slowdown at 93°C, margin 5°C → 88°C or higher triggers Slowdown.
        let gpu = make_gpu(89);
        assert_eq!(
            gpu.thermal_proximity(ThermalProximityConfig::default()),
            ThermalProximity::Slowdown
        );
    }

    #[test]
    fn thermal_proximity_shutdown_takes_priority_over_slowdown() {
        // Shutdown at 98°C, margin 2°C → 96°C or higher triggers Shutdown.
        // Even though slowdown also applies, shutdown wins.
        let gpu = make_gpu(97);
        assert_eq!(
            gpu.thermal_proximity(ThermalProximityConfig::default()),
            ThermalProximity::Shutdown
        );
    }

    #[test]
    fn thermal_proximity_zero_thresholds_are_ignored() {
        // Defensive: if NVML somehow reports zero, treat as "unavailable"
        // rather than classifying every temperature as at-threshold.
        let mut gpu = make_gpu(10);
        gpu.temperature_threshold_slowdown = Some(0);
        gpu.temperature_threshold_shutdown = Some(0);
        assert_eq!(
            gpu.thermal_proximity(ThermalProximityConfig::default()),
            ThermalProximity::Normal
        );
    }

    #[test]
    fn thermal_proximity_none_thresholds_are_normal() {
        let mut gpu = make_gpu(95);
        gpu.temperature_threshold_slowdown = None;
        gpu.temperature_threshold_shutdown = None;
        assert_eq!(
            gpu.thermal_proximity(ThermalProximityConfig::default()),
            ThermalProximity::Normal
        );
    }

    #[test]
    fn thermal_proximity_respects_custom_margins() {
        // With a 10°C slowdown margin, slowdown fires at 83°C given a
        // 93°C threshold.
        let gpu = make_gpu(83);
        assert_eq!(
            gpu.thermal_proximity(ThermalProximityConfig {
                slowdown_margin: 10,
                shutdown_margin: 2,
            }),
            ThermalProximity::Slowdown
        );
    }

    #[test]
    fn thermal_proximity_saturates_on_extreme_values() {
        // Defensive: malformed remote input can yield `u32::MAX` for the
        // temperature (via `saturating_u32` in the network parser) and a
        // pathological config could carry `u32::MAX` margins. The
        // computation MUST NOT panic — it should saturate instead.
        let mut gpu = make_gpu(u32::MAX);
        gpu.temperature_threshold_slowdown = Some(50);
        gpu.temperature_threshold_shutdown = Some(50);
        let cfg = ThermalProximityConfig {
            slowdown_margin: u32::MAX,
            shutdown_margin: u32::MAX,
        };
        // With saturation, both sums saturate to u32::MAX, so the shutdown
        // branch fires first and we end up classified as Shutdown.
        assert_eq!(gpu.thermal_proximity(cfg), ThermalProximity::Shutdown);
    }

    // --- render row no-op and emission checks ---

    #[test]
    fn render_thermal_pstate_row_is_noop_when_no_data() {
        let mut gpu = make_gpu(50);
        gpu.temperature_threshold_slowdown = None;
        gpu.temperature_threshold_shutdown = None;
        gpu.temperature_threshold_max_operating = None;
        gpu.temperature_threshold_acoustic = None;
        gpu.performance_state = None;
        let mut buf: Vec<u8> = Vec::new();
        render_thermal_pstate_row(&mut buf, &gpu);
        assert!(
            buf.is_empty(),
            "expected no output when nothing is reported"
        );
    }

    #[test]
    fn render_thermal_pstate_row_emits_labels_when_data_present() {
        let gpu = make_gpu(50);
        let mut buf: Vec<u8> = Vec::new();
        render_thermal_pstate_row(&mut buf, &gpu);
        let rendered = String::from_utf8(buf).expect("valid utf-8");
        assert!(rendered.contains("Slowdown:"), "{rendered}");
        assert!(rendered.contains("Shutdown:"), "{rendered}");
        assert!(rendered.contains("MaxOp:"), "{rendered}");
        assert!(rendered.contains("P-State:"), "{rendered}");
        assert!(rendered.contains("93°C"), "{rendered}");
        assert!(rendered.contains("98°C"), "{rendered}");
        assert!(rendered.contains("87°C"), "{rendered}");
        assert!(rendered.contains("P2"), "{rendered}");
    }

    #[test]
    fn render_thermal_pstate_row_emits_pstate_only_when_only_pstate_present() {
        let mut gpu = make_gpu(50);
        gpu.temperature_threshold_slowdown = None;
        gpu.temperature_threshold_shutdown = None;
        gpu.temperature_threshold_max_operating = None;
        gpu.temperature_threshold_acoustic = None;
        gpu.performance_state = Some(8);
        let mut buf: Vec<u8> = Vec::new();
        render_thermal_pstate_row(&mut buf, &gpu);
        let rendered = String::from_utf8(buf).expect("valid utf-8");
        assert!(rendered.contains("P-State:"), "{rendered}");
        assert!(rendered.contains("P8"), "{rendered}");
        assert!(
            !rendered.contains("Slowdown:"),
            "should not render Slowdown without data: {rendered}"
        );
    }

    #[test]
    fn render_pstate_only_has_no_double_leading_space() {
        // When only performance_state is populated the row must not start
        // with two consecutive spaces. The indent ("     ") is always
        // emitted, but no extra separator space should precede the first
        // field on the row.
        let mut gpu = make_gpu(50);
        gpu.temperature_threshold_slowdown = None;
        gpu.temperature_threshold_shutdown = None;
        gpu.temperature_threshold_max_operating = None;
        gpu.temperature_threshold_acoustic = None;
        gpu.performance_state = Some(3);
        let mut buf: Vec<u8> = Vec::new();
        render_thermal_pstate_row(&mut buf, &gpu);
        // Strip ANSI escape sequences to get the plain text.
        let raw = String::from_utf8(buf).expect("valid utf-8");
        // Remove all ESC [...m sequences.
        let plain: String = {
            let mut out = String::new();
            let mut chars = raw.chars().peekable();
            while let Some(c) = chars.next() {
                if c == '\x1b' {
                    // consume up to and including the final 'm'
                    for ch in chars.by_ref() {
                        if ch == 'm' {
                            break;
                        }
                    }
                } else {
                    out.push(c);
                }
            }
            out
        };
        // The row must contain "P-State:" and "P3".
        assert!(plain.contains("P-State:"), "missing P-State: in {plain:?}");
        assert!(plain.contains("P3"), "missing P3 in {plain:?}");
        // After the fixed 5-char indent the next printable character must
        // not be another space — that would indicate a double leading space.
        let after_indent = plain.trim_start_matches(' ');
        assert!(
            !after_indent.starts_with(' '),
            "double leading space detected in {plain:?}"
        );
    }

    #[test]
    fn render_thermal_pstate_row_includes_acoustic_when_present() {
        let mut gpu = make_gpu(50);
        gpu.temperature_threshold_acoustic = Some(75);
        let mut buf: Vec<u8> = Vec::new();
        render_thermal_pstate_row(&mut buf, &gpu);
        let rendered = String::from_utf8(buf).expect("valid utf-8");
        assert!(rendered.contains("Acoustic:"), "{rendered}");
        assert!(rendered.contains("75°C"), "{rendered}");
    }

    // --- gpu_render_line_count tests ---

    fn make_vgpu_host(gpu_uuid: &str, instances: usize) -> VgpuHostInfo {
        use crate::device::types::VgpuInfo;
        let vgpus = (0..instances)
            .map(|i| VgpuInfo {
                instance_id: i as u32,
                uuid: format!("vgpu-{i}"),
                vm_id: String::new(),
                vgpu_type_name: "GRID".into(),
                fb_used_bytes: 0,
                fb_total_bytes: 1 << 30,
                gpu_utilization: Some(0),
                memory_utilization: Some(0),
                is_active: true,
            })
            .collect();
        VgpuHostInfo {
            host_id: "h".to_string(),
            hostname: "h".to_string(),
            instance: "h".to_string(),
            gpu_index: 0,
            gpu_uuid: gpu_uuid.to_string(),
            gpu_name: "Test GPU".to_string(),
            host_mode: "Sriov".to_string(),
            scheduler_policy: 1,
            scheduler_arr_mode: 2,
            is_arr_supported: true,
            vgpus,
            detail: HashMap::new(),
        }
    }

    #[test]
    fn line_count_is_two_for_minimal_non_nvidia_gpu() {
        // Apple Silicon / AMD / Jetson: no thermal threshold fields, no
        // P-state, no vGPU. Layout collapses to the historical two rows.
        let mut gpu = make_gpu(40);
        gpu.temperature_threshold_slowdown = None;
        gpu.temperature_threshold_shutdown = None;
        gpu.temperature_threshold_max_operating = None;
        gpu.temperature_threshold_acoustic = None;
        gpu.performance_state = None;
        assert_eq!(gpu_render_line_count(&gpu, &[], &[]), 2);
    }

    #[test]
    fn line_count_grows_to_three_when_thermal_or_pstate_present() {
        // Each of the 5 new fields independently bumps the row count to 3.
        for setter in [
            |g: &mut GpuInfo| g.temperature_threshold_slowdown = Some(93),
            |g: &mut GpuInfo| g.temperature_threshold_shutdown = Some(98),
            |g: &mut GpuInfo| g.temperature_threshold_max_operating = Some(87),
            |g: &mut GpuInfo| g.temperature_threshold_acoustic = Some(75),
            |g: &mut GpuInfo| g.performance_state = Some(2),
        ] {
            let mut gpu = make_gpu(40);
            gpu.temperature_threshold_slowdown = None;
            gpu.temperature_threshold_shutdown = None;
            gpu.temperature_threshold_max_operating = None;
            gpu.temperature_threshold_acoustic = None;
            gpu.performance_state = None;
            setter(&mut gpu);
            assert_eq!(
                gpu_render_line_count(&gpu, &[], &[]),
                3,
                "expected 3 lines for {gpu:?}"
            );
        }
    }

    #[test]
    fn line_count_includes_vgpu_section_when_uuid_matches() {
        // Active vGPU host with N instances adds 1 header + N rows.
        let mut gpu = make_gpu(40);
        gpu.temperature_threshold_slowdown = None;
        gpu.temperature_threshold_shutdown = None;
        gpu.temperature_threshold_max_operating = None;
        gpu.temperature_threshold_acoustic = None;
        gpu.performance_state = None;
        gpu.uuid = "GPU-A".to_string();
        let host = make_vgpu_host("GPU-A", 3);
        // 2 base + 0 thermal + (1 header + 3 instances) = 6
        assert_eq!(
            gpu_render_line_count(&gpu, std::slice::from_ref(&host), &[]),
            6
        );
    }

    #[test]
    fn line_count_falls_back_to_hostname_and_name_when_uuid_does_not_match() {
        // Remote-mode metrics may carry a missing/empty UUID; the fallback
        // matcher uses hostname + gpu_name. Layout math must agree with
        // `find_matching_vgpu_host`.
        let mut gpu = make_gpu(40);
        gpu.temperature_threshold_slowdown = None;
        gpu.temperature_threshold_shutdown = None;
        gpu.temperature_threshold_max_operating = None;
        gpu.temperature_threshold_acoustic = None;
        gpu.performance_state = None;
        gpu.uuid = "GPU-A".to_string();
        gpu.hostname = "h".to_string();
        gpu.name = "Test GPU".to_string();
        // Host has a *different* uuid but matching hostname + gpu_name.
        let host = make_vgpu_host("GPU-OTHER", 2);
        assert_eq!(
            gpu_render_line_count(&gpu, std::slice::from_ref(&host), &[]),
            5
        );
    }

    #[test]
    fn line_count_ignores_disabled_vgpu_host_with_no_instances() {
        let mut gpu = make_gpu(40);
        gpu.temperature_threshold_slowdown = None;
        gpu.temperature_threshold_shutdown = None;
        gpu.temperature_threshold_max_operating = None;
        gpu.temperature_threshold_acoustic = None;
        gpu.performance_state = None;
        gpu.uuid = "GPU-A".to_string();
        let mut host = make_vgpu_host("GPU-A", 0);
        host.host_mode = "Disabled".to_string();
        // Disabled host with no instances renders nothing → count stays at 2.
        assert_eq!(
            gpu_render_line_count(&gpu, std::slice::from_ref(&host), &[]),
            2
        );
    }

    #[test]
    fn line_count_combines_thermal_and_vgpu_contributions() {
        // Realistic NVIDIA datacentre case: thermal row present and vGPU
        // host with two instances.
        let mut gpu = make_gpu(40);
        gpu.uuid = "GPU-A".to_string();
        // make_gpu sets slowdown/shutdown/max_op + P-state; that's the
        // thermal row +1 line.
        let host = make_vgpu_host("GPU-A", 2);
        // 2 base + 1 thermal + (1 header + 2 instances) = 6
        assert_eq!(
            gpu_render_line_count(&gpu, std::slice::from_ref(&host), &[]),
            6
        );
    }

    fn make_mig_host(gpu_uuid: &str, instances: usize) -> MigGpuInfo {
        use crate::device::types::MigInstanceInfo;
        let entries = (0..instances)
            .map(|i| MigInstanceInfo {
                instance_id: i as u32,
                gpu_instance_id: Some((i as u32) + 1),
                compute_instance_id: Some(0),
                uuid: format!("MIG-{i}"),
                profile_name: "1g.5gb".into(),
                utilization_gpu: Some(0),
                utilization_memory: Some(0),
                memory_used_bytes: 0,
                memory_total_bytes: 5 << 30,
            })
            .collect();
        MigGpuInfo {
            host_id: "h".to_string(),
            hostname: "h".to_string(),
            instance: "h".to_string(),
            gpu_index: 0,
            gpu_uuid: gpu_uuid.to_string(),
            gpu_name: "Test GPU".to_string(),
            mig_mode: true,
            instances: entries,
        }
    }

    #[test]
    fn line_count_includes_mig_section_when_uuid_matches() {
        let mut gpu = make_gpu(40);
        gpu.temperature_threshold_slowdown = None;
        gpu.temperature_threshold_shutdown = None;
        gpu.temperature_threshold_max_operating = None;
        gpu.temperature_threshold_acoustic = None;
        gpu.performance_state = None;
        gpu.uuid = "GPU-A".to_string();
        let mig_host = make_mig_host("GPU-A", 4);
        // 2 base + 0 thermal + (1 MIG header + 4 instances) = 7
        assert_eq!(
            gpu_render_line_count(&gpu, &[], std::slice::from_ref(&mig_host)),
            7
        );
    }

    #[test]
    fn line_count_falls_back_to_hostname_for_mig_when_uuid_missing() {
        let mut gpu = make_gpu(40);
        gpu.temperature_threshold_slowdown = None;
        gpu.temperature_threshold_shutdown = None;
        gpu.temperature_threshold_max_operating = None;
        gpu.temperature_threshold_acoustic = None;
        gpu.performance_state = None;
        gpu.uuid = "GPU-A".to_string();
        gpu.hostname = "h".to_string();
        gpu.name = "Test GPU".to_string();
        // MIG host has a different UUID but matching hostname + gpu_name.
        let mig_host = make_mig_host("GPU-OTHER", 2);
        // 2 base + (1 header + 2 instances) = 5
        assert_eq!(
            gpu_render_line_count(&gpu, &[], std::slice::from_ref(&mig_host)),
            5
        );
    }

    #[test]
    fn line_count_combines_vgpu_and_mig_contributions() {
        // Pathological but supported: a single GPU could in theory carry both
        // vGPU and MIG records (e.g. a vGPU host that scrapes a MIG-enabled
        // physical card via NVML on a passthrough-style setup). Exercise the
        // additive path.
        let mut gpu = make_gpu(40);
        gpu.temperature_threshold_slowdown = None;
        gpu.temperature_threshold_shutdown = None;
        gpu.temperature_threshold_max_operating = None;
        gpu.temperature_threshold_acoustic = None;
        gpu.performance_state = None;
        gpu.uuid = "GPU-A".to_string();
        let vgpu_host = make_vgpu_host("GPU-A", 2);
        let mig_host = make_mig_host("GPU-A", 3);
        // 2 base + 0 thermal + (1+2 vGPU) + (1+3 MIG) = 9
        assert_eq!(
            gpu_render_line_count(
                &gpu,
                std::slice::from_ref(&vgpu_host),
                std::slice::from_ref(&mig_host),
            ),
            9
        );
    }

    #[test]
    fn line_count_ignores_mig_host_with_no_instances_and_disabled_mode() {
        let mut gpu = make_gpu(40);
        gpu.temperature_threshold_slowdown = None;
        gpu.temperature_threshold_shutdown = None;
        gpu.temperature_threshold_max_operating = None;
        gpu.temperature_threshold_acoustic = None;
        gpu.performance_state = None;
        gpu.uuid = "GPU-A".to_string();
        let mut mig_host = make_mig_host("GPU-A", 0);
        mig_host.mig_mode = false;
        assert_eq!(
            gpu_render_line_count(&gpu, &[], std::slice::from_ref(&mig_host)),
            2
        );
    }

    // --- hardware-details row (issue #132) tests ---

    fn bare_gpu() -> GpuInfo {
        let mut gpu = make_gpu(40);
        gpu.temperature_threshold_slowdown = None;
        gpu.temperature_threshold_shutdown = None;
        gpu.temperature_threshold_max_operating = None;
        gpu.temperature_threshold_acoustic = None;
        gpu.performance_state = None;
        gpu
    }

    #[test]
    fn hw_row_noop_when_no_hardware_details() {
        let gpu = bare_gpu();
        let mut buf: Vec<u8> = Vec::new();
        render_hardware_details_row(&mut buf, &gpu);
        assert!(
            buf.is_empty(),
            "expected no output when no hw details populated"
        );
    }

    #[test]
    fn hw_row_renders_numa_when_populated() {
        let mut gpu = bare_gpu();
        gpu.numa_node_id = Some(1);
        let mut buf: Vec<u8> = Vec::new();
        render_hardware_details_row(&mut buf, &gpu);
        let rendered = String::from_utf8(buf).expect("valid utf-8");
        assert!(rendered.contains("HW"), "{rendered}");
        assert!(rendered.contains("NUMA:"), "{rendered}");
        assert!(rendered.contains('1'), "{rendered}");
    }

    #[test]
    fn hw_row_renders_gsp_mode_label() {
        for (code, label) in [(0u8, "disabled"), (1, "enabled"), (2, "default")] {
            let mut gpu = bare_gpu();
            gpu.gsp_firmware_mode = Some(code);
            let mut buf: Vec<u8> = Vec::new();
            render_hardware_details_row(&mut buf, &gpu);
            let rendered = String::from_utf8(buf).expect("valid utf-8");
            assert!(
                rendered.contains(label),
                "expected '{label}' in: {rendered}"
            );
        }
    }

    /// Strip ANSI escape sequences from a rendered TUI row so substring
    /// assertions aren't confused by colour control codes embedded inside
    /// labels like `v550.54.15` that get split across multiple
    /// `print_colored_text` calls.
    fn strip_ansi(raw: &str) -> String {
        let mut out = String::with_capacity(raw.len());
        let mut chars = raw.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                // Consume up to and including the terminator (e.g. 'm'
                // for CSI SGR sequences, or any letter for other CSI).
                for ch in chars.by_ref() {
                    if ch.is_ascii_alphabetic() {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    #[test]
    fn hw_row_renders_gsp_version_prefix() {
        let mut gpu = bare_gpu();
        gpu.gsp_firmware_version = Some("550.54.15".to_string());
        let mut buf: Vec<u8> = Vec::new();
        render_hardware_details_row(&mut buf, &gpu);
        let rendered = strip_ansi(&String::from_utf8(buf).expect("valid utf-8"));
        assert!(rendered.contains("v550.54.15"), "{rendered}");
    }

    #[test]
    fn hw_row_renders_nvlink_summary_with_type_counts() {
        use crate::device::{NvLinkRemoteDevice, NvLinkRemoteType};
        let mut gpu = bare_gpu();
        gpu.nvlink_remote_devices = vec![
            NvLinkRemoteDevice {
                link_index: 0,
                remote_type: NvLinkRemoteType::Gpu,
            },
            NvLinkRemoteDevice {
                link_index: 1,
                remote_type: NvLinkRemoteType::Gpu,
            },
            NvLinkRemoteDevice {
                link_index: 2,
                remote_type: NvLinkRemoteType::Gpu,
            },
            NvLinkRemoteDevice {
                link_index: 3,
                remote_type: NvLinkRemoteType::Gpu,
            },
            NvLinkRemoteDevice {
                link_index: 4,
                remote_type: NvLinkRemoteType::Gpu,
            },
            NvLinkRemoteDevice {
                link_index: 5,
                remote_type: NvLinkRemoteType::Switch,
            },
        ];
        let mut buf: Vec<u8> = Vec::new();
        render_hardware_details_row(&mut buf, &gpu);
        let rendered = String::from_utf8(buf).expect("valid utf-8");
        assert!(rendered.contains("NVLink:"), "{rendered}");
        assert!(rendered.contains("6x"), "{rendered}");
        assert!(rendered.contains("gpu=5"), "{rendered}");
        assert!(rendered.contains("sw=1"), "{rendered}");
    }

    #[test]
    fn hw_row_omits_gpm_when_only_support_probe_populated() {
        // Reader emits `Some(GpmMetrics::default())` on Hopper+ until the
        // two-sample handshake lands. Until then the TUI must not show
        // "GPM:" at all — a zero reading would be misleading.
        use crate::device::GpmMetrics;
        let mut gpu = bare_gpu();
        gpu.numa_node_id = Some(0);
        gpu.gpm_metrics = Some(GpmMetrics::default());
        let mut buf: Vec<u8> = Vec::new();
        render_hardware_details_row(&mut buf, &gpu);
        let rendered = String::from_utf8(buf).expect("valid utf-8");
        assert!(
            !rendered.contains("GPM:"),
            "GPM label should be hidden when no values sampled: {rendered}"
        );
    }

    #[test]
    fn hw_row_renders_gpm_values_when_populated() {
        use crate::device::GpmMetrics;
        let mut gpu = bare_gpu();
        gpu.numa_node_id = Some(0);
        gpu.gpm_metrics = Some(GpmMetrics {
            sm_occupancy: Some(0.67),
            memory_bandwidth_utilization: Some(0.42),
        });
        let mut buf: Vec<u8> = Vec::new();
        render_hardware_details_row(&mut buf, &gpu);
        let rendered = strip_ansi(&String::from_utf8(buf).expect("valid utf-8"));
        assert!(rendered.contains("GPM:"), "{rendered}");
        assert!(rendered.contains("SM=0.67"), "{rendered}");
        assert!(rendered.contains("MemBW=0.42"), "{rendered}");
    }

    #[test]
    fn hw_row_accounts_for_line_in_gpu_render_line_count() {
        // NUMA alone bumps the row count from 2 -> 3.
        let mut gpu = bare_gpu();
        gpu.numa_node_id = Some(0);
        assert_eq!(gpu_render_line_count(&gpu, &[], &[]), 3);
    }

    #[test]
    fn hw_row_and_thermal_row_both_counted() {
        // Both optional rows present: 2 base + thermal + hw = 4.
        let gpu = make_gpu(50); // make_gpu populates thermals + pstate
        let mut gpu = gpu;
        gpu.numa_node_id = Some(0);
        assert_eq!(gpu_render_line_count(&gpu, &[], &[]), 4);
    }

    #[test]
    fn hw_row_gpm_only_does_not_emit_row() {
        // Support-only GPM must not trigger the row — the row is reserved
        // for topology / firmware / NUMA info that a human would inspect.
        use crate::device::GpmMetrics;
        let mut gpu = bare_gpu();
        gpu.gpm_metrics = Some(GpmMetrics::default());
        assert_eq!(gpu_render_line_count(&gpu, &[], &[]), 2);
    }

    #[test]
    fn count_nvlink_remote_types_classifies_all_variants() {
        use crate::device::{NvLinkRemoteDevice, NvLinkRemoteType};
        let links = vec![
            NvLinkRemoteDevice {
                link_index: 0,
                remote_type: NvLinkRemoteType::Gpu,
            },
            NvLinkRemoteDevice {
                link_index: 1,
                remote_type: NvLinkRemoteType::Switch,
            },
            NvLinkRemoteDevice {
                link_index: 2,
                remote_type: NvLinkRemoteType::IbmNpu,
            },
            NvLinkRemoteDevice {
                link_index: 3,
                remote_type: NvLinkRemoteType::Unknown,
            },
        ];
        assert_eq!(count_nvlink_remote_types(&links), (1, 1, 1, 1));
    }
}
