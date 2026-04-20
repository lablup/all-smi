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

//! Renderer for the cluster-wide Users tab (issue #189).
//!
//! The renderer is deliberately pure: it consumes borrowed slices of
//! [`UserAggregate`] plus a few small view-state fields and writes into
//! a [`std::io::Write`] target.  No terminal-size probes happen here —
//! the caller passes the resolved width / available rows.

use std::io::Write;

use crossterm::{
    queue,
    style::{Color, Print},
};

use crate::app_state::UsersTabState;
use crate::ui::aggregation::user::{
    UNATTRIBUTED_DISPLAY, UNATTRIBUTED_USER, UserAggregate, UserAggregationResult, UserSortKey,
    format_longest, sort_users,
};
use crate::ui::renderers::utils::truncate_str;
use crate::ui::text::print_colored_text;

/// Column widths for the top-level Users table.  The rightmost
/// `COMMAND` column takes whatever space is left; these widths are the
/// fixed part.
const COL_USER: usize = 22;
const COL_NODES: usize = 6;
const COL_GPUS: usize = 6;
const COL_PROCS: usize = 6;
const COL_VRAM: usize = 10;
const COL_POWER: usize = 10;
const COL_LONGEST: usize = 16;
const COMMAND_MIN: usize = 12;

/// Layout constants for drill-down rows.
const DRILL_HOST: usize = 18;
const DRILL_GPUS: usize = 16;
const DRILL_VRAM: usize = 10;
const DRILL_POWER: usize = 10;
const DRILL_PIDS: usize = 6;

/// Result of [`render_users_tab`].
///
/// `visible_rows` tells the event handler how many row cursors Up/Down
/// can traverse so selection stays in bounds.  Currently consumed via
/// `AppState::users_aggregation()` directly rather than threaded back
/// through the render result; kept on the struct for future callers
/// that need per-frame bounds without re-running the aggregation.
pub struct UsersRenderResult {
    #[allow(dead_code)]
    pub visible_rows: usize,
}

/// Render the top-level Users table (or the drill-down view when the
/// operator has hit `Enter`).  Returns the number of rows currently
/// visible so callers can bound keyboard navigation.
pub fn render_users_tab<W: Write>(
    stdout: &mut W,
    aggregation: &UserAggregationResult,
    tab_state: &UsersTabState,
    cols: u16,
    rows_available: u16,
) -> UsersRenderResult {
    // Drill-down takes precedence when a user is selected.
    if let Some(user_name) = &tab_state.drill_user
        && let Some(user) = aggregation.users.iter().find(|u| u.user == *user_name)
    {
        return render_drill_down(stdout, user, tab_state, cols, rows_available);
    }

    // ------------------------------------------------------------------
    // Filter + sort the view.  This is only a vector copy — the pure
    // aggregation in `AppState::users_aggregation` is already cached by
    // data_version.
    // ------------------------------------------------------------------
    let mut rows: Vec<UserAggregate> = aggregation
        .users
        .iter()
        .filter(|u| !(tab_state.filter_sys && u.is_system))
        .cloned()
        .collect();
    sort_users(&mut rows, tab_state.sort);

    // ------------------------------------------------------------------
    // Banners
    // ------------------------------------------------------------------
    render_banner(stdout, aggregation, tab_state, &rows, cols as usize);

    // ------------------------------------------------------------------
    // Header
    // ------------------------------------------------------------------
    render_table_header(stdout, tab_state.sort, cols as usize);

    // ------------------------------------------------------------------
    // Body
    // ------------------------------------------------------------------
    let footer_rows: u16 = 2; // "Showing..." + in-tab hints
    let banner_rows: u16 = 3; // 2 banner lines + header + separator, approximated
    let body_budget = rows_available
        .saturating_sub(footer_rows)
        .saturating_sub(banner_rows) as usize;
    let body_budget = body_budget.max(1);

    if rows.is_empty() {
        render_empty_message(stdout, aggregation, cols as usize);
        // Even in the empty-state case, footer is still useful.
        render_footer(stdout, 0, 0, tab_state, cols as usize);
        return UsersRenderResult { visible_rows: 0 };
    }

    let visible_rows = rows.len().min(body_budget);
    let selected = tab_state.selected_row.min(visible_rows.saturating_sub(1));

    for (i, user) in rows.iter().take(visible_rows).enumerate() {
        render_user_row(stdout, user, i == selected, cols as usize);
    }

    render_footer(stdout, rows.len(), visible_rows, tab_state, cols as usize);

    UsersRenderResult { visible_rows }
}

// ---------------------------------------------------------------------
// Banners
// ---------------------------------------------------------------------

fn render_banner<W: Write>(
    stdout: &mut W,
    aggregation: &UserAggregationResult,
    tab_state: &UsersTabState,
    rows: &[UserAggregate],
    cols: usize,
) {
    // Summary line: totals.
    let total_vram = aggregation.users.iter().map(|u| u.vram_bytes).sum::<u64>();
    let total_users = rows.len();
    let total_nodes = aggregation.total_hosts;
    let mut summary = format!(
        " Users: {total_users}  |  Reporting nodes: {}/{total_nodes}  |  Total VRAM: {}",
        aggregation.reporting_hosts,
        format_bytes(total_vram),
    );
    if tab_state.filter_sys {
        summary.push_str("  |  sys-hidden");
    }
    print_colored_text(
        stdout,
        &pad_to_width(&summary, cols),
        Color::White,
        Some(Color::Blue),
        None,
    );
    queue!(stdout, Print("\r\n")).unwrap();

    // Partial-coverage chip.
    if aggregation.is_partial() {
        let chip = format!(
            " ⚠ partial coverage: {} of {} nodes reporting process data",
            aggregation.reporting_hosts, aggregation.total_hosts
        );
        print_colored_text(
            stdout,
            &pad_to_width(&chip, cols),
            Color::Black,
            Some(Color::Yellow),
            None,
        );
        queue!(stdout, Print("\r\n")).unwrap();
    }

    // Export toast (remembered across frames until the next export).
    if let Some(path) = &tab_state.last_export_path {
        let toast = format!(" ✓ Exported to {path}");
        print_colored_text(
            stdout,
            &pad_to_width(&toast, cols),
            Color::Green,
            None,
            None,
        );
        queue!(stdout, Print("\r\n")).unwrap();
    } else if let Some(err) = &tab_state.last_export_error {
        let toast = format!(" ✗ Export failed: {err}");
        print_colored_text(stdout, &pad_to_width(&toast, cols), Color::Red, None, None);
        queue!(stdout, Print("\r\n")).unwrap();
    }
}

// ---------------------------------------------------------------------
// Empty-state message
// ---------------------------------------------------------------------

fn render_empty_message<W: Write>(
    stdout: &mut W,
    aggregation: &UserAggregationResult,
    cols: usize,
) {
    let msg = if aggregation.reporting_hosts == 0 {
        "  No process data is available on any host.  Start the API server with \
         `all-smi api --processes` to populate this tab."
    } else {
        "  No user matches the current filter.  Press `f` to show system \
         accounts."
    };
    let _ = cols; // explicit: we pad in print_colored_text via truncate below
    print_colored_text(stdout, msg, Color::DarkGrey, None, None);
    queue!(stdout, Print("\r\n")).unwrap();
}

// ---------------------------------------------------------------------
// Header
// ---------------------------------------------------------------------

fn render_table_header<W: Write>(stdout: &mut W, sort: UserSortKey, cols: usize) {
    let header = format_user_header(sort, cols);
    print_colored_text(stdout, &header, Color::Black, Some(Color::White), None);
    queue!(stdout, Print("\r\n")).unwrap();
}

fn format_user_header(sort: UserSortKey, cols: usize) -> String {
    let command_width = command_column_width(cols);
    let mk = |label: &str, key: UserSortKey| {
        if key == sort {
            format!("▼{label}")
        } else {
            format!(" {label}")
        }
    };
    let user = mk("USER", UserSortKey::User);
    let nodes = mk("NODES", UserSortKey::Nodes);
    let gpus = " GPUs";
    let procs = " PROCS";
    let vram = mk("VRAM", UserSortKey::Memory);
    let power = mk("POWER*", UserSortKey::Power);
    let longest = mk("LONGEST", UserSortKey::Longest);
    let cmd = "CMD (top-1 by GPU mem)";
    let user_w = COL_USER;
    let nodes_w = COL_NODES + 1; // extra space for the sort marker
    let gpus_w = COL_GPUS;
    let procs_w = COL_PROCS;
    let vram_w = COL_VRAM + 1;
    let power_w = COL_POWER + 1;
    let longest_w = COL_LONGEST + 1;
    let cmd_w = command_width;
    format!(
        "{user:<user_w$}{nodes:>nodes_w$}{gpus:>gpus_w$}{procs:>procs_w$}{vram:>vram_w$}{power:>power_w$}{longest:>longest_w$} {cmd:<cmd_w$}",
    )
    .chars()
    .take(cols)
    .collect()
}

fn command_column_width(cols: usize) -> usize {
    // Fixed columns + spaces between.  See render_user_row for the
    // exact layout; we mirror the widths here.
    let fixed = COL_USER + COL_NODES + COL_GPUS + COL_PROCS + COL_VRAM + COL_POWER + COL_LONGEST;
    cols.saturating_sub(fixed + 1).max(COMMAND_MIN)
}

// ---------------------------------------------------------------------
// Rows
// ---------------------------------------------------------------------

fn render_user_row<W: Write>(stdout: &mut W, user: &UserAggregate, selected: bool, cols: usize) {
    let display_user = if user.user == UNATTRIBUTED_USER {
        UNATTRIBUTED_DISPLAY.to_string()
    } else {
        truncate_str(&user.user, COL_USER)
    };
    let command_width = command_column_width(cols);
    let nodes = user.node_count;
    let gpus = user.gpu_count;
    let procs = user.process_count;
    let vram = format_bytes(user.vram_bytes);
    let power = format_power(user.power_watts);
    let longest = format_longest(user.longest_seconds);
    let cmd = truncate_str(&user.top_command, command_width);
    let user_w = COL_USER;
    let nodes_w = COL_NODES;
    let gpus_w = COL_GPUS;
    let procs_w = COL_PROCS;
    let vram_w = COL_VRAM;
    let power_w = COL_POWER;
    let longest_w = COL_LONGEST;
    let cmd_w = command_width;
    let row = format!(
        "{display_user:<user_w$}{nodes:>nodes_w$}{gpus:>gpus_w$}{procs:>procs_w$}{vram:>vram_w$}{power:>power_w$}{longest:>longest_w$} {cmd:<cmd_w$}",
    );
    let row_trunc: String = row.chars().take(cols).collect();
    let fg = if user.is_system {
        Color::DarkGrey
    } else {
        Color::White
    };
    if selected {
        print_colored_text(stdout, &row_trunc, Color::Black, Some(Color::Cyan), None);
    } else {
        print_colored_text(stdout, &row_trunc, fg, None, None);
    }
    queue!(stdout, Print("\r\n")).unwrap();
}

// ---------------------------------------------------------------------
// Footer
// ---------------------------------------------------------------------

fn render_footer<W: Write>(
    stdout: &mut W,
    total: usize,
    visible: usize,
    tab_state: &UsersTabState,
    cols: usize,
) {
    let showing = format!(
        "  Showing {visible}/{total} users  |  sort: {}  |  sys: {}",
        describe_sort(tab_state.sort),
        if tab_state.filter_sys {
            "hidden"
        } else {
            "shown"
        },
    );
    print_colored_text(
        stdout,
        &pad_to_width(&showing, cols),
        Color::DarkGrey,
        None,
        None,
    );
    queue!(stdout, Print("\r\n")).unwrap();

    let hints = "  Keys: u user  m memory  p power  n nodes  t longest | \
                 Enter drill  ESC back  f filter-sys  e export CSV";
    print_colored_text(stdout, &pad_to_width(hints, cols), Color::Green, None, None);
    queue!(stdout, Print("\r\n")).unwrap();
}

fn describe_sort(key: UserSortKey) -> &'static str {
    match key {
        UserSortKey::User => "user",
        UserSortKey::Memory => "memory",
        UserSortKey::Power => "power*",
        UserSortKey::Nodes => "nodes",
        UserSortKey::Longest => "longest",
    }
}

// ---------------------------------------------------------------------
// Drill-down
// ---------------------------------------------------------------------

fn render_drill_down<W: Write>(
    stdout: &mut W,
    user: &UserAggregate,
    tab_state: &UsersTabState,
    cols: u16,
    rows_available: u16,
) -> UsersRenderResult {
    let cols = cols as usize;
    let banner = format!(
        " User: {user}  |  {nodes} nodes  |  {gpus} GPUs  |  VRAM {vram}  |  POWER* {power}",
        user = if user.user == UNATTRIBUTED_USER {
            UNATTRIBUTED_DISPLAY.to_string()
        } else {
            user.user.clone()
        },
        nodes = user.node_count,
        gpus = user.gpu_count,
        vram = format_bytes(user.vram_bytes),
        power = format_power(user.power_watts),
    );
    print_colored_text(
        stdout,
        &pad_to_width(&banner, cols),
        Color::Black,
        Some(Color::Cyan),
        None,
    );
    queue!(stdout, Print("\r\n")).unwrap();

    let sub = format!(
        "  ESC to exit drill-down  |  {procs} processes running",
        procs = user.process_count
    );
    print_colored_text(
        stdout,
        &pad_to_width(&sub, cols),
        Color::DarkGrey,
        None,
        None,
    );
    queue!(stdout, Print("\r\n")).unwrap();

    let host = "HOST";
    let gpus_hdr = "GPUs";
    let vram_hdr = "VRAM";
    let power_hdr = "POWER*";
    let pids_hdr = "PIDS";
    let host_w = DRILL_HOST;
    let gpus_w = DRILL_GPUS;
    let vram_w = DRILL_VRAM;
    let power_w = DRILL_POWER;
    let pids_w = DRILL_PIDS;
    let header = format!(
        "{host:<host_w$}{gpus_hdr:<gpus_w$}{vram_hdr:>vram_w$}{power_hdr:>power_w$}{pids_hdr:>pids_w$} COMMAND",
    );
    let header_trunc: String = header.chars().take(cols).collect();
    print_colored_text(
        stdout,
        &pad_to_width(&header_trunc, cols),
        Color::Black,
        Some(Color::White),
        None,
    );
    queue!(stdout, Print("\r\n")).unwrap();

    let fixed = DRILL_HOST + DRILL_GPUS + DRILL_VRAM + DRILL_POWER + DRILL_PIDS;
    let command_width = cols.saturating_sub(fixed + 1).max(COMMAND_MIN);

    let budget = rows_available.saturating_sub(4) as usize;
    let body_budget = budget.max(1);
    let visible_rows = user.per_host.len().min(body_budget);
    let selected_host = tab_state.drill_host.as_deref();

    for (i, per_host) in user.per_host.iter().take(visible_rows).enumerate() {
        let gpu_range = format_gpu_range(&per_host.gpu_indices);
        let host = truncate_str(&per_host.host, DRILL_HOST - 1);
        let gpus = truncate_str(&gpu_range, DRILL_GPUS - 1);
        let vram = format_bytes(per_host.vram_bytes);
        let power = format_power(per_host.power_watts);
        let pids = per_host.pid_count;
        let cmd = truncate_str(&per_host.top_command, command_width);
        let host_w = DRILL_HOST;
        let gpus_w = DRILL_GPUS;
        let vram_w = DRILL_VRAM;
        let power_w = DRILL_POWER;
        let pids_w = DRILL_PIDS;
        let cmd_w = command_width;
        let row = format!(
            "{host:<host_w$}{gpus:<gpus_w$}{vram:>vram_w$}{power:>power_w$}{pids:>pids_w$} {cmd:<cmd_w$}",
        );
        let row_trunc: String = row.chars().take(cols).collect();

        let is_selected_host = selected_host == Some(per_host.host.as_str())
            || (selected_host.is_none() && i == tab_state.selected_row.min(visible_rows - 1));
        if is_selected_host {
            print_colored_text(stdout, &row_trunc, Color::Black, Some(Color::Cyan), None);
        } else {
            print_colored_text(stdout, &row_trunc, Color::White, None, None);
        }
        queue!(stdout, Print("\r\n")).unwrap();
    }

    UsersRenderResult { visible_rows }
}

/// Collapse an ordered set of GPU indices into a run-length string
/// (`0-3, 5, 8-9`).  Makes the drill-down `GPUs` column readable.
fn format_gpu_range(indices: &std::collections::BTreeSet<u32>) -> String {
    if indices.is_empty() {
        return String::new();
    }
    let mut parts: Vec<String> = Vec::new();
    let mut iter = indices.iter().copied();
    let Some(mut start) = iter.next() else {
        return String::new();
    };
    let mut end = start;
    for i in iter {
        if i == end + 1 {
            end = i;
        } else {
            parts.push(fmt_range(start, end));
            start = i;
            end = i;
        }
    }
    parts.push(fmt_range(start, end));
    parts.join(",")
}

fn fmt_range(start: u32, end: u32) -> String {
    if start == end {
        start.to_string()
    } else {
        format!("{start}-{end}")
    }
}

// ---------------------------------------------------------------------
// Formatting helpers
// ---------------------------------------------------------------------

/// Format bytes as a human-readable string with 2-digit resolution.
/// Always emits an ASCII unit suffix so column widths stay predictable.
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB"];
    if bytes == 0 {
        return "0 B".to_string();
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if value >= 100.0 || unit == 0 {
        format!("{value:.0} {}", UNITS[unit])
    } else if value >= 10.0 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.2} {}", UNITS[unit])
    }
}

/// Format watts as kW when >= 1 000 W, otherwise as W.  `—` for zero.
pub fn format_power(watts: f64) -> String {
    if watts <= 0.0 {
        return "—".to_string();
    }
    if watts >= 1_000.0 {
        format!("{:.2} kW", watts / 1_000.0)
    } else if watts >= 100.0 {
        format!("{watts:.0} W")
    } else {
        format!("{watts:.1} W")
    }
}

fn pad_to_width(s: &str, cols: usize) -> String {
    let trunc: String = s.chars().take(cols).collect();
    let len = trunc.chars().count();
    if len >= cols {
        trunc
    } else {
        format!("{trunc}{}", " ".repeat(cols - len))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_bytes_renders_readable_units() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1024), "1.00 KiB");
        assert_eq!(format_bytes(1024 * 1024), "1.00 MiB");
        assert!(format_bytes(1024u64.pow(3) * 384).starts_with("384"));
    }

    #[test]
    fn format_power_respects_zero_and_kw_boundary() {
        assert_eq!(format_power(0.0), "—");
        assert_eq!(format_power(-3.5), "—");
        assert_eq!(format_power(12.0), "12.0 W");
        assert!(format_power(999.0).ends_with("W"));
        assert!(format_power(1_500.0).contains("kW"));
    }

    #[test]
    fn format_gpu_range_collapses_consecutive_indices() {
        let mut s: std::collections::BTreeSet<u32> = Default::default();
        s.extend([0, 1, 2, 3, 5, 8, 9]);
        assert_eq!(format_gpu_range(&s), "0-3,5,8-9");
    }

    #[test]
    fn format_gpu_range_handles_empty_and_singletons() {
        let empty: std::collections::BTreeSet<u32> = Default::default();
        assert_eq!(format_gpu_range(&empty), "");
        let mut single: std::collections::BTreeSet<u32> = Default::default();
        single.insert(7);
        assert_eq!(format_gpu_range(&single), "7");
    }
}
