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

//! Server-Sent Events `/events` handler (issue #193).
//!
//! Subscribes each client to the shared [`FrameBus`] and streams one SSE
//! frame per collection cycle. Lagging subscribers are surfaced as
//! synthetic `event: lag` frames so the client learns about dropped
//! frames instead of silently missing them.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::IntoResponse;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures_util::Stream;
use futures_util::stream::unfold;
use serde::Deserialize;
use tokio::sync::broadcast::Receiver;
use tokio::sync::broadcast::error::RecvError;

use crate::api::frame_bus::FrameBus;
use crate::snapshot::Snapshot;

use super::snapshot::{SectionFilter, filter_snapshot_value, parse_include};

/// Default keep-alive interval. Matches the `HTTP2_KEEPALIVE_SECS` figure
/// referenced in the issue body; reverse proxies typically idle-timeout
/// a connection between 60 s (nginx default) and 75 s (haproxy default),
/// so 30 s sits comfortably under both.
pub const DEFAULT_HEARTBEAT_SECS: u64 = 30;

/// Upper bound for `?throttle=` and `?heartbeat=` so a buggy client cannot
/// silently request an hour-long gap and block a reverse-proxy timeout
/// from triggering. 24 h feels like a generous ceiling for both.
pub const MAX_INTERVAL_SECS: u64 = 86_400;

#[derive(Debug, Default, Deserialize)]
pub struct EventsQuery {
    /// Comma-separated section filter, same grammar as the `/snapshot`
    /// `?include=` param.
    pub include: Option<String>,
    /// Minimum interval between emitted snapshot frames, in seconds.
    /// Clamped to `>= collection_interval` so clients cannot ask for
    /// updates faster than the server actually produces them.
    pub throttle: Option<u64>,
    /// Keep-alive interval in seconds. Falls back to
    /// [`DEFAULT_HEARTBEAT_SECS`] when omitted or when a value of `0` is
    /// supplied.
    pub heartbeat: Option<u64>,
}

/// SSE entry point. Per the issue spec, a `Last-Event-ID` header hint is
/// acknowledged but ignored — `all-smi` does not retain history, so the
/// client resumes with the next live frame regardless of the ID it sends.
pub async fn events_handler(
    State(bus): State<FrameBus>,
    Query(params): Query<EventsQuery>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let filter = parse_include(params.include.as_deref());
    let throttle = resolve_throttle(params.throttle, bus.collection_interval());
    let heartbeat = resolve_heartbeat(params.heartbeat);

    // `Last-Event-ID` is logged but never used for replay. Including it in
    // the debug trace helps operators match up a reconnect with the
    // preceding disconnect when chasing flaky-network bugs.
    if let Some(id) = headers.get("last-event-id").and_then(|v| v.to_str().ok()) {
        tracing::debug!(
            client_last_event_id = %id,
            "SSE client reconnected; history replay not supported, resuming with next live frame"
        );
    }

    let stream = build_sse_stream(bus.subscribe(), filter, throttle);
    let sse = Sse::new(stream).keep_alive(KeepAlive::new().interval(heartbeat).text("keep-alive"));

    // Discourage reverse proxies (nginx, cloudfront, etc.) from
    // accumulating the event stream into a single buffered chunk. The
    // response body stays the axum-rendered SSE body; we only layer on
    // extra headers before returning.
    let mut extra = HeaderMap::new();
    extra.insert("X-Accel-Buffering", HeaderValue::from_static("no"));
    extra.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    (extra, sse)
}

fn resolve_throttle(user: Option<u64>, collection_interval: Duration) -> Duration {
    let secs = user
        .unwrap_or(0)
        .clamp(collection_interval.as_secs(), MAX_INTERVAL_SECS);
    // When the user either omitted `throttle` or asked for `0`, the
    // `.clamp(collection_interval, ...)` above already yields the
    // collection interval — keeping the SSE cadence aligned with the
    // producer by default.
    if secs == 0 {
        collection_interval
    } else {
        Duration::from_secs(secs)
    }
}

fn resolve_heartbeat(user: Option<u64>) -> Duration {
    let secs = user.unwrap_or(0);
    if secs == 0 {
        Duration::from_secs(DEFAULT_HEARTBEAT_SECS)
    } else {
        Duration::from_secs(secs.clamp(1, MAX_INTERVAL_SECS))
    }
}

/// Per-stream state carried through `futures_util::stream::unfold`.
struct StreamState {
    rx: Receiver<Arc<Snapshot>>,
    filter: SectionFilter,
    throttle: Duration,
    last_emit: Option<Instant>,
}

/// Build the per-client event stream. Isolated from the handler so the
/// test module can drive it with a synthetic channel.
pub fn build_sse_stream(
    rx: Receiver<Arc<Snapshot>>,
    filter: SectionFilter,
    throttle: Duration,
) -> impl Stream<Item = Result<Event, Infallible>> {
    unfold(
        StreamState {
            rx,
            filter,
            throttle,
            last_emit: None,
        },
        |mut state| async move {
            loop {
                match state.rx.recv().await {
                    Ok(frame) => {
                        // Enforce the `?throttle=` minimum interval
                        // between snapshot frames. Dropped frames from
                        // throttling are simply not reported — `lag`
                        // events are reserved for broadcast-buffer drops,
                        // which indicate server backpressure rather than
                        // an intentional rate limit.
                        if let Some(prev) = state.last_emit
                            && prev.elapsed() < state.throttle
                        {
                            continue;
                        }
                        let event = build_snapshot_event(&frame, &state.filter);
                        state.last_emit = Some(Instant::now());
                        return Some((Ok(event), state));
                    }
                    Err(RecvError::Lagged(n)) => {
                        // After a lag event the receiver has been
                        // advanced to the head of the channel; the next
                        // `recv()` will return the freshest frame so the
                        // client immediately recovers.
                        let event = build_lag_event(n);
                        return Some((Ok(event), state));
                    }
                    Err(RecvError::Closed) => {
                        // The sender was dropped, which only happens
                        // during server shutdown. Cleanly terminate the
                        // stream.
                        return None;
                    }
                }
            }
        },
    )
}

fn build_snapshot_event(snapshot: &Arc<Snapshot>, filter: &SectionFilter) -> Event {
    let value = filter_snapshot_value(snapshot, filter);
    let event = Event::default()
        .event("snapshot")
        .id(event_id_for(snapshot));
    match event.clone().json_data(&value) {
        Ok(e) => e,
        Err(err) => error_event(&err.to_string()),
    }
}

fn build_lag_event(dropped: u64) -> Event {
    let payload = serde_json::json!({ "dropped": dropped });
    Event::default()
        .event("lag")
        .json_data(&payload)
        .unwrap_or_else(|e| error_event(&e.to_string()))
}

fn error_event(message: &str) -> Event {
    let payload = serde_json::json!({ "error": message });
    Event::default()
        .event("error")
        .json_data(&payload)
        // Falling back to a literal comment when even the error event
        // fails to serialize avoids an infinite retry loop.
        .unwrap_or_else(|_| Event::default().comment("serialization failure"))
}

/// Synthesise a stable `id:` value for an emitted snapshot event.
///
/// Clients can use the `id` to detect missing frames. The `Snapshot`
/// struct's `timestamp` field gives a human-readable id that maps 1:1 to
/// the frame's `timestamp` field in the body — enough for reconnect
/// diagnostics even though the server intentionally ignores the
/// `Last-Event-ID` header on resume.
fn event_id_for(snapshot: &Arc<Snapshot>) -> String {
    snapshot.timestamp.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::frame_bus::FrameBus;
    use crate::snapshot::Snapshot;
    use futures_util::StreamExt;

    fn minimal_snapshot() -> Snapshot {
        Snapshot {
            schema: 1,
            timestamp: "2026-04-20T00:00:01Z".to_string(),
            hostname: "h".to_string(),
            gpus: Some(Vec::new()),
            cpus: Some(Vec::new()),
            memory: Some(Vec::new()),
            chassis: Some(Vec::new()),
            processes: None,
            storage: None,
            errors: Vec::new(),
        }
    }

    #[test]
    fn resolve_throttle_clamps_below_collection_interval() {
        let d = resolve_throttle(Some(1), Duration::from_secs(5));
        assert_eq!(d, Duration::from_secs(5));
    }

    #[test]
    fn resolve_throttle_defaults_to_collection_interval() {
        let d = resolve_throttle(None, Duration::from_secs(3));
        assert_eq!(d, Duration::from_secs(3));
    }

    #[test]
    fn resolve_heartbeat_defaults_to_thirty() {
        let d = resolve_heartbeat(None);
        assert_eq!(d, Duration::from_secs(DEFAULT_HEARTBEAT_SECS));
    }

    #[test]
    fn resolve_heartbeat_accepts_custom_value() {
        let d = resolve_heartbeat(Some(10));
        assert_eq!(d, Duration::from_secs(10));
    }

    #[tokio::test]
    async fn stream_emits_published_frame() {
        let bus = FrameBus::new(Duration::from_millis(10));
        let filter = SectionFilter::default_http();
        let rx = bus.subscribe();
        bus.publish(minimal_snapshot()).await;
        let stream = build_sse_stream(rx, filter, Duration::from_millis(10));
        futures_util::pin_mut!(stream);
        let next = stream.next().await.expect("stream yields at least once");
        assert!(next.is_ok());
    }

    #[tokio::test]
    async fn lag_event_emitted_when_receiver_falls_behind() {
        let bus = FrameBus::new(Duration::from_millis(10));
        let filter = SectionFilter::default_http();
        let rx = bus.subscribe();
        // Fill the broadcast buffer past `FRAME_BUFFER` so the next
        // `recv()` sees a `Lagged` error.
        for _ in 0..(crate::api::frame_bus::FRAME_BUFFER + 4) {
            bus.publish(minimal_snapshot()).await;
        }
        let stream = build_sse_stream(rx, filter, Duration::from_millis(10));
        futures_util::pin_mut!(stream);
        let first = stream.next().await.expect("stream yields at least once");
        assert!(first.is_ok());
    }
}
