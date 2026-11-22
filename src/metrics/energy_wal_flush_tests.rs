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

//! Tests for the WAL flush cycle (issue #503): lifetime totals are only
//! collected when the file is due for compaction, and compaction keeps the
//! replayed totals exact.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::RwLock;

use super::{LIFETIME_SNAPSHOTS, WAL_MAX_BYTES, WalWriter, flush_cycle, replay_from_path};
use crate::app_state::AppState;
use crate::metrics::energy::{EnergyKey, PowerIntegrator};

type Shared = Arc<RwLock<AppState>>;

fn key() -> EnergyKey {
    EnergyKey::gpu("test-host", "uuid-flush")
}

/// Integrate `watts` for ten seconds on top of whatever the device has.
async fn add_ten_seconds(state: &Shared, origin: Instant, step: u32, watts: f64) {
    let mut s = state.write().await;
    let at = origin + Duration::from_secs(10) * step;
    s.energy.integrator_mut().record_sample(key(), at, watts);
}

fn replayed_total(path: &std::path::Path) -> f64 {
    let mut integ = PowerIntegrator::default();
    let index = replay_from_path(path, &mut integ).expect("replay");
    index
        .lookup(key().host_hash(), key().device_hash())
        .unwrap_or(0.0)
}

fn snapshots() -> usize {
    LIFETIME_SNAPSHOTS.with(|count| count.get())
}

#[tokio::test(flavor = "current_thread")]
async fn a_small_wal_never_collects_lifetime_totals() {
    let dir = tempfile::tempdir().unwrap();
    let wal_path = dir.path().join("small.bin");
    let state: Shared = Arc::new(RwLock::new(AppState::new()));
    let origin = Instant::now();
    add_ten_seconds(&state, origin, 0, 100.0).await;
    add_ten_seconds(&state, origin, 1, 100.0).await;

    let before = snapshots();
    let writer = WalWriter::open(&wal_path).unwrap();
    let writer = flush_cycle(writer, &wal_path, &state).await;
    add_ten_seconds(&state, origin, 2, 100.0).await;
    // Close the writer before replaying: on Windows it holds the file with
    // `share_mode(0)`, so a second open fails with a sharing violation.
    drop(flush_cycle(writer, &wal_path, &state).await);

    assert_eq!(
        snapshots() - before,
        0,
        "a WAL below the compaction threshold must not snapshot lifetime totals"
    );
    assert!((replayed_total(&wal_path) - 2_000.0).abs() < 1e-6);
}

#[tokio::test(flavor = "current_thread")]
async fn a_wal_above_the_threshold_snapshots_once_and_compacts_exactly() {
    let dir = tempfile::tempdir().unwrap();
    let wal_path = dir.path().join("large.bin");
    // A sparse file just past the threshold. Its zeroed records carry no
    // energy, so the device total is what this test integrates.
    {
        let file = std::fs::File::create(&wal_path).unwrap();
        file.set_len(WAL_MAX_BYTES + 24).unwrap();
    }

    let state: Shared = Arc::new(RwLock::new(AppState::new()));
    let origin = Instant::now();
    add_ten_seconds(&state, origin, 0, 100.0).await;
    add_ten_seconds(&state, origin, 1, 100.0).await;

    let before = snapshots();
    let writer = WalWriter::open(&wal_path).unwrap();
    drop(flush_cycle(writer, &wal_path, &state).await);
    assert_eq!(snapshots() - before, 1, "compaction takes one snapshot");

    let size = std::fs::metadata(&wal_path).unwrap().len();
    assert!(
        size < 1024,
        "compaction must rewrite the file to one record per device, got {size} bytes"
    );
    assert!((replayed_total(&wal_path) - 1_000.0).abs() < 1e-6);

    // The next cycle is back under the threshold: no further snapshot, and
    // the new delta lands once on top of the compacted record.
    add_ten_seconds(&state, origin, 2, 100.0).await;
    let writer = WalWriter::open(&wal_path).unwrap();
    drop(flush_cycle(writer, &wal_path, &state).await);
    assert_eq!(snapshots() - before, 1);
    assert!((replayed_total(&wal_path) - 2_000.0).abs() < 1e-6);
}
