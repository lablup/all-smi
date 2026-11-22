// Copyright 2025 Lablup Inc.
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

//! gRPC client for TPU runtime metrics.
//!
//! This module provides a native Rust gRPC client to collect TPU metrics
//! directly from the libtpu runtime metrics server at localhost:8431.
//!
//! The gRPC server is only available when a TPU workload (JAX/TensorFlow)
//! is actively running.

#![allow(unused)]

use once_cell::sync::OnceCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tonic::transport::{Channel, Endpoint};
use tracing::{debug, warn};

use super::tpu_info_runner;

// Include the generated protobuf code
pub mod tpu_proto {
    tonic::include_proto!("tpu.monitoring.runtime");
}

use tpu_proto::MetricRequest;
use tpu_proto::runtime_metric_service_client::RuntimeMetricServiceClient;

/// Default gRPC server address for libtpu metrics
const DEFAULT_GRPC_ADDR: &str = "http://localhost:8431";

/// Connection timeout for gRPC
const CONNECT_TIMEOUT: Duration = Duration::from_millis(500);

/// Request timeout for gRPC calls
const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);

/// Minimum time between connection attempts after one failed.
///
/// libtpu's metrics server only exists while a workload runs, so on an idle
/// host every connect fails. Without this gate each collection tick would
/// attempt (and, behind a filtering firewall, wait out) a connect.
const GRPC_RETRY_INTERVAL: Duration = Duration::from_secs(10);

/// Track whether gRPC was available on last check
static GRPC_WAS_AVAILABLE: OnceCell<AtomicBool> = OnceCell::new();

/// Metric names defined in libtpu
pub mod metric_names {
    pub const TOTAL_MEMORY: &str = "tpu.runtime.hbm.memory.total.bytes";
    pub const MEMORY_USAGE: &str = "tpu.runtime.hbm.memory.usage.bytes";
    pub const DUTY_CYCLE_PCT: &str = "tpu.runtime.tensorcore.dutycycle.percent";
    pub const HLO_QUEUE_SIZE: &str = "hlo.queue.size.gauge";
    pub const HLO_EXEC_TIMING: &str = "hlo.execution.timing.distribution.microseconds";
}

/// TPU usage metrics from gRPC
#[derive(Debug, Clone, Default)]
pub struct TpuUsageMetrics {
    /// Device ID (chip index)
    pub device_id: i64,
    /// Memory usage in bytes
    pub memory_usage: u64,
    /// Total memory in bytes
    pub total_memory: u64,
    /// Duty cycle percentage (0-100)
    pub duty_cycle_pct: f64,
}

/// HLO Queue Size metric
#[derive(Debug, Clone, Default)]
pub struct HloQueueSize {
    /// Device ID (chip index)
    pub device_id: i64,
    /// Queue size (number of pending HLO programs)
    pub queue_size: i64,
}

/// HLO Execution Timing metric (microseconds)
#[derive(Debug, Clone, Default)]
pub struct HloExecutionTiming {
    /// Device ID (chip index)
    pub device_id: i64,
    /// Mean execution time in microseconds
    pub mean_us: f64,
    /// 50th percentile (median) in microseconds
    pub p50_us: f64,
    /// 90th percentile in microseconds
    pub p90_us: f64,
    /// 95th percentile in microseconds
    pub p95_us: f64,
    /// 99.9th percentile in microseconds
    pub p999_us: f64,
}

/// Whether a new connection attempt must wait: the last one failed less
/// than [`GRPC_RETRY_INTERVAL`] before `now`.
fn connect_backoff_active(last_failure: Option<Instant>, now: Instant) -> bool {
    last_failure
        .is_some_and(|failed_at| now.saturating_duration_since(failed_at) < GRPC_RETRY_INTERVAL)
}

/// The connection to one metrics server: the cached channel, the time of the
/// last failed connect, and a count of connect attempts.
struct GrpcLink {
    addr: String,
    channel: Mutex<Option<Channel>>,
    last_connect_failure: std::sync::Mutex<Option<Instant>>,
    /// Connection attempts made through this link. Read by tests.
    connect_attempts: AtomicUsize,
}

impl GrpcLink {
    fn new(addr: impl Into<String>) -> Self {
        Self {
            addr: addr.into(),
            channel: Mutex::new(None),
            last_connect_failure: std::sync::Mutex::new(None),
            connect_attempts: AtomicUsize::new(0),
        }
    }

    fn last_connect_failure(&self) -> Option<Instant> {
        self.last_connect_failure
            .lock()
            .map(|guard| *guard)
            .unwrap_or(None)
    }

    fn set_last_connect_failure(&self, value: Option<Instant>) {
        if let Ok(mut guard) = self.last_connect_failure.lock() {
            *guard = value;
        }
    }

    /// Get the cached channel or connect.
    ///
    /// Returns `None` without connecting while the last failed attempt is
    /// younger than [`GRPC_RETRY_INTERVAL`].
    ///
    /// `cache` must be `false` when the caller runs on a temporary runtime:
    /// a channel's background task lives on the runtime that created it, so
    /// a channel cached from a runtime that is about to be dropped would be
    /// dead on the next call.
    async fn channel(&self, cache: bool, now: Instant) -> Option<Channel> {
        let mut guard = self.channel.lock().await;

        // Return cached channel if available
        if let Some(ref channel) = *guard {
            return Some(channel.clone());
        }
        if connect_backoff_active(self.last_connect_failure(), now) {
            return None;
        }

        // Bind the result to a local variable to ensure temporaries from
        // connect().await are dropped before the MutexGuard (Rust 2024 drop order).
        let channel_result = self.connect().await;
        match channel_result {
            Ok(channel) => {
                self.set_last_connect_failure(None);
                if cache {
                    *guard = Some(channel.clone());
                }
                Some(channel)
            }
            Err(e) => {
                debug!("Failed to create gRPC channel: {}", e);
                self.set_last_connect_failure(Some(now));
                None
            }
        }
    }

    /// Create a new gRPC channel to the TPU metrics server
    async fn connect(&self) -> Result<Channel, tonic::transport::Error> {
        self.connect_attempts.fetch_add(1, Ordering::Relaxed);
        Endpoint::from_shared(self.addr.clone())?
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .connect()
            .await
    }

    /// Clear cached channel (call when connection fails)
    async fn clear_channel(&self) {
        let mut guard = self.channel.lock().await;
        *guard = None;
    }
}

/// The process-wide link to libtpu's metrics server.
static GRPC_LINK: OnceCell<GrpcLink> = OnceCell::new();

fn default_link() -> &'static GrpcLink {
    GRPC_LINK.get_or_init(|| GrpcLink::new(DEFAULT_GRPC_ADDR))
}

/// Fetch a single metric from the gRPC server
async fn fetch_metric(
    client: &mut RuntimeMetricServiceClient<Channel>,
    metric_name: &str,
) -> Option<Vec<(i64, MetricValue)>> {
    let request = tonic::Request::new(MetricRequest {
        metric_name: metric_name.to_string(),
        skip_node_aggregation: false,
    });

    match client.get_runtime_metric(request).await {
        Ok(response) => {
            let metric = response.into_inner().metric?;
            let mut results = Vec::new();

            for m in metric.metrics {
                // Extract device ID from attribute
                let device_id = m
                    .attribute
                    .as_ref()
                    .and_then(|attr| attr.value.as_ref())
                    .and_then(|v| match v.attr.as_ref()? {
                        tpu_proto::attr_value::Attr::IntAttr(i) => Some(*i),
                        _ => None,
                    })
                    .unwrap_or(0);

                // Extract gauge value
                if let Some(tpu_proto::metric::Measure::Gauge(gauge)) = m.measure {
                    let value = match gauge.value {
                        Some(tpu_proto::gauge::Value::AsInt(i)) => MetricValue::Int(i),
                        Some(tpu_proto::gauge::Value::AsDouble(d)) => MetricValue::Double(d),
                        _ => continue,
                    };
                    results.push((device_id, value));
                }
            }

            // Sort by device ID
            results.sort_by_key(|(id, _)| *id);
            Some(results)
        }
        Err(e) => {
            debug!("Failed to fetch metric '{}': {}", metric_name, e);
            None
        }
    }
}

/// Metric value type
#[derive(Debug, Clone)]
enum MetricValue {
    Int(i64),
    Double(f64),
}

impl MetricValue {
    fn as_u64(&self) -> u64 {
        match self {
            MetricValue::Int(i) => (*i).max(0) as u64,
            MetricValue::Double(d) => (*d).max(0.0) as u64,
        }
    }

    fn as_f64(&self) -> f64 {
        match self {
            MetricValue::Int(i) => *i as f64,
            MetricValue::Double(d) => *d,
        }
    }
}

/// Update gRPC availability status and notify tpu_info_runner
fn update_grpc_status(available: bool) {
    let was_available = GRPC_WAS_AVAILABLE.get_or_init(|| AtomicBool::new(false));

    let prev = was_available.swap(available, Ordering::Relaxed);

    // Notify tpu_info_runner of status change
    if prev != available {
        tpu_info_runner::get_runner().set_grpc_available(available);
        if available {
            debug!("gRPC server became available - switching to native metrics");
        } else {
            debug!("gRPC server unavailable - falling back to CLI polling");
        }
    }
}

/// Fetch all TPU usage metrics via gRPC
///
/// Returns None if the gRPC server is not available (no workload running)
pub async fn get_tpu_metrics_grpc() -> Option<Vec<TpuUsageMetrics>> {
    let link = default_link();
    let channel = link.channel(true, Instant::now()).await;
    usage_metrics(link, channel).await
}

/// Usage metrics over an already resolved channel (`None`: no connection).
async fn usage_metrics(link: &GrpcLink, channel: Option<Channel>) -> Option<Vec<TpuUsageMetrics>> {
    let channel = match channel {
        Some(ch) => ch,
        None => {
            update_grpc_status(false);
            return None;
        }
    };

    let mut client = RuntimeMetricServiceClient::new(channel);

    // Fetch all three metrics
    let totals = match fetch_metric(&mut client, metric_names::TOTAL_MEMORY).await {
        Some(t) => t,
        None => {
            update_grpc_status(false);
            link.clear_channel().await;
            return None;
        }
    };

    let usages = match fetch_metric(&mut client, metric_names::MEMORY_USAGE).await {
        Some(u) => u,
        None => {
            update_grpc_status(false);
            link.clear_channel().await;
            return None;
        }
    };

    let duty_cycles = fetch_metric(&mut client, metric_names::DUTY_CYCLE_PCT)
        .await
        .unwrap_or_default();

    // Verify we have matching data
    if totals.len() != usages.len() {
        warn!(
            "Metric count mismatch: totals={}, usages={}",
            totals.len(),
            usages.len()
        );
        update_grpc_status(false);
        link.clear_channel().await;
        return None;
    }

    // Build result vector
    let mut results = Vec::with_capacity(totals.len());

    for ((device_id, total), (_, usage)) in totals.iter().zip(usages.iter()) {
        // Find matching duty cycle (may have different count due to per-chip vs per-core)
        let duty_cycle = duty_cycles
            .iter()
            .find(|(id, _)| *id == *device_id)
            .map(|(_, v)| v.as_f64())
            .unwrap_or(0.0);

        results.push(TpuUsageMetrics {
            device_id: *device_id,
            memory_usage: usage.as_u64(),
            total_memory: total.as_u64(),
            duty_cycle_pct: duty_cycle.clamp(0.0, 100.0),
        });
    }

    if results.is_empty() {
        update_grpc_status(false);
        None
    } else {
        // Success! gRPC is working
        update_grpc_status(true);
        Some(results)
    }
}

/// Check if the gRPC metrics server is available
pub async fn is_grpc_server_available() -> bool {
    default_link().channel(true, Instant::now()).await.is_some()
}

/// Everything one collection tick reads from the metrics server.
#[derive(Debug, Clone, Default)]
pub struct TpuGrpcSnapshot {
    /// Memory and duty cycle per chip; `None` when the server is not
    /// reachable (no workload running).
    pub metrics: Option<Vec<TpuUsageMetrics>>,
    pub hlo_queue_sizes: Option<Vec<HloQueueSize>>,
    pub hlo_exec_timings: Option<Vec<HloExecutionTiming>>,
}

/// Fetch the usage metrics and, when those succeed, both HLO metrics, over
/// one channel.
///
/// The HLO fetches are skipped when the usage metrics failed: the server is
/// down or mid-restart, and two more requests would only fail the same way.
async fn collect_snapshot(link: &GrpcLink, cache_channel: bool) -> TpuGrpcSnapshot {
    let channel = link.channel(cache_channel, Instant::now()).await;
    let metrics = usage_metrics(link, channel.clone()).await;
    let (Some(channel), Some(_)) = (channel, metrics.as_ref()) else {
        return TpuGrpcSnapshot {
            metrics,
            ..TpuGrpcSnapshot::default()
        };
    };
    let hlo_queue_sizes = hlo_queue_size(channel.clone()).await;
    let hlo_exec_timings = hlo_execution_timing(channel).await;
    TpuGrpcSnapshot {
        metrics,
        hlo_queue_sizes,
        hlo_exec_timings,
    }
}

/// Run `task` to completion from synchronous code.
///
/// `task` receives whether a channel it creates may be cached: `true` on the
/// caller's own runtime, `false` on the temporary runtime built when no
/// runtime is current (library use), which is dropped as soon as the task
/// returns.
fn block_on_grpc<T, F>(task: impl FnOnce(bool) -> F) -> Option<T>
where
    F: std::future::Future<Output = T>,
{
    match tokio::runtime::Handle::try_current() {
        // We're in an async context, use block_in_place
        Ok(handle) => Some(tokio::task::block_in_place(|| handle.block_on(task(true)))),
        _ => {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .ok()?;
            Some(rt.block_on(task(false)))
        }
    }
}

/// One tick's gRPC metrics from synchronous code: a single `block_on`, and
/// so at most one temporary runtime and one connection, for all three
/// fetches.
pub fn collect_tpu_grpc_sync() -> TpuGrpcSnapshot {
    block_on_grpc(|cache| collect_snapshot(default_link(), cache)).unwrap_or_default()
}

/// Synchronous wrapper for get_tpu_metrics_grpc
/// Uses the tokio runtime to run the async function
pub fn get_tpu_metrics_grpc_sync() -> Option<Vec<TpuUsageMetrics>> {
    block_on_grpc(|cache| async move {
        let link = default_link();
        let channel = link.channel(cache, Instant::now()).await;
        usage_metrics(link, channel).await
    })
    .flatten()
}

/// Extract device_ordinal from kvlist_attr attributes
fn extract_device_ordinal(attr: &tpu_proto::Attribute) -> Option<i64> {
    let value = attr.value.as_ref()?;
    let kvlist = match value.attr.as_ref()? {
        tpu_proto::attr_value::Attr::KvlistAttr(kv) => kv,
        _ => return None,
    };

    for kv_attr in &kvlist.attributes {
        if kv_attr.key == "device_ordinal"
            && let Some(ref val) = kv_attr.value
        {
            match &val.attr {
                Some(tpu_proto::attr_value::Attr::StringAttr(s)) => {
                    return s.parse().ok();
                }
                Some(tpu_proto::attr_value::Attr::IntAttr(i)) => {
                    return Some(*i);
                }
                _ => {}
            }
        }
    }
    None
}

/// Calculate percentile from exponential bucket distribution
fn calculate_percentile(
    target_count: i64,
    total_count: i64,
    bucket_counts: &[i64],
    scale: f64,
    growth_factor: f64,
) -> f64 {
    if total_count == 0 || bucket_counts.is_empty() {
        return 0.0;
    }

    let mut cumulative = 0i64;
    for (i, &count) in bucket_counts.iter().enumerate() {
        cumulative += count;
        if cumulative >= target_count {
            // Estimate value at this bucket
            // Bucket boundaries: scale * growth_factor^i
            let lower = if i == 0 {
                0.0
            } else {
                scale * growth_factor.powi(i as i32 - 1)
            };
            let upper = scale * growth_factor.powi(i as i32);
            // Linear interpolation within bucket
            return (lower + upper) / 2.0;
        }
    }

    // If we didn't find it, return the last bucket upper bound
    scale * growth_factor.powi(bucket_counts.len() as i32 - 1)
}

/// Fetch HLO Queue Size metrics via gRPC
pub async fn get_hlo_queue_size() -> Option<Vec<HloQueueSize>> {
    let channel = default_link().channel(true, Instant::now()).await?;
    hlo_queue_size(channel).await
}

/// HLO queue sizes over an already connected channel.
async fn hlo_queue_size(channel: Channel) -> Option<Vec<HloQueueSize>> {
    let mut client = RuntimeMetricServiceClient::new(channel);

    let request = tonic::Request::new(MetricRequest {
        metric_name: metric_names::HLO_QUEUE_SIZE.to_string(),
        skip_node_aggregation: false,
    });

    let response = match client.get_runtime_metric(request).await {
        Ok(resp) => resp,
        Err(e) => {
            debug!("Failed to fetch HLO queue size: {}", e);
            return None;
        }
    };

    let metric = response.into_inner().metric?;
    let mut results = Vec::new();

    for m in metric.metrics {
        // Extract device_ordinal from kvlist_attr
        let device_id = m
            .attribute
            .as_ref()
            .and_then(extract_device_ordinal)
            .unwrap_or(-1);

        // If extraction failed, try to get device_id from index 0
        let device_id = if device_id < 0 {
            // Fallback: use metric index as device_id for single-device setups
            results.len() as i64
        } else {
            device_id
        };

        // Extract gauge value
        if let Some(tpu_proto::metric::Measure::Gauge(gauge)) = m.measure {
            let queue_size = match gauge.value {
                Some(tpu_proto::gauge::Value::AsInt(i)) => i,
                Some(tpu_proto::gauge::Value::AsDouble(d)) => d as i64,
                _ => continue,
            };
            results.push(HloQueueSize {
                device_id,
                queue_size,
            });
        }
    }

    results.sort_by_key(|r| r.device_id);
    if results.is_empty() {
        None
    } else {
        Some(results)
    }
}

/// Fetch HLO Execution Timing metrics via gRPC
pub async fn get_hlo_execution_timing() -> Option<Vec<HloExecutionTiming>> {
    let channel = default_link().channel(true, Instant::now()).await?;
    hlo_execution_timing(channel).await
}

/// HLO execution timings over an already connected channel.
async fn hlo_execution_timing(channel: Channel) -> Option<Vec<HloExecutionTiming>> {
    let mut client = RuntimeMetricServiceClient::new(channel);

    let request = tonic::Request::new(MetricRequest {
        metric_name: metric_names::HLO_EXEC_TIMING.to_string(),
        skip_node_aggregation: false,
    });

    let response = match client.get_runtime_metric(request).await {
        Ok(resp) => resp,
        Err(e) => {
            debug!("Failed to fetch HLO execution timing: {}", e);
            return None;
        }
    };

    let metric = response.into_inner().metric?;
    let mut results = Vec::new();

    for m in metric.metrics {
        // Extract device_ordinal from kvlist_attr
        let device_id = m
            .attribute
            .as_ref()
            .and_then(extract_device_ordinal)
            .unwrap_or(-1);

        if device_id < 0 {
            continue;
        }

        // Extract distribution
        if let Some(tpu_proto::metric::Measure::Distribution(dist)) = m.measure {
            let count = dist.count;
            let mean = dist.mean;
            let bucket_counts = &dist.bucket_counts;

            // Get exponential bucket options
            let (scale, growth_factor) = dist
                .bucket_options
                .as_ref()
                .and_then(|opts| opts.options.as_ref())
                .and_then(|o| match o {
                    tpu_proto::distribution::bucket_options::Options::ExponentialBuckets(exp) => {
                        Some((exp.scale, exp.growth_factor))
                    }
                    _ => None,
                })
                .unwrap_or((1.0, 2.0));

            // Calculate percentiles
            let p50 = calculate_percentile(
                (count as f64 * 0.5) as i64,
                count,
                bucket_counts,
                scale,
                growth_factor,
            );
            let p90 = calculate_percentile(
                (count as f64 * 0.9) as i64,
                count,
                bucket_counts,
                scale,
                growth_factor,
            );
            let p95 = calculate_percentile(
                (count as f64 * 0.95) as i64,
                count,
                bucket_counts,
                scale,
                growth_factor,
            );
            let p999 = calculate_percentile(
                (count as f64 * 0.999) as i64,
                count,
                bucket_counts,
                scale,
                growth_factor,
            );

            results.push(HloExecutionTiming {
                device_id,
                mean_us: mean,
                p50_us: p50,
                p90_us: p90,
                p95_us: p95,
                p999_us: p999,
            });
        }
    }

    results.sort_by_key(|r| r.device_id);
    if results.is_empty() {
        None
    } else {
        Some(results)
    }
}

/// Synchronous wrapper for get_hlo_queue_size
pub fn get_hlo_queue_size_sync() -> Option<Vec<HloQueueSize>> {
    block_on_grpc(|cache| async move {
        let channel = default_link().channel(cache, Instant::now()).await?;
        hlo_queue_size(channel).await
    })
    .flatten()
}

/// Synchronous wrapper for get_hlo_execution_timing
pub fn get_hlo_execution_timing_sync() -> Option<Vec<HloExecutionTiming>> {
    block_on_grpc(|cache| async move {
        let channel = default_link().channel(cache, Instant::now()).await?;
        hlo_execution_timing(channel).await
    })
    .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_grpc_connection() {
        // This test will pass if no TPU workload is running (expected to fail gracefully)
        let result = get_tpu_metrics_grpc().await;
        println!("gRPC metrics result: {result:?}");
    }

    #[test]
    fn connect_backoff_covers_the_retry_interval_only() {
        let failed_at = Instant::now();
        assert!(!connect_backoff_active(None, failed_at));
        assert!(connect_backoff_active(Some(failed_at), failed_at));
        assert!(connect_backoff_active(
            Some(failed_at),
            failed_at + GRPC_RETRY_INTERVAL - Duration::from_millis(1)
        ));
        assert!(!connect_backoff_active(
            Some(failed_at),
            failed_at + GRPC_RETRY_INTERVAL
        ));
    }

    /// A loopback address nothing listens on: bind an ephemeral port, note
    /// it, and close the listener.
    fn unused_loopback_addr() -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().expect("local addr").port();
        drop(listener);
        format!("http://127.0.0.1:{port}")
    }

    #[tokio::test]
    async fn a_dead_server_is_connected_to_once_per_retry_interval() {
        let link = GrpcLink::new(unused_loopback_addr());

        // Two ticks back to back: metrics fail, HLO is skipped, and the
        // second tick does not even try to connect.
        let first = collect_snapshot(&link, true).await;
        let second = collect_snapshot(&link, true).await;

        assert!(first.metrics.is_none());
        assert!(first.hlo_queue_sizes.is_none() && first.hlo_exec_timings.is_none());
        assert!(second.metrics.is_none());
        assert_eq!(link.connect_attempts.load(Ordering::Relaxed), 1);

        // Once the interval has passed, the next call connects again.
        let later = Instant::now() + GRPC_RETRY_INTERVAL;
        assert!(link.channel(true, later).await.is_none());
        assert_eq!(link.connect_attempts.load(Ordering::Relaxed), 2);
    }
}
