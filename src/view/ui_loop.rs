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

use std::collections::HashSet;
use std::io::{stdout, Write};
use std::sync::Arc;
use std::time::Duration;

use chrono::Local;
use crossterm::{
    cursor,
    event::{self, Event},
    queue,
    style::{Color, Print},
    terminal::size,
};
use tokio::sync::Mutex;

use crate::app_state::AppState;
use crate::cli::ViewArgs;
use crate::common::config::AppConfig;
use crate::ui::buffer::{BufferWriter, DifferentialRenderer};
use crate::ui::dashboard::{draw_dashboard_items, draw_system_view};
use crate::ui::layout::LayoutCalculator;
use crate::ui::renderer::{
    print_chassis_info, print_cpu_info, print_function_keys, print_gpu_info,
    print_loading_indicator, print_memory_info, print_process_info, print_storage_info,
};
use crate::ui::tabs::draw_tabs;
use crate::ui::text::print_colored_text;
use crate::view::event_handler::handle_key_event;

pub struct UiLoop {
    app_state: Arc<Mutex<AppState>>,
    differential_renderer: DifferentialRenderer,
    previous_show_help: bool,
    previous_loading: bool,
    previous_tab: usize,
    previous_show_per_core_cpu: bool,
    last_render_time: std::time::Instant,
    resize_occurred: bool,
    /// Track the last rendered data version to skip re-rendering unchanged data
    last_rendered_data_version: u64,
    /// Track scroll state changes
    previous_gpu_scroll_offset: usize,
    previous_storage_scroll_offset: usize,
    previous_selected_process_index: usize,
    previous_process_horizontal_scroll_offset: usize,
    previous_tab_scroll_offset: usize,
    previous_gpu_filter_enabled: bool,
    #[cfg(target_os = "linux")]
    hlsmi_notified: bool,
    #[cfg(target_os = "linux")]
    hlsmi_pending_notified: bool,
    #[cfg(target_os = "linux")]
    last_hlsmi_check: std::time::Instant,
}

impl UiLoop {
    pub fn new(app_state: Arc<Mutex<AppState>>) -> Result<Self, Box<dyn std::error::Error>> {
        let differential_renderer =
            DifferentialRenderer::new().map_err(|_| "Failed to create differential renderer")?;

        Ok(Self {
            app_state,
            differential_renderer,
            previous_show_help: false,
            previous_loading: false,
            previous_tab: 0,
            previous_show_per_core_cpu: false,
            last_render_time: std::time::Instant::now(),
            resize_occurred: false,
            last_rendered_data_version: 0,
            previous_gpu_scroll_offset: 0,
            previous_storage_scroll_offset: 0,
            previous_selected_process_index: 0,
            previous_process_horizontal_scroll_offset: 0,
            previous_tab_scroll_offset: 0,
            previous_gpu_filter_enabled: false,
            #[cfg(target_os = "linux")]
            hlsmi_notified: false,
            #[cfg(target_os = "linux")]
            hlsmi_pending_notified: false,
            #[cfg(target_os = "linux")]
            last_hlsmi_check: std::time::Instant::now(),
        })
    }

    pub async fn run(&mut self, args: &ViewArgs) -> Result<(), Box<dyn std::error::Error>> {
        loop {
            // Check hl-smi initialization on Linux (periodic check for performance)
            #[cfg(target_os = "linux")]
            {
                use std::time::Duration;

                // Early exit: skip all checks if both notifications have been shown
                if !(self.hlsmi_notified && self.hlsmi_pending_notified) {
                    // Only check if enough time has passed since last check (500ms)
                    if self.last_hlsmi_check.elapsed() >= Duration::from_millis(500) {
                        use crate::device::hlsmi::{get_hlsmi_manager, has_hlsmi_data};

                        // Update last check time
                        self.last_hlsmi_check = std::time::Instant::now();

                        // Show pending notification if manager exists but data not ready
                        if !self.hlsmi_pending_notified
                            && get_hlsmi_manager().is_some()
                            && !has_hlsmi_data()
                        {
                            let mut state = self.app_state.lock().await;
                            let _ = state
                                .notifications
                                .info("Initializing hl-smi...".to_string());
                            self.hlsmi_pending_notified = true;
                        }

                        // Show success notification when data is ready
                        if !self.hlsmi_notified && has_hlsmi_data() {
                            let mut state = self.app_state.lock().await;
                            let _ = state.notifications.status("Intel Gaudi ready".to_string());
                            self.hlsmi_notified = true;
                        }
                    }
                }
            }
            // Handle events with timeout
            if let Ok(has_event) =
                event::poll(Duration::from_millis(AppConfig::EVENT_POLL_TIMEOUT_MS))
            {
                if has_event {
                    match event::read() {
                        Ok(Event::Key(key_event)) => {
                            let mut state = self.app_state.lock().await;
                            let should_break = handle_key_event(key_event, &mut state, args).await;
                            if should_break {
                                break;
                            }
                            drop(state);
                        }
                        Ok(Event::Mouse(mouse_event)) => {
                            let mut state = self.app_state.lock().await;
                            let should_break = crate::view::event_handler::handle_mouse_event(
                                mouse_event,
                                &mut state,
                                args,
                            )
                            .await;
                            if should_break {
                                break;
                            }
                            drop(state);
                        }
                        Ok(Event::Resize(_width, _height)) => {
                            // Force a re-render on terminal resize
                            self.differential_renderer.force_clear().ok();
                            self.resize_occurred = true;
                        }
                        _ => {
                            // Ignore other event types (focus, paste)
                        }
                    }
                }
            }

            // Update display with throttling
            let mut state = self.app_state.lock().await;

            // Check if we need to force clear due to mode change or tab change
            let force_clear = state.show_help != self.previous_show_help
                || state.loading != self.previous_loading
                || state.current_tab != self.previous_tab
                || state.show_per_core_cpu != self.previous_show_per_core_cpu
                || state.gpu_filter_enabled != self.previous_gpu_filter_enabled
                || self.resize_occurred;

            // Check if data has changed (used for skipping expensive rendering when idle)
            let data_changed = state.data_version != self.last_rendered_data_version;

            // Check if scroll/selection state has changed (requires re-render)
            let scroll_changed = state.gpu_scroll_offset != self.previous_gpu_scroll_offset
                || state.storage_scroll_offset != self.previous_storage_scroll_offset
                || state.selected_process_index != self.previous_selected_process_index
                || state.process_horizontal_scroll_offset
                    != self.previous_process_horizontal_scroll_offset
                || state.tab_scroll_offset != self.previous_tab_scroll_offset;

            // Check if enough time has passed for rendering (throttle to prevent visual artifacts)
            let now = std::time::Instant::now();
            let time_to_render = now.duration_since(self.last_render_time).as_millis()
                >= AppConfig::MIN_RENDER_INTERVAL_MS as u128;

            // Only render if there's something worth rendering
            // Note: We always render when time_to_render is true to ensure smooth
            // text scrolling animations. DifferentialRenderer's hash check will
            // skip actual rendering if content is unchanged.
            let should_render = force_clear
                || self.resize_occurred
                || (time_to_render && (data_changed || scroll_changed));

            // Update scroll offsets for long text (controlled by SCROLL_UPDATE_FREQUENCY)
            // This runs independently of should_render to keep animations smooth
            if time_to_render {
                state.frame_counter += 1;
                #[allow(clippy::modulo_one)]
                if state.frame_counter % AppConfig::SCROLL_UPDATE_FREQUENCY == 0 {
                    self.update_scroll_offsets(&mut state);
                }
            }

            if !should_render && !time_to_render {
                drop(state);
                continue; // Skip if nothing changed and not time to render yet
            }

            self.last_render_time = now;

            let (cols, rows) = match size() {
                Ok((c, r)) => (c, r),
                Err(_) => return Err("Failed to get terminal size".into()),
            };

            let mut stdout = stdout();
            if queue!(stdout, cursor::Hide).is_err() {
                break;
            }

            if force_clear && self.differential_renderer.force_clear().is_err() {
                break;
            }

            // Create content using buffer, then render differentially
            let content = if state.show_help {
                self.render_help_popup_content(&state, args, cols, rows)
            } else if state.loading {
                let is_remote = args.hosts.is_some() || args.hostfile.is_some();
                self.render_loading_content(&state, is_remote, cols, rows)
            } else {
                self.render_main_content(&state, args, cols, rows)
            };

            // Use differential rendering to update only changed lines
            if self
                .differential_renderer
                .render_differential(&content)
                .is_err()
            {
                break;
            }

            // Update previous state
            self.previous_show_help = state.show_help;
            self.previous_loading = state.loading;
            self.previous_tab = state.current_tab;
            self.previous_show_per_core_cpu = state.show_per_core_cpu;
            self.previous_gpu_filter_enabled = state.gpu_filter_enabled;
            self.last_rendered_data_version = state.data_version;
            self.previous_gpu_scroll_offset = state.gpu_scroll_offset;
            self.previous_storage_scroll_offset = state.storage_scroll_offset;
            self.previous_selected_process_index = state.selected_process_index;
            self.previous_process_horizontal_scroll_offset = state.process_horizontal_scroll_offset;
            self.previous_tab_scroll_offset = state.tab_scroll_offset;
            self.resize_occurred = false;

            if queue!(stdout, cursor::Show).is_err() {
                break;
            }
            if stdout.flush().is_err() {
                break;
            }
        }

        Ok(())
    }

    fn update_scroll_offsets(&self, state: &mut AppState) {
        let mut processed_hostnames = HashSet::new();

        // Collect GPU keys and lengths first to avoid borrow conflicts
        let gpu_updates: Vec<_> = state
            .gpu_info
            .iter()
            .filter_map(|gpu| {
                if gpu.name.len() > 15 {
                    Some((gpu.uuid.clone(), gpu.name.len()))
                } else {
                    None
                }
            })
            .collect();

        let gpu_hostname_updates: Vec<_> = state
            .gpu_info
            .iter()
            .filter_map(|gpu| {
                if gpu.hostname.len() > 9 && processed_hostnames.insert(gpu.host_id.clone()) {
                    Some((gpu.host_id.clone(), gpu.hostname.len()))
                } else {
                    None
                }
            })
            .collect();

        // Collect CPU keys and lengths
        let cpu_updates: Vec<_> = state
            .cpu_info
            .iter()
            .filter_map(|cpu| {
                if cpu.cpu_model.len() > 15 {
                    let key = format!("{}-{}", cpu.hostname, cpu.cpu_model);
                    Some((key, cpu.cpu_model.len()))
                } else {
                    None
                }
            })
            .collect();

        let cpu_hostname_updates: Vec<_> = state
            .cpu_info
            .iter()
            .filter_map(|cpu| {
                if cpu.hostname.len() > 9 && processed_hostnames.insert(cpu.host_id.clone()) {
                    Some((cpu.host_id.clone(), cpu.hostname.len()))
                } else {
                    None
                }
            })
            .collect();

        // Apply GPU device name scroll updates in-place
        for (key, name_len) in gpu_updates {
            let offset = state.device_name_scroll_offsets.entry(key).or_insert(0);
            *offset = (*offset + 1) % (name_len + 3);
        }

        // Apply hostname scroll updates in-place (GPU + CPU)
        for (key, hostname_len) in gpu_hostname_updates.into_iter().chain(cpu_hostname_updates) {
            let offset = state.host_id_scroll_offsets.entry(key).or_insert(0);
            *offset = (*offset + 1) % (hostname_len + 3);
        }

        // Apply CPU name scroll updates in-place
        for (key, model_len) in cpu_updates {
            let offset = state.cpu_name_scroll_offsets.entry(key).or_insert(0);
            *offset = (*offset + 1) % (model_len + 3);
        }
    }

    fn render_help_popup_content(
        &self,
        state: &AppState,
        args: &ViewArgs,
        cols: u16,
        rows: u16,
    ) -> String {
        let is_remote = args.hosts.is_some() || args.hostfile.is_some();
        crate::ui::help::generate_help_popup_content(cols, rows, state, is_remote)
    }

    fn render_loading_content(
        &self,
        state: &AppState,
        is_remote: bool,
        cols: u16,
        rows: u16,
    ) -> String {
        let mut buffer = BufferWriter::new();
        print_function_keys(&mut buffer, cols, rows, state, is_remote);
        print_loading_indicator(
            &mut buffer,
            cols,
            rows,
            state.frame_counter,
            &state.startup_status_lines,
        );
        buffer.get_buffer().to_string()
    }

    fn render_main_content(
        &self,
        state: &AppState,
        args: &ViewArgs,
        cols: u16,
        rows: u16,
    ) -> String {
        let width = cols as usize;
        let mut buffer = BufferWriter::new();

        // Write time/date header to buffer first
        let current_time = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
        let version = env!("CARGO_PKG_VERSION");
        let header_text = format!("all-smi - {current_time}");
        let version_text = format!("v{version}");

        // Get runtime environment info
        let runtime_shield = if let Some((name, color)) = state.runtime_environment.display_info() {
            // Create a shield-style badge with padding
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
            .unwrap_or(0); // +1 for space before shield
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

        // Write remaining header content to buffer
        print_colored_text(&mut buffer, "Cluster Overview\r\n", Color::Cyan, None, None);
        draw_system_view(&mut buffer, state, cols);

        draw_dashboard_items(&mut buffer, state, cols);
        draw_tabs(&mut buffer, state, cols);

        let is_remote = args.hosts.is_some() || args.hostfile.is_some();

        // Render chassis information (node-level metrics)
        self.render_chassis_section(&mut buffer, state, width);

        // Render GPU information
        self.render_gpu_section(&mut buffer, state, args, cols, rows);

        // Render other device information based on mode
        if is_remote {
            self.render_remote_devices(&mut buffer, state, width);
        } else {
            self.render_local_devices(&mut buffer, state, width);
        }

        // Add function keys to main content view
        print_function_keys(&mut buffer, cols, rows, state, is_remote);

        buffer.get_buffer().to_string()
    }

    fn render_gpu_section(
        &self,
        buffer: &mut BufferWriter,
        state: &AppState,
        args: &ViewArgs,
        cols: u16,
        rows: u16,
    ) {
        let mut gpu_info_to_display: Vec<_> =
            if state.current_tab < state.tabs.len() && state.tabs[state.current_tab] == "All" {
                state.gpu_info.iter().collect()
            } else {
                state
                    .gpu_info
                    .iter()
                    .filter(|info| info.host_id == state.tabs[state.current_tab])
                    .collect()
            };

        // Sort GPUs based on current sort criteria
        gpu_info_to_display.sort_by(|a, b| state.sort_criteria.sort_gpus(a, b));

        // Calculate available space and render GPUs
        let header_lines = LayoutCalculator::calculate_header_lines(state);
        let content_start_row = header_lines;
        let _available_rows = rows.saturating_sub(content_start_row).saturating_sub(1) as usize;

        // Calculate content area and GPU display parameters
        let content_area = LayoutCalculator::calculate_content_area(state, cols, rows);
        let gpu_display_params =
            LayoutCalculator::calculate_gpu_display_params(state, args, &content_area);
        let max_gpu_items = gpu_display_params.max_items;

        // Display GPUs with scrolling
        let start_gpu_index = state.gpu_scroll_offset;
        let end_gpu_index = (start_gpu_index + max_gpu_items).min(gpu_info_to_display.len());

        for (i, gpu_info) in gpu_info_to_display
            .iter()
            .enumerate()
            .skip(start_gpu_index)
            .take(end_gpu_index - start_gpu_index)
        {
            let device_name_scroll_offset = state
                .device_name_scroll_offsets
                .get(&gpu_info.uuid)
                .copied()
                .unwrap_or(0);
            let hostname_scroll_offset = state
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
            );
        }
    }

    fn render_chassis_section(&self, buffer: &mut BufferWriter, state: &AppState, width: usize) {
        if state.chassis_info.is_empty() {
            return;
        }

        // Filter chassis info based on mode and current tab
        let chassis_to_display: Vec<_> = if state.is_local_mode {
            // Local mode: show all chassis info (should be just one)
            state.chassis_info.iter().collect()
        } else if state.current_tab == 0 {
            // Remote mode, "All" tab - don't show individual chassis, skip
            return;
        } else if state.current_tab < state.tabs.len() {
            // Remote mode, specific node tab
            let current_host = &state.tabs[state.current_tab];
            state
                .chassis_info
                .iter()
                .filter(|c| c.host_id == *current_host || c.hostname == *current_host)
                .collect()
        } else {
            // Fallback - show all (shouldn't happen normally)
            state.chassis_info.iter().collect()
        };

        for (i, chassis) in chassis_to_display.iter().enumerate() {
            let hostname_scroll_offset = state
                .host_id_scroll_offsets
                .get(&chassis.host_id)
                .copied()
                .unwrap_or(0);

            print_chassis_info(buffer, i, chassis, width, hostname_scroll_offset);
        }
    }

    fn render_remote_devices(&self, buffer: &mut BufferWriter, state: &AppState, width: usize) {
        // CPU and Memory information for remote mode (only for specific host tabs, not "All" tab)
        if state.current_tab > 0 && state.current_tab < state.tabs.len() {
            let current_hostname = &state.tabs[state.current_tab];

            // Check connection status for the current node
            let is_connected =
                if let Some(host_id) = state.hostname_to_host_id.get(current_hostname) {
                    // Found in reverse lookup, get the connection status
                    state
                        .connection_status
                        .get(host_id)
                        .map(|status| status.is_connected)
                        .unwrap_or(false)
                } else {
                    // Direct lookup by host_id
                    state
                        .connection_status
                        .get(current_hostname)
                        .map(|status| status.is_connected)
                        .unwrap_or(true) // Default to connected for local mode
                };

            if !is_connected {
                // Show elegant disconnection notification
                self.render_disconnection_notification(buffer, current_hostname, width);
                return;
            }

            // CPU information for specific host
            let cpu_info_to_display: Vec<_> = state
                .cpu_info
                .iter()
                .filter(|info| info.host_id == *current_hostname)
                .collect();

            for (i, cpu_info) in cpu_info_to_display.iter().enumerate() {
                // Get scroll offsets for CPU name and hostname
                let cpu_name_scroll_offset = state
                    .cpu_name_scroll_offsets
                    .get(&format!("{}-{}", cpu_info.hostname, cpu_info.cpu_model))
                    .copied()
                    .unwrap_or(0);
                let hostname_scroll_offset = state
                    .host_id_scroll_offsets
                    .get(&cpu_info.host_id)
                    .copied()
                    .unwrap_or(0);
                print_cpu_info(
                    buffer,
                    i,
                    cpu_info,
                    width,
                    state.show_per_core_cpu,
                    cpu_name_scroll_offset,
                    hostname_scroll_offset,
                );
            }

            // Memory information for specific host
            let memory_info_to_display: Vec<_> = state
                .memory_info
                .iter()
                .filter(|info| info.host_id == *current_hostname)
                .collect();

            for (i, memory_info) in memory_info_to_display.iter().enumerate() {
                let hostname_scroll_offset = state
                    .host_id_scroll_offsets
                    .get(&memory_info.host_id)
                    .copied()
                    .unwrap_or(0);
                print_memory_info(buffer, i, memory_info, width, hostname_scroll_offset);
            }

            // Storage information for specific host
            let storage_info_to_display: Vec<_> = state
                .storage_info
                .iter()
                .filter(|info| info.host_id == *current_hostname)
                .collect();

            let visible_storage = storage_info_to_display
                .iter()
                .skip(state.storage_scroll_offset)
                .take(10);

            for (i, storage_info) in visible_storage.enumerate() {
                let hostname_scroll_offset = state
                    .host_id_scroll_offsets
                    .get(&storage_info.host_id)
                    .copied()
                    .unwrap_or(0);
                print_storage_info(buffer, i, storage_info, width, hostname_scroll_offset);
            }
        }
    }

    fn render_disconnection_notification(
        &self,
        buffer: &mut BufferWriter,
        hostname: &str,
        width: usize,
    ) {
        use crate::ui::text::print_colored_text;
        use crossterm::style::Color;

        // Add some spacing
        writeln!(buffer).unwrap();
        writeln!(buffer).unwrap();

        // Create a centered notification box
        let box_width = (width - 4).min(60); // Leave margin and max width
        let margin = (width - box_width) / 2;

        // Top border
        write!(buffer, "{}", " ".repeat(margin)).unwrap();
        print_colored_text(buffer, "┌", Color::Red, None, None);
        print_colored_text(buffer, &"─".repeat(box_width - 2), Color::Red, None, None);
        print_colored_text(buffer, "┐", Color::Red, None, None);
        writeln!(buffer).unwrap();

        // Title line
        let title = "CONNECTION LOST";
        let title_padding = (box_width - 4 - title.len()) / 2;
        write!(buffer, "{}", " ".repeat(margin)).unwrap();
        print_colored_text(buffer, "│ ", Color::Red, None, None);
        print_colored_text(buffer, &" ".repeat(title_padding), Color::White, None, None);
        print_colored_text(buffer, title, Color::Red, None, None);
        print_colored_text(
            buffer,
            &" ".repeat(box_width - 4 - title_padding - title.len()),
            Color::White,
            None,
            None,
        );
        print_colored_text(buffer, " │", Color::Red, None, None);
        writeln!(buffer).unwrap();

        // Empty line
        write!(buffer, "{}", " ".repeat(margin)).unwrap();
        print_colored_text(buffer, "│", Color::Red, None, None);
        print_colored_text(buffer, &" ".repeat(box_width - 2), Color::White, None, None);
        print_colored_text(buffer, "│", Color::Red, None, None);
        writeln!(buffer).unwrap();

        // Hostname line
        let hostname_text = format!("Node: {hostname}");
        let hostname_padding = (box_width - 4 - hostname_text.len()) / 2;
        write!(buffer, "{}", " ".repeat(margin)).unwrap();
        print_colored_text(buffer, "│ ", Color::Red, None, None);
        print_colored_text(
            buffer,
            &" ".repeat(hostname_padding),
            Color::White,
            None,
            None,
        );
        print_colored_text(buffer, &hostname_text, Color::Yellow, None, None);
        print_colored_text(
            buffer,
            &" ".repeat(box_width - 4 - hostname_padding - hostname_text.len()),
            Color::White,
            None,
            None,
        );
        print_colored_text(buffer, " │", Color::Red, None, None);
        writeln!(buffer).unwrap();

        // Status line
        let status_text = "Unable to retrieve node information";
        let status_padding = (box_width - 4 - status_text.len()) / 2;
        write!(buffer, "{}", " ".repeat(margin)).unwrap();
        print_colored_text(buffer, "│ ", Color::Red, None, None);
        print_colored_text(
            buffer,
            &" ".repeat(status_padding),
            Color::White,
            None,
            None,
        );
        print_colored_text(buffer, status_text, Color::DarkGrey, None, None);
        print_colored_text(
            buffer,
            &" ".repeat(box_width - 4 - status_padding - status_text.len()),
            Color::White,
            None,
            None,
        );
        print_colored_text(buffer, " │", Color::Red, None, None);
        writeln!(buffer).unwrap();

        // Empty line
        write!(buffer, "{}", " ".repeat(margin)).unwrap();
        print_colored_text(buffer, "│", Color::Red, None, None);
        print_colored_text(buffer, &" ".repeat(box_width - 2), Color::White, None, None);
        print_colored_text(buffer, "│", Color::Red, None, None);
        writeln!(buffer).unwrap();

        // Bottom border
        write!(buffer, "{}", " ".repeat(margin)).unwrap();
        print_colored_text(buffer, "└", Color::Red, None, None);
        print_colored_text(buffer, &"─".repeat(box_width - 2), Color::Red, None, None);
        print_colored_text(buffer, "┘", Color::Red, None, None);
        writeln!(buffer).unwrap();
    }

    fn render_local_devices(&self, buffer: &mut BufferWriter, state: &AppState, width: usize) {
        // CPU information for local mode
        for (i, cpu_info) in state.cpu_info.iter().enumerate() {
            // Get scroll offsets for CPU name and hostname
            let cpu_name_scroll_offset = state
                .cpu_name_scroll_offsets
                .get(&format!("{}-{}", cpu_info.hostname, cpu_info.cpu_model))
                .copied()
                .unwrap_or(0);
            let hostname_scroll_offset = state
                .host_id_scroll_offsets
                .get(&cpu_info.host_id)
                .copied()
                .unwrap_or(0);
            print_cpu_info(
                buffer,
                i,
                cpu_info,
                width,
                state.show_per_core_cpu,
                cpu_name_scroll_offset,
                hostname_scroll_offset,
            );
        }

        // Memory information for local mode
        for (i, memory_info) in state.memory_info.iter().enumerate() {
            let hostname_scroll_offset = state
                .host_id_scroll_offsets
                .get(&memory_info.host_id)
                .copied()
                .unwrap_or(0);
            print_memory_info(buffer, i, memory_info, width, hostname_scroll_offset);
        }

        // Storage information for local mode
        for (i, storage_info) in state.storage_info.iter().enumerate() {
            let hostname_scroll_offset = state
                .host_id_scroll_offsets
                .get(&storage_info.host_id)
                .copied()
                .unwrap_or(0);
            print_storage_info(buffer, i, storage_info, width, hostname_scroll_offset);
        }

        // Process information for local mode (if available)
        if !state.process_info.is_empty() {
            // The print_process_info function expects the full process list and handles slicing internally
            let (cols, rows) = match crossterm::terminal::size() {
                Ok((c, r)) => (c, r),
                Err(_) => (
                    AppConfig::DEFAULT_TERMINAL_WIDTH,
                    AppConfig::DEFAULT_TERMINAL_HEIGHT,
                ),
            };

            // Calculate how many lines have been used so far
            // Use the efficient line counter from BufferWriter
            let lines_used = buffer.line_count();

            // Add a blank line before process list
            queue!(buffer, Print("\r\n")).unwrap();

            // Reserve 1 line for function keys at the bottom
            let function_key_rows = 1;

            // Calculate available rows for process list
            // Use all remaining space from current position to the function keys
            // Account for the blank line we just added
            let available_rows = rows.saturating_sub(lines_used as u16 + 1 + function_key_rows);

            // Get current user for process coloring
            let current_user = whoami::username();

            // Apply GPU filter if enabled
            let filtered_processes: Vec<_> = if state.gpu_filter_enabled {
                state
                    .process_info
                    .iter()
                    .filter(|p| p.used_memory > 0)
                    .cloned()
                    .collect()
            } else {
                state.process_info.clone()
            };

            print_process_info(
                buffer,
                &filtered_processes,
                state.selected_process_index,
                state.start_index,
                available_rows,
                cols,
                state.process_horizontal_scroll_offset,
                &current_user,
                &state.sort_criteria,
                &state.sort_direction,
            );
        }
    }
}
