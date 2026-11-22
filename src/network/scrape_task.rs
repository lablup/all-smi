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

//! One host's scrape: fetch with bounded retries, then parse inside the
//! host's own task so the collector loop only merges results (issue #500).

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{RwLock, Semaphore};

use crate::common::config::{AppConfig, EnvConfig};
use crate::network::metrics_parser::{MAX_METRICS_TEXT_BYTES, MetricsParser, ParsedRemoteMetrics};

use super::client::{RateLimiter, read_response_text_with_limit};

/// Bodies up to this size are parsed inline on the host task; larger ones
/// move to the blocking pool so a big exposition cannot stall a runtime
/// worker. Below the threshold the pool hop costs more than the parse.
pub(crate) const INLINE_PARSE_MAX_BYTES: usize = 64 * 1024;

/// What one host task hands back to the collector loop.
pub(crate) enum ScrapeOutcome {
    /// The host answered and its body parsed.
    Parsed(Box<ParsedRemoteMetrics>),
    /// The host answered with an empty body.
    Empty,
    /// The scrape failed; the message's prefix selects the status chip.
    Failed(String),
}

/// Everything a host task needs, cloned out of the `NetworkClient`.
pub(crate) struct ScrapeJob {
    pub(crate) client: reqwest::Client,
    pub(crate) auth_token: Option<String>,
    pub(crate) rate_limiter: Arc<RwLock<RateLimiter>>,
    pub(crate) semaphore: Arc<Semaphore>,
    pub(crate) host: String,
    pub(crate) host_id: String,
    pub(crate) stagger: Duration,
    pub(crate) request_timeout: Duration,
}

impl ScrapeJob {
    pub(crate) async fn run(self) -> ScrapeOutcome {
        // Stagger connection attempts to avoid overwhelming the listen queue
        tokio::time::sleep(self.stagger).await;

        // Acquire semaphore permit to limit concurrency
        let Ok(_permit) = self.semaphore.acquire().await else {
            return ScrapeOutcome::Failed("Connection limiter closed".to_string());
        };

        {
            let mut limiter = self.rate_limiter.write().await;
            if !limiter.check_rate_limit(&self.host).await {
                return ScrapeOutcome::Failed("Rate limit exceeded".to_string());
            }
        }

        let url = match super::NetworkClient::validate_and_build_url(&self.host) {
            Ok(url) => url,
            Err(e) => return ScrapeOutcome::Failed(format!("Invalid URL: {e}")),
        };

        let text = match self.fetch_body(&url).await {
            Ok(text) => text,
            Err(message) => return ScrapeOutcome::Failed(message),
        };
        if text.is_empty() {
            return ScrapeOutcome::Empty;
        }
        match parse_body(text, self.host_id).await {
            Ok(parsed) => ScrapeOutcome::Parsed(Box::new(parsed)),
            Err(message) => ScrapeOutcome::Failed(message),
        }
    }

    fn timeout_message(&self, error: &reqwest::Error) -> String {
        format!(
            "timeout: no response within {} ms ({error})",
            self.request_timeout.as_millis()
        )
    }

    /// Fetch the metrics body. Fast failures (connection refused, HTTP 5xx)
    /// are retried up to `AppConfig::RETRY_ATTEMPTS` times; a timeout is not,
    /// because another attempt against a host that does not answer would
    /// only run into the tick deadline.
    async fn fetch_body(&self, url: &str) -> Result<String, String> {
        let attempts = AppConfig::RETRY_ATTEMPTS.max(1);
        for attempt in 1..=attempts {
            let last = attempt == attempts;
            let mut request = self.client.get(url);
            if let Some(ref token) = self.auth_token {
                request = request.header("Authorization", format!("Bearer {token}"));
            }

            // Bind the send result to a local variable to ensure
            // temporaries are dropped before surrounding locals (Rust 2024 drop order).
            let send_result = request.send().await;
            match send_result {
                Ok(response) if response.status().is_success() => {
                    match read_response_text_with_limit(response, MAX_METRICS_TEXT_BYTES).await {
                        Ok(text) => return Ok(text),
                        Err(e) if e.is_timeout() => return Err(self.timeout_message(&e)),
                        Err(e) if last => return Err(format!("Text parse error: {e}")),
                        Err(_) => {}
                    }
                }
                Ok(response) => {
                    let status = response.status();
                    if last || !status.is_server_error() {
                        return Err(format!("HTTP {status}"));
                    }
                }
                Err(e) if e.is_timeout() => return Err(self.timeout_message(&e)),
                Err(e) if last => {
                    return Err(format!("Connection error after {attempt} attempts: {e}"));
                }
                Err(_) => {}
            }

            tokio::time::sleep(Duration::from_millis(EnvConfig::retry_delay(attempt))).await;
        }
        Err("All retry attempts failed".to_string())
    }
}

/// Parse one host's body, on the blocking pool when it is large.
///
/// If the tick deadline aborts the host task while a blocking parse runs,
/// the parse finishes in the background and its result is dropped; the
/// work is bounded by the 10 MiB body cap.
async fn parse_body(text: String, host_id: String) -> Result<ParsedRemoteMetrics, String> {
    if text.len() <= INLINE_PARSE_MAX_BYTES {
        return Ok(MetricsParser::new().parse_metrics_with_chassis(&text, &host_id));
    }
    tokio::task::spawn_blocking(move || {
        MetricsParser::new().parse_metrics_with_chassis(&text, &host_id)
    })
    .await
    .map_err(|e| format!("Metrics parse failed: {e}"))
}
