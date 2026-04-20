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
//! When the channel is saturated, the caller uses `try_send` to drop the
//! **newest** payload rather than block rendering — the UI-never-blocks
//! invariant is what matters here, not FIFO preservation.
//!
//! # Security
//!
//! The webhook worker disables HTTP redirects (`Policy::none()`) to avoid
//! an SSRF pivot where an attacker-controlled webhook returns a 3xx
//! pointing at a cloud metadata endpoint (e.g. `169.254.169.254`) or other
//! internal-only service. Operators explicitly configure the exact URL and
//! we refuse to follow redirects away from it.
//!
//! URLs logged on failure are passed through [`redact_url_userinfo`] so
//! that a misconfigured webhook containing Basic-Auth credentials does not
//! spill them into log aggregators.

use std::time::Duration;

use reqwest::{Client, redirect::Policy};
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
///
/// # Security
///
/// Redirects are disabled (`Policy::none()`). If an operator configures
/// `https://hook.example.com/alerts` and that server returns a 302 to
/// `http://169.254.169.254/latest/meta-data/...`, reqwest returns the 3xx
/// response verbatim rather than fetching the redirect target. This keeps
/// the webhook feature from becoming an SSRF primitive on clouds that
/// expose metadata services on link-local addresses.
pub fn spawn_webhook_worker(url: String) -> mpsc::Sender<WebhookPayload> {
    let (tx, mut rx) = mpsc::channel::<WebhookPayload>(WEBHOOK_QUEUE_CAPACITY);
    tokio::spawn(async move {
        // Explicitly disable redirects: the operator configures the exact
        // destination, and we refuse to chase server-side 3xx responses
        // that could pivot to internal services (SSRF).
        let client = match Client::builder()
            .timeout(WEBHOOK_TIMEOUT)
            .redirect(Policy::none())
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("alert-webhook: failed to build HTTP client: {e}");
                return;
            }
        };
        let safe_url = redact_url_userinfo(&url);
        while let Some(payload) = rx.recv().await {
            if url.is_empty() {
                continue; // disabled — silently drain
            }
            match client.post(&url).json(&payload).send().await {
                Ok(resp) => {
                    let status = resp.status();
                    // Explicitly call out redirect responses so operators
                    // who accidentally configured a URL that 302s to an
                    // internal target know why their webhook never
                    // reaches its intended destination.
                    if status.is_redirection() {
                        tracing::warn!(
                            "alert-webhook: {} returned redirect {} (not followed)",
                            safe_url,
                            status
                        );
                    } else if !status.is_success() {
                        tracing::warn!("alert-webhook: {} responded {}", safe_url, status);
                    }
                }
                Err(e) => tracing::warn!("alert-webhook: POST to {safe_url} failed: {e}"),
            }
        }
    });
    tx
}

/// Strip any `userinfo` component (credentials of the form
/// `scheme://user:pass@host/...`) from a URL for logging.
///
/// Only the textual prefix is inspected, so this works for any scheme
/// regardless of whether reqwest accepted the URL. When no userinfo is
/// present the input is returned verbatim.
fn redact_url_userinfo(url: &str) -> String {
    // Find the `://` boundary; anything before that is the scheme.
    let Some(scheme_end) = url.find("://") else {
        return url.to_string();
    };
    let after_scheme = &url[scheme_end + 3..];
    // Userinfo ends at the first `@` that appears *before* the first path
    // separator (`/`, `?`, `#`), so we can't be fooled by `@` characters
    // inside query strings.
    let authority_end = after_scheme
        .find(['/', '?', '#'])
        .unwrap_or(after_scheme.len());
    let authority = &after_scheme[..authority_end];
    if let Some(at_pos) = authority.find('@') {
        let redacted_authority = format!("***@{host}", host = &authority[at_pos + 1..]);
        let mut out = String::with_capacity(url.len());
        out.push_str(&url[..scheme_end + 3]);
        out.push_str(&redacted_authority);
        out.push_str(&after_scheme[authority_end..]);
        out
    } else {
        url.to_string()
    }
}

/// Enqueue a payload on the worker channel using `try_send`.
///
/// Non-blocking: new alerts are dropped if the worker queue is saturated
/// (the **newest** payload is the one lost, not the oldest). This keeps
/// the UI-never-blocks invariant even when the remote webhook is slow or
/// unreachable — payload ordering is best-effort.
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

    #[test]
    fn redact_url_userinfo_strips_basic_auth() {
        assert_eq!(
            redact_url_userinfo("https://user:pass@hook.example.com/alerts"),
            "https://***@hook.example.com/alerts"
        );
    }

    #[test]
    fn redact_url_userinfo_strips_user_only() {
        assert_eq!(
            redact_url_userinfo("https://token@hook.example.com/"),
            "https://***@hook.example.com/"
        );
    }

    #[test]
    fn redact_url_userinfo_preserves_plain_url() {
        assert_eq!(
            redact_url_userinfo("https://hook.example.com/alerts"),
            "https://hook.example.com/alerts"
        );
    }

    #[test]
    fn redact_url_userinfo_ignores_at_in_path() {
        // `@` that appears only in the path must not be treated as
        // userinfo; the URL should be returned verbatim.
        assert_eq!(
            redact_url_userinfo("https://hook.example.com/path/with@symbol"),
            "https://hook.example.com/path/with@symbol"
        );
    }

    #[test]
    fn redact_url_userinfo_ignores_at_in_query() {
        assert_eq!(
            redact_url_userinfo("https://hook.example.com/alerts?email=a@b"),
            "https://hook.example.com/alerts?email=a@b"
        );
    }

    #[test]
    fn redact_url_userinfo_handles_missing_scheme() {
        // Malformed inputs are passed through verbatim rather than
        // panicking.
        assert_eq!(redact_url_userinfo("hook.example.com"), "hook.example.com");
    }
}
