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

//! Tests for the `/snapshot` stale path (issue #503): wait for the next
//! publish before reading hardware, build the fallback collector once,
//! and reuse a recent fallback result.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use super::{Resolved, SectionFilter, parse_include, resolve_snapshot};
use crate::api::frame_bus::FrameBus;
use crate::device::{ChassisInfo, CpuInfo, GpuInfo, MemoryInfo, ProcessInfo};
use crate::snapshot::{Snapshot, SnapshotCollector};
use crate::storage::info::StorageInfo;

/// Counts how often it is built and how often it collects.
#[derive(Default)]
struct Counters {
    built: AtomicUsize,
    gpu_collects: AtomicUsize,
    storage_collects: AtomicUsize,
}

struct CountingCollector(Arc<Counters>);

impl SnapshotCollector for CountingCollector {
    fn hostname(&self) -> String {
        "fallback".to_string()
    }
    fn collect_gpus(&self) -> Vec<GpuInfo> {
        self.0.gpu_collects.fetch_add(1, Ordering::SeqCst);
        Vec::new()
    }
    fn collect_cpus(&self) -> Vec<CpuInfo> {
        Vec::new()
    }
    fn collect_memory(&self) -> Vec<MemoryInfo> {
        Vec::new()
    }
    fn collect_chassis(&self) -> Vec<ChassisInfo> {
        Vec::new()
    }
    fn collect_processes(&self) -> Vec<ProcessInfo> {
        Vec::new()
    }
    fn collect_storage(&self) -> Vec<StorageInfo> {
        self.0.storage_collects.fetch_add(1, Ordering::SeqCst);
        Vec::new()
    }
}

fn counting_bus(interval: Duration) -> (FrameBus, Arc<Counters>) {
    let counters = Arc::new(Counters::default());
    let for_factory = counters.clone();
    let bus = FrameBus::with_collector_factory(
        interval,
        Arc::new(move || {
            for_factory.built.fetch_add(1, Ordering::SeqCst);
            Arc::new(CountingCollector(for_factory.clone())) as Arc<dyn SnapshotCollector>
        }),
    );
    (bus, counters)
}

fn published(host: &str) -> Snapshot {
    Snapshot {
        schema: 1,
        timestamp: "2026-04-20T00:00:00Z".to_string(),
        hostname: host.to_string(),
        gpus: Some(Vec::new()),
        cpus: Some(Vec::new()),
        memory: Some(Vec::new()),
        chassis: Some(Vec::new()),
        processes: None,
        storage: None,
        errors: Vec::new(),
    }
}

/// A stale request that sees a publish within one interval serves that
/// frame and builds no reader. Before issue #503 it constructed a full
/// collector immediately.
#[tokio::test]
async fn a_stale_request_waits_for_the_next_publish_instead_of_collecting() {
    let (bus, counters) = counting_bus(Duration::from_secs(3));
    assert!(bus.latest().await.is_none());

    let resolver = {
        let bus = bus.clone();
        tokio::spawn(async move { resolve_snapshot(&bus, &SectionFilter::default_http()).await })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!resolver.is_finished(), "the request waits for a publish");
    bus.publish(published("from-the-loop")).await;

    let resolved = tokio::time::timeout(Duration::from_secs(5), resolver)
        .await
        .expect("resolves once a frame is published")
        .expect("task")
        .expect("resolved");
    assert!(matches!(resolved, Resolved::Frame(_)));
    assert_eq!(resolved.snapshot().hostname, "from-the-loop");
    assert_eq!(counters.built.load(Ordering::SeqCst), 0);
    assert_eq!(counters.gpu_collects.load(Ordering::SeqCst), 0);
    assert_eq!(
        bus.subscriber_count(),
        0,
        "the temporary receiver is dropped"
    );
}

/// With no publish for a whole interval the request falls back to a fresh
/// collect. The collector is built once, a second request inside the
/// interval reuses the result, and the fallback never reaches the bus.
#[tokio::test(start_paused = true)]
async fn two_stale_requests_within_an_interval_share_one_fresh_collect() {
    let (bus, counters) = counting_bus(Duration::from_secs(3));
    let filter = SectionFilter::default_http();

    let first = resolve_snapshot(&bus, &filter).await.expect("fallback");
    assert!(matches!(first, Resolved::Fallback(_)));
    assert_eq!(first.snapshot().hostname, "fallback");
    assert_eq!(counters.built.load(Ordering::SeqCst), 1);
    assert_eq!(counters.gpu_collects.load(Ordering::SeqCst), 1);

    let second = resolve_snapshot(&bus, &filter).await.expect("fallback");
    assert!(Arc::ptr_eq(first.snapshot(), second.snapshot()));
    assert_eq!(counters.gpu_collects.load(Ordering::SeqCst), 1);

    // A narrower filter is covered by the kept result.
    let narrower = parse_include(Some("gpu"));
    let third = resolve_snapshot(&bus, &narrower).await.expect("fallback");
    assert!(Arc::ptr_eq(first.snapshot(), third.snapshot()));
    assert_eq!(counters.gpu_collects.load(Ordering::SeqCst), 1);

    // A filter that asks for a section the kept result lacks collects
    // again, with the same collector.
    let wider = parse_include(Some("gpu,storage"));
    let fourth = resolve_snapshot(&bus, &wider).await.expect("fallback");
    assert!(fourth.snapshot().storage.is_some());
    assert_eq!(counters.gpu_collects.load(Ordering::SeqCst), 2);
    assert_eq!(counters.storage_collects.load(Ordering::SeqCst), 1);

    // Once the result is older than the interval it is not reused.
    tokio::time::sleep(Duration::from_secs(4)).await;
    resolve_snapshot(&bus, &filter).await.expect("fallback");
    assert_eq!(counters.gpu_collects.load(Ordering::SeqCst), 3);

    assert_eq!(
        counters.built.load(Ordering::SeqCst),
        1,
        "the fallback collector is built once per process"
    );
    assert!(
        bus.latest().await.is_none(),
        "a fallback snapshot must not be published on the bus"
    );
}

/// A burst of concurrent stale requests collects once.
#[tokio::test(start_paused = true)]
async fn a_burst_of_stale_requests_collects_once() {
    let (bus, counters) = counting_bus(Duration::from_secs(3));
    let tasks: Vec<_> = (0..8)
        .map(|_| {
            let bus = bus.clone();
            tokio::spawn(
                async move { resolve_snapshot(&bus, &SectionFilter::default_http()).await },
            )
        })
        .collect();
    for task in tasks {
        let resolved = task.await.expect("task").expect("resolved");
        assert_eq!(resolved.snapshot().hostname, "fallback");
    }
    assert_eq!(counters.built.load(Ordering::SeqCst), 1);
    assert_eq!(counters.gpu_collects.load(Ordering::SeqCst), 1);
}

/// A fresh published frame is served directly, with no wait.
#[tokio::test]
async fn a_fresh_frame_is_served_without_waiting() {
    let (bus, counters) = counting_bus(Duration::from_secs(3));
    bus.publish(published("fresh")).await;
    let resolved = tokio::time::timeout(
        Duration::from_millis(500),
        resolve_snapshot(&bus, &SectionFilter::default_http()),
    )
    .await
    .expect("no wait for a fresh frame")
    .expect("resolved");
    assert_eq!(resolved.snapshot().hostname, "fresh");
    assert_eq!(counters.built.load(Ordering::SeqCst), 0);
}

/// A collector that cannot be built is the 503 path.
#[tokio::test(start_paused = true)]
async fn a_failed_fallback_is_an_error() {
    let bus = FrameBus::with_collector_factory(
        Duration::from_secs(3),
        Arc::new(|| panic!("no readers (expected by this test)")),
    );
    let err = match resolve_snapshot(&bus, &SectionFilter::default_http()).await {
        Ok(_) => panic!("a fallback that cannot build its collector must fail"),
        Err(err) => err,
    };
    assert!(
        err.contains("building the snapshot collector failed"),
        "{err}"
    );
}
