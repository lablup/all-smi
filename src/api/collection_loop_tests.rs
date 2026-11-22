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

//! Tests for the api collection loop (issue #503): tick cadence, keeping
//! the async workers free, surviving a reader panic, and rendering the
//! `/metrics` body once per tick.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use axum::extract::State;
use tokio::sync::RwLock;

use super::{Collected, publish_tick, run_loop_with};
use crate::api::FrameBus;
use crate::api::handlers::{SharedState, metrics_handler};
use crate::api::metrics::render::{
    MetricsRenderInputs, RENDER_COUNT, render_prometheus_exposition,
};
use crate::app_state::AppState;
use crate::device::MemoryInfo;

fn memory() -> MemoryInfo {
    MemoryInfo {
        index: 0,
        host_id: "node-1".to_string(),
        hostname: "node-1".to_string(),
        instance: "node-1".to_string(),
        total_bytes: 64 << 30,
        used_bytes: 16 << 30,
        available_bytes: 48 << 30,
        free_bytes: 40 << 30,
        buffers_bytes: 0,
        cached_bytes: 0,
        swap_total_bytes: 0,
        swap_used_bytes: 0,
        swap_free_bytes: 0,
        utilization: 25.0,
        time: String::new(),
    }
}

fn collected() -> Collected {
    Collected {
        gpus: Vec::new(),
        cpus: Vec::new(),
        memory: vec![memory()],
        processes: Vec::new(),
        chassis: Vec::new(),
        storage: Vec::new(),
    }
}

fn shared_state() -> SharedState {
    Arc::new(RwLock::new(AppState::default()))
}

async fn body_of(state: &SharedState) -> axum::body::Bytes {
    let response = metrics_handler(State(state.clone())).await;
    axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("metrics body")
}

/// Tick start times are `interval` apart even when a collection takes a
/// large part of the interval. With the old trailing sleep the period was
/// `interval + collection time`, 600 ms here.
///
/// Real time on purpose: the read phase runs on the blocking pool, and a
/// paused tokio clock does not advance while a blocking task is running.
#[tokio::test]
async fn tick_period_does_not_include_the_collection_time() {
    const INTERVAL: Duration = Duration::from_millis(400);
    const COLLECT: Duration = Duration::from_millis(200);

    let bus = FrameBus::new(INTERVAL);
    let mut rx = bus.subscribe();
    let task = tokio::spawn(run_loop_with(
        shared_state(),
        bus.clone(),
        INTERVAL,
        "node-1".to_string(),
        false,
        || {
            std::thread::sleep(COLLECT);
            collected()
        },
    ));

    let mut published = Vec::new();
    for _ in 0..3 {
        tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("a frame within 5 s")
            .expect("bus open");
        published.push(Instant::now());
    }
    task.abort();

    for pair in published.windows(2) {
        let gap = pair[1] - pair[0];
        assert!(
            gap >= Duration::from_millis(300) && gap < Duration::from_millis(520),
            "publishes must be about {INTERVAL:?} apart, not interval + collection \
             ({:?}); got {gap:?}",
            INTERVAL + COLLECT
        );
    }
}

/// The read phase must not occupy an async worker: on a current-thread
/// runtime a 10 ms timer still fires on time while a collect blocks for
/// 500 ms. Before issue #503 the readers ran inside the async task, so the
/// timer could not fire until the collect returned.
#[tokio::test(flavor = "current_thread")]
async fn a_blocking_collect_leaves_the_async_worker_free() {
    let bus = FrameBus::new(Duration::from_secs(60));
    let mut rx = bus.subscribe();
    let started = Instant::now();
    let task = tokio::spawn(run_loop_with(
        shared_state(),
        bus.clone(),
        Duration::from_secs(60),
        "node-1".to_string(),
        false,
        || {
            std::thread::sleep(Duration::from_millis(500));
            collected()
        },
    ));
    // Let the loop start its first tick and enter the blocking collect.
    tokio::task::yield_now().await;

    let timer = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(10)).await;
        started.elapsed()
    });
    let fired_after = timer.await.expect("timer task");
    assert!(
        fired_after < Duration::from_millis(250),
        "a 10 ms timer fired after {fired_after:?}; the collect is blocking the worker"
    );

    // The collect was really in flight during that window.
    tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("a frame within 5 s")
        .expect("bus open");
    assert!(started.elapsed() >= Duration::from_millis(450));
    task.abort();
}

/// A panicking reader skips its tick; the loop keeps running and the next
/// tick publishes.
#[tokio::test]
async fn a_panicking_reader_does_not_end_the_loop() {
    let bus = FrameBus::new(Duration::from_millis(20));
    let mut rx = bus.subscribe();
    let state = shared_state();
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_in_loop = calls.clone();
    let task = tokio::spawn(run_loop_with(
        state.clone(),
        bus.clone(),
        Duration::from_millis(20),
        "node-1".to_string(),
        false,
        move || {
            if calls_in_loop.fetch_add(1, Ordering::SeqCst) == 0 {
                panic!("reader exploded (expected by this test)");
            }
            collected()
        },
    ));

    tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("the tick after the panic must publish")
        .expect("bus open");
    assert!(calls.load(Ordering::SeqCst) >= 2);
    assert!(!task.is_finished(), "the loop must survive a reader panic");
    assert!(!state.read().await.loading);
    task.abort();
}

/// `/metrics` serves the body the tick rendered: one render for any number
/// of scrapes, byte-identical to the shared renderer on the same inputs,
/// and `all_smi_up` flips from 0 to 1 with the first tick.
#[tokio::test(flavor = "current_thread")]
async fn metrics_body_is_rendered_once_per_tick_not_per_scrape() {
    let state = shared_state();
    let bus = FrameBus::new(Duration::from_secs(3));

    let before = body_of(&state).await;
    let before = std::str::from_utf8(&before).expect("utf-8");
    assert!(
        before
            .lines()
            .any(|l| l.starts_with("all_smi_up{") && l.ends_with(" 0")),
        "before the first tick the body reports all_smi_up 0:\n{before}"
    );

    let renders_before = RENDER_COUNT.with(|count| count.get());
    publish_tick(&state, &bus, "node-1", false, collected()).await;
    let first = body_of(&state).await;
    let second = body_of(&state).await;
    let renders = RENDER_COUNT.with(|count| count.get()) - renders_before;
    assert_eq!(
        renders, 1,
        "two scrapes after one tick must cost one render"
    );
    assert_eq!(
        first.as_ptr(),
        second.as_ptr(),
        "both scrapes must share the stored body"
    );

    let text = std::str::from_utf8(&first).expect("utf-8");
    assert!(
        text.lines()
            .any(|l| l.starts_with("all_smi_up{") && l.ends_with(" 1")),
        "{text}"
    );
    assert!(text.contains("all_smi_memory_total_bytes{"), "{text}");

    // Byte-identical to the shared renderer on the same inputs, which is
    // what keeps `snapshot --format prometheus` parity.
    let guard = state.read().await;
    let memory = [memory()];
    let expected = render_prometheus_exposition(&MetricsRenderInputs {
        gpu_info: &[],
        process_info: &[],
        cpu_info: &[],
        memory_info: &memory,
        storage_info: &[],
        runtime_environment: &guard.runtime_environment,
        chassis_info: &[],
        vgpu_info: &[],
        mig_info: &[],
        energy_integrator: Some(guard.energy.integrator()),
        ready: true,
    });
    assert_eq!(text, expected);

    // The frame published for the same tick carries the same data.
    let frame = bus.latest().await.expect("published frame");
    assert_eq!(frame.snapshot.memory.as_ref().map(Vec::len), Some(1));
    assert!(frame.snapshot.processes.is_none());
}
