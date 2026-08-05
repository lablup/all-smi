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

//! A one-way boolean latch that any number of tasks can await
//! (issue #311).
//!
//! `all-smi api` previously learned about shutdown from exactly two
//! sources, `Ctrl+C` and `SIGTERM`. The Windows Service Control Manager
//! has neither: a Stop control arrives on a handler thread with no
//! signal to raise, so the SCM backend needs a way to reach the same
//! graceful path (server drain, then energy WAL flush). The same shape
//! answers the mirror question of when the service may report
//! `SERVICE_RUNNING`, which is "once a listener is bound".
//!
//! Both are one-way transitions: `false` once, `true` forever after.
//! [`Latch`] is that primitive, and it is a plain value rather than a
//! process global so its semantics can be unit tested deterministically
//! on any platform, including hosts that will never run a Windows
//! service.

use std::sync::Arc;

use tokio::sync::watch;

/// A latch that starts closed and can be opened exactly once.
///
/// Cloning shares the underlying state. [`Latch::wait`] resolves for
/// *every* waiter, including waiters created after the trigger, which is
/// why this wraps a [`watch`] channel rather than a
/// [`tokio::sync::Notify`]: `notify_one` would hand the single stored
/// permit to whichever waiter got there first, and `notify_waiters`
/// would be lost entirely when it fires before anyone subscribes.
#[derive(Debug, Clone)]
pub struct Latch {
    tx: Arc<watch::Sender<bool>>,
}

impl Latch {
    /// Create a closed latch.
    pub fn new() -> Self {
        Self {
            tx: Arc::new(watch::channel(false).0),
        }
    }

    /// Open the latch, waking every current and future waiter.
    ///
    /// Idempotent, and safe to call from a non-async context such as a
    /// Windows service control handler.
    pub fn trigger(&self) {
        // `send` fails when every receiver has been dropped and would
        // then leave the stored value untouched, which would silently
        // lose a Stop control that arrived before any listener task
        // subscribed. `send_replace` always writes.
        self.tx.send_replace(true);
    }

    /// Whether the latch has already been opened.
    pub fn is_triggered(&self) -> bool {
        *self.tx.borrow()
    }

    /// Resolve once the latch is open, immediately if it already is.
    pub async fn wait(&self) {
        let mut rx = self.tx.subscribe();
        // `subscribe` marks the current value as seen, so a trigger that
        // happened before this call would never show up as a change.
        // Read it once up front to close that race.
        if *rx.borrow() {
            return;
        }
        while rx.changed().await.is_ok() {
            if *rx.borrow_and_update() {
                return;
            }
        }
        // `changed` only errors once the sender is dropped, and this
        // struct owns the sender for as long as any clone is alive, so
        // the loop above cannot fall through while `self` is borrowed.
        // Park rather than returning, so a caller selecting on this
        // future never mistakes an impossible state for a trigger.
        std::future::pending::<()>().await
    }
}

impl Default for Latch {
    fn default() -> Self {
        Self::new()
    }
}

// Test module lives in `latch_tests.rs` to keep this file focused.
#[cfg(test)]
#[path = "latch_tests.rs"]
mod tests;
