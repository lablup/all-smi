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

//! Tests for [`super::Latch`] (issue #311).
//!
//! Every test builds its own latch, so nothing here depends on process
//! state, on signal delivery, or on test execution order.

use std::time::Duration;

use super::Latch;

#[test]
fn a_fresh_latch_is_closed() {
    let latch = Latch::new();
    assert!(!latch.is_triggered());
    assert!(!Latch::default().is_triggered());
}

#[test]
fn trigger_is_visible_synchronously() {
    let latch = Latch::new();
    latch.trigger();
    assert!(latch.is_triggered());
}

#[test]
fn trigger_is_idempotent() {
    let latch = Latch::new();
    latch.trigger();
    latch.trigger();
    assert!(latch.is_triggered());
}

#[test]
fn clones_share_state() {
    let latch = Latch::new();
    let clone = latch.clone();
    clone.trigger();
    assert!(latch.is_triggered(), "a clone must open the same latch");
}

#[tokio::test]
async fn wait_returns_immediately_when_already_triggered() {
    let latch = Latch::new();
    latch.trigger();
    // No timeout wrapper: if this ever blocks, the test harness hangs
    // loudly rather than passing on a technicality.
    latch.wait().await;
}

#[tokio::test]
async fn wait_pends_until_triggered() {
    let latch = Latch::new();
    let pending = tokio::time::timeout(Duration::from_millis(50), latch.wait()).await;
    assert!(
        pending.is_err(),
        "wait must not resolve while the latch is closed"
    );

    latch.trigger();
    tokio::time::timeout(Duration::from_secs(5), latch.wait())
        .await
        .expect("wait must resolve once the latch is open");
}

#[tokio::test]
async fn every_waiter_wakes() {
    // The dual-listener path builds one shutdown future per listener, so
    // a single trigger has to release all of them.
    let latch = Latch::new();
    let waiters: Vec<_> = (0..4)
        .map(|_| {
            let latch = latch.clone();
            tokio::spawn(async move { latch.wait().await })
        })
        .collect();

    // Give the tasks a chance to actually subscribe before triggering,
    // so this exercises the "changed" path rather than the fast path.
    tokio::time::sleep(Duration::from_millis(20)).await;
    latch.trigger();

    for w in waiters {
        tokio::time::timeout(Duration::from_secs(5), w)
            .await
            .expect("waiter must wake")
            .expect("waiter task must not panic");
    }
}

#[tokio::test]
async fn a_waiter_subscribing_after_the_trigger_still_wakes() {
    // The Windows Stop control can arrive before the API server has
    // spawned its listeners. `send_replace` plus the up-front read in
    // `wait` is what keeps that from deadlocking.
    let latch = Latch::new();
    latch.trigger();
    let late = tokio::spawn({
        let latch = latch.clone();
        async move { latch.wait().await }
    });
    tokio::time::timeout(Duration::from_secs(5), late)
        .await
        .expect("late waiter must wake")
        .expect("late waiter task must not panic");
}
