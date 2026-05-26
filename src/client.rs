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
    ChassisInfo, ChassisReader, CpuInfo, CpuReader, GpuInfo, GpuReader, MemoryInfo, MemoryReader,
    MigGpuInfo, ProcessInfo, VgpuHostInfo, create_chassis_reader, get_cpu_readers, get_gpu_readers,
    get_memory_readers,
};
use crate::error::Result;
use crate::storage::{StorageInfo, StorageReader, create_storage_reader};

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
/// # Refreshing data
///
/// The `get_*_info()` getters return point-in-time owned snapshots. Re-calling
/// a getter returns fresh values but re-enumerates every device. To refresh a
/// single device of interest without that cost, use a stable correlation
/// identifier:
///
/// * GPUs and NPUs are keyed by [`crate::device::GpuInfo::uuid`]; use
///   [`AllSmi::get_gpu_by_uuid`] or [`AllSmi::refresh_gpu`].
/// * CPUs and memory entries are keyed by their 0-based `index`, populated by
///   `get_cpu_info` / `get_memory_info`; use [`AllSmi::get_cpu_by_index`] /
///   [`AllSmi::refresh_cpu`] and [`AllSmi::get_memory_by_index`] /
///   [`AllSmi::refresh_memory`].
/// * Storage entries already expose
///   [`crate::storage::StorageInfo::index`].
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
        // `config` is read only by the platform-specific initialisation
        // blocks below (macOS native metrics, Linux Gaudi). Silence the
        // unused-variable warning on platforms (e.g. Windows) that have
        // no such init.
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        let _ = &config;

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

    /// Fetch fresh information for a single GPU/NPU identified by its stable
    /// [`GpuInfo::uuid`], without re-enumerating every device.
    ///
    /// Each GPU reader is queried via [`crate::device::GpuReader::get_gpu_info_by_uuid`]
    /// in turn. The default trait implementation filters that reader's full
    /// enumeration; readers that can address a device directly (for example
    /// NVML via `nvmlDeviceGetHandleByUUID`) MAY override it for a faster path.
    /// Returns `None` when no reader currently sees a device with that UUID
    /// (e.g., the device was removed, the driver lost it, or the UUID is
    /// unknown to every installed reader).
    ///
    /// # Refreshing a previously held snapshot
    ///
    /// Use this to refresh a single device of interest without paying the cost
    /// of re-enumerating every accelerator in the system:
    ///
    /// ```rust,no_run
    /// use all_smi::AllSmi;
    ///
    /// let smi = AllSmi::new()?;
    /// let snapshot = smi.get_gpu_info();
    /// if let Some(first) = snapshot.first() {
    ///     // Later, refresh just this one device.
    ///     if let Some(fresh) = smi.get_gpu_by_uuid(&first.uuid) {
    ///         println!("{} util now {:.1}%", fresh.name, fresh.utilization);
    ///     } else {
    ///         println!("{} has disappeared", first.name);
    ///     }
    /// }
    /// # Ok::<(), all_smi::Error>(())
    /// ```
    pub fn get_gpu_by_uuid(&self, uuid: &str) -> Option<GpuInfo> {
        for reader in &self.gpu_readers {
            if let Some(gpu) = reader.get_gpu_info_by_uuid(uuid) {
                return Some(gpu);
            }
        }
        None
    }

    /// Refresh a previously fetched [`GpuInfo`] in place by its UUID.
    ///
    /// Returns `true` when the device was found and `*info` was overwritten
    /// with fresh data, `false` when no reader currently sees a device with
    /// `info.uuid` (the original struct is left untouched in that case so the
    /// caller can decide how to handle a disappeared device).
    ///
    /// This is a convenience wrapper around [`Self::get_gpu_by_uuid`]. The
    /// existing per-entry identifier on [`GpuInfo::uuid`] is what makes the
    /// in-place refresh unambiguous even when devices hot-plug or MIG
    /// reconfiguration renumbers things between calls.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use all_smi::AllSmi;
    ///
    /// let smi = AllSmi::new()?;
    /// let mut gpus = smi.get_gpu_info();
    /// if let Some(gpu) = gpus.first_mut() {
    ///     if smi.refresh_gpu(gpu) {
    ///         println!("{} refreshed: {:.1}% util", gpu.name, gpu.utilization);
    ///     } else {
    ///         println!("{} disappeared between calls", gpu.name);
    ///     }
    /// }
    /// # Ok::<(), all_smi::Error>(())
    /// ```
    pub fn refresh_gpu(&self, info: &mut GpuInfo) -> bool {
        match self.get_gpu_by_uuid(&info.uuid) {
            Some(fresh) => {
                *info = fresh;
                true
            }
            None => false,
        }
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

    /// Get NVIDIA vGPU information for every vGPU-enabled host GPU.
    ///
    /// Returns an empty vector on bare-metal or non-NVIDIA hosts. Each
    /// [`VgpuHostInfo`] record carries the host mode, scheduler metadata, and
    /// a list of active vGPU instances. Non-NVIDIA readers return empty via
    /// the trait default.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use all_smi::AllSmi;
    ///
    /// let smi = AllSmi::new()?;
    /// for host in smi.get_vgpu_info() {
    ///     println!("{}: {} (ARR {})", host.gpu_name, host.host_mode, host.scheduler_arr_mode);
    ///     for vgpu in &host.vgpus {
    ///         println!("  {} - util {:?}%", vgpu.vgpu_type_name, vgpu.gpu_utilization);
    ///     }
    /// }
    /// # Ok::<(), all_smi::Error>(())
    /// ```
    pub fn get_vgpu_info(&self) -> Vec<VgpuHostInfo> {
        let mut all_vgpu = Vec::new();
        for reader in &self.gpu_readers {
            all_vgpu.extend(reader.get_vgpu_info());
        }
        all_vgpu
    }

    /// Get NVIDIA MIG (Multi-Instance GPU) information for every GPU that
    /// has MIG mode enabled.
    ///
    /// Returns an empty vector on consumer cards, pre-Ampere datacenter GPUs
    /// (which do not support compute MIG), and bare-metal hosts where MIG is
    /// not configured. Each [`MigGpuInfo`] record carries the parent GPU
    /// metadata, the live MIG mode flag, and a list of enumerated instances.
    /// Non-NVIDIA readers return empty via the trait default.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use all_smi::AllSmi;
    ///
    /// let smi = AllSmi::new()?;
    /// for host in smi.get_mig_info() {
    ///     println!("{}: MIG {}", host.gpu_name, if host.mig_mode { "on" } else { "off" });
    ///     for inst in &host.instances {
    ///         println!("  instance {} - {} ({:?}% util)",
    ///             inst.instance_id, inst.profile_name, inst.utilization_gpu);
    ///     }
    /// }
    /// # Ok::<(), all_smi::Error>(())
    /// ```
    pub fn get_mig_info(&self) -> Vec<MigGpuInfo> {
        let mut all_mig = Vec::new();
        for reader in &self.gpu_readers {
            all_mig.extend(reader.get_mig_info());
        }
        all_mig
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
        // Assign stable 0-based correlation indices over the flattened result.
        // CPU topology is static, so the same physical entry receives the same
        // index across successive calls — callers may use this as a stable key
        // when refreshing a previously held [`CpuInfo`].
        for (idx, cpu) in all_cpus.iter_mut().enumerate() {
            cpu.index = idx as u32;
        }
        all_cpus
    }

    /// Fetch fresh [`CpuInfo`] for a single entry by its stable
    /// [`CpuInfo::index`].
    ///
    /// Re-enumerates CPUs and returns the entry whose freshly assigned index
    /// matches `index`. CPU topology is static, so the index is a stable
    /// correlation key across refreshes. Returns `None` when `index` is out
    /// of range for the current enumeration. The efficiency win over
    /// [`Self::get_cpu_info`] is marginal in practice (typically a single
    /// aggregate CPU entry per host); this helper exists for API symmetry
    /// with [`Self::get_gpu_by_uuid`].
    pub fn get_cpu_by_index(&self, index: u32) -> Option<CpuInfo> {
        self.get_cpu_info().into_iter().find(|c| c.index == index)
    }

    /// Refresh a previously fetched [`CpuInfo`] in place using its stable
    /// [`CpuInfo::index`].
    ///
    /// Returns `true` when the entry was found and `*info` was overwritten,
    /// `false` when the index is no longer present (the original struct is
    /// left untouched). See [`Self::get_cpu_by_index`] for the underlying
    /// lookup semantics.
    pub fn refresh_cpu(&self, info: &mut CpuInfo) -> bool {
        match self.get_cpu_by_index(info.index) {
            Some(fresh) => {
                *info = fresh;
                true
            }
            None => false,
        }
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
        // Assign stable 0-based correlation indices over the flattened result
        // (see [`Self::get_cpu_info`] for the rationale).
        for (idx, mem) in all_memory.iter_mut().enumerate() {
            mem.index = idx as u32;
        }
        all_memory
    }

    /// Fetch fresh [`MemoryInfo`] for a single entry by its stable
    /// [`MemoryInfo::index`].
    ///
    /// Returns `None` when `index` is out of range for the current
    /// enumeration. Memory is effectively a per-host singleton; this helper
    /// exists for API symmetry with [`Self::get_cpu_by_index`] and
    /// [`Self::get_gpu_by_uuid`].
    pub fn get_memory_by_index(&self, index: u32) -> Option<MemoryInfo> {
        self.get_memory_info()
            .into_iter()
            .find(|m| m.index == index)
    }

    /// Refresh a previously fetched [`MemoryInfo`] in place using its stable
    /// [`MemoryInfo::index`].
    ///
    /// Returns `true` when the entry was found and `*info` was overwritten,
    /// `false` when the index is no longer present (the original struct is
    /// left untouched).
    pub fn refresh_memory(&self, info: &mut MemoryInfo) -> bool {
        match self.get_memory_by_index(info.index) {
            Some(fresh) => {
                *info = fresh;
                true
            }
            None => false,
        }
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
