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

//! Broadcast bus that fans out one `Snapshot` per collection cycle to every
//! SSE client and the on-demand `/snapshot` handler (issue #193).
//!
//! Design (see the issue's "Broadcast architecture" block):
//!
//! * Exactly one collection task is spawned by `api::server::run_api_mode`.
//!   Each tick produces one `Arc<Snapshot>` and calls
//!   [`FrameBus::publish`], which:
//!   * stores the frame in an internal `RwLock` so `/snapshot` can read it
//!     without blocking the collection loop, and
//!   * forwards the same `Arc` through a
//!     [`tokio::sync::broadcast::channel`] so every live SSE subscriber
//!     receives it.
//! * Each SSE client obtains its own `broadcast::Receiver` through
//!   [`FrameBus::subscribe`]. The channel buffer is intentionally small
//!   (16 frames ≈ 48 s at the default 3 s interval) so a client that
//!   falls behind visibly lags rather than forcing the server to
//!   accumulate unbounded memory — broadcast semantics drop the oldest
//!   frame once the buffer fills.
//! * The collection task never awaits a client. Sending to a
//!   `broadcast::Sender` is non-blocking: it returns `Err(SendError)` only
//!   if there are no receivers, which is the no-client steady state. That
//!   error case is deliberately ignored.
//!
//! The [`FrameBus`] instance is `Clone` (cheap — one `Arc<Inner>`) and is
//! passed to the axum router via `.with_state(...)`. Handlers extract it
//! with `axum::extract::State<FrameBus>`.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use tokio::sync::broadcast;

use crate::snapshot::Snapshot;

/// Number of `Arc<Snapshot>` frames buffered in the broadcast channel.
///
/// Capped intentionally small: a client that drops behind by 16 frames is
/// already lagging badly (≥48 s at the default 3 s interval), and the
/// design goal is "no unbounded memory growth" rather than "no dropped
/// frames". Clients see the gap through the synthesised `event: lag`
/// emitted by the SSE handler.
pub const FRAME_BUFFER: usize = 16;

/// Broadcast bus handed to the axum router as shared state.
///
/// Cloning a `FrameBus` is cheap — every clone shares the same inner
/// [`broadcast::Sender`] and the same `RwLock<Option<LatestFrame>>`. That
/// means every route handler sees the most recent frame and every new
/// SSE subscriber picks up from the next live publish.
#[derive(Clone)]
pub struct FrameBus {
    inner: Arc<Inner>,
}

struct Inner {
    /// Broadcast channel fanning out every collection cycle. Receivers
    /// are created lazily by [`FrameBus::subscribe`].
    sender: broadcast::Sender<Arc<Snapshot>>,
    /// Last published frame (or `None` before the first collection cycle
    /// returns). Stored separately from the broadcast channel so the
    /// `/snapshot` handler can reply without subscribing.
    latest: tokio::sync::RwLock<Option<LatestFrame>>,
    /// Monotonically increasing sequence number stamped onto each frame
    /// before it leaves [`FrameBus::publish`]. Used for SSE `id:` values.
    next_seq: AtomicU64,
    /// Collection interval reported by the caller. The SSE handler clamps
    /// `?throttle=N` against this so clients cannot ask for updates
    /// faster than the server actually produces them.
    collection_interval: Duration,
    /// Single-flight lock serializing the `/snapshot` fresh-collect
    /// fallback path. Without this, a burst of `/snapshot` requests
    /// against a freshly-started server (no cached frame yet) or a
    /// stalled collector (cached frame older than `2 * interval`) would
    /// each spawn their own `DefaultSnapshotCollector` and saturate the
    /// Tokio blocking pool in parallel. Serializing fresh collects means
    /// at most one is in flight at a time; late-arriving requests block
    /// briefly, then see the newly cached frame without doing their own
    /// hardware read. See `api::handlers::snapshot::resolve_snapshot`.
    fresh_collect_lock: tokio::sync::Mutex<()>,
}

/// A published frame together with the wall-clock instant at which it was
/// captured. The instant drives the `/snapshot` staleness check.
#[derive(Clone)]
pub struct LatestFrame {
    pub snapshot: Arc<Snapshot>,
    pub published_at: Instant,
    pub seq: u64,
}

impl FrameBus {
    /// Construct a new bus with the given `collection_interval`. Callers
    /// from the HTTP server pass `Duration::from_secs(interval)` where
    /// `interval` is the merged CLI / config value.
    pub fn new(collection_interval: Duration) -> Self {
        let (sender, _rx) = broadcast::channel(FRAME_BUFFER);
        Self {
            inner: Arc::new(Inner {
                sender,
                latest: tokio::sync::RwLock::new(None),
                next_seq: AtomicU64::new(1),
                collection_interval,
                fresh_collect_lock: tokio::sync::Mutex::new(()),
            }),
        }
    }

    /// Publish one collected snapshot. Stores it as "latest" for the
    /// `/snapshot` handler and fans it out to every SSE subscriber.
    ///
    /// Returns the sequence number stamped on the frame so callers can
    /// emit structured logs correlated with the SSE `id:` field.
    pub async fn publish(&self, snapshot: Snapshot) -> u64 {
        let seq = self.inner.next_seq.fetch_add(1, Ordering::Relaxed);
        let arc = Arc::new(snapshot);
        {
            let mut guard = self.inner.latest.write().await;
            *guard = Some(LatestFrame {
                snapshot: arc.clone(),
                published_at: Instant::now(),
                seq,
            });
        }
        // A `broadcast::Sender::send` returns `Err` only when every
        // receiver has been dropped. That is the "no clients connected"
        // steady state and is expected, so it must not bubble up.
        let _ = self.inner.sender.send(arc);
        seq
    }

    /// Subscribe a new receiver. Each SSE stream owns exactly one
    /// receiver; when the stream ends the receiver is dropped and the
    /// broadcast channel reclaims its slot.
    pub fn subscribe(&self) -> broadcast::Receiver<Arc<Snapshot>> {
        self.inner.sender.subscribe()
    }

    /// Current number of live SSE subscribers. Used by the test suite to
    /// verify that dropped connections actually release their slot; the
    /// operator-facing log line also includes it.
    pub fn subscriber_count(&self) -> usize {
        self.inner.sender.receiver_count()
    }

    /// Read the most recent published frame without blocking the
    /// collection loop. `None` before the first cycle completes.
    pub async fn latest(&self) -> Option<LatestFrame> {
        self.inner.latest.read().await.clone()
    }

    /// Collection interval used to clamp `?throttle=N` and to compute the
    /// `/snapshot` staleness threshold.
    pub fn collection_interval(&self) -> Duration {
        self.inner.collection_interval
    }

    /// Acquire the single-flight lock for a `/snapshot` fresh collect.
    ///
    /// Returns a guard; while held, any other task attempting to acquire
    /// the same guard will wait. The lock is not held across the
    /// collection itself in the caller — callers should acquire, re-read
    /// [`Self::latest`] (it may have been refreshed while waiting), and
    /// only perform the collection if the frame is still stale. See
    /// `api::handlers::snapshot::resolve_snapshot` for the usage pattern.
    pub async fn lock_fresh_collect(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.inner.fresh_collect_lock.lock().await
    }

    /// The next sequence number that would be assigned. Exposed so tests
    /// and handlers can reason about the channel state; publishers must
    /// continue to use [`Self::publish`].
    #[cfg(test)]
    pub fn next_seq(&self) -> u64 {
        self.inner.next_seq.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::Snapshot;

    fn make_snapshot(host: &str) -> Snapshot {
        Snapshot {
            schema: 1,
            timestamp: "2026-04-20T00:00:00Z".to_string(),
            hostname: host.to_string(),
            gpus: None,
            cpus: None,
            memory: None,
            chassis: None,
            processes: None,
            storage: None,
            errors: Vec::new(),
        }
    }

    #[tokio::test]
    async fn publish_updates_latest_and_increments_seq() {
        let bus = FrameBus::new(Duration::from_secs(3));
        assert!(bus.latest().await.is_none());
        let seq = bus.publish(make_snapshot("h1")).await;
        assert_eq!(seq, 1);
        let latest = bus.latest().await.expect("latest must be present");
        assert_eq!(latest.snapshot.hostname, "h1");
        assert_eq!(latest.seq, 1);
        let seq2 = bus.publish(make_snapshot("h2")).await;
        assert_eq!(seq2, 2);
    }

    #[tokio::test]
    async fn subscribers_receive_published_frames() {
        let bus = FrameBus::new(Duration::from_secs(3));
        let mut rx = bus.subscribe();
        assert_eq!(bus.subscriber_count(), 1);
        bus.publish(make_snapshot("rx-host")).await;
        let frame = rx.recv().await.expect("receiver must get the frame");
        assert_eq!(frame.hostname, "rx-host");
    }

    #[tokio::test]
    async fn dropping_receiver_releases_slot() {
        let bus = FrameBus::new(Duration::from_secs(3));
        let rx = bus.subscribe();
        assert_eq!(bus.subscriber_count(), 1);
        drop(rx);
        assert_eq!(bus.subscriber_count(), 0);
    }

    /// Regression for the security review of #193: the fresh-collect
    /// lock must serialize concurrent `/snapshot` callers so a burst
    /// cannot saturate the Tokio blocking pool. We cannot easily assert
    /// "only one collect ran" without touching hardware, but we can
    /// observe that a task holding the guard blocks a second caller
    /// until the guard drops.
    #[tokio::test]
    async fn fresh_collect_lock_serializes_callers() {
        let bus = FrameBus::new(Duration::from_secs(3));
        let guard = bus.lock_fresh_collect().await;

        // Spawn a second acquirer and race it with a short sleep. If the
        // lock is serializing correctly, the spawned task will not
        // resolve until we drop `guard` below.
        let bus2 = bus.clone();
        let handle = tokio::spawn(async move {
            let _g = bus2.lock_fresh_collect().await;
        });

        // Give the second task a moment to try to acquire.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(
            !handle.is_finished(),
            "second lock_fresh_collect caller should block while first guard is held"
        );

        drop(guard);
        handle
            .await
            .expect("second caller must complete once the guard is released");
    }
}
