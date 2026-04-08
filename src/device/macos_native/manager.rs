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

//! Native metrics manager for Apple Silicon
//!
//! This module provides a unified manager for collecting Apple Silicon
//! metrics using native macOS APIs instead of the powermetrics command.
//!
//! ## Features
//! - No sudo required
//! - Lower latency
//! - More stable (no external process)
//! - Additional metrics (temperature, system power)
//!
//! ## Usage
//! ```ignore
//! use all_smi::device::macos_native::{initialize_native_metrics_manager, get_native_metrics_manager};
//!
//! // Initialize once at startup
//! initialize_native_metrics_manager(1000)?; // 1 second sample interval
//!
//! // Get metrics
//! if let Some(manager) = get_native_metrics_manager() {
//!     let data = manager.get_latest_data()?;
//!     println!("CPU Power: {:.2}W", data.cpu_power_mw / 1000.0);
//! }
//! ```

use super::ioreport::{IOReport, IOReportMetrics};
use super::metrics::NativeMetricsData;
use super::smc::SMCMetrics;
use super::thermal::get_thermal_state;
use once_cell::sync::Lazy;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::thread;
use std::time::Duration;

/// Global singleton for NativeMetricsManager
static NATIVE_METRICS_MANAGER: Lazy<Mutex<Option<Arc<NativeMetricsManager>>>> =
    Lazy::new(|| Mutex::new(None));

/// Track if first data has been received
static FIRST_DATA_RECEIVED: AtomicBool = AtomicBool::new(false);

/// Configuration for the native metrics manager
#[derive(Debug, Clone)]
pub struct NativeMetricsConfig {
    /// Sample interval in milliseconds for IOReport
    pub sample_interval_ms: u64,
    /// Number of samples to average (smooths out transient variations)
    pub sample_count: usize,
    /// Enable SMC temperature collection
    #[allow(dead_code)]
    pub enable_smc: bool,
}

impl Default for NativeMetricsConfig {
    fn default() -> Self {
        Self {
            sample_interval_ms: 100, // 100ms sample window
            sample_count: 4,         // Average 4 samples (like macmon)
            enable_smc: true,
        }
    }
}

/// Manages native metrics collection for Apple Silicon
pub struct NativeMetricsManager {
    config: NativeMetricsConfig,
    #[allow(dead_code)]
    ioreport: Mutex<Option<IOReport>>,
    latest_data: RwLock<Option<NativeMetricsData>>,
    last_collection_time: RwLock<Option<std::time::Instant>>,
    /// Mutex to prevent concurrent collections (only one collection at a time)
    collection_lock: Mutex<()>,
    is_running: AtomicBool,
    collector_handle: Mutex<Option<thread::JoinHandle<()>>>,
}

impl NativeMetricsManager {
    /// Create a new NativeMetricsManager
    ///
    /// Note: The `_interval_ms` parameter is kept for API compatibility but is not used.
    /// IOReport sampling uses a fixed 100ms interval for optimal performance.
    pub fn new(_interval_ms: u64) -> Result<Self, Box<dyn std::error::Error>> {
        // Use default config with 100ms sample interval for fast IOReport delta sampling
        // The CLI interval parameter is for data collection frequency, not IOReport sampling
        let config = NativeMetricsConfig::default();

        // Initialize IOReport
        let ioreport = IOReport::new().map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;

        Ok(Self {
            config,
            ioreport: Mutex::new(Some(ioreport)),
            latest_data: RwLock::new(None),
            last_collection_time: RwLock::new(None),
            collection_lock: Mutex::new(()),
            is_running: AtomicBool::new(false),
            collector_handle: Mutex::new(None),
        })
    }

    /// Start background collection
    #[allow(dead_code)]
    pub fn start(&self) -> Result<(), Box<dyn std::error::Error>> {
        if self.is_running.load(Ordering::Acquire) {
            return Ok(());
        }

        self.is_running.store(true, Ordering::Release);

        // Take ownership of IOReport for the collector thread
        let mut ioreport_guard = self.ioreport.lock().unwrap();
        let ioreport = ioreport_guard.take().ok_or("IOReport already taken")?;

        let config = self.config.clone();
        let is_running = Arc::new(AtomicBool::new(true));
        let is_running_clone = is_running.clone();

        // Create a channel to send data back
        let (tx, rx) = std::sync::mpsc::channel::<NativeMetricsData>();

        // Spawn collector thread
        let handle = thread::spawn(move || {
            Self::collector_loop(ioreport, config, is_running_clone, tx);
        });

        // Store the handle
        *self.collector_handle.lock().unwrap() = Some(handle);

        // Spawn a thread to receive data and update latest_data
        // This is a simplified approach - in a full implementation, we'd
        // use a proper async mechanism
        let latest_data = Arc::new(RwLock::new(None::<NativeMetricsData>));
        let latest_data_clone = latest_data.clone();

        thread::spawn(move || {
            while let Ok(data) = rx.recv() {
                if let Ok(mut guard) = latest_data_clone.write() {
                    *guard = Some(data);
                    FIRST_DATA_RECEIVED.store(true, Ordering::Relaxed);
                }
            }
        });

        Ok(())
    }

    /// Background collection loop
    #[allow(dead_code)]
    fn collector_loop(
        mut ioreport: IOReport,
        config: NativeMetricsConfig,
        is_running: Arc<AtomicBool>,
        tx: std::sync::mpsc::Sender<NativeMetricsData>,
    ) {
        while is_running.load(Ordering::Relaxed) {
            // Collect multiple samples and average them
            let mut samples: Vec<IOReportMetrics> = Vec::with_capacity(config.sample_count);

            for _ in 0..config.sample_count {
                if !is_running.load(Ordering::Relaxed) {
                    return;
                }

                match ioreport.get_sample(config.sample_interval_ms) {
                    Ok((iterator, duration_ns)) => {
                        let metrics = IOReportMetrics::from_sample(iterator, duration_ns);
                        samples.push(metrics);
                    }
                    Err(_e) => {
                        #[cfg(debug_assertions)]
                        eprintln!("IOReport sample failed: {_e}");
                    }
                }
            }

            if samples.is_empty() {
                thread::sleep(Duration::from_millis(config.sample_interval_ms));
                continue;
            }

            // Average the samples
            let avg_metrics = Self::average_samples(&samples);

            // Collect SMC metrics
            let smc_metrics = if config.enable_smc {
                SMCMetrics::collect()
            } else {
                SMCMetrics::default()
            };

            // Get thermal state
            let thermal_state = get_thermal_state();

            // Combine into unified metrics
            let native_data =
                NativeMetricsData::from_components(avg_metrics, smc_metrics, thermal_state);

            // Send to receiver
            if tx.send(native_data).is_err() {
                // Receiver dropped, stop collecting
                break;
            }
        }
    }

    /// Average multiple IOReport samples
    fn average_samples(samples: &[IOReportMetrics]) -> IOReportMetrics {
        if samples.is_empty() {
            return IOReportMetrics::default();
        }

        let count = samples.len() as f64;
        let mut avg = IOReportMetrics::default();

        for sample in samples {
            avg.cpu_power += sample.cpu_power;
            avg.gpu_power += sample.gpu_power;
            avg.ane_power += sample.ane_power;
            avg.dram_power += sample.dram_power;
            avg.package_power += sample.package_power;
            avg.e_cluster_freq += sample.e_cluster_freq;
            avg.p_cluster_freq += sample.p_cluster_freq;
            avg.e_cluster_residency += sample.e_cluster_residency;
            avg.p_cluster_residency += sample.p_cluster_residency;
            avg.gpu_freq += sample.gpu_freq;
            avg.gpu_residency += sample.gpu_residency;
        }

        avg.cpu_power /= count;
        avg.gpu_power /= count;
        avg.ane_power /= count;
        avg.dram_power /= count;
        avg.package_power /= count;
        avg.e_cluster_freq = (avg.e_cluster_freq as f64 / count) as u32;
        avg.p_cluster_freq = (avg.p_cluster_freq as f64 / count) as u32;
        avg.e_cluster_residency /= count;
        avg.p_cluster_residency /= count;
        avg.gpu_freq = (avg.gpu_freq as f64 / count) as u32;
        avg.gpu_residency /= count;

        // Use cluster data from last sample for detail
        if let Some(last) = samples.last() {
            avg.e_cluster_data = last.e_cluster_data.clone();
            avg.p_cluster_data = last.p_cluster_data.clone();
        }

        avg
    }

    /// Get the latest collected metrics
    #[allow(dead_code)]
    pub fn get_latest_data(&self) -> Result<NativeMetricsData, Box<dyn std::error::Error>> {
        let guard = self.latest_data.read().map_err(|_| "Lock poisoned")?;
        guard.clone().ok_or_else(|| "No data available yet".into())
    }

    /// Get the latest data as a Result for consistent API usage
    #[allow(dead_code)]
    pub fn get_latest_data_result(&self) -> Result<NativeMetricsData, Box<dyn std::error::Error>> {
        self.get_latest_data()
    }

    /// Check if data is available
    #[allow(dead_code)]
    pub fn has_data(&self) -> bool {
        self.latest_data
            .read()
            .map(|guard| guard.is_some())
            .unwrap_or(false)
    }

    /// Collect a single sample synchronously (for testing or one-shot use)
    ///
    /// This method implements caching: if called within 500ms of a previous collection,
    /// it returns the cached data instead of collecting new samples.
    /// Uses double-checked locking to prevent concurrent collections.
    pub fn collect_once(&self) -> Result<NativeMetricsData, Box<dyn std::error::Error>> {
        // Use longer cache duration during startup to handle tokio blocking
        // After first few calls, use shorter duration for responsiveness
        static CALL_COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let call_num = CALL_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        // First 10 calls use 5 second cache (startup phase)
        // After that, use 500ms cache (normal operation)
        let cache_duration_ms: u128 = if call_num < 10 { 5000 } else { 500 };

        // First check: quick read-only cache check (no lock)
        if let (Ok(time_guard), Ok(data_guard)) =
            (self.last_collection_time.read(), self.latest_data.read())
            && let (Some(last_time), Some(data)) = (*time_guard, data_guard.clone())
            && last_time.elapsed().as_millis() < cache_duration_ms
        {
            return Ok(data);
        }

        // Acquire collection lock to prevent concurrent collections
        let _lock = self
            .collection_lock
            .lock()
            .map_err(|_| "Collection lock poisoned")?;

        // Second check: re-check cache after acquiring lock (another thread may have collected)
        if let (Ok(time_guard), Ok(data_guard)) =
            (self.last_collection_time.read(), self.latest_data.read())
            && let (Some(last_time), Some(data)) = (*time_guard, data_guard.clone())
            && last_time.elapsed().as_millis() < cache_duration_ms
        {
            return Ok(data);
        }

        // OPTIMIZATION: Reuse the existing IOReport instance instead of creating a new one
        // Creating IOReport::new() is expensive (involves IOKit setup)
        let mut ioreport_guard = self.ioreport.lock().map_err(|_| "IOReport lock poisoned")?;
        let ioreport = ioreport_guard.as_mut().ok_or("IOReport not initialized")?;

        // For first collection, use fewer samples for faster startup
        // Subsequent calls can use full sample count for accuracy
        static FIRST_COLLECTION: std::sync::atomic::AtomicBool =
            std::sync::atomic::AtomicBool::new(true);

        let sample_count = if FIRST_COLLECTION.swap(false, std::sync::atomic::Ordering::Relaxed) {
            1 // First call: single sample for fast startup (~100ms)
        } else {
            self.config.sample_count // Subsequent calls: full averaging
        };

        // Collect samples
        let mut samples: Vec<IOReportMetrics> = Vec::new();
        for _ in 0..sample_count {
            let (iterator, duration_ns) = ioreport.get_sample(self.config.sample_interval_ms)?;
            samples.push(IOReportMetrics::from_sample(iterator, duration_ns));
        }

        // Average samples
        let avg_metrics = Self::average_samples(&samples);

        // Collect SMC metrics
        let smc_metrics = SMCMetrics::collect();

        // Get thermal state
        let thermal_state = get_thermal_state();

        // Combine
        let data = NativeMetricsData::from_components(avg_metrics, smc_metrics, thermal_state);

        // Update latest data and timestamp
        if let Ok(mut guard) = self.latest_data.write() {
            *guard = Some(data.clone());
            FIRST_DATA_RECEIVED.store(true, Ordering::Relaxed);
        }
        if let Ok(mut guard) = self.last_collection_time.write() {
            *guard = Some(std::time::Instant::now());
        }

        Ok(data)
    }

    /// Shutdown the manager
    pub fn shutdown(&self) {
        self.is_running.store(false, Ordering::Release);

        // Wait for collector thread to finish
        if let Ok(mut guard) = self.collector_handle.lock()
            && let Some(handle) = guard.take()
        {
            let _ = handle.join();
        }

        FIRST_DATA_RECEIVED.store(false, Ordering::Relaxed);
    }
}

impl Drop for NativeMetricsManager {
    fn drop(&mut self) {
        self.shutdown();
    }
}

// Safety: NativeMetricsManager uses thread-safe primitives
unsafe impl Send for NativeMetricsManager {}
unsafe impl Sync for NativeMetricsManager {}

/// Initialize the global native metrics manager
///
/// This should be called once at startup for macOS Apple Silicon systems.
/// Also pre-collects first data sample to warm up the cache for faster startup.
///
/// # Arguments
/// * `interval_ms` - Sample interval in milliseconds (minimum 50ms)
///
/// # Returns
/// Ok(()) if initialization succeeded, Err if it failed
pub fn initialize_native_metrics_manager(
    interval_ms: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut manager_guard = NATIVE_METRICS_MANAGER.lock().map_err(|_| "Lock poisoned")?;

    if manager_guard.is_none() {
        let manager = NativeMetricsManager::new(interval_ms)?;

        // Pre-collect first data sample to warm up the cache
        // This ensures all subsequent calls from readers use cached data
        let _ = manager.collect_once();

        *manager_guard = Some(Arc::new(manager));
    }

    Ok(())
}

/// Get the global native metrics manager instance
pub fn get_native_metrics_manager() -> Option<Arc<NativeMetricsManager>> {
    NATIVE_METRICS_MANAGER.lock().ok()?.clone()
}

/// Shutdown and cleanup the native metrics manager
#[allow(dead_code)]
pub fn shutdown_native_metrics_manager() {
    if let Ok(mut guard) = NATIVE_METRICS_MANAGER.lock()
        && let Some(manager) = guard.take()
    {
        manager.shutdown();
    }
    FIRST_DATA_RECEIVED.store(false, Ordering::Relaxed);
}

/// Check if native metrics have received first data
#[allow(dead_code)]
pub fn has_native_metrics_data() -> bool {
    FIRST_DATA_RECEIVED.load(Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_defaults() {
        let config = NativeMetricsConfig::default();
        assert_eq!(config.sample_interval_ms, 100);
        assert_eq!(config.sample_count, 4);
        assert!(config.enable_smc);
    }

    #[test]
    fn test_average_samples_empty() {
        let result = NativeMetricsManager::average_samples(&[]);
        assert_eq!(result.cpu_power, 0.0);
    }

    #[test]
    fn test_average_samples() {
        let samples = vec![
            IOReportMetrics {
                cpu_power: 2.0,
                gpu_power: 1.0,
                ..Default::default()
            },
            IOReportMetrics {
                cpu_power: 4.0,
                gpu_power: 3.0,
                ..Default::default()
            },
        ];

        let avg = NativeMetricsManager::average_samples(&samples);
        assert!((avg.cpu_power - 3.0).abs() < 0.01);
        assert!((avg.gpu_power - 2.0).abs() < 0.01);
    }
}
