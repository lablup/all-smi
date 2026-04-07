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

use std::fs;
use std::process::Command;

use crate::device::{platform_detection::get_os_type, types::ProcessInfoResult};

// Helper function to get system process information
#[allow(dead_code)]
pub fn get_system_process_info(pid: u32) -> ProcessInfoResult {
    let os_type = get_os_type();

    match os_type {
        "linux" => get_linux_process_info(pid),
        "macos" => get_macos_process_info(pid),
        _ => None,
    }
}

#[allow(dead_code)]
fn get_linux_process_info(pid: u32) -> ProcessInfoResult {
    // Read /proc/[pid]/stat for basic process information
    let stat_path = format!("/proc/{pid}/stat");
    let stat_content = fs::read_to_string(&stat_path).ok()?;
    let stat_fields: Vec<&str> = stat_content.split_whitespace().collect();

    if stat_fields.len() < 24 {
        return None;
    }

    // Parse relevant fields
    let state = stat_fields.get(2)?.to_string();
    let ppid = stat_fields.get(3)?.parse::<u32>().ok()?;
    let rss_pages = stat_fields.get(23)?.parse::<u64>().unwrap_or(0);
    let rss_bytes = rss_pages * 4096; // Assuming 4KB page size

    // Read /proc/[pid]/status for additional information
    let status_path = format!("/proc/{pid}/status");
    let status_content = fs::read_to_string(&status_path).ok()?;

    let mut vms_bytes = 0u64;
    let mut uid = 0u32;
    let mut threads = 1u32;

    for line in status_content.lines() {
        if line.starts_with("VmSize:") {
            if let Some(size_str) = line.split_whitespace().nth(1)
                && let Ok(size_kb) = size_str.parse::<u64>()
            {
                vms_bytes = size_kb * 1024; // Convert KB to bytes
            }
        } else if line.starts_with("Uid:") {
            if let Some(uid_str) = line.split_whitespace().nth(1) {
                uid = uid_str.parse::<u32>().unwrap_or(0);
            }
        } else if line.starts_with("Threads:")
            && let Some(thread_str) = line.split_whitespace().nth(1)
        {
            threads = thread_str.parse::<u32>().unwrap_or(1);
        }
    }

    // Get username from UID
    let user = get_username_from_uid(uid);

    // Get process start time
    let start_time = get_process_start_time(pid).unwrap_or_else(|| "unknown".to_string());

    // Read /proc/[pid]/cmdline for command
    let cmdline_path = format!("/proc/{pid}/cmdline");
    let command = if let Ok(cmdline_content) = fs::read_to_string(&cmdline_path) {
        cmdline_content.replace('\0', " ").trim().to_string()
    } else {
        "unknown".to_string()
    };

    // For CPU and memory percentages, we'd need more complex calculations
    // For now, returning placeholder values
    let cpu_percent = 0.0;
    let memory_percent = 0.0;
    let cpu_time = 0u64;

    Some((
        cpu_percent,
        memory_percent,
        rss_bytes,
        vms_bytes,
        user,
        state,
        start_time,
        cpu_time,
        command,
        ppid,
        threads,
    ))
}

#[allow(dead_code)]
fn get_macos_process_info(pid: u32) -> ProcessInfoResult {
    // Use ps command to get process information on macOS
    let output = Command::new("ps")
        .args([
            "-p",
            &pid.to_string(),
            "-o",
            "pid,ppid,uid,pcpu,pmem,rss,vsz,state,lstart,time,comm,args",
        ])
        .output()
        .ok()?;

    let output_str = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = output_str.lines().collect();

    if lines.len() < 2 {
        return None;
    }

    // Parse the process line (skip header)
    let process_line = lines[1];
    let fields: Vec<&str> = process_line.split_whitespace().collect();

    if fields.len() < 11 {
        return None;
    }

    // Parse fields
    let ppid = fields[1].parse::<u32>().unwrap_or(0);
    let uid = fields[2].parse::<u32>().unwrap_or(0);
    let cpu_percent = fields[3].parse::<f64>().unwrap_or(0.0);
    let memory_percent = fields[4].parse::<f64>().unwrap_or(0.0);
    let rss_kb = fields[5].parse::<u64>().unwrap_or(0);
    let vms_kb = fields[6].parse::<u64>().unwrap_or(0);
    let state = fields[7].to_string();

    let rss_bytes = rss_kb * 1024; // Convert KB to bytes
    let vms_bytes = vms_kb * 1024; // Convert KB to bytes

    // Get username from UID
    let user = get_username_from_uid(uid);

    // Start time (fields 8-10 typically)
    let start_time = if fields.len() > 10 {
        format!("{} {} {}", fields[8], fields[9], fields[10])
    } else {
        "unknown".to_string()
    };

    // CPU time
    let cpu_time_str = fields.get(10).unwrap_or(&"0:00");
    let cpu_time = parse_time_to_seconds(cpu_time_str);

    // Command (take remaining fields)
    let command = if fields.len() > 11 {
        fields[11..].join(" ")
    } else {
        fields.get(10).unwrap_or(&"unknown").to_string()
    };

    // Number of threads (not easily available via ps, use 1 as default)
    let num_threads = 1;

    Some((
        cpu_percent,
        memory_percent,
        rss_bytes,
        vms_bytes,
        user,
        state,
        start_time,
        cpu_time,
        command,
        ppid,
        num_threads,
    ))
}

#[allow(dead_code)]
fn get_username_from_uid(uid: u32) -> String {
    // Try to get username from /etc/passwd
    if let Ok(passwd_content) = fs::read_to_string("/etc/passwd") {
        for line in passwd_content.lines() {
            let fields: Vec<&str> = line.split(':').collect();
            if fields.len() >= 3
                && let Ok(line_uid) = fields[2].parse::<u32>()
                && line_uid == uid
            {
                return fields[0].to_string();
            }
        }
    }
    uid.to_string()
}

#[allow(dead_code)]
fn get_process_start_time(pid: u32) -> Option<String> {
    let stat_path = format!("/proc/{pid}/stat");
    let stat_content = fs::read_to_string(&stat_path).ok()?;
    let stat_fields: Vec<&str> = stat_content.split_whitespace().collect();

    if let Some(starttime_str) = stat_fields.get(21)
        && let Ok(starttime_jiffies) = starttime_str.parse::<u64>()
    {
        // Convert jiffies to seconds since boot
        let starttime_seconds = starttime_jiffies / 100; // Assuming 100 HZ

        // Get boot time
        if let Ok(uptime_content) = fs::read_to_string("/proc/uptime")
            && let Some(uptime_str) = uptime_content.split_whitespace().next()
            && let Ok(uptime_seconds) = uptime_str.parse::<f64>()
        {
            let boot_time = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as f64
                - uptime_seconds;

            let process_start_time = boot_time + starttime_seconds as f64;
            let start_time =
                std::time::UNIX_EPOCH + std::time::Duration::from_secs(process_start_time as u64);

            if let Ok(datetime) = start_time.duration_since(std::time::UNIX_EPOCH) {
                return Some(format!("{}", datetime.as_secs()));
            }
        }
    }

    None
}

#[allow(dead_code)]
fn parse_time_to_seconds(time_str: &str) -> u64 {
    // Parse time in format like "0:01.23" or "1:23:45"
    let parts: Vec<&str> = time_str.split(':').collect();

    match parts.len() {
        2 => {
            // MM:SS format
            let minutes = parts[0].parse::<u64>().unwrap_or(0);
            let seconds = parts[1]
                .split('.')
                .next()
                .unwrap_or("0")
                .parse::<u64>()
                .unwrap_or(0);
            minutes * 60 + seconds
        }
        3 => {
            // HH:MM:SS format
            let hours = parts[0].parse::<u64>().unwrap_or(0);
            let minutes = parts[1].parse::<u64>().unwrap_or(0);
            let seconds = parts[2]
                .split('.')
                .next()
                .unwrap_or("0")
                .parse::<u64>()
                .unwrap_or(0);
            hours * 3600 + minutes * 60 + seconds
        }
        _ => 0,
    }
}
