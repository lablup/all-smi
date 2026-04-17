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

//! Compact per-GPU vGPU section renderer.
//!
//! This renderer is called from the frame renderer only when a GPU has a
//! matching [`VgpuHostInfo`] record. On bare-metal hosts the entire feature
//! remains invisible — there is no header, no placeholder, no empty section.

use std::io::Write;

use crossterm::{queue, style::Color, style::Print};

use crate::device::VgpuHostInfo;
use crate::ui::renderers::utils::truncate_str;
use crate::ui::text::print_colored_text;

/// Render a compact vGPU sub-section beneath a physical GPU row.
///
/// Layout:
/// ```text
///   vGPU host: Sriov  policy=1 ARR=off (supported) instances=2
///       [0] GRID A100-8C   util:42%  mem:1.2/8GB  vm=vm-node-01
///       [1] GRID A100-2C   util: 0%  mem:0.0/2GB  vm=vm-node-02
/// ```
///
/// The function writes nothing when the host carries no vGPU data and no
/// scheduler info that would be useful to surface (`is_vgpu_active == false`).
pub fn print_vgpu_section<W: Write>(stdout: &mut W, host: &VgpuHostInfo, _width: usize) {
    if !host.is_vgpu_active() {
        return;
    }

    // Indent the entire section to visually nest under the parent GPU line.
    print_colored_text(stdout, "  vGPU host: ", Color::DarkGrey, None, None);
    print_colored_text(stdout, &host.host_mode, Color::Cyan, None, None);

    print_colored_text(stdout, "  policy=", Color::DarkGrey, None, None);
    print_colored_text(
        stdout,
        &host.scheduler_policy.to_string(),
        Color::White,
        None,
        None,
    );

    print_colored_text(stdout, " ARR=", Color::DarkGrey, None, None);
    print_colored_text(
        stdout,
        arr_label(host.scheduler_arr_mode),
        Color::White,
        None,
        None,
    );

    if host.is_arr_supported {
        print_colored_text(stdout, " (supported)", Color::DarkGreen, None, None);
    } else {
        print_colored_text(stdout, " (unsupported)", Color::DarkRed, None, None);
    }

    print_colored_text(stdout, "  instances=", Color::DarkGrey, None, None);
    print_colored_text(
        stdout,
        &host.vgpus.len().to_string(),
        Color::Yellow,
        None,
        None,
    );

    queue!(stdout, Print("\r\n")).unwrap();

    for (i, vgpu) in host.vgpus.iter().enumerate() {
        print_colored_text(stdout, "      [", Color::DarkGrey, None, None);
        print_colored_text(stdout, &i.to_string(), Color::Cyan, None, None);
        print_colored_text(stdout, "] ", Color::DarkGrey, None, None);

        let type_display = if vgpu.vgpu_type_name.is_empty() {
            "unknown"
        } else {
            vgpu.vgpu_type_name.as_str()
        };
        // Truncate long profile names to keep the row compact.
        let type_trunc = truncate_str(type_display, 20);
        print_colored_text(
            stdout,
            &format!("{type_trunc:<20}"),
            Color::White,
            None,
            None,
        );

        print_colored_text(stdout, " util:", Color::Yellow, None, None);
        let util_str = match vgpu.gpu_utilization {
            Some(u) => format!("{u:>3}%"),
            None => "  -%".to_string(),
        };
        print_colored_text(stdout, &util_str, Color::White, None, None);

        print_colored_text(stdout, "  mem:", Color::Blue, None, None);
        let used_gb = vgpu.fb_used_bytes as f64 / (1024.0 * 1024.0 * 1024.0);
        let total_gb = vgpu.fb_total_bytes as f64 / (1024.0 * 1024.0 * 1024.0);
        let mem_str = if vgpu.fb_total_bytes > 0 {
            format!("{used_gb:>4.1}/{total_gb:.1}GB")
        } else {
            format!("{used_gb:>4.1}GB")
        };
        print_colored_text(stdout, &mem_str, Color::White, None, None);

        if !vgpu.vm_id.is_empty() {
            let vm_trunc = truncate_str(&vgpu.vm_id, 24);
            print_colored_text(stdout, "  vm=", Color::DarkGrey, None, None);
            print_colored_text(stdout, &vm_trunc, Color::White, None, None);
        }

        if vgpu.is_active {
            print_colored_text(stdout, "  ●", Color::Green, None, None);
        } else {
            print_colored_text(stdout, "  ○", Color::DarkGrey, None, None);
        }

        queue!(stdout, Print("\r\n")).unwrap();
    }
}

/// Map the numeric ARR mode reported by NVML to a compact display label.
fn arr_label(arr_mode: u32) -> &'static str {
    match arr_mode {
        0 => "n/a",
        1 => "off",
        2 => "on",
        _ => "?",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::{VgpuHostInfo, VgpuInfo};
    use std::collections::HashMap;

    fn host_with(vgpus: Vec<VgpuInfo>, host_mode: &str) -> VgpuHostInfo {
        VgpuHostInfo {
            host_id: "h".to_string(),
            hostname: "h".to_string(),
            instance: "h".to_string(),
            gpu_index: 0,
            gpu_uuid: "GPU-x".to_string(),
            gpu_name: "GPU".to_string(),
            host_mode: host_mode.to_string(),
            scheduler_policy: 1,
            scheduler_arr_mode: 2,
            is_arr_supported: true,
            vgpus,
            detail: HashMap::new(),
        }
    }

    #[test]
    fn renders_nothing_for_disabled_host_with_no_instances() {
        let host = host_with(Vec::new(), "Disabled");
        let mut buf: Vec<u8> = Vec::new();
        print_vgpu_section(&mut buf, &host, 80);
        assert!(buf.is_empty(), "Disabled host with no vGPUs must be silent");
    }

    #[test]
    fn renders_section_when_host_mode_is_sriov_even_without_instances() {
        let host = host_with(Vec::new(), "Sriov");
        let mut buf: Vec<u8> = Vec::new();
        print_vgpu_section(&mut buf, &host, 80);
        let out = String::from_utf8_lossy(&buf);
        assert!(out.contains("vGPU host"));
        assert!(out.contains("Sriov"));
    }

    /// Strip common ANSI color escape sequences so tests can assert textual
    /// content without matching the interleaved control codes.
    fn strip_ansi(text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let mut chars = text.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\u{1b}' {
                // CSI: ESC '[' ... letter
                if chars.peek() == Some(&'[') {
                    chars.next();
                    for next in chars.by_ref() {
                        if next.is_ascii_alphabetic() {
                            break;
                        }
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    #[test]
    fn renders_each_vgpu_instance_row() {
        let vgpus = vec![
            VgpuInfo {
                instance_id: 0,
                uuid: "u0".into(),
                vm_id: "vm-0".into(),
                vgpu_type_name: "GRID A100-8C".into(),
                fb_used_bytes: 2 * (1 << 30),
                fb_total_bytes: 8 * (1 << 30),
                gpu_utilization: Some(42),
                memory_utilization: Some(30),
                is_active: true,
            },
            VgpuInfo {
                instance_id: 1,
                uuid: "u1".into(),
                vm_id: String::new(),
                vgpu_type_name: "GRID A100-2C".into(),
                fb_used_bytes: 0,
                fb_total_bytes: 2 * (1 << 30),
                gpu_utilization: None,
                memory_utilization: None,
                is_active: false,
            },
        ];
        let host = host_with(vgpus, "Sriov");
        let mut buf: Vec<u8> = Vec::new();
        print_vgpu_section(&mut buf, &host, 100);
        let raw = String::from_utf8_lossy(&buf);
        let plain = strip_ansi(&raw);
        assert!(plain.contains("[0]"), "missing [0] in:\n{plain}");
        assert!(plain.contains("[1]"), "missing [1] in:\n{plain}");
        assert!(plain.contains("GRID A100-8C"));
        assert!(plain.contains("GRID A100-2C"));
        // First instance has util=42 (reported), second is "-" (None).
        assert!(plain.contains(" 42%"), "missing 42% in:\n{plain}");
        assert!(plain.contains("-%"), "missing -% in:\n{plain}");
    }

    #[test]
    fn renders_nothing_when_buffer_is_unchanged_across_runs() {
        // Regression: strip_ansi helper used in tests must preserve plain
        // content. Empty input should stay empty.
        assert_eq!(strip_ansi(""), "");
        assert_eq!(strip_ansi("plain"), "plain");
        assert_eq!(strip_ansi("\x1b[31mX\x1b[0m"), "X");
    }

    #[test]
    fn arr_label_mapping_is_stable() {
        assert_eq!(arr_label(0), "n/a");
        assert_eq!(arr_label(1), "off");
        assert_eq!(arr_label(2), "on");
        assert_eq!(arr_label(99), "?");
    }
}
