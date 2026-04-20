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

use crossterm::{
    cursor, queue,
    style::Color,
    terminal::{Clear, ClearType},
};

use crate::app_state::AppState;
use crate::ui::constants::{ANIMATION_SPEED, BLOCK_SIZE_DIVISOR, BLOCK_SIZE_MAX, SCREEN_MARGIN};
use crate::ui::text::{display_width, print_colored_text, truncate_to_width};

pub fn print_loading_indicator<W: Write>(
    stdout: &mut W,
    cols: u16,
    rows: u16,
    frame_counter: u64,
    startup_status_lines: &[String],
) {
    // Center the loading message
    let message = "Loading...";
    let x = (cols.saturating_sub(message.len() as u16)) / 2;
    let y = rows / 2;

    queue!(stdout, cursor::MoveTo(x, y)).unwrap();
    print_colored_text(stdout, message, Color::Yellow, None, None);

    // Progress bar parameters
    let bar_width = 40.min(cols as usize - SCREEN_MARGIN); // Ensure it fits on screen
    let bar_x = (cols.saturating_sub(bar_width as u16)) / 2;
    let bar_y = y + 2; // 2 lines below "Loading..."

    // Create animated progress bar
    // Lower ANIMATION_SPEED = faster
    let position = ((frame_counter / ANIMATION_SPEED) % (bar_width as u64 * 2)) as usize;

    // Calculate the sliding block position (ping-pong effect)
    let block_size = BLOCK_SIZE_MAX.min(bar_width / BLOCK_SIZE_DIVISOR); // Calculate block size relative to bar width
    let actual_pos = if position < bar_width {
        position
    } else {
        bar_width * 2 - position - 1
    };

    // Ensure the block doesn't go out of bounds
    let block_start = actual_pos.min(bar_width.saturating_sub(block_size));
    let block_end = (block_start + block_size).min(bar_width);

    // Move to progress bar position
    queue!(stdout, cursor::MoveTo(bar_x, bar_y)).unwrap();

    // Draw the progress bar with thinner characters
    for i in 0..bar_width {
        if i >= block_start && i < block_end {
            print_colored_text(stdout, "━", Color::Cyan, None, None);
        } else {
            print_colored_text(stdout, "─", Color::DarkGrey, None, None);
        }
    }

    // Display startup status lines below the progress bar
    if !startup_status_lines.is_empty() {
        let status_start_y = bar_y + 2; // 2 lines below the progress bar

        // Calculate starting position to show last N lines that fit on screen
        let max_lines = ((rows - status_start_y) - 1).min(10) as usize; // Show max 10 lines
        let lines_to_show = startup_status_lines.len().min(max_lines);
        let start_idx = startup_status_lines.len().saturating_sub(lines_to_show);

        // Align with progress bar position plus 3 spaces
        let status_x = bar_x + 3;

        for (i, status_line) in startup_status_lines[start_idx..].iter().enumerate() {
            let status_y = status_start_y + i as u16;
            queue!(stdout, cursor::MoveTo(status_x, status_y)).unwrap();

            // Use different colors based on status
            let color = if status_line.contains("✓") {
                Color::DarkGreen
            } else {
                Color::DarkGrey
            };

            print_colored_text(stdout, status_line, color, None, None);
            // Clear to end of line to remove any leftover characters from previous longer text
            queue!(stdout, Clear(ClearType::UntilNewLine)).unwrap();
        }
    }
}

pub fn print_function_keys<W: Write>(
    stdout: &mut W,
    cols: u16,
    rows: u16,
    state: &AppState,
    is_remote: bool,
) {
    // Move to bottom of screen
    queue!(stdout, cursor::MoveTo(0, rows - 1)).unwrap();

    // Filter bar takes precedence over the hotkey strip.
    //
    // When the operator is editing the filter buffer or a committed
    // filter is currently active, show `Filter: <buffer>_  [matched X of Y]`
    // in place of the usual keys. ESC/Enter/Ctrl-R restore normal
    // rendering. Inline parse errors appear in red after the buffer.
    if state.filter_input_mode == crate::app_state::FilterInputMode::Editing
        || state.filter_query.is_some()
    {
        print_filter_bar(stdout, cols, state);
        return;
    }

    // Get current sorting indicator
    let sort_indicator = match state.sort_criteria {
        crate::app_state::SortCriteria::Default => "Sort:Default",
        crate::app_state::SortCriteria::Pid => "Sort:PID",
        crate::app_state::SortCriteria::User => "Sort:User",
        crate::app_state::SortCriteria::Priority => "Sort:Priority",
        crate::app_state::SortCriteria::Nice => "Sort:Nice",
        crate::app_state::SortCriteria::VirtualMemory => "Sort:VIRT",
        crate::app_state::SortCriteria::ResidentMemory => "Sort:RES",
        crate::app_state::SortCriteria::State => "Sort:State",
        crate::app_state::SortCriteria::CpuPercent => "Sort:CPU%",
        crate::app_state::SortCriteria::MemoryPercent => "Sort:MEM%",
        crate::app_state::SortCriteria::GpuPercent => "Sort:GPU%",
        crate::app_state::SortCriteria::GpuMemoryUsage => "Sort:GPU-Mem",
        crate::app_state::SortCriteria::CpuTime => "Sort:Time",
        crate::app_state::SortCriteria::Command => "Sort:Command",
        crate::app_state::SortCriteria::Utilization => "Sort:Util",
        crate::app_state::SortCriteria::GpuMemory => "Sort:GPU-Mem",
        crate::app_state::SortCriteria::Power => "Sort:Power",
        crate::app_state::SortCriteria::Temperature => "Sort:Temp",
    };

    // Get GPU filter indicator
    let filter_indicator = if state.gpu_filter_enabled {
        "Filter:GPU"
    } else {
        ""
    };

    let function_keys = if is_remote {
        // Remote mode: only GPU sorting
        format!(
            "h:Help q:Exit ←→:Tabs ↑↓:Scroll PgUp/PgDn:Page d:Default u:Util g:GPU-Mem [{sort_indicator}]"
        )
    } else {
        // Local mode: both process and GPU sorting
        if state.gpu_filter_enabled {
            format!(
                "h:Help q:Exit f:Filter ←→:Scroll ↑↓:Scroll p:PID m:Memory g:GPU-Mem [{sort_indicator}] [{filter_indicator}]"
            )
        } else {
            format!(
                "h:Help q:Exit f:Filter ←→:Scroll ↑↓:Scroll p:PID m:Memory g:GPU-Mem [{sort_indicator}]"
            )
        }
    };

    // Truncate function keys to terminal width. This runs once per frame
    // so a potential allocation here is acceptable.
    let truncated_keys = if display_width(&function_keys) > cols as usize {
        truncate_to_width(&function_keys, cols as usize).into_owned()
    } else {
        function_keys
    };

    // Check if there's a notification to display
    let notification_msg = state.notifications.get_current_message().unwrap_or("");
    let notification_len = display_width(notification_msg);

    // Calculate space available for function keys (reserve space for notification)
    let available_space = if notification_len > 0 {
        cols.saturating_sub(notification_len as u16 + 1) // +1 for separator space
    } else {
        cols
    } as usize;

    // Truncate function keys if necessary to make room for notification
    let final_function_keys = if display_width(&truncated_keys) > available_space {
        truncate_to_width(&truncated_keys, available_space)
    } else {
        std::borrow::Cow::Borrowed(truncated_keys.as_str())
    };

    // Print function keys
    print_colored_text(stdout, &final_function_keys, Color::DarkGreen, None, None);

    // Print notification if there is one
    if notification_len > 0 {
        // Add separator
        print_colored_text(stdout, " ", Color::White, None, None);

        // Print notification with appropriate color
        let notification_color =
            if notification_msg.contains("Error") || notification_msg.contains("Failed") {
                Color::Red
            } else if notification_msg.contains("Warning") {
                Color::Yellow
            } else {
                Color::Cyan
            };

        print_colored_text(stdout, notification_msg, notification_color, None, None);
    }

    // Fill remaining space to clear any leftover text
    let used_space = display_width(&final_function_keys)
        + if notification_len > 0 {
            notification_len + 1
        } else {
            0
        };
    let remaining_space = cols as usize - used_space.min(cols as usize);

    if remaining_space > 0 {
        print_colored_text(
            stdout,
            &" ".repeat(remaining_space),
            Color::White,
            None,
            None,
        );
    }
}

/// Render the filter bar. Active when the operator is editing a query or
/// has committed one. `_` is appended to the buffer to indicate the cursor
/// while editing. Inline errors are shown in red after the buffer.
fn print_filter_bar<W: Write>(stdout: &mut W, cols: u16, state: &AppState) {
    let editing = state.filter_input_mode == crate::app_state::FilterInputMode::Editing;
    let mut bar = String::new();
    bar.push_str("Filter: ");
    bar.push_str(&state.filter_buffer);
    if editing {
        bar.push('_');
    }
    if let Some((matched, total)) = state.filter_preview_count {
        bar.push_str(&format!(" [matched {matched} of {total}]"));
    }
    let error = state.filter_error.clone();

    // Truncate to terminal width so an overlong query doesn't wrap.
    let room = cols as usize;
    let error_budget = error.as_ref().map(|e| display_width(e) + 2).unwrap_or(0);
    let bar_budget = room.saturating_sub(error_budget);
    let truncated_bar = if display_width(&bar) > bar_budget {
        truncate_to_width(&bar, bar_budget).into_owned()
    } else {
        bar
    };

    // Cyan for the bar, red for any trailing error.
    let bar_color = if editing { Color::Yellow } else { Color::Cyan };
    print_colored_text(stdout, &truncated_bar, bar_color, None, None);
    let mut used = display_width(&truncated_bar);
    if let Some(err) = error {
        print_colored_text(stdout, "  ", Color::White, None, None);
        print_colored_text(stdout, &err, Color::Red, None, None);
        used += 2 + display_width(&err);
    }
    let remaining = (cols as usize).saturating_sub(used);
    if remaining > 0 {
        print_colored_text(stdout, &" ".repeat(remaining), Color::White, None, None);
    }
}
