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

use crate::storage::info::StorageInfo;
use crate::ui::text::{print_colored_text, truncate_to_width};
use crate::ui::widgets::draw_bar;

/// Storage renderer struct implementing the DeviceRenderer trait
#[allow(dead_code)]
pub struct StorageRenderer;

impl Default for StorageRenderer {
    fn default() -> Self {
        Self::new()
    }
}

#[allow(dead_code)]
impl StorageRenderer {
    pub fn new() -> Self {
        Self
    }
}

/// Helper function to format hostname with scrolling
fn format_hostname_with_scroll(hostname: &str, scroll_offset: usize) -> String {
    if hostname.len() > 9 {
        let scroll_len = hostname.len() + 3;
        let start_pos = scroll_offset % scroll_len;
        let extended_hostname = format!("{hostname}   {hostname}");
        extended_hostname
            .chars()
            .skip(start_pos)
            .take(9)
            .collect::<String>()
    } else {
        // Always return 9 characters, left-aligned with space padding
        format!("{hostname:<9}")
    }
}

/// Render storage information including mount point, total space, used space, and utilization
pub fn print_storage_info<W: Write>(
    stdout: &mut W,
    _index: usize,
    info: &StorageInfo,
    width: usize,
    hostname_scroll_offset: usize,
    show_hostname: bool,
) {
    // Convert bytes to appropriate units
    let total_gb = info.total_bytes as f64 / (1024.0 * 1024.0 * 1024.0);
    let available_gb = info.available_bytes as f64 / (1024.0 * 1024.0 * 1024.0);
    let used_gb = total_gb - available_gb;

    // Calculate usage percentage
    let usage_percent = if total_gb > 0.0 {
        (used_gb / total_gb) * 100.0
    } else {
        0.0
    };

    // Format size with appropriate units
    let format_size = |gb: f64| -> String {
        if gb >= 1024.0 {
            format!("{:.1}TB", gb / 1024.0)
        } else {
            format!("{gb:.0}GB")
        }
    };

    // Print Disk info line
    print_colored_text(stdout, "Disk ", Color::Cyan, None, None);
    print_colored_text(
        stdout,
        &format!("{:<15}", truncate_to_width(&info.mount_point, 15)),
        Color::White,
        None,
        None,
    );
    if show_hostname {
        let hostname_display = format_hostname_with_scroll(&info.hostname, hostname_scroll_offset);
        print_colored_text(stdout, " @ ", Color::DarkGreen, None, None);
        print_colored_text(stdout, &hostname_display, Color::White, None, None);
    }
    print_colored_text(stdout, " Total:", Color::Green, None, None);
    print_colored_text(
        stdout,
        &format!("{:>8}", format_size(total_gb)),
        Color::White,
        None,
        None,
    );
    print_colored_text(stdout, " Used:", Color::Red, None, None);
    print_colored_text(
        stdout,
        &format!("{:>8}", format_size(used_gb)),
        Color::White,
        None,
        None,
    );
    print_colored_text(stdout, " Util:", Color::Magenta, None, None);
    print_colored_text(
        stdout,
        &format!("{usage_percent:>5.1}%"),
        Color::White,
        None,
        None,
    );
    queue!(stdout, Print("\r\n")).unwrap();

    // Calculate gauge widths with 5 char padding on each side
    let available_width = width.saturating_sub(10); // 5 padding each side

    let gauge_width = available_width;

    // Calculate actual space used and dynamic right padding
    let total_gauge_width = gauge_width;
    let left_padding = 5;
    let right_padding = width - left_padding - total_gauge_width;

    print_colored_text(stdout, "     ", Color::White, None, None); // 5 char left padding

    // Just Used gauge (matching the other lists format)
    draw_bar(
        stdout,
        "Used",
        usage_percent,
        100.0,
        gauge_width,
        Some(format_size(used_gb)),
    );

    print_colored_text(stdout, &" ".repeat(right_padding), Color::White, None, None); // dynamic right padding
    queue!(stdout, Print("\r\n")).unwrap();
}
