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

//! Event-driven wakeup coordinator for the TUI loop.
//!
//! Replaces the fixed `event::poll(100ms)` model with a `tokio::select!`-based
//! coordinator that wakes the UI only for meaningful reasons: terminal input,
//! resize, fresh collector data, and animation ticks.

use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{self, Event};
use tokio::sync::{mpsc, Notify};

use crate::common::config::AppConfig;

/// All possible reasons the UI loop should wake up and consider re-rendering.
#[derive(Debug)]
pub enum UiEvent {
    /// A terminal input event (key press, mouse click, etc.)
    TerminalInput(Event),
    /// The terminal was resized
    Resize(u16, u16),
    /// A background data collector has new data ready
    DataReady,
    /// An animation tick fired (for loading indicator, marquee scroll, clock)
    AnimationTick,
    /// The terminal reader task has exited (terminal closed or error).
    /// The UI loop should shut down gracefully.
    TerminalClosed,
}

/// Manages all event sources and delivers them through a unified channel.
///
/// The coordinator spawns a dedicated blocking task for crossterm event reading
/// (since crossterm uses synchronous I/O) and combines it with async notification
/// sources in a `tokio::select!` loop.
pub struct UiEventCoordinator {
    /// Sender passed to the terminal reader task.
    /// Stored as `Option` so we can `take()` it in `spawn_terminal_reader`,
    /// ensuring no extra sender keeps the channel open after the reader exits.
    term_tx: Option<mpsc::Sender<Event>>,
    /// Receiver for terminal events
    term_rx: mpsc::Receiver<Event>,
    /// Notification from data collectors when new data is available
    data_notify: Arc<Notify>,
    /// Animation tick interval (only active when animations are visible)
    animation_interval: tokio::time::Interval,
    /// Whether animation ticks should be active
    animations_active: bool,
}

impl UiEventCoordinator {
    /// Create a new event coordinator with the given data notification handle.
    ///
    /// `data_notify` should be shared with data collectors so they can signal
    /// the UI when fresh data is available.
    pub fn new(data_notify: Arc<Notify>) -> Self {
        // Bounded channel prevents unbounded memory growth if UI is slow.
        // 64 events is generous enough to buffer rapid keystrokes.
        let (term_tx, term_rx) = mpsc::channel::<Event>(64);

        let mut animation_interval =
            tokio::time::interval(Duration::from_millis(AppConfig::ANIMATION_TICK_MS));
        // Don't burst-fire missed ticks -- just skip them
        animation_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        Self {
            term_tx: Some(term_tx),
            term_rx,
            data_notify,
            animation_interval,
            animations_active: true, // Start active for loading screen
        }
    }

    /// Spawn the background terminal event reader task.
    ///
    /// This must be called once before entering the event loop. The task reads
    /// crossterm events in a blocking context and forwards them through the
    /// channel. It exits automatically when the channel is closed.
    ///
    /// The sender is *moved* into the spawned task so that the channel closes
    /// naturally when the reader exits, allowing `next_event()` to detect
    /// terminal loss via `TerminalClosed`.
    pub fn spawn_terminal_reader(&mut self) {
        let tx = self
            .term_tx
            .take()
            .expect("spawn_terminal_reader must be called exactly once");
        tokio::task::spawn_blocking(move || {
            Self::terminal_reader_loop(tx);
        });
    }

    /// Blocking loop that reads terminal events and forwards them.
    ///
    /// Uses a short poll timeout so the task can detect channel closure
    /// promptly and exit.
    fn terminal_reader_loop(tx: mpsc::Sender<Event>) {
        loop {
            // Poll with a short timeout so we can detect shutdown
            match event::poll(Duration::from_millis(AppConfig::TERMINAL_READER_POLL_MS)) {
                Ok(true) => match event::read() {
                    Ok(evt) => {
                        // blocking_send is fine here -- we are in a blocking context
                        if tx.blocking_send(evt).is_err() {
                            // Receiver dropped, UI loop ended -- exit
                            break;
                        }
                    }
                    Err(_) => {
                        // Terminal read error; likely terminal gone -- exit
                        break;
                    }
                },
                Ok(false) => {
                    // No event within the poll window -- check if channel still alive
                    if tx.is_closed() {
                        break;
                    }
                }
                Err(_) => {
                    // Poll error -- exit
                    break;
                }
            }
        }
    }

    /// Set whether animation ticks should fire.
    ///
    /// When `active` is false the animation interval is effectively paused:
    /// `next_event()` will not return `AnimationTick` variants.
    pub fn set_animations_active(&mut self, active: bool) {
        self.animations_active = active;
    }

    /// Wait for the next UI event from any source.
    ///
    /// This is the main select point. It sleeps efficiently until one of the
    /// registered sources has something to deliver. When multiple sources fire
    /// simultaneously, `tokio::select!` picks one at random, ensuring fairness.
    ///
    /// Returns `TerminalClosed` when the terminal reader task has exited,
    /// signalling that the UI loop should shut down.
    pub async fn next_event(&mut self) -> UiEvent {
        tokio::select! {
            // Branch 1: terminal input/resize from the blocking reader.
            // When the channel closes (reader exited), recv() returns None
            // and we signal TerminalClosed for graceful shutdown.
            result = self.term_rx.recv() => {
                match result {
                    Some(Event::Resize(w, h)) => UiEvent::Resize(w, h),
                    Some(other) => UiEvent::TerminalInput(other),
                    None => UiEvent::TerminalClosed,
                }
            }

            // Branch 2: data collector notification
            _ = self.data_notify.notified() => {
                UiEvent::DataReady
            }

            // Branch 3: animation tick (only when active)
            _ = self.animation_interval.tick(), if self.animations_active => {
                UiEvent::AnimationTick
            }
        }
    }
}
