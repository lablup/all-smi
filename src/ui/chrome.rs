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
    // A terminal with no cells has nowhere to put any of this, and every
    // `MoveTo` below would address a position outside the window. Bail out
    // rather than emit cursor motion into nothing (issue #326).
    //
    // This is only the panic-safety floor. The policy question of what to
    // show on a terminal that is real but too small to be useful belongs to
    // `ui::viewport`, which gates this function's callers well above 1x1.
    if cols == 0 || rows == 0 {
        return;
    }

    // Center the loading message
    let message = "Loading...";
    let x = (cols.saturating_sub(message.len() as u16)) / 2;
    let y = rows / 2;

    queue!(stdout, cursor::MoveTo(x, y)).unwrap();
    print_colored_text(stdout, message, Color::Yellow, None, None);

    // Progress bar parameters. A terminal narrower than SCREEN_MARGIN leaves
    // no room at all, which `saturating_sub` reports as a zero-width bar.
    let bar_width = 40.min((cols as usize).saturating_sub(SCREEN_MARGIN));
    let bar_x = (cols.saturating_sub(bar_width as u16)) / 2;
    let bar_y = y + 2; // 2 lines below "Loading..."

    // Skip the bar when it has no width (the modulo below would divide by
    // zero) or when it would land past the last row.
    if bar_width > 0 && bar_y < rows {
        // Create animated progress bar
        // Lower ANIMATION_SPEED = faster
        let position = ((frame_counter / ANIMATION_SPEED) % (bar_width as u64 * 2)) as usize;

        // Calculate the sliding block position (ping-pong effect)
        let block_size = BLOCK_SIZE_MAX.min(bar_width / BLOCK_SIZE_DIVISOR); // Calculate block size relative to bar width
        let actual_pos = if position < bar_width {
            position
        } else {
            // `position` is a modulo of `bar_width * 2`, so this stays >= 0.
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
    }

    // Display startup status lines below the progress bar
    if !startup_status_lines.is_empty() {
        let status_start_y = bar_y + 2; // 2 lines below the progress bar

        // Calculate starting position to show last N lines that fit on screen.
        // Saturating: on a short terminal `status_start_y` can already be at
        // or past the last row, which means zero lines fit.
        let max_lines = rows
            .saturating_sub(status_start_y)
            .saturating_sub(1)
            .min(10) as usize; // Show max 10 lines
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
    // The status bar occupies the last row. A terminal reporting zero rows
    // has no last row to occupy, and `rows - 1` used to underflow here and
    // abort the process (issue #326). Emit nothing instead.
    //
    // This is the panic-safety floor only. Deciding that a real-but-tiny
    // terminal should show a notice rather than a cramped frame is
    // `ui::viewport`'s job, not this function's.
    if rows == 0 {
        return;
    }

    // Move to bottom of screen
    queue!(stdout, cursor::MoveTo(0, rows.saturating_sub(1))).unwrap();

    // Precedence on the status bar:
    //
    // 1. Filter bar (issue #186) — operator is editing or a filter is
    //    committed.
    // 2. Replay status bar (issue #187) — `view --replay` is active.
    //
    // Filter edit mode still wins: the operator needs an escape hatch to
    // drop the filter even while replaying. The status bar never shows
    // both at once — replay metadata is cheap to re-read the moment the
    // filter clears.
    if state.filter_input_mode == crate::app_state::FilterInputMode::Editing
        || state.filter_query.is_some()
    {
        print_filter_bar(stdout, cols, state);
        return;
    }
    if state.replay.is_some() {
        print_replay_bar(stdout, cols, state);
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
                "h:Help q:Exit f:Filter ←→:Scroll ↑↓:Scroll p:PID c:CPU m:Memory g:GPU-Mem [{sort_indicator}] [{filter_indicator}]"
            )
        } else {
            format!(
                "h:Help q:Exit f:Filter ←→:Scroll ↑↓:Scroll p:PID c:CPU m:Memory g:GPU-Mem [{sort_indicator}]"
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

/// Render the replay status bar. Active when `view --replay` is running.
/// Layout follows the issue spec:
///
/// ```text
/// REPLAY | 00:12:34 / 01:00:00 | 2.0x | paused   [SPACE:play  ]:step  g:seek  L:loop]
/// ```
///
/// When the `g` timecode editor is open, the center of the bar is
/// replaced with `Seek: HH:MM:SS_` and any parse error is appended in
/// red. The status bar is never drawn while the filter bar is active
/// (that mode wins in `print_function_keys`).
fn print_replay_bar<W: Write>(stdout: &mut W, cols: u16, state: &AppState) {
    let Some(replay) = state.replay.as_ref() else {
        return;
    };

    // Left chip: always "REPLAY" on a contrasting color.
    print_colored_text(stdout, "REPLAY", Color::Black, Some(Color::Yellow), None);
    print_colored_text(stdout, " ", Color::White, None, None);

    // Timecode input mode: render a focused editor instead of the
    // normal metadata strip. Tells the operator exactly what they are
    // typing and surfaces parse errors inline.
    if replay.timecode_input_mode {
        let mut bar = String::from("Seek: ");
        bar.push_str(&replay.timecode_buffer);
        bar.push('_');
        let error_str = replay.timecode_error.clone();
        let error_budget = error_str
            .as_ref()
            .map(|e| display_width(e) + 2)
            .unwrap_or(0);
        let budget = (cols as usize).saturating_sub(7 /* "REPLAY " */ + error_budget);
        let truncated = if display_width(&bar) > budget {
            truncate_to_width(&bar, budget).into_owned()
        } else {
            bar
        };
        print_colored_text(stdout, &truncated, Color::Yellow, None, None);
        let mut used = 7 + display_width(&truncated);
        if let Some(err) = error_str {
            print_colored_text(stdout, "  ", Color::White, None, None);
            print_colored_text(stdout, &err, Color::Red, None, None);
            used += 2 + display_width(&err);
        }
        fill_remaining(stdout, cols, used);
        return;
    }

    // Metadata chips.
    let elapsed = format_hms(replay.elapsed.as_secs());
    // Total time is harder to compute precisely until EOF; show the
    // frame-count and total-frames instead, which are always exact.
    let total_frames = if replay.at_eof {
        replay.total_frames.to_string()
    } else {
        format!("{}+", replay.total_frames)
    };
    let state_str = if replay.timecode_input_mode {
        "seeking"
    } else if replay.paused {
        "paused"
    } else if replay.at_eof {
        "end"
    } else {
        "playing"
    };
    let loop_str = if replay.replay_loop { " (loop)" } else { "" };
    let meta = format!(
        "{elapsed} | frame {} / {total_frames} | {:.2}x | {state_str}{loop_str}",
        replay.current_seq + 1,
        replay.speed
    );
    let hotkeys = "[SPACE:play  ]/[:step  +/-:speed  j/k:±10s  g:seek  L:loop]";

    let body = format!("{meta}   {hotkeys}");
    let budget = (cols as usize).saturating_sub(7);
    let truncated = if display_width(&body) > budget {
        truncate_to_width(&body, budget).into_owned()
    } else {
        body
    };
    print_colored_text(stdout, &truncated, Color::Cyan, None, None);
    fill_remaining(stdout, cols, 7 + display_width(&truncated));
}

/// Fill the remainder of the row with spaces so leftover text from a
/// previous frame cannot bleed through.
fn fill_remaining<W: Write>(stdout: &mut W, cols: u16, used: usize) {
    let remaining = (cols as usize).saturating_sub(used);
    if remaining > 0 {
        print_colored_text(stdout, &" ".repeat(remaining), Color::White, None, None);
    }
}

fn format_hms(total_seconds: u64) -> String {
    let h = total_seconds / 3600;
    let m = (total_seconds / 60) % 60;
    let s = total_seconds % 60;
    format!("{h:02}:{m:02}:{s:02}")
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

#[cfg(test)]
mod tests {
    use super::{print_function_keys, print_loading_indicator};
    use crate::app_state::{AppState, SortCriteria};
    use crate::ui::viewport::{MIN_COLS, MIN_ROWS};

    /// Every degenerate geometry the chrome has to survive. Zero and one in
    /// both axes, plus the mixed cases: the panic in issue #326 was on rows,
    /// not columns, so a width-only sweep would have missed it entirely.
    const DEGENERATE_SIZES: &[(u16, u16)] = &[
        (0, 0),
        (1, 1),
        (0, 1),
        (1, 0),
        (0, 24),
        (80, 0),
        (1, 24),
        (80, 1),
        (10, 24), // cols == SCREEN_MARGIN: zero-width progress bar
        (11, 2),
        (MIN_COLS, MIN_ROWS),
    ];

    #[test]
    fn local_status_bar_advertises_cpu_sort_shortcut_and_indicator() {
        let mut state = AppState::new();
        state.sort_criteria = SortCriteria::CpuPercent;

        let mut output = Vec::new();
        print_function_keys(&mut output, 200, 30, &state, false);
        let rendered = String::from_utf8(output).expect("status bar should be valid UTF-8");

        assert!(
            rendered.contains("c:CPU"),
            "local status bar must advertise the CPU sort shortcut.\n--- status ---\n{rendered}"
        );
        assert!(
            rendered.contains("Sort:CPU%"),
            "local status bar must surface the active CPU sort indicator.\n--- status ---\n{rendered}"
        );
    }

    #[test]
    fn remote_status_bar_keeps_cpu_shortcut_hidden() {
        let state = AppState::new();

        let mut output = Vec::new();
        print_function_keys(&mut output, 200, 30, &state, true);
        let rendered = String::from_utf8(output).expect("status bar should be valid UTF-8");

        assert!(
            !rendered.contains("c:CPU"),
            "remote status bar must not advertise the local-only CPU shortcut.\n--- status ---\n{rendered}"
        );
    }

    // -----------------------------------------------------------------------
    // Degenerate terminal geometry (issue #326).
    //
    // `script -q /dev/null all-smi local` hands the process a pty with no
    // window size, so crossterm reports 0x0 and `rows - 1` in
    // `print_function_keys` aborted the process. These drive the real
    // rendering functions, not a copy of their arithmetic.
    // -----------------------------------------------------------------------

    #[test]
    fn function_keys_survive_every_degenerate_size() {
        let state = AppState::new();
        for &(cols, rows) in DEGENERATE_SIZES {
            for is_remote in [false, true] {
                let mut output = Vec::new();
                print_function_keys(&mut output, cols, rows, &state, is_remote);
                // Reaching here without unwinding is the assertion; before the
                // fix this underflowed at every `rows == 0` entry.
                if rows == 0 {
                    assert!(
                        output.is_empty(),
                        "no row exists to draw the status bar on at {cols}x{rows}"
                    );
                }
            }
        }
    }

    #[test]
    fn loading_indicator_survives_every_degenerate_size() {
        // Non-empty status lines are required to reach the `rows -
        // status_start_y - 1` subtraction, which is the second underflow the
        // issue names.
        let status_lines = [
            "Detecting GPUs...".to_string(),
            "\u{2713} Found 8 devices".to_string(),
            "Connecting...".to_string(),
        ];
        for &(cols, rows) in DEGENERATE_SIZES {
            for lines in [&[][..], &status_lines[..]] {
                let mut output = Vec::new();
                print_loading_indicator(&mut output, cols, rows, 0, lines);
                if cols == 0 || rows == 0 {
                    assert!(
                        output.is_empty(),
                        "nothing can be drawn into a {cols}x{rows} terminal"
                    );
                }
            }
        }
    }

    #[test]
    fn loading_indicator_animation_survives_a_zero_width_progress_bar() {
        // At `cols == SCREEN_MARGIN` the bar has zero width. The frame
        // counter feeds a `% (bar_width * 2)`, which is a division by zero
        // unless the bar is skipped. Sweep several counters so the ping-pong
        // branch is taken too.
        for frame_counter in [0u64, 1, 7, 40, 4096] {
            let mut output = Vec::new();
            print_loading_indicator(&mut output, 10, 24, frame_counter, &[]);
            assert!(
                String::from_utf8(output)
                    .expect("loading screen should be valid UTF-8")
                    .contains("Loading"),
                "the message still renders even when the bar does not"
            );
        }
    }

    #[test]
    fn function_keys_still_render_at_the_minimum_supported_size() {
        // The floor `ui::viewport` enforces must actually produce a status
        // bar, otherwise the minimum is set below what the chrome can draw.
        let state = AppState::new();
        let mut output = Vec::new();
        print_function_keys(&mut output, MIN_COLS, MIN_ROWS, &state, false);
        let rendered = String::from_utf8(output).expect("status bar should be valid UTF-8");
        assert!(
            rendered.contains("h:Help"),
            "the minimum size must fit at least the first hotkey.\n--- status ---\n{rendered}"
        );
    }

    #[test]
    fn function_keys_never_exceed_one_row_of_cells() {
        // The status bar owns exactly one row, so it must never emit more
        // printable cells than the terminal is wide. A regression here wraps
        // the bar onto the row above and corrupts the frame.
        let state = AppState::new();
        for cols in [1u16, 2, 5, 13, MIN_COLS, 40, 200] {
            let mut output = Vec::new();
            print_function_keys(&mut output, cols, 24, &state, false);
            let rendered = String::from_utf8(output).expect("status bar should be valid UTF-8");
            let printable: String = strip_ansi(&rendered);
            assert!(
                crate::ui::text::display_width(&printable) <= cols as usize,
                "status bar overflowed {cols} columns: {printable:?}"
            );
        }
    }

    /// Drop CSI escape sequences and the cursor-motion prefix so only the
    /// cells that land on screen are measured.
    fn strip_ansi(input: &str) -> String {
        let mut out = String::with_capacity(input.len());
        let mut chars = input.chars().peekable();
        while let Some(c) = chars.next() {
            if c != '\u{1b}' {
                out.push(c);
                continue;
            }
            if chars.peek() == Some(&'[') {
                chars.next();
                // A CSI sequence runs until a byte in the range 0x40..=0x7e.
                for term in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&term) {
                        break;
                    }
                }
            }
        }
        out
    }
}
