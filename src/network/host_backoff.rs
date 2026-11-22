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

//! Scrape time bounds and per-host failure backoff for remote collection.
//!
//! Lives in the network client rather than `AppState` because
//! `record --source=remote` drives the same client without an `AppState`
//! (issue #500).

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use crate::common::config::AppConfig;

/// Time bounds for one remote scrape tick.
///
/// `request` is kept below `budget` so a single attempt against a host that
/// accepts and never answers ends on its own, before the tick deadline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ScrapeTimeouts {
    /// TCP connect timeout for each attempt.
    pub(crate) connect: Duration,
    /// Whole-request timeout for each attempt (connect, headers, body).
    pub(crate) request: Duration,
    /// Deadline for the whole tick across every host.
    pub(crate) budget: Duration,
}

impl Default for ScrapeTimeouts {
    fn default() -> Self {
        Self {
            connect: Duration::from_millis(AppConfig::CONNECT_TIMEOUT_MS),
            request: Duration::from_secs(AppConfig::CONNECTION_TIMEOUT_SECS),
            budget: Duration::from_millis(AppConfig::REMOTE_SCRAPE_BUDGET_MS),
        }
    }
}

/// Consecutive failures after which a host starts being skipped.
pub(crate) const BACKOFF_AFTER_FAILURES: u32 = 2;
/// First skip window, doubled for every further failure.
pub(crate) const BACKOFF_BASE: Duration = Duration::from_secs(2);
/// Longest skip window.
pub(crate) const BACKOFF_MAX: Duration = Duration::from_secs(60);

/// Skip window after `failures` consecutive failures:
/// `min(2 s * 2^(failures - 2), 60 s)` from the second failure on.
pub(crate) fn backoff_delay(failures: u32) -> Option<Duration> {
    if failures < BACKOFF_AFTER_FAILURES {
        return None;
    }
    // 2 s * 2^5 already exceeds the cap, so clamp the exponent to keep the
    // shift well defined for any failure count.
    let exponent = (failures - BACKOFF_AFTER_FAILURES).min(16);
    Some(BACKOFF_BASE.saturating_mul(1 << exponent).min(BACKOFF_MAX))
}

/// Failure history of one host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HostBackoff {
    pub(crate) consecutive_failures: u32,
    /// The host is not scraped before this instant.
    pub(crate) next_attempt: Instant,
}

/// Status message for a host skipped while backing off. The `backing-off`
/// prefix is what `ConnectionStatus::mark_failure` classifies on.
pub(crate) fn backing_off_message(remaining: Duration, failures: u32) -> String {
    let secs = remaining.as_secs() + u64::from(remaining.subsec_nanos() > 0);
    format!(
        "backing-off: next retry in {}s ({failures} consecutive failures)",
        secs.max(1)
    )
}

/// Per-host backoff state keyed by host identity (`host:port`).
#[derive(Debug, Default)]
pub(crate) struct BackoffTable {
    hosts: HashMap<String, HostBackoff>,
}

impl BackoffTable {
    /// Drop entries for hosts that are no longer configured.
    pub(crate) fn retain_configured(&mut self, configured: &HashSet<&str>) {
        self.hosts
            .retain(|host_id, _| configured.contains(host_id.as_str()));
    }

    /// `Some((remaining, failures))` when `host_id` must be skipped at `now`.
    pub(crate) fn pending_backoff(&self, host_id: &str, now: Instant) -> Option<(Duration, u32)> {
        let entry = self.hosts.get(host_id)?;
        (entry.next_attempt > now).then(|| (entry.next_attempt - now, entry.consecutive_failures))
    }

    /// One success clears the host's history.
    pub(crate) fn record_success(&mut self, host_id: &str) {
        self.hosts.remove(host_id);
    }

    /// Count a failure and schedule the next attempt; returns the new
    /// consecutive-failure count.
    pub(crate) fn record_failure(&mut self, host_id: &str, now: Instant) -> u32 {
        let entry = self
            .hosts
            .entry(host_id.to_string())
            .or_insert(HostBackoff {
                consecutive_failures: 0,
                next_attempt: now,
            });
        entry.consecutive_failures = entry.consecutive_failures.saturating_add(1);
        entry.next_attempt = now + backoff_delay(entry.consecutive_failures).unwrap_or_default();
        entry.consecutive_failures
    }

    #[cfg(test)]
    pub(crate) fn get(&self, host_id: &str) -> Option<&HostBackoff> {
        self.hosts.get(host_id)
    }

    #[cfg(test)]
    pub(crate) fn expire_all(&mut self) {
        let now = Instant::now();
        for entry in self.hosts.values_mut() {
            entry.next_attempt = now;
        }
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.hosts.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delay_starts_at_second_failure_and_caps_at_sixty_seconds() {
        assert_eq!(backoff_delay(0), None);
        assert_eq!(backoff_delay(1), None);
        assert_eq!(backoff_delay(2), Some(Duration::from_secs(2)));
        assert_eq!(backoff_delay(3), Some(Duration::from_secs(4)));
        assert_eq!(backoff_delay(4), Some(Duration::from_secs(8)));
        assert_eq!(backoff_delay(6), Some(Duration::from_secs(32)));
        assert_eq!(backoff_delay(7), Some(BACKOFF_MAX));
        assert_eq!(backoff_delay(u32::MAX), Some(BACKOFF_MAX));
    }

    #[test]
    fn table_skips_after_second_failure_and_success_clears() {
        let mut table = BackoffTable::default();
        let now = Instant::now();
        assert_eq!(table.record_failure("a:1", now), 1);
        assert_eq!(table.pending_backoff("a:1", now), None);
        assert_eq!(table.record_failure("a:1", now), 2);
        let (remaining, failures) = table.pending_backoff("a:1", now).unwrap();
        assert_eq!((remaining, failures), (Duration::from_secs(2), 2));
        assert_eq!(table.pending_backoff("a:1", now + remaining), None);
        table.record_success("a:1");
        assert!(table.get("a:1").is_none());
    }

    #[test]
    fn table_drops_unconfigured_hosts() {
        let mut table = BackoffTable::default();
        let now = Instant::now();
        table.record_failure("a:1", now);
        table.record_failure("b:1", now);
        table.retain_configured(&HashSet::from(["b:1"]));
        assert_eq!(table.len(), 1);
        assert!(table.get("b:1").is_some());
    }

    #[test]
    fn message_rounds_up_and_carries_the_chip_prefix() {
        let msg = backing_off_message(Duration::from_millis(1500), 3);
        assert_eq!(
            msg,
            "backing-off: next retry in 2s (3 consecutive failures)"
        );
        assert!(backing_off_message(Duration::ZERO, 2).contains("in 1s"));
    }

    #[test]
    fn default_request_timeout_fits_inside_the_budget() {
        let timeouts = ScrapeTimeouts::default();
        assert!(timeouts.connect < timeouts.request);
        assert!(timeouts.request < timeouts.budget);
    }
}
