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

//! Tests for [`FrameBus`]: publish and subscribe, the fresh-collect lock,
//! and the per-frame body cache (issue #503).

use super::*;
use crate::api::handlers::snapshot::SectionFilter;
use crate::snapshot::Snapshot;

fn make_snapshot(host: &str) -> Snapshot {
    Snapshot {
        schema: 1,
        timestamp: "2026-04-20T00:00:00Z".to_string(),
        hostname: host.to_string(),
        gpus: None,
        cpus: None,
        memory: None,
        chassis: None,
        processes: None,
        storage: None,
        errors: Vec::new(),
    }
}

#[tokio::test]
async fn publish_updates_latest_and_increments_seq() {
    let bus = FrameBus::new(Duration::from_secs(3));
    assert!(bus.latest().await.is_none());
    let seq = bus.publish(make_snapshot("h1")).await;
    assert_eq!(seq, 1);
    let latest = bus.latest().await.expect("latest must be present");
    assert_eq!(latest.snapshot.hostname, "h1");
    assert_eq!(latest.seq, 1);
    let seq2 = bus.publish(make_snapshot("h2")).await;
    assert_eq!(seq2, 2);
}

#[tokio::test]
async fn subscribers_receive_published_frames() {
    let bus = FrameBus::new(Duration::from_secs(3));
    let mut rx = bus.subscribe();
    assert_eq!(bus.subscriber_count(), 1);
    bus.publish(make_snapshot("rx-host")).await;
    let frame = rx.recv().await.expect("receiver must get the frame");
    assert_eq!(frame.snapshot.hostname, "rx-host");
    assert_eq!(frame.seq, 1);
}

#[tokio::test]
async fn dropping_receiver_releases_slot() {
    let bus = FrameBus::new(Duration::from_secs(3));
    let rx = bus.subscribe();
    assert_eq!(bus.subscriber_count(), 1);
    drop(rx);
    assert_eq!(bus.subscriber_count(), 0);
}

/// Regression for the security review of #193: the fresh-collect
/// lock must serialize concurrent `/snapshot` callers so a burst
/// cannot saturate the Tokio blocking pool. We cannot easily assert
/// "only one collect ran" without touching hardware, but we can
/// observe that a task holding the guard blocks a second caller
/// until the guard drops.
#[tokio::test]
async fn fresh_collect_lock_serializes_callers() {
    let bus = FrameBus::new(Duration::from_secs(3));
    let guard = bus.lock_fresh_collect().await;

    // Spawn a second acquirer and race it with a short sleep. If the
    // lock is serializing correctly, the spawned task will not
    // resolve until we drop `guard` below.
    let bus2 = bus.clone();
    let handle = tokio::spawn(async move {
        let _g = bus2.lock_fresh_collect().await;
    });

    // Give the second task a moment to try to acquire.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(
        !handle.is_finished(),
        "second lock_fresh_collect caller should block while first guard is held"
    );

    drop(guard);
    handle
        .await
        .expect("second caller must complete once the guard is released");
}

fn default_filter() -> SectionFilter {
    SectionFilter::default_http()
}

fn serializations() -> usize {
    crate::api::handlers::snapshot::SERIALIZE_COUNT.with(|count| count.get())
}

/// Ten receivers of one frame on the same filter cost one serialization
/// and share one body. Before issue #503 each receiver converted and
/// serialized the snapshot itself.
#[tokio::test(flavor = "current_thread")]
async fn ten_receivers_of_one_frame_share_one_serialization() {
    let bus = FrameBus::new(Duration::from_secs(3));
    let mut receivers: Vec<_> = (0..10).map(|_| bus.subscribe()).collect();
    bus.publish(make_snapshot("shared")).await;

    let before = serializations();
    let mut bodies = Vec::new();
    for rx in &mut receivers {
        let frame = rx.recv().await.expect("frame");
        bodies.push(
            bus.body_for(&frame, &default_filter(), false)
                .expect("body"),
        );
    }
    assert_eq!(serializations() - before, 1);
    for body in &bodies {
        assert!(
            Arc::ptr_eq(body, &bodies[0]),
            "receivers must share the body"
        );
    }
    assert!(bodies[0].contains("\"hostname\":\"shared\""));
}

/// The cache is keyed by filter and pretty flag, and replaced by the next
/// frame.
#[tokio::test(flavor = "current_thread")]
async fn body_cache_is_keyed_by_filter_and_pretty_and_reset_per_frame() {
    let bus = FrameBus::new(Duration::from_secs(3));
    bus.publish(make_snapshot("one")).await;
    let first = bus.latest().await.expect("frame");

    let gpu_only = SectionFilter {
        cpu: false,
        memory: false,
        chassis: false,
        ..default_filter()
    };
    let before = serializations();
    let compact = bus.body_for(&first, &default_filter(), false).unwrap();
    let pretty = bus.body_for(&first, &default_filter(), true).unwrap();
    let other = bus.body_for(&first, &gpu_only, false).unwrap();
    assert_eq!(serializations() - before, 3, "three distinct keys");
    assert!(!Arc::ptr_eq(&compact, &pretty));
    assert!(pretty.contains('\n') && !compact.contains('\n'));
    for _ in 0..3 {
        assert!(Arc::ptr_eq(
            &compact,
            &bus.body_for(&first, &default_filter(), false).unwrap()
        ));
        assert!(Arc::ptr_eq(
            &other,
            &bus.body_for(&first, &gpu_only, false).unwrap()
        ));
    }
    assert_eq!(
        serializations() - before,
        3,
        "repeat lookups are cache hits"
    );

    // A new frame replaces the cache.
    bus.publish(make_snapshot("two")).await;
    let second = bus.latest().await.expect("frame");
    let body = bus.body_for(&second, &default_filter(), false).unwrap();
    assert!(body.contains("\"hostname\":\"two\""));
    assert_eq!(serializations() - before, 4);

    // A lagging subscriber still gets the right body for its older frame,
    // without evicting the newest frame's entries.
    let stale = bus.body_for(&first, &default_filter(), false).unwrap();
    assert!(stale.contains("\"hostname\":\"one\""));
    assert!(Arc::ptr_eq(
        &body,
        &bus.body_for(&second, &default_filter(), false).unwrap()
    ));
}

/// A serialization in progress for one key must not block other keys: the
/// global cache lock is only held to fetch the per-key cell.
#[tokio::test(flavor = "current_thread")]
async fn different_keys_do_not_block_each_other() {
    let bus = FrameBus::new(Duration::from_secs(3));
    bus.publish(make_snapshot("parallel")).await;
    let frame = bus.latest().await.expect("frame");
    let gpu_only = SectionFilter {
        cpu: false,
        memory: false,
        chassis: false,
        ..default_filter()
    };

    // Create the cell for the default key and hold its lock, as a slow
    // serialization would.
    bus.body_for(&frame, &default_filter(), false).unwrap();
    let cell = bus
        .inner
        .bodies
        .lock()
        .unwrap()
        .bodies
        .get(&(default_filter(), false))
        .cloned()
        .expect("cell");
    let _held = cell.lock().unwrap();

    let other_bus = bus.clone();
    let other_frame = frame.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(other_bus.body_for(&other_frame, &gpu_only, false).is_ok());
    });
    let finished = rx
        .recv_timeout(Duration::from_secs(10))
        .expect("another key must not wait for the held cell");
    assert!(finished);
}
