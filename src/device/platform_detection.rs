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

use std::sync::OnceLock;

#[cfg(target_os = "linux")]
use crate::device::common::constants::google_tpu::is_libtpu_available;
use crate::device::common::execute_command_default;

// Platform detection results are immutable for the lifetime of the process:
// hardware doesn't appear or disappear at runtime. Cache each detection call
// in a process-global OnceLock so expensive probes (e.g. `system_profiler
// SPPCIDataType` on macOS, `lspci` on Linux, `nvidia-smi -L` everywhere) run
// at most once, instead of being re-executed on every view refresh cycle.

pub fn has_nvidia() -> bool {
    static CACHE: OnceLock<bool> = OnceLock::new();
    *CACHE.get_or_init(detect_nvidia)
}

fn detect_nvidia() -> bool {
    // On macOS, use system_profiler to check for NVIDIA devices
    if std::env::consts::OS == "macos" {
        // First check system_profiler for NVIDIA PCI devices
        if let Ok(output) = execute_command_default("system_profiler", &["SPPCIDataType"])
            && output.status == 0
        {
            // Look for NVIDIA in the output - could be in Type field or device name
            if output.stdout.contains("NVIDIA") {
                return true;
            }
        }

        // Fallback to nvidia-smi check
        if let Ok(output) = execute_command_default("nvidia-smi", &["-L"])
            && output.status == 0
        {
            // nvidia-smi -L outputs lines like "GPU 0: NVIDIA GeForce..."
            return output
                .stdout
                .lines()
                .any(|line| line.trim().starts_with("GPU"));
        }
        return false;
    }

    // On Windows, check if nvidia-smi is available and can list GPUs
    if std::env::consts::OS == "windows" {
        // Try nvidia-smi first (most reliable on Windows)
        if let Ok(output) = execute_command_default("nvidia-smi", &["-L"])
            && output.status == 0
        {
            // nvidia-smi -L outputs lines like "GPU 0: NVIDIA GeForce..."
            let has_gpu = output.stdout.lines().any(|line| {
                let trimmed = line.trim();
                trimmed.starts_with("GPU") && trimmed.contains(":")
            });
            if has_gpu {
                return true;
            }
        }

        // Try NVML directly via the nvml-wrapper crate (will be attempted in reader)
        // If nvidia-smi fails, we can still try NVML initialization
        return false;
    }

    // On Linux, first try lspci to check for NVIDIA VGA/3D controllers
    if let Ok(output) = execute_command_default("lspci", &[])
        && output.status == 0
    {
        // Look for NVIDIA VGA or 3D controllers
        for line in output.stdout.lines() {
            if (line.contains("VGA") || line.contains("3D")) && line.contains("NVIDIA") {
                return true;
            }
        }
    }

    // Fallback: Check if nvidia-smi can actually list GPUs
    if let Ok(output) = execute_command_default("nvidia-smi", &["-L"]) {
        // Check both exit status and output content
        if output.status == 0 {
            // nvidia-smi -L outputs lines like "GPU 0: NVIDIA GeForce..."
            // Make sure we have actual GPU lines, not just an empty output
            let has_gpu = output.stdout.lines().any(|line| {
                let trimmed = line.trim();
                trimmed.starts_with("GPU") && trimmed.contains(":")
            });
            if has_gpu {
                return true;
            }
        }

        // Also check stderr for "No devices were found" message
        if output.stderr.contains("No devices were found")
            || output.stderr.contains("Failed to initialize NVML")
        {
            return false;
        }
    }
    false
}

#[cfg(all(target_os = "linux", not(target_env = "musl")))]
pub fn has_amd() -> bool {
    static CACHE: OnceLock<bool> = OnceLock::new();
    *CACHE.get_or_init(detect_amd)
}

#[cfg(all(target_os = "linux", not(target_env = "musl")))]
fn detect_amd() -> bool {
    // On Linux, check for AMD GPUs
    if std::env::consts::OS == "linux" {
        // Check lspci for AMD devices (Vendor ID 1002)
        if let Ok(output) = execute_command_default("lspci", &["-n"])
            && output.status == 0
        {
            for line in output.stdout.lines() {
                if line.contains(":1002:") {
                    return true;
                }
            }
        }

        // Fallback: check /sys/class/drm
        if let Ok(entries) = std::fs::read_dir("/sys/class/drm") {
            for entry in entries.flatten() {
                let path = entry.path().join("device/vendor");
                if let Ok(vendor) = std::fs::read_to_string(path)
                    && vendor.trim() == "0x1002"
                {
                    return true;
                }
            }
        }
    }
    false
}

pub fn is_jetson() -> bool {
    static CACHE: OnceLock<bool> = OnceLock::new();
    *CACHE.get_or_init(detect_jetson)
}

fn detect_jetson() -> bool {
    if let Ok(compatible) = std::fs::read_to_string("/proc/device-tree/compatible") {
        return compatible.contains("tegra");
    }
    false
}

pub fn is_apple_silicon() -> bool {
    static CACHE: OnceLock<bool> = OnceLock::new();
    *CACHE.get_or_init(detect_apple_silicon)
}

fn detect_apple_silicon() -> bool {
    // Only check on macOS
    if std::env::consts::OS != "macos" {
        return false;
    }

    let output =
        execute_command_default("uname", &["-m"]).expect("Failed to execute uname command");

    output.stdout.trim() == "arm64"
}

pub fn has_furiosa() -> bool {
    static CACHE: OnceLock<bool> = OnceLock::new();
    *CACHE.get_or_init(detect_furiosa)
}

fn detect_furiosa() -> bool {
    // Check if devices are visible under the /sys/class/rngd_mgmt directory
    let rngd_mgmt_path = std::path::Path::new("/sys/class/rngd_mgmt");
    if !rngd_mgmt_path.exists() {
        return false;
    }

    // Check if /sys/class/rngd_mgmt/rngd!npu0mgmt exists
    let npu0_mgmt_path = rngd_mgmt_path.join("rngd!npu0mgmt");
    if !npu0_mgmt_path.exists() {
        return false;
    }

    // Check if the content of platform_type is FuriosaAI
    let platform_type_path = npu0_mgmt_path.join("platform_type");
    if let Ok(platform_type) = std::fs::read_to_string(platform_type_path)
        && platform_type.trim() == "FuriosaAI"
    {
        return true;
    }

    false
}

#[cfg(target_os = "linux")]
pub fn has_tenstorrent() -> bool {
    static CACHE: OnceLock<bool> = OnceLock::new();
    *CACHE.get_or_init(detect_tenstorrent)
}

#[cfg(target_os = "linux")]
fn detect_tenstorrent() -> bool {
    // First check if device directory exists
    if std::path::Path::new("/dev/tenstorrent").exists() {
        return true;
    }

    // On macOS, use system_profiler
    if std::env::consts::OS == "macos" {
        if let Ok(output) = execute_command_default("system_profiler", &["SPPCIDataType"])
            && output.status == 0
            && output.stdout.contains("Tenstorrent")
        {
            return true;
        }
    } else {
        // On Linux, try lspci to check for Tenstorrent devices
        if let Ok(output) = execute_command_default("lspci", &[])
            && output.status == 0
        {
            // Look for Tenstorrent devices
            if output.stdout.contains("Tenstorrent") {
                return true;
            }
        }
    }

    // Last resort: check if tt-smi can actually list devices
    if let Ok(output) = execute_command_default("tt-smi", &["-s", "--snapshot_no_tty"])
        && output.status == 0
    {
        // Check if output contains device_info
        return output.stdout.contains("device_info");
    }

    false
}

pub fn has_rebellions() -> bool {
    static CACHE: OnceLock<bool> = OnceLock::new();
    *CACHE.get_or_init(detect_rebellions)
}

fn detect_rebellions() -> bool {
    // First check if device files exist (rbln0, rbln1, etc.)
    if std::path::Path::new("/dev/rbln0").exists() {
        return true;
    }

    // On macOS, use system_profiler
    if std::env::consts::OS == "macos" {
        if let Ok(output) = execute_command_default("system_profiler", &["SPPCIDataType"])
            && output.status == 0
            && (output.stdout.contains("Rebellions") || output.stdout.contains("RBLN"))
        {
            return true;
        }
    } else {
        // On Linux, try lspci to check for Rebellions devices
        if let Ok(output) = execute_command_default("lspci", &[])
            && output.status == 0
        {
            // Look for Rebellions devices - vendor ID 1f3f
            if output.stdout.contains("1f3f:") || output.stdout.contains("Rebellions") {
                return true;
            }
        }
    }

    // Last resort: check if rbln-stat or rbln-smi can actually list devices
    for cmd in &[
        "rbln-stat",
        "/usr/local/bin/rbln-stat",
        "/usr/bin/rbln-stat",
        "rbln-smi",
        "/usr/local/bin/rbln-smi",
        "/usr/bin/rbln-smi",
    ] {
        if let Ok(output) = execute_command_default(cmd, &["-j"])
            && output.status == 0
        {
            // Check if output contains device information
            if output.stdout.contains("\"devices\"") && output.stdout.contains("\"uuid\"") {
                return true;
            }
        }
    }

    false
}

/// Check if Google TPU devices are present
/// Uses only file system and environment variable checks to avoid process spawning.
/// IMPORTANT: No external commands are executed to prevent process accumulation.
#[cfg(target_os = "linux")]
pub fn has_google_tpu() -> bool {
    static CACHE: OnceLock<bool> = OnceLock::new();
    *CACHE.get_or_init(detect_google_tpu)
}

#[cfg(target_os = "linux")]
fn detect_google_tpu() -> bool {
    // Method 1: Check if /dev/accel* devices exist with Google vendor ID
    // This works for on-premise TPU nodes and some TPU versions
    if let Ok(entries) = std::fs::read_dir("/dev") {
        for entry in entries.flatten() {
            if let Some(name) = entry.file_name().to_str()
                && name.starts_with("accel")
            {
                // Check sysfs for Google vendor ID (0x1ae0)
                let sysfs_path = format!("/sys/class/accel/{name}/device/vendor");
                if let Ok(vendor) = std::fs::read_to_string(&sysfs_path)
                    && vendor.trim() == "0x1ae0"
                {
                    return true;
                }
            }
        }
    }

    // Method 2: Check for TPU VM environment variables
    // TPU VMs (like v6e) set these environment variables
    if std::env::var("TPU_NAME").is_ok()
        || std::env::var("TPU_CHIPS_PER_HOST_BOUNDS").is_ok()
        || std::env::var("CLOUD_TPU_TASK_ID").is_ok()
        || std::env::var("TPU_ACCELERATOR_TYPE").is_ok()
        || std::env::var("TPU_WORKER_ID").is_ok()
        || std::env::var("TPU_WORKER_HOSTNAMES").is_ok()
    {
        return true;
    }

    // Method 3: Check libtpu availability combined with TPU indicators
    if is_libtpu_available() {
        // Check for PJRT TPU plugin indicators
        if let Ok(pjrt_names) = std::env::var("PJRT_DEVICE")
            && pjrt_names.to_lowercase().contains("tpu")
        {
            return true;
        }

        // If on GCE (Google Compute Engine), libtpu likely means TPU
        if let Ok(product) = std::fs::read_to_string("/sys/class/dmi/id/product_name")
            && product.to_lowercase().contains("google")
        {
            return true;
        }
    }

    false
}

pub fn has_gaudi() -> bool {
    static CACHE: OnceLock<bool> = OnceLock::new();
    *CACHE.get_or_init(detect_gaudi)
}

fn detect_gaudi() -> bool {
    // First check if device files exist (typical Gaudi device paths)
    // Intel Gaudi uses /dev/accel/accel* device files
    if std::path::Path::new("/dev/accel/accel0").exists() {
        // Make sure it's not a Google TPU by checking vendor ID
        let sysfs_path = "/sys/class/accel/accel0/device/vendor";
        if let Ok(vendor) = std::fs::read_to_string(sysfs_path) {
            // Google vendor ID is 0x1ae0, Habana is 0x1da3
            if vendor.trim() == "0x1ae0" {
                // This is a Google TPU, not Gaudi
                // Fall through to check for hl-smi
            } else {
                return true;
            }
        } else {
            return true;
        }
    }

    // Also check /dev/hl* device files (older naming convention)
    if std::path::Path::new("/dev/hl0").exists() {
        return true;
    }

    // Check for hl-smi command availability
    const PATHS: &[&str] = &[
        "/usr/bin/hl-smi",
        "/usr/local/bin/hl-smi",
        "/opt/habanalabs/bin/hl-smi",
    ];

    for path in PATHS {
        if std::path::Path::new(path).exists() {
            return true;
        }
    }

    // On Linux, try lspci to check for Habana devices
    if std::env::consts::OS == "linux" {
        // Check with numeric vendor ID format (lspci -n)
        // Habana Labs vendor ID: 1da3
        if let Ok(output) = execute_command_default("lspci", &["-n"])
            && output.status == 0
        {
            // Look for Habana Labs vendor ID (1da3)
            for line in output.stdout.lines() {
                if line.contains("1da3:") {
                    return true;
                }
            }
        }

        // Also check regular lspci output for text matches
        if let Ok(output) = execute_command_default("lspci", &[])
            && output.status == 0
        {
            // Look for Habana Labs / Intel Gaudi devices
            // May show as "Processing accelerators" with Habana in the name
            let stdout_lower = output.stdout.to_lowercase();
            if stdout_lower.contains("habana") || stdout_lower.contains("gaudi") {
                return true;
            }
        }
    }

    // Last resort: check if hl-smi can actually list devices
    if let Ok(output) = execute_command_default("hl-smi", &["-L"])
        && output.status == 0
    {
        // Check if output contains device listing
        return !output.stdout.is_empty();
    }

    false
}

pub fn get_os_type() -> &'static str {
    std::env::consts::OS
}

#[allow(dead_code)]
pub fn is_running_in_container() -> bool {
    // Only check on Linux, as containers are Linux-specific
    if std::env::consts::OS != "linux" {
        return false;
    }

    // Check for Docker
    if std::path::Path::new("/.dockerenv").exists() {
        return true;
    }

    // Check for Kubernetes
    if std::env::var("KUBERNETES_SERVICE_HOST").is_ok() {
        return true;
    }

    // Check /proc/self/cgroup for container runtimes
    if let Ok(cgroup_content) = std::fs::read_to_string("/proc/self/cgroup") {
        let container_patterns = [
            "docker",
            "containerd",
            "crio",
            "podman",
            "garden",
            "lxc",
            "systemd-nspawn",
        ];

        for pattern in &container_patterns {
            if cgroup_content.contains(pattern) {
                return true;
            }
        }
    }

    // Check /proc/1/sched for container hints
    if let Ok(sched_content) = std::fs::read_to_string("/proc/1/sched")
        && sched_content.lines().next().is_some_and(|line| {
            line.contains("bash") || line.contains("sh") || line.contains("init")
        })
    {
        // If PID 1 is a shell or init process that's not systemd/upstart, likely in container
        if !sched_content.contains("systemd") && !sched_content.contains("upstart") {
            return true;
        }
    }

    false
}

#[allow(dead_code)]
pub fn get_container_pid_namespace() -> Option<u32> {
    // Get the PID namespace ID for the current process
    if let Ok(ns_link) = std::fs::read_link("/proc/self/ns/pid") {
        // Convert PathBuf to String
        if let Some(ns_str) = ns_link.to_str() {
            // Extract namespace ID from the link (format: "pid:[4026531836]")
            if let Some(start) = ns_str.find('[')
                && let Some(end) = ns_str.find(']')
            {
                let ns_id_str = &ns_str[start + 1..end];
                // Parse as u64 first, then convert to u32 if within range
                if let Ok(ns_id_u64) = ns_id_str.parse::<u64>() {
                    // Namespace IDs can be larger than u32::MAX
                    // For comparison purposes, we'll use the lower 32 bits
                    let ns_id = ns_id_u64 as u32;
                    return Some(ns_id);
                }
            }
        }
    }
    None
}

/// Aggregated hardware-detection snapshot.
///
/// Extracted from the individual detector functions so `all-smi doctor`
/// (issue #188) and `reader_factory` share a single call-site for
/// "what hardware is present on this host?". Calling
/// [`introspection::snapshot`] once at startup is cheaper than calling
/// each detector individually because every detector is already cached in
/// a `OnceLock` — the snapshot is just a struct wrapper with read-only
/// accessors.
pub mod introspection {
    /// Summary of hardware detected on this host. Every field is the
    /// result of the corresponding `has_*` / `is_*` detector in the
    /// parent module. Fields are `bool`s so consumers can match on
    /// structural shape rather than re-running detection.
    #[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
    pub struct PlatformSnapshot {
        pub os: &'static str,
        pub nvidia: bool,
        pub jetson: bool,
        /// `true` on glibc Linux targets when AMD is detected; always
        /// `false` on musl builds because the `libamdgpu_top` dep is
        /// compiled out.
        pub amd: bool,
        pub apple_silicon: bool,
        pub gaudi: bool,
        pub google_tpu: bool,
        pub tenstorrent: bool,
        pub rebellions: bool,
        pub furiosa: bool,
    }

    /// Produce a fresh [`PlatformSnapshot`] from the cached detectors.
    pub fn snapshot() -> PlatformSnapshot {
        PlatformSnapshot {
            os: super::get_os_type(),
            nvidia: super::has_nvidia(),
            jetson: super::is_jetson(),
            amd: detect_amd(),
            apple_silicon: super::is_apple_silicon(),
            gaudi: super::has_gaudi(),
            google_tpu: detect_google_tpu(),
            tenstorrent: detect_tenstorrent(),
            rebellions: super::has_rebellions(),
            furiosa: super::has_furiosa(),
        }
    }

    #[cfg(all(target_os = "linux", not(target_env = "musl")))]
    fn detect_amd() -> bool {
        super::has_amd()
    }

    #[cfg(not(all(target_os = "linux", not(target_env = "musl"))))]
    fn detect_amd() -> bool {
        false
    }

    #[cfg(target_os = "linux")]
    fn detect_google_tpu() -> bool {
        super::has_google_tpu()
    }

    #[cfg(not(target_os = "linux"))]
    fn detect_google_tpu() -> bool {
        false
    }

    #[cfg(target_os = "linux")]
    fn detect_tenstorrent() -> bool {
        super::has_tenstorrent()
    }

    #[cfg(not(target_os = "linux"))]
    fn detect_tenstorrent() -> bool {
        false
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn snapshot_os_matches_consts() {
            let snap = snapshot();
            assert_eq!(snap.os, std::env::consts::OS);
        }

        #[test]
        fn snapshot_is_default_friendly() {
            // Ensure `PlatformSnapshot` is constructible via `Default::default()`
            // for mock/test scenarios.
            let empty = PlatformSnapshot::default();
            assert_eq!(empty.os, "");
            assert!(!empty.nvidia);
        }
    }
}
