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

use std::collections::HashMap;
use std::net::IpAddr;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::Once;
use std::time::{Duration, Instant};

use futures_util::stream::{FuturesUnordered, StreamExt};
use regex::Regex;
use tokio::sync::RwLock;
use url::Url;

use crate::app_state::ConnectionStatus;
use crate::common::config::{AppConfig, EnvConfig};
use crate::device::{CpuInfo, GpuInfo, MemoryInfo};
use crate::storage::info::StorageInfo;

pub struct NetworkClient {
    client: reqwest::Client,
    auth_token: Option<String>,
    rate_limiter: Arc<RwLock<RateLimiter>>,
}

/// Simple rate limiter to prevent DoS attacks
struct RateLimiter {
    /// Map of host to (last_request_time, request_count)
    host_requests: HashMap<String, (Instant, u32)>,
    /// Maximum requests per host per second
    max_requests_per_second: u32,
    /// Time window in seconds
    window_seconds: u64,
}

impl RateLimiter {
    fn new() -> Self {
        Self {
            host_requests: HashMap::new(),
            max_requests_per_second: 10, // 10 requests per second per host
            window_seconds: 1,
        }
    }

    /// Check if a request to a host is allowed
    async fn check_rate_limit(&mut self, host: &str) -> bool {
        let now = Instant::now();
        let window = Duration::from_secs(self.window_seconds);

        match self.host_requests.get_mut(host) {
            Some((last_time, count)) => {
                if now.duration_since(*last_time) > window {
                    // Reset the window
                    *last_time = now;
                    *count = 1;
                    true
                } else if *count < self.max_requests_per_second {
                    // Within window and under limit
                    *count += 1;
                    true
                } else {
                    // Rate limit exceeded
                    false
                }
            }
            None => {
                // First request from this host
                self.host_requests.insert(host.to_string(), (now, 1));
                true
            }
        }
    }
}

impl NetworkClient {
    pub fn new() -> Self {
        // Validate connection pool limits against system resources
        let max_idle_per_host = Self::validate_pool_limits(AppConfig::POOL_MAX_IDLE_PER_HOST);

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(AppConfig::CONNECTION_TIMEOUT_SECS))
            .pool_idle_timeout(Duration::from_secs(AppConfig::POOL_IDLE_TIMEOUT_SECS))
            .pool_max_idle_per_host(max_idle_per_host)
            .tcp_keepalive(Duration::from_secs(AppConfig::TCP_KEEPALIVE_SECS))
            .http2_keep_alive_interval(Duration::from_secs(AppConfig::HTTP2_KEEPALIVE_SECS))
            .build()
            .unwrap();

        // Check for authentication token in environment variable
        let auth_token = std::env::var("ALL_SMI_AUTH_TOKEN").ok();
        if auth_token.is_some() {
            eprintln!("Using authentication token from ALL_SMI_AUTH_TOKEN environment variable");
        }

        Self {
            client,
            auth_token,
            rate_limiter: Arc::new(RwLock::new(RateLimiter::new())),
        }
    }

    #[allow(dead_code)]
    pub fn with_auth_token(auth_token: Option<String>) -> Self {
        let max_idle_per_host = Self::validate_pool_limits(AppConfig::POOL_MAX_IDLE_PER_HOST);

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(AppConfig::CONNECTION_TIMEOUT_SECS))
            .pool_idle_timeout(Duration::from_secs(AppConfig::POOL_IDLE_TIMEOUT_SECS))
            .pool_max_idle_per_host(max_idle_per_host)
            .tcp_keepalive(Duration::from_secs(AppConfig::TCP_KEEPALIVE_SECS))
            .http2_keep_alive_interval(Duration::from_secs(AppConfig::HTTP2_KEEPALIVE_SECS))
            .build()
            .unwrap();

        Self {
            client,
            auth_token,
            rate_limiter: Arc::new(RwLock::new(RateLimiter::new())),
        }
    }

    /// Validate and build a secure URL from the host string
    fn validate_and_build_url(host: &str) -> Result<String, String> {
        // Prevent SSRF attacks by validating the host
        let base_url = if host.starts_with("http://") || host.starts_with("https://") {
            host.to_string()
        } else {
            format!("http://{host}")
        };

        // Parse and validate URL
        let mut url = Url::parse(&base_url).map_err(|e| format!("Invalid URL format: {e}"))?;

        // Check for suspicious schemes
        match url.scheme() {
            "http" | "https" => {}
            scheme => return Err(format!("Invalid scheme: {scheme}. Only http/https allowed")),
        }

        // Validate host is not localhost or private IP (unless explicitly allowed)
        if let Some(host_str) = url.host_str() {
            // Check for localhost
            if host_str == "localhost" || host_str == "127.0.0.1" || host_str == "::1" {
                // Allow localhost for local testing, but log it once unless suppressed
                static LOCALHOST_WARNING: Once = Once::new();
                if std::env::var("SUPPRESS_LOCALHOST_WARNING").is_err() {
                    LOCALHOST_WARNING.call_once(|| {
                        eprintln!("Warning: Connecting to localhost address (subsequent warnings suppressed)");
                    });
                }
            }

            // Check for private IP ranges
            if let Ok(addr) = IpAddr::from_str(host_str) {
                match addr {
                    IpAddr::V4(ipv4) => {
                        if ipv4.is_private() || ipv4.is_loopback() || ipv4.is_link_local() {
                            static PRIVATE_IP_WARNING: Once = Once::new();
                            if std::env::var("SUPPRESS_LOCALHOST_WARNING").is_err() {
                                PRIVATE_IP_WARNING.call_once(|| {
                                    eprintln!("Warning: Connecting to private/local IP addresses (subsequent warnings suppressed)");
                                });
                            }
                        }
                    }
                    IpAddr::V6(ipv6) => {
                        if ipv6.is_loopback() || ipv6.is_unspecified() {
                            static IPV6_WARNING: Once = Once::new();
                            if std::env::var("SUPPRESS_LOCALHOST_WARNING").is_err() {
                                IPV6_WARNING.call_once(|| {
                                    eprintln!("Warning: Connecting to loopback/unspecified IPv6 addresses (subsequent warnings suppressed)");
                                });
                            }
                        }
                    }
                }
            }

            // Check port is in reasonable range (port 0 is invalid)
            if let Some(port) = url.port()
                && port == 0
            {
                return Err(format!("Invalid port number: {port}"));
            }
        } else {
            return Err("Missing host in URL".to_string());
        }

        // Set the path to /metrics
        url.set_path("/metrics");

        // Clear any query parameters and fragments to prevent injection
        url.set_query(None);
        url.set_fragment(None);

        Ok(url.to_string())
    }

    /// Validate pool limits against system resources
    fn validate_pool_limits(requested: usize) -> usize {
        // Get system limits using sysctl or /proc
        #[cfg(unix)]
        {
            use std::process::Command;

            // Try to get system file descriptor limit
            let limit = if cfg!(target_os = "macos") {
                Command::new("sysctl")
                    .args(["kern.maxfiles"])
                    .output()
                    .ok()
                    .and_then(|output| {
                        String::from_utf8_lossy(&output.stdout)
                            .split(':')
                            .nth(1)
                            .and_then(|s| s.trim().parse::<usize>().ok())
                    })
            } else {
                // Linux: read from /proc
                std::fs::read_to_string("/proc/sys/fs/file-max")
                    .ok()
                    .and_then(|s| s.trim().parse::<usize>().ok())
            };

            // Use conservative fraction of system limit
            if let Some(sys_limit) = limit {
                let safe_limit = sys_limit / 10; // Use max 10% of system limit
                if requested > safe_limit {
                    eprintln!(
                        "Warning: Requested pool size {requested} exceeds safe limit {safe_limit}, using {safe_limit}"
                    );
                    return safe_limit;
                }
            }
        }

        // Validate against reasonable bounds
        const MIN_POOL_SIZE: usize = 10;
        const MAX_POOL_SIZE: usize = 500;

        if requested < MIN_POOL_SIZE {
            MIN_POOL_SIZE
        } else if requested > MAX_POOL_SIZE {
            eprintln!(
                "Warning: Pool size {requested} exceeds maximum {MAX_POOL_SIZE}, using maximum"
            );
            MAX_POOL_SIZE
        } else {
            requested
        }
    }

    #[allow(clippy::type_complexity)]
    pub async fn fetch_remote_data(
        &self,
        hosts: &[String],
        semaphore: &Arc<tokio::sync::Semaphore>,
        re: &Regex,
    ) -> (
        Vec<GpuInfo>,
        Vec<CpuInfo>,
        Vec<MemoryInfo>,
        Vec<StorageInfo>,
        Vec<crate::device::VgpuHostInfo>,
        Vec<crate::device::MigGpuInfo>,
        Vec<ConnectionStatus>,
    ) {
        let mut all_gpu_info = Vec::new();
        let mut all_cpu_info = Vec::new();
        let mut all_memory_info = Vec::new();
        let mut all_storage_info = Vec::new();
        let mut all_vgpu_info: Vec<crate::device::VgpuHostInfo> = Vec::new();
        let mut all_mig_info: Vec<crate::device::MigGpuInfo> = Vec::new();
        let mut connection_statuses = Vec::new();

        // Parallel data collection with concurrency limiting and retries
        let total_hosts = hosts.len();
        let mut fetch_futures = FuturesUnordered::new();

        for (i, host) in hosts.iter().enumerate() {
            let client = self.client.clone();
            let host = host.clone();
            let semaphore = semaphore.clone();
            let auth_token = self.auth_token.clone();
            let rate_limiter = self.rate_limiter.clone();

            let future = tokio::spawn(async move {
                // Stagger connection attempts to avoid overwhelming the listen queue
                let stagger_delay = EnvConfig::connection_stagger_delay(i, total_hosts);
                tokio::time::sleep(Duration::from_millis(stagger_delay)).await;

                // Acquire semaphore permit to limit concurrency
                let _permit = semaphore.acquire().await.unwrap();

                // Check rate limit before making request
                {
                    let mut limiter = rate_limiter.write().await;
                    if !limiter.check_rate_limit(&host).await {
                        return Some((
                            host,
                            String::new(),
                            Some("Rate limit exceeded".to_string()),
                        ));
                    }
                }

                // Validate and sanitize the URL
                let url = match Self::validate_and_build_url(&host) {
                    Ok(u) => u,
                    Err(e) => {
                        return Some((host, String::new(), Some(format!("Invalid URL: {e}"))));
                    }
                };

                // Retry logic with exponential backoff
                for attempt in 1..=AppConfig::RETRY_ATTEMPTS {
                    // Build request with optional authentication
                    let mut request = client.get(&url);
                    if let Some(ref token) = auth_token {
                        request = request.header("Authorization", format!("Bearer {token}"));
                    }

                    // Bind the send result to a local variable to ensure
                    // temporaries are dropped before surrounding locals (Rust 2024 drop order).
                    let send_result = request.send().await;
                    match send_result {
                        Ok(response) => {
                            if response.status().is_success() {
                                let text_result = response.text().await;
                                match text_result {
                                    Ok(text) => return Some((host, text, None)),
                                    Err(e) => {
                                        if attempt == 3 {
                                            return Some((
                                                host,
                                                String::new(),
                                                Some(format!("Text parse error: {e}")),
                                            ));
                                        }
                                    }
                                }
                            } else if attempt == 3 {
                                return Some((
                                    host,
                                    String::new(),
                                    Some(format!("HTTP {}", response.status())),
                                ));
                            }
                        }
                        Err(e) => {
                            if attempt == 3 {
                                return Some((
                                    host,
                                    String::new(),
                                    Some(format!("Connection error after {attempt} attempts: {e}")),
                                ));
                            }
                        }
                    }

                    // Exponential backoff
                    tokio::time::sleep(Duration::from_millis(EnvConfig::retry_delay(attempt)))
                        .await;
                }

                Some((
                    host,
                    String::new(),
                    Some("All retry attempts failed".to_string()),
                ))
            });

            fetch_futures.push(future);
        }

        // Process results as they arrive using streaming with overall timeout
        let mut _successful_connections = 0;
        let mut _failed_connections = 0;
        let mut responses_received = 0;

        // Set overall timeout for collecting results (4 seconds)
        let overall_timeout = Duration::from_secs(4);
        let timeout_future = tokio::time::sleep(overall_timeout);
        tokio::pin!(timeout_future);

        loop {
            tokio::select! {
                // Process next result if available
                Some(task_result) = fetch_futures.next() => {
                    responses_received += 1;

                    match task_result {
                        Ok(Some((host, text, error))) => {
                            let host_identifier = host.clone();
                            let mut connection_status =
                                ConnectionStatus::new(host_identifier.clone(), host.clone());

                            if let Some(error_msg) = error {
                                _failed_connections += 1;
                                connection_status.mark_failure(error_msg);
                                connection_statuses.push(connection_status);
                            } else {
                                _successful_connections += 1;
                                connection_status.mark_success();

                                if text.is_empty() {
                                    connection_statuses.push(connection_status);
                                } else {
                                    let parser = super::metrics_parser::MetricsParser::new();
                                    let parsed = parser.parse_metrics(&text, &host, re);

                                    // Extract the instance name from device info if available
                                    let instance_name = if let Some(first_gpu) = parsed.gpu_info.first() {
                                        Some(first_gpu.instance.clone())
                                    } else if let Some(first_cpu) = parsed.cpu_info.first() {
                                        Some(first_cpu.instance.clone())
                                    } else { parsed.memory_info.first().map(|first_memory| first_memory.instance.clone()) };

                                    // Store the instance name as actual_hostname for display purposes
                                    connection_status.actual_hostname = instance_name;
                                    connection_statuses.push(connection_status);

                                    all_gpu_info.extend(parsed.gpu_info);
                                    all_cpu_info.extend(parsed.cpu_info);
                                    all_memory_info.extend(parsed.memory_info);
                                    all_storage_info.extend(parsed.storage_info);
                                    all_vgpu_info.extend(parsed.vgpu_info);
                                    all_mig_info.extend(parsed.mig_info);
                                }
                            }
                        }
                        Ok(None) => {
                            _failed_connections += 1;
                            // We don't have host information for None results, so we can't create a connection status
                        }
                        Err(_) => {
                            _failed_connections += 1;
                            // We don't have host information for Err results, so we can't create a connection status
                        }
                    }

                    // Check if we've received responses from all hosts
                    if responses_received >= total_hosts {
                        break;
                    }
                }
                // Timeout reached - return partial results
                _ = &mut timeout_future => {
                    // Mark remaining hosts as timed out
                    break;
                }
            }
        }

        // Debug logging for connection success rate - commented out to avoid interfering with TUI
        // if failed_connections > 0 {
        //     eprintln!(
        //         "Connection stats: {successful_connections} successful, {failed_connections} failed out of {total_hosts} total"
        //     );
        // }

        (
            all_gpu_info,
            all_cpu_info,
            all_memory_info,
            all_storage_info,
            all_vgpu_info,
            all_mig_info,
            connection_statuses,
        )
    }
}

impl Default for NetworkClient {
    fn default() -> Self {
        Self::new()
    }
}
