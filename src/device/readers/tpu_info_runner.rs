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

use regex::Regex;
use std::collections::HashMap;
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::thread;
use std::time::{Duration, Instant};
use tracing::debug;

static RUNNER: OnceLock<TpuInfoRunner> = OnceLock::new();
static ANSI_REGEX: OnceLock<Regex> = OnceLock::new();

/// CLI polling interval when gRPC is NOT available (fallback mode)
/// Set high to reduce overhead - CLI spawns a Python process each time
const POLL_INTERVAL_IDLE_SECS: u64 = 30;

/// CLI polling interval when gRPC IS available
/// This is rarely used since gRPC handles metrics directly
const POLL_INTERVAL_ACTIVE_SECS: u64 = 5;

/// Minimum time between CLI executions to prevent hammering
#[allow(dead_code)]
const MIN_CLI_INTERVAL_SECS: u64 = 5;

pub fn get_runner() -> &'static TpuInfoRunner {
    RUNNER.get_or_init(TpuInfoRunner::new)
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum TableType {
    None,
    RuntimeUtilization,
    DutyCycle,
    HbmUsage,
    TensorCoreUtilization,
}

#[derive(Clone)]
pub struct TpuInfoRunner {
    /// Latest captured metrics per device index
    /// HashMap<DeviceIndex, HashMap<MetricName, Value>>
    pub device_metrics: Arc<RwLock<HashMap<u32, HashMap<String, f64>>>>,
    /// Status message for notification
    pub status: Arc<Mutex<String>>,
    /// Whether gRPC is currently available (set by tpu_grpc module)
    grpc_available: Arc<AtomicBool>,
    /// Last CLI execution timestamp (unix seconds)
    last_cli_run: Arc<AtomicU64>,
}

impl Default for TpuInfoRunner {
    fn default() -> Self {
        Self::new()
    }
}

impl TpuInfoRunner {
    pub fn new() -> Self {
        let runner = Self {
            device_metrics: Arc::new(RwLock::new(HashMap::new())),
            status: Arc::new(Mutex::new("Initializing tpu-info...".to_string())),
            grpc_available: Arc::new(AtomicBool::new(false)),
            last_cli_run: Arc::new(AtomicU64::new(0)),
        };
        runner.start_background_thread();
        runner
    }

    /// Notify that gRPC is available (reduces CLI polling frequency)
    pub fn set_grpc_available(&self, available: bool) {
        self.grpc_available.store(available, Ordering::Relaxed);
    }

    /// Check if gRPC is currently available
    #[allow(dead_code)]
    pub fn is_grpc_available(&self) -> bool {
        self.grpc_available.load(Ordering::Relaxed)
    }

    /// Force an immediate CLI refresh (within rate limits)
    #[allow(dead_code)]
    pub fn request_refresh(&self) {
        let now = Instant::now().elapsed().as_secs();
        let last = self.last_cli_run.load(Ordering::Relaxed);

        // Only allow refresh if enough time has passed
        if now.saturating_sub(last) >= MIN_CLI_INTERVAL_SECS {
            self.run_cli_once();
        }
    }

    fn start_background_thread(&self) {
        let metrics_store = self.device_metrics.clone();
        let status = self.status.clone();
        let grpc_available = self.grpc_available.clone();
        let last_cli_run = self.last_cli_run.clone();

        thread::spawn(move || {
            // Run once immediately at startup
            Self::run_cli_internal(&metrics_store, &status, &last_cli_run);

            loop {
                // Determine polling interval based on gRPC availability
                let poll_interval = if grpc_available.load(Ordering::Relaxed) {
                    // gRPC is handling metrics - poll CLI less frequently
                    POLL_INTERVAL_ACTIVE_SECS
                } else {
                    // Fallback mode - still poll infrequently to reduce overhead
                    POLL_INTERVAL_IDLE_SECS
                };

                thread::sleep(Duration::from_secs(poll_interval));

                // Skip CLI execution if gRPC is available
                // (gRPC provides real-time metrics, CLI is just for fallback)
                if !grpc_available.load(Ordering::Relaxed) {
                    Self::run_cli_internal(&metrics_store, &status, &last_cli_run);
                }
            }
        });
    }

    fn run_cli_once(&self) {
        Self::run_cli_internal(&self.device_metrics, &self.status, &self.last_cli_run);
    }

    fn run_cli_internal(
        metrics_store: &Arc<RwLock<HashMap<u32, HashMap<String, f64>>>>,
        status: &Arc<Mutex<String>>,
        last_cli_run: &Arc<AtomicU64>,
    ) {
        // Update last run timestamp
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        last_cli_run.store(now, Ordering::Relaxed);

        // Run tpu-info in normal mode (NOT streaming mode)
        // Streaming mode uses Rich's Live display which doesn't work with pipes
        let output_res = Command::new("tpu-info")
            .env("TERM", "dumb")
            .env("NO_COLOR", "1")
            .env("FORCE_COLOR", "0")
            .output();

        match output_res {
            Ok(output) => {
                if output.status.success() {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    let mut current_table = TableType::None;
                    let mut any_updated = false;

                    for line in stdout.lines() {
                        let updated = Self::parse_line(line, &mut current_table, metrics_store);
                        if updated {
                            any_updated = true;
                        }
                    }

                    if any_updated {
                        let mut s = status.lock().unwrap();
                        *s = "Ready".to_string();
                    } else {
                        // Check if we got any data at all
                        let metrics = metrics_store.read().unwrap();
                        if metrics.is_empty() {
                            let mut s = status.lock().unwrap();
                            *s = "tpu-info running, no metrics yet...".to_string();
                        }
                    }
                } else {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    let mut s = status.lock().unwrap();
                    *s = format!(
                        "tpu-info error: {}",
                        stderr.lines().next().unwrap_or("unknown error")
                    );
                }
            }
            Err(e) => {
                let mut s = status.lock().unwrap();
                *s = format!("Failed to run tpu-info: {e}");
            }
        }
    }

    fn parse_line(
        line: &str,
        current_table: &mut TableType,
        store: &Arc<RwLock<HashMap<u32, HashMap<String, f64>>>>,
    ) -> bool {
        let ansi_regex = ANSI_REGEX.get_or_init(|| {
            Regex::new(
                r"[\u001b\u009b][\[()#;?]*(?:[0-9]{1,4}(?:;[0-9]{0,4})*)?[0-9A-ORZcf-nqry=><]",
            )
            .unwrap()
        });
        let line_no_ansi = ansi_regex.replace_all(line, "");
        let line = line_no_ansi.trim();

        if line.is_empty() {
            return false;
        }

        // 1. Detect table headers
        if line.contains("TPU Runtime Utilization") {
            *current_table = TableType::RuntimeUtilization;
            return false;
        } else if line.contains("TPU Duty Cycle") {
            *current_table = TableType::DutyCycle;
            return false;
        } else if line.contains("TPU HBM Usage") {
            *current_table = TableType::HbmUsage;
            return false;
        } else if line.contains("TensorCore Utilization") {
            *current_table = TableType::TensorCoreUtilization;
            return false;
        } else if line.contains("Runtime Utilization Status")
            || line.contains("Supported Metrics")
            || line.contains("TPU Chips")
            || line.contains("TPU Process Info")
        {
            *current_table = TableType::None; // Skip other tables/warnings
            return false;
        }

        // 2. Parse table rows
        if line.contains('│') || line.contains('┃') || line.contains('|') {
            let normalized_line = line.replace(['│', '┃'], "|");
            let parts: Vec<&str> = normalized_line
                .split('|')
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .collect();

            let mut updated = false;
            match *current_table {
                TableType::RuntimeUtilization => {
                    // Header: ["Chip", "HBM Usage (GiB)", "Duty cycle"]
                    // Row: "0", "1.23 GiB / 16.00 GiB", "45.67%" (or "N/A")
                    if parts.len() >= 3
                        && let Ok(idx) = parts[0].parse::<u32>()
                    {
                        let hbm_str = parts[1];
                        let duty_str = parts[2];

                        if hbm_str != "N/A" {
                            let (used, total) = Self::parse_hbm_usage(hbm_str);
                            if let Ok(mut map_guard) = store.write() {
                                let dev_map = map_guard.entry(idx).or_insert_with(HashMap::new);
                                dev_map.insert("hbm_usage".to_string(), used);
                                dev_map.insert("memory_total".to_string(), total);
                                updated = true;
                            }
                            debug!("Parsed RuntimeUtil HBM [Dev {}]: {} / {}", idx, used, total);
                        }

                        if duty_str != "N/A" && !duty_str.is_empty() {
                            let duty = Self::parse_percent(duty_str);
                            if let Ok(mut map_guard) = store.write() {
                                let dev_map = map_guard.entry(idx).or_insert_with(HashMap::new);
                                dev_map.insert("duty_cycle_percent".to_string(), duty);
                                updated = true;
                            }
                            debug!("Parsed RuntimeUtil Duty [Dev {}]: {}", idx, duty);
                        }
                    }
                }
                TableType::DutyCycle => {
                    // Header: ["Core ID", "Duty Cycle (%)"]
                    // Row: "0", "N/A" or "10.5%"
                    if parts.len() >= 2
                        && let Ok(idx) = parts[0].parse::<u32>()
                    {
                        let val_str = parts[1];
                        if val_str != "N/A" {
                            let val = Self::parse_percent(val_str);
                            if let Ok(mut map_guard) = store.write() {
                                let dev_map = map_guard.entry(idx).or_insert_with(HashMap::new);
                                dev_map.insert("duty_cycle_percent".to_string(), val);
                                updated = true;
                            }
                            debug!("Parsed DutyCycle [Dev {}]: {}", idx, val);
                        }
                    }
                }
                TableType::HbmUsage => {
                    // Header: ["Device", "HBM Usage (GiB)"]
                    // Row: "0", "N/A" or "1.23 GiB / 16.00 GiB"
                    if parts.len() >= 2
                        && let Ok(idx) = parts[0].parse::<u32>()
                    {
                        let val_str = parts[1];
                        if val_str != "N/A" {
                            let (used, total) = Self::parse_hbm_usage(val_str);
                            if let Ok(mut map_guard) = store.write() {
                                let dev_map = map_guard.entry(idx).or_insert_with(HashMap::new);
                                dev_map.insert("hbm_usage".to_string(), used);
                                dev_map.insert("memory_total".to_string(), total);
                                updated = true;
                            }
                            debug!("Parsed HBM [Dev {}]: {} / {}", idx, used, total);
                        }
                    }
                }
                TableType::TensorCoreUtilization => {
                    // Header: ["Core ID", "TensorCore Utilization"]
                    // Row: "0", "0.00%"
                    if parts.len() >= 2
                        && let Ok(idx) = parts[0].parse::<u32>()
                    {
                        let val_str = parts[1];
                        if val_str != "N/A" {
                            let util = Self::parse_percent(val_str);
                            if let Ok(mut map_guard) = store.write() {
                                let dev_map = map_guard.entry(idx).or_insert_with(HashMap::new);
                                dev_map.insert("tensorcore_utilization".to_string(), util);
                                updated = true;
                            }
                            debug!("Parsed TensorCore [Dev {}]: {}", idx, util);
                        }
                    }
                }
                TableType::None => {}
            }
            return updated;
        }
        false
    }

    fn parse_hbm_usage(s: &str) -> (f64, f64) {
        // "1.23 GiB / 16.00 GiB"
        let parts: Vec<&str> = s.split('/').map(|p| p.trim()).collect();
        if parts.len() >= 2 {
            (Self::parse_bytes(parts[0]), Self::parse_bytes(parts[1]))
        } else {
            (0.0, 0.0)
        }
    }

    fn parse_bytes(s: &str) -> f64 {
        let parts: Vec<&str> = s.split_whitespace().collect();
        if parts.is_empty() {
            return 0.0;
        }
        if let Ok(mut val) = parts[0].parse::<f64>() {
            if parts.len() >= 2 {
                let unit = parts[1].to_lowercase();
                if unit.contains("gi") || unit == "gb" {
                    val *= 1024.0 * 1024.0 * 1024.0;
                } else if unit.contains("mi") || unit == "mb" {
                    val *= 1024.0 * 1024.0;
                } else if unit.contains("ki") || unit == "kb" {
                    val *= 1024.0;
                }
            }
            val
        } else {
            0.0
        }
    }

    fn parse_percent(s: &str) -> f64 {
        // "45.67%" or "N/A"
        s.trim_end_matches('%').parse::<f64>().unwrap_or(0.0)
    }

    pub fn get_status(&self) -> Option<String> {
        let s = self.status.lock().unwrap().clone();
        if s == "Ready" { None } else { Some(s) }
    }

    pub fn get_metric(&self, device_idx: u32, key: &str) -> Option<f64> {
        self.device_metrics
            .read()
            .unwrap()
            .get(&device_idx)
            .and_then(|m| m.get(key).copied())
    }
}
