//! Constants used throughout the mock server

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

// General configuration constants
pub const DEFAULT_NVIDIA_GPU_NAME: &str = "NVIDIA B200 192GB HBM3";
pub const DEFAULT_NVIDIA_DRIVER_VERSION: &str = "580.82.07";
pub const DEFAULT_CUDA_VERSION: &str = "13.0";
pub const DEFAULT_AMD_GPU_NAME: &str = "AMD Instinct MI355X 288GB HBM3";
#[allow(dead_code)]
pub const DEFAULT_AMD_INSTINCT_NAME: &str = "AMD Instinct MI300X 192GB";
pub const DEFAULT_AMD_DRIVER_VERSION: &str = "30.10.1";
pub const DEFAULT_AMD_ROCM_VERSION: &str = "7.0.2";
pub const DEFAULT_TENSTORRENT_NAME: &str = "Tenstorrent Grayskull e75 120W";
pub const DEFAULT_FURIOSA_NAME: &str = "Furiosa RNGD";
/// Default device name for the Intel client GPU mock (issue #244).
/// We anchor on the Arc B580 (Battlemage, 12GB) — current-generation
/// discrete Arc, around 190W TDP. Override via `--gpu-name`.
pub const DEFAULT_INTEL_GPU_NAME: &str = "Intel Arc B580 12GB";
/// Representative Intel Graphics Driver version string. The "real"
/// equivalent on Linux is the kernel/mesa stack, which doesn't
/// translate to a single version number; the Windows-style version is
/// what consumers see in the WMI reader's `detail["Driver Version"]`.
pub const DEFAULT_INTEL_DRIVER_VERSION: &str = "32.0.101.6299";
pub const NUM_GPUS: usize = 8;
pub const UPDATE_INTERVAL_SECS: u64 = 3;
pub const MAX_CONNECTIONS_PER_SERVER: usize = 10;

// Disk size options in bytes
pub const DISK_SIZE_1TB: u64 = 1024 * 1024 * 1024 * 1024;
pub const DISK_SIZE_4TB: u64 = 4 * 1024 * 1024 * 1024 * 1024;
pub const DISK_SIZE_12TB: u64 = 12 * 1024 * 1024 * 1024 * 1024;

// CPU placeholders
#[allow(dead_code)]
pub const PLACEHOLDER_CPU_UTIL: &str = "{{CPU_UTIL}}";
#[allow(dead_code)]
pub const PLACEHOLDER_CPU_SOCKET0_UTIL: &str = "{{CPU_SOCKET0_UTIL}}";
#[allow(dead_code)]
pub const PLACEHOLDER_CPU_SOCKET1_UTIL: &str = "{{CPU_SOCKET1_UTIL}}";
#[allow(dead_code)]
pub const PLACEHOLDER_CPU_P_CORE_UTIL: &str = "{{CPU_P_CORE_UTIL}}";
#[allow(dead_code)]
pub const PLACEHOLDER_CPU_E_CORE_UTIL: &str = "{{CPU_E_CORE_UTIL}}";
#[allow(dead_code)]
pub const PLACEHOLDER_CPU_TEMP: &str = "{{CPU_TEMP}}";
#[allow(dead_code)]
pub const PLACEHOLDER_CPU_POWER: &str = "{{CPU_POWER}}";

// System memory placeholders
#[allow(dead_code)]
pub const PLACEHOLDER_SYS_MEMORY_USED: &str = "{{SYS_MEMORY_USED}}";
#[allow(dead_code)]
pub const PLACEHOLDER_SYS_MEMORY_AVAILABLE: &str = "{{SYS_MEMORY_AVAILABLE}}";
#[allow(dead_code)]
pub const PLACEHOLDER_SYS_MEMORY_FREE: &str = "{{SYS_MEMORY_FREE}}";
#[allow(dead_code)]
pub const PLACEHOLDER_SYS_MEMORY_UTIL: &str = "{{SYS_MEMORY_UTIL}}";
#[allow(dead_code)]
pub const PLACEHOLDER_SYS_SWAP_USED: &str = "{{SYS_SWAP_USED}}";
#[allow(dead_code)]
pub const PLACEHOLDER_SYS_SWAP_FREE: &str = "{{SYS_SWAP_FREE}}";
#[allow(dead_code)]
pub const PLACEHOLDER_SYS_MEMORY_BUFFERS: &str = "{{SYS_MEMORY_BUFFERS}}";
#[allow(dead_code)]
pub const PLACEHOLDER_SYS_MEMORY_CACHED: &str = "{{SYS_MEMORY_CACHED}}";

// Disk placeholders
pub const PLACEHOLDER_DISK_AVAIL: &str = "{{DISK_AVAIL}}";
pub const PLACEHOLDER_DISK_TOTAL: &str = "{{DISK_TOTAL}}";
