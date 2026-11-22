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

//! Cache for an expensive probe whose answer rarely or never changes.
//!
//! The Windows readers use it for WMI inventory queries and the macOS CPU
//! reader for its one-time hardware probe (issue #501). A success is kept
//! for `success_ttl` (`None` keeps it for the process lifetime); a failure
//! is not retried until `failure_retry` has passed, so a host where the
//! probe keeps failing pays for it once per retry window instead of once
//! per collection tick.
//!
//! The clock is injected so the policy is unit-tested with synthetic
//! instants on every platform.

use std::time::{Duration, Instant};

/// Last outcome of a probe, with the instants that drive its retry policy.
#[derive(Debug, Clone)]
pub struct CachedProbe<T: Clone> {
    /// Last successful value and when it was obtained.
    success: Option<(T, Instant)>,
    /// When the most recent attempt failed, cleared by a success.
    last_failure: Option<Instant>,
}

impl<T: Clone> Default for CachedProbe<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Clone> CachedProbe<T> {
    /// An empty cache: the first `get_or_try` always runs the probe.
    pub const fn new() -> Self {
        Self {
            success: None,
            last_failure: None,
        }
    }

    /// Return the cached value or run `probe`.
    ///
    /// * A success younger than `success_ttl` (or any success when the TTL
    ///   is `None`) is returned without running the probe.
    /// * Within `failure_retry` of a failed attempt the probe is not run
    ///   again; the last success, if there ever was one, is returned stale.
    /// * Otherwise the probe runs. `Some` replaces the cached value, `None`
    ///   records a failure and falls back to the stale value.
    pub fn get_or_try(
        &mut self,
        now: Instant,
        success_ttl: Option<Duration>,
        failure_retry: Duration,
        probe: impl FnOnce() -> Option<T>,
    ) -> Option<T> {
        if let Some((value, at)) = &self.success
            && success_ttl.is_none_or(|ttl| now.saturating_duration_since(*at) < ttl)
        {
            return Some(value.clone());
        }

        if let Some(failed_at) = self.last_failure
            && now.saturating_duration_since(failed_at) < failure_retry
        {
            return self.stale();
        }

        match probe() {
            Some(value) => {
                self.success = Some((value.clone(), now));
                self.last_failure = None;
                Some(value)
            }
            None => {
                self.last_failure = Some(now);
                self.stale()
            }
        }
    }

    fn stale(&self) -> Option<T> {
        self.success.as_ref().map(|(value, _)| value.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    const RETRY: Duration = Duration::from_secs(30);
    const TTL: Duration = Duration::from_secs(300);

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn success_without_ttl_is_kept_forever() {
        let calls = Cell::new(0);
        let mut probe = CachedProbe::new();
        let t0 = Instant::now();
        for offset in [0, 1, 60, 86_400] {
            let value = probe.get_or_try(t0 + secs(offset), None, RETRY, || {
                calls.set(calls.get() + 1);
                Some(7u32)
            });
            assert_eq!(value, Some(7));
        }
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn success_is_refreshed_after_ttl() {
        let calls = Cell::new(0);
        let mut probe = CachedProbe::new();
        let t0 = Instant::now();
        let mut run = |at: Duration| {
            probe.get_or_try(t0 + at, Some(TTL), RETRY, || {
                calls.set(calls.get() + 1);
                Some(calls.get())
            })
        };
        for tick in 0..10 {
            assert_eq!(run(secs(tick * 2)), Some(1));
        }
        assert_eq!(calls.get(), 1, "ten ticks inside the TTL query once");
        assert_eq!(run(secs(300)), Some(2), "TTL elapsed, probe runs again");
        assert_eq!(calls.get(), 2);
    }

    #[test]
    fn failure_is_retried_only_after_the_retry_window() {
        let calls = Cell::new(0);
        let mut probe: CachedProbe<u32> = CachedProbe::new();
        let t0 = Instant::now();
        let mut run = |at: Duration, answer: Option<u32>| {
            probe.get_or_try(t0 + at, None, RETRY, || {
                calls.set(calls.get() + 1);
                answer
            })
        };
        assert_eq!(run(secs(0), None), None);
        for tick in 1..15 {
            assert_eq!(run(secs(tick * 2), Some(1)), None, "inside retry window");
        }
        assert_eq!(calls.get(), 1);
        assert_eq!(run(secs(30), None), None, "window elapsed, probe reruns");
        assert_eq!(calls.get(), 2);
        assert_eq!(run(secs(60), Some(5)), Some(5));
        assert_eq!(run(secs(61), None), Some(5), "success is now cached");
        assert_eq!(calls.get(), 3);
    }

    #[test]
    fn failed_refresh_serves_the_stale_value_until_retry() {
        let calls = Cell::new(0);
        let mut probe = CachedProbe::new();
        let t0 = Instant::now();
        let mut run = |at: Duration, answer: Option<&'static str>| {
            probe.get_or_try(t0 + at, Some(TTL), RETRY, || {
                calls.set(calls.get() + 1);
                answer
            })
        };
        assert_eq!(run(secs(0), Some("a")), Some("a"));
        assert_eq!(run(secs(300), None), Some("a"), "refresh failed, stale");
        assert_eq!(run(secs(310), Some("b")), Some("a"), "inside retry window");
        assert_eq!(calls.get(), 2);
        assert_eq!(run(secs(330), Some("b")), Some("b"));
        assert_eq!(calls.get(), 3);
    }

    #[test]
    fn clock_going_backwards_does_not_panic_or_refresh() {
        let mut probe = CachedProbe::new();
        let t0 = Instant::now() + secs(100);
        assert_eq!(probe.get_or_try(t0, Some(TTL), RETRY, || Some(1)), Some(1));
        assert_eq!(
            probe.get_or_try(t0 - secs(50), Some(TTL), RETRY, || Some(2)),
            Some(1)
        );
    }
}
