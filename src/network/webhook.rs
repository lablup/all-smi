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

//! Fire-and-forget webhook POST helper used by the threshold alerter.
//!
//! The caller owns a bounded `tokio::sync::mpsc` sender created by
//! [`spawn_webhook_worker`]. Each transition pushes one
//! [`WebhookPayload`] onto the channel; the worker drains and POSTs with a
//! 2-second timeout. Failures are logged via `tracing::warn` and dropped.
//! When the channel fills up, the caller uses `try_send` to drop the
//! oldest item rather than block rendering.

use std::time::Duration;

use reqwest::Client;
use tokio::sync::mpsc;

use crate::ui::alerts::WebhookPayload;

/// Bounded capacity of the webhook queue. Bigger than any realistic burst
/// (dozens of transitions per second across a 256-node cluster) but small
/// enough that a misconfigured webhook never starves memory.
pub const WEBHOOK_QUEUE_CAPACITY: usize = 64;

/// Per-request timeout. The requirement is to never block the UI; the
/// worker thread also enforces a body-level timeout here.
const WEBHOOK_TIMEOUT: Duration = Duration::from_secs(2);

/// Spawn a background worker that POSTs [`WebhookPayload`]s to `url` and
/// return the sender that the caller enqueues into.
///
/// The worker lives until the sender is dropped. When `url` is empty the
/// function still returns a sender but the worker silently drains without
/// making HTTP calls — this keeps the call sites branch-free.
pub fn spawn_webhook_worker(url: String) -> mpsc::Sender<WebhookPayload> {
    let (tx, mut rx) = mpsc::channel::<WebhookPayload>(WEBHOOK_QUEUE_CAPACITY);
    tokio::spawn(async move {
        // `Client::new` already applies sane defaults (rustls, HTTP/2). We
        // also set a per-call timeout via `.timeout()`, so a slow webhook
        // can't wedge the worker across many items.
        let client = match Client::builder().timeout(WEBHOOK_TIMEOUT).build() {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("alert-webhook: failed to build HTTP client: {e}");
                return;
            }
        };
        while let Some(payload) = rx.recv().await {
            if url.is_empty() {
                continue; // disabled — silently drain
            }
            match client.post(&url).json(&payload).send().await {
                Ok(resp) => {
                    if !resp.status().is_success() {
                        tracing::warn!("alert-webhook: {} responded {}", url, resp.status());
                    }
                }
                Err(e) => tracing::warn!("alert-webhook: POST to {url} failed: {e}"),
            }
        }
    });
    tx
}

/// Enqueue a payload on the worker channel using `try_send`. Drops the
/// payload when the channel is full (never blocks the UI).
///
/// Returns `true` when the payload was successfully enqueued, `false` when
/// the queue was full or the worker has exited. The caller may use this to
/// emit a `tracing::warn` on drop.
pub fn enqueue(tx: &mpsc::Sender<WebhookPayload>, payload: WebhookPayload) -> bool {
    match tx.try_send(payload) {
        Ok(()) => true,
        Err(mpsc::error::TrySendError::Full(_)) => {
            tracing::warn!("alert-webhook: queue full, dropping payload");
            false
        }
        Err(mpsc::error::TrySendError::Closed(_)) => {
            tracing::warn!("alert-webhook: worker channel closed, dropping payload");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::alerts::WebhookPayload;

    #[test]
    fn payload_json_contains_expected_fields() {
        let p = WebhookPayload {
            timestamp: "2026-04-20T00:00:00+00:00".to_string(),
            host: "n01".to_string(),
            gpu_index: Some(3),
            rule: "temperature".to_string(),
            from: "warn".to_string(),
            to: "crit".to_string(),
            value: 95.5,
            threshold: 90.0,
        };
        let j = serde_json::to_string(&p).unwrap();
        // Must contain all keys mentioned in the issue's "Body" snippet.
        assert!(j.contains("\"timestamp\":"));
        assert!(j.contains("\"host\":\"n01\""));
        assert!(j.contains("\"gpu_index\":3"));
        assert!(j.contains("\"rule\":\"temperature\""));
        assert!(j.contains("\"value\":95.5"));
        assert!(j.contains("\"threshold\":90"));
    }

    #[tokio::test]
    async fn enqueue_returns_true_on_empty_queue() {
        let tx = spawn_webhook_worker(String::new());
        let p = WebhookPayload {
            timestamp: "2026-04-20T00:00:00+00:00".to_string(),
            host: "n01".to_string(),
            gpu_index: None,
            rule: "temperature".to_string(),
            from: "ok".to_string(),
            to: "warn".to_string(),
            value: 85.0,
            threshold: 80.0,
        };
        assert!(enqueue(&tx, p));
    }
}
