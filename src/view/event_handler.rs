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

use std::time::Duration;

use crossterm::{
    event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind},
    terminal::size,
};

use crate::app_state::{AppState, FilterInputMode, SortCriteria};
use crate::cli::ViewArgs;
use crate::record::replay::parse_timecode;
use crate::ui::filter_dsl::{apply as apply_filter, parse as parse_filter};
use crate::ui::layout::LayoutCalculator;

/// Upper bound on the filter input buffer size (bytes).
///
/// Bracketed-paste can deliver an arbitrarily large blob into a single
/// key-event burst, and `update_filter_preview` runs the lexer+parser on
/// every keystroke. Capping the buffer keeps a 10 MB paste from
/// turning into 10 MB of per-keystroke work on the UI thread.
const FILTER_BUFFER_MAX: usize = 512;

/// Get the actual number of visible process rows from the last rendered frame.
/// Falls back to a conservative estimate if the renderer hasn't set it yet.
fn get_visible_process_rows(state: &AppState) -> usize {
    if state.visible_process_rows > 0 {
        state.visible_process_rows
    } else {
        // Fallback for the first frame before rendering has set the value
        let (_cols, rows) = size().unwrap_or((80, 24));
        (rows / 2).saturating_sub(1) as usize
    }
}

pub async fn handle_key_event(key_event: KeyEvent, state: &mut AppState, args: &ViewArgs) -> bool {
    // Mode precedence (highest first) — do NOT reorder:
    //
    // 1. Filter-edit mode (`/`) intercepts everything so `q`/`d`/`u`
    //    become literal text the operator can type into the query.
    // 2. Replay timecode input (`g` → `HH:MM:SS`) intercepts everything
    //    so the same keys become digits/colons, never hotkeys.
    // 3. Normal keys: quit, help, alerts, arrows.
    // 4. Replay-mode keys (SPACE/`]`/`[`/`+`/`-`/`j`/`k`/`g`/`L`) are
    //    routed BEFORE `handle_navigation_keys` so the sort-by-GpuMem
    //    `g` binding doesn't shadow the timecode editor.
    if state.filter_input_mode == FilterInputMode::Editing {
        return handle_filter_input(key_event, state);
    }
    if state.replay.as_ref().is_some_and(|r| r.timecode_input_mode) {
        return handle_timecode_input(key_event, state);
    }

    match key_event.code {
        KeyCode::Esc => {
            if state.alert_panel_open {
                state.alert_panel_open = false;
                false
            } else if state.show_help {
                state.show_help = false;
                false
            } else if state.filter_query.is_some() {
                // ESC outside filter-input mode clears the committed query.
                clear_filter(state);
                false
            } else {
                true // Exit
            }
        }
        KeyCode::Char('q') => true, // Exit
        KeyCode::Char('/') => {
            enter_filter_edit(state);
            false
        }
        KeyCode::Char('A') => {
            state.alert_panel_open = !state.alert_panel_open;
            false
        }
        KeyCode::Char('1') | KeyCode::Char('h') => {
            state.show_help = !state.show_help;
            false
        }
        KeyCode::Left => {
            if !state.show_help {
                handle_left_arrow(state);
            }
            false
        }
        KeyCode::Right => {
            if !state.show_help {
                handle_right_arrow(state);
            }
            false
        }
        _ if !state.loading && !state.show_help => {
            if state.replay.is_some() && handle_replay_keys(key_event, state) {
                return false;
            }
            handle_navigation_keys(key_event, state, args);
            false
        }
        _ => false,
    }
}

/// Dispatch replay-mode keys. Returns `true` if the key was consumed.
/// Only active when `state.replay.is_some()`. Runs BEFORE
/// `handle_navigation_keys` so `g` (timecode editor) wins over `g`
/// (sort by GpuMemory).
fn handle_replay_keys(key_event: KeyEvent, state: &mut AppState) -> bool {
    let KeyEvent {
        code, modifiers, ..
    } = key_event;
    if modifiers.contains(KeyModifiers::CONTROL) || modifiers.contains(KeyModifiers::ALT) {
        return false;
    }
    let replay = match state.replay.as_mut() {
        Some(r) => r,
        None => return false,
    };
    match code {
        KeyCode::Char(' ') => {
            replay.paused = !replay.paused;
            if replay.at_eof && !replay.paused {
                // Un-pausing past EOF rewinds to frame 0 if loop is on;
                // otherwise stays at EOF (user can then hit `[` to step
                // back). Loop behavior matches the issue spec.
                if replay.replay_loop {
                    replay.pending_seek = Some(Duration::ZERO);
                    replay.at_eof = false;
                } else {
                    replay.paused = true;
                }
            }
            state.mark_data_changed();
            true
        }
        KeyCode::Char(']') => {
            replay.pending_step = Some(1);
            replay.paused = true;
            state.mark_data_changed();
            true
        }
        KeyCode::Char('[') => {
            replay.pending_step = Some(-1);
            replay.paused = true;
            state.mark_data_changed();
            true
        }
        KeyCode::Char('+') | KeyCode::Char('=') => {
            // `+` usually requires Shift on US layouts, so also accept `=`
            // to avoid forcing the operator to hold Shift mid-playback.
            replay.cycle_speed(true);
            state.mark_data_changed();
            true
        }
        KeyCode::Char('-') | KeyCode::Char('_') => {
            replay.cycle_speed(false);
            state.mark_data_changed();
            true
        }
        KeyCode::Char('j') => {
            seek_relative(replay, -10);
            state.mark_data_changed();
            true
        }
        KeyCode::Char('k') => {
            seek_relative(replay, 10);
            state.mark_data_changed();
            true
        }
        KeyCode::Char('g') => {
            replay.timecode_input_mode = true;
            replay.timecode_buffer.clear();
            replay.timecode_error = None;
            state.mark_data_changed();
            true
        }
        KeyCode::Char('L') => {
            replay.replay_loop = !replay.replay_loop;
            state.mark_data_changed();
            true
        }
        _ => false,
    }
}

/// Nudge the seek target by `delta_secs` (positive = forward, negative =
/// backward). Works by computing the new absolute offset from the
/// currently-displayed elapsed time.
fn seek_relative(replay: &mut crate::app_state::ReplayState, delta_secs: i64) {
    let current = replay.elapsed.as_secs() as i64;
    let new = (current + delta_secs).max(0) as u64;
    replay.pending_seek = Some(Duration::from_secs(new));
}

/// Handle keys while the `g <HH:MM:SS>` timecode editor is open.
/// Everything except `Esc`/`Enter`/digits/`:` is dropped so the buffer
/// cannot accumulate garbage.
fn handle_timecode_input(key_event: KeyEvent, state: &mut AppState) -> bool {
    let KeyEvent { code, .. } = key_event;
    let Some(replay) = state.replay.as_mut() else {
        return false;
    };
    match code {
        KeyCode::Esc => {
            replay.timecode_input_mode = false;
            replay.timecode_buffer.clear();
            replay.timecode_error = None;
            state.mark_data_changed();
            false
        }
        KeyCode::Enter => {
            match parse_timecode(&replay.timecode_buffer) {
                Ok(d) => {
                    replay.pending_seek = Some(d);
                    replay.timecode_input_mode = false;
                    replay.timecode_buffer.clear();
                    replay.timecode_error = None;
                }
                Err(e) => {
                    replay.timecode_error = Some(format!("{e}"));
                }
            }
            state.mark_data_changed();
            false
        }
        KeyCode::Backspace => {
            replay.timecode_buffer.pop();
            state.mark_data_changed();
            false
        }
        KeyCode::Char(c) if c.is_ascii_digit() || c == ':' => {
            if replay.timecode_buffer.len() < 16 {
                replay.timecode_buffer.push(c);
                state.mark_data_changed();
            }
            false
        }
        _ => false,
    }
}

/// Enter the filter bar: stash prior filter text in the buffer so the
/// operator can edit, not restart.
fn enter_filter_edit(state: &mut AppState) {
    // If a filter is committed, prefill with the original query so the
    // operator can tweak it rather than retyping.
    state.filter_input_mode = FilterInputMode::Editing;
    if state.filter_buffer.is_empty()
        && let Some(first) = state.filter_recent.front()
    {
        state.filter_buffer.clone_from(first);
    }
    state.filter_error = None;
    state.filter_recall_index = None;
    update_filter_preview(state);
}

/// Clear the committed filter and any active edit state.
fn clear_filter(state: &mut AppState) {
    state.filter_query = None;
    state.filter_buffer.clear();
    state.filter_error = None;
    state.filter_input_mode = FilterInputMode::Idle;
    state.filter_preview_count = None;
    state.filter_recall_index = None;
    state.mark_data_changed();
}

/// Recompute the live preview count using the current buffer.
fn update_filter_preview(state: &mut AppState) {
    if state.filter_buffer.trim().is_empty() {
        state.filter_preview_count = None;
        state.filter_error = None;
        return;
    }
    match parse_filter(&state.filter_buffer) {
        Ok(Some(expr)) => {
            let total = state.gpu_info.len();
            let matched = state
                .gpu_info
                .iter()
                .filter(|g| apply_filter(Some(&expr), *g))
                .count();
            state.filter_preview_count = Some((matched, total));
            state.filter_error = None;
        }
        Ok(None) => {
            state.filter_preview_count = None;
            state.filter_error = None;
        }
        Err(e) => {
            state.filter_preview_count = None;
            state.filter_error = Some(format!("parse error: {} at col {}", e.msg, e.col));
        }
    }
}

/// Commit the current buffer as the active filter. Returns true when the
/// commit succeeded (the buffer parsed cleanly).
fn commit_filter(state: &mut AppState) -> bool {
    let input = state.filter_buffer.trim().to_string();
    if input.is_empty() {
        // Empty commit clears the filter.
        clear_filter(state);
        return true;
    }
    match parse_filter(&input) {
        Ok(Some(expr)) => {
            state.filter_query = Some(expr);
            state.push_recent_filter(input.clone());
            state.filter_input_mode = FilterInputMode::Idle;
            state.filter_error = None;
            state.filter_recall_index = None;
            update_filter_preview(state);
            state.mark_data_changed();
            true
        }
        Ok(None) => {
            clear_filter(state);
            true
        }
        Err(e) => {
            state.filter_error = Some(format!("parse error: {} at col {}", e.msg, e.col));
            false
        }
    }
}

/// Handle a single key while in filter-edit mode.
fn handle_filter_input(key_event: KeyEvent, state: &mut AppState) -> bool {
    let KeyEvent {
        code, modifiers, ..
    } = key_event;

    match code {
        KeyCode::Esc => {
            // Abort the edit without changing the committed query.
            state.filter_input_mode = FilterInputMode::Idle;
            state.filter_error = None;
            state.filter_recall_index = None;
            // Restore the buffer to the committed query so the operator
            // sees consistent state on re-entry.
            state.filter_buffer = if let Some(q) = state.filter_recent.front() {
                q.clone()
            } else {
                String::new()
            };
            if state.filter_query.is_none() {
                state.filter_buffer.clear();
            }
            false
        }
        KeyCode::Enter => {
            let _committed = commit_filter(state);
            false
        }
        KeyCode::Backspace => {
            state.filter_buffer.pop();
            state.filter_recall_index = None;
            update_filter_preview(state);
            false
        }
        KeyCode::Char(c) if modifiers.contains(KeyModifiers::CONTROL) && c == 'r' => {
            // Cycle through the most-recent queue. Each press picks the
            // next older entry; wrapping past the end clears the buffer.
            let len = state.filter_recent.len();
            if len == 0 {
                return false;
            }
            let next = match state.filter_recall_index {
                Some(i) => (i + 1) % len,
                None => 0,
            };
            state.filter_recall_index = Some(next);
            state.filter_buffer = state.filter_recent[next].clone();
            update_filter_preview(state);
            false
        }
        KeyCode::Char(c) if modifiers.contains(KeyModifiers::CONTROL) && c == 'u' => {
            // Emacs convention: Ctrl-U clears the entire line.
            state.filter_buffer.clear();
            state.filter_recall_index = None;
            update_filter_preview(state);
            false
        }
        KeyCode::Char(c) => {
            // Do not treat modifier+char as literal characters unless the
            // modifier is Shift alone.
            if modifiers.contains(KeyModifiers::CONTROL) || modifiers.contains(KeyModifiers::ALT) {
                return false;
            }
            // Cap the buffer so a bracketed-paste of megabytes of data
            // cannot turn every subsequent keystroke into an O(n) parse
            // over the entire buffer and DoS the UI thread. A 512-char
            // filter is far beyond any practical query.
            if state.filter_buffer.len() >= FILTER_BUFFER_MAX {
                return false;
            }
            state.filter_buffer.push(c);
            state.filter_recall_index = None;
            update_filter_preview(state);
            false
        }
        _ => false,
    }
}

fn handle_left_arrow(state: &mut AppState) {
    // Check if we're in local mode ("All" tab + local hostname)
    if state.is_local_mode {
        // Local mode - handle horizontal scrolling for process list
        if state.process_horizontal_scroll_offset > 0 {
            state.process_horizontal_scroll_offset =
                state.process_horizontal_scroll_offset.saturating_sub(10);
        }
    } else {
        // Remote mode - handle tab switching
        if state.current_tab > 0 {
            state.current_tab -= 1;

            // If we're moving to a node tab (not "All" tab), adjust scroll if needed
            if state.current_tab > 0 {
                // Calculate which node tab index this is (subtract 1 for "All" tab)
                let node_tab_index = state.current_tab - 1;
                if node_tab_index < state.tab_scroll_offset {
                    state.tab_scroll_offset = node_tab_index;
                }
            }
            // If moving to "All" tab (index 0), no scroll adjustment needed since it's always visible
        }
        state.gpu_scroll_offset = 0;
        state.storage_scroll_offset = 0;
    }
}

fn handle_right_arrow(state: &mut AppState) {
    // Check if we're in local mode ("All" tab + local hostname)
    if state.is_local_mode {
        // Local mode - handle horizontal scrolling for process list
        state.process_horizontal_scroll_offset += 10;
    } else {
        // Remote mode - handle tab switching
        if state.current_tab < state.tabs.len() - 1 {
            state.current_tab += 1;

            // If we're moving to a node tab (not "All" tab), check if we need to scroll
            if state.current_tab > 0 {
                let (cols, _) = size().unwrap();
                let mut available_width = cols.saturating_sub(8); // Space for "Tabs: " prefix

                // Reserve space for "All" tab (always visible)
                if !state.tabs.is_empty() {
                    let all_tab_width = state.tabs[0].len() as u16 + 2;
                    available_width = available_width.saturating_sub(all_tab_width);
                }

                // Calculate which node tabs are visible starting from scroll offset
                let mut last_visible_node_tab_index = state.tab_scroll_offset;

                for (node_index, tab) in state
                    .tabs
                    .iter()
                    .enumerate()
                    .skip(1)
                    .skip(state.tab_scroll_offset)
                {
                    let tab_width = tab.len() as u16 + 2;
                    if available_width < tab_width {
                        break;
                    }
                    available_width -= tab_width;
                    last_visible_node_tab_index = node_index - 1; // Convert to node tab index (subtract 1 for "All")
                }

                // Check if current tab is a node tab and not visible
                let current_node_tab_index = state.current_tab - 1; // Convert to node tab index
                if current_node_tab_index > last_visible_node_tab_index {
                    state.tab_scroll_offset += 1;
                }
            }
            // If moving to "All" tab, no scroll adjustment needed since it's always visible
        }
        state.gpu_scroll_offset = 0;
        state.storage_scroll_offset = 0;
    }
}

fn handle_navigation_keys(key_event: KeyEvent, state: &mut AppState, args: &ViewArgs) {
    match key_event.code {
        KeyCode::Up => handle_up_arrow(state, args),
        KeyCode::Down => handle_down_arrow(state, args),
        KeyCode::PageUp => handle_page_up(state, args),
        KeyCode::PageDown => handle_page_down(state, args),
        KeyCode::Char('p') => state.sort_criteria = SortCriteria::Pid,
        KeyCode::Char('m') => state.sort_criteria = SortCriteria::MemoryPercent,
        KeyCode::Char('u') => state.sort_criteria = SortCriteria::Utilization,
        KeyCode::Char('g') => state.sort_criteria = SortCriteria::GpuMemory,
        KeyCode::Char('d') => state.sort_criteria = SortCriteria::Default,
        KeyCode::Char('f') => {
            let was_enabled = state.gpu_filter_enabled;
            state.gpu_filter_enabled = !state.gpu_filter_enabled;

            // Reset selection indices when enabling filter to avoid out-of-bounds issues
            if !was_enabled && state.gpu_filter_enabled {
                state.selected_process_index = 0;
                state.start_index = 0;
            }
        }
        _ => {}
    }
}

fn handle_up_arrow(state: &mut AppState, args: &ViewArgs) {
    let is_remote = args.hosts.is_some() || args.hostfile.is_some();
    if is_remote {
        // Unified scrolling for remote mode
        if state.gpu_scroll_offset > 0 {
            state.gpu_scroll_offset -= 1;
            state.storage_scroll_offset = 0; // Reset storage scroll when in GPU area
        } else if state.storage_scroll_offset > 0 {
            state.storage_scroll_offset -= 1;
        }
    } else {
        // Local mode - process list scrolling
        if state.selected_process_index > 0 {
            state.selected_process_index -= 1;
        }
        if state.selected_process_index < state.start_index {
            state.start_index = state.selected_process_index;
        }
    }
}

fn handle_down_arrow(state: &mut AppState, args: &ViewArgs) {
    let is_remote = args.hosts.is_some() || args.hostfile.is_some();
    if is_remote {
        // Unified scrolling for remote mode
        let gpu_count = if state.current_tab == 0 {
            state.gpu_info.len()
        } else {
            state
                .gpu_info
                .iter()
                .filter(|info| info.host_id == state.tabs[state.current_tab])
                .count()
        };

        let storage_count = if state.current_tab == 0 {
            // No storage on 'All' tab
            0
        } else {
            state
                .storage_info
                .iter()
                .filter(|info| info.host_id == state.tabs[state.current_tab])
                .count()
        };

        if state.gpu_scroll_offset < gpu_count.saturating_sub(1) {
            state.gpu_scroll_offset += 1;
            state.storage_scroll_offset = 0; // Reset storage scroll when in GPU area
        } else if state.storage_scroll_offset < storage_count.saturating_sub(1) {
            state.storage_scroll_offset += 1;
        }
    } else {
        // Local mode - process list scrolling
        if !state.process_info.is_empty()
            && state.selected_process_index < state.process_info.len() - 1
        {
            state.selected_process_index += 1;
        }
        let visible = get_visible_process_rows(state);
        if visible > 0 && state.selected_process_index >= state.start_index + visible {
            state.start_index = state.selected_process_index - visible + 1;
        }
    }
}

fn handle_page_up(state: &mut AppState, args: &ViewArgs) {
    let is_remote = args.hosts.is_some() || args.hostfile.is_some();
    if is_remote {
        // Remote mode - page up through GPU list
        let (_cols, rows) = size().unwrap();
        let content_start_row = 19;
        let available_rows = rows.saturating_sub(content_start_row).saturating_sub(1) as usize;

        // Calculate storage display space for current tab
        let storage_items_count = if state.current_tab > 0 && !state.storage_info.is_empty() {
            let current_hostname = &state.tabs[state.current_tab];
            state
                .storage_info
                .iter()
                .filter(|info| info.host_id == *current_hostname)
                .count()
        } else {
            0
        };
        let storage_display_rows = if storage_items_count > 0 {
            storage_items_count + 2 // Each storage item takes 1 line (labels + bar on same line)
        } else {
            0
        };

        let gpu_display_rows = available_rows.saturating_sub(storage_display_rows);
        // Per-GPU line count is dynamic now: NVIDIA rows with thermal /
        // P-state data emit 3 lines, vGPU-enabled GPUs emit even more.
        // Use the maximum line count any visible GPU would render so the
        // page size never overshoots the rendered area.
        let lines_per_gpu = LayoutCalculator::max_gpu_lines_for_tab(state).max(2);
        let max_gpu_items = gpu_display_rows / lines_per_gpu;
        let page_size = max_gpu_items.max(1); // At least 1 item per page

        state.gpu_scroll_offset = state.gpu_scroll_offset.saturating_sub(page_size);
        state.storage_scroll_offset = 0; // Reset storage scroll when paging GPU list
    } else {
        // Local mode - page up through process list
        let page_size = get_visible_process_rows(state).max(1);
        state.selected_process_index = state.selected_process_index.saturating_sub(page_size);
        if state.selected_process_index < state.start_index {
            state.start_index = state.selected_process_index;
        }
    }
}

fn handle_page_down(state: &mut AppState, args: &ViewArgs) {
    let is_remote = args.hosts.is_some() || args.hostfile.is_some();
    if is_remote {
        // Remote mode - page down through GPU list
        let (_cols, rows) = size().unwrap();
        let content_start_row = 19;
        let available_rows = rows.saturating_sub(content_start_row).saturating_sub(1) as usize;

        // Calculate storage display space for current tab
        let storage_items_count = if state.current_tab > 0 && !state.storage_info.is_empty() {
            let current_hostname = &state.tabs[state.current_tab];
            state
                .storage_info
                .iter()
                .filter(|info| info.host_id == *current_hostname)
                .count()
        } else {
            0
        };
        let storage_display_rows = if storage_items_count > 0 {
            storage_items_count + 2 // Each storage item takes 1 line (labels + bar on same line)
        } else {
            0
        };

        let gpu_display_rows = available_rows.saturating_sub(storage_display_rows);
        // Per-GPU line count is dynamic now: NVIDIA rows with thermal /
        // P-state data emit 3 lines, vGPU-enabled GPUs emit even more.
        // Use the maximum line count any visible GPU would render so the
        // page size never overshoots the rendered area.
        let lines_per_gpu = LayoutCalculator::max_gpu_lines_for_tab(state).max(2);
        let max_gpu_items = gpu_display_rows / lines_per_gpu;
        let page_size = max_gpu_items.max(1); // At least 1 item per page

        // Calculate total GPUs for current tab
        let total_gpus = if state.current_tab == 0 {
            state.gpu_info.len()
        } else {
            state
                .gpu_info
                .iter()
                .filter(|info| info.host_id == state.tabs[state.current_tab])
                .count()
        };

        if total_gpus > 0 {
            let max_offset = total_gpus.saturating_sub(max_gpu_items);
            state.gpu_scroll_offset = (state.gpu_scroll_offset + page_size).min(max_offset);
            state.storage_scroll_offset = 0; // Reset storage scroll when paging GPU list
        }
    } else {
        // Local mode - page down through process list
        if !state.process_info.is_empty() {
            let visible = get_visible_process_rows(state);
            let page_size = visible.max(1);
            state.selected_process_index =
                (state.selected_process_index + page_size).min(state.process_info.len() - 1);
            if visible > 0 && state.selected_process_index >= state.start_index + visible {
                state.start_index = state.selected_process_index - visible + 1;
            }
        }
    }
}

pub async fn handle_mouse_event(
    mouse_event: MouseEvent,
    state: &mut AppState,
    _args: &ViewArgs,
) -> bool {
    match mouse_event.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            // Only handle clicks when not in help mode and not loading
            if !state.show_help && !state.loading {
                handle_process_header_click(mouse_event.column, mouse_event.row, state);
            }
            false
        }
        _ => false,
    }
}

fn handle_process_header_click(x: u16, y: u16, state: &mut AppState) {
    // Check if we're in local mode with process list visible
    if !state.is_local_mode {
        return;
    }

    // Get terminal size to calculate process list position
    let (_cols, rows) = match size() {
        Ok((c, r)) => (c, r),
        Err(_) => return,
    };

    // Calculate where the process header should be
    // The header is at half_rows - 1 based on testing
    let half_rows = rows / 2;
    let process_header_row = half_rows - 1;

    // Check if click is on the process header row
    if y != process_header_row {
        return;
    }

    // Calculate column positions based on fixed widths
    let fixed_widths = [7, 12, 3, 3, 6, 6, 1, 5, 5, 5, 7, 8];
    let mut column_start: usize = 0;
    let mut column_index = None;

    // Account for horizontal scrolling
    let scroll_offset = state.process_horizontal_scroll_offset;

    // Find which column was clicked
    for (i, &width) in fixed_widths.iter().enumerate() {
        let column_end = column_start + width;

        // Adjust for scroll offset
        let visible_start = column_start.saturating_sub(scroll_offset) as u16;
        let visible_end = column_end.saturating_sub(scroll_offset) as u16;

        if x >= visible_start && x < visible_end {
            column_index = Some(i);
            break;
        }
        column_start = column_end + 1; // +1 for space between columns
    }

    // Map column index to sort criteria
    if let Some(idx) = column_index {
        let new_criteria = match idx {
            0 => SortCriteria::Pid,
            1 => SortCriteria::User,
            2 => SortCriteria::Priority,
            3 => SortCriteria::Nice,
            4 => SortCriteria::VirtualMemory,
            5 => SortCriteria::ResidentMemory,
            6 => SortCriteria::State,
            7 => SortCriteria::CpuPercent,
            8 => SortCriteria::MemoryPercent,
            9 => SortCriteria::GpuPercent,
            10 => SortCriteria::GpuMemoryUsage,
            11 => SortCriteria::CpuTime,
            _ => return, // Command column or beyond
        };

        // Toggle sort direction if clicking the same column
        if state.sort_criteria == new_criteria {
            state.sort_direction = match state.sort_direction {
                crate::app_state::SortDirection::Ascending => {
                    crate::app_state::SortDirection::Descending
                }
                crate::app_state::SortDirection::Descending => {
                    crate::app_state::SortDirection::Ascending
                }
            };
        } else {
            // New column, default to descending for most columns
            state.sort_criteria = new_criteria;
            state.sort_direction = match new_criteria {
                SortCriteria::User | SortCriteria::State | SortCriteria::Command => {
                    crate::app_state::SortDirection::Ascending
                }
                _ => crate::app_state::SortDirection::Descending,
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn key_with_mods(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    fn args() -> ViewArgs {
        ViewArgs {
            hosts: None,
            hostfile: None,
            interval: None,
            alert_temp: None,
            alert_util_low_mins: None,
            replay: None,
            speed: 1.0,
            start: None,
            replay_loop: false,
        }
    }

    #[tokio::test]
    async fn slash_enters_filter_edit_mode() {
        let mut state = AppState::new();
        handle_key_event(key(KeyCode::Char('/')), &mut state, &args()).await;
        assert_eq!(state.filter_input_mode, FilterInputMode::Editing);
    }

    #[tokio::test]
    async fn typing_in_filter_mode_appends_to_buffer() {
        let mut state = AppState::new();
        handle_key_event(key(KeyCode::Char('/')), &mut state, &args()).await;
        for c in ['t', 'e', 'm', 'p', '>', '8', '5'] {
            handle_key_event(key(KeyCode::Char(c)), &mut state, &args()).await;
        }
        assert_eq!(state.filter_buffer, "temp>85");
    }

    #[tokio::test]
    async fn enter_commits_valid_filter() {
        let mut state = AppState::new();
        handle_key_event(key(KeyCode::Char('/')), &mut state, &args()).await;
        for c in "temp>85".chars() {
            handle_key_event(key(KeyCode::Char(c)), &mut state, &args()).await;
        }
        handle_key_event(key(KeyCode::Enter), &mut state, &args()).await;
        assert_eq!(state.filter_input_mode, FilterInputMode::Idle);
        assert!(state.filter_query.is_some());
        assert_eq!(state.filter_recent.len(), 1);
    }

    #[tokio::test]
    async fn enter_with_invalid_filter_does_not_commit() {
        let mut state = AppState::new();
        handle_key_event(key(KeyCode::Char('/')), &mut state, &args()).await;
        for c in "temp>>".chars() {
            handle_key_event(key(KeyCode::Char(c)), &mut state, &args()).await;
        }
        handle_key_event(key(KeyCode::Enter), &mut state, &args()).await;
        // Still in edit mode because the commit failed.
        assert_eq!(state.filter_input_mode, FilterInputMode::Editing);
        assert!(state.filter_query.is_none());
        assert!(state.filter_error.is_some());
    }

    #[tokio::test]
    async fn escape_aborts_edit_without_committing() {
        let mut state = AppState::new();
        handle_key_event(key(KeyCode::Char('/')), &mut state, &args()).await;
        for c in "abc".chars() {
            handle_key_event(key(KeyCode::Char(c)), &mut state, &args()).await;
        }
        handle_key_event(key(KeyCode::Esc), &mut state, &args()).await;
        assert_eq!(state.filter_input_mode, FilterInputMode::Idle);
        assert!(state.filter_query.is_none());
    }

    #[tokio::test]
    async fn q_does_not_quit_in_filter_mode() {
        let mut state = AppState::new();
        handle_key_event(key(KeyCode::Char('/')), &mut state, &args()).await;
        let quit = handle_key_event(key(KeyCode::Char('q')), &mut state, &args()).await;
        assert!(!quit, "`q` must not exit while the filter bar is active");
        assert!(
            state.filter_buffer.contains('q'),
            "`q` must be treated as literal text"
        );
    }

    #[tokio::test]
    async fn escape_outside_edit_clears_committed_filter() {
        let mut state = AppState::new();
        // Commit a filter.
        handle_key_event(key(KeyCode::Char('/')), &mut state, &args()).await;
        for c in "temp>80".chars() {
            handle_key_event(key(KeyCode::Char(c)), &mut state, &args()).await;
        }
        handle_key_event(key(KeyCode::Enter), &mut state, &args()).await;
        assert!(state.filter_query.is_some());
        // ESC in idle mode clears it.
        handle_key_event(key(KeyCode::Esc), &mut state, &args()).await;
        assert!(state.filter_query.is_none());
    }

    #[tokio::test]
    async fn backspace_shrinks_buffer() {
        let mut state = AppState::new();
        handle_key_event(key(KeyCode::Char('/')), &mut state, &args()).await;
        for c in "abc".chars() {
            handle_key_event(key(KeyCode::Char(c)), &mut state, &args()).await;
        }
        handle_key_event(key(KeyCode::Backspace), &mut state, &args()).await;
        assert_eq!(state.filter_buffer, "ab");
    }

    #[tokio::test]
    async fn ctrl_r_recalls_last_query() {
        let mut state = AppState::new();
        state.push_recent_filter("temp>85".to_string());
        state.push_recent_filter("util<5".to_string());

        handle_key_event(key(KeyCode::Char('/')), &mut state, &args()).await;
        // Clear any prefill so we exercise ctrl-r from empty.
        state.filter_buffer.clear();
        handle_key_event(
            key_with_mods(KeyCode::Char('r'), KeyModifiers::CONTROL),
            &mut state,
            &args(),
        )
        .await;
        // Newest first.
        assert_eq!(state.filter_buffer, "util<5");
    }

    #[tokio::test]
    async fn capital_a_toggles_alert_panel() {
        let mut state = AppState::new();
        handle_key_event(key(KeyCode::Char('A')), &mut state, &args()).await;
        assert!(state.alert_panel_open);
        handle_key_event(key(KeyCode::Char('A')), &mut state, &args()).await;
        assert!(!state.alert_panel_open);
    }

    #[tokio::test]
    async fn esc_closes_alert_panel_when_open() {
        let mut state = AppState::new();
        state.alert_panel_open = true;
        handle_key_event(key(KeyCode::Esc), &mut state, &args()).await;
        assert!(!state.alert_panel_open);
    }

    // -----------------------------------------------------------------------
    // Replay mode (issue #187)
    // -----------------------------------------------------------------------

    fn replay_state() -> crate::app_state::ReplayState {
        crate::app_state::ReplayState {
            paused: false,
            speed: 1.0,
            current_seq: 0,
            total_frames: 0,
            elapsed: Duration::ZERO,
            at_eof: false,
            replay_loop: false,
            pending_seek: None,
            pending_step: None,
            timecode_input_mode: false,
            timecode_buffer: String::new(),
            timecode_error: None,
        }
    }

    #[tokio::test]
    async fn space_toggles_replay_pause() {
        let mut state = AppState::new();
        state.replay = Some(replay_state());
        state.loading = false;
        handle_key_event(key(KeyCode::Char(' ')), &mut state, &args()).await;
        assert!(
            state.replay.as_ref().unwrap().paused,
            "SPACE should pause playback"
        );
        handle_key_event(key(KeyCode::Char(' ')), &mut state, &args()).await;
        assert!(
            !state.replay.as_ref().unwrap().paused,
            "SPACE again should resume"
        );
    }

    #[tokio::test]
    async fn bracket_keys_step_frames() {
        let mut state = AppState::new();
        state.replay = Some(replay_state());
        state.loading = false;

        handle_key_event(key(KeyCode::Char(']')), &mut state, &args()).await;
        let r = state.replay.as_ref().unwrap();
        assert_eq!(r.pending_step, Some(1));
        assert!(r.paused, "stepping must auto-pause");

        handle_key_event(key(KeyCode::Char('[')), &mut state, &args()).await;
        assert_eq!(state.replay.as_ref().unwrap().pending_step, Some(-1));
    }

    #[tokio::test]
    async fn plus_minus_cycle_speed() {
        let mut state = AppState::new();
        let mut rs = replay_state();
        rs.speed = 1.0;
        state.replay = Some(rs);
        state.loading = false;

        handle_key_event(key(KeyCode::Char('+')), &mut state, &args()).await;
        assert_eq!(state.replay.as_ref().unwrap().speed, 2.0);
        handle_key_event(key(KeyCode::Char('-')), &mut state, &args()).await;
        assert_eq!(state.replay.as_ref().unwrap().speed, 1.0);
    }

    #[tokio::test]
    async fn j_k_seek_by_ten_seconds() {
        let mut state = AppState::new();
        let mut rs = replay_state();
        rs.elapsed = Duration::from_secs(30);
        state.replay = Some(rs);
        state.loading = false;

        handle_key_event(key(KeyCode::Char('k')), &mut state, &args()).await;
        assert_eq!(
            state.replay.as_ref().unwrap().pending_seek,
            Some(Duration::from_secs(40))
        );
        handle_key_event(key(KeyCode::Char('j')), &mut state, &args()).await;
        // j seeks backward from the same elapsed (30 - 10 = 20) because
        // elapsed isn't updated until the driver applies the previous
        // seek. This asserts the event-handler math uses the last known
        // elapsed, which matches how the driver overwrites pending_seek.
        assert_eq!(
            state.replay.as_ref().unwrap().pending_seek,
            Some(Duration::from_secs(20))
        );
    }

    #[tokio::test]
    async fn g_opens_timecode_editor() {
        let mut state = AppState::new();
        state.replay = Some(replay_state());
        state.loading = false;

        handle_key_event(key(KeyCode::Char('g')), &mut state, &args()).await;
        assert!(
            state.replay.as_ref().unwrap().timecode_input_mode,
            "g should open the timecode editor"
        );
        // Typing digits + colon accumulates into the buffer.
        for c in ['0', '0', ':', '1', '5'] {
            handle_key_event(key(KeyCode::Char(c)), &mut state, &args()).await;
        }
        assert_eq!(state.replay.as_ref().unwrap().timecode_buffer, "00:15");
        // Enter commits — pending_seek receives 15 seconds.
        handle_key_event(key(KeyCode::Enter), &mut state, &args()).await;
        let r = state.replay.as_ref().unwrap();
        assert_eq!(r.pending_seek, Some(Duration::from_secs(15)));
        assert!(!r.timecode_input_mode);
    }

    #[tokio::test]
    async fn capital_l_toggles_loop() {
        let mut state = AppState::new();
        state.replay = Some(replay_state());
        state.loading = false;

        handle_key_event(key(KeyCode::Char('L')), &mut state, &args()).await;
        assert!(state.replay.as_ref().unwrap().replay_loop);
        handle_key_event(key(KeyCode::Char('L')), &mut state, &args()).await;
        assert!(!state.replay.as_ref().unwrap().replay_loop);
    }

    #[tokio::test]
    async fn replay_keys_inert_when_replay_is_none() {
        // Regression guard: SPACE must not toggle anything when replay
        // mode is not active. Its default binding outside replay is
        // "no-op" — handle_navigation_keys receives it but does
        // nothing. If a future change accidentally wires SPACE to
        // pause, this test fails.
        let mut state = AppState::new();
        state.loading = false;
        state.replay = None;
        handle_key_event(key(KeyCode::Char(' ')), &mut state, &args()).await;
        // Nothing to assert about replay state — just that we did not
        // panic and did not create a replay control block out of thin
        // air.
        assert!(state.replay.is_none());
    }

    #[tokio::test]
    async fn filter_mode_wins_over_replay_mode() {
        // Regression guard for mode precedence: while the operator is
        // editing a filter query, typing `]` must go into the buffer,
        // NOT advance a replay frame.
        let mut state = AppState::new();
        state.replay = Some(replay_state());
        state.loading = false;
        handle_key_event(key(KeyCode::Char('/')), &mut state, &args()).await;
        assert_eq!(state.filter_input_mode, FilterInputMode::Editing);
        handle_key_event(key(KeyCode::Char(']')), &mut state, &args()).await;
        assert!(
            state.filter_buffer.contains(']'),
            "`]` must be literal text while filter editor is open"
        );
        assert_eq!(
            state.replay.as_ref().unwrap().pending_step,
            None,
            "filter mode must not leak keys into replay"
        );
    }

    /// Regression guard: typing past `FILTER_BUFFER_MAX` (512 bytes) must be
    /// silently dropped so a bracketed-paste of megabytes does not turn
    /// every subsequent keystroke into an O(n) re-parse of the entire buffer.
    #[tokio::test]
    async fn filter_buffer_capped_at_max() {
        let mut state = AppState::new();
        // Enter filter-edit mode.
        handle_key_event(key(KeyCode::Char('/')), &mut state, &args()).await;

        // Fill the buffer to exactly FILTER_BUFFER_MAX using 'a'.
        for _ in 0..FILTER_BUFFER_MAX {
            handle_key_event(key(KeyCode::Char('a')), &mut state, &args()).await;
        }
        assert_eq!(state.filter_buffer.len(), FILTER_BUFFER_MAX);

        // One more character must be silently dropped.
        handle_key_event(key(KeyCode::Char('z')), &mut state, &args()).await;
        assert_eq!(
            state.filter_buffer.len(),
            FILTER_BUFFER_MAX,
            "buffer grew past FILTER_BUFFER_MAX"
        );
        assert!(
            !state.filter_buffer.contains('z'),
            "overflow character was appended"
        );
    }
}
