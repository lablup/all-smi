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

//! High-level client API for all-smi library.
//!
//! This module provides the main [`AllSmi`] struct, which offers a simple,
//! ergonomic interface for querying GPU, CPU, memory, and process information
//! across all supported platforms.
//!
//! # Example
//!
//! ```rust,no_run
//! use all_smi::{AllSmi, Result};
//!
//! fn main() -> Result<()> {
//!     // Initialize with auto-detection
//!     let smi = AllSmi::new()?;
//!
//!     // Get all GPU/NPU information
//!     let gpus = smi.get_gpu_info();
//!     for gpu in &gpus {
//!         println!("{}: {}% utilization, {:.1}W",
//!             gpu.name, gpu.utilization, gpu.power_consumption);
//!     }
//!
//!     // Get CPU information
//!     let cpus = smi.get_cpu_info();
//!     for cpu in &cpus {
//!         println!("{}: {:.1}% utilization", cpu.cpu_model, cpu.utilization);
//!     }
//!
//!     // Get memory information
//!     let memory = smi.get_memory_info();
//!     for mem in &memory {
//!         println!("Memory: {:.1}% used", mem.utilization);
//!     }
//!
//!     Ok(())
//! }
//! ```

use crate::device::{
    create_chassis_reader, get_cpu_readers, get_gpu_readers, get_memory_readers, ChassisInfo,
    ChassisReader, CpuInfo, CpuReader, GpuInfo, GpuReader, MemoryInfo, MemoryReader, ProcessInfo,
};
use crate::error::Result;
use crate::storage::{create_storage_reader, StorageInfo, StorageReader};

#[cfg(target_os = "macos")]
use crate::device::macos_native::{
    initialize_native_metrics_manager, shutdown_native_metrics_manager,
};

#[cfg(target_os = "linux")]
use crate::device::hlsmi::{initialize_hlsmi_manager, shutdown_hlsmi_manager};

#[cfg(target_os = "linux")]
use crate::device::platform_detection::has_gaudi;

/// The type of device that can be monitored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DeviceType {
    /// NVIDIA GPU
    NvidiaGpu,
    /// AMD GPU
    AmdGpu,
    /// Apple Silicon GPU
    AppleSiliconGpu,
    /// NVIDIA Jetson
    NvidiaJetson,
    /// Intel Gaudi NPU
    IntelGaudi,
    /// Furiosa NPU
    FuriosaNpu,
    /// Rebellions NPU
    RebellionsNpu,
    /// Tenstorrent NPU
    TenstorrentNpu,
    /// Google TPU
    GoogleTpu,
}

impl std::fmt::Display for DeviceType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DeviceType::NvidiaGpu => write!(f, "NVIDIA GPU"),
            DeviceType::AmdGpu => write!(f, "AMD GPU"),
            DeviceType::AppleSiliconGpu => write!(f, "Apple Silicon GPU"),
            DeviceType::NvidiaJetson => write!(f, "NVIDIA Jetson"),
            DeviceType::IntelGaudi => write!(f, "Intel Gaudi"),
            DeviceType::FuriosaNpu => write!(f, "Furiosa NPU"),
            DeviceType::RebellionsNpu => write!(f, "Rebellions NPU"),
            DeviceType::TenstorrentNpu => write!(f, "Tenstorrent NPU"),
            DeviceType::GoogleTpu => write!(f, "Google TPU"),
        }
    }
}

/// Main client for accessing hardware monitoring information.
///
/// `AllSmi` provides a high-level API for querying GPU, NPU, CPU, and memory
/// information across all supported platforms. It handles platform-specific
/// initialization and cleanup automatically.
///
/// # Thread Safety
///
/// `AllSmi` is `Send + Sync` and can be safely shared across threads.
///
/// # Example
///
/// ```rust,no_run
/// use all_smi::AllSmi;
///
/// let smi = AllSmi::new().expect("Failed to initialize");
///
/// // Query GPU information
/// for gpu in smi.get_gpu_info() {
///     println!("{}: {}% utilization", gpu.name, gpu.utilization);
/// }
/// ```
pub struct AllSmi {
    gpu_readers: Vec<Box<dyn GpuReader>>,
    cpu_readers: Vec<Box<dyn CpuReader>>,
    memory_readers: Vec<Box<dyn MemoryReader>>,
    chassis_reader: Box<dyn ChassisReader>,
    storage_reader: Box<dyn StorageReader>,
    #[cfg(target_os = "macos")]
    _macos_initialized: bool,
    #[cfg(target_os = "linux")]
    _gaudi_initialized: bool,
}

impl AllSmi {
    /// Create a new `AllSmi` instance with auto-detected hardware.
    ///
    /// This constructor initializes all platform-specific managers and
    /// creates readers for available hardware. It does not fail if no
    /// hardware is detected; instead, the corresponding `get_*_info()`
    /// methods will return empty collections.
    ///
    /// # Errors
    ///
    /// Returns an error if platform initialization fails critically
    /// (e.g., macOS IOReport API unavailable, or system-level errors).
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use all_smi::AllSmi;
    ///
    /// let smi = AllSmi::new()?;
    /// println!("Found {} GPU(s)", smi.get_gpu_info().len());
    /// # Ok::<(), all_smi::Error>(())
    /// ```
    #[must_use = "AllSmi instance must be stored to access hardware information"]
    pub fn new() -> Result<Self> {
        Self::with_config(AllSmiConfig::default())
    }

    /// Create a new `AllSmi` instance with custom configuration.
    ///
    /// # Arguments
    ///
    /// * `config` - Configuration options for the client
    ///
    /// # Errors
    ///
    /// Returns an error if platform initialization fails.
    #[must_use = "AllSmi instance must be stored to access hardware information"]
    pub fn with_config(config: AllSmiConfig) -> Result<Self> {
        // Initialize platform-specific managers
        #[cfg(target_os = "macos")]
        let macos_initialized = {
            match initialize_native_metrics_manager(config.sample_interval_ms) {
                Ok(()) => true,
                Err(e) => {
                    // Log but don't fail - some metrics may still work
                    if config.verbose {
                        eprintln!("Warning: macOS native metrics init failed: {e}");
                    }
                    false
                }
            }
        };

        #[cfg(target_os = "linux")]
        let gaudi_initialized = {
            if has_gaudi() {
                match initialize_hlsmi_manager(config.sample_interval_ms / 1000) {
                    Ok(()) => true,
                    Err(e) => {
                        if config.verbose {
                            eprintln!("Warning: Intel Gaudi hl-smi init failed: {e}");
                        }
                        false
                    }
                }
            } else {
                false
            }
        };

        // Get readers
        let gpu_readers = get_gpu_readers();
        let cpu_readers = get_cpu_readers();
        let memory_readers = get_memory_readers();
        let chassis_reader = create_chassis_reader();
        let storage_reader = create_storage_reader();

        Ok(AllSmi {
            gpu_readers,
            cpu_readers,
            memory_readers,
            chassis_reader,
            storage_reader,
            #[cfg(target_os = "macos")]
            _macos_initialized: macos_initialized,
            #[cfg(target_os = "linux")]
            _gaudi_initialized: gaudi_initialized,
        })
    }

    /// Get information about all detected GPUs and NPUs.
    ///
    /// Returns a vector of [`GpuInfo`] structs containing metrics for each
    /// detected accelerator. The list includes NVIDIA GPUs, AMD GPUs,
    /// Apple Silicon GPUs, Intel Gaudi NPUs, and other supported devices.
    ///
    /// Returns an empty vector if no devices are detected.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use all_smi::AllSmi;
    ///
    /// let smi = AllSmi::new()?;
    /// for gpu in smi.get_gpu_info() {
    ///     println!("{}: {}% util, {:.1}W power, {}MB/{} MB memory",
    ///         gpu.name,
    ///         gpu.utilization,
    ///         gpu.power_consumption,
    ///         gpu.used_memory / 1024 / 1024,
    ///         gpu.total_memory / 1024 / 1024);
    /// }
    /// # Ok::<(), all_smi::Error>(())
    /// ```
    pub fn get_gpu_info(&self) -> Vec<GpuInfo> {
        let mut all_gpus = Vec::new();
        for reader in &self.gpu_readers {
            all_gpus.extend(reader.get_gpu_info());
        }
        all_gpus
    }

    /// Get information about GPU/NPU processes.
    ///
    /// Returns a vector of [`ProcessInfo`] structs containing information
    /// about processes using GPU resources. This includes process ID, name,
    /// GPU memory usage, and other metrics.
    ///
    /// Returns an empty vector if no GPU processes are found.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use all_smi::AllSmi;
    ///
    /// let smi = AllSmi::new()?;
    /// for proc in smi.get_process_info() {
    ///     println!("PID {}: {} using {} MB GPU memory",
    ///         proc.pid,
    ///         proc.process_name,
    ///         proc.used_memory / 1024 / 1024);
    /// }
    /// # Ok::<(), all_smi::Error>(())
    /// ```
    pub fn get_process_info(&self) -> Vec<ProcessInfo> {
        let mut all_processes = Vec::new();
        for reader in &self.gpu_readers {
            all_processes.extend(reader.get_process_info());
        }
        all_processes
    }

    /// Get information about system CPUs.
    ///
    /// Returns a vector of [`CpuInfo`] structs containing metrics for each
    /// CPU socket or processor. This includes model name, utilization,
    /// frequency, temperature, and platform-specific details.
    ///
    /// Returns an empty vector if CPU information is not available.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use all_smi::AllSmi;
    ///
    /// let smi = AllSmi::new()?;
    /// for cpu in smi.get_cpu_info() {
    ///     println!("{}: {:.1}% utilization, {} MHz",
    ///         cpu.cpu_model,
    ///         cpu.utilization,
    ///         cpu.base_frequency_mhz);
    ///     if let Some(temp) = cpu.temperature {
    ///         println!("  Temperature: {}C", temp);
    ///     }
    /// }
    /// # Ok::<(), all_smi::Error>(())
    /// ```
    pub fn get_cpu_info(&self) -> Vec<CpuInfo> {
        let mut all_cpus = Vec::new();
        for reader in &self.cpu_readers {
            all_cpus.extend(reader.get_cpu_info());
        }
        all_cpus
    }

    /// Get information about system memory.
    ///
    /// Returns a vector of [`MemoryInfo`] structs containing memory
    /// utilization metrics including total, used, available, and swap memory.
    ///
    /// Returns an empty vector if memory information is not available.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use all_smi::AllSmi;
    ///
    /// let smi = AllSmi::new()?;
    /// for mem in smi.get_memory_info() {
    ///     let total_gb = mem.total_bytes as f64 / 1024.0 / 1024.0 / 1024.0;
    ///     let used_gb = mem.used_bytes as f64 / 1024.0 / 1024.0 / 1024.0;
    ///     println!("Memory: {:.1} GB / {:.1} GB ({:.1}% used)",
    ///         used_gb, total_gb, mem.utilization);
    /// }
    /// # Ok::<(), all_smi::Error>(())
    /// ```
    pub fn get_memory_info(&self) -> Vec<MemoryInfo> {
        let mut all_memory = Vec::new();
        for reader in &self.memory_readers {
            all_memory.extend(reader.get_memory_info());
        }
        all_memory
    }

    /// Get chassis/node-level information.
    ///
    /// Returns [`ChassisInfo`] if available, containing system-wide metrics
    /// such as total power consumption (CPU + GPU + ANE), thermal pressure,
    /// fan speeds, and PSU status.
    ///
    /// Returns `None` if chassis information is not available on this platform.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use all_smi::AllSmi;
    ///
    /// let smi = AllSmi::new()?;
    /// if let Some(chassis) = smi.get_chassis_info() {
    ///     if let Some(power) = chassis.total_power_watts {
    ///         println!("Total system power: {:.1}W", power);
    ///     }
    ///     if let Some(ref pressure) = chassis.thermal_pressure {
    ///         println!("Thermal pressure: {}", pressure);
    ///     }
    /// }
    /// # Ok::<(), all_smi::Error>(())
    /// ```
    pub fn get_chassis_info(&self) -> Option<ChassisInfo> {
        self.chassis_reader.get_chassis_info()
    }

    /// Get information about storage devices.
    ///
    /// Returns a vector of [`StorageInfo`] structs containing metrics for each
    /// detected storage device. The information includes mount point, total space,
    /// available space, and host identification.
    ///
    /// Returns an empty vector if storage information is not available.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use all_smi::AllSmi;
    ///
    /// let smi = AllSmi::new()?;
    /// for storage in smi.get_storage_info() {
    ///     let used_bytes = storage.total_bytes - storage.available_bytes;
    ///     let usage_percent = if storage.total_bytes > 0 {
    ///         (used_bytes as f64 / storage.total_bytes as f64) * 100.0
    ///     } else {
    ///         0.0
    ///     };
    ///     let total_gb = storage.total_bytes as f64 / 1024.0 / 1024.0 / 1024.0;
    ///     let available_gb = storage.available_bytes as f64 / 1024.0 / 1024.0 / 1024.0;
    ///     println!("{}: {:.1} GB / {:.1} GB ({:.1}% used)",
    ///         storage.mount_point, available_gb, total_gb, usage_percent);
    /// }
    /// # Ok::<(), all_smi::Error>(())
    /// ```
    pub fn get_storage_info(&self) -> Vec<StorageInfo> {
        self.storage_reader.get_storage_info()
    }

    /// Get the number of detected GPU readers.
    ///
    /// This returns the number of reader types, not the number of GPUs.
    /// Use `get_gpu_info().len()` to get the actual GPU count.
    pub fn gpu_reader_count(&self) -> usize {
        self.gpu_readers.len()
    }

    /// Check if any GPUs/NPUs are available.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use all_smi::AllSmi;
    ///
    /// let smi = AllSmi::new()?;
    /// if smi.has_gpus() {
    ///     println!("Found {} GPU(s)", smi.get_gpu_info().len());
    /// } else {
    ///     println!("No GPUs detected");
    /// }
    /// # Ok::<(), all_smi::Error>(())
    /// ```
    pub fn has_gpus(&self) -> bool {
        !self.gpu_readers.is_empty()
    }

    /// Check if CPU monitoring is available.
    pub fn has_cpu_monitoring(&self) -> bool {
        !self.cpu_readers.is_empty()
    }

    /// Check if memory monitoring is available.
    pub fn has_memory_monitoring(&self) -> bool {
        !self.memory_readers.is_empty()
    }

    /// Check if storage monitoring is available.
    ///
    /// This always returns `true` as storage monitoring is available on all
    /// supported platforms through the `sysinfo` crate.
    pub fn has_storage_monitoring(&self) -> bool {
        // Storage monitoring is always available via sysinfo
        true
    }
}

impl Drop for AllSmi {
    fn drop(&mut self) {
        // Cleanup platform-specific managers
        #[cfg(target_os = "macos")]
        if self._macos_initialized {
            shutdown_native_metrics_manager();
        }

        #[cfg(target_os = "linux")]
        if self._gaudi_initialized {
            shutdown_hlsmi_manager();
        }
    }
}

// SAFETY: AllSmi is safe to send and share across threads because:
// 1. All reader traits (GpuReader, CpuReader, MemoryReader, ChassisReader) require
//    Send + Sync bounds, ensuring all stored readers are thread-safe
// 2. The platform-specific managers (NativeMetricsManager on macOS, HlsmiManager on Linux)
//    are designed to be accessed from any thread
// 3. The initialization flags are only written during construction and only read during drop,
//    with no concurrent access possible due to ownership semantics
unsafe impl Send for AllSmi {}
unsafe impl Sync for AllSmi {}

/// Configuration options for [`AllSmi`].
#[derive(Debug, Clone)]
pub struct AllSmiConfig {
    /// Sample interval in milliseconds for platform managers.
    /// Default: 1000ms (1 second)
    pub sample_interval_ms: u64,
    /// Whether to print verbose warnings during initialization.
    /// Default: false
    pub verbose: bool,
}

impl Default for AllSmiConfig {
    fn default() -> Self {
        Self {
            sample_interval_ms: 1000,
            verbose: false,
        }
    }
}

impl AllSmiConfig {
    /// Create a new configuration with default values.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the sample interval in milliseconds.
    ///
    /// # Arguments
    ///
    /// * `interval_ms` - Sample interval (minimum 100ms recommended)
    pub fn sample_interval(mut self, interval_ms: u64) -> Self {
        self.sample_interval_ms = interval_ms;
        self
    }

    /// Enable verbose output during initialization.
    pub fn verbose(mut self, verbose: bool) -> Self {
        self.verbose = verbose;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_allsmi_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<AllSmi>();
    }

    #[test]
    fn test_device_type_display() {
        assert_eq!(DeviceType::NvidiaGpu.to_string(), "NVIDIA GPU");
        assert_eq!(DeviceType::AppleSiliconGpu.to_string(), "Apple Silicon GPU");
        assert_eq!(DeviceType::IntelGaudi.to_string(), "Intel Gaudi");
    }

    #[test]
    fn test_config_default() {
        let config = AllSmiConfig::default();
        assert_eq!(config.sample_interval_ms, 1000);
        assert!(!config.verbose);
    }

    #[test]
    fn test_config_builder() {
        let config = AllSmiConfig::new().sample_interval(500).verbose(true);
        assert_eq!(config.sample_interval_ms, 500);
        assert!(config.verbose);
    }

    #[test]
    fn test_allsmi_new() {
        // This test verifies that AllSmi can be created without panicking
        // It may not find any hardware in CI environments
        let result = AllSmi::new();
        assert!(result.is_ok());

        let smi = result.unwrap();
        // These should not panic even without hardware
        let _ = smi.get_gpu_info();
        let _ = smi.get_cpu_info();
        let _ = smi.get_memory_info();
        let _ = smi.get_process_info();
        let _ = smi.get_chassis_info();
        let _ = smi.get_storage_info();
    }

    #[test]
    fn test_storage_info() {
        let smi = AllSmi::new().unwrap();

        // Storage monitoring should always be available
        assert!(smi.has_storage_monitoring());

        // Get storage info and verify basic properties
        let storage_info = smi.get_storage_info();

        // Storage info should be returned (may be empty in some CI environments)
        for storage in &storage_info {
            // Mount point should not be empty
            assert!(!storage.mount_point.is_empty());

            // Available bytes should not exceed total bytes
            assert!(storage.available_bytes <= storage.total_bytes);

            // Hostname should not be empty
            assert!(!storage.hostname.is_empty());
        }
    }

    #[test]
    fn test_allsmi_with_config() {
        let config = AllSmiConfig::new().sample_interval(500);
        let result = AllSmi::with_config(config);
        assert!(result.is_ok());
    }
}
