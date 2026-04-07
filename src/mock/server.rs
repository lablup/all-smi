//! HTTP server implementation for the mock server

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

use anyhow::Result;
use futures_util::future::join_all;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use hyper_util::server::conn::auto::Builder;
use rand::{RngExt, rng};
use std::collections::HashMap;
use std::convert::Infallible;
use std::fs::File;
use std::io::Write;
use std::net::SocketAddr;
use std::ops::RangeInclusive;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tokio::time::interval;

use crate::mock::Args;
use crate::mock::constants::{MAX_CONNECTIONS_PER_SERVER, UPDATE_INTERVAL_SECS};
use crate::mock::metrics::PlatformType;
use crate::mock::node::MockNode;

/// Parse port range from string (e.g., "10001-10010" or "10001")
pub fn parse_port_range(range_str: &str) -> Result<RangeInclusive<u16>> {
    if let Some((start, end)) = range_str.split_once('-') {
        Ok(start.parse()?..=end.parse()?)
    } else {
        let port = range_str.parse()?;
        Ok(port..=port)
    }
}

/// Handle incoming HTTP request
pub async fn handle_request(
    _req: Request<hyper::body::Incoming>,
    nodes: Arc<Mutex<HashMap<u16, MockNode>>>,
    port: u16,
) -> Result<Response<String>, Infallible> {
    // Check if node is responding and copy response data
    let (is_responding, metrics) = {
        let nodes_guard = nodes.lock().unwrap();
        let node = nodes_guard.get(&port).unwrap();
        (node.is_responding, node.get_response().to_string())
    };

    // If node is not responding, simulate a connection timeout/error
    if !is_responding {
        // Return a 503 Service Unavailable to simulate failure
        let response = Response::builder()
            .status(503)
            .header("Content-Type", "text/plain; charset=utf-8")
            .body("Service temporarily unavailable".to_string())
            .unwrap();
        return Ok(response);
    }

    // Build optimized HTTP response with performance headers
    let response = Response::builder()
        .status(200)
        .header("Content-Type", "text/plain; charset=utf-8")
        .header("Cache-Control", "max-age=2, must-revalidate") // Cache for 2 seconds
        .header("Connection", "keep-alive") // Enable connection reuse
        .header("Keep-Alive", "timeout=60, max=1000") // Keep connections alive
        .header("Content-Length", metrics.len().to_string()) // Explicit content length
        .body(metrics)
        .unwrap();

    Ok(response)
}

/// Start background updater task that updates all nodes every UPDATE_INTERVAL_SECS
pub fn start_updater_task(
    nodes: Arc<Mutex<HashMap<u16, MockNode>>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = interval(Duration::from_secs(UPDATE_INTERVAL_SECS));
        loop {
            interval.tick().await;
            let mut nodes_guard = nodes.lock().unwrap();
            for node in nodes_guard.values_mut() {
                node.update();
            }
        }
    })
}

/// Start failure simulation task if failure_nodes > 0
pub fn start_failure_task(
    nodes: Arc<Mutex<HashMap<u16, MockNode>>>,
    failure_count: u32,
) -> Option<tokio::task::JoinHandle<()>> {
    if failure_count == 0 {
        return None;
    }

    Some(tokio::spawn(async move {
        let mut interval = interval(Duration::from_secs(10)); // Every 10 seconds
        loop {
            interval.tick().await;
            let mut rng = rng(); // Create RNG inside the loop to avoid Send issues
            let mut nodes_guard = nodes.lock().unwrap();
            let port_list: Vec<u16> = nodes_guard.keys().cloned().collect();

            if port_list.len() as u32 >= failure_count {
                // Randomly select nodes to fail
                let mut selected_ports = Vec::new();
                while selected_ports.len() < failure_count as usize {
                    let port = port_list[rng.random_range(0..port_list.len())];
                    if !selected_ports.contains(&port) {
                        selected_ports.push(port);
                    }
                }

                // Toggle failure state for all nodes
                for (port, node) in nodes_guard.iter_mut() {
                    if selected_ports.contains(port) {
                        // Randomly fail/recover selected nodes
                        node.is_responding = rng.random_bool(0.3); // 30% chance to be responding
                    } else {
                        // Non-selected nodes have higher chance to be responding
                        node.is_responding = rng.random_bool(0.9); // 90% chance to be responding
                    }
                }

                let responding_count = nodes_guard.values().filter(|n| n.is_responding).count();
                let total_count = nodes_guard.len();
                println!("Failure simulation: {responding_count}/{total_count} nodes responding");
            }
        }
    }))
}

/// Start a single HTTP server on the given port
async fn start_server(
    port: u16,
    nodes: Arc<Mutex<HashMap<u16, MockNode>>>,
) -> Result<tokio::task::JoinHandle<()>> {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = TcpListener::bind(addr).await?;
    println!("Listening on http://{addr}");

    Ok(tokio::spawn(async move {
        // Create builder once per server, not per connection
        let builder = Arc::new(Builder::new(hyper_util::rt::TokioExecutor::new()));
        // Limit concurrent connections per server
        let semaphore = Arc::new(Semaphore::new(MAX_CONNECTIONS_PER_SERVER));

        loop {
            match listener.accept().await {
                Ok((tcp, _)) => {
                    let io = TokioIo::new(tcp);
                    let nodes_clone = Arc::clone(&nodes);
                    let builder_clone = Arc::clone(&builder);
                    let permit = semaphore.clone().acquire_owned().await.unwrap();

                    let service =
                        service_fn(move |req| handle_request(req, Arc::clone(&nodes_clone), port));

                    tokio::spawn(async move {
                        let conn = builder_clone.serve_connection(io, service);

                        if let Err(err) = conn.await {
                            eprintln!("Connection failed: {err:?}");
                        }
                        drop(permit); // Release semaphore permit
                    });
                }
                Err(e) => {
                    eprintln!("Failed to accept connection: {e}");
                    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
                }
            }
        }
    }))
}

/// Start all servers and background tasks
pub async fn start_servers(args: Args) -> Result<()> {
    let port_range = match args.port_range {
        Some(range) => parse_port_range(&range)?,
        None => 10001..=10010,
    };

    let platform_type = PlatformType::from_str(&args.platform);
    let nodes = Arc::new(Mutex::new(HashMap::new()));
    let mut file = File::create(&args.o)?;
    let mut instance_counter = args.start_index;

    // Use appropriate GPU/NPU name based on platform
    let device_name = if args.gpu_name == crate::mock::constants::DEFAULT_NVIDIA_GPU_NAME {
        // If using default GPU name, switch to platform-specific default
        match platform_type {
            PlatformType::Tenstorrent => {
                crate::mock::constants::DEFAULT_TENSTORRENT_NAME.to_string()
            }
            PlatformType::Furiosa => crate::mock::constants::DEFAULT_FURIOSA_NAME.to_string(),
            PlatformType::AmdGpu => crate::mock::constants::DEFAULT_AMD_GPU_NAME.to_string(),
            _ => args.gpu_name.clone(),
        }
    } else {
        args.gpu_name.clone()
    };

    // Initialize nodes
    for port in port_range.clone() {
        let instance_name = format!("node-{instance_counter:04}");
        let node = MockNode::new(instance_name, device_name.clone(), platform_type.clone());
        nodes.lock().unwrap().insert(port, node);
        writeln!(file, "localhost:{port}").unwrap();
        instance_counter += 1;
    }

    println!("Outputting server list to {}", args.o);

    // Start background updater task
    let updater_task = start_updater_task(Arc::clone(&nodes));

    // Start failure simulation task if needed
    let failure_task = start_failure_task(Arc::clone(&nodes), args.failure_nodes);

    // Start all servers
    let mut servers = vec![];
    for port in port_range {
        let server = start_server(port, Arc::clone(&nodes)).await?;
        servers.push(server);
    }

    if args.failure_nodes > 0 {
        println!(
            "Started {} servers with background updater (updates every {UPDATE_INTERVAL_SECS}s) and failure simulation ({} nodes)",
            servers.len(),
            args.failure_nodes
        );
    } else {
        println!(
            "Started {} servers with background updater (updates every {UPDATE_INTERVAL_SECS}s)",
            servers.len()
        );
    }

    // Run servers, updater, and failure simulation concurrently
    servers.push(updater_task);
    if let Some(failure_task) = failure_task {
        servers.push(failure_task);
    }
    join_all(servers).await;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_port_range_single() {
        let result = parse_port_range("8080").unwrap();
        assert_eq!(*result.start(), 8080);
        assert_eq!(*result.end(), 8080);
    }

    #[test]
    fn test_parse_port_range_range() {
        let result = parse_port_range("10001-10010").unwrap();
        assert_eq!(*result.start(), 10001);
        assert_eq!(*result.end(), 10010);
    }

    #[test]
    fn test_parse_port_range_invalid() {
        assert!(parse_port_range("invalid").is_err());
        assert!(parse_port_range("80-70").is_ok()); // Range validation happens elsewhere
        assert!(parse_port_range("").is_err());
    }
}
