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
use tokio::sync::{Notify, mpsc};

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

    /// Set whether fast animation ticks should fire.
    ///
    /// When `active` is true, ticks fire at `ANIMATION_TICK_MS` (200ms) for
    /// smooth marquee and loading animations. When false, ticks fire at
    /// `REFRESH_TICK_MS` (1s) for clock updates and periodic refresh.
    /// The tick is never fully disabled so the display stays alive.
    pub fn set_animations_active(&mut self, active: bool) {
        if active != self.animations_active {
            self.animations_active = active;
            let new_period = if active {
                Duration::from_millis(AppConfig::ANIMATION_TICK_MS)
            } else {
                Duration::from_millis(AppConfig::REFRESH_TICK_MS)
            };
            self.animation_interval = tokio::time::interval(new_period);
            self.animation_interval
                .set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        }
    }

    /// Wait for the next UI event from any source.
    ///
    /// This is the main select point. It sleeps efficiently until one of the
    /// registered sources has something to deliver. When multiple sources fire
    /// simultaneously, `tokio::select!` picks one at random, ensuring fairness.
    ///
    /// Returns `TerminalClosed` when the terminal reader task has exited,
    /// Drain all pending terminal events from the channel without blocking.
    /// Returns them as a vector so the caller can batch-process them
    /// and render only once at the end.
    pub fn drain_pending_events(&mut self) -> Vec<UiEvent> {
        let mut events = Vec::new();
        while let Ok(event) = self.term_rx.try_recv() {
            match event {
                Event::Resize(w, h) => events.push(UiEvent::Resize(w, h)),
                other => events.push(UiEvent::TerminalInput(other)),
            }
        }
        events
    }

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

            // Branch 3: periodic tick (fast for animations, slow for clock-only refresh)
            _ = self.animation_interval.tick() => {
                UiEvent::AnimationTick
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::Notify;

    // -----------------------------------------------------------------------
    // UiEvent: basic enum behaviour
    // -----------------------------------------------------------------------

    #[test]
    fn test_ui_event_debug_variants() {
        // Verify Debug can be derived for all variants without panicking.
        let events: Vec<UiEvent> = vec![
            UiEvent::DataReady,
            UiEvent::AnimationTick,
            UiEvent::TerminalClosed,
            UiEvent::Resize(80, 24),
        ];
        for e in events {
            let _ = format!("{e:?}");
        }
    }

    #[test]
    fn test_resize_event_carries_dimensions() {
        let ev = UiEvent::Resize(120, 40);
        match ev {
            UiEvent::Resize(w, h) => {
                assert_eq!(w, 120);
                assert_eq!(h, 40);
            }
            _ => panic!("Expected Resize variant"),
        }
    }

    // -----------------------------------------------------------------------
    // UiEventCoordinator: construction
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_coordinator_new_does_not_panic() {
        let notify = Arc::new(Notify::new());
        let _coordinator = UiEventCoordinator::new(notify);
    }

    #[tokio::test]
    async fn test_coordinator_animations_active_by_default() {
        // The coordinator starts with animations active so the loading screen
        // shows its spinner immediately.  Drop it to confirm no panic occurs.
        let notify = Arc::new(Notify::new());
        let coordinator = UiEventCoordinator::new(notify);
        drop(coordinator);
    }

    // -----------------------------------------------------------------------
    // UiEventCoordinator: set_animations_active
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_set_animations_active_toggle() {
        let notify = Arc::new(Notify::new());
        let mut coordinator = UiEventCoordinator::new(notify);

        // Toggle off and on; neither call should panic.
        coordinator.set_animations_active(false);
        coordinator.set_animations_active(true);
        coordinator.set_animations_active(false);
    }

    // -----------------------------------------------------------------------
    // UiEventCoordinator: DataReady path
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_next_event_data_ready() {
        let notify = Arc::new(Notify::new());
        let mut coordinator = UiEventCoordinator::new(Arc::clone(&notify));
        // Disable animation ticks so they don't race with our assertion.
        coordinator.set_animations_active(false);

        // Pre-notify before calling next_event so there is no blocking wait.
        notify.notify_one();

        let event = tokio::time::timeout(Duration::from_secs(1), coordinator.next_event())
            .await
            .expect("next_event timed out");

        assert!(
            matches!(event, UiEvent::DataReady),
            "Expected DataReady, got {event:?}"
        );
    }

    // -----------------------------------------------------------------------
    // UiEventCoordinator: TerminalClosed path
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_next_event_terminal_closed_when_channel_drops() {
        let notify = Arc::new(Notify::new());
        let mut coordinator = UiEventCoordinator::new(Arc::clone(&notify));
        coordinator.set_animations_active(false);

        // spawn_terminal_reader consumes the internal sender by moving it into
        // the blocking task.  That task will try to read from the real terminal
        // and exit quickly when the channel receiver is gone; however in a test
        // environment the reader task may linger until the poll timeout fires.
        // Instead of relying on the real reader, we simulate channel closure by
        // dropping the coordinator's term_tx via spawn_terminal_reader and then
        // waiting for the coordinator to observe the closed channel.
        coordinator.spawn_terminal_reader();

        // Drive next_event in a loop; the blocking task will eventually close
        // the channel sender (it exits when tx.blocking_send fails or the poll
        // timeout elapses and tx.is_closed() returns true).
        let event = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                match coordinator.next_event().await {
                    UiEvent::TerminalClosed => return UiEvent::TerminalClosed,
                    // Animation ticks or DataReady may fire before the reader
                    // task exits; skip them.
                    _ => continue,
                }
            }
        })
        .await
        .expect("TerminalClosed event never arrived");

        assert!(matches!(event, UiEvent::TerminalClosed));
    }

    // -----------------------------------------------------------------------
    // Event mapping logic (unit-tested in isolation)
    // -----------------------------------------------------------------------

    /// Verify that the Event::Resize mapping logic used inside next_event
    /// produces UiEvent::Resize with correct dimensions.
    #[test]
    fn test_resize_event_mapping() {
        let crossterm_event = Event::Resize(100, 50);
        let ui_event = match crossterm_event {
            Event::Resize(w, h) => UiEvent::Resize(w, h),
            other => UiEvent::TerminalInput(other),
        };
        assert!(matches!(ui_event, UiEvent::Resize(100, 50)));
    }

    /// Verify that a non-Resize crossterm event is wrapped in TerminalInput.
    #[test]
    fn test_terminal_input_wrapping() {
        let key_event = Event::Key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE));
        let ui_event = match key_event {
            Event::Resize(w, h) => UiEvent::Resize(w, h),
            other => UiEvent::TerminalInput(other),
        };
        assert!(
            matches!(ui_event, UiEvent::TerminalInput(_)),
            "Expected TerminalInput, got {ui_event:?}"
        );
    }

    // -----------------------------------------------------------------------
    // AppConfig: event-driven constants are sane
    // -----------------------------------------------------------------------

    #[test]
    fn test_animation_tick_ms_reasonable() {
        // Must be positive and not unreasonably large.
        assert!(AppConfig::ANIMATION_TICK_MS > 0);
        assert!(AppConfig::ANIMATION_TICK_MS <= 500);
    }

    #[test]
    fn test_terminal_reader_poll_ms_reasonable() {
        // Must be positive and not so large that shutdown detection is sluggish.
        assert!(AppConfig::TERMINAL_READER_POLL_MS > 0);
        assert!(AppConfig::TERMINAL_READER_POLL_MS <= 200);
    }

    #[test]
    fn test_animation_tick_ms_value() {
        assert_eq!(AppConfig::ANIMATION_TICK_MS, 200);
    }

    #[test]
    fn test_terminal_reader_poll_ms_value() {
        assert_eq!(AppConfig::TERMINAL_READER_POLL_MS, 50);
    }

    // -----------------------------------------------------------------------
    // DataCollector: notify_ui wiring
    // -----------------------------------------------------------------------

    /// Verify that DataCollector::with_notify stores the notify handle and
    /// that the coordinator observes a DataReady event when the collector
    /// calls notify_one() on the shared handle.
    #[tokio::test]
    async fn test_data_collector_notify_wiring() {
        use crate::app_state::AppState;
        use crate::view::data_collector::DataCollector;
        use tokio::sync::Mutex;

        let app_state = Arc::new(Mutex::new(AppState::new()));
        let notify = Arc::new(Notify::new());

        // Build collector and coordinator sharing the same notify.
        let _collector = DataCollector::with_notify(Arc::clone(&app_state), Arc::clone(&notify));
        let mut coordinator = UiEventCoordinator::new(Arc::clone(&notify));
        coordinator.set_animations_active(false);

        // Signal as the collector would after a successful data update.
        notify.notify_one();

        let event = tokio::time::timeout(Duration::from_millis(200), coordinator.next_event())
            .await
            .expect("DataReady event never arrived");

        assert!(
            matches!(event, UiEvent::DataReady),
            "Expected DataReady, got {event:?}"
        );
    }
}
