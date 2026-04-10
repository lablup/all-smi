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
use std::io::Write;
use std::sync::Arc;

use crossterm::{cursor, event::Event, queue, terminal::size};
use tokio::sync::{Mutex, Notify};

use crate::app_state::AppState;
use crate::cli::ViewArgs;
use crate::common::config::AppConfig;
use crate::ui::buffer::DifferentialRenderer;
use crate::view::event_handler::handle_key_event;
use crate::view::frame_renderer::FrameRenderer;
use crate::view::render_snapshot::{RenderDecisions, RenderSnapshot};
use crate::view::ui_events::{UiEvent, UiEventCoordinator};
use crate::view::view_cache::ViewCache;

pub struct UiLoop {
    app_state: Arc<Mutex<AppState>>,
    differential_renderer: DifferentialRenderer,
    previous_show_help: bool,
    previous_loading: bool,
    previous_tab: usize,
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
    /// Cached derived view data (sorted GPU lists, filtered host subsets, etc.)
    view_cache: ViewCache,
    /// Event coordinator for event-driven wakeups
    event_coordinator: UiEventCoordinator,
    #[cfg(target_os = "linux")]
    hlsmi_notified: bool,
    #[cfg(target_os = "linux")]
    hlsmi_pending_notified: bool,
    #[cfg(target_os = "linux")]
    last_hlsmi_check: std::time::Instant,
}

impl UiLoop {
    pub fn new(
        app_state: Arc<Mutex<AppState>>,
        data_notify: Arc<Notify>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let differential_renderer =
            DifferentialRenderer::new().map_err(|_| "Failed to create differential renderer")?;

        let event_coordinator = UiEventCoordinator::new(data_notify);

        Ok(Self {
            app_state,
            differential_renderer,
            previous_show_help: false,
            previous_loading: false,
            previous_tab: 0,
            last_render_time: std::time::Instant::now(),
            resize_occurred: false,
            last_rendered_data_version: 0,
            previous_gpu_scroll_offset: 0,
            previous_storage_scroll_offset: 0,
            previous_selected_process_index: 0,
            previous_process_horizontal_scroll_offset: 0,
            previous_tab_scroll_offset: 0,
            previous_gpu_filter_enabled: false,
            view_cache: ViewCache::new(),
            event_coordinator,
            #[cfg(target_os = "linux")]
            hlsmi_notified: false,
            #[cfg(target_os = "linux")]
            hlsmi_pending_notified: false,
            #[cfg(target_os = "linux")]
            last_hlsmi_check: std::time::Instant::now(),
        })
    }

    pub async fn run(&mut self, args: &ViewArgs) -> Result<(), Box<dyn std::error::Error>> {
        // Start the background terminal event reader
        self.event_coordinator.spawn_terminal_reader();

        // Hide cursor once at session start. The cursor is restored in
        // TerminalManager::drop() (LeaveAlternateScreen resets it).
        // This avoids per-frame Hide/Show churn.
        {
            let mut stdout = std::io::stdout();
            if queue!(stdout, cursor::Hide).is_err() {
                return Err("Failed to hide cursor".into());
            }
            stdout.flush().ok();
        }

        // Track whether we need to render after processing events
        let mut needs_render = true; // Render once at startup
        let mut tick_fired = false; // Whether an animation/refresh tick triggered this iteration

        loop {
            // Check hl-smi initialization on Linux (periodic check for performance)
            #[cfg(target_os = "linux")]
            self.check_hlsmi_status().await;

            // If nothing needs rendering, wait for the next event (fully async sleep)
            if !needs_render {
                tick_fired = false;
                match self.event_coordinator.next_event().await {
                    UiEvent::TerminalInput(Event::Key(key_event)) => {
                        let mut state = self.app_state.lock().await;
                        let should_break = handle_key_event(key_event, &mut state, args).await;
                        if should_break {
                            break;
                        }
                        drop(state);
                        needs_render = true;
                    }
                    UiEvent::TerminalInput(Event::Mouse(mouse_event)) => {
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
                        needs_render = true;
                    }
                    UiEvent::TerminalInput(_) => {
                        // Ignore other terminal event types (focus, paste)
                    }
                    UiEvent::Resize(w, h) => {
                        self.differential_renderer.update_dimensions(w, h);
                        self.differential_renderer.force_clear().ok();
                        self.resize_occurred = true;
                        needs_render = true;
                    }
                    UiEvent::DataReady => {
                        needs_render = true;
                    }
                    UiEvent::TerminalClosed => {
                        // Terminal reader exited -- shut down gracefully
                        break;
                    }
                    UiEvent::AnimationTick => {
                        needs_render = true;
                        tick_fired = true;
                    }
                }
            }

            if !needs_render {
                continue;
            }

            // ------------------------------------------------------------------
            // Critical section: acquire state, update mutable bookkeeping,
            // capture snapshot, compute render decisions, then release the lock.
            // All expensive work (frame composition) happens AFTER this block.
            // ------------------------------------------------------------------
            let (snapshot, decisions) = {
                let mut state = self.app_state.lock().await;

                // Activate animation ticks only when there are animated elements.
                // In remote mode, only enable fast ticks when there are actual
                // scrolling animations; otherwise use the slow refresh tick to
                // reduce terminal output over SSH.
                let has_scroll_animations = !state.device_name_scroll_offsets.is_empty()
                    || !state.host_id_scroll_offsets.is_empty()
                    || !state.cpu_name_scroll_offsets.is_empty();
                let animations_needed = state.loading || has_scroll_animations;
                self.event_coordinator
                    .set_animations_active(animations_needed);

                // Check if we need to force clear due to mode change or tab change
                let force_clear = state.show_help != self.previous_show_help
                    || state.loading != self.previous_loading
                    || state.current_tab != self.previous_tab
                    || state.gpu_filter_enabled != self.previous_gpu_filter_enabled
                    || self.resize_occurred;

                // Check if data has changed
                let data_changed = state.data_version != self.last_rendered_data_version;

                // Check if scroll/selection state has changed
                let scroll_changed = state.gpu_scroll_offset != self.previous_gpu_scroll_offset
                    || state.storage_scroll_offset != self.previous_storage_scroll_offset
                    || state.selected_process_index != self.previous_selected_process_index
                    || state.process_horizontal_scroll_offset
                        != self.previous_process_horizontal_scroll_offset
                    || state.tab_scroll_offset != self.previous_tab_scroll_offset;

                // Throttle rendering to prevent visual artifacts
                let now = std::time::Instant::now();
                let time_to_render = now.duration_since(self.last_render_time).as_millis()
                    >= AppConfig::MIN_RENDER_INTERVAL_MS as u128;

                // User-driven scroll/cursor changes render immediately (no throttle)
                // so that keyboard navigation feels responsive.
                // Data-driven updates and periodic ticks are throttled to MIN_RENDER_INTERVAL_MS.
                let should_render = force_clear
                    || self.resize_occurred
                    || scroll_changed
                    || (time_to_render && (data_changed || tick_fired));

                // Update scroll offsets for long text (marquee animation)
                if time_to_render {
                    state.frame_counter += 1;
                    #[allow(clippy::modulo_one)]
                    if state.frame_counter % AppConfig::SCROLL_UPDATE_FREQUENCY == 0 {
                        Self::update_scroll_offsets(&mut state);
                    }
                }

                if !should_render {
                    needs_render = false;
                    // Lock is dropped here via `state` going out of scope
                    continue;
                }

                self.last_render_time = now;
                needs_render = false;

                // Capture snapshot while still holding the lock
                let snapshot = RenderSnapshot::capture(&state);

                // Update previous-state tracking
                self.previous_show_help = state.show_help;
                self.previous_loading = state.loading;
                self.previous_tab = state.current_tab;
                self.previous_gpu_filter_enabled = state.gpu_filter_enabled;
                self.last_rendered_data_version = state.data_version;
                self.previous_gpu_scroll_offset = state.gpu_scroll_offset;
                self.previous_storage_scroll_offset = state.storage_scroll_offset;
                self.previous_selected_process_index = state.selected_process_index;
                self.previous_process_horizontal_scroll_offset =
                    state.process_horizontal_scroll_offset;
                self.previous_tab_scroll_offset = state.tab_scroll_offset;
                self.resize_occurred = false;

                let decisions = RenderDecisions {
                    force_clear,
                    should_render: true,
                    animations_needed,
                };

                (snapshot, decisions)
                // `state` is dropped here -- lock released before frame composition
            };

            // ------------------------------------------------------------------
            // Frame composition: operates entirely on the snapshot, no lock held.
            // ------------------------------------------------------------------
            let (cols, rows) = match size() {
                Ok((c, r)) => (c, r),
                Err(_) => return Err("Failed to get terminal size".into()),
            };

            if decisions.force_clear {
                self.view_cache.invalidate_all();
                if self.differential_renderer.force_clear().is_err() {
                    break;
                }
            }

            // Update derived view cache (only recomputes stale entries)
            self.view_cache.update(&snapshot);

            // Assemble frame content from the snapshot (no lock held)
            let (content, visible_process_rows) = if snapshot.show_help {
                (FrameRenderer::render_help(&snapshot, args, cols, rows), 0)
            } else if snapshot.loading {
                let is_remote = args.hosts.is_some() || args.hostfile.is_some();
                (
                    FrameRenderer::render_loading(&snapshot, is_remote, cols, rows),
                    0,
                )
            } else {
                FrameRenderer::render_main(&snapshot, args, cols, rows, Some(&self.view_cache))
            };

            // Store actual visible process rows so the event handler scrolls correctly
            if visible_process_rows > 0 {
                let mut state = self.app_state.lock().await;
                state.visible_process_rows = visible_process_rows;
            }

            // Use differential rendering to update only changed lines.
            let _t2 = std::time::Instant::now();
            if self
                .differential_renderer
                .render_differential(&content, cols, rows)
                .is_err()
            {
                break;
            }

            // After rendering (which may block on flush over SSH), drain any
            // pending key/mouse events that accumulated during the flush.
            // Process them all now, then render once — this prevents the
            // "one render per queued keypress" cascade that causes input lag.
            for event in self.event_coordinator.drain_pending_events() {
                match event {
                    UiEvent::TerminalInput(Event::Key(key_event)) => {
                        let mut state = self.app_state.lock().await;
                        let should_break = handle_key_event(key_event, &mut state, args).await;
                        if should_break {
                            return Ok(());
                        }
                        needs_render = true;
                    }
                    UiEvent::TerminalInput(Event::Mouse(mouse_event)) => {
                        let mut state = self.app_state.lock().await;
                        let should_break = crate::view::event_handler::handle_mouse_event(
                            mouse_event,
                            &mut state,
                            args,
                        )
                        .await;
                        if should_break {
                            return Ok(());
                        }
                        needs_render = true;
                    }
                    UiEvent::Resize(w, h) => {
                        self.differential_renderer.update_dimensions(w, h);
                        self.differential_renderer.force_clear().ok();
                        self.resize_occurred = true;
                        needs_render = true;
                    }
                    _ => {}
                }
            }
        }

        Ok(())
    }

    /// Check hl-smi initialization on Linux (periodic check for performance).
    #[cfg(target_os = "linux")]
    async fn check_hlsmi_status(&mut self) {
        use std::time::Duration;

        // Early exit: skip all checks if both notifications have been shown
        if self.hlsmi_notified && self.hlsmi_pending_notified {
            return;
        }

        // Only check if enough time has passed since last check (500ms)
        if self.last_hlsmi_check.elapsed() < Duration::from_millis(500) {
            return;
        }

        use crate::device::hlsmi::{get_hlsmi_manager, has_hlsmi_data};

        // Update last check time
        self.last_hlsmi_check = std::time::Instant::now();

        // Show pending notification if manager exists but data not ready
        if !self.hlsmi_pending_notified && get_hlsmi_manager().is_some() && !has_hlsmi_data() {
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

    fn update_scroll_offsets(state: &mut AppState) {
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

        // Skip hostname scroll animation in local mode -- the hostname is
        // not rendered on any device row, so advancing the offset would
        // just burn CPU for no visible effect.
        let gpu_hostname_updates: Vec<_> = if state.is_local_mode {
            Vec::new()
        } else {
            state
                .gpu_info
                .iter()
                .filter_map(|gpu| {
                    if gpu.hostname.len() > 9 && processed_hostnames.insert(gpu.host_id.clone()) {
                        Some((gpu.host_id.clone(), gpu.hostname.len()))
                    } else {
                        None
                    }
                })
                .collect()
        };

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

        let cpu_hostname_updates: Vec<_> = if state.is_local_mode {
            Vec::new()
        } else {
            state
                .cpu_info
                .iter()
                .filter_map(|cpu| {
                    if cpu.hostname.len() > 9 && processed_hostnames.insert(cpu.host_id.clone()) {
                        Some((cpu.host_id.clone(), cpu.hostname.len()))
                    } else {
                        None
                    }
                })
                .collect()
        };

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
}
