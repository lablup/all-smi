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

use crate::app_state::AppState;
use crossterm::style::{Color, Stylize};

/// Generate a full-screen, colorful help interface with three sections:
/// 1. Top: Title section with ASCII logo
/// 2. Middle: Keyboard shortcuts cheat sheet  
/// 3. Bottom: Terminal usage options
pub fn generate_help_popup_content(
    cols: u16,
    rows: u16,
    state: &AppState,
    is_remote: bool,
) -> String {
    let width = cols as usize;
    let height = rows as usize;

    let mut content = String::new();

    // Create full-screen help with window border
    for row in 0..height {
        let line = match row {
            0 => format!("╔{}╗", "═".repeat(width.saturating_sub(2))),
            r if r == height - 1 => format!("╚{}╝", "═".repeat(width.saturating_sub(2))),
            _ => {
                let inner_content = get_row_content(row, width, height, state, is_remote);
                // Ensure content exactly fills the space between borders
                let padded_content = pad_content_to_width(inner_content, width.saturating_sub(2));
                format!("║{padded_content}║")
            }
        };

        content.push_str(&line);
        if row < height - 1 {
            content.push('\n');
        }
    }

    content
}

fn pad_content_to_width(content: String, target_width: usize) -> String {
    // Calculate display width correctly for Unicode characters
    let display_width = calculate_display_width(&content);

    if display_width >= target_width {
        // If content is too long, just return as-is since our content should fit
        content
    } else {
        // Pad with spaces to reach target width
        format!("{content}{}", " ".repeat(target_width - display_width))
    }
}

fn calculate_display_width(text: &str) -> usize {
    // First strip ANSI escape codes
    let clean_text = strip_ansi_codes(text);

    // Count display width of each character
    let mut width = 0;
    for ch in clean_text.chars() {
        width += char_display_width(ch);
    }
    width
}

fn char_display_width(ch: char) -> usize {
    match ch {
        // Box drawing characters (ALL-SMI logo)
        '█' | '╔' | '╗' | '╚' | '╝' | '║' | '═' | '╭' | '╮' | '╰' | '╯' | '│' | '─' | '┌' | '┐'
        | '└' | '┘' | '├' | '┤' | '┬' | '┴' | '┼' => 1,

        // Arrow characters
        '←' | '→' | '↑' | '↓' => 1,

        // Regular ASCII characters
        c if c.is_ascii() => 1,

        // Most other Unicode characters (default to width 1 for terminal safety)
        _ => 1,
    }
}

fn get_row_content(
    row: usize,
    width: usize,
    height: usize,
    state: &AppState,
    is_remote: bool,
) -> String {
    let content_width = width.saturating_sub(2); // Account for border

    // Define section boundaries
    let title_start = 2;
    let title_end = 12;
    let shortcuts_start = 14;
    let shortcuts_end = height.saturating_sub(16); // Reserve space for terminal section
    let terminal_start = shortcuts_end + 1;

    if row >= title_start && row <= title_end {
        // TOP SECTION: Title and Logo
        render_title_section(row - title_start, content_width)
    } else if row >= shortcuts_start && row < shortcuts_end {
        // MIDDLE SECTION: Keyboard Shortcuts
        render_shortcuts_section(row - shortcuts_start, content_width, state, is_remote)
    } else if row >= terminal_start && row < height - 1 {
        // BOTTOM SECTION: Terminal Usage
        render_terminal_section(row - terminal_start, content_width)
    } else {
        // Empty spacer lines
        " ".repeat(content_width)
    }
}

fn render_title_section(line_idx: usize, width: usize) -> String {
    let title_lines = [
        "",
        "    █████╗ ██╗     ██╗          ███████╗███╗   ███╗██╗",
        "   ██╔══██╗██║     ██║          ██╔════╝████╗ ████║██║",
        "   ███████║██║     ██║    █████╗███████╗██╔████╔██║██║",
        "   ██╔══██║██║     ██║    ╚════╝╚════██║██║╚██╔╝██║██║",
        "   ██║  ██║███████╗███████╗     ███████║██║ ╚═╝ ██║██║",
        "   ╚═╝  ╚═╝╚══════╝╚══════╝     ╚══════╝╚═╝     ╚═╝╚═╝",
        "",
        "GPU Monitoring and Management Tool",
        "",
    ];

    let description_lines = ["Developed and maintained as part of the Backend.AI project."];

    if line_idx < title_lines.len() {
        center_text_colored(title_lines[line_idx], width, Color::Cyan)
    } else if line_idx < title_lines.len() + description_lines.len() {
        center_text_colored(
            description_lines[line_idx - title_lines.len()],
            width,
            Color::Green,
        )
    } else {
        " ".repeat(width)
    }
}

fn render_shortcuts_section(
    line_idx: usize,
    width: usize,
    state: &AppState,
    is_remote: bool,
) -> String {
    // Split content into left and right columns
    let mut left_column = vec![
        ("Navigation Keys:", "", "header"),
        (
            "  ← →",
            "Switch tabs (remote) / Scroll process list (local)",
            "shortcut",
        ),
        ("  ↑ ↓", "Scroll up/down in lists", "shortcut"),
        ("  PgUp PgDn", "Page up/down navigation", "shortcut"),
        ("  Home End", "Jump to top/bottom", "shortcut"),
        ("", "", ""),
        ("Display Control:", "", "header"),
        ("  H", "Toggle this help screen", "shortcut"),
        ("  C", "Toggle per-core CPU display", "shortcut"),
        ("  F", "Toggle GPU process filter", "shortcut"),
        ("  Q", "Exit application", "shortcut"),
        ("  ESC", "Close help or exit", "shortcut"),
        ("", "", ""),
        ("Data Sorting:", "", "header"),
        ("  D", "Sort by default (hostname+index)", "shortcut"),
        ("  U", "Sort by GPU utilization", "shortcut"),
        ("  G", "Sort by GPU memory usage", "shortcut"),
    ];

    // Add mode-specific shortcuts
    if !is_remote {
        left_column.extend(vec![
            ("  P", "Sort processes by PID", "shortcut"),
            ("  M", "Sort processes by memory", "shortcut"),
        ]);
    }

    left_column.extend(vec![
        ("", "", ""),
        ("Process View Columns:", "", "header"),
        ("  PID", "Process ID", "legend"),
        ("  USER", "Process owner", "legend"),
        ("  PRI", "Priority (0-139, lower is higher)", "legend"),
        ("  NI", "Nice value (-20 to 19)", "legend"),
        ("  VIRT", "Virtual memory size", "legend"),
        ("  RES", "Resident memory size", "legend"),
        ("  S", "Process state (R/S/D/Z/T)", "legend"),
        ("  CPU%", "CPU utilization", "legend"),
        ("  MEM%", "Memory utilization", "legend"),
        ("  GPU%", "GPU utilization (if available)", "legend"),
        ("  VRAM", "GPU memory usage", "legend"),
        ("  TIME+", "Total CPU time used", "legend"),
        ("  Command", "Command line (← → to scroll)", "legend"),
    ]);

    let mut right_column = vec![
        ("Process Color Legend:", "", "header"),
        ("  Your processes", "White text", "legend"),
        ("  Root/unknown", "Dark grey text", "legend"),
        ("  High usage", "Red/Yellow based on CPU/Memory %", "legend"),
        (
            "  GPU processes",
            "Green/Cyan based on system load",
            "legend",
        ),
        ("", "", ""),
        ("Resource Gauge Legend:", "", "header"),
        (
            "  Memory gauge:",
            "[used/buffers/cache                    used%]",
            "membar",
        ),
        ("", "", ""),
        ("", "", ""),
        ("Current Status:", "", "header"),
    ];

    // Add current sort status
    let sort_status = get_current_sort_status(&state.sort_criteria);
    right_column.push(("  Sort mode:", &sort_status, "status"));

    // Add filter status
    let filter_status = if state.gpu_filter_enabled {
        "GPU Only"
    } else {
        "All Processes"
    };
    right_column.push(("  Filter:", filter_status, "status"));

    // Handle special rows
    match line_idx {
        0 => center_text_colored("KEYBOARD SHORTCUTS & NAVIGATION", width, Color::Yellow),
        1 => "═".repeat(width).with(Color::DarkGrey).to_string(),
        2 => " ".repeat(width),
        _ => {
            let content_line = line_idx - 3; // Adjust for title and separator
            let column_width = (width - 4) / 2; // Leave space for middle separator

            // Get content from both columns
            let left_content = if content_line < left_column.len() {
                let (key, desc, style) = &left_column[content_line];
                format_shortcut_line(key, desc, style, column_width)
            } else {
                " ".repeat(column_width)
            };

            let right_content = if content_line < right_column.len() {
                let (key, desc, style) = &right_column[content_line];
                format_shortcut_line(key, desc, style, column_width)
            } else {
                " ".repeat(column_width)
            };

            // Combine columns with separator
            format!("{left_content}  │  {right_content}")
        }
    }
}

fn render_terminal_section(line_idx: usize, width: usize) -> String {
    let terminal_lines = vec![
        ("", "TERMINAL USAGE OPTIONS", "title"),
        ("", "", "separator"),
        ("", "", ""),
        ("Local Monitoring:", "", "header"),
        ("  all-smi", "Monitor local GPUs (default mode)", "command"),
        (
            "  sudo all-smi local",
            "Monitor local GPUs (requires sudo on macOS)",
            "command",
        ),
        ("", "", ""),
        ("Remote Monitoring:", "", "header"),
        (
            "  all-smi view --hosts http://node1:9090",
            "Monitor specific remote hosts",
            "command",
        ),
        (
            "  all-smi view --hostfile hosts.csv",
            "Monitor hosts from CSV file",
            "command",
        ),
        ("", "", ""),
        ("API Server Mode:", "", "header"),
        (
            "  all-smi api --port 9090",
            "Run as Prometheus metrics server",
            "command",
        ),
        (
            "  curl http://localhost:9090/metrics",
            "Fetch Prometheus metrics via HTTP",
            "command",
        ),
    ];

    if line_idx < terminal_lines.len() {
        let (cmd, desc, style) = &terminal_lines[line_idx];
        format_terminal_line(cmd, desc, style, width)
    } else {
        " ".repeat(width)
    }
}

fn format_shortcut_line(key: &str, desc: &str, style: &str, width: usize) -> String {
    let content = match style {
        "title" => center_text_colored(desc, width, Color::Yellow),
        "separator" => "═".repeat(width).with(Color::DarkGrey).to_string(),
        "header" => format!(" {}", key.green()),
        "shortcut" => {
            if key.is_empty() {
                String::new()
            } else {
                let key_str = key.white().bold().to_string();
                let desc_str = desc.white().to_string();
                // Calculate available space for description
                let key_display_width = calculate_display_width(&key_str) + 1; // +1 for leading space
                let available_desc_width = width.saturating_sub(key_display_width + 2); // +2 for spaces
                let truncated_desc = if calculate_display_width(&desc_str) > available_desc_width {
                    let mut truncated = String::new();
                    let mut current_width = 0;
                    for ch in desc.chars() {
                        let ch_width = char_display_width(ch);
                        if current_width + ch_width + 3 > available_desc_width {
                            truncated.push_str("...");
                            break;
                        }
                        truncated.push(ch);
                        current_width += ch_width;
                    }
                    truncated
                } else {
                    desc.to_string()
                };
                format!(" {key_str:<10} {}", truncated_desc.white())
            }
        }
        "legend" => {
            let available_desc_width = width.saturating_sub(12); // 10 for key + 2 for spacing
            let truncated_desc = if desc.len() > available_desc_width {
                format!("{}...", &desc[..available_desc_width.saturating_sub(3)])
            } else {
                desc.to_string()
            };
            format!(" {key:<10} {}", truncated_desc.white())
        }
        "status" => {
            let key_str = key.cyan().to_string();
            let desc_str = desc.yellow().to_string();
            format!(" {key_str:<10} {desc_str}")
        }
        "membar" => {
            // Format memory bar with colored segments
            let colored_desc = desc
                .replace("used", &"used".green().to_string())
                .replace("buffers", &"buffers".blue().to_string())
                .replace("cache", &"cache".yellow().to_string());
            format!(" {key:<10} {colored_desc}")
        }
        _ => String::new(),
    };

    // Ensure content fills exactly the width
    pad_content_to_width(content, width)
}

fn format_terminal_line(cmd: &str, desc: &str, style: &str, width: usize) -> String {
    let content = match style {
        "title" => center_text_colored(desc, width, Color::Magenta),
        "separator" => "═".repeat(width).with(Color::DarkGrey).to_string(),
        "header" => format!(" {}", cmd.green()),
        "command" => {
            if cmd.is_empty() {
                String::new()
            } else {
                let formatted_cmd = format!(" {:<35}", cmd.white().bold().to_string());
                let formatted_seperator = "#".with(Color::DarkGrey).to_string();
                let formatted_desc = desc.blue().to_string();
                format!("{formatted_cmd} {formatted_seperator} {formatted_desc}")
            }
        }
        _ => String::new(),
    };

    // Ensure content fills exactly the width
    pad_content_to_width(content, width)
}

fn center_text_colored(text: &str, width: usize, color: Color) -> String {
    let display_width = calculate_display_width(text);

    if display_width >= width {
        text.to_string()
    } else {
        let total_padding = width - display_width;
        let left_padding = total_padding / 2;
        let right_padding = total_padding - left_padding;

        format!(
            "{}{}{}",
            " ".repeat(left_padding),
            text.with(color),
            " ".repeat(right_padding)
        )
    }
}

fn strip_ansi_codes(text: &str) -> String {
    // Simple ANSI escape sequence removal for length calculation
    let mut result = String::new();
    let mut chars = text.chars();

    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            // Skip ANSI escape sequence
            if chars.next() == Some('[') {
                // Skip until we find the end character (typically 'm')
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
        } else {
            result.push(ch);
        }
    }

    result
}

fn get_current_sort_status(sort_criteria: &crate::app_state::SortCriteria) -> String {
    match sort_criteria {
        crate::app_state::SortCriteria::Default => "Default (hostname+index)",
        crate::app_state::SortCriteria::Pid => "Process PID",
        crate::app_state::SortCriteria::User => "User",
        crate::app_state::SortCriteria::Priority => "Priority",
        crate::app_state::SortCriteria::Nice => "Nice Value",
        crate::app_state::SortCriteria::VirtualMemory => "Virtual Memory",
        crate::app_state::SortCriteria::ResidentMemory => "Resident Memory",
        crate::app_state::SortCriteria::State => "Process State",
        crate::app_state::SortCriteria::CpuPercent => "CPU Usage %",
        crate::app_state::SortCriteria::MemoryPercent => "Memory Usage %",
        crate::app_state::SortCriteria::GpuPercent => "GPU Usage %",
        crate::app_state::SortCriteria::GpuMemoryUsage => "GPU Memory Usage",
        crate::app_state::SortCriteria::CpuTime => "CPU Time",
        crate::app_state::SortCriteria::Command => "Command",
        crate::app_state::SortCriteria::Utilization => "GPU Utilization",
        crate::app_state::SortCriteria::GpuMemory => "GPU Memory",
        crate::app_state::SortCriteria::Power => "Power Consumption",
        crate::app_state::SortCriteria::Temperature => "Temperature",
    }
    .to_string()
}
