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

//! Compact per-GPU MIG section renderer.
//!
//! Mirrors the vGPU renderer in [`super::vgpu_renderer`]: called from the
//! frame renderer only when a GPU has a matching [`MigGpuInfo`] record.
//! Non-MIG GPUs receive zero output.

use std::io::Write;

use crossterm::{queue, style::Color, style::Print};

use crate::device::MigGpuInfo;
use crate::ui::text::print_colored_text;

/// Render a compact MIG sub-section beneath a physical GPU row.
///
/// Layout:
/// ```text
///   MIG host: enabled  instances=3
///       [0] 1g.5gb   util:42%  mem: 1.2/5.0GB  gi=7  ci=0
///       [1] 2g.10gb  util:18%  mem: 4.0/10.0GB gi=2  ci=0
///       [2] 7g.40gb  util: 0%  mem: 0.0/40.0GB
/// ```
///
/// Writes nothing when the host carries no MIG-related data.
pub fn print_mig_section<W: Write>(stdout: &mut W, host: &MigGpuInfo, _width: usize) {
    if !host.is_mig_active() {
        return;
    }

    print_colored_text(stdout, "  MIG host: ", Color::DarkGrey, None, None);
    if host.mig_mode {
        print_colored_text(stdout, "enabled", Color::Green, None, None);
    } else {
        print_colored_text(stdout, "disabled", Color::DarkRed, None, None);
    }

    print_colored_text(stdout, "  instances=", Color::DarkGrey, None, None);
    print_colored_text(
        stdout,
        &host.instances.len().to_string(),
        Color::Yellow,
        None,
        None,
    );

    queue!(stdout, Print("\r\n")).unwrap();

    for (i, inst) in host.instances.iter().enumerate() {
        print_colored_text(stdout, "      [", Color::DarkGrey, None, None);
        print_colored_text(stdout, &i.to_string(), Color::Cyan, None, None);
        print_colored_text(stdout, "] ", Color::DarkGrey, None, None);

        let profile_display = if inst.profile_name.is_empty() {
            "unknown"
        } else {
            inst.profile_name.as_str()
        };
        let profile_trunc = truncate_str(profile_display, 10);
        print_colored_text(
            stdout,
            &format!("{profile_trunc:<10}"),
            Color::White,
            None,
            None,
        );

        print_colored_text(stdout, " util:", Color::Yellow, None, None);
        let util_str = match inst.utilization_gpu {
            Some(u) => format!("{u:>3}%"),
            None => "  -%".to_string(),
        };
        print_colored_text(stdout, &util_str, Color::White, None, None);

        print_colored_text(stdout, "  mem:", Color::Blue, None, None);
        let used_gb = inst.memory_used_bytes as f64 / (1024.0 * 1024.0 * 1024.0);
        let total_gb = inst.memory_total_bytes as f64 / (1024.0 * 1024.0 * 1024.0);
        let mem_str = if inst.memory_total_bytes > 0 {
            format!("{used_gb:>5.1}/{total_gb:.1}GB")
        } else {
            format!("{used_gb:>5.1}GB")
        };
        print_colored_text(stdout, &mem_str, Color::White, None, None);

        if let Some(gi) = inst.gpu_instance_id {
            print_colored_text(stdout, "  gi=", Color::DarkGrey, None, None);
            print_colored_text(stdout, &gi.to_string(), Color::White, None, None);
        }
        if let Some(ci) = inst.compute_instance_id {
            print_colored_text(stdout, "  ci=", Color::DarkGrey, None, None);
            print_colored_text(stdout, &ci.to_string(), Color::White, None, None);
        }

        queue!(stdout, Print("\r\n")).unwrap();
    }
}

/// Truncate a display string on char boundaries to the given max char count.
/// Appends an ellipsis when truncation actually occurred.
fn truncate_str(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::{MigGpuInfo, MigInstanceInfo};

    fn host_with(instances: Vec<MigInstanceInfo>, mig_mode: bool) -> MigGpuInfo {
        MigGpuInfo {
            host_id: "h".to_string(),
            hostname: "h".to_string(),
            instance: "h".to_string(),
            gpu_index: 0,
            gpu_uuid: "GPU-x".to_string(),
            gpu_name: "GPU".to_string(),
            mig_mode,
            instances,
        }
    }

    /// Strip common ANSI color escape sequences so tests can assert textual
    /// content without matching the interleaved control codes.
    fn strip_ansi(text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let mut chars = text.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\u{1b}' {
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
    fn renders_nothing_when_no_instances_and_mode_disabled() {
        let host = host_with(Vec::new(), false);
        let mut buf: Vec<u8> = Vec::new();
        print_mig_section(&mut buf, &host, 80);
        assert!(
            buf.is_empty(),
            "Disabled host with no instances must be silent"
        );
    }

    #[test]
    fn renders_section_when_mig_mode_is_enabled_even_without_instances() {
        let host = host_with(Vec::new(), true);
        let mut buf: Vec<u8> = Vec::new();
        print_mig_section(&mut buf, &host, 80);
        let out = strip_ansi(&String::from_utf8_lossy(&buf));
        assert!(out.contains("MIG host"));
        assert!(out.contains("enabled"));
        assert!(out.contains("instances=0"));
    }

    #[test]
    fn renders_each_mig_instance_row() {
        let instances = vec![
            MigInstanceInfo {
                instance_id: 0,
                gpu_instance_id: Some(7),
                compute_instance_id: Some(0),
                uuid: "MIG-0".into(),
                profile_name: "1g.5gb".into(),
                utilization_gpu: Some(42),
                utilization_memory: Some(30),
                memory_used_bytes: 2 * (1 << 30),
                memory_total_bytes: 5 * (1 << 30),
            },
            MigInstanceInfo {
                instance_id: 1,
                gpu_instance_id: None,
                compute_instance_id: None,
                uuid: "MIG-1".into(),
                profile_name: "2g.10gb".into(),
                utilization_gpu: None,
                utilization_memory: None,
                memory_used_bytes: 0,
                memory_total_bytes: 10 * (1 << 30),
            },
        ];
        let host = host_with(instances, true);
        let mut buf: Vec<u8> = Vec::new();
        print_mig_section(&mut buf, &host, 100);
        let plain = strip_ansi(&String::from_utf8_lossy(&buf));

        assert!(plain.contains("[0]"), "missing [0] in:\n{plain}");
        assert!(plain.contains("[1]"), "missing [1] in:\n{plain}");
        assert!(plain.contains("1g.5gb"));
        assert!(plain.contains("2g.10gb"));
        assert!(plain.contains(" 42%"), "missing 42% in:\n{plain}");
        assert!(plain.contains("-%"), "missing -% in:\n{plain}");
        // gi/ci visible only on the instance that reports them.
        assert!(plain.contains("gi=7"), "missing gi=7 in:\n{plain}");
        assert!(plain.contains("ci=0"));
    }

    #[test]
    fn renders_unknown_profile_when_name_empty() {
        let instances = vec![MigInstanceInfo {
            instance_id: 0,
            gpu_instance_id: None,
            compute_instance_id: None,
            uuid: String::new(),
            profile_name: String::new(),
            utilization_gpu: Some(1),
            utilization_memory: Some(1),
            memory_used_bytes: 0,
            memory_total_bytes: 0,
        }];
        let host = host_with(instances, true);
        let mut buf: Vec<u8> = Vec::new();
        print_mig_section(&mut buf, &host, 80);
        let plain = strip_ansi(&String::from_utf8_lossy(&buf));
        assert!(plain.contains("unknown"));
    }

    #[test]
    fn truncate_str_preserves_short_strings() {
        assert_eq!(truncate_str("abc", 10), "abc");
        assert_eq!(truncate_str("abcdefghij", 10), "abcdefghij");
    }

    #[test]
    fn truncate_str_trims_and_appends_ellipsis() {
        let out = truncate_str("abcdefghij", 5);
        assert_eq!(out.chars().count(), 5);
        assert!(out.ends_with('…'));
    }
}
