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

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::str::FromStr;
use std::sync::{Arc, Mutex, Once, PoisonError};
use std::time::{Duration, Instant};

use futures_util::stream::{FuturesUnordered, StreamExt};
use regex::Regex;
use tokio::sync::RwLock;

use crate::app_state::ConnectionStatus;
use crate::common::config::{AppConfig, EnvConfig};
use crate::device::{ChassisInfo, CpuInfo, GpuInfo, MemoryInfo};
use crate::network::metrics_parser::{ParsedProcessRow, ParsedRemoteMetrics};

use super::host_backoff::{BackoffTable, ScrapeTimeouts, backing_off_message};
use super::scrape_task::{ScrapeJob, ScrapeOutcome};
use crate::storage::info::StorageInfo;

const INITIAL_METRICS_BODY_CAPACITY: usize = 64 * 1024;

pub(crate) async fn read_response_text_with_limit(
    mut response: reqwest::Response,
    max_bytes: usize,
) -> Result<String, reqwest::Error> {
    // Do not trust Content-Length enough to preallocate the full advertised
    // size. A small hint avoids repeated growth for normal responses without
    // letting a malicious header reserve the entire limit before any bytes
    // arrive.
    let initial_capacity = response
        .content_length()
        .and_then(|length| usize::try_from(length).ok())
        .unwrap_or_default()
        .min(max_bytes)
        .min(INITIAL_METRICS_BODY_CAPACITY);
    let mut body = Vec::with_capacity(initial_capacity);

    while body.len() < max_bytes {
        let Some(chunk) = response.chunk().await? else {
            break;
        };
        let bytes_to_copy = chunk.len().min(max_bytes - body.len());
        body.extend_from_slice(&chunk[..bytes_to_copy]);

        if bytes_to_copy < chunk.len() || body.len() == max_bytes {
            break;
        }
    }

    // Dropping a response whose body exceeded the limit cancels the remaining
    // body stream instead of buffering it in the background. Match reqwest's
    // `Response::text` behavior for malformed UTF-8 by replacing invalid byte
    // sequences rather than rejecting the entire scrape.
    drop(response);
    Ok(String::from_utf8_lossy(&body).into_owned())
}

/// Collect task results until every handle has been awaited or `budget`
/// elapses. When the budget elapses once, every remaining handle is aborted
/// and collection continues: a task that finished before the abort still
/// delivers its result, and a cancelled one is skipped. Every handle has been
/// awaited before this returns, so no aborted task can overlap with a later
/// collector tick. Returns whether the deadline fired.
async fn collect_until_deadline<T>(
    tasks: &mut FuturesUnordered<tokio::task::JoinHandle<T>>,
    budget: Duration,
    mut on_result: impl FnMut(T),
) -> bool {
    let mut deadline_hit = false;
    let timeout_future = tokio::time::sleep(budget);
    tokio::pin!(timeout_future);

    while !tasks.is_empty() {
        tokio::select! {
            biased;
            _ = &mut timeout_future, if !deadline_hit => {
                deadline_hit = true;
                for task in tasks.iter() {
                    task.abort();
                }
            }
            Some(task_result) = tasks.next() => {
                // An Err is a task cancelled by the abort above (or a
                // panic): no result to merge.
                if let Ok(value) = task_result {
                    on_result(value);
                }
            }
        }
    }
    deadline_hit
}

pub struct NetworkClient {
    client: reqwest::Client,
    auth_token: Option<String>,
    rate_limiter: Arc<RwLock<RateLimiter>>,
    timeouts: ScrapeTimeouts,
    /// Per-host failure history. Kept here rather than in `AppState` so
    /// `record --source=remote`, which has no `AppState`, backs off too.
    backoff: Arc<Mutex<BackoffTable>>,
}

/// Internal structured result used by the remote view collector. The public
/// [`NetworkClient::fetch_remote_data`] tuple remains unchanged for embedding
/// callers while the in-crate pipeline can carry chassis records as well.
#[derive(Default)]
pub(crate) struct RemoteScrapeData {
    pub(crate) gpu_info: Vec<GpuInfo>,
    pub(crate) cpu_info: Vec<CpuInfo>,
    pub(crate) memory_info: Vec<MemoryInfo>,
    pub(crate) storage_info: Vec<StorageInfo>,
    // The library target does not compile the binary-only `view` module, but
    // the main target consumes this field in RemoteCollector.
    #[allow(dead_code)]
    pub(crate) chassis_info: Vec<ChassisInfo>,
    pub(crate) vgpu_info: Vec<crate::device::VgpuHostInfo>,
    pub(crate) mig_info: Vec<crate::device::MigGpuInfo>,
    pub(crate) process_info: Vec<ParsedProcessRow>,
    pub(crate) connection_statuses: Vec<ConnectionStatus>,
}

/// Simple rate limiter to prevent DoS attacks
pub(crate) struct RateLimiter {
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
    pub(crate) async fn check_rate_limit(&mut self, host: &str) -> bool {
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
        // Check for authentication token in environment variable
        let auth_token = std::env::var("ALL_SMI_AUTH_TOKEN").ok();
        if auth_token.is_some() {
            eprintln!("Using authentication token from ALL_SMI_AUTH_TOKEN environment variable");
        }
        Self::build(auth_token, ScrapeTimeouts::default())
    }

    #[allow(dead_code)]
    pub fn with_auth_token(auth_token: Option<String>) -> Self {
        Self::build(auth_token, ScrapeTimeouts::default())
    }

    /// Client with explicit scrape time bounds, for tests that cannot wait
    /// out the production budget.
    #[allow(dead_code)] // Used by the binary's remote-view tests.
    pub(crate) fn with_timeouts(timeouts: ScrapeTimeouts) -> Self {
        Self::build(None, timeouts)
    }

    fn build(auth_token: Option<String>, timeouts: ScrapeTimeouts) -> Self {
        // Validate connection pool limits against system resources
        let max_idle_per_host = Self::validate_pool_limits(AppConfig::POOL_MAX_IDLE_PER_HOST);

        let client = reqwest::Client::builder()
            .connect_timeout(timeouts.connect)
            .timeout(timeouts.request)
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
            timeouts,
            backoff: Arc::new(Mutex::new(BackoffTable::default())),
        }
    }

    /// Validate and build a secure URL from the host string
    pub(crate) fn validate_and_build_url(host: &str) -> Result<String, String> {
        // Use the same syntax contract as startup validation so collector
        // retries cannot reinterpret an endpoint that was accepted earlier.
        let mut url = crate::common::http_hosts::parse_http_host_url(host)
            .map_err(|error| error.to_string())?;

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
            use crate::utils::command::new_command;

            // Try to get system file descriptor limit
            let limit = if cfg!(target_os = "macos") {
                new_command("sysctl")
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

    /// Scrape every host once.
    ///
    /// `re` is unused since issue #500 (the parser no longer runs a regex)
    /// and is kept only for source compatibility.
    #[allow(clippy::type_complexity, dead_code)]
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
        Vec<ParsedProcessRow>,
        Vec<ConnectionStatus>,
    ) {
        let _ = re;
        let data = self.fetch_remote_metrics(hosts, semaphore).await;
        (
            data.gpu_info,
            data.cpu_info,
            data.memory_info,
            data.storage_info,
            data.vgpu_info,
            data.mig_info,
            data.process_info,
            data.connection_statuses,
        )
    }

    fn lock_backoff(&self) -> std::sync::MutexGuard<'_, BackoffTable> {
        self.backoff.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Scrape every host once within `ScrapeTimeouts::budget`.
    ///
    /// Hosts in backoff are not contacted and report a `backing-off`
    /// status; hosts still pending at the deadline report `timeout`. Every
    /// configured host therefore gets exactly one status per call, in the
    /// order of `hosts`.
    pub(crate) async fn fetch_remote_metrics(
        &self,
        hosts: &[String],
        semaphore: &Arc<tokio::sync::Semaphore>,
    ) -> RemoteScrapeData {
        let mut data = RemoteScrapeData::default();
        let host_ids: Vec<String> = hosts
            .iter()
            .map(|host| crate::common::http_hosts::http_host_identifier(host))
            .collect();
        let mut statuses: Vec<Option<ConnectionStatus>> = vec![None; hosts.len()];

        // Pick the hosts to contact this tick and drop backoff entries for
        // hosts that left the configuration.
        let mut to_scrape: Vec<usize> = Vec::with_capacity(hosts.len());
        {
            let now = Instant::now();
            let mut table = self.lock_backoff();
            let configured: HashSet<&str> = host_ids.iter().map(String::as_str).collect();
            table.retain_configured(&configured);
            for (index, host_id) in host_ids.iter().enumerate() {
                match table.pending_backoff(host_id, now) {
                    Some((remaining, failures)) => {
                        let mut status =
                            ConnectionStatus::new(host_id.clone(), hosts[index].clone());
                        status.mark_failure(backing_off_message(remaining, failures));
                        status.consecutive_failures = failures;
                        statuses[index] = Some(status);
                    }
                    None => to_scrape.push(index),
                }
            }
        }

        // Parallel data collection with concurrency limiting and retries
        let mut fetch_futures = FuturesUnordered::new();
        let spawn_count = to_scrape.len();
        for (spawn_index, &index) in to_scrape.iter().enumerate() {
            let job = ScrapeJob {
                client: self.client.clone(),
                auth_token: self.auth_token.clone(),
                rate_limiter: self.rate_limiter.clone(),
                semaphore: semaphore.clone(),
                host: hosts[index].clone(),
                host_id: host_ids[index].clone(),
                stagger: Duration::from_millis(EnvConfig::connection_stagger_delay(
                    spawn_index,
                    spawn_count,
                )),
                request_timeout: self.timeouts.request,
            };
            fetch_futures.push(tokio::spawn(async move { (index, job.run().await) }));
        }

        let mut pending: HashSet<usize> = to_scrape.iter().copied().collect();
        let deadline_hit = collect_until_deadline(
            &mut fetch_futures,
            self.timeouts.budget,
            |(index, outcome)| {
                pending.remove(&index);
                let mut status =
                    ConnectionStatus::new(host_ids[index].clone(), hosts[index].clone());
                match outcome {
                    ScrapeOutcome::Failed(message) => status.mark_failure(message),
                    ScrapeOutcome::Empty => status.mark_success(),
                    ScrapeOutcome::Parsed(parsed) => {
                        status.mark_success();
                        status.actual_hostname = instance_name(&parsed);
                        data.merge(*parsed);
                    }
                }
                statuses[index] = Some(status);
            },
        )
        .await;

        for &index in &pending {
            let mut status = ConnectionStatus::new(host_ids[index].clone(), hosts[index].clone());
            status.mark_failure(if deadline_hit {
                format!(
                    "timeout: no response within {} ms",
                    self.timeouts.budget.as_millis()
                )
            } else {
                "Scrape task failed".to_string()
            });
            statuses[index] = Some(status);
        }

        // Feed this tick's results into the backoff table. Hosts skipped
        // while backing off keep their schedule untouched.
        {
            let now = Instant::now();
            let mut table = self.lock_backoff();
            for &index in &to_scrape {
                let Some(status) = statuses[index].as_mut() else {
                    continue;
                };
                if status.is_connected {
                    table.record_success(&host_ids[index]);
                } else {
                    status.consecutive_failures = table.record_failure(&host_ids[index], now);
                }
            }
        }

        data.connection_statuses = statuses.into_iter().flatten().collect();
        data
    }

    #[cfg(test)]
    pub(crate) fn backoff_entry(&self, host_id: &str) -> Option<super::host_backoff::HostBackoff> {
        self.lock_backoff().get(host_id).copied()
    }

    /// Make every backed-off host eligible again, as if its window passed.
    #[cfg(test)]
    pub(crate) fn expire_backoff_for_tests(&self) {
        self.lock_backoff().expire_all();
    }

    #[cfg(test)]
    pub(crate) fn record_failure_for_tests(&self, host_id: &str) {
        self.lock_backoff().record_failure(host_id, Instant::now());
    }
}

/// Display name for a host: the `instance` label of its first device row.
fn instance_name(parsed: &ParsedRemoteMetrics) -> Option<String> {
    let metrics = &parsed.metrics;
    metrics
        .gpu_info
        .first()
        .map(|gpu| gpu.instance.clone())
        .or_else(|| metrics.cpu_info.first().map(|cpu| cpu.instance.clone()))
        .or_else(|| metrics.memory_info.first().map(|mem| mem.instance.clone()))
        .or_else(|| {
            parsed
                .chassis_info
                .first()
                .map(|chassis| chassis.instance.clone())
        })
}

impl RemoteScrapeData {
    fn merge(&mut self, parsed: ParsedRemoteMetrics) {
        self.gpu_info.extend(parsed.metrics.gpu_info);
        self.cpu_info.extend(parsed.metrics.cpu_info);
        self.memory_info.extend(parsed.metrics.memory_info);
        self.storage_info.extend(parsed.metrics.storage_info);
        self.chassis_info.extend(parsed.chassis_info);
        self.vgpu_info.extend(parsed.metrics.vgpu_info);
        self.mig_info.extend(parsed.metrics.mig_info);
        self.process_info.extend(parsed.metrics.process_info);
    }
}

impl Default for NetworkClient {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[path = "client_scrape_tests.rs"]
mod scrape_tests;

#[cfg(test)]
mod tests {
    use std::future::pending;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    #[tokio::test]
    async fn response_body_reading_stops_at_byte_limit() {
        const BODY_SIZE: usize = 128 * 1024;
        const READ_LIMIT: usize = 1024;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 1024];
            let _ = socket.read(&mut request).await;
            let headers = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {BODY_SIZE}\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\n"
            );
            socket.write_all(headers.as_bytes()).await.unwrap();
            let _ = socket.write_all(&vec![b'x'; BODY_SIZE]).await;
        });

        let response = reqwest::Client::new()
            .get(format!("http://{address}/metrics"))
            .send()
            .await
            .unwrap();
        let text = read_response_text_with_limit(response, READ_LIMIT)
            .await
            .unwrap();

        assert_eq!(text.len(), READ_LIMIT);
        assert!(text.bytes().all(|byte| byte == b'x'));
        let _ = server.await;
    }

    #[tokio::test]
    async fn collect_until_deadline_waits_for_cancellation() {
        struct DropCounter(Arc<AtomicUsize>);

        impl Drop for DropCounter {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }

        let dropped = Arc::new(AtomicUsize::new(0));
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let task_dropped = dropped.clone();
        let task = tokio::spawn(async move {
            let _counter = DropCounter(task_dropped);
            let _ = started_tx.send(());
            pending::<()>().await;
        });
        started_rx.await.unwrap();

        let mut tasks = FuturesUnordered::new();
        tasks.push(task);
        let mut results = 0;
        let hit =
            collect_until_deadline(&mut tasks, Duration::from_millis(10), |_| results += 1).await;

        assert!(hit);
        assert_eq!(results, 0);
        assert!(tasks.is_empty());
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn collect_until_deadline_keeps_results_finished_before_abort() {
        let mut tasks = FuturesUnordered::new();
        tasks.push(tokio::spawn(async { 1_u32 }));
        tasks.push(tokio::spawn(async {
            pending::<()>().await;
            2_u32
        }));
        // Let the first task finish; its handle is complete but not yet
        // polled by the collector when the zero budget fires.
        tokio::task::yield_now().await;

        let mut results = Vec::new();
        let hit = collect_until_deadline(&mut tasks, Duration::ZERO, |v| results.push(v)).await;

        assert!(hit);
        assert_eq!(results, vec![1]);
        assert!(tasks.is_empty());
    }

    #[tokio::test]
    async fn collect_until_deadline_without_timeout_returns_all_results() {
        let mut tasks = FuturesUnordered::new();
        for value in 0..3_u32 {
            tasks.push(tokio::spawn(async move { value }));
        }
        let mut results = Vec::new();
        let hit =
            collect_until_deadline(&mut tasks, Duration::from_secs(30), |v| results.push(v)).await;
        results.sort_unstable();
        assert!(!hit);
        assert_eq!(results, vec![0, 1, 2]);
    }
}
