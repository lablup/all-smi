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

//! Tests for replay playback pacing and end-of-file handling (issue #503).
//!
//! The driver used to anchor the next frame's offset on the next frame
//! itself, so the gap between frames was always zero and every frame was
//! due on the next 50 ms tick whatever the recording's cadence or the
//! speed setting. It also never saw end of file, because the replayer kept
//! returning the last frame.

use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;
use tokio::time::Instant;

use super::{ReplayDriver, initial_replay_state};
use crate::app_state::AppState;

const TICK: Duration = Duration::from_millis(50);

/// Three frames one second apart.
fn write_fixture(path: &Path) {
    let mut f = std::fs::File::create(path).unwrap();
    writeln!(
        f,
        "{{\"schema\":1,\"header\":true,\"interval_ms\":1000,\"hosts\":[\"a\"]}}"
    )
    .unwrap();
    for i in 0..3 {
        writeln!(
            f,
            "{{\"schema\":1,\"timestamp\":\"2026-04-20T00:00:{i:02}Z\",\"hostname\":\"a\",\"gpus\":[]}}"
        )
        .unwrap();
    }
}

/// What a playback run observed.
struct Playback {
    /// Virtual time at which each frame after the first became current.
    shown_at: Vec<(u64, Duration)>,
    state: Arc<Mutex<AppState>>,
}

/// Tick the driver every 50 ms of virtual time for `run_for`.
async fn play(speed: f32, replay_loop: bool, run_for: Duration) -> Playback {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("three.ndjson");
    write_fixture(&path);

    let mut app = AppState::new();
    app.replay = Some(initial_replay_state(speed, replay_loop));
    let state = Arc::new(Mutex::new(app));
    let mut driver = ReplayDriver::open(path).unwrap();

    let started = Instant::now();
    let mut shown_at = Vec::new();
    let mut last_seq = 0;
    while started.elapsed() <= run_for {
        driver.tick(state.clone()).await.unwrap();
        let seq = state.lock().await.replay.as_ref().unwrap().current_seq;
        if seq != last_seq {
            shown_at.push((seq, started.elapsed()));
            last_seq = seq;
        }
        tokio::time::advance(TICK).await;
    }
    Playback { shown_at, state }
}

#[tokio::test(start_paused = true)]
async fn playback_at_speed_one_follows_the_recorded_spacing() {
    let playback = play(1.0, false, Duration::from_secs(4)).await;
    assert_eq!(
        playback.shown_at,
        vec![(1, Duration::from_secs(1)), (2, Duration::from_secs(2))],
        "one frame per recorded second, not one per 50 ms tick"
    );

    // After the last frame the driver reports end of file and pauses.
    let state = playback.state.lock().await;
    let replay = state.replay.as_ref().unwrap();
    assert!(replay.at_eof, "the driver must notice the end of the file");
    assert!(replay.paused, "playback pauses at the end");
    assert_eq!(replay.current_seq, 2);
    assert_eq!(replay.elapsed, Duration::from_secs(2));
}

#[tokio::test(start_paused = true)]
async fn playback_at_speed_two_is_twice_as_fast() {
    let playback = play(2.0, false, Duration::from_secs(3)).await;
    assert_eq!(
        playback.shown_at,
        vec![
            (1, Duration::from_millis(500)),
            (2, Duration::from_millis(1_000))
        ]
    );
    assert!(playback.state.lock().await.replay.as_ref().unwrap().at_eof);
}

#[tokio::test(start_paused = true)]
async fn playback_loops_at_the_end_when_asked_to() {
    let playback = play(1.0, true, Duration::from_millis(4_060)).await;
    // 0 s frame 0, 1 s frame 1, 2 s frame 2, then the tick after that
    // finds end of file and restarts at frame 0, and it plays again.
    let seqs: Vec<u64> = playback.shown_at.iter().map(|(seq, _)| *seq).collect();
    assert_eq!(seqs, vec![1, 2, 0, 1, 2]);
    assert_eq!(playback.shown_at[2].1, Duration::from_millis(2_050));
    assert_eq!(playback.shown_at[3].1, Duration::from_millis(3_050));
    let state = playback.state.lock().await;
    assert!(!state.replay.as_ref().unwrap().paused);
}
