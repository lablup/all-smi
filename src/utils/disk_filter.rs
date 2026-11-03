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
use sysinfo::{Disk, Disks};

pub struct DiskFilter {
    excluded_prefixes: HashSet<&'static str>,
    excluded_exact: HashSet<&'static str>,
    docker_file_mounts: HashSet<&'static str>,
}

impl DiskFilter {
    pub fn new() -> Self {
        let mut excluded_prefixes = HashSet::new();
        let mut excluded_exact = HashSet::new();

        // Platform-specific system directories
        Self::add_macos_exclusions(&mut excluded_prefixes, &mut excluded_exact);
        Self::add_linux_exclusions(&mut excluded_prefixes, &mut excluded_exact);
        Self::add_common_exclusions(&mut excluded_prefixes, &mut excluded_exact);
        Self::add_backendai_exclusions(&mut excluded_prefixes, &mut excluded_exact);

        // Docker-specific file mounts to exclude
        let mut docker_file_mounts = HashSet::new();
        Self::add_docker_exclusions(&mut docker_file_mounts);

        Self {
            excluded_prefixes,
            excluded_exact,
            docker_file_mounts,
        }
    }

    pub fn should_include(&self, mount_point: &str) -> bool {
        // Check exact matches first (faster)
        if self.excluded_exact.contains(mount_point) {
            return false;
        }

        // Check Docker file mounts
        if self.docker_file_mounts.contains(mount_point) {
            return false;
        }

        // Check prefix matches
        for prefix in &self.excluded_prefixes {
            if mount_point.starts_with(prefix) {
                return false;
            }
        }

        true
    }

    fn add_macos_exclusions(
        prefixes: &mut HashSet<&'static str>,
        exact: &mut HashSet<&'static str>,
    ) {
        // macOS system volumes
        prefixes.insert("/System/Volumes/");
        prefixes.insert("/Library/");
        prefixes.insert("/Applications/");
        prefixes.insert("/System/");
        prefixes.insert("/private/");
        // Exclude specific system volumes, but allow external drives
        prefixes.insert("/Volumes/VM/");
        exact.insert("/Volumes"); // Empty volumes directory
        prefixes.insert("/Network/");

        // Docker paths on macOS
        prefixes.insert("/var/lib/docker/");
        exact.insert("/var/lib/docker");

        exact.insert("/Users/Shared");
        exact.insert("/cores");
    }

    fn add_linux_exclusions(
        prefixes: &mut HashSet<&'static str>,
        exact: &mut HashSet<&'static str>,
    ) {
        // Linux system directories
        prefixes.insert("/dev/");
        prefixes.insert("/proc/");
        prefixes.insert("/sys/");
        prefixes.insert("/run/");
        prefixes.insert("/snap/");
        prefixes.insert("/usr/");
        prefixes.insert("/var/log/");
        prefixes.insert("/var/cache/");
        prefixes.insert("/var/lib/");
        prefixes.insert("/var/tmp/");
        prefixes.insert("/var/spool/");

        // Docker-specific paths
        prefixes.insert("/var/lib/docker/");
        exact.insert("/var/lib/docker");

        exact.insert("/boot");
        exact.insert("/boot/efi");
        exact.insert("/tmp");
        exact.insert("/bin");
        exact.insert("/sbin");
        exact.insert("/etc");
        exact.insert("/lib");
        exact.insert("/lib64");
        exact.insert("/opt");
        exact.insert("/media");
        exact.insert("/mnt");
        exact.insert("/root");
        exact.insert("/srv");
    }

    fn add_backendai_exclusions(
        prefixes: &mut HashSet<&'static str>,
        _exact: &mut HashSet<&'static str>,
    ) {
        // macOS system volumes
        prefixes.insert("/opt/backend.ai/");
    }

    fn add_common_exclusions(
        prefixes: &mut HashSet<&'static str>,
        exact: &mut HashSet<&'static str>,
    ) {
        // Common runtime and temporary directories
        prefixes.insert("/tmp/");
        prefixes.insert("/var/tmp/");

        // Docker-specific paths (common across platforms)
        prefixes.insert("/var/lib/docker/");
        exact.insert("/var/lib/docker");
        prefixes.insert("/var/lib/containerd/");
        exact.insert("/var/lib/containerd");
    }

    fn add_docker_exclusions(docker_mounts: &mut HashSet<&'static str>) {
        // Common Docker file bind mounts
        docker_mounts.insert("/etc/hosts");
        docker_mounts.insert("/etc/hostname");
        docker_mounts.insert("/etc/resolv.conf");
        docker_mounts.insert("/etc/timezone");
        docker_mounts.insert("/etc/localtime");
    }
}

impl Default for DiskFilter {
    fn default() -> Self {
        Self::new()
    }
}

// Thread-safe singleton for global use
use std::sync::OnceLock;

static DISK_FILTER: OnceLock<DiskFilter> = OnceLock::new();

/// Docker-aware disk filtering that handles bind mounts
pub fn filter_docker_aware_disks(disks: &Disks) -> Vec<&Disk> {
    let filter = DISK_FILTER.get_or_init(DiskFilter::new);

    // First pass: count mounts per device
    let mut device_mount_count: HashMap<String, usize> = HashMap::new();
    let mut device_to_disks: HashMap<String, Vec<&Disk>> = HashMap::new();

    for disk in disks {
        let device_name = disk.name().to_string_lossy().to_string();
        let mount_point = disk.mount_point().to_string_lossy().to_string();

        // Skip if mount point should be excluded by basic filter
        if !filter.should_include(&mount_point) {
            continue;
        }

        *device_mount_count.entry(device_name.clone()).or_insert(0) += 1;
        device_to_disks.entry(device_name).or_default().push(disk);
    }

    // Second pass: apply Docker-aware filtering
    let mut filtered_disks = Vec::new();

    for (device_name, disks_for_device) in device_to_disks {
        let mount_count = device_mount_count.get(&device_name).unwrap_or(&0);

        if *mount_count > 5 {
            // This device has many mounts, likely Docker bind mounts
            // Only include primary mount points (directories, not files)
            for disk in disks_for_device {
                let mount_point = disk.mount_point().to_string_lossy();

                // Include only if:
                // 1. It's a directory mount (has children or is a known good path)
                // 2. It's not a file mount (doesn't have file extension or specific file pattern)
                if is_primary_mount_point(&mount_point) {
                    filtered_disks.push(disk);
                }
            }
        } else {
            // Normal device with few mounts, include all that passed basic filter
            filtered_disks.extend(disks_for_device);
        }
    }

    // Special case: include overlay filesystem (Docker container root) if it passes basic filter
    for disk in disks {
        let mount_point = disk.mount_point().to_string_lossy().to_string();
        if (disk.file_system() == "overlay" || disk.file_system() == "overlay2")
            && filter.should_include(&mount_point)
            && !filtered_disks
                .iter()
                .any(|d| d.mount_point() == disk.mount_point())
        {
            filtered_disks.push(disk);
        }
    }

    filtered_disks
}

fn is_primary_mount_point(mount_point: &str) -> bool {
    // Primary mount points to keep
    const PRIMARY_MOUNTS: &[&str] = &[
        "/",
        "/home",
        "/home/work",
        "/data",
        "/mnt",
        "/opt",
        "/var",
        "/opt/backend.ai",
    ];

    // Known Docker file mounts to exclude
    const DOCKER_FILE_PATTERNS: &[&str] = &[
        "/usr/bin/",
        "/usr/lib/",
        "/opt/kernel/",
        "/etc/backend.ai/jail/plugins/",
        ".so",
        ".json",
        ".py",
        ".sh",
        ".md",
        "docker-init",
        "nvidia-smi",
        "cuda-mps",
    ];

    // Check if it's a known primary mount
    if PRIMARY_MOUNTS.contains(&mount_point) {
        return true;
    }

    // Check if it matches any Docker file patterns
    for pattern in DOCKER_FILE_PATTERNS {
        if mount_point.contains(pattern) {
            return false;
        }
    }

    // Check if it starts with a primary mount prefix and ends with /
    for primary in PRIMARY_MOUNTS {
        if mount_point.starts_with(primary) && mount_point.ends_with('/') {
            return true;
        }
    }

    // Exclude specific file extensions
    if mount_point.ends_with(".so")
        || mount_point.ends_with(".json")
        || mount_point.ends_with(".py")
        || mount_point.ends_with(".sh")
        || mount_point.ends_with(".md")
    {
        return false;
    }

    // Include directories that end with /
    if mount_point.ends_with('/') {
        return true;
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_macos_exclusions() {
        let filter = DiskFilter::new();

        // Should exclude macOS system paths
        assert!(!filter.should_include("/System/Volumes/Data"));
        assert!(!filter.should_include("/Library/Preferences"));
        assert!(!filter.should_include("/Applications/Safari.app"));
        assert!(!filter.should_include("/Users/Shared"));
        assert!(!filter.should_include("/var/lib/docker"));
        assert!(!filter.should_include("/var/lib/docker/volumes"));

        // Should include user and data paths
        assert!(filter.should_include("/Users/john"));
        assert!(filter.should_include("/"));
        assert!(filter.should_include("/Volumes/ExternalDrive"));
    }

    #[test]
    fn test_linux_exclusions() {
        let filter = DiskFilter::new();

        // Should exclude Linux system paths
        assert!(!filter.should_include("/dev/sda1"));
        assert!(!filter.should_include("/proc/meminfo"));
        assert!(!filter.should_include("/sys/class"));
        assert!(!filter.should_include("/run/user/1000"));
        assert!(!filter.should_include("/usr/bin"));
        assert!(!filter.should_include("/var/log/syslog"));
        assert!(!filter.should_include("/var/lib/docker"));
        assert!(!filter.should_include("/var/lib/docker/volumes"));
        assert!(!filter.should_include("/boot/efi"));

        // Should include user and data paths
        assert!(filter.should_include("/home/user"));
        assert!(filter.should_include("/"));
        assert!(filter.should_include("/data"));
    }

    #[test]
    fn test_docker_exclusions() {
        let filter = DiskFilter::new();

        // Should exclude Docker file mounts
        assert!(!filter.should_include("/etc/hosts"));
        assert!(!filter.should_include("/etc/hostname"));
        assert!(!filter.should_include("/etc/resolv.conf"));
        assert!(!filter.should_include("/etc/timezone"));
        assert!(!filter.should_include("/etc/localtime"));
    }

    #[test]
    fn test_is_primary_mount_point() {
        // Should identify primary mount points
        assert!(is_primary_mount_point("/"));
        assert!(is_primary_mount_point("/home"));
        assert!(is_primary_mount_point("/home/work"));
        assert!(is_primary_mount_point("/opt/backend.ai"));
        assert!(is_primary_mount_point("/data"));

        // Should exclude file mounts
        assert!(!is_primary_mount_point("/usr/bin/nvidia-smi"));
        assert!(!is_primary_mount_point(
            "/opt/kernel/libcudahook.ubuntu18.04.x86_64.so"
        ));
        assert!(!is_primary_mount_point(
            "/usr/lib/x86_64-linux-gnu/libnvidia-ml.so.575.51.03"
        ));
        assert!(!is_primary_mount_point(
            "/etc/backend.ai/jail/plugins/libcuda_jail.so"
        ));

        // Should include certain directories under /usr, /opt, /etc
        assert!(is_primary_mount_point("/usr/local/"));
        assert!(is_primary_mount_point("/opt/apps/"));
        assert!(!is_primary_mount_point("/etc/hosts"));
    }

    #[test]
    fn test_performance() {
        let filter = DiskFilter::new();
        let mount_points = [
            "/",
            "/home",
            "/data",
            "/System/Volumes/Data",
            "/usr/bin",
            "/var/log/messages",
            "/Users/john",
            "/Applications/Safari.app",
        ];

        // Test that all lookups are fast
        for mount_point in &mount_points {
            filter.should_include(mount_point);
        }
    }
}
