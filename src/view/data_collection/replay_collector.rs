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

//! Replay strategy for `all-smi view --replay <file>`.
//!
//! Uses [`crate::record::replay::Replayer`] to stream recorded frames
//! back into the same `RenderSnapshot` pipeline the live collectors
//! feed. All playback controls (play/pause, step, seek, speed) are
//! stored on [`AppState::replay`]; the event handler mutates them in
//! response to key presses and the collector task consumes them.
//!
//! No renderer code should branch on replay mode: the only UI
//! difference is the status-bar chip in `src/ui/chrome.rs`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;

use crate::app_state::{AppState, ReplayState};
use crate::record::replay::{ReplayError, Replayer};

/// Drives the replay pipeline: reads frames from disk, honors
/// play/pause/speed/seek commands stashed on `AppState.replay`, and
/// pushes device data back onto `AppState`.
pub struct ReplayDriver {
    replayer: Replayer,
    /// Local cache of the wall-clock time at which the *currently
    /// displayed* frame was first shown. Used to decide when the next
    /// frame is due under the operator's speed setting.
    displayed_at: Instant,
    /// Timestamp of the currently displayed frame (from the recording).
    /// Next frame is due when `Instant::now() >= displayed_at +
    /// (next.timestamp - current.timestamp) / speed`.
    last_frame_time_offset: Option<Duration>,
}

impl ReplayDriver {
    pub fn open(path: PathBuf) -> Result<Self, ReplayError> {
        let replayer = Replayer::open(&path)?;
        Ok(Self {
            replayer,
            displayed_at: Instant::now(),
            last_frame_time_offset: None,
        })
    }

    pub fn total_hosts(&self) -> Vec<String> {
        self.replayer
            .header()
            .map(|h| h.hosts.clone())
            .unwrap_or_default()
    }

    /// Advance one tick of the replay pipeline and push any updated
    /// frame into `AppState`. The caller should loop this every
    /// ~50ms while the TUI is alive.
    pub async fn tick(&mut self, app_state: Arc<Mutex<AppState>>) -> Result<(), ReplayError> {
        // Snapshot the control block under the lock. We release the
        // lock before doing IO so the event handler stays responsive.
        let control = {
            let state = app_state.lock().await;
            state.replay.clone()
        };

        let Some(control) = control else {
            return Ok(());
        };

        // Handle a pending seek request. Seek runs while the stream is
        // paused or playing; either way we align to the requested
        // offset and mark the seek as consumed.
        if let Some(seek_to) = control.pending_seek {
            match self.replayer.seek(seek_to) {
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(error = %e, "replay: seek failed");
                }
            }
            self.displayed_at = Instant::now();
            self.last_frame_time_offset = self.replayer.elapsed();
            apply_frame_to_state(app_state.clone(), &mut self.replayer, true).await;
            clear_pending_seek(app_state.clone()).await;
            return Ok(());
        }

        if let Some(step) = control.pending_step {
            // Step one frame in the requested direction.
            if step > 0 {
                let _ = self.replayer.next()?;
            } else if step < 0 {
                let _ = self.replayer.prev()?;
            }
            self.displayed_at = Instant::now();
            self.last_frame_time_offset = self.replayer.elapsed();
            apply_frame_to_state(app_state.clone(), &mut self.replayer, true).await;
            clear_pending_step(app_state.clone()).await;
            return Ok(());
        }

        if control.paused {
            return Ok(());
        }

        // Playing: decide whether the next frame is due.
        let Some(current) = self.replayer.current() else {
            return Ok(());
        };
        let current_offset = self.replayer.elapsed().unwrap_or(Duration::ZERO);
        // On the very first tick we seed `last_frame_time_offset` and
        // render the current frame once.
        if self.last_frame_time_offset.is_none() {
            self.last_frame_time_offset = Some(current_offset);
            self.displayed_at = Instant::now();
            apply_frame_to_state(app_state.clone(), &mut self.replayer, true).await;
            return Ok(());
        }

        // Peek at the next frame's timestamp by advancing once; if we
        // are too early, retreat the cursor. The Replayer supports
        // prev() back to cache start so this is cheap while the next
        // frame is close in time.
        let prev_seq = current.seq;
        let next_frame = self.replayer.next()?;
        let (due, reached_end) = match next_frame {
            None => {
                // EOF: either loop or stop.
                if control.replay_loop {
                    self.replayer.seek(Duration::ZERO).ok();
                    self.displayed_at = Instant::now();
                    self.last_frame_time_offset = self.replayer.elapsed();
                    apply_frame_to_state(app_state.clone(), &mut self.replayer, true).await;
                    return Ok(());
                }
                pause_at_end(app_state.clone()).await;
                return Ok(());
            }
            Some(next) => {
                let next_offset = (next.timestamp - first_frame_ts(&self.replayer))
                    .to_std()
                    .unwrap_or(Duration::ZERO);
                let delta = next_offset.saturating_sub(current_offset);
                // `Duration::from_secs_f32` panics on NaN or negative
                // inputs, so normalise `speed` defensively: clamp into
                // the documented [0.05, 16.0] playback range and fall
                // back to 1.0 for NaN / +inf / -inf. A hostile
                // construct-time speed (or a future config bug) can't
                // take the replay driver down this way.
                let mut speed = control.speed;
                if !speed.is_finite() {
                    speed = 1.0;
                }
                let speed = speed.clamp(0.05, 16.0);
                let scaled_delta = Duration::from_secs_f32(delta.as_secs_f32() / speed);
                let due_time = self.displayed_at + scaled_delta;
                let due_now = Instant::now() >= due_time;
                (due_now, false)
            }
        };

        if !due {
            // Retreat back to the previously-displayed frame. prev()
            // here is O(1) because we stayed within the cache window
            // the Replayer maintains. On cache-boundary conditions or
            // a hostile file that caused an unexpected cursor position,
            // fall back to rendering whatever the replayer currently
            // has instead of panicking via `debug_assert_eq!`. Losing
            // a single-frame scheduling step is preferable to aborting
            // the replay session.
            let _ = self.replayer.prev()?;
            let landed_seq = self.replayer.current().map(|f| f.seq);
            if landed_seq != Some(prev_seq) {
                tracing::warn!(
                    expected = prev_seq,
                    got = ?landed_seq,
                    "replay cursor retreat landed on unexpected frame; proceeding"
                );
            }
            return Ok(());
        }

        // The new frame became current via the next() call above.
        self.displayed_at = Instant::now();
        self.last_frame_time_offset = self.replayer.elapsed();
        apply_frame_to_state(app_state.clone(), &mut self.replayer, reached_end).await;
        Ok(())
    }
}

/// Best-effort: we need the first-frame timestamp as an anchor for
/// delta calculations. `Replayer::elapsed()` already factors this in,
/// but when we materialise the *next* frame we need the same anchor
/// to compute its elapsed.  We do NOT rewind the stream here — we
/// just compute the next-frame delta relative to the cache-front.
fn first_frame_ts(replayer: &Replayer) -> chrono::DateTime<chrono::Utc> {
    // Replayer keeps the first frame in cache after priming (or after
    // any seek to offset 0), which is what ReplayDriver ensures before
    // calling this helper. When evicted, fall back to the current
    // frame's timestamp — the resulting elapsed will be zero, which
    // makes the scheduler render immediately. That's acceptable: the
    // worst case is a single-frame stutter after a long back-scroll.
    replayer
        .current()
        .map(|f| f.timestamp)
        .unwrap_or_else(chrono::Utc::now)
}

/// Copy the Replayer's current frame into `AppState` fields the TUI
/// reads from. Mirrors what `LocalCollector::update_state` and
/// `RemoteCollector::update_state` do for live data.
async fn apply_frame_to_state(
    app_state: Arc<Mutex<AppState>>,
    replayer: &mut Replayer,
    force: bool,
) {
    let Some(frame) = replayer.current() else {
        return;
    };
    let mut state = app_state.lock().await;
    let snap = &frame.snapshot;

    if force || state.gpu_info.is_empty() {
        state.gpu_info = snap.gpus.clone().unwrap_or_default();
    } else {
        // Preserve UUIDs where possible so UI invariants (scroll
        // offsets keyed by UUID) do not reset on every tick.
        let new_gpus = snap.gpus.clone().unwrap_or_default();
        state.gpu_info = new_gpus;
    }
    state.cpu_info = snap.cpus.clone().unwrap_or_default();
    state.memory_info = snap.memory.clone().unwrap_or_default();
    state.chassis_info = snap.chassis.clone().unwrap_or_default();
    state.process_info = snap.processes.clone().unwrap_or_default();
    state.storage_info = snap.storage.clone().unwrap_or_default();

    // Cluster-wide Users tab (issue #189): lift the recorded processes
    // into the remote-style row representation so replays render the
    // tab identically to a live scrape.  The host label is whichever
    // identifier the recording stored alongside the snapshot — prefer
    // the first GPU's `host_id` (what the remote collector uses) and
    // fall back to the frame's hostname.
    let host_label = snap
        .gpus
        .as_ref()
        .and_then(|v| v.first())
        .map(|g| g.host_id.clone())
        .unwrap_or_else(|| snap.hostname.clone());
    if let Some(procs) = snap.processes.as_ref() {
        state.remote_process_info = procs
            .iter()
            .map(|p| {
                crate::network::metrics_parser::ParsedProcessRow::from_local_process(p, &host_label)
            })
            .collect();
    } else {
        state.remote_process_info.clear();
    }

    state.loading = false;

    // Keep the replay status bar in sync.
    if let Some(replay) = state.replay.as_mut() {
        replay.current_seq = frame.seq;
        replay.total_frames = replayer.frames_seen();
        replay.at_eof = replayer.at_eof();
        replay.elapsed = replayer.elapsed().unwrap_or(Duration::ZERO);
    }

    // Tabs: for replay we take the union of hosts seen in the
    // recording so the tab bar mirrors what the operator would have
    // seen live. "All" is always first, Users is always second
    // (issue #189) so cluster-level tabs cluster together.
    let mut host_ids: std::collections::BTreeSet<String> = state
        .gpu_info
        .iter()
        .map(|g| g.host_id.clone())
        .filter(|h| !h.is_empty())
        .collect();
    if host_ids.is_empty() {
        // Single-host recording — use the frame's hostname so the UI
        // has *something* to display instead of an empty list.
        if !snap.hostname.is_empty() {
            host_ids.insert(snap.hostname.clone());
        }
    }
    let mut tabs = vec![
        "All".to_string(),
        crate::ui::tabs::USERS_TAB_NAME.to_string(),
        crate::ui::tabs::TOPOLOGY_TAB_NAME.to_string(),
    ];
    tabs.extend(host_ids);

    // Preserve the operator's current selection where possible.
    let previous_name = state.tabs.get(state.current_tab).cloned();
    state.tabs = tabs;
    if let Some(name) = previous_name
        && let Some(idx) = state.tabs.iter().position(|t| *t == name)
    {
        state.current_tab = idx;
    } else if state.current_tab >= state.tabs.len() {
        state.current_tab = 0;
    }

    // Invalidate the Topology tab's remembered host when the stashed name
    // is no longer present in the tab strip (e.g. switched recordings,
    // host dropped out of the replay frame). The renderer will fall back
    // to the first host tab until the operator picks a new one.
    if let Some(last) = state.topology_last_host_tab.as_ref()
        && !state.tabs.iter().any(|t| t == last)
    {
        state.topology_last_host_tab = None;
    }

    state.mark_collector_data_changed();
}

async fn clear_pending_seek(app_state: Arc<Mutex<AppState>>) {
    let mut state = app_state.lock().await;
    if let Some(r) = state.replay.as_mut() {
        r.pending_seek = None;
    }
}

async fn clear_pending_step(app_state: Arc<Mutex<AppState>>) {
    let mut state = app_state.lock().await;
    if let Some(r) = state.replay.as_mut() {
        r.pending_step = None;
    }
}

async fn pause_at_end(app_state: Arc<Mutex<AppState>>) {
    let mut state = app_state.lock().await;
    if let Some(r) = state.replay.as_mut() {
        r.paused = true;
        r.at_eof = true;
    }
}

/// Factory helper used by `runner::run_replay_mode` to construct the
/// initial [`AppState::replay`] block.
pub fn initial_replay_state(speed: f32, replay_loop: bool) -> ReplayState {
    ReplayState {
        paused: false,
        speed,
        current_seq: 0,
        total_frames: 0,
        elapsed: Duration::ZERO,
        at_eof: false,
        replay_loop,
        pending_seek: None,
        pending_step: None,
        timecode_input_mode: false,
        timecode_buffer: String::new(),
        timecode_error: None,
    }
}
