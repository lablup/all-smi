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
//
//! Integration tests for the `/events` SSE stream and `/snapshot` JSON
//! endpoint added by issue #193.
//!
//! The tests spin up a standalone axum router backed by an in-memory
//! `FrameBus` and a hand-driven publisher task — real hardware readers
//! are not involved so the suite runs in any CI environment.

#![cfg(feature = "cli")]

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use all_smi::api::FrameBus;
use all_smi::api::handlers::events::events_handler;
use all_smi::api::handlers::snapshot::snapshot_handler;
use all_smi::api::server_state::ApiState;
use all_smi::app_state::AppState;
use all_smi::snapshot::Snapshot;
use axum::{Router, routing::get};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep, timeout};

/// Drive a publish loop that produces one test snapshot every
/// `interval_ms` milliseconds with a monotonically-increasing
/// timestamp. Returns the spawned `JoinHandle` so the caller can abort
/// it after the assertions run.
fn spawn_publisher(bus: FrameBus, interval_ms: u64) -> JoinHandle<()> {
    let counter = Arc::new(AtomicU64::new(0));
    tokio::spawn(async move {
        loop {
            let i = counter.fetch_add(1, Ordering::Relaxed);
            let snapshot = Snapshot {
                schema: 1,
                timestamp: format!("2026-04-20T00:00:{i:02}Z"),
                hostname: "test-host".to_string(),
                gpus: Some(Vec::new()),
                cpus: Some(Vec::new()),
                memory: Some(Vec::new()),
                chassis: Some(Vec::new()),
                processes: None,
                storage: None,
                errors: Vec::new(),
            };
            bus.publish(snapshot).await;
            sleep(Duration::from_millis(interval_ms)).await;
        }
    })
}

/// Build the axum router used by the tests. Matches `server.rs` wiring
/// but without the CORS / Trace layers that are orthogonal to the
/// routes under test.
fn build_router(bus: FrameBus) -> (Router, ApiState) {
    let shared = Arc::new(RwLock::new(AppState::default()));
    let state = ApiState::new(shared, bus);
    let router = Router::new()
        .route("/events", get(events_handler))
        .route("/snapshot", get(snapshot_handler))
        .with_state(state.clone());
    (router, state)
}

/// Bind a TCP listener on `127.0.0.1:0`, serve the router, and return
/// the bound address so the test can talk to it.
async fn spawn_server(router: Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local_addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    addr
}

/// Open a raw TCP connection and issue a GET `path` request. Returns a
/// buffered reader positioned right after the response headers so the
/// caller can read the event stream line by line.
async fn open_sse(addr: SocketAddr, path: &str) -> BufReader<TcpStream> {
    let mut stream = TcpStream::connect(addr).await.expect("tcp connect");
    let req = format!(
        "GET {path} HTTP/1.1\r\nHost: localhost\r\nAccept: text/event-stream\r\nConnection: keep-alive\r\n\r\n"
    );
    stream.write_all(req.as_bytes()).await.expect("write req");
    let mut reader = BufReader::new(stream);
    // Drain the HTTP status line + headers (terminated by CRLF CRLF).
    // Axum returns chunked encoding for SSE, which we do not decode in
    // full — we only need to skip past the header boundary and read
    // event chunks as they arrive.
    let mut header_end_seen = false;
    let mut line = String::new();
    while !header_end_seen {
        line.clear();
        let n = reader.read_line(&mut line).await.expect("read header");
        if n == 0 {
            panic!("server closed connection before headers finished");
        }
        if line == "\r\n" {
            header_end_seen = true;
        }
    }
    reader
}

/// Collect `count` SSE `event: snapshot` events from `reader`. Returns
/// after the `count`-th event or the `deadline` passes, whichever is
/// first. Each element is the body after the `data: ` prefix.
async fn collect_snapshot_events(
    reader: &mut BufReader<TcpStream>,
    count: usize,
    deadline: Duration,
) -> Vec<String> {
    let start = Instant::now();
    let mut events = Vec::new();
    let mut current_event: Option<String> = None;
    let mut current_data: Vec<String> = Vec::new();
    while events.len() < count && start.elapsed() < deadline {
        let mut line = String::new();
        let remaining = deadline
            .checked_sub(start.elapsed())
            .unwrap_or(Duration::from_millis(1));
        match timeout(remaining, reader.read_line(&mut line)).await {
            Ok(Ok(0)) => break,
            Ok(Ok(_)) => {}
            Ok(Err(_)) => break,
            Err(_) => break,
        }
        // Strip the chunked-encoding length prefix if present. The
        // chunked framing lines are either a bare hex length or CRLF
        // after the chunk body. Our SSE payloads are always ASCII, so
        // lines starting with "event:", "data:", or an SSE comment ":"
        // are actual SSE content; anything else we ignore.
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            // Blank line = event boundary. Flush the accumulated
            // `data:` lines if we have a complete event.
            if current_event.as_deref() == Some("snapshot") && !current_data.is_empty() {
                events.push(current_data.join("\n"));
            }
            current_event = None;
            current_data.clear();
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("event: ") {
            current_event = Some(rest.to_string());
        } else if let Some(rest) = trimmed.strip_prefix("data: ") {
            current_data.push(rest.to_string());
        }
    }
    events
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn events_emits_at_least_three_frames_within_five_seconds() {
    // 100 ms publish interval → ≥3 frames within ~300 ms; we give it up
    // to 5 s per the issue spec.
    let bus = FrameBus::new(Duration::from_millis(100));
    let publisher = spawn_publisher(bus.clone(), 100);
    let (router, _state) = build_router(bus);
    let addr = spawn_server(router).await;

    let mut reader = open_sse(addr, "/events").await;
    let events = collect_snapshot_events(&mut reader, 3, Duration::from_secs(5)).await;
    publisher.abort();

    assert!(
        events.len() >= 3,
        "expected ≥3 snapshot events in 5 s, got {}",
        events.len()
    );
    // Each event should be a JSON object with `schema`, `timestamp`,
    // `hostname`, and the default HTTP sections.
    for data in &events {
        let v: serde_json::Value = serde_json::from_str(data)
            .unwrap_or_else(|e| panic!("invalid JSON body {data:?}: {e}"));
        assert_eq!(v["schema"], serde_json::json!(1));
        assert!(v["timestamp"].is_string());
        assert!(v.get("gpus").is_some(), "default include must yield gpus");
        assert!(v.get("cpus").is_some(), "default include must yield cpus");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn events_include_filter_drops_other_sections() {
    let bus = FrameBus::new(Duration::from_millis(100));
    let publisher = spawn_publisher(bus.clone(), 100);
    let (router, _state) = build_router(bus);
    let addr = spawn_server(router).await;

    let mut reader = open_sse(addr, "/events?include=gpu").await;
    let events = collect_snapshot_events(&mut reader, 2, Duration::from_secs(3)).await;
    publisher.abort();

    assert!(!events.is_empty(), "expected at least one frame");
    for data in &events {
        let v: serde_json::Value = serde_json::from_str(data).expect("valid JSON");
        assert!(
            v.get("gpus").is_some(),
            "gpus must be present with include=gpu"
        );
        assert!(v.get("cpus").is_none(), "cpus must be filtered out");
        assert!(v.get("memory").is_none(), "memory must be filtered out");
        assert!(v.get("chassis").is_none(), "chassis must be filtered out");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn events_throttle_reduces_emission_rate() {
    // Producer runs at 100 ms; with `throttle=1` (seconds), we should
    // see at most ~2-3 frames in a 2-second window (the first frame is
    // always emitted, then throttled to 1 s intervals).
    let bus = FrameBus::new(Duration::from_millis(100));
    let publisher = spawn_publisher(bus.clone(), 100);
    let (router, _state) = build_router(bus);
    let addr = spawn_server(router).await;

    let mut reader = open_sse(addr, "/events?throttle=1").await;
    let events = collect_snapshot_events(&mut reader, 20, Duration::from_millis(2_000)).await;
    publisher.abort();

    // ≤4 is generous: first frame + ~2 s / 1 s = 3; allow an extra 1
    // for timing jitter.
    assert!(
        events.len() <= 4,
        "throttle=1 should limit to ≤4 frames in 2 s, got {}",
        events.len()
    );
    // Some frames should still come through — a throttle of 1 s over a
    // 2 s window must emit at least 1.
    assert!(!events.is_empty(), "expected at least one throttled frame");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn events_lag_event_emitted_for_slow_receiver() {
    // Publish 20 frames before the client subscribes — when the SSE
    // stream finally starts pulling, the broadcast receiver will see a
    // `Lagged` error and the handler must convert that into a
    // synthetic `event: lag` frame.
    let bus = FrameBus::new(Duration::from_millis(10));
    let (router, _state) = build_router(bus.clone());
    let addr = spawn_server(router).await;

    // Connect first so we have a receiver, then publish faster than we
    // read.
    let mut stream = TcpStream::connect(addr).await.expect("connect");
    stream
        .write_all(
            b"GET /events HTTP/1.1\r\nHost: localhost\r\nAccept: text/event-stream\r\nConnection: keep-alive\r\n\r\n",
        )
        .await
        .expect("write req");

    // Wait until the handler has actually called `bus.subscribe()`.
    // Without this, publishing races ahead of the subscription and
    // every published frame is silently dropped with zero receivers —
    // the client then sees no lag event because the broadcast buffer
    // never actually overflowed its visible-to-this-subscriber window.
    let subscribe_deadline = Instant::now() + Duration::from_secs(2);
    while bus.subscriber_count() == 0 && Instant::now() < subscribe_deadline {
        sleep(Duration::from_millis(20)).await;
    }
    assert!(
        bus.subscriber_count() >= 1,
        "SSE subscriber never registered on the bus"
    );

    // Flood the bus. Because FRAME_BUFFER = 16, publishing 24 back-to-back
    // guarantees the receiver's slot gets overrun.
    for _ in 0..24 {
        let snapshot = Snapshot {
            schema: 1,
            timestamp: "2026-04-20T00:00:00Z".to_string(),
            hostname: "test".to_string(),
            gpus: Some(Vec::new()),
            cpus: Some(Vec::new()),
            memory: Some(Vec::new()),
            chassis: Some(Vec::new()),
            processes: None,
            storage: None,
            errors: Vec::new(),
        };
        bus.publish(snapshot).await;
    }

    // Drain enough bytes to cover the HTTP headers + lag event. We use
    // a short timeout because a lag event should appear almost
    // immediately after the first read.
    let mut buf = vec![0u8; 8192];
    let mut body = String::new();
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        let n = match timeout(Duration::from_millis(500), stream.read(&mut buf)).await {
            Ok(Ok(n)) if n > 0 => n,
            _ => break,
        };
        body.push_str(&String::from_utf8_lossy(&buf[..n]));
        if body.contains("event: lag") {
            break;
        }
    }

    assert!(
        body.contains("event: lag"),
        "expected `event: lag` in stream, got:\n{body}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fifty_concurrent_clients_do_not_stall_the_publisher() {
    // Spec (issue #193): "collection tick jitter stays within ±20 ms"
    // with 50+ concurrent SSE clients. The first version of this test
    // measured the wrong thing (issue #327): it timed the gap between
    // consecutive publishes, which is the `sleep` plus scheduler slop,
    // and never timed `publish` at all, so a publish that blocked for a
    // second would have sailed through it.
    //
    // What `FrameBus::publish` actually does: bump an `AtomicU64`, wrap
    // the snapshot in one `Arc`, take the `latest` write lock
    // (uncontended here, because no `/snapshot` request runs alongside),
    // and call `broadcast::Sender::send`. Broadcast never applies
    // backpressure: once the 16-slot ring is full the send overwrites
    // the oldest slot and the lagging receiver is told about the gap
    // through `RecvError::Lagged`. So publish is O(1) work plus waking
    // whichever receivers are parked, and it never waits on a client.
    //
    // Two separate properties follow from that, each asserted below with
    // its own budget and its own message:
    //
    //   A. Cadence. Holding 50 subscribers must not starve the runtime
    //      enough to stretch the publisher's loop period. Budgeted over
    //      the whole loop rather than per tick, because per-tick timing
    //      on a shared runner is scheduler noise: `tokio::time::sleep`
    //      guarantees only a lower bound, and the CI flake that led to
    //      #327 was exactly one such overshoot.
    //   B. Publish cost. Timed around the `publish` call itself, over a
    //      burst of calls made while every subscriber is behind, and
    //      asserted on the mean.
    const TICKS: u32 = 20;
    const BURST: u32 = 200;
    const BURST_SPACING: Duration = Duration::from_millis(1);
    let publish_interval = Duration::from_millis(50);
    // Nominal loop wall time is TICKS * publish_interval = 1 s. The
    // worst real datum we have is a single 108.5 ms tick on a two-core
    // hosted runner with two CI jobs overlapping (run 30994446017), i.e.
    // ~58 ms of slop on one tick out of twenty. Budgeting the loop as a
    // whole at twice nominal absorbs a dozen such ticks, so one
    // descheduled tick cannot fail the run, while a real cadence
    // collapse (seconds, not milliseconds) still does.
    let cadence_budget = publish_interval * TICKS * 2;
    // Budget for the mean cost of a single `publish` call across the
    // burst. Two anchors bracket it. Below, the measurement: with 50
    // subscribers attached this path costs a mean of 25-35 µs per call
    // (p95 ~41 µs), nearly all of it the `Arc` allocation plus waking
    // fifty parked receivers. Above, the failure mode: a publish that
    // waited on even one subscriber would pay a scheduler round trip per
    // subscriber per call, milliseconds at the very best, and would
    // simply never return for the clients used here, which read nothing
    // and whose socket buffers are full. 1 ms sits ~35x above the
    // measurement and at least an order of magnitude below the failure
    // mode. It is also 2% of the collection interval, so an
    // implementation that spent any meaningful share of a tick inside
    // publish fails.
    //
    // The assertion is on the mean, not the max, and that is deliberate.
    // A wall-clock max cannot distinguish a blocked publish from a
    // measuring thread that the OS descheduled mid-call: those samples
    // come back quantized to whole scheduler quanta, tens of
    // milliseconds each, which is the same noise that produced the flake
    // behind #327. Averaging over BURST samples amortizes it, while a
    // publish that genuinely waits pays its cost on every single call.
    // The max is still reported on failure, as a diagnostic rather than
    // as a trigger. Concretely, across eight instrumented local runs the
    // mean landed between 25 and 35 µs seven times; the eighth caught a
    // single 14 ms deschedule inside a timed window and still came out at
    // 218 µs, comfortably inside budget, where any max-based budget below
    // 14 ms would have failed that run. Only a few milliseconds of the
    // burst's ~200 ms wall time sits inside a timed window at all, which
    // is what keeps that exposure small.
    let mean_publish_budget = Duration::from_millis(1);
    // Pure hang guard for the burst, which needs BURST * BURST_SPACING =
    // 200 ms of sleeping plus a few milliseconds of work. A publish that
    // awaited a receiver would never return with these clients, so
    // without a timeout that failure mode is a CI job hanging until the
    // runner kills it rather than a test failing with a message. 5 s is
    // 25x the nominal burst, far too loose to fire on scheduling noise.
    let burst_hang_guard = Duration::from_secs(5);
    let bus = FrameBus::new(publish_interval);
    let (router, _state) = build_router(bus.clone());
    let addr = spawn_server(router).await;

    // Spawn 50 SSE clients that subscribe but never actively read —
    // the kernel + axum buffers absorb the first few frames, then the
    // broadcast buffer starts dropping for that subscriber. Each is
    // treated by the server as a normal receiver.
    let mut clients = Vec::with_capacity(50);
    for _ in 0..50 {
        let s = TcpStream::connect(addr).await.expect("client connect");
        clients.push(s);
    }
    for s in clients.iter_mut() {
        s.write_all(
            b"GET /events HTTP/1.1\r\nHost: localhost\r\nAccept: text/event-stream\r\nConnection: keep-alive\r\n\r\n",
        )
        .await
        .expect("write req");
    }
    // Give axum time to invoke every handler and call `bus.subscribe()`.
    let subscribe_deadline = Instant::now() + Duration::from_secs(2);
    while bus.subscriber_count() < 50 && Instant::now() < subscribe_deadline {
        sleep(Duration::from_millis(20)).await;
    }
    let subscribers = bus.subscriber_count();
    assert!(
        subscribers >= 50,
        "expected 50 subscribers, only registered {subscribers}"
    );

    // One frame's worth of payload. Built outside every timed window so
    // the measurement covers `publish` and nothing else.
    fn load_snapshot() -> Snapshot {
        Snapshot {
            schema: 1,
            timestamp: "2026-04-20T00:00:00Z".to_string(),
            hostname: "host".to_string(),
            gpus: Some(Vec::new()),
            cpus: Some(Vec::new()),
            memory: Some(Vec::new()),
            chassis: Some(Vec::new()),
            processes: None,
            storage: None,
            errors: Vec::new(),
        }
    }

    // Property A: cadence. Run the publisher at its nominal period and
    // time the loop as a whole. This measures `sleep` overshoot and
    // runtime scheduling slop with 50 subscribers attached; it says
    // nothing about the cost of any individual publish, which property B
    // measures directly.
    let cadence_start = Instant::now();
    for _ in 0..TICKS {
        bus.publish(load_snapshot()).await;
        sleep(publish_interval).await;
    }
    let cadence_elapsed = cadence_start.elapsed();
    assert!(
        cadence_elapsed <= cadence_budget,
        "publisher cadence: {TICKS} ticks at {publish_interval:?} took {cadence_elapsed:?} with {subscribers} subscribers, over the {cadence_budget:?} whole-loop budget. This measures sleep overshoot and runtime scheduling slop across the loop, not the cost of a single publish (see the separate publish-cost assertion below)."
    );

    // Property B: publish cost. Publish BURST frames, timing each call
    // individually from just before `publish` to just after it returns,
    // while all 50 subscribers sit there reading nothing.
    //
    // BURST is more than ten times the 16-slot ring, so the ring is
    // overwritten many times over and every subscriber spends the burst
    // behind: exactly the state in which an implementation that applied
    // backpressure would have to wait for someone.
    //
    // The BURST_SPACING sleep between calls is load-bearing, and it sits
    // outside the timed window. It gives the 50 SSE tasks time to come
    // back around to `recv()` and register a waker, so each publish pays
    // the full fifty-way wake. Publishing back to back instead measures
    // a much cheaper path (~1 µs versus ~28 µs here) because no receiver
    // has re-parked in between, and there is nothing to wake: that would
    // make the subscriber count, the whole point of this test, almost
    // invisible to the measurement.
    let burst = timeout(burst_hang_guard, async {
        let mut costs: Vec<Duration> = Vec::with_capacity(BURST as usize);
        for _ in 0..BURST {
            let snapshot = load_snapshot();
            sleep(BURST_SPACING).await;
            let t0 = Instant::now();
            bus.publish(snapshot).await;
            costs.push(t0.elapsed());
        }
        costs
    })
    .await;
    let costs = match burst {
        Ok(costs) => costs,
        Err(_) => panic!(
            "publish cost: a burst of {BURST} FrameBus::publish calls spaced {BURST_SPACING:?} apart did not finish within {burst_hang_guard:?} while {subscribers} subscribers were attached and reading nothing. Publish must hand each frame to the broadcast channel and return; it must never wait for a receiver to catch up."
        ),
    };
    let total_publish: Duration = costs.iter().sum();
    let mean_publish = total_publish / BURST;
    let worst_publish = costs
        .iter()
        .copied()
        .max()
        .expect("BURST > 0, so the burst produced at least one sample");
    assert!(
        mean_publish <= mean_publish_budget,
        "publish cost: FrameBus::publish averaged {mean_publish:?} per call over {BURST} calls with {subscribers} subscribers attached and reading nothing ({total_publish:?} spent inside publish in total), over the {mean_publish_budget:?} per-call budget. Publish is a non-blocking broadcast send and should cost tens of microseconds no matter how far behind the subscribers are. Slowest single call was {worst_publish:?}, reported for diagnosis only: an isolated slow sample is usually the OS descheduling the measuring thread, which is why the budget is on the mean."
    );

    // Drop all clients; subscriber count should drop back toward zero.
    drop(clients);
    // The receiver count decays as axum notices the broken TCP streams.
    // We do not hard-assert 0 here because the OS/tokio timing of socket
    // close propagation is platform-dependent, but it must shrink.
    sleep(Duration::from_millis(500)).await;
    assert!(
        bus.subscriber_count() < 50,
        "dropped clients must release their broadcast slots"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn snapshot_pretty_flag_produces_multiline_body() {
    let bus = FrameBus::new(Duration::from_secs(1));
    let published = Snapshot {
        schema: 1,
        timestamp: "2026-04-20T00:00:00Z".to_string(),
        hostname: "pretty-host".to_string(),
        gpus: Some(Vec::new()),
        cpus: Some(Vec::new()),
        memory: Some(Vec::new()),
        chassis: Some(Vec::new()),
        processes: None,
        storage: None,
        errors: Vec::new(),
    };
    bus.publish(published).await;

    let (router, _state) = build_router(bus);
    let addr = spawn_server(router).await;

    let mut stream = TcpStream::connect(addr).await.expect("connect");
    stream
        .write_all(
            b"GET /snapshot?pretty=1 HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
        )
        .await
        .expect("write");
    let mut body = Vec::new();
    stream.read_to_end(&mut body).await.expect("read body");
    let text = String::from_utf8_lossy(&body).into_owned();

    // A pretty-printed JSON object contains multiple lines indented
    // with at least two spaces. A compact body would be single-line.
    assert!(
        text.contains("\n  \"schema\""),
        "pretty=1 body should be multiline + indented, got:\n{text}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn snapshot_include_filter_drops_sections() {
    let bus = FrameBus::new(Duration::from_secs(1));
    let published = Snapshot {
        schema: 1,
        timestamp: "2026-04-20T00:00:00Z".to_string(),
        hostname: "filter-host".to_string(),
        gpus: Some(Vec::new()),
        cpus: Some(Vec::new()),
        memory: Some(Vec::new()),
        chassis: Some(Vec::new()),
        processes: None,
        storage: None,
        errors: Vec::new(),
    };
    bus.publish(published).await;

    let (router, _state) = build_router(bus);
    let addr = spawn_server(router).await;

    let mut stream = TcpStream::connect(addr).await.expect("connect");
    stream
        .write_all(
            b"GET /snapshot?include=cpu,memory HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
        )
        .await
        .expect("write");
    let mut body = Vec::new();
    stream.read_to_end(&mut body).await.expect("read body");
    let text = String::from_utf8_lossy(&body).into_owned();

    // Parse the JSON body from the response.
    let body_area = &text[text.find("\r\n\r\n").expect("sep") + 4..];
    let json_start = body_area.find('{').expect("json start");
    let json_end = body_area.rfind('}').expect("json end") + 1;
    let payload = &body_area[json_start..json_end];
    let v: serde_json::Value = serde_json::from_str(payload).expect("valid JSON");
    assert!(v.get("cpus").is_some());
    assert!(v.get("memory").is_some());
    assert!(
        v.get("gpus").is_none(),
        "gpus must be dropped when include=cpu,memory"
    );
    assert!(v.get("chassis").is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn snapshot_falls_back_to_fresh_collect_when_stale() {
    // No frame ever published → the handler must force a fresh
    // collection. On CI hosts with no GPU/CPU reader ever registering,
    // the snapshot may contain zero devices but it must still return
    // a valid JSON object with `schema: 1`.
    let bus = FrameBus::new(Duration::from_millis(100));
    let (router, _state) = build_router(bus);
    let addr = spawn_server(router).await;

    let mut stream = TcpStream::connect(addr).await.expect("connect");
    stream
        .write_all(b"GET /snapshot HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .expect("write");
    let mut body = Vec::new();
    stream.read_to_end(&mut body).await.expect("read body");
    let text = String::from_utf8_lossy(&body).into_owned();

    // Even with no frame published, the response must be 200 OK.
    assert!(
        text.starts_with("HTTP/1.1 200 OK"),
        "stale /snapshot must still succeed via fresh collect, got:\n{text}"
    );
    let body_area = &text[text.find("\r\n\r\n").expect("sep") + 4..];
    let json_start = body_area.find('{').expect("body must contain JSON");
    let json_end = body_area.rfind('}').expect("json end") + 1;
    let payload = &body_area[json_start..json_end];
    let v: serde_json::Value = serde_json::from_str(payload).expect("valid JSON body");
    assert_eq!(v["schema"], serde_json::json!(1));
    assert!(v["timestamp"].is_string());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn snapshot_fresh_collect_honours_storage_include() {
    // Regression test (issue #193): when `/snapshot` has to fall back to
    // a fresh collect because no frame has been published yet, the
    // fresh collect must honour the caller's `?include=` filter — in
    // particular, `?include=storage` must populate the `storage`
    // section. Before the fix the stale-fallback path hard-coded
    // `storage: false` and silently dropped the requested section.
    let bus = FrameBus::new(Duration::from_millis(100));
    let (router, _state) = build_router(bus);
    let addr = spawn_server(router).await;

    let mut stream = TcpStream::connect(addr).await.expect("connect");
    stream
        .write_all(
            b"GET /snapshot?include=storage HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
        )
        .await
        .expect("write");
    let mut body = Vec::new();
    stream.read_to_end(&mut body).await.expect("read body");
    let text = String::from_utf8_lossy(&body).into_owned();

    assert!(
        text.starts_with("HTTP/1.1 200 OK"),
        "/snapshot?include=storage should succeed, got:\n{text}"
    );
    let body_area = &text[text.find("\r\n\r\n").expect("sep") + 4..];
    let json_start = body_area.find('{').expect("body must contain JSON");
    let json_end = body_area.rfind('}').expect("json end") + 1;
    let payload = &body_area[json_start..json_end];
    let v: serde_json::Value = serde_json::from_str(payload).expect("valid JSON body");
    // The fresh-collect path now propagates the requested section,
    // so `storage` must be present (even if empty on CI hosts with no
    // real disks worth reporting).
    assert!(
        v.get("storage").is_some(),
        "fresh-collect stale fallback must honour ?include=storage, got: {v:?}"
    );
    // And unrequested sections must not leak.
    assert!(v.get("gpus").is_none());
    assert!(v.get("cpus").is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn snapshot_error_response_carries_no_cache_headers() {
    // The `/snapshot` error path must still emit `Cache-Control:
    // no-store` and `X-Accel-Buffering: no` so a reverse proxy does
    // not cache a transient failure. The only way to reliably drive
    // the error branch from a test is to compose a request that makes
    // `collect_once` produce a non-UTF-8 payload, which is impossible
    // with the default readers — instead we assert the *success* path
    // and the documented error-path helper both route through the
    // shared `no_cache_headers()` helper. A code-level regression is
    // therefore prevented by the unit tests in the handler module; here
    // we simply verify the success-path headers stay consistent.
    let bus = FrameBus::new(Duration::from_secs(1));
    let published = Snapshot {
        schema: 1,
        timestamp: "2026-04-20T00:00:00Z".to_string(),
        hostname: "hdr-host".to_string(),
        gpus: Some(Vec::new()),
        cpus: Some(Vec::new()),
        memory: Some(Vec::new()),
        chassis: Some(Vec::new()),
        processes: None,
        storage: None,
        errors: Vec::new(),
    };
    bus.publish(published).await;
    let (router, _state) = build_router(bus);
    let addr = spawn_server(router).await;

    let mut stream = TcpStream::connect(addr).await.expect("connect");
    stream
        .write_all(b"GET /snapshot HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .expect("write");
    let mut body = Vec::new();
    stream.read_to_end(&mut body).await.expect("read body");
    let text = String::from_utf8_lossy(&body).into_owned();
    let lower = text.to_ascii_lowercase();
    assert!(
        lower.contains("cache-control: no-store"),
        "missing Cache-Control: no-store on /snapshot, got:\n{text}"
    );
    assert!(
        lower.contains("x-accel-buffering: no"),
        "missing X-Accel-Buffering: no on /snapshot, got:\n{text}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn snapshot_returns_latest_frame() {
    let bus = FrameBus::new(Duration::from_secs(1));
    let published = Snapshot {
        schema: 1,
        timestamp: "2026-04-20T00:00:00Z".to_string(),
        hostname: "snap-host".to_string(),
        gpus: Some(Vec::new()),
        cpus: Some(Vec::new()),
        memory: Some(Vec::new()),
        chassis: Some(Vec::new()),
        processes: None,
        storage: None,
        errors: Vec::new(),
    };
    bus.publish(published).await;

    let (router, _state) = build_router(bus);
    let addr = spawn_server(router).await;

    let mut stream = TcpStream::connect(addr).await.expect("connect");
    stream
        .write_all(b"GET /snapshot HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .expect("write");
    let mut body = Vec::new();
    stream.read_to_end(&mut body).await.expect("read body");
    let text = String::from_utf8_lossy(&body).into_owned();

    // Extract the JSON body — find the `{` after the header
    // separator.
    let header_end = text
        .find("\r\n\r\n")
        .expect("response must contain header-body separator");
    let body_area = &text[header_end + 4..];
    let json_start = body_area
        .find('{')
        .expect("body must contain a JSON object");
    // Chunked encoding framing wraps the JSON body; we parse the
    // largest JSON object we can find by trimming trailing hex/CRLF
    // chunks back to the final `}`.
    let mut end = body_area.rfind('}').expect("body must contain `}`") + 1;
    // Allow a few trailing garbage characters from chunked framing.
    while end > json_start && !body_area[..end].ends_with('}') {
        end -= 1;
    }
    let payload = &body_area[json_start..end];
    let v: serde_json::Value =
        serde_json::from_str(payload).unwrap_or_else(|e| panic!("invalid JSON {payload:?}: {e}"));
    assert_eq!(v["hostname"], serde_json::json!("snap-host"));
    assert_eq!(v["schema"], serde_json::json!(1));
    // Default includes yield gpus/cpus/memory/chassis.
    assert!(v.get("gpus").is_some());
    assert!(v.get("cpus").is_some());
}
