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

// Common constants for device readers

#![allow(dead_code)]

/// Memory conversion constants
pub const BYTES_PER_KB: u64 = 1024;
pub const BYTES_PER_MB: u64 = BYTES_PER_KB * 1024;
pub const BYTES_PER_GB: u64 = BYTES_PER_MB * 1024;
pub const BYTES_PER_TB: u64 = BYTES_PER_GB * 1024;

/// Default memory sizes for specific devices
pub const FURIOSA_HBM3_MEMORY_GB: u64 = 48;
pub const FURIOSA_HBM3_MEMORY_BYTES: u64 = FURIOSA_HBM3_MEMORY_GB * BYTES_PER_GB;

/// Power conversion constants
pub const MILLIWATTS_PER_WATT: f64 = 1000.0;

/// Temperature constants
pub const ABSOLUTE_ZERO_CELSIUS: i32 = -273;
pub const MAX_SAFE_TEMPERATURE_C: u32 = 100;

/// Frequency constants
pub const MHZ_PER_GHZ: u32 = 1000;

/// Default values for missing data
pub const DEFAULT_TEMPERATURE: u32 = 0;
pub const DEFAULT_POWER: f64 = 0.0;
pub const DEFAULT_FREQUENCY: u32 = 0;
pub const DEFAULT_UTILIZATION: f64 = 0.0;
pub const DEFAULT_MEMORY: u64 = 0;

/// Device-specific constants
pub mod furiosa {
    pub const CORE_COUNT: u32 = 8;
    pub const PE_COUNT_STR: &str = "64K";
    pub const MEMORY_BANDWIDTH: &str = "1.63TB/s";
    pub const ON_CHIP_SRAM: &str = "256MB";
}

/// Google TPU-specific constants
pub mod google_tpu {
    /// TPU v2 HBM memory size in bytes (8 GB)
    pub const TPU_V2_HBM_BYTES: u64 = 8 * 1024 * 1024 * 1024;
    /// TPU v3 HBM memory size in bytes (16 GB)
    pub const TPU_V3_HBM_BYTES: u64 = 16 * 1024 * 1024 * 1024;
    /// TPU v4 HBM memory size in bytes (32 GB)
    pub const TPU_V4_HBM_BYTES: u64 = 32 * 1024 * 1024 * 1024;
    /// TPU v5e HBM memory size in bytes (16 GB)
    pub const TPU_V5E_HBM_BYTES: u64 = 16 * 1024 * 1024 * 1024;
    /// TPU v5p HBM memory size in bytes (95 GB)
    pub const TPU_V5P_HBM_BYTES: u64 = 95 * 1024 * 1024 * 1024;
    /// TPU v6e HBM memory size in bytes (16 GB, cost-optimized)
    pub const TPU_V6E_HBM_BYTES: u64 = 16 * 1024 * 1024 * 1024;
    /// TPU v6 Trillium HBM memory size in bytes (32 GB)
    pub const TPU_V6_TRILLIUM_HBM_BYTES: u64 = 32 * 1024 * 1024 * 1024;
    /// TPU v7 Ironwood HBM3e memory size in bytes (192 GB)
    pub const TPU_V7_IRONWOOD_HBM_BYTES: u64 = 192 * 1024 * 1024 * 1024;

    /// Google vendor ID for PCI devices
    pub const GOOGLE_VENDOR_ID: &str = "0x1ae0";
    /// Google vendor ID without 0x prefix (for lspci -n output)
    pub const GOOGLE_VENDOR_ID_SHORT: &str = "1ae0";

    /// Common system-wide libtpu library paths (static)
    pub const LIBTPU_PATHS: &[&str] = &[
        "/usr/local/lib/libtpu.so",
        "/usr/lib/libtpu.so",
        "/opt/google/libtpu/libtpu.so",
    ];

    /// Search for libtpu.so in Python site-packages directories
    /// Returns all found paths including system paths and user Python environments
    #[cfg(target_os = "linux")]
    pub fn find_libtpu_paths() -> Vec<std::path::PathBuf> {
        use std::path::PathBuf;

        let mut paths = Vec::new();

        // Add static system paths first
        for path in LIBTPU_PATHS {
            let p = PathBuf::from(path);
            if p.exists() {
                paths.push(p);
            }
        }

        // Search in user's home directory Python environments
        // $HOME/.local/lib/python*/site-packages/libtpu/libtpu.so
        if let Ok(home) = std::env::var("HOME") {
            let local_lib = PathBuf::from(&home).join(".local/lib");
            if local_lib.exists()
                && let Ok(entries) = std::fs::read_dir(&local_lib)
            {
                for entry in entries.flatten() {
                    let name = entry.file_name();
                    let name_str = name.to_string_lossy();
                    if name_str.starts_with("python") {
                        let libtpu_path = entry.path().join("site-packages/libtpu/libtpu.so");
                        if libtpu_path.exists() {
                            paths.push(libtpu_path);
                        }
                    }
                }
            }
        }

        // Search in virtual environments (common venv paths)
        // Check VIRTUAL_ENV environment variable
        if let Ok(venv) = std::env::var("VIRTUAL_ENV") {
            let venv_libtpu = PathBuf::from(&venv).join("lib");
            if venv_libtpu.exists()
                && let Ok(entries) = std::fs::read_dir(&venv_libtpu)
            {
                for entry in entries.flatten() {
                    let name = entry.file_name();
                    let name_str = name.to_string_lossy();
                    if name_str.starts_with("python") {
                        let libtpu_path = entry.path().join("site-packages/libtpu/libtpu.so");
                        if libtpu_path.exists() {
                            paths.push(libtpu_path);
                        }
                    }
                }
            }
        }

        // Search in conda environments
        // $HOME/anaconda3/envs/*/lib/python*/site-packages/libtpu/libtpu.so
        // $HOME/miniconda3/envs/*/lib/python*/site-packages/libtpu/libtpu.so
        if let Ok(home) = std::env::var("HOME") {
            for conda_dir in ["anaconda3", "miniconda3", "mambaforge", "miniforge3"] {
                let envs_path = PathBuf::from(&home).join(conda_dir).join("envs");
                if envs_path.exists()
                    && let Ok(env_entries) = std::fs::read_dir(&envs_path)
                {
                    for env_entry in env_entries.flatten() {
                        let lib_path = env_entry.path().join("lib");
                        if lib_path.exists()
                            && let Ok(lib_entries) = std::fs::read_dir(&lib_path)
                        {
                            for lib_entry in lib_entries.flatten() {
                                let name = lib_entry.file_name();
                                let name_str = name.to_string_lossy();
                                if name_str.starts_with("python") {
                                    let libtpu_path =
                                        lib_entry.path().join("site-packages/libtpu/libtpu.so");
                                    if libtpu_path.exists() {
                                        paths.push(libtpu_path);
                                    }
                                }
                            }
                        }
                    }
                }

                // Also check base conda environment
                let base_lib = PathBuf::from(&home).join(conda_dir).join("lib");
                if base_lib.exists()
                    && let Ok(entries) = std::fs::read_dir(&base_lib)
                {
                    for entry in entries.flatten() {
                        let name = entry.file_name();
                        let name_str = name.to_string_lossy();
                        if name_str.starts_with("python") {
                            let libtpu_path = entry.path().join("site-packages/libtpu/libtpu.so");
                            if libtpu_path.exists() {
                                paths.push(libtpu_path);
                            }
                        }
                    }
                }
            }
        }

        // Search in system Python site-packages
        // /usr/lib/python*/site-packages/libtpu/libtpu.so
        // /usr/local/lib/python*/site-packages/libtpu/libtpu.so
        for base in ["/usr/lib", "/usr/local/lib"] {
            let base_path = PathBuf::from(base);
            if base_path.exists()
                && let Ok(entries) = std::fs::read_dir(&base_path)
            {
                for entry in entries.flatten() {
                    let name = entry.file_name();
                    let name_str = name.to_string_lossy();
                    if name_str.starts_with("python") {
                        let libtpu_path = entry.path().join("site-packages/libtpu/libtpu.so");
                        if libtpu_path.exists() {
                            paths.push(libtpu_path);
                        }
                    }
                }
            }
        }

        paths
    }

    /// Check if libtpu is available in any known location
    #[cfg(target_os = "linux")]
    pub fn is_libtpu_available() -> bool {
        // Quick check of static paths first
        for path in LIBTPU_PATHS {
            if std::path::Path::new(path).exists() {
                return true;
            }
        }
        // Then search Python environments
        !find_libtpu_paths().is_empty()
    }
}

/// Process information defaults
pub const DEFAULT_PID: u32 = 0;
pub const DEFAULT_CPU_PERCENT: f64 = 0.0;
pub const DEFAULT_PRIORITY: i32 = 0;
pub const DEFAULT_NICE_VALUE: i32 = 0;

/// String parsing constants
pub const DEVICE_ID_PREFIX_NPU: &str = "npu";
pub const DEVICE_ID_PREFIX_GPU: &str = "gpu";
pub const TEMPERATURE_SUFFIX_C: char = 'C';
pub const POWER_SUFFIX_W: char = 'W';
pub const PERCENTAGE_SUFFIX: char = '%';
pub const MEMORY_SUFFIX_MB: &str = "MB";
pub const MEMORY_SUFFIX_MIB: &str = "MiB";
pub const FREQUENCY_SUFFIX_MHZ: &str = "MHz";
