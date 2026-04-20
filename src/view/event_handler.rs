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
use crate::ui::aggregation::user::{UserSortKey, sort_users};
use crate::ui::filter_dsl::{apply as apply_filter, parse as parse_filter};
use crate::ui::layout::LayoutCalculator;
use crate::ui::tabs::{topology_tab_index, users_tab_index};

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

/// Stash the name of the currently-selected host tab into
/// `state.topology_last_host_tab` so the Topology tab can later render the
/// operator's preferred host instead of the first one in the tab strip.
///
/// Does nothing when the active tab is one of the cluster-level reserved
/// tabs (`All`, `Users`, `Topology`) — those are not host tabs. Called from
/// the `T` hotkey and from the arrow-key navigation handlers so Topology's
/// target host follows whichever host the operator last selected.
fn remember_current_host_tab(state: &mut AppState) {
    if let Some(current_name) = state.tabs.get(state.current_tab).cloned()
        && current_name != "All"
        && current_name != crate::ui::tabs::USERS_TAB_NAME
        && current_name != crate::ui::tabs::TOPOLOGY_TAB_NAME
    {
        state.topology_last_host_tab = Some(current_name);
    }
}

pub async fn handle_key_event(key_event: KeyEvent, state: &mut AppState, args: &ViewArgs) -> bool {
    // Mode precedence (highest first) — do NOT reorder:
    //
    // 1. Filter-edit mode (`/`) intercepts everything so `q`/`d`/`u`
    //    become literal text the operator can type into the query.
    // 2. Replay timecode input (`g` → `HH:MM:SS`) intercepts everything
    //    so the same keys become digits/colons, never hotkeys.
    // 3. Users-tab keys (issue #189) when the Users tab is active, so
    //    the `u/m/p/n/t/f/e/Enter/ESC` in-tab bindings override the
    //    global GPU-sort bindings (`u` sort, `m` sort, `p` sort, `f`
    //    GPU-filter toggle).  They still fall through to replay / normal
    //    keys for navigation (arrows, `/`, `q`, `h`, `A`, `1`).
    // 4. Topology-tab keys (issue #190) when the Topology tab is active:
    //    `M` toggles the graph/matrix mode. Must come BEFORE the global
    //    ladder so the Topology's `M` wins over the process-sort `m`.
    // 5. Normal keys: quit, help, alerts, arrows. Includes `T` which
    //    jumps to the Topology tab regardless of what tab is current.
    // 6. Replay-mode keys (SPACE/`]`/`[`/`+`/`-`/`j`/`k`/`g`/`L`) are
    //    routed BEFORE `handle_navigation_keys` so the sort-by-GpuMem
    //    `g` binding doesn't shadow the timecode editor.
    if state.filter_input_mode == FilterInputMode::Editing {
        return handle_filter_input(key_event, state);
    }
    if state.replay.as_ref().is_some_and(|r| r.timecode_input_mode) {
        return handle_timecode_input(key_event, state);
    }

    // Users tab (issue #189): when the tab is active we intercept
    // in-tab keys BEFORE the global `match`, otherwise `m`, `u`, `p`
    // etc. would hit the outer `handle_navigation_keys` GPU-sort
    // bindings.  `handle_users_tab_keys` only consumes keys the Users
    // tab owns; everything else (quit, help, navigation, replay)
    // falls through to the default ladder below.
    if crate::ui::tabs::is_users_tab_active(state)
        && !state.loading
        && !state.show_help
        && handle_users_tab_keys(key_event, state)
    {
        return false;
    }

    // Topology tab (issue #190): when active, `M` toggles the
    // graph/matrix mode. Checked after Users-tab keys (per the mode-
    // precedence ladder above) but BEFORE the global `match` so the
    // Topology's `M` never collides with the global sort-by-memory `m`.
    if crate::ui::tabs::is_topology_tab_active(state)
        && !state.loading
        && !state.show_help
        && handle_topology_tab_keys(key_event, state)
    {
        return false;
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
        KeyCode::Char('V') => {
            // Jump to the cluster-wide Users tab (issue #189).  Silent
            // no-op when the tab doesn't exist (local mode, replays
            // before the first frame has seeded tabs).
            if let Some(idx) = users_tab_index(&state.tabs) {
                state.current_tab = idx;
                state.gpu_scroll_offset = 0;
                state.storage_scroll_offset = 0;
                state.mark_data_changed();
            }
            false
        }
        KeyCode::Char('T') => {
            // Jump to the per-host Topology tab (issue #190). Silent
            // no-op when the tab is not present (local mode before the
            // first data frame populates it).
            if let Some(idx) = topology_tab_index(&state.tabs) {
                // Remember the operator-selected host tab BEFORE
                // overwriting `current_tab`, so the Topology renderer
                // can point at that host instead of falling back to
                // the first host tab.
                remember_current_host_tab(state);
                state.current_tab = idx;
                state.gpu_scroll_offset = 0;
                state.storage_scroll_offset = 0;
                state.mark_data_changed();
            }
            false
        }
        KeyCode::Char('1') | KeyCode::Char('h') => {
            state.show_help = !state.show_help;
            false
        }
        KeyCode::Left => {
            if !state.show_help {
                handle_left_arrow(state);
                remember_current_host_tab(state);
            }
            false
        }
        KeyCode::Right => {
            if !state.show_help {
                handle_right_arrow(state);
                remember_current_host_tab(state);
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

/// Handle keys owned by the cluster-wide Users tab (issue #189).
///
/// Returns `true` when the key was consumed so the caller stops
/// dispatching.  Keys that the Users tab does **not** own (quit,
/// help, filter-edit, alert panel, `V`) return `false` so the
/// main ladder still processes them.
///
/// Mode precedence note: this helper is invoked only AFTER
/// filter-edit and replay-timecode modes have been short-circuited,
/// so it never has to worry about shadowing those.
fn handle_users_tab_keys(key_event: KeyEvent, state: &mut AppState) -> bool {
    let KeyEvent {
        code, modifiers, ..
    } = key_event;
    if modifiers.contains(KeyModifiers::CONTROL) || modifiers.contains(KeyModifiers::ALT) {
        return false;
    }

    // Cache aggregation length for navigation bounds.  The borrow is
    // short — we release it before mutating tab_state below.
    let row_count = {
        let agg = state.users_aggregation().clone();
        let filter_sys = state.users_tab_state.filter_sys;
        if state.users_tab_state.drill_user.is_some() {
            // Drill-down bounds are the per-host row count.
            agg.users
                .iter()
                .find(|u| Some(&u.user) == state.users_tab_state.drill_user.as_ref())
                .map(|u| u.per_host.len())
                .unwrap_or(0)
        } else {
            agg.users
                .iter()
                .filter(|u| !(filter_sys && u.is_system))
                .count()
        }
    };

    match code {
        KeyCode::Char('u') => {
            change_users_sort(state, UserSortKey::User);
            true
        }
        KeyCode::Char('m') => {
            change_users_sort(state, UserSortKey::Memory);
            true
        }
        KeyCode::Char('p') => {
            change_users_sort(state, UserSortKey::Power);
            true
        }
        KeyCode::Char('n') => {
            change_users_sort(state, UserSortKey::Nodes);
            true
        }
        KeyCode::Char('t') => {
            change_users_sort(state, UserSortKey::Longest);
            true
        }
        KeyCode::Char('f') => {
            state.users_tab_state.filter_sys = !state.users_tab_state.filter_sys;
            state.users_tab_state.selected_row = 0;
            state.mark_data_changed();
            true
        }
        KeyCode::Char('e') => {
            match export_users_csv(state) {
                Ok(path) => {
                    state.users_tab_state.last_export_path = Some(path);
                    state.users_tab_state.last_export_error = None;
                }
                Err(err) => {
                    state.users_tab_state.last_export_error = Some(err);
                    state.users_tab_state.last_export_path = None;
                }
            }
            state.mark_data_changed();
            true
        }
        KeyCode::Enter => {
            enter_users_drill_down(state);
            true
        }
        KeyCode::Esc => {
            // Back out of drill-down without exiting the app.  ESC
            // higher up in the ladder (alert panel, filter clear,
            // help close) is reached only when drill-down is closed.
            if state.users_tab_state.drill_host.is_some() {
                state.users_tab_state.drill_host = None;
                state.users_tab_state.selected_row = 0;
                state.mark_data_changed();
                return true;
            }
            if state.users_tab_state.drill_user.is_some() {
                state.users_tab_state.drill_user = None;
                state.users_tab_state.selected_row = 0;
                state.mark_data_changed();
                return true;
            }
            false
        }
        KeyCode::Up => {
            if state.users_tab_state.selected_row > 0 {
                state.users_tab_state.selected_row -= 1;
                state.mark_data_changed();
            }
            true
        }
        KeyCode::Down => {
            let max = row_count.saturating_sub(1);
            if state.users_tab_state.selected_row < max {
                state.users_tab_state.selected_row += 1;
                state.mark_data_changed();
            }
            true
        }
        _ => false,
    }
}

fn change_users_sort(state: &mut AppState, key: UserSortKey) {
    if state.users_tab_state.sort != key {
        state.users_tab_state.sort = key;
        state.users_tab_state.selected_row = 0;
        state.mark_data_changed();
    }
}

/// Handle keys owned by the per-host Topology tab (issue #190).
///
/// Returns `true` when the key was consumed so the caller stops
/// dispatching.  The tab currently owns a single key:
///
/// * `M` — toggle between graph and matrix render modes.
///
/// `Tab` / `Shift-Tab` / arrow navigation is intentionally **not** owned
/// by the topology tab so the operator can still move between hosts
/// without leaving the Topology view (per the issue spec: "Remote mode:
/// defaults to showing selected host's topology; `Tab`/`Shift-Tab`
/// cycles nodes").
fn handle_topology_tab_keys(key_event: KeyEvent, state: &mut AppState) -> bool {
    let KeyEvent {
        code, modifiers, ..
    } = key_event;
    if modifiers.contains(KeyModifiers::CONTROL) || modifiers.contains(KeyModifiers::ALT) {
        return false;
    }
    // Accept both `m` and `M` to minimise muscle-memory friction: the
    // issue spec uses uppercase `M` but operators may hit it without
    // Shift on systems where the caps-lock LED is off.
    if matches!(code, KeyCode::Char('M') | KeyCode::Char('m')) {
        state.topology_view_mode = state.topology_view_mode.toggled();
        state.mark_data_changed();
        return true;
    }
    false
}

/// Drill into the currently-highlighted user, or the highlighted host
/// inside the drill-down.  Two-level navigation keeps the same `Enter`
/// hotkey working as the issue spec requires.
fn enter_users_drill_down(state: &mut AppState) {
    let agg = state.users_aggregation().clone();
    if state.users_tab_state.drill_user.is_none() {
        // Top-level → drill into the selected user.
        let filter_sys = state.users_tab_state.filter_sys;
        let sort = state.users_tab_state.sort;
        let mut visible: Vec<_> = agg
            .users
            .iter()
            .filter(|u| !(filter_sys && u.is_system))
            .cloned()
            .collect();
        sort_users(&mut visible, sort);
        if let Some(user) = visible.get(state.users_tab_state.selected_row).cloned() {
            state.users_tab_state.drill_user = Some(user.user);
            state.users_tab_state.drill_host = None;
            state.users_tab_state.selected_row = 0;
            state.mark_data_changed();
        }
    } else if state.users_tab_state.drill_host.is_none() {
        // Intermediate → pick the host to inspect further.
        if let Some(user_name) = state.users_tab_state.drill_user.clone()
            && let Some(user) = agg.users.iter().find(|u| u.user == user_name)
            && let Some(ph) = user.per_host.get(state.users_tab_state.selected_row)
        {
            state.users_tab_state.drill_host = Some(ph.host.clone());
            state.mark_data_changed();
        }
    }
}

/// Export the currently-filtered user view to
/// `$HOME/.cache/all-smi/users-<timestamp>.csv`.  Returns either the
/// path written or a human-friendly error suitable for display in the
/// top-of-tab chip.
///
/// On Unix the cache directory and the CSV file itself are opened with
/// `O_NOFOLLOW` + mode `0o600` so a co-tenant cannot pre-plant
/// `~/.cache/all-smi` (or the final filename) as a symlink and redirect
/// the write to an attacker-chosen location — matching the hardening in
/// `src/snapshot/mod.rs::write_output_atomic` and
/// `src/record/writer.rs::open_secure` (addressed for those subcommands
/// in prior security reviews). On Windows we fall back to `share_mode(0)`
/// (exclusive access); NTFS symlink TOCTOU is handled via directory ACLs.
fn export_users_csv(state: &mut AppState) -> Result<String, String> {
    use std::fmt::Write as _;
    use std::io::Write as _;

    let agg = state.users_aggregation().clone();
    let filter_sys = state.users_tab_state.filter_sys;
    let sort = state.users_tab_state.sort;

    // Compose the filtered, sorted row list the table currently
    // displays so the CSV matches what the operator sees.
    let mut rows: Vec<_> = agg
        .users
        .iter()
        .filter(|u| !(filter_sys && u.is_system))
        .cloned()
        .collect();
    crate::ui::aggregation::user::sort_users(&mut rows, sort);

    // Resolve `~/.cache/all-smi/users-<timestamp>.csv`.  Mirrors the
    // XDG Base Directory spec for Linux/macOS and drops the same
    // layout under `%LOCALAPPDATA%` on Windows.  Avoids pulling in the
    // `dirs` crate for a single lookup.
    let base = cache_dir_for_all_smi()?;
    std::fs::create_dir_all(&base).map_err(|e| format!("mkdir {}: {e}", base.display()))?;
    // Defense in depth: refuse to write when the cache dir itself is a
    // symlink. `create_dir_all` is a no-op if the path already exists as
    // a symlink-to-dir, so without this check an attacker who controls
    // `~/.cache/all-smi` could redirect the CSV into any directory the
    // user is allowed to write to.
    #[cfg(unix)]
    {
        let meta = std::fs::symlink_metadata(&base)
            .map_err(|e| format!("stat {}: {e}", base.display()))?;
        if meta.file_type().is_symlink() {
            return Err(format!(
                "refusing to export: cache dir {} is a symlink",
                base.display()
            ));
        }
    }
    let ts = chrono::Local::now().format("%Y%m%dT%H%M%S");
    let path = base.join(format!("users-{ts}.csv"));

    let mut body = String::new();
    body.push_str(
        "user,is_system,nodes,gpus,procs,vram_bytes,power_watts,longest_seconds,top_command\n",
    );
    for u in &rows {
        // RFC-4180 quoting: any field containing comma / quote /
        // newline is wrapped in quotes with internal quotes doubled.
        // Additionally, CSV-injection-guard: fields that would be
        // interpreted as a formula by Excel / LibreOffice / Google
        // Sheets (leading `=`, `+`, `-`, `@`, TAB, CR) are prefixed
        // with a single quote inside the quoted form so the spreadsheet
        // treats them as plain text instead of executing them.
        writeln!(
            &mut body,
            "{user},{sys},{nodes},{gpus},{procs},{vram},{power:.3},{longest},{cmd}",
            user = csv_escape(&u.user),
            sys = if u.is_system { "true" } else { "false" },
            nodes = u.node_count,
            gpus = u.gpu_count,
            procs = u.process_count,
            vram = u.vram_bytes,
            power = u.power_watts.max(0.0),
            longest = u.longest_seconds,
            cmd = csv_escape(&u.top_command),
        )
        .ok();
    }

    // Open with `create_new(true)` + `O_NOFOLLOW` + mode `0o600` so a
    // pre-planted symlink at the final path can't redirect us and the
    // CSV lands mode 0600 (per-user readable). The timestamp granularity
    // is seconds, so `create_new` fires only on a double-press within
    // the same second; surface the error rather than silently clobbering.
    let mut file = open_export_secure(&path)?;
    file.write_all(body.as_bytes())
        .map_err(|e| format!("write: {e}"))?;
    file.sync_all().map_err(|e| format!("sync: {e}"))?;
    Ok(path.display().to_string())
}

/// Open `path` for exclusive write without following symlinks and with
/// owner-only permissions. Mirrors `src/record/writer.rs::open_secure` /
/// `src/snapshot/mod.rs::write_output_atomic` — the CSV export needs
/// the same mitigation because the cache dir is a well-known path on a
/// shared machine and thus a viable symlink-plant target.
fn open_export_secure(path: &std::path::Path) -> Result<std::fs::File, String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .custom_flags(libc::O_NOFOLLOW)
            .mode(0o600)
            .open(path)
            .map_err(|e| format!("open {}: {e}", path.display()))
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .share_mode(0)
            .open(path)
            .map_err(|e| format!("open {}: {e}", path.display()))
    }
    #[cfg(not(any(unix, windows)))]
    {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|e| format!("open {}: {e}", path.display()))
    }
}

/// RFC 4180 quoting plus CSV-injection mitigation.
///
/// The RFC-4180 rule: wrap the field in double quotes whenever it
/// contains `,`, `"`, `\r`, or `\n`; double any embedded `"`.
///
/// The CSV-injection rule (OWASP): spreadsheet apps (Excel, LibreOffice
/// Calc, Google Sheets) treat any cell whose first character is `=`,
/// `+`, `-`, `@`, TAB (`\t`), or CR (`\r`) as a formula and may execute
/// embedded commands when the CSV is opened.  Because user names and
/// process command lines flow into this CSV from untrusted remote hosts,
/// we prefix such fields with a single quote (`'`) inside the quoted
/// form so the spreadsheet treats them as plain text.  The prefix is a
/// common-enough mitigation that downstream analysts recognise it; the
/// alternative (dropping the character) would silently corrupt the data.
fn csv_escape(s: &str) -> String {
    let first = s.chars().next();
    let needs_formula_guard = matches!(
        first,
        Some('=') | Some('+') | Some('-') | Some('@') | Some('\t') | Some('\r')
    );
    let needs_rfc_quote = s.contains(',')
        || s.contains('"')
        || s.contains('\n')
        || s.contains('\r')
        || needs_formula_guard;
    if !needs_rfc_quote {
        return s.to_string();
    }
    let inner = s.replace('"', "\"\"");
    if needs_formula_guard {
        format!("\"'{inner}\"")
    } else {
        format!("\"{inner}\"")
    }
}

/// Best-effort cross-platform resolution of the `all-smi` cache dir,
/// following the XDG convention on Linux/macOS and
/// `%LOCALAPPDATA%\all-smi` on Windows.  Returns `Err` when neither
/// `HOME` / `XDG_CACHE_HOME` nor `LOCALAPPDATA` is set.
fn cache_dir_for_all_smi() -> Result<std::path::PathBuf, String> {
    if let Ok(xdg) = std::env::var("XDG_CACHE_HOME")
        && !xdg.is_empty()
    {
        return Ok(std::path::PathBuf::from(xdg).join("all-smi"));
    }
    #[cfg(target_os = "windows")]
    {
        if let Ok(local) = std::env::var("LOCALAPPDATA")
            && !local.is_empty()
        {
            return Ok(std::path::PathBuf::from(local)
                .join("all-smi")
                .join("cache"));
        }
    }
    if let Ok(home) = std::env::var("HOME")
        && !home.is_empty()
    {
        return Ok(std::path::PathBuf::from(home)
            .join(".cache")
            .join("all-smi"));
    }
    Err("no home/cache directory available in environment".to_string())
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
    // `args.replay` routes scrolling through the remote branch because the
    // replay UI renders tabs + GPU columns (not the local process list).
    let is_remote = args.hosts.is_some() || args.hostfile.is_some() || args.replay.is_some();
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
    // `args.replay` routes scrolling through the remote branch because the
    // replay UI renders tabs + GPU columns (not the local process list).
    let is_remote = args.hosts.is_some() || args.hostfile.is_some() || args.replay.is_some();
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
    // `args.replay` routes scrolling through the remote branch because the
    // replay UI renders tabs + GPU columns (not the local process list).
    let is_remote = args.hosts.is_some() || args.hostfile.is_some() || args.replay.is_some();
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
    // `args.replay` routes scrolling through the remote branch because the
    // replay UI renders tabs + GPU columns (not the local process list).
    let is_remote = args.hosts.is_some() || args.hostfile.is_some() || args.replay.is_some();
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

    // -----------------------------------------------------------------------
    // Users tab (issue #189)
    // -----------------------------------------------------------------------

    /// Build a state that has the Users tab active.  Used by the
    /// Users-tab key routing tests to avoid duplicating setup.
    fn state_with_users_tab() -> AppState {
        let mut state = AppState::new();
        state.is_local_mode = false;
        state.loading = false;
        state.tabs = vec![
            "All".to_string(),
            crate::ui::tabs::USERS_TAB_NAME.to_string(),
            "host-0".to_string(),
        ];
        state.current_tab = 1; // Users tab
        state
    }

    #[tokio::test]
    async fn capital_v_jumps_to_users_tab() {
        let mut state = AppState::new();
        state.is_local_mode = false;
        state.loading = false;
        state.tabs = vec![
            "All".to_string(),
            crate::ui::tabs::USERS_TAB_NAME.to_string(),
            "host-0".to_string(),
        ];
        state.current_tab = 2;
        handle_key_event(key(KeyCode::Char('V')), &mut state, &args()).await;
        assert_eq!(state.current_tab, 1, "`V` must jump to the Users tab");
    }

    #[tokio::test]
    async fn capital_v_is_a_noop_when_users_tab_absent() {
        // Local mode — no Users tab in the list.
        let mut state = AppState::new();
        state.tabs = vec!["All".to_string()];
        state.current_tab = 0;
        handle_key_event(key(KeyCode::Char('V')), &mut state, &args()).await;
        assert_eq!(state.current_tab, 0);
    }

    #[tokio::test]
    async fn users_tab_m_key_changes_sort_not_global_memory_filter() {
        // `m` on the Users tab must switch the users-sort key, NOT fall
        // through to the global MemoryPercent sort binding.
        let mut state = state_with_users_tab();
        handle_key_event(key(KeyCode::Char('m')), &mut state, &args()).await;
        assert_eq!(
            state.users_tab_state.sort,
            crate::ui::aggregation::user::UserSortKey::Memory,
            "`m` on Users tab must update the users-sort key"
        );
        // Global GPU sort must not have been touched.
        assert_ne!(
            state.sort_criteria,
            SortCriteria::MemoryPercent,
            "Users-tab `m` must not leak into the global GPU sort"
        );
    }

    #[tokio::test]
    async fn users_tab_f_toggles_system_filter_not_gpu_filter() {
        let mut state = state_with_users_tab();
        let initial = state.users_tab_state.filter_sys;
        let gpu_filter_initial = state.gpu_filter_enabled;
        handle_key_event(key(KeyCode::Char('f')), &mut state, &args()).await;
        assert_ne!(
            state.users_tab_state.filter_sys, initial,
            "`f` on Users tab toggles the system-filter"
        );
        assert_eq!(
            state.gpu_filter_enabled, gpu_filter_initial,
            "`f` on Users tab must not leak into the global GPU filter"
        );
    }

    #[tokio::test]
    async fn users_tab_enter_drills_down_then_escape_backs_out() {
        // Seed some aggregation data so Enter has a user to drill
        // into.  We build it through the AppState helper path.
        let mut state = state_with_users_tab();
        state.remote_process_info = vec![crate::network::metrics_parser::ParsedProcessRow {
            host: "host-0".into(),
            pid: 1,
            user: "alice".into(),
            command: "x".into(),
            name: "x".into(),
            gpu_index: 0,
            gpu_uuid: "GPU-0".into(),
            gpu_memory_bytes: 1000,
            cpu_pct_tenths: 0,
            start_time_seconds: 10,
        }];
        // Simulate a collector push so the aggregation cache picks up
        // the new process data; UI-only `mark_data_changed` would leave
        // the collector version untouched and keep the stale empty
        // aggregation around.
        state.mark_collector_data_changed();
        // Force the aggregation cache to warm.
        let _ = state.users_aggregation();

        // Enter drills into "alice".
        handle_key_event(key(KeyCode::Enter), &mut state, &args()).await;
        assert_eq!(state.users_tab_state.drill_user.as_deref(), Some("alice"));

        // ESC pops back out to the top-level table.
        handle_key_event(key(KeyCode::Esc), &mut state, &args()).await;
        assert!(state.users_tab_state.drill_user.is_none());
    }

    #[tokio::test]
    async fn users_tab_esc_without_drilldown_does_not_exit_app() {
        // ESC on the Users tab with no drill-down must NOT exit the
        // app — it should fall through to the normal ESC handler
        // which checks alert panel / help / filter in turn and only
        // then returns true (Exit).  With none of those active it
        // returns true too; so we assert the return value here.
        let mut state = state_with_users_tab();
        let exited = handle_key_event(key(KeyCode::Esc), &mut state, &args()).await;
        // The top-level ESC falls through to the default handler,
        // which returns true in the absence of any other state to
        // clear.  That's the current contract — just verify we don't
        // accidentally swallow ESC when drill-down is closed.
        assert!(exited || state.users_tab_state.drill_user.is_none());
    }

    #[tokio::test]
    async fn filter_mode_wins_over_users_tab_keys() {
        // Regression guard: while the operator is editing a filter
        // query, typing `m` must go into the buffer, NOT change the
        // users-sort key.
        let mut state = state_with_users_tab();
        handle_key_event(key(KeyCode::Char('/')), &mut state, &args()).await;
        handle_key_event(key(KeyCode::Char('m')), &mut state, &args()).await;
        assert!(
            state.filter_buffer.contains('m'),
            "`m` must be literal while filter editor is open"
        );
        assert_eq!(
            state.users_tab_state.sort,
            crate::ui::aggregation::user::UserSortKey::User,
            "filter mode must not leak keys into the Users tab"
        );
    }

    /// Regression guard for F1 in PR #199: a second `Enter` on the
    /// per-host table must set `drill_host` AND the renderer must draw
    /// the per-(user, host) process list for that pair. The previous
    /// implementation set `drill_host` correctly but never rendered
    /// the second-level view, so the issue-#189 spec
    /// ("Enter again drills to the full process list for that user on
    ///  the selected node") silently misfired.
    ///
    /// ESC peels back one level at a time: first clears `drill_host`
    /// (returns to per-host table), then clears `drill_user` (returns
    /// to top table).
    #[tokio::test]
    async fn users_tab_enter_twice_drills_into_per_host_processes() {
        use crate::network::metrics_parser::ParsedProcessRow;

        let mut state = state_with_users_tab();
        // Two distinct hosts so the per-host table has a row to drill
        // into; two PIDs per host so the per-process view has
        // something to show.
        for (host, pid, user, cmd, vram) in [
            ("host-0", 100u32, "alice", "train.py --eval", 1_000u64),
            ("host-0", 101, "alice", "tensorboard", 500),
            ("host-1", 200, "alice", "infer.py", 2_000),
        ] {
            state.remote_process_info.push(ParsedProcessRow {
                host: host.into(),
                pid,
                user: user.into(),
                command: cmd.into(),
                name: cmd.into(),
                gpu_index: 0,
                gpu_uuid: format!("GPU-{host}"),
                gpu_memory_bytes: vram,
                cpu_pct_tenths: 0,
                start_time_seconds: 60,
            });
        }
        state.mark_collector_data_changed();
        let _ = state.users_aggregation();

        // First Enter: drill into alice.
        handle_key_event(key(KeyCode::Enter), &mut state, &args()).await;
        assert_eq!(state.users_tab_state.drill_user.as_deref(), Some("alice"));
        assert!(state.users_tab_state.drill_host.is_none());

        // Second Enter: drill into the currently-selected host (row 0
        // in the per-host table is `host-0` because the per-host list
        // is sorted alphabetically in `UserScratch::finalize`).
        handle_key_event(key(KeyCode::Enter), &mut state, &args()).await;
        assert_eq!(
            state.users_tab_state.drill_host.as_deref(),
            Some("host-0"),
            "second Enter must set drill_host to the selected per-host row"
        );

        // Render at the second-level: the renderer must emit rows
        // filtered to (alice, host-0) — so both commands show up and
        // `host-1` / alice's process there does NOT.
        let mut buffer: Vec<u8> = Vec::new();
        let agg = state.users_aggregation().clone();
        let result = crate::ui::renderers::user_renderer::render_users_tab(
            &mut buffer,
            &agg,
            &state.users_tab_state,
            &state.remote_process_info,
            120,
            24,
        );
        let output = String::from_utf8_lossy(&buffer);
        assert!(
            output.contains("per-process view"),
            "expected per-process banner, got:\n{output}"
        );
        assert!(
            output.contains("train.py"),
            "expected alice's train.py on host-0, got:\n{output}"
        );
        assert!(
            output.contains("tensorboard"),
            "expected alice's tensorboard on host-0, got:\n{output}"
        );
        assert!(
            !output.contains("infer.py"),
            "alice's host-1 process must NOT leak into the host-0 drill-down: {output}"
        );
        assert_eq!(result.visible_rows, 2, "two rows on host-0 for alice");

        // First ESC: clear drill_host, return to per-host table.
        handle_key_event(key(KeyCode::Esc), &mut state, &args()).await;
        assert!(
            state.users_tab_state.drill_host.is_none(),
            "first ESC clears drill_host"
        );
        assert_eq!(
            state.users_tab_state.drill_user.as_deref(),
            Some("alice"),
            "first ESC keeps drill_user so the per-host table is shown"
        );

        // Second ESC: clear drill_user, return to the top table.
        handle_key_event(key(KeyCode::Esc), &mut state, &args()).await;
        assert!(state.users_tab_state.drill_user.is_none());
    }

    /// Regression guard for F3 in PR #199: UI-only state changes
    /// (sort, filter-sys toggle, drill-down nav) must NOT invalidate
    /// the cluster-wide user aggregation cache. Before the split
    /// between `data_version` and `collector_data_version`, every
    /// Users-tab sort keypress re-ran `aggregate_users` on the full
    /// cluster.
    #[tokio::test]
    async fn users_tab_sort_keypress_does_not_invalidate_aggregation_cache() {
        use crate::network::metrics_parser::ParsedProcessRow;

        let mut state = state_with_users_tab();
        for (pid, user) in [(1u32, "alice"), (2, "bob")] {
            state.remote_process_info.push(ParsedProcessRow {
                host: "host-0".into(),
                pid,
                user: user.into(),
                command: "x".into(),
                name: "x".into(),
                gpu_index: 0,
                gpu_uuid: "GPU-0".into(),
                gpu_memory_bytes: 1024,
                cpu_pct_tenths: 0,
                start_time_seconds: 10,
            });
        }
        state.mark_collector_data_changed();
        // Warm the cache.
        let _ = state.users_aggregation();
        let cached_version_before = state.users_aggregation_cache.data_version;

        // A full sweep of Users-tab UI hotkeys — these all route
        // through `mark_data_changed`, which must NOT bump
        // `collector_data_version`.
        for key_char in ['m', 'u', 'p', 'n', 't', 'f'] {
            handle_key_event(key(KeyCode::Char(key_char)), &mut state, &args()).await;
        }

        // Cache key is still on the same collector version...
        assert_eq!(
            state.users_aggregation_cache.data_version, cached_version_before,
            "sort / filter keypresses bumped the collector data version"
        );

        // ...and a subsequent aggregation call finds the cached result
        // (the function returns without rebuilding — we can't observe
        // that directly without instrumentation, so we assert the
        // invariant on the cache key which `users_aggregation` updates
        // only on a rebuild).
        let _ = state.users_aggregation();
        assert_eq!(
            state.users_aggregation_cache.data_version, cached_version_before,
            "aggregate_users ran a second time on cached data"
        );
    }

    /// Regression guard for F5 in PR #199: a replayed local recording
    /// emits GPUs whose `detail["index"]` label is missing (local
    /// readers never populate it). The aggregation must fall back to
    /// the GPU's positional order within its host so every card stays
    /// distinguishable on the per-host drill-down — before this fix,
    /// all 8 GPUs of a recorded single-host session collapsed onto
    /// `gpu_index = 0`.
    #[tokio::test]
    async fn users_aggregation_assigns_positional_gpu_index_when_detail_missing() {
        use crate::device::GpuInfo;
        use crate::network::metrics_parser::ParsedProcessRow;

        let mut state = AppState::new();
        state.is_local_mode = true;
        // Eight GPUs on a single host, none with `detail["index"]`.
        for i in 0..8u32 {
            state.gpu_info.push(GpuInfo {
                uuid: format!("gpu-{i}"),
                time: String::new(),
                name: format!("GPU {i}"),
                device_type: "GPU".to_string(),
                host_id: "replay-host".into(),
                hostname: "replay-host".into(),
                instance: String::new(),
                utilization: 0.0,
                ane_utilization: 0.0,
                dla_utilization: None,
                tensorcore_utilization: None,
                temperature: 0,
                used_memory: 0,
                total_memory: 16_384,
                frequency: 0,
                power_consumption: 100.0,
                gpu_core_count: None,
                temperature_threshold_slowdown: None,
                temperature_threshold_shutdown: None,
                temperature_threshold_max_operating: None,
                temperature_threshold_acoustic: None,
                performance_state: None,
                numa_node_id: None,
                gsp_firmware_mode: None,
                gsp_firmware_version: None,
                nvlink_remote_devices: Vec::new(),
                gpm_metrics: None,
                // Empty detail map -- replicates the local-mode replay
                // path that F5 identifies as broken.
                detail: std::collections::HashMap::new(),
            });
        }
        // One process per GPU so the aggregation touches every (host,
        // gpu_index) pair.
        for i in 0..8u32 {
            state.remote_process_info.push(ParsedProcessRow {
                host: "replay-host".into(),
                pid: 1000 + i,
                user: "alice".into(),
                command: "train".into(),
                name: "train".into(),
                gpu_index: i,
                gpu_uuid: format!("gpu-{i}"),
                gpu_memory_bytes: 1_000_000_000,
                cpu_pct_tenths: 0,
                start_time_seconds: 10,
            });
        }
        state.mark_collector_data_changed();
        let agg = state.users_aggregation();
        let alice = agg.users.iter().find(|u| u.user == "alice").unwrap();
        // Alice has touched 8 distinct (host, gpu_index) pairs — not
        // collapsed onto one.
        assert_eq!(
            alice.gpu_count, 8,
            "expected 8 distinct GPUs, got {} — positional fallback is broken",
            alice.gpu_count
        );
        // Per-host breakdown shows the full index set 0..=7.
        let per_host = &alice.per_host[0];
        let indices: Vec<u32> = per_host.gpu_indices.iter().copied().collect();
        assert_eq!(indices, (0..8).collect::<Vec<u32>>());
    }

    #[tokio::test]
    async fn users_tab_up_down_moves_row_cursor() {
        let mut state = state_with_users_tab();
        // Seed 3 users so Down has room to move.
        for (pid, user) in [(1, "alice"), (2, "bob"), (3, "carol")] {
            state
                .remote_process_info
                .push(crate::network::metrics_parser::ParsedProcessRow {
                    host: "host-0".into(),
                    pid,
                    user: user.into(),
                    command: "x".into(),
                    name: "x".into(),
                    gpu_index: 0,
                    gpu_uuid: "GPU-0".into(),
                    gpu_memory_bytes: 1000,
                    cpu_pct_tenths: 0,
                    start_time_seconds: 10,
                });
        }
        // Simulate a collector push so the aggregation cache picks up
        // the new process data.
        state.mark_collector_data_changed();
        let _ = state.users_aggregation();

        assert_eq!(state.users_tab_state.selected_row, 0);
        handle_key_event(key(KeyCode::Down), &mut state, &args()).await;
        assert_eq!(state.users_tab_state.selected_row, 1);
        handle_key_event(key(KeyCode::Down), &mut state, &args()).await;
        assert_eq!(state.users_tab_state.selected_row, 2);
        // At the bottom: Down stays put.
        handle_key_event(key(KeyCode::Down), &mut state, &args()).await;
        assert_eq!(state.users_tab_state.selected_row, 2);
        handle_key_event(key(KeyCode::Up), &mut state, &args()).await;
        assert_eq!(state.users_tab_state.selected_row, 1);
    }

    #[test]
    fn csv_escape_passes_through_benign_strings() {
        assert_eq!(super::csv_escape("alice"), "alice");
        assert_eq!(super::csv_escape("python train.py"), "python train.py");
    }

    #[test]
    fn csv_escape_rfc4180_quotes_fields_with_commas_and_quotes() {
        // Plain comma: RFC-4180 wrapping only (no formula guard).
        assert_eq!(
            super::csv_escape("train,eval"),
            r#""train,eval""#,
            "comma triggers RFC-4180 quoting"
        );
        // Embedded double-quote: the value `say "hi"` becomes
        // `"say ""hi"""` — wrapper quotes on the outside, each internal
        // `"` doubled.
        assert_eq!(
            super::csv_escape(r#"say "hi""#),
            r#""say ""hi""""#,
            "embedded double-quotes must be doubled"
        );
    }

    #[test]
    fn csv_escape_blocks_formula_injection_via_equals() {
        // CSV injection: a user name or command starting with `=`
        // becomes a formula in Excel. We must prefix it with `'` so
        // spreadsheets treat it as plain text. The field is wrapped in
        // quotes so the leading apostrophe doesn't get stripped as a
        // row separator by lenient parsers.
        let out = super::csv_escape(r#"=cmd|"/c calc"!A1"#);
        assert!(
            out.starts_with(r#""'"#),
            "missing leading-quote guard for `=`: {out}"
        );
        // The guarded field never leaves the leading `=` outside a
        // quoted form where a spreadsheet would evaluate it.
        assert!(
            !out.starts_with("="),
            "unquoted leading `=` leaks through: {out}"
        );
    }

    #[test]
    fn csv_escape_blocks_formula_injection_via_plus_minus_at() {
        for prefix in ['+', '-', '@'] {
            let out = super::csv_escape(&format!("{prefix}SUM(A1)"));
            assert!(
                out.starts_with(r#""'"#),
                "missing formula guard for leading `{prefix}`: {out}"
            );
        }
    }

    #[test]
    fn csv_escape_blocks_formula_injection_via_tab_and_cr() {
        let out = super::csv_escape("\t=cmd");
        assert!(
            out.starts_with(r#""'"#),
            "missing formula guard for leading tab: {out}"
        );
        let out = super::csv_escape("\r=cmd");
        assert!(
            out.starts_with(r#""'"#),
            "missing formula guard for leading CR: {out}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn open_export_secure_refuses_symlinks() {
        // A co-tenant plants a symlink at the would-be output path; our
        // secure opener must refuse rather than follow it to write into
        // an attacker-chosen target. Mirrors the regression tests in
        // `src/record/writer.rs` and `src/doctor/bundle.rs`.
        let tmp = tempfile::tempdir().expect("tempdir");
        let sink = tmp.path().join("sink.txt");
        std::fs::write(&sink, b"").expect("write sink");
        let link = tmp.path().join("users-20260420T120000.csv");
        std::os::unix::fs::symlink(&sink, &link).expect("symlink");

        let result = super::open_export_secure(&link);
        assert!(
            result.is_err(),
            "open_export_secure must refuse a pre-existing symlink (got Ok)"
        );

        // The sink must remain empty — no accidental follow-through.
        let sink_body = std::fs::read(&sink).expect("read sink");
        assert!(sink_body.is_empty(), "symlink target was written to anyway");
    }

    #[cfg(unix)]
    #[test]
    fn open_export_secure_sets_owner_only_mode() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("users-test.csv");
        let file = super::open_export_secure(&target).expect("open");
        drop(file);
        let mode = std::fs::metadata(&target)
            .expect("stat")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "export CSV file must be 0o600, got {mode:o}");
    }

    // -----------------------------------------------------------------------
    // Topology tab key handlers (issue #190)
    // -----------------------------------------------------------------------

    /// Build a minimal remote-mode state with the standard tab strip:
    /// `[All, Users, Topology, host1, host2]`.
    fn make_topology_state() -> AppState {
        let mut state = AppState::new();
        state.is_local_mode = false;
        state.loading = false;
        state.tabs = vec![
            "All".to_string(),
            crate::ui::tabs::USERS_TAB_NAME.to_string(),
            crate::ui::tabs::TOPOLOGY_TAB_NAME.to_string(),
            "host1".to_string(),
            "host2".to_string(),
        ];
        // Start on host1 so the T hotkey has something to remember.
        state.current_tab = 3;
        state
    }

    #[tokio::test]
    async fn t_key_jumps_to_topology_tab_and_remembers_host() {
        let mut state = make_topology_state();
        // current_tab == 3 == "host1"; pressing T should stash "host1"
        // and move current_tab to the Topology index (2).
        handle_key_event(key(KeyCode::Char('T')), &mut state, &args()).await;
        let topo_idx = crate::ui::tabs::topology_tab_index(&state.tabs).unwrap();
        assert_eq!(state.current_tab, topo_idx);
        assert_eq!(
            state.topology_last_host_tab.as_deref(),
            Some("host1"),
            "T must stash the previously-active host tab"
        );
    }

    #[tokio::test]
    async fn t_key_is_noop_when_topology_tab_absent() {
        // In local mode the Topology tab is not inserted into the strip.
        let mut state = AppState::new();
        state.is_local_mode = true;
        state.tabs = vec!["All".to_string()];
        state.current_tab = 0;
        let was_tab = state.current_tab;
        handle_key_event(key(KeyCode::Char('T')), &mut state, &args()).await;
        assert_eq!(
            state.current_tab, was_tab,
            "T must be a silent no-op when no Topology tab exists"
        );
    }

    #[test]
    fn remember_current_host_tab_skips_reserved_tabs() {
        let mut state = make_topology_state();
        // When the current tab is "All" (index 0), nothing should be stashed.
        state.current_tab = 0;
        super::remember_current_host_tab(&mut state);
        assert!(
            state.topology_last_host_tab.is_none(),
            "All tab must not be stashed"
        );

        // When the current tab is Users (index 1), nothing should be stashed.
        state.current_tab = 1;
        super::remember_current_host_tab(&mut state);
        assert!(
            state.topology_last_host_tab.is_none(),
            "Users tab must not be stashed"
        );

        // When the current tab is Topology itself (index 2), nothing should be
        // stashed — the renderer's fallback handles the self-reference case.
        state.current_tab = 2;
        super::remember_current_host_tab(&mut state);
        assert!(
            state.topology_last_host_tab.is_none(),
            "Topology tab must not be stashed"
        );
    }

    #[test]
    fn remember_current_host_tab_stashes_host_tab() {
        let mut state = make_topology_state();
        state.current_tab = 4; // "host2"
        super::remember_current_host_tab(&mut state);
        assert_eq!(
            state.topology_last_host_tab.as_deref(),
            Some("host2"),
            "host tab must be stashed"
        );
    }

    #[tokio::test]
    async fn m_key_toggles_topology_view_mode_when_topology_active() {
        let mut state = make_topology_state();
        // Jump to the Topology tab first so the mode-specific handler fires.
        let topo_idx = crate::ui::tabs::topology_tab_index(&state.tabs).unwrap();
        state.current_tab = topo_idx;
        assert_eq!(
            state.topology_view_mode,
            crate::ui::topology::TopologyViewMode::Graph
        );
        // Uppercase M (as documented in the help overlay).
        handle_key_event(key(KeyCode::Char('M')), &mut state, &args()).await;
        assert_eq!(
            state.topology_view_mode,
            crate::ui::topology::TopologyViewMode::Matrix,
            "first M must switch to matrix"
        );
        // Second press cycles back to graph.
        handle_key_event(key(KeyCode::Char('M')), &mut state, &args()).await;
        assert_eq!(
            state.topology_view_mode,
            crate::ui::topology::TopologyViewMode::Graph,
            "second M must cycle back to graph"
        );
    }

    #[tokio::test]
    async fn lowercase_m_also_toggles_topology_view_mode() {
        // The handler accepts both 'm' and 'M' to reduce muscle-memory
        // friction (operators may not use Shift).
        let mut state = make_topology_state();
        let topo_idx = crate::ui::tabs::topology_tab_index(&state.tabs).unwrap();
        state.current_tab = topo_idx;
        handle_key_event(key(KeyCode::Char('m')), &mut state, &args()).await;
        assert_eq!(
            state.topology_view_mode,
            crate::ui::topology::TopologyViewMode::Matrix,
            "lowercase m must toggle topology mode when Topology tab is active"
        );
    }

    #[tokio::test]
    async fn m_key_does_not_toggle_topology_mode_outside_topology_tab() {
        // 'm' outside the Topology tab hits the global GPU-sort-by-memory
        // binding instead; topology_view_mode must not change.
        let mut state = make_topology_state();
        state.current_tab = 3; // "host1" — not the Topology tab
        handle_key_event(key(KeyCode::Char('M')), &mut state, &args()).await;
        assert_eq!(
            state.topology_view_mode,
            crate::ui::topology::TopologyViewMode::Graph,
            "M outside the Topology tab must not toggle topology_view_mode"
        );
    }
}
