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
//!   * forwards the same `Arc`, with its sequence number, through a
//!     [`tokio::sync::broadcast::channel`] so every live SSE subscriber
//!     receives it.
//! * The JSON body of a frame is the same for every client that asks for
//!   the same sections, so [`FrameBus::body_for`] serializes it once per
//!   `(filter, pretty)` and hands every client the same `Arc<str>` (issue
//!   #503).
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

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::broadcast;

use crate::api::handlers::snapshot::{SectionFilter, serialize_filtered};
use crate::api::snapshot_fallback::SnapshotFallback;
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
    sender: broadcast::Sender<LatestFrame>,
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
    /// Serialized bodies of the newest frame, one per `(filter, pretty)`.
    bodies: Mutex<FrameBodyCache>,
    /// State of the `/snapshot` fresh-collect fallback: its single-flight
    /// lock, the collector it builds once, and its last result. See
    /// [`SnapshotFallback`].
    fallback: SnapshotFallback,
}

/// Serialized JSON bodies for one frame.
///
/// Keyed by `(filter, pretty)`: the filter is six booleans, so at most 128
/// entries exist, and all of them are dropped when a newer frame arrives.
///
/// Each entry is a per-key cell. The map lock is held only to fetch or
/// insert a cell; serialization runs under the cell's own lock, so callers
/// with different keys serialize in parallel while callers with the same
/// key wait for one shared result.
#[derive(Default)]
struct FrameBodyCache {
    seq: u64,
    bodies: HashMap<(SectionFilter, bool), BodyCell>,
}

/// One cached body, filled by the first caller for its key.
type BodyCell = Arc<Mutex<Option<Arc<str>>>>;

/// A published frame together with the wall-clock instant at which it was
/// captured. The instant drives the `/snapshot` staleness check; the
/// sequence number keys the per-frame body cache.
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
        Self::with_fallback(collection_interval, SnapshotFallback::default())
    }

    /// A bus whose `/snapshot` fallback builds its collector with
    /// `factory` instead of the real device readers.
    #[cfg(test)]
    pub(crate) fn with_collector_factory(
        collection_interval: Duration,
        factory: crate::api::snapshot_fallback::CollectorFactory,
    ) -> Self {
        Self::with_fallback(collection_interval, SnapshotFallback::new(factory))
    }

    fn with_fallback(collection_interval: Duration, fallback: SnapshotFallback) -> Self {
        let (sender, _rx) = broadcast::channel(FRAME_BUFFER);
        Self {
            inner: Arc::new(Inner {
                sender,
                latest: tokio::sync::RwLock::new(None),
                next_seq: AtomicU64::new(1),
                collection_interval,
                bodies: Mutex::new(FrameBodyCache::default()),
                fallback,
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
        let frame = LatestFrame {
            snapshot: Arc::new(snapshot),
            published_at: Instant::now(),
            seq,
        };
        {
            let mut guard = self.inner.latest.write().await;
            *guard = Some(frame.clone());
        }
        // A `broadcast::Sender::send` returns `Err` only when every
        // receiver has been dropped. That is the "no clients connected"
        // steady state and is expected, so it must not bubble up.
        let _ = self.inner.sender.send(frame);
        seq
    }

    /// Subscribe a new receiver. Each SSE stream owns exactly one
    /// receiver; when the stream ends the receiver is dropped and the
    /// broadcast channel reclaims its slot. Frames arrive with their
    /// sequence number so the receiver can ask [`Self::body_for`] for the
    /// shared serialized body.
    pub fn subscribe(&self) -> broadcast::Receiver<LatestFrame> {
        self.inner.sender.subscribe()
    }

    /// The JSON body of a published frame for one `(filter, pretty)`.
    ///
    /// Serialized at most once per frame and key: every SSE subscriber and
    /// every `/snapshot` request with the same filter shares one
    /// `Arc<str>`, where each used to convert and serialize the whole
    /// snapshot for itself. The cache holds the newest frame only; a
    /// subscriber that is still working through an older frame gets a
    /// one-off serialization that does not disturb it.
    ///
    /// The global cache lock is held only to fetch or insert the key's
    /// cell. Serialization happens under that cell's lock, so concurrent
    /// callers for the same key wait for one result instead of each
    /// producing their own, while callers for other keys are not blocked.
    pub fn body_for(
        &self,
        frame: &LatestFrame,
        filter: &SectionFilter,
        pretty: bool,
    ) -> Result<Arc<str>, serde_json::Error> {
        let cell = {
            let mut cache = lock_ignoring_poison(&self.inner.bodies);
            if frame.seq < cache.seq {
                // Not cached and never will be: one-off serialization that
                // does not touch the cache.
                drop(cache);
                return serialize_filtered(&frame.snapshot, filter, pretty).map(Arc::from);
            }
            if frame.seq > cache.seq {
                cache.bodies.clear();
                cache.seq = frame.seq;
            }
            cache.bodies.entry((*filter, pretty)).or_default().clone()
        };
        let mut slot = lock_ignoring_poison(&cell);
        if let Some(body) = slot.as_ref() {
            return Ok(body.clone());
        }
        let body: Arc<str> = serialize_filtered(&frame.snapshot, filter, pretty)?.into();
        *slot = Some(body.clone());
        Ok(body)
    }

    /// State of the `/snapshot` fresh-collect fallback.
    pub(crate) fn fallback(&self) -> &SnapshotFallback {
        &self.inner.fallback
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
    /// Without this, a burst of `/snapshot` requests against a
    /// freshly-started server (no cached frame yet) or a stalled collector
    /// (cached frame older than `2 * interval`) would each run their own
    /// hardware read and saturate the Tokio blocking pool in parallel.
    ///
    /// Returns a guard; while held, any other task attempting to acquire
    /// the same guard will wait. Callers should acquire, re-read
    /// [`Self::latest`] and the fallback's last result (either may have
    /// been refreshed while waiting), and only collect if both are still
    /// stale. See `api::handlers::snapshot::resolve_snapshot` for the
    /// usage pattern.
    pub async fn lock_fresh_collect(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.inner.fallback.lock().await
    }

    /// The next sequence number that would be assigned. Exposed so tests
    /// and handlers can reason about the channel state; publishers must
    /// continue to use [`Self::publish`].
    #[cfg(test)]
    pub fn next_seq(&self) -> u64 {
        self.inner.next_seq.load(Ordering::Relaxed)
    }
}

/// Lock a plain-data mutex; a panic while it was held cannot leave it in a
/// state worth refusing to read.
fn lock_ignoring_poison<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

#[cfg(test)]
#[path = "frame_bus_tests.rs"]
mod tests;
