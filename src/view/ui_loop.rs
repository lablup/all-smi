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
use std::sync::Arc;

use crossterm::{cursor, event::Event, queue};
use tokio::sync::{Mutex, Notify};

use crate::app_state::AppState;
use crate::cli::ViewArgs;
use crate::common::config::AppConfig;
use crate::ui::buffer::DifferentialRenderer;
use crate::ui::viewport::Viewport;
use crate::view::event_handler::handle_key_event;
use crate::view::frame_renderer::FrameRenderer;
use crate::view::render_snapshot::{HeavyCache, RenderDecisions, RenderSnapshot};
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
    previous_alert_panel_open: bool,
    previous_filter_is_active: bool,
    /// Whether the last iteration rendered the too-small notice instead of a
    /// frame, so the transition back can repaint from scratch.
    previous_too_small: bool,
    /// Cached derived view data (sorted GPU lists, filtered host subsets, etc.)
    view_cache: ViewCache,
    /// Heavy part of the last snapshot, reused until the data version moves.
    heavy_cache: HeavyCache,
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
            previous_alert_panel_open: false,
            previous_filter_is_active: false,
            previous_too_small: false,
            view_cache: ViewCache::new(),
            heavy_cache: HeavyCache::default(),
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

        // Hide cursor once at session start. The cursor is restored on every
        // exit path (normal q, panic, SIGINT/SIGTERM) by terminal_manager —
        // see Drop and restore_terminal() (issue #235).
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

                // Fast ticks only while something on screen animates;
                // otherwise the slow refresh tick keeps the clock alive
                // and keeps terminal output low over SSH.
                let animations_needed = animations_needed(&state);
                self.event_coordinator
                    .set_animations_active(animations_needed);

                // Check if we need to force clear due to mode change or tab change
                let filter_is_active = state.filter_query.is_some()
                    || state.filter_input_mode == crate::app_state::FilterInputMode::Editing;
                let force_clear = state.show_help != self.previous_show_help
                    || state.loading != self.previous_loading
                    || state.current_tab != self.previous_tab
                    || state.gpu_filter_enabled != self.previous_gpu_filter_enabled
                    || state.alert_panel_open != self.previous_alert_panel_open
                    || filter_is_active != self.previous_filter_is_active
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

                if !should_render {
                    needs_render = false;
                    // Lock is dropped here via `state` going out of scope
                    continue;
                }

                // One step per drawn frame: the marquee, loading bar and
                // LED grid pages derive their phase from this counter, so
                // wakeups that draw nothing must not advance it.
                state.frame_counter += 1;

                self.last_render_time = now;
                needs_render = false;

                // Capture snapshot while still holding the lock. The heavy
                // collections are re-cloned only when data_version moved;
                // otherwise the previous frame's copy is shared.
                let snapshot = RenderSnapshot::capture(&mut state, &mut self.heavy_cache);

                // Update previous-state tracking
                self.previous_show_help = state.show_help;
                self.previous_loading = state.loading;
                self.previous_tab = state.current_tab;
                self.previous_gpu_filter_enabled = state.gpu_filter_enabled;
                self.previous_alert_panel_open = state.alert_panel_open;
                self.previous_filter_is_active = filter_is_active;
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
            let viewport = Viewport::current();
            let (cols, rows) = (viewport.cols, viewport.rows);

            // Terminals below the minimum cannot hold a frame: the header,
            // one content row and the status bar do not fit, and the gauge
            // renderers have no room to lay out. Show a one-line notice and
            // skip composition entirely. `UiEvent::Resize` wakes the loop, so
            // growing the window recovers on its own (issue #326).
            //
            // A pty reporting no geometry never lands here. `Viewport`
            // resolved that to the fallback size above, because "the kernel
            // does not know how big this window is" is a different statement
            // from "this window is too small to draw in".
            if !viewport.is_renderable() {
                if !self.previous_too_small {
                    self.previous_too_small = true;
                    self.view_cache.invalidate_all();
                    self.differential_renderer.force_clear().ok();
                }
                if self
                    .differential_renderer
                    .render_differential(&viewport.too_small_notice(), cols, rows)
                    .is_err()
                {
                    break;
                }
                continue;
            }

            // Coming back from the notice: the screen holds one stale line
            // and the cache was dropped, so repaint from scratch.
            if self.previous_too_small {
                self.previous_too_small = false;
                self.view_cache.invalidate_all();
                if self.differential_renderer.force_clear().is_err() {
                    break;
                }
            }

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
            } else if snapshot.alert_panel_open {
                (FrameRenderer::render_alert_panel(&snapshot, cols, rows), 0)
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
}

/// Name lengths above which the remote rows scroll their text instead of
/// fitting it (the widths `print_gpu_info`, `print_cpu_info` and the
/// hostname formatters truncate at).
const MARQUEE_NAME_WIDTH: usize = 15;
const MARQUEE_HOSTNAME_WIDTH: usize = 9;

/// Whether the UI needs the fast animation tick right now.
///
/// True while loading, while any alert card flashes, and in remote mode
/// while any GPU name, CPU model or hostname is long enough to scroll.
/// Local mode never scrolls names (its rows come from
/// `print_gpu_details` / `print_cpu_details`, which take no offset), so a
/// long local CPU model does not keep the 200 ms tick on. The answer is
/// derived from the current data only, so a long-named host that leaves
/// the cluster stops the fast tick with it.
fn animations_needed(state: &AppState) -> bool {
    if state.loading || state.alerter.any_flashing() {
        return true;
    }
    if state.is_local_mode {
        return false;
    }
    state.gpu_info.iter().any(|gpu| {
        gpu.name.len() > MARQUEE_NAME_WIDTH || gpu.hostname.len() > MARQUEE_HOSTNAME_WIDTH
    }) || state.cpu_info.iter().any(|cpu| {
        cpu.cpu_model.len() > MARQUEE_NAME_WIDTH || cpu.hostname.len() > MARQUEE_HOSTNAME_WIDTH
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::CpuInfo;
    use crate::view::render_snapshot::tests::test_gpu;

    fn cpu(model: &str, hostname: &str) -> CpuInfo {
        CpuInfo {
            host_id: hostname.to_string(),
            hostname: hostname.to_string(),
            instance: hostname.to_string(),
            cpu_model: model.to_string(),
            ..CpuInfo::test_default()
        }
    }

    fn long_named_state(is_local_mode: bool) -> AppState {
        let mut state = AppState::new();
        state.loading = false;
        state.is_local_mode = is_local_mode;
        state.gpu_info = vec![test_gpu("GPU-0", "NVIDIA GeForce RTX 4090", "ws")];
        state.cpu_info = vec![cpu("AMD Ryzen 9 7950X 16-Core Processor", "ws")];
        state
    }

    #[test]
    fn local_mode_with_long_names_uses_the_slow_tick() {
        assert!(!animations_needed(&long_named_state(true)));
    }

    #[test]
    fn remote_mode_with_long_names_keeps_the_fast_tick() {
        assert!(animations_needed(&long_named_state(false)));

        // A long hostname alone also scrolls.
        let mut state = AppState::new();
        state.loading = false;
        state.is_local_mode = false;
        state.gpu_info = vec![test_gpu("GPU-0", "H100", "gpu-node-0001")];
        assert!(animations_needed(&state));
    }

    #[test]
    fn flashing_alert_keeps_the_fast_tick_in_both_modes() {
        for local in [true, false] {
            let mut state = AppState::new();
            state.loading = false;
            state.is_local_mode = local;
            assert!(!animations_needed(&state));
            state.alerter.mark_flash("GPU-0");
            assert!(animations_needed(&state), "local={local}");
        }
    }

    #[test]
    fn loading_keeps_the_fast_tick() {
        let state = AppState::new();
        assert!(state.loading);
        assert!(animations_needed(&state));
    }

    #[test]
    fn removing_the_long_named_hosts_stops_the_fast_tick() {
        let mut state = long_named_state(false);
        assert!(animations_needed(&state));
        state.gpu_info.clear();
        state.cpu_info.clear();
        assert!(
            !animations_needed(&state),
            "no per-entity marquee state may outlive the hosts it scrolled"
        );
    }
}
