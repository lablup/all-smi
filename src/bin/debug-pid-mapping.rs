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

/// Debug tool for PID namespace mapping
/// Usage: cargo run --bin debug-pid-mapping -- [host_pid]
use std::env;
use std::fs;

fn main() {
    let args: Vec<String> = env::args().collect();

    println!("=== PID Namespace Debug Tool ===");

    // Check if we're in a container
    let in_container = check_container();
    println!("Running in container: {in_container}");

    // Get our own PID mapping
    if let Some((container_pid, host_pid)) = get_self_pid_mapping() {
        println!("Self PID mapping: container={container_pid}, host={host_pid}");
    } else {
        println!("Self PID mapping: Not in a nested namespace");
    }

    // Check namespace info
    if let Ok(ns) = fs::read_link("/proc/self/ns/pid") {
        println!("Current PID namespace: {}", ns.display());
    }

    // If a specific PID was provided, try to map it
    if args.len() > 1
        && let Ok(target_pid) = args[1].parse::<u32>()
    {
        println!("\n=== Searching for PID {target_pid} ===");

        // Strategy 1: Direct check
        if std::path::Path::new(&format!("/proc/{target_pid}")).exists() {
            println!("✓ PID {target_pid} exists in current namespace");
            print_pid_info(target_pid);
        } else {
            println!("✗ PID {target_pid} not found in current namespace");
        }

        // Strategy 2: Check host proc mounts
        let host_proc_paths = vec!["/host/proc", "/hostproc", "/proc_host"];
        for host_proc in &host_proc_paths {
            let status_path = format!("{host_proc}/{target_pid}/status");
            if let Ok(status) = fs::read_to_string(&status_path) {
                println!("\n✓ Found in {host_proc}/");
                for line in status.lines() {
                    if line.starts_with("NSpid:") || line.starts_with("Name:") {
                        println!("  {line}");
                    }
                }
            }
        }

        // Strategy 3: Scan all processes
        println!("\n=== Scanning all processes for mapping ===");
        let mut found = false;
        if let Ok(entries) = fs::read_dir("/proc") {
            for entry in entries.flatten() {
                if let Some(pid_str) = entry.file_name().to_str()
                    && let Ok(pid) = pid_str.parse::<u32>()
                    && let Ok(status) = fs::read_to_string(format!("/proc/{pid}/status"))
                {
                    for line in status.lines() {
                        if line.starts_with("NSpid:") {
                            let pids: Vec<&str> = line.split_whitespace().skip(1).collect();
                            // Check if any PID in the list matches our target
                            for (i, p) in pids.iter().enumerate() {
                                if let Ok(parsed) = p.parse::<u32>()
                                    && parsed == target_pid
                                {
                                    if !found {
                                        println!("\nFound mappings:");
                                        found = true;
                                    }
                                    println!(
                                        "  Container PID {pid} -> NSpid: {line} (position {i})"
                                    );
                                    // Also show the process name
                                    for status_line in status.lines() {
                                        if status_line.starts_with("Name:") {
                                            println!("    {status_line}");
                                            break;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        if !found {
            println!("No mappings found for PID {target_pid}");
        }
    }

    // Show example processes
    println!("\n=== Example Process Mappings ===");
    let mut count = 0;
    if let Ok(entries) = fs::read_dir("/proc") {
        for entry in entries.flatten() {
            if count >= 5 {
                break;
            }
            if let Some(pid_str) = entry.file_name().to_str()
                && let Ok(pid) = pid_str.parse::<u32>()
                && let Ok(status) = fs::read_to_string(format!("/proc/{pid}/status"))
            {
                let mut name = String::new();
                let mut nspid = String::new();
                for line in status.lines() {
                    if line.starts_with("Name:") {
                        name = line.to_string();
                    } else if line.starts_with("NSpid:") {
                        nspid = line.to_string();
                    }
                }
                if !nspid.is_empty() && nspid != "NSpid:\t1" {
                    println!("PID {pid}: {name} -> {nspid}");
                    count += 1;
                }
            }
        }
    }
}

fn check_container() -> bool {
    std::path::Path::new("/.dockerenv").exists()
        || fs::read_to_string("/proc/self/cgroup")
            .map(|c| {
                c.contains("/docker/") || c.contains("/containerd/") || c.contains("/kubepods/")
            })
            .unwrap_or(false)
}

fn get_self_pid_mapping() -> Option<(u32, u32)> {
    if let Ok(status) = fs::read_to_string("/proc/self/status") {
        for line in status.lines() {
            if line.starts_with("NSpid:") {
                let pids: Vec<&str> = line.split_whitespace().skip(1).collect();
                if pids.len() >= 2 {
                    let container_pid = pids[0].parse::<u32>().ok()?;
                    let host_pid = pids[1].parse::<u32>().ok()?;
                    return Some((container_pid, host_pid));
                }
            }
        }
    }
    None
}

fn print_pid_info(pid: u32) {
    if let Ok(status) = fs::read_to_string(format!("/proc/{pid}/status")) {
        for line in status.lines() {
            if line.starts_with("Name:") || line.starts_with("NSpid:") || line.starts_with("Pid:") {
                println!("  {line}");
            }
        }
    }
}
