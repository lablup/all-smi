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

use std::collections::HashMap;
/// Container detection and PID mapping utilities
///
/// This module provides functionality to:
/// - Detect if all-smi is running inside a container
/// - Map PIDs between host and container namespaces
/// - Handle PID namespace traversal for accurate process identification
///
/// When all-smi runs inside a container:
/// - Process PIDs are in container namespace (1, 2, 3...)
/// - NPU/GPU drivers report host PIDs (since they operate at kernel level)
/// - We need to map host PIDs back to container PIDs
///
/// When all-smi runs on host:
/// - Process PIDs are host PIDs
/// - NPU/GPU drivers report host PIDs
/// - We can optionally show container PIDs for containerized processes
use std::fs;

/// Check if all-smi is running inside a container
pub fn is_running_in_container() -> bool {
    // Method 1: Check if /.dockerenv exists
    if std::path::Path::new("/.dockerenv").exists() {
        return true;
    }

    // Method 2: Check cgroup for docker/containerd/lxc/k8s
    if let Ok(cgroup) = fs::read_to_string("/proc/self/cgroup")
        && (cgroup.contains("/docker/")
            || cgroup.contains("/containerd/")
            || cgroup.contains("/lxc/")
            || cgroup.contains("/kubepods/"))
    {
        return true;
    }

    // Method 3: Check if PID 1 is not systemd/init
    if let Ok(cmdline) = fs::read_to_string("/proc/1/cmdline") {
        let cmd = cmdline.split('\0').next().unwrap_or("");
        if !cmd.contains("systemd") && !cmd.contains("init") && !cmd.is_empty() {
            return true;
        }
    }

    // Method 4: Check for container-specific environment variables
    if std::env::var("KUBERNETES_SERVICE_HOST").is_ok() || std::env::var("DOCKER_CONTAINER").is_ok()
    {
        return true;
    }

    false
}

/// Check if a process is running in a container by examining its namespace
#[allow(dead_code)]
pub fn is_containerized_process(pid: u32) -> bool {
    // Check if the process has a different PID namespace than init (PID 1)
    if let (Ok(init_ns), Ok(proc_ns)) = (
        fs::read_link("/proc/1/ns/pid"),
        fs::read_link(format!("/proc/{pid}/ns/pid")),
    ) {
        return init_ns != proc_ns;
    }
    false
}

/// Get the container PID from host PID by reading NSpid field
pub fn get_container_pid_mapping(host_pid: u32) -> Option<u32> {
    if let Ok(status) = fs::read_to_string(format!("/proc/{host_pid}/status")) {
        for line in status.lines() {
            if line.starts_with("NSpid:") {
                let pids: Vec<&str> = line.split_whitespace().skip(1).collect();
                // NSpid shows: host_pid [container_pid] [grandparent_pid...]
                if pids.len() > 1 {
                    return pids[1].parse::<u32>().ok();
                }
            }
        }
    }
    None
}

/// Get our own PID mapping when running inside a container
/// Returns (container_pid, host_pid)
#[allow(dead_code)]
pub fn get_self_pid_mapping() -> Option<(u32, u32)> {
    // When running in a container, we can check our own NSpid
    if let Ok(status) = fs::read_to_string("/proc/self/status") {
        for line in status.lines() {
            if line.starts_with("NSpid:") {
                let pids: Vec<&str> = line.split_whitespace().skip(1).collect();

                // Debug output if enabled
                if std::env::var("ALL_SMI_DEBUG_PID").is_ok() {
                    eprintln!("Debug: Self NSpid: {line}");
                }

                // NSpid format varies:
                // - Not in container: single PID
                // - In container: container_pid host_pid [parent_ns_pids...]
                if pids.len() == 1 {
                    // Not in a container namespace
                    return None;
                } else if pids.len() >= 2 {
                    // In a container: first is our PID in current namespace,
                    // second is our PID in parent namespace (usually host)
                    let container_pid = pids[0].parse::<u32>().ok()?;
                    let host_pid = pids[1].parse::<u32>().ok()?;
                    return Some((container_pid, host_pid));
                }
            }
        }
    }
    None
}

/// Map host PID to container PID when running inside a container
/// This is the most challenging case: NPU reports host PID, we need container PID
#[allow(dead_code)]
pub fn map_host_to_container_pid(host_pid: u32) -> Option<u32> {
    // If we're not in a container, return the original PID
    if !is_running_in_container() {
        return Some(host_pid);
    }

    // Strategy 1: Direct lookup if PID exists in our namespace
    // (happens when NPU driver is namespace-aware)
    if std::path::Path::new(&format!("/proc/{host_pid}")).exists() {
        return Some(host_pid);
    }

    // Strategy 2: Check if host /proc is mounted (common in monitoring containers)
    let host_proc_paths = vec![
        "/host/proc", // Kubernetes common mount
        "/hostproc",  // Alternative mount
        "/proc_host", // Another alternative
    ];

    for host_proc in &host_proc_paths {
        let status_path = format!("{host_proc}/{host_pid}/status");
        if let Ok(status) = fs::read_to_string(&status_path) {
            // Parse NSpid to find container PID
            for line in status.lines() {
                if line.starts_with("NSpid:") {
                    let pids: Vec<&str> = line.split_whitespace().skip(1).collect();
                    // NSpid format when reading from host /proc:
                    // NSpid: <host_pid> [parent_ns_pid] [container_pid]
                    // We need to determine our namespace depth
                    if let Some((our_container_pid, our_host_pid)) = get_self_pid_mapping() {
                        // Find our position in the namespace hierarchy
                        if our_host_pid == host_pid {
                            return Some(our_container_pid);
                        }
                    }

                    // Try to match based on the number of PIDs
                    // Usually: host_pid, container_pid (when 1 level of nesting)
                    if pids.len() == 2 && pids[0].parse::<u32>().ok() == Some(host_pid) {
                        // This means: host_pid container_pid
                        if let Ok(container_pid) = pids[1].parse::<u32>() {
                            return Some(container_pid);
                        }
                    } else if pids.len() > 2 {
                        // Multiple levels of nesting, the last is usually the most nested
                        if let Some(container_pid_str) = pids.last()
                            && let Ok(container_pid) = container_pid_str.parse::<u32>()
                        {
                            return Some(container_pid);
                        }
                    }
                }
            }
        }
    }

    // Strategy 3: Scan our local /proc to find a process that maps to this host PID
    // This is expensive but works when we have partial visibility
    // First, understand our namespace structure
    let _our_namespace_info = get_self_pid_mapping();

    if let Ok(entries) = fs::read_dir("/proc") {
        for entry in entries.flatten() {
            if let Some(pid_str) = entry.file_name().to_str()
                && let Ok(pid) = pid_str.parse::<u32>()
            {
                // Check if this container PID maps to our host PID
                if let Ok(status) = fs::read_to_string(format!("/proc/{pid}/status")) {
                    for line in status.lines() {
                        if line.starts_with("NSpid:") {
                            let pids: Vec<&str> = line.split_whitespace().skip(1).collect();

                            // Debug: print the NSpid line for troubleshooting
                            if std::env::var("ALL_SMI_DEBUG_PID").is_ok() {
                                eprintln!("Debug: PID {pid} has NSpid: {line}");
                            }

                            // When inside container reading /proc/[pid]/status:
                            // NSpid format depends on our view:
                            // - Same namespace: single PID
                            // - Different namespace: container_pid host_pid [grandparent_pids...]

                            // Most common case: we're in container, format is "container_pid host_pid"
                            if pids.len() >= 2 && pids[1].parse::<u32>().ok() == Some(host_pid) {
                                return Some(pid);
                            }

                            // Less common: check if it's a direct match (same namespace)
                            if !pids.is_empty() && pids[0].parse::<u32>().ok() == Some(host_pid) {
                                // Verify we're in the same namespace
                                if let Ok(our_ns) = fs::read_link("/proc/self/ns/pid")
                                    && let Ok(proc_ns) =
                                        fs::read_link(format!("/proc/{pid}/ns/pid"))
                                    && our_ns == proc_ns
                                {
                                    return Some(pid);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    None
}

/// Find host PID from container PID by scanning all processes
/// This is needed when NPU driver reports container PIDs instead of host PIDs
#[allow(dead_code)]
pub fn find_host_pid_from_container_pid(
    container_pid: u32,
    container_init_pid: Option<u32>,
) -> Option<u32> {
    // If we know the container's init process, we can narrow the search
    if let Some(init_pid) = container_init_pid {
        // Read the container's PID namespace
        if let Ok(container_ns) = fs::read_link(format!("/proc/{init_pid}/ns/pid")) {
            // Scan /proc for processes in the same namespace
            if let Ok(entries) = fs::read_dir("/proc") {
                for entry in entries.flatten() {
                    if let Some(pid_str) = entry.file_name().to_str()
                        && let Ok(pid) = pid_str.parse::<u32>()
                    {
                        // Check if this process is in the same namespace
                        if let Ok(proc_ns) = fs::read_link(format!("/proc/{pid}/ns/pid"))
                            && proc_ns == container_ns
                        {
                            // Check if this host PID maps to our container PID
                            if get_container_pid_mapping(pid) == Some(container_pid) {
                                return Some(pid);
                            }
                        }
                    }
                }
            }
        }
    }

    // Fallback: scan all processes
    if let Ok(entries) = fs::read_dir("/proc") {
        for entry in entries.flatten() {
            if let Some(pid_str) = entry.file_name().to_str()
                && let Ok(pid) = pid_str.parse::<u32>()
                && get_container_pid_mapping(pid) == Some(container_pid)
            {
                return Some(pid);
            }
        }
    }
    None
}

/// Build a cache of PID mappings for efficient lookup
/// Returns HashMap<host_pid, container_pid>
#[allow(dead_code)]
pub fn build_pid_mapping_cache() -> HashMap<u32, u32> {
    let mut cache = HashMap::new();

    if !is_running_in_container() {
        return cache;
    }

    // Try to read from host /proc if available
    let host_proc_paths = vec!["/host/proc", "/hostproc", "/proc_host"];

    for host_proc in &host_proc_paths {
        if let Ok(entries) = fs::read_dir(host_proc) {
            for entry in entries.flatten() {
                if let Some(pid_str) = entry.file_name().to_str()
                    && let Ok(host_pid) = pid_str.parse::<u32>()
                {
                    let status_path = format!("{host_proc}/{host_pid}/status");
                    if let Ok(status) = fs::read_to_string(&status_path) {
                        for line in status.lines() {
                            if line.starts_with("NSpid:") {
                                let pids: Vec<&str> = line.split_whitespace().skip(1).collect();
                                if pids.len() >= 2 {
                                    // Typically: host_pid container_pid
                                    if let Ok(container_pid) = pids[1].parse::<u32>() {
                                        cache.insert(host_pid, container_pid);
                                    }
                                }
                            }
                        }
                    }
                }
            }
            // If we found a working host proc, don't try others
            if !cache.is_empty() {
                return cache;
            }
        }
    }

    // Fallback: scan local /proc and build reverse mapping
    if let Ok(entries) = fs::read_dir("/proc") {
        for entry in entries.flatten() {
            if let Some(pid_str) = entry.file_name().to_str()
                && let Ok(container_pid) = pid_str.parse::<u32>()
                && let Ok(status) = fs::read_to_string(format!("/proc/{container_pid}/status"))
            {
                for line in status.lines() {
                    if line.starts_with("NSpid:") {
                        let pids: Vec<&str> = line.split_whitespace().skip(1).collect();
                        if pids.len() >= 2 {
                            // Inside container: container_pid host_pid
                            if let Ok(host_pid) = pids[1].parse::<u32>() {
                                cache.insert(host_pid, container_pid);
                            }
                        }
                    }
                }
            }
        }
    }

    cache
}

/// Format process name with container indicator if applicable
#[allow(dead_code)]
pub fn format_process_name_with_container_info(process_name: String, pid: u32) -> String {
    if is_running_in_container() {
        // We're in a container, indicate this in the process name
        format!("{process_name} [container]")
    } else {
        // We're on the host, check if this is a containerized process
        if is_containerized_process(pid) {
            if let Some(container_pid) = get_container_pid_mapping(pid) {
                // Add container PID info to process name
                format!("{process_name} [c:{container_pid}]")
            } else {
                process_name
            }
        } else {
            process_name
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_container_detection() {
        // This test will pass/fail based on where it's run
        let in_container = is_running_in_container();
        println!("Running in container: {in_container}");
    }

    #[test]
    fn test_self_pid_mapping() {
        if let Some((container_pid, host_pid)) = get_self_pid_mapping() {
            println!("Self PID mapping: container={container_pid}, host={host_pid}");
            assert!(container_pid > 0);
            assert!(host_pid > 0);
        }
    }
}
