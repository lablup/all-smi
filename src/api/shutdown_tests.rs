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

//! Tests for the graceful-shutdown source added in issue #311.
//!
//! The point of these is the *negative* case as much as the positive
//! one. `shutdown_signal` is what lets `axum::serve` return so the
//! energy WAL flush can run, so an extra `select!` arm that resolved
//! spuriously would tear the server down the instant it started on every
//! platform. `shutdown_signal_with` therefore takes the external latch
//! as a parameter: each test drives its own, nothing depends on process
//! state or on a real signal being delivered, and the Ctrl+C / SIGTERM
//! arms stay exactly the code they were before.

use std::time::Duration;

use super::*;

/// How long "does not resolve" is sampled for. Long enough that a
/// mistakenly-ready future is caught, short enough to keep the suite
/// fast.
const PEND_WINDOW: Duration = Duration::from_millis(150);

#[tokio::test]
async fn pends_while_no_source_has_fired() {
    // The regression guard: with no Ctrl+C, no SIGTERM, and a closed
    // latch, the future must stay pending. If this fails, every
    // `all-smi api` invocation on every platform exits immediately.
    let latch = Latch::new();
    let result = tokio::time::timeout(PEND_WINDOW, shutdown_signal_with(&latch)).await;
    assert!(
        result.is_err(),
        "shutdown_signal must not resolve before a shutdown source fires"
    );
}

#[tokio::test]
async fn resolves_when_the_latch_was_triggered_first() {
    // The Windows Stop control can land before the listener installs its
    // shutdown future.
    let latch = Latch::new();
    latch.trigger();
    tokio::time::timeout(Duration::from_secs(5), shutdown_signal_with(&latch))
        .await
        .expect("an already-triggered latch must resolve the shutdown future");
}

#[tokio::test]
async fn resolves_on_a_later_trigger() {
    let latch = Latch::new();
    let waiter = {
        let latch = latch.clone();
        tokio::spawn(async move { shutdown_signal_with(&latch).await })
    };
    tokio::time::sleep(Duration::from_millis(20)).await;
    latch.trigger();
    tokio::time::timeout(Duration::from_secs(5), waiter)
        .await
        .expect("a trigger must resolve an already-installed shutdown future")
        .expect("shutdown task must not panic");
}

#[tokio::test]
async fn every_listener_future_resolves_from_one_trigger() {
    // The dual-listener path builds one `shutdown_signal` per listener.
    // Both have to come back or `run_api_mode` never reaches the WAL
    // flush.
    let latch = Latch::new();
    let futures: Vec<_> = (0..2)
        .map(|_| {
            let latch = latch.clone();
            tokio::spawn(async move { shutdown_signal_with(&latch).await })
        })
        .collect();
    tokio::time::sleep(Duration::from_millis(20)).await;
    latch.trigger();
    for f in futures {
        tokio::time::timeout(Duration::from_secs(5), f)
            .await
            .expect("each listener's shutdown future must resolve")
            .expect("shutdown task must not panic");
    }
}

#[tokio::test]
async fn the_process_wide_helpers_drive_the_process_wide_latch() {
    // `shutdown_signal()` reads the same latch `request_shutdown()`
    // writes, which is the wiring the SCM Stop handler depends on. This
    // is the only test that touches process state; it only ever moves
    // the latch from closed to open, and no other test asserts that it
    // is closed.
    request_shutdown();
    assert!(shutdown_requested());
    tokio::time::timeout(Duration::from_secs(5), shutdown_signal())
        .await
        .expect("request_shutdown must resolve the process-wide shutdown future");
}

#[tokio::test]
async fn readiness_is_latched_by_mark_serving() {
    mark_serving();
    assert!(is_serving());
    tokio::time::timeout(Duration::from_secs(5), wait_until_serving())
        .await
        .expect("wait_until_serving must resolve once a listener is bound");
}
