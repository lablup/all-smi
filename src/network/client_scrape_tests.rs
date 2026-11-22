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

//! Scrape-tick latency bounds and per-host backoff (issue #500).
//!
//! Every server here is a local listener; the time margins are several
//! times the configured timeouts so the assertions do not depend on
//! scheduler jitter.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use axum::{Router, routing::get};
use tokio::sync::Semaphore;

use super::NetworkClient;
use crate::app_state::ConnectionStatus;
use crate::common::http_hosts::http_host_identifier;
use crate::network::host_backoff::ScrapeTimeouts;

const METRICS: &str = r#"
all_smi_gpu_info{gpu="Healthy GPU",instance="node-ok",gpu_uuid="GPU-OK",gpu_index="0",type="GPU"} 1
all_smi_gpu_memory_total_bytes{gpu="Healthy GPU",instance="node-ok",gpu_uuid="GPU-OK",gpu_index="0"} 17179869184
all_smi_gpu_utilization{gpu="Healthy GPU",instance="node-ok",gpu_uuid="GPU-OK",gpu_index="0"} 42
"#;

async fn spawn_healthy_server() -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = Router::new().route("/metrics", get(|| async { METRICS }));
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    addr
}

/// A listener that accepts connections and never writes a byte, counting
/// every accept. Accepted sockets are held open for the test's lifetime.
async fn spawn_hung_server() -> (SocketAddr, Arc<AtomicUsize>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let accepts = Arc::new(AtomicUsize::new(0));
    let counter = accepts.clone();
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((socket, _)) = listener.accept().await {
            counter.fetch_add(1, Ordering::SeqCst);
            held.push(socket);
        }
    });
    (addr, accepts)
}

fn status_for<'a>(statuses: &'a [ConnectionStatus], host: &str) -> &'a ConnectionStatus {
    let host_id = http_host_identifier(host);
    statuses
        .iter()
        .find(|s| s.host_id == host_id)
        .unwrap_or_else(|| panic!("no status for {host}: {statuses:?}"))
}

/// State a closed localhost port reports. Windows retries a refused
/// connect for about two seconds, longer than the test connect timeout,
/// so there the attempt ends as a timeout instead of a refusal.
fn refused_port_state() -> &'static str {
    if cfg!(windows) {
        "timeout"
    } else {
        "disconnected"
    }
}

fn short_timeouts() -> ScrapeTimeouts {
    ScrapeTimeouts {
        connect: Duration::from_millis(200),
        request: Duration::from_millis(300),
        budget: Duration::from_secs(1),
    }
}

#[tokio::test]
async fn hung_host_times_out_then_backs_off_without_delaying_the_tick() {
    let healthy = format!("http://{}", spawn_healthy_server().await);
    let (hung_addr, accepts) = spawn_hung_server().await;
    let hung = format!("http://{hung_addr}");
    let hosts = vec![healthy.clone(), hung.clone()];
    let client = NetworkClient::with_timeouts(short_timeouts());
    let semaphore = Arc::new(Semaphore::new(8));

    for tick in 1..=2u32 {
        let data = client.fetch_remote_metrics(&hosts, &semaphore).await;
        assert_eq!(data.connection_statuses.len(), 2, "tick {tick}");
        assert!(
            data.gpu_info.iter().any(|gpu| gpu.uuid == "GPU-OK"),
            "tick {tick}: healthy host's GPU must still be returned"
        );
        assert!(status_for(&data.connection_statuses, &healthy).is_connected);
        let status = status_for(&data.connection_statuses, &hung);
        assert!(!status.is_connected, "tick {tick}");
        assert_eq!(
            status.connection_state.as_deref(),
            Some("timeout"),
            "tick {tick}"
        );
        assert_eq!(status.consecutive_failures, tick, "tick {tick}");
    }
    // A timed-out attempt is not retried: one connection per tick.
    let accepts_before = accepts.load(Ordering::SeqCst);
    assert_eq!(accepts_before, 2);

    let started = Instant::now();
    let data = client.fetch_remote_metrics(&hosts, &semaphore).await;
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_millis(250),
        "a tick with the dead host in backoff took {elapsed:?}"
    );
    let status = status_for(&data.connection_statuses, &hung);
    assert!(!status.is_connected);
    assert_eq!(status.connection_state.as_deref(), Some("backing-off"));
    assert_eq!(status.consecutive_failures, 2);
    assert!(
        status
            .last_error
            .as_deref()
            .is_some_and(|e| e.contains("2 consecutive failures")),
        "{status:?}"
    );
    assert!(data.gpu_info.iter().any(|gpu| gpu.uuid == "GPU-OK"));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        accepts.load(Ordering::SeqCst),
        accepts_before,
        "a host in backoff must not be contacted"
    );
}

#[tokio::test]
async fn backoff_expires_success_clears_it_and_removed_hosts_are_dropped() {
    let healthy = format!("http://{}", spawn_healthy_server().await);
    // Reserve a port and close it so connecting fails fast (refused).
    let refused_addr = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap()
    };
    let refused = format!("http://{refused_addr}");
    let refused_id = http_host_identifier(&refused);
    let client = NetworkClient::with_timeouts(short_timeouts());
    let semaphore = Arc::new(Semaphore::new(8));

    let hosts = vec![refused.clone()];
    for _ in 0..2 {
        let data = client.fetch_remote_metrics(&hosts, &semaphore).await;
        let status = status_for(&data.connection_statuses, &refused);
        assert_eq!(
            status.connection_state.as_deref(),
            Some(refused_port_state())
        );
    }
    let entry = client.backoff_entry(&refused_id).expect("backoff entry");
    assert_eq!(entry.consecutive_failures, 2);
    let data = client.fetch_remote_metrics(&hosts, &semaphore).await;
    assert_eq!(
        status_for(&data.connection_statuses, &refused)
            .connection_state
            .as_deref(),
        Some("backing-off")
    );

    // Once the window has passed the host is contacted again.
    client.expire_backoff_for_tests();
    let data = client.fetch_remote_metrics(&hosts, &semaphore).await;
    let status = status_for(&data.connection_statuses, &refused);
    assert_eq!(
        status.connection_state.as_deref(),
        Some(refused_port_state())
    );
    assert_eq!(status.consecutive_failures, 3);

    // Removing the host from the configuration drops its history.
    let data = client
        .fetch_remote_metrics(std::slice::from_ref(&healthy), &semaphore)
        .await;
    assert_eq!(data.connection_statuses.len(), 1);
    assert!(client.backoff_entry(&refused_id).is_none());

    // A success clears the history of a host that had failed before.
    let healthy_id = http_host_identifier(&healthy);
    client.record_failure_for_tests(&healthy_id);
    assert!(client.backoff_entry(&healthy_id).is_some());
    let data = client
        .fetch_remote_metrics(std::slice::from_ref(&healthy), &semaphore)
        .await;
    assert!(status_for(&data.connection_statuses, &healthy).is_connected);
    assert!(client.backoff_entry(&healthy_id).is_none());
}

#[tokio::test]
async fn host_pending_at_the_deadline_reports_timeout() {
    // Request timeout longer than the budget, so the deadline ends the tick.
    let (hung_addr, _) = spawn_hung_server().await;
    let hung = format!("http://{hung_addr}");
    let client = NetworkClient::with_timeouts(ScrapeTimeouts {
        connect: Duration::from_millis(200),
        request: Duration::from_secs(5),
        budget: Duration::from_millis(300),
    });
    let semaphore = Arc::new(Semaphore::new(8));
    let started = Instant::now();
    let data = client
        .fetch_remote_metrics(std::slice::from_ref(&hung), &semaphore)
        .await;
    assert!(started.elapsed() < Duration::from_secs(3));
    let status = status_for(&data.connection_statuses, &hung);
    assert!(!status.is_connected);
    assert_eq!(status.connection_state.as_deref(), Some("timeout"));
    assert!(
        status
            .last_error
            .as_deref()
            .is_some_and(|e| e.contains("within 300 ms")),
        "{status:?}"
    );
}
