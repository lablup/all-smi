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

//! Frame content assembly operating on a `RenderSnapshot`.
//!
//! All methods here are pure functions: they read the snapshot and produce
//! a `String` (or write into a `BufferWriter`) without touching shared state.
//! This means the `AppState` mutex is not held during any of this work.

use std::borrow::Cow;
use std::collections::HashMap;
use std::io::Write;

use chrono::Local;
use crossterm::{
    queue,
    style::{Color, Print},
};

use crate::app_state::AppState;
use crate::cli::ViewArgs;
use crate::device::ProcessInfo;
use crate::ui::activity_panel;
use crate::ui::buffer::BufferWriter;
use crate::ui::dashboard::{draw_dashboard_items, draw_system_view};
use crate::ui::gpu_sparkline_panel;
use crate::ui::layout::LayoutCalculator;
use crate::ui::local_header::draw_local_header_bar;
use crate::ui::renderer::{
    print_chassis_info, print_cpu_info, print_function_keys, print_gpu_info,
    print_loading_indicator, print_memory_info, print_mig_section, print_process_info,
    print_storage_info, print_vgpu_section,
};
use crate::ui::tabs::draw_tabs;
use crate::ui::text::print_colored_text;
use crate::view::render_snapshot::RenderSnapshot;
use crate::view::view_cache::ViewCache;

/// Stateless frame renderer that operates on a `RenderSnapshot`.
///
/// This struct holds no mutable state of its own. Each method takes an
/// immutable snapshot and returns the assembled frame content as a `String`.
pub struct FrameRenderer;

impl FrameRenderer {
    /// Render help popup content from the snapshot.
    pub fn render_help(snapshot: &RenderSnapshot, args: &ViewArgs, cols: u16, rows: u16) -> String {
        let is_remote = args.hosts.is_some() || args.hostfile.is_some();
        let view_state = snapshot.as_app_state();
        crate::ui::help::generate_help_popup_content(cols, rows, &view_state, is_remote)
    }

    /// Render loading screen content from the snapshot.
    pub fn render_loading(
        snapshot: &RenderSnapshot,
        is_remote: bool,
        cols: u16,
        rows: u16,
    ) -> String {
        let mut buffer = BufferWriter::new();
        let view_state = snapshot.as_app_state();
        print_function_keys(&mut buffer, cols, rows, &view_state, is_remote);
        print_loading_indicator(
            &mut buffer,
            cols,
            rows,
            snapshot.frame_counter,
            &snapshot.startup_status_lines,
        );
        buffer.get_buffer().to_string()
    }

    /// Render main content (the primary monitoring view) from the snapshot.
    ///
    /// When a `ViewCache` is provided, pre-computed sorted/filtered indices
    /// are used instead of re-sorting and re-filtering on every frame.
    /// Render the main TUI view.
    ///
    /// Returns `(content, visible_process_rows)` where `visible_process_rows`
    /// is the actual number of process rows that fit on screen. The caller
    /// should store this value so the event handler can scroll correctly.
    pub fn render_main(
        snapshot: &RenderSnapshot,
        args: &ViewArgs,
        cols: u16,
        rows: u16,
        cache: Option<&ViewCache>,
    ) -> (String, usize) {
        let width = cols as usize;
        let mut buffer = BufferWriter::new();

        let view_state = snapshot.as_app_state();

        // Write time/date header to buffer first
        let current_time = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
        let version = env!("CARGO_PKG_VERSION");
        let header_text = format!("all-smi - {current_time}");
        let version_text = format!("v{version}");

        // Get runtime environment info
        let runtime_shield =
            if let Some((name, color)) = snapshot.runtime_environment.display_info() {
                let shield_content = format!(" {name} ");
                let shield_len = shield_content.len();
                Some((shield_content, color, shield_len))
            } else {
                None
            };

        // Calculate spacing to right-align version, accounting for runtime shield
        let total_width = cols as usize;
        let runtime_shield_len = runtime_shield
            .as_ref()
            .map(|(_, _, len)| len + 1)
            .unwrap_or(0);
        let content_length = header_text.len() + runtime_shield_len + version_text.len();
        let spacing = if total_width > content_length {
            " ".repeat(total_width - content_length)
        } else {
            " ".to_string()
        };

        // Print header with runtime environment shield
        print_colored_text(&mut buffer, &header_text, Color::White, None, None);

        if let Some((shield_content, shield_color, _)) = runtime_shield {
            print_colored_text(&mut buffer, " ", Color::White, None, None);
            print_colored_text(
                &mut buffer,
                &shield_content,
                Color::Black,
                Some(shield_color),
                None,
            );
        }

        print_colored_text(
            &mut buffer,
            &format!("{spacing}{version_text}\r\n"),
            Color::White,
            None,
            None,
        );

        let is_remote = args.hosts.is_some() || args.hostfile.is_some();

        // Cluster Overview, dashboard items, and the tabs row are only meaningful
        // when monitoring multiple remote hosts. `is_local_mode` is false the moment
        // any --hosts / --hostfile argument is supplied (see the assignment sites in
        // `src/view/runner.rs::run_view_mode` / `run_local_mode`), so a single remote
        // host still shows these widgets.
        //
        // In local mode we show the compact two-line host summary bar instead.
        if view_state.is_local_mode {
            draw_local_header_bar(&mut buffer, &view_state, cols);

            // Activity panel: CPU per-core bars (left) + GPU sparklines (right)
            if activity_panel::should_show_panel(cols) {
                gpu_sparkline_panel::render_combined_activity_panel(
                    &mut buffer,
                    &view_state,
                    &snapshot.cpu_info,
                    width,
                );
            }
        } else {
            // Write remaining header content to buffer
            print_colored_text(&mut buffer, "Cluster Overview\r\n", Color::Cyan, None, None);
            draw_system_view(&mut buffer, &view_state, cols);

            draw_dashboard_items(&mut buffer, &view_state, cols);
            draw_tabs(&mut buffer, &view_state, cols);
        }

        // Render chassis information (node-level metrics)
        Self::render_chassis_section(&mut buffer, snapshot, width, cache);

        // Render GPU information (reuse the single view_state for layout calculation)
        Self::render_gpu_section(&mut buffer, snapshot, &view_state, args, cols, rows, cache);

        // Render other device information based on mode
        let visible_process_rows = if is_remote {
            Self::render_remote_devices(&mut buffer, snapshot, width, cache);
            0
        } else {
            Self::render_local_devices(&mut buffer, snapshot, cols, rows, cache)
        };

        // Add function keys to main content view
        print_function_keys(&mut buffer, cols, rows, &view_state, is_remote);

        (buffer.get_buffer().to_string(), visible_process_rows)
    }

    fn render_gpu_section(
        buffer: &mut BufferWriter,
        snapshot: &RenderSnapshot,
        view_state: &AppState,
        args: &ViewArgs,
        cols: u16,
        rows: u16,
        cache: Option<&ViewCache>,
    ) {
        // Use cached sorted indices when available, otherwise fall back to
        // the previous per-frame filter + sort path.
        let cached_indices;
        let fallback_indices;
        let display_indices: &[usize] = if let Some(indices) = cache.and_then(|c| c.gpu_indices()) {
            cached_indices = indices;
            cached_indices
        } else {
            // Fallback: filter + sort inline (only reached when cache is None).
            // Use .get() to guard against out-of-bounds current_tab.
            let mut indices: Vec<usize> =
                if let Some(tab_name) = snapshot.tabs.get(snapshot.current_tab) {
                    if tab_name == "All" {
                        (0..snapshot.gpu_info.len()).collect()
                    } else {
                        snapshot
                            .gpu_info
                            .iter()
                            .enumerate()
                            .filter(|(_, info)| info.host_id == *tab_name)
                            .map(|(i, _)| i)
                            .collect()
                    }
                } else {
                    // Out-of-bounds tab index: show all (defensive)
                    (0..snapshot.gpu_info.len()).collect()
                };
            indices.sort_by(|&a, &b| {
                snapshot
                    .sort_criteria
                    .sort_gpus(&snapshot.gpu_info[a], &snapshot.gpu_info[b])
            });
            fallback_indices = indices;
            &fallback_indices
        };

        // Calculate content area and GPU display parameters using the shared
        // view_state from render_main, avoiding a second as_app_state() call.
        let content_area = LayoutCalculator::calculate_content_area(view_state, cols, rows);
        let gpu_display_params =
            LayoutCalculator::calculate_gpu_display_params(view_state, args, &content_area);
        let max_gpu_items = gpu_display_params.max_items;

        // Display GPUs with scrolling
        let start_gpu_index = snapshot.gpu_scroll_offset;
        let end_gpu_index = (start_gpu_index + max_gpu_items).min(display_indices.len());

        // Build O(1) lookup maps for vGPU and MIG matching, replacing the
        // previous per-GPU linear scans that were O(G*V) + O(G*M) per frame.
        let vgpu_lookup = build_vgpu_lookup(&snapshot.vgpu_info);
        let mig_lookup = build_mig_lookup(&snapshot.mig_info);

        for (i, &gpu_idx) in display_indices
            .iter()
            .enumerate()
            .skip(start_gpu_index)
            .take(end_gpu_index.saturating_sub(start_gpu_index))
        {
            let gpu_info = &snapshot.gpu_info[gpu_idx];
            let device_name_scroll_offset = snapshot
                .device_name_scroll_offsets
                .get(&gpu_info.uuid)
                .copied()
                .unwrap_or(0);
            let hostname_scroll_offset = snapshot
                .host_id_scroll_offsets
                .get(&gpu_info.host_id)
                .copied()
                .unwrap_or(0);

            print_gpu_info(
                buffer,
                i,
                gpu_info,
                cols as usize,
                device_name_scroll_offset,
                hostname_scroll_offset,
                !view_state.is_local_mode,
            );

            // If this GPU is vGPU-enabled, render the nested section directly
            // beneath the GPU row. O(1) UUID lookup with hostname+gpu-name
            // fallback for remote-mode data without UUID.
            if let Some(vgpu_host) =
                lookup_vgpu_host(&vgpu_lookup, &snapshot.vgpu_info, gpu_info)
            {
                print_vgpu_section(buffer, vgpu_host, cols as usize);
            }

            // If this GPU has MIG mode enabled, render the nested MIG section
            // directly beneath the GPU row using the same UUID-first matching
            // strategy as the vGPU section above.
            if let Some(mig_host) = lookup_mig_gpu(&mig_lookup, &snapshot.mig_info, gpu_info) {
                print_mig_section(buffer, mig_host, cols as usize);
            }
        }
    }

    fn render_chassis_section(
        buffer: &mut BufferWriter,
        snapshot: &RenderSnapshot,
        width: usize,
        cache: Option<&ViewCache>,
    ) {
        if snapshot.chassis_info.is_empty() {
            return;
        }

        // Use cached chassis indices when available
        if let Some(hd) = cache.and_then(|c| c.host_device_indices()) {
            if hd.chassis_indices.is_empty() {
                return;
            }
            for (i, &idx) in hd.chassis_indices.iter().enumerate() {
                let chassis = &snapshot.chassis_info[idx];
                let hostname_scroll_offset = snapshot
                    .host_id_scroll_offsets
                    .get(&chassis.host_id)
                    .copied()
                    .unwrap_or(0);
                print_chassis_info(buffer, i, chassis, width, hostname_scroll_offset);
            }
            return;
        }

        // Fallback: filter inline (only reached when cache is None)
        let chassis_to_display: Vec<_> = if snapshot.is_local_mode {
            snapshot.chassis_info.iter().collect()
        } else if snapshot.current_tab == 0 {
            return;
        } else if snapshot.current_tab < snapshot.tabs.len() {
            let current_host = &snapshot.tabs[snapshot.current_tab];
            snapshot
                .chassis_info
                .iter()
                .filter(|c| c.host_id == *current_host || c.hostname == *current_host)
                .collect()
        } else {
            snapshot.chassis_info.iter().collect()
        };

        for (i, chassis) in chassis_to_display.iter().enumerate() {
            let hostname_scroll_offset = snapshot
                .host_id_scroll_offsets
                .get(&chassis.host_id)
                .copied()
                .unwrap_or(0);
            print_chassis_info(buffer, i, chassis, width, hostname_scroll_offset);
        }
    }

    fn render_remote_devices(
        buffer: &mut BufferWriter,
        snapshot: &RenderSnapshot,
        width: usize,
        cache: Option<&ViewCache>,
    ) {
        if snapshot.current_tab == 0 || snapshot.current_tab >= snapshot.tabs.len() {
            return;
        }

        let current_hostname = &snapshot.tabs[snapshot.current_tab];

        // Check connection status for the current node
        let is_connected = if let Some(host_id) = snapshot.hostname_to_host_id.get(current_hostname)
        {
            snapshot
                .connection_status
                .get(host_id)
                .map(|status| status.is_connected)
                .unwrap_or(false)
        } else {
            snapshot
                .connection_status
                .get(current_hostname)
                .map(|status| status.is_connected)
                .unwrap_or(true)
        };

        if !is_connected {
            Self::render_disconnection_notification(buffer, current_hostname, width);
            return;
        }

        // Resolve host-device indices: use cache when available, otherwise
        // build a temporary index list from an inline filter.
        let fallback_cpu;
        let fallback_mem;
        let fallback_stor;
        let (cpu_idx, mem_idx, stor_idx) = if let Some(hd) =
            cache.and_then(|c| c.host_device_indices())
        {
            (
                hd.cpu_indices.as_slice(),
                hd.memory_indices.as_slice(),
                hd.storage_indices.as_slice(),
            )
        } else {
            fallback_cpu =
                Self::filter_indices(&snapshot.cpu_info, |c| c.host_id == *current_hostname);
            fallback_mem =
                Self::filter_indices(&snapshot.memory_info, |m| m.host_id == *current_hostname);
            fallback_stor =
                Self::filter_indices(&snapshot.storage_info, |s| s.host_id == *current_hostname);
            (
                fallback_cpu.as_slice(),
                fallback_mem.as_slice(),
                fallback_stor.as_slice(),
            )
        };

        // CPU
        for (i, &idx) in cpu_idx.iter().enumerate() {
            let cpu_info = &snapshot.cpu_info[idx];
            let cpu_name_scroll_offset = snapshot
                .cpu_name_scroll_offsets
                .get(&format!("{}-{}", cpu_info.hostname, cpu_info.cpu_model))
                .copied()
                .unwrap_or(0);
            let hostname_scroll_offset = snapshot
                .host_id_scroll_offsets
                .get(&cpu_info.host_id)
                .copied()
                .unwrap_or(0);
            print_cpu_info(
                buffer,
                i,
                cpu_info,
                width,
                false,
                cpu_name_scroll_offset,
                hostname_scroll_offset,
                true,
            );
        }

        // Memory
        for (i, &idx) in mem_idx.iter().enumerate() {
            let memory_info = &snapshot.memory_info[idx];
            let hostname_scroll_offset = snapshot
                .host_id_scroll_offsets
                .get(&memory_info.host_id)
                .copied()
                .unwrap_or(0);
            print_memory_info(buffer, i, memory_info, width, hostname_scroll_offset, true);
        }

        // Storage with scroll offset
        for (i, &idx) in stor_idx
            .iter()
            .skip(snapshot.storage_scroll_offset)
            .take(10)
            .enumerate()
        {
            let storage_info = &snapshot.storage_info[idx];
            let hostname_scroll_offset = snapshot
                .host_id_scroll_offsets
                .get(&storage_info.host_id)
                .copied()
                .unwrap_or(0);
            print_storage_info(buffer, i, storage_info, width, hostname_scroll_offset, true);
        }
    }

    /// Collect indices of elements matching a predicate.
    fn filter_indices<T>(items: &[T], predicate: impl Fn(&T) -> bool) -> Vec<usize> {
        items
            .iter()
            .enumerate()
            .filter(|(_, item)| predicate(item))
            .map(|(i, _)| i)
            .collect()
    }

    fn render_disconnection_notification(buffer: &mut BufferWriter, hostname: &str, width: usize) {
        writeln!(buffer).unwrap();
        writeln!(buffer).unwrap();

        let box_width = width.saturating_sub(4).min(60);
        // Ensure minimum box width for the border characters
        if box_width < 6 {
            return;
        }
        let margin = width.saturating_sub(box_width) / 2;
        let margin_str = " ".repeat(margin);

        // Top border
        write!(buffer, "{margin_str}").unwrap();
        print_colored_text(buffer, "\u{250c}", Color::Red, None, None);
        print_colored_text(
            buffer,
            &"\u{2500}".repeat(box_width.saturating_sub(2)),
            Color::Red,
            None,
            None,
        );
        print_colored_text(buffer, "\u{2510}", Color::Red, None, None);
        writeln!(buffer).unwrap();

        // Content rows: title, blank, hostname, status, blank
        let rows: &[(&str, Color)] = &[
            ("CONNECTION LOST", Color::Red),
            ("", Color::White),
            (&format!("Node: {hostname}"), Color::Yellow),
            ("Unable to retrieve node information", Color::DarkGrey),
            ("", Color::White),
        ];
        // Inner width available for text content (between "| " and " |")
        let inner_width = box_width.saturating_sub(4);
        for (text, color) in rows {
            write!(buffer, "{margin_str}").unwrap();
            if text.is_empty() {
                // Empty row
                print_colored_text(buffer, "\u{2502}", Color::Red, None, None);
                print_colored_text(
                    buffer,
                    &" ".repeat(box_width.saturating_sub(2)),
                    Color::White,
                    None,
                    None,
                );
                print_colored_text(buffer, "\u{2502}", Color::Red, None, None);
            } else {
                // Truncate text if it exceeds available inner width
                let display_text: Cow<'_, str> = if text.len() > inner_width {
                    Cow::Owned(text.chars().take(inner_width).collect())
                } else {
                    Cow::Borrowed(text)
                };
                let pad_left = inner_width.saturating_sub(display_text.len()) / 2;
                let pad_right = inner_width.saturating_sub(pad_left + display_text.len());
                print_colored_text(buffer, "\u{2502} ", Color::Red, None, None);
                print_colored_text(buffer, &" ".repeat(pad_left), Color::White, None, None);
                print_colored_text(buffer, &display_text, *color, None, None);
                print_colored_text(buffer, &" ".repeat(pad_right), Color::White, None, None);
                print_colored_text(buffer, " \u{2502}", Color::Red, None, None);
            }
            writeln!(buffer).unwrap();
        }

        // Bottom border
        write!(buffer, "{margin_str}").unwrap();
        print_colored_text(buffer, "\u{2514}", Color::Red, None, None);
        print_colored_text(
            buffer,
            &"\u{2500}".repeat(box_width.saturating_sub(2)),
            Color::Red,
            None,
            None,
        );
        print_colored_text(buffer, "\u{2518}", Color::Red, None, None);
        writeln!(buffer).unwrap();
    }

    /// Returns the number of visible process rows for event handler scroll calculation.
    fn render_local_devices(
        buffer: &mut BufferWriter,
        snapshot: &RenderSnapshot,
        cols: u16,
        rows: u16,
        cache: Option<&ViewCache>,
    ) -> usize {
        let width = cols as usize;

        // CPU information for local mode
        // Per-core bars are now always shown in the Activity panel above,
        // so we pass show_per_core=false here to avoid duplication.
        for (i, cpu_info) in snapshot.cpu_info.iter().enumerate() {
            let cpu_name_scroll_offset = snapshot
                .cpu_name_scroll_offsets
                .get(&format!("{}-{}", cpu_info.hostname, cpu_info.cpu_model))
                .copied()
                .unwrap_or(0);
            let hostname_scroll_offset = snapshot
                .host_id_scroll_offsets
                .get(&cpu_info.host_id)
                .copied()
                .unwrap_or(0);
            print_cpu_info(
                buffer,
                i,
                cpu_info,
                width,
                false,
                cpu_name_scroll_offset,
                hostname_scroll_offset,
                false,
            );
        }

        // Memory information for local mode
        for (i, memory_info) in snapshot.memory_info.iter().enumerate() {
            let hostname_scroll_offset = snapshot
                .host_id_scroll_offsets
                .get(&memory_info.host_id)
                .copied()
                .unwrap_or(0);
            print_memory_info(buffer, i, memory_info, width, hostname_scroll_offset, false);
        }

        // Storage information for local mode
        for (i, storage_info) in snapshot.storage_info.iter().enumerate() {
            let hostname_scroll_offset = snapshot
                .host_id_scroll_offsets
                .get(&storage_info.host_id)
                .copied()
                .unwrap_or(0);
            print_storage_info(
                buffer,
                i,
                storage_info,
                width,
                hostname_scroll_offset,
                false,
            );
        }

        // Process information for local mode (if available)
        if !snapshot.process_info.is_empty() {
            let lines_used = buffer.line_count();

            // Add a blank line before process list
            queue!(buffer, Print("\r\n")).unwrap();

            // Reserve 1 line for function keys at the bottom
            let function_key_rows = 1;

            let available_rows = rows.saturating_sub(lines_used as u16 + 1 + function_key_rows);

            // Calculate actual visible process rows (must match process_renderer logic)
            // RESERVED_HEADER_ROWS = 4 ("Processes:" title, column header, separator, blank)
            // footer_rows = 2 ("Showing..." + "Active..." stats)
            let visible = (available_rows as usize).saturating_sub(4 + 2);

            // Get current user for process coloring
            let current_user = whoami::username().unwrap_or_default();

            // Use cached GPU-filtered process list when available, avoiding
            // a per-frame clone of the entire process vector.
            let processes_to_display: Cow<'_, [ProcessInfo]> =
                if let Some(pl) = cache.and_then(|c| c.process_display_list()) {
                    match &pl.filtered {
                        Some(filtered) => Cow::Borrowed(filtered.as_slice()),
                        None => Cow::Borrowed(&snapshot.process_info),
                    }
                } else if snapshot.gpu_filter_enabled {
                    // Fallback: filter inline (only when cache is None)
                    Cow::Owned(
                        snapshot
                            .process_info
                            .iter()
                            .filter(|p| p.used_memory > 0)
                            .cloned()
                            .collect(),
                    )
                } else {
                    Cow::Borrowed(&snapshot.process_info)
                };

            print_process_info(
                buffer,
                &processes_to_display,
                snapshot.selected_process_index,
                snapshot.start_index,
                available_rows,
                cols,
                snapshot.process_horizontal_scroll_offset,
                &current_user,
                &snapshot.sort_criteria,
                &snapshot.sort_direction,
            );

            return visible;
        }
        0
    }
}

/// Locate the [`crate::device::VgpuHostInfo`] record matching a given GPU row.
///
/// Build an O(1) lookup map from `gpu_uuid` to index in the vGPU info slice.
/// Called once per frame to replace per-GPU linear scans.
fn build_vgpu_lookup<'a>(vgpu_info: &'a [crate::device::VgpuHostInfo]) -> HashMap<&'a str, usize> {
    let mut map = HashMap::with_capacity(vgpu_info.len());
    for (i, host) in vgpu_info.iter().enumerate() {
        map.entry(host.gpu_uuid.as_str()).or_insert(i);
    }
    map
}

/// Build an O(1) lookup map from `gpu_uuid` to index in the MIG info slice.
/// Called once per frame to replace per-GPU linear scans.
fn build_mig_lookup<'a>(mig_info: &'a [crate::device::MigGpuInfo]) -> HashMap<&'a str, usize> {
    let mut map = HashMap::with_capacity(mig_info.len());
    for (i, host) in mig_info.iter().enumerate() {
        map.entry(host.gpu_uuid.as_str()).or_insert(i);
    }
    map
}

/// O(1) vGPU host lookup by UUID with hostname+gpu_name fallback.
///
/// Matching precedence:
/// 1. Exact `gpu_uuid` match via HashMap (authoritative, O(1)).
/// 2. Fallback: same `hostname` + matching `gpu_name` — used when UUID
///    propagation is missing (e.g. remote mode with incomplete metrics).
///    This path is a rare linear scan only hit for entries not found by UUID.
///
/// Returns `None` when no match is found, which keeps the vGPU section from
/// appearing under unrelated GPU rows.
fn lookup_vgpu_host<'a>(
    lookup: &HashMap<&str, usize>,
    vgpu_info: &'a [crate::device::VgpuHostInfo],
    gpu: &crate::device::GpuInfo,
) -> Option<&'a crate::device::VgpuHostInfo> {
    if let Some(&idx) = lookup.get(gpu.uuid.as_str()) {
        return Some(&vgpu_info[idx]);
    }
    // Fallback: hostname + gpu_name linear scan for entries without UUID match.
    vgpu_info
        .iter()
        .find(|v| v.hostname == gpu.hostname && v.gpu_name == gpu.name)
}

/// O(1) MIG GPU lookup by UUID with hostname+gpu_name fallback.
///
/// Same precedence as [`lookup_vgpu_host`]:
/// 1. Exact `gpu_uuid` match via HashMap (authoritative, O(1)).
/// 2. Fallback: same `hostname` + matching `gpu_name` — used when UUID
///    propagation is missing (e.g. remote mode with incomplete metrics).
///
/// Returns `None` when no match is found, keeping the MIG section from
/// appearing under unrelated GPU rows.
fn lookup_mig_gpu<'a>(
    lookup: &HashMap<&str, usize>,
    mig_info: &'a [crate::device::MigGpuInfo],
    gpu: &crate::device::GpuInfo,
) -> Option<&'a crate::device::MigGpuInfo> {
    if let Some(&idx) = lookup.get(gpu.uuid.as_str()) {
        return Some(&mig_info[idx]);
    }
    // Fallback: hostname + gpu_name linear scan for entries without UUID match.
    mig_info
        .iter()
        .find(|m| m.hostname == gpu.hostname && m.gpu_name == gpu.name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_state::AppState;
    use crate::view::render_snapshot::RenderSnapshot;

    fn make_local_args() -> ViewArgs {
        ViewArgs {
            hosts: None,
            hostfile: None,
            interval: None,
        }
    }

    fn make_snapshot() -> RenderSnapshot {
        let state = AppState::new();
        RenderSnapshot::capture(&state)
    }

    // -----------------------------------------------------------------------
    // FrameRenderer: construction
    // -----------------------------------------------------------------------

    #[test]
    fn test_frame_renderer_is_zero_sized() {
        // FrameRenderer is a unit struct; assert it holds no state.
        assert_eq!(std::mem::size_of::<FrameRenderer>(), 0);
    }

    // -----------------------------------------------------------------------
    // render_loading: smoke tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_render_loading_does_not_panic() {
        let snapshot = make_snapshot();
        let output = FrameRenderer::render_loading(&snapshot, false, 80, 24);
        // Loading screen must produce some output even when state is empty.
        assert!(!output.is_empty());
    }

    #[test]
    fn test_render_loading_remote_does_not_panic() {
        let snapshot = make_snapshot();
        let output = FrameRenderer::render_loading(&snapshot, true, 80, 24);
        assert!(!output.is_empty());
    }

    #[test]
    fn test_render_loading_with_startup_status_lines() {
        let mut state = AppState::new();
        state
            .startup_status_lines
            .push("Connecting to GPUs...".to_string());
        let snapshot = RenderSnapshot::capture(&state);
        // Should not panic and produce output with the status line.
        let output = FrameRenderer::render_loading(&snapshot, false, 80, 24);
        assert!(!output.is_empty());
    }

    // -----------------------------------------------------------------------
    // render_main: smoke tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_render_main_does_not_panic_empty_state() {
        let snapshot = make_snapshot();
        let args = make_local_args();
        let (output, _) = FrameRenderer::render_main(&snapshot, &args, 80, 24, None);
        // Header must be present.
        assert!(output.contains("all-smi"));
    }

    #[test]
    fn test_render_main_contains_header_timestamp() {
        let snapshot = make_snapshot();
        let args = make_local_args();
        let (output, _) = FrameRenderer::render_main(&snapshot, &args, 120, 40, None);
        // The header includes the current year which is deterministic for the test run.
        assert!(output.contains("all-smi - 20"));
    }

    #[test]
    fn test_render_main_contains_version() {
        let snapshot = make_snapshot();
        let args = make_local_args();
        let (output, _) = FrameRenderer::render_main(&snapshot, &args, 80, 24, None);
        let version = env!("CARGO_PKG_VERSION");
        assert!(output.contains(version));
    }

    // -----------------------------------------------------------------------
    // render_disconnection_notification: box geometry
    // -----------------------------------------------------------------------

    #[test]
    fn test_disconnection_notification_width_too_narrow_produces_no_box() {
        // width=9 → box_width = min(9-4, 60) = 5 which is < 6, so nothing is rendered
        // (only the two leading blank lines appear).
        let mut buffer = BufferWriter::new();
        FrameRenderer::render_disconnection_notification(&mut buffer, "node1", 9);
        let output = buffer.get_buffer().to_string();
        // The box should NOT be rendered; the output must not contain the box corner.
        assert!(!output.contains('\u{250c}'));
    }

    #[test]
    fn test_disconnection_notification_normal_width_contains_hostname() {
        let mut buffer = BufferWriter::new();
        FrameRenderer::render_disconnection_notification(&mut buffer, "my-node", 80);
        let output = buffer.get_buffer().to_string();
        assert!(output.contains("my-node"));
        assert!(output.contains("CONNECTION LOST"));
    }

    #[test]
    fn test_disconnection_notification_box_max_width_capped_at_60() {
        // With a very wide terminal (200 cols) the box should be capped at 60 chars.
        let mut buffer = BufferWriter::new();
        FrameRenderer::render_disconnection_notification(&mut buffer, "node1", 200);
        let output = buffer.get_buffer().to_string();
        // The box top border is: "─" repeated (box_width-2) times, capped at 58 for width=200.
        // Count the number of consecutive box-drawing horizontal lines.
        let horizontal_line_count = output.matches('\u{2500}').count();
        // max box_width = 60, so max horizontal lines per border = 58
        // Two borders (top + bottom) → at most 116.
        assert!(horizontal_line_count <= 116);
        // But there must be at least some lines (it renders).
        assert!(horizontal_line_count > 0);
    }

    #[test]
    fn test_disconnection_notification_long_hostname_is_truncated() {
        // A hostname that exceeds inner_width (box_width - 4) should be truncated.
        let long_hostname = "a".repeat(200);
        let mut buffer = BufferWriter::new();
        FrameRenderer::render_disconnection_notification(&mut buffer, &long_hostname, 80);
        let output = buffer.get_buffer().to_string();
        // "Node: " prefix plus some of the hostname must appear, but not all 200 'a's.
        assert!(output.contains("Node: "));
        assert!(!output.contains(&long_hostname));
    }

    // -----------------------------------------------------------------------
    // render_help: smoke test
    // -----------------------------------------------------------------------

    #[test]
    fn test_render_help_does_not_panic() {
        let snapshot = make_snapshot();
        let args = make_local_args();
        let output = FrameRenderer::render_help(&snapshot, &args, 80, 24);
        // Help popup must produce output.
        assert!(!output.is_empty());
    }
}
