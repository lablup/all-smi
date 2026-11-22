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

//! One-shot `/snapshot` JSON handler (issue #193).
//!
//! Serves the most recent frame published through the shared
//! [`FrameBus`]. If the last frame is older than `2 × collection_interval`
//! (for example, the background collector is hung or the server just
//! started and no cycle has completed yet), the handler waits up to one
//! interval for the next publish and, failing that, falls back to a fresh
//! collection so the response never silently serves stale data.

use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use tokio::sync::broadcast::Receiver;
use tokio::sync::broadcast::error::RecvError;

use crate::api::frame_bus::{FrameBus, LatestFrame};
use crate::snapshot::{SNAPSHOT_SCHEMA_VERSION, Snapshot};

#[cfg(test)]
pub(crate) use super::snapshot_filter::SERIALIZE_COUNT;
// `filter_snapshot_value` stays exported for library callers; the binary
// target, whose module tree is private, has no use for it.
#[allow(unused_imports)]
pub use super::snapshot_filter::filter_snapshot_value;
pub use super::snapshot_filter::{SectionFilter, parse_include, serialize_filtered};

/// Per-reader timeout used when `/snapshot` forces a fresh collection.
/// Matches the CLI `snapshot --timeout-ms` default so operators see the
/// same behaviour across both surfaces.
const FRESH_COLLECT_TIMEOUT: Duration = Duration::from_millis(5_000);

#[derive(Debug, Default, Deserialize)]
pub struct SnapshotQuery {
    /// Comma-separated section filter. Accepts the same names as the CLI
    /// `snapshot --include` flag: `gpu,cpu,memory,chassis,process,storage`.
    /// Unknown names are silently ignored so a client typo does not
    /// surface as a 400.
    pub include: Option<String>,
    /// Pretty-print the JSON body. `?pretty=1` / `?pretty=true` enable;
    /// anything else (including the absence of the param) disables.
    pub pretty: Option<String>,
}

pub async fn snapshot_handler(
    State(bus): State<FrameBus>,
    Query(params): Query<SnapshotQuery>,
) -> Response {
    let filter = parse_include(params.include.as_deref());
    let pretty = matches!(
        params.pretty.as_deref(),
        Some("1") | Some("true") | Some("yes")
    );

    let resolved = match resolve_snapshot(&bus, &filter).await {
        Ok(s) => s,
        Err(err) => {
            tracing::warn!("/snapshot: fresh collect failed: {err}");
            return error_response(StatusCode::SERVICE_UNAVAILABLE, &err);
        }
    };

    let body = match &resolved {
        // A published frame: the body is shared with every other client
        // that asks for the same sections of the same frame.
        Resolved::Frame(frame) => bus.body_for(frame, &filter, pretty),
        // A fallback snapshot has no sequence number to cache under.
        Resolved::Fallback(snapshot) => {
            serialize_filtered(snapshot, &filter, pretty).map(Arc::from)
        }
    };
    let body = match body {
        Ok(body) => body,
        Err(err) => {
            tracing::warn!("/snapshot: JSON serialization failed: {err}");
            return error_response(StatusCode::INTERNAL_SERVER_ERROR, &err.to_string());
        }
    };

    let mut headers = no_cache_headers();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );

    (StatusCode::OK, headers, String::from(&*body)).into_response()
}

/// Common no-cache header set shared between the success and error
/// responses. Without them a reverse proxy can silently cache a transient
/// `/snapshot` failure and keep serving the stale error to subsequent
/// clients — the SSE spec already bans this on `/events` and we mirror it
/// here for symmetry (issue #193).
fn no_cache_headers() -> HeaderMap {
    let mut h = HeaderMap::new();
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert("X-Accel-Buffering", HeaderValue::from_static("no"));
    h
}

fn error_response(status: StatusCode, message: &str) -> Response {
    let body = Json(serde_json::json!({
        "schema": SNAPSHOT_SCHEMA_VERSION,
        "error": message,
    }));
    (status, no_cache_headers(), body).into_response()
}

/// What `/snapshot` serves: a frame the collection loop published, or the
/// result of the fresh-collect fallback.
pub(crate) enum Resolved {
    Frame(LatestFrame),
    Fallback(Arc<Snapshot>),
}

impl Resolved {
    #[cfg(test)]
    pub(crate) fn snapshot(&self) -> &Arc<Snapshot> {
        match self {
            Resolved::Frame(frame) => &frame.snapshot,
            Resolved::Fallback(snapshot) => snapshot,
        }
    }
}

/// The latest published frame, unless it is older than
/// `2 × collection_interval` (or nothing has been published yet).
async fn fresh_frame(bus: &FrameBus) -> Option<LatestFrame> {
    let stale_after = bus.collection_interval().saturating_mul(2);
    bus.latest()
        .await
        .filter(|frame| frame.published_at.elapsed() <= stale_after)
}

/// Wait up to `interval` for the next frame on `rx`.
async fn next_publish(rx: &mut Receiver<LatestFrame>, interval: Duration) -> Option<LatestFrame> {
    let recv = async {
        loop {
            match rx.recv().await {
                Ok(frame) => return Some(frame),
                // Frames were dropped from this receiver's buffer; the
                // next `recv` returns the newest one still held.
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => return None,
            }
        }
    };
    tokio::time::timeout(interval, recv).await.ok().flatten()
}

/// Resolve the frame served to the caller.
///
/// 1. The last published frame, when it is fresh.
/// 2. A recent fallback result that covers the requested sections.
/// 3. The next frame the collection loop publishes, waiting up to one
///    collection interval. At startup the first tick is usually moments
///    away, and waiting for it costs no hardware read at all. The
///    receiver is subscribed *before* `latest()` is re-read so a publish
///    between the two cannot be missed.
/// 4. A fresh collection honouring the caller's `?include=` filter, so the
///    fallback is observationally indistinguishable from the cached path
///    (issue #193). It runs under a single-flight lock, through a
///    collector built once per process on the blocking pool, and its
///    result is kept for requests that arrive within the next interval
///    (issue #503). The fallback snapshot is not published on the bus.
pub(crate) async fn resolve_snapshot(
    bus: &FrameBus,
    filter: &SectionFilter,
) -> Result<Resolved, String> {
    let interval = bus.collection_interval();
    if let Some(frame) = fresh_frame(bus).await {
        return Ok(Resolved::Frame(frame));
    }
    if let Some(snapshot) = bus.fallback().cached(filter, interval) {
        return Ok(Resolved::Fallback(snapshot));
    }

    {
        let mut rx = bus.subscribe();
        if let Some(frame) = fresh_frame(bus).await {
            return Ok(Resolved::Frame(frame));
        }
        if let Some(frame) = next_publish(&mut rx, interval).await {
            return Ok(Resolved::Frame(frame));
        }
    }

    // Serialise fresh collects with a single-flight lock. A burst of
    // `/snapshot` requests against a server whose collection loop has
    // stalled would otherwise each issue their own hardware read and
    // saturate the Tokio blocking pool — an amplification attack on an
    // otherwise cheap HTTP endpoint. Re-checking under the lock means a
    // winning collector's output is observed by every queued caller
    // without any of them reading hardware again.
    let _guard = bus.lock_fresh_collect().await;
    if let Some(frame) = fresh_frame(bus).await {
        return Ok(Resolved::Frame(frame));
    }
    if let Some(snapshot) = bus.fallback().cached(filter, interval) {
        return Ok(Resolved::Fallback(snapshot));
    }
    bus.fallback()
        .collect(filter, FRESH_COLLECT_TIMEOUT)
        .await
        .map(Resolved::Fallback)
}

#[cfg(test)]
#[path = "snapshot_tests.rs"]
mod tests;
