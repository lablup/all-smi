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

//! Integration test for the alert-webhook pipeline.
//!
//! Spins up a minimal TCP server, has the webhook worker POST a payload
//! to it, and asserts on the body bytes. The server is not a full
//! HTTP/1.1 implementation — it only parses enough to extract the body —
//! which keeps the test dependency-free beyond the existing `tokio` +
//! `serde_json` set.

use std::time::Duration;

use all_smi::network::webhook::{enqueue, spawn_webhook_worker};
use all_smi::ui::alerts::WebhookPayload;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::time::timeout;

/// Spawn a minimal HTTP echo server that reports the first request body
/// it receives back over a oneshot channel.
async fn spawn_capture_server() -> (String, oneshot::Receiver<Vec<u8>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("http://{addr}/alerts");
    let (tx, rx) = oneshot::channel::<Vec<u8>>();

    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = Vec::new();
        // Read until we can see the body. Simplest approach: read a
        // fixed block, then parse out the body by finding the first
        // "\r\n\r\n". The payloads are small so 4 KiB is plenty.
        let mut tmp = [0u8; 4096];
        loop {
            match socket.read(&mut tmp).await {
                Ok(0) => break,
                Ok(n) => {
                    buf.extend_from_slice(&tmp[..n]);
                    if let Some(pos) = find_double_crlf(&buf) {
                        // Work out Content-Length.
                        let headers = &buf[..pos];
                        let body_start = pos + 4;
                        let content_length = parse_content_length(headers).unwrap_or(0);
                        while buf.len() < body_start + content_length {
                            let n2 = socket.read(&mut tmp).await.unwrap();
                            if n2 == 0 {
                                break;
                            }
                            buf.extend_from_slice(&tmp[..n2]);
                        }
                        let body = buf[body_start..body_start + content_length].to_vec();
                        let _ = socket
                            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                            .await;
                        let _ = tx.send(body);
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    (url, rx)
}

fn find_double_crlf(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

fn parse_content_length(headers: &[u8]) -> Option<usize> {
    let s = std::str::from_utf8(headers).ok()?;
    for line in s.lines() {
        if let Some(value) = line.strip_prefix("Content-Length:") {
            return value.trim().parse().ok();
        }
        if let Some(value) = line.strip_prefix("content-length:") {
            return value.trim().parse().ok();
        }
    }
    None
}

#[tokio::test]
async fn webhook_body_matches_expected_shape() {
    let (url, body_rx) = spawn_capture_server().await;

    let tx = spawn_webhook_worker(url);
    let payload = WebhookPayload {
        timestamp: "2026-04-20T12:34:56+00:00".to_string(),
        host: "dgx-01".to_string(),
        gpu_index: Some(3),
        rule: "temperature".to_string(),
        from: "warn".to_string(),
        to: "crit".to_string(),
        value: 92.0,
        threshold: 90.0,
    };
    assert!(enqueue(&tx, payload.clone()));

    let body = timeout(Duration::from_secs(5), body_rx)
        .await
        .expect("server did not receive body in time")
        .expect("oneshot dropped");
    let got: WebhookPayload = serde_json::from_slice(&body).expect("invalid JSON body");
    assert_eq!(got.timestamp, payload.timestamp);
    assert_eq!(got.host, payload.host);
    assert_eq!(got.gpu_index, payload.gpu_index);
    assert_eq!(got.rule, payload.rule);
    assert_eq!(got.from, payload.from);
    assert_eq!(got.to, payload.to);
    assert_eq!(got.value, payload.value);
    assert_eq!(got.threshold, payload.threshold);
}
