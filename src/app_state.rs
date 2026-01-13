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

use crate::device::{ChassisInfo, CpuInfo, GpuInfo, MemoryInfo, ProcessInfo};
use crate::storage::info::StorageInfo;
use crate::ui::notification::NotificationManager;
use crate::utils::RuntimeEnvironment;
use std::cmp::Ordering;
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct ConnectionStatus {
    pub host_id: String, // This is the server address key (e.g., "localhost:10001")
    #[allow(dead_code)]
    pub url: String,
    pub actual_hostname: Option<String>, // The real hostname from API (e.g., "node-0001")
    pub is_connected: bool,
    pub last_successful_connection: Option<Instant>,
    pub consecutive_failures: u32,
    pub last_error: Option<String>,
    pub last_update: Instant,
}

impl ConnectionStatus {
    pub fn new(host_id: String, url: String) -> Self {
        Self {
            host_id,
            url,
            actual_hostname: None,
            is_connected: false,
            last_successful_connection: None,
            consecutive_failures: 0,
            last_error: None,
            last_update: Instant::now(),
        }
    }

    pub fn mark_success(&mut self) {
        self.is_connected = true;
        self.last_successful_connection = Some(Instant::now());
        self.consecutive_failures = 0;
        self.last_error = None;
        self.last_update = Instant::now();
    }

    pub fn mark_failure(&mut self, error: String) {
        self.is_connected = false;
        self.consecutive_failures += 1;
        self.last_error = Some(error);
        self.last_update = Instant::now();
    }

    #[allow(dead_code)]
    pub fn is_recently_failed(&self) -> bool {
        !self.is_connected && self.last_update.elapsed() < Duration::from_secs(30)
    }

    #[allow(dead_code)]
    pub fn connection_duration(&self) -> Option<Duration> {
        self.last_successful_connection.map(|t| t.elapsed())
    }
}

#[derive(Clone)]
pub struct AppState {
    pub gpu_info: Vec<GpuInfo>,
    pub cpu_info: Vec<CpuInfo>,
    pub memory_info: Vec<MemoryInfo>,
    pub process_info: Vec<ProcessInfo>,
    pub chassis_info: Vec<ChassisInfo>,
    pub selected_process_index: usize,
    pub start_index: usize,
    pub sort_criteria: SortCriteria,
    pub sort_direction: SortDirection,
    pub loading: bool,
    pub startup_status_lines: Vec<String>,
    pub tabs: Vec<String>,
    pub current_tab: usize,
    pub gpu_scroll_offset: usize,
    pub storage_scroll_offset: usize,
    pub tab_scroll_offset: usize,
    pub process_horizontal_scroll_offset: usize,
    pub device_name_scroll_offsets: HashMap<String, usize>,
    pub host_id_scroll_offsets: HashMap<String, usize>,
    pub cpu_name_scroll_offsets: HashMap<String, usize>,
    pub frame_counter: u64,
    pub storage_info: Vec<StorageInfo>,
    pub show_help: bool,
    pub show_per_core_cpu: bool,
    pub utilization_history: VecDeque<f64>,
    pub memory_history: VecDeque<f64>,
    pub temperature_history: VecDeque<f64>,
    pub cpu_utilization_history: VecDeque<f64>,
    pub system_memory_history: VecDeque<f64>,
    pub cpu_temperature_history: VecDeque<f64>,
    pub notifications: NotificationManager,
    pub nvml_notification_shown: bool,
    #[cfg(target_os = "linux")]
    pub tenstorrent_notification_shown: bool,
    #[cfg(target_os = "linux")]
    pub tpu_notification_shown: bool,
    // Connection status tracking for remote mode
    pub connection_status: HashMap<String, ConnectionStatus>,
    pub known_hosts: Vec<String>,
    // Reverse lookup: actual_hostname -> host_id for efficient connection status retrieval
    pub hostname_to_host_id: HashMap<String, String>,
    // Mode tracking - true for local monitoring, false for remote monitoring
    pub is_local_mode: bool,
    // Runtime environment (container/VM) information
    pub runtime_environment: RuntimeEnvironment,
    /// Version counter that increments when data changes, used to detect if re-render is needed
    pub data_version: u64,
    /// Filter to show only GPU processes (processes with used_memory > 0)
    pub gpu_filter_enabled: bool,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum SortCriteria {
    // Process sorting (local mode only)
    Pid,            // Process ID
    User,           // User name
    Priority,       // Process priority (PRI)
    Nice,           // Nice value
    VirtualMemory,  // Virtual memory (VIRT)
    ResidentMemory, // Resident memory (RES)
    State,          // Process state
    CpuPercent,     // CPU usage percentage
    MemoryPercent,  // Memory usage percentage (was Memory)
    GpuPercent,     // GPU usage percentage
    GpuMemoryUsage, // GPU memory usage
    CpuTime,        // CPU time (TIME+)
    Command,        // Command line
    // GPU sorting (both local and remote modes)
    Default,     // Hostname then index (current behavior)
    Utilization, // GPU utilization
    GpuMemory,   // GPU memory usage
    #[allow(dead_code)]
    Power, // Power consumption
    #[allow(dead_code)]
    Temperature, // Temperature
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SortDirection {
    Ascending,
    Descending,
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

impl AppState {
    pub fn new() -> Self {
        AppState {
            gpu_info: Vec::new(),
            cpu_info: Vec::new(),
            memory_info: Vec::new(),
            process_info: Vec::new(),
            chassis_info: Vec::new(),
            selected_process_index: 0,
            start_index: 0,
            sort_criteria: SortCriteria::Default,
            sort_direction: SortDirection::Descending,
            loading: true,
            startup_status_lines: Vec::new(),
            tabs: vec![
                "All".to_string(),
                "GPU".to_string(),
                "Storage".to_string(),
                "Process".to_string(),
            ],
            current_tab: 0,
            gpu_scroll_offset: 0,
            storage_scroll_offset: 0,
            tab_scroll_offset: 0,
            process_horizontal_scroll_offset: 0,
            device_name_scroll_offsets: HashMap::new(),
            host_id_scroll_offsets: HashMap::new(),
            cpu_name_scroll_offsets: HashMap::new(),
            frame_counter: 0,
            storage_info: Vec::new(),
            show_help: false,
            show_per_core_cpu: false,
            utilization_history: VecDeque::new(),
            memory_history: VecDeque::new(),
            temperature_history: VecDeque::new(),
            cpu_utilization_history: VecDeque::new(),
            system_memory_history: VecDeque::new(),
            cpu_temperature_history: VecDeque::new(),
            notifications: NotificationManager::new(),
            nvml_notification_shown: false,
            #[cfg(target_os = "linux")]
            tenstorrent_notification_shown: false,
            #[cfg(target_os = "linux")]
            tpu_notification_shown: false,
            // Connection status tracking for remote mode
            connection_status: HashMap::new(),
            known_hosts: Vec::new(),
            hostname_to_host_id: HashMap::new(),
            is_local_mode: true, // Default to local mode
            runtime_environment: RuntimeEnvironment::detect(),
            data_version: 0,
            gpu_filter_enabled: false, // GPU filter disabled by default
        }
    }

    /// Increment the data version to signal that data has changed
    pub fn mark_data_changed(&mut self) {
        self.data_version = self.data_version.wrapping_add(1);
    }
}

impl SortCriteria {
    pub fn sort_gpus(&self, a: &GpuInfo, b: &GpuInfo) -> Ordering {
        match self {
            SortCriteria::Default => {
                // Sort by hostname first, then by index (original behavior)
                a.hostname.cmp(&b.hostname).then_with(|| {
                    let a_index = a
                        .detail
                        .get("index")
                        .and_then(|s| s.parse::<u32>().ok())
                        .unwrap_or(0);
                    let b_index = b
                        .detail
                        .get("index")
                        .and_then(|s| s.parse::<u32>().ok())
                        .unwrap_or(0);
                    a_index.cmp(&b_index)
                })
            }
            SortCriteria::Utilization => {
                // Sort by utilization (descending), then by hostname and index
                b.utilization
                    .partial_cmp(&a.utilization)
                    .unwrap_or(Ordering::Equal)
                    .then_with(|| a.hostname.cmp(&b.hostname))
                    .then_with(|| {
                        let a_index = a
                            .detail
                            .get("index")
                            .and_then(|s| s.parse::<u32>().ok())
                            .unwrap_or(0);
                        let b_index = b
                            .detail
                            .get("index")
                            .and_then(|s| s.parse::<u32>().ok())
                            .unwrap_or(0);
                        a_index.cmp(&b_index)
                    })
            }
            SortCriteria::GpuMemory => {
                // Sort by memory usage (descending), then by hostname and index
                b.used_memory
                    .cmp(&a.used_memory)
                    .then_with(|| a.hostname.cmp(&b.hostname))
                    .then_with(|| {
                        let a_index = a
                            .detail
                            .get("index")
                            .and_then(|s| s.parse::<u32>().ok())
                            .unwrap_or(0);
                        let b_index = b
                            .detail
                            .get("index")
                            .and_then(|s| s.parse::<u32>().ok())
                            .unwrap_or(0);
                        a_index.cmp(&b_index)
                    })
            }
            SortCriteria::Power => {
                // Sort by power consumption (descending), then by hostname and index
                b.power_consumption
                    .partial_cmp(&a.power_consumption)
                    .unwrap_or(Ordering::Equal)
                    .then_with(|| a.hostname.cmp(&b.hostname))
                    .then_with(|| {
                        let a_index = a
                            .detail
                            .get("index")
                            .and_then(|s| s.parse::<u32>().ok())
                            .unwrap_or(0);
                        let b_index = b
                            .detail
                            .get("index")
                            .and_then(|s| s.parse::<u32>().ok())
                            .unwrap_or(0);
                        a_index.cmp(&b_index)
                    })
            }
            SortCriteria::Temperature => {
                // Sort by temperature (descending), then by hostname and index
                b.temperature
                    .cmp(&a.temperature)
                    .then_with(|| a.hostname.cmp(&b.hostname))
                    .then_with(|| {
                        let a_index = a
                            .detail
                            .get("index")
                            .and_then(|s| s.parse::<u32>().ok())
                            .unwrap_or(0);
                        let b_index = b
                            .detail
                            .get("index")
                            .and_then(|s| s.parse::<u32>().ok())
                            .unwrap_or(0);
                        a_index.cmp(&b_index)
                    })
            }
            _ => {
                // For process sorting criteria, fall back to default GPU sorting
                a.hostname.cmp(&b.hostname).then_with(|| {
                    let a_index = a
                        .detail
                        .get("index")
                        .and_then(|s| s.parse::<u32>().ok())
                        .unwrap_or(0);
                    let b_index = b
                        .detail
                        .get("index")
                        .and_then(|s| s.parse::<u32>().ok())
                        .unwrap_or(0);
                    a_index.cmp(&b_index)
                })
            }
        }
    }

    pub fn sort_processes(
        &self,
        a: &ProcessInfo,
        b: &ProcessInfo,
        direction: SortDirection,
    ) -> Ordering {
        let base_ordering = match self {
            SortCriteria::Pid => a.pid.cmp(&b.pid),
            SortCriteria::User => a.user.cmp(&b.user).then_with(|| a.pid.cmp(&b.pid)),
            SortCriteria::Priority => a.priority.cmp(&b.priority).then_with(|| a.pid.cmp(&b.pid)),
            SortCriteria::Nice => a.nice_value.cmp(&b.nice_value).then_with(|| a.pid.cmp(&b.pid)),
            SortCriteria::VirtualMemory => {
                a.memory_vms.cmp(&b.memory_vms).then_with(|| a.pid.cmp(&b.pid))
            }
            SortCriteria::ResidentMemory => {
                a.memory_rss.cmp(&b.memory_rss).then_with(|| a.pid.cmp(&b.pid))
            }
            SortCriteria::State => a.state.cmp(&b.state).then_with(|| a.pid.cmp(&b.pid)),
            SortCriteria::CpuPercent => a
                .cpu_percent
                .partial_cmp(&b.cpu_percent)
                .unwrap_or(Ordering::Equal)
                .then_with(|| a.pid.cmp(&b.pid)),
            SortCriteria::MemoryPercent => a
                .memory_percent
                .partial_cmp(&b.memory_percent)
                .unwrap_or(Ordering::Equal)
                .then_with(|| a.pid.cmp(&b.pid)),
            SortCriteria::GpuPercent => a
                .gpu_utilization
                .partial_cmp(&b.gpu_utilization)
                .unwrap_or(Ordering::Equal)
                .then_with(|| a.pid.cmp(&b.pid)),
            SortCriteria::GpuMemoryUsage => {
                a.used_memory.cmp(&b.used_memory).then_with(|| a.pid.cmp(&b.pid))
            }
            SortCriteria::CpuTime => a.cpu_time.cmp(&b.cpu_time).then_with(|| a.pid.cmp(&b.pid)),
            SortCriteria::Command => a.command.cmp(&b.command).then_with(|| a.pid.cmp(&b.pid)),
            // For GPU-related sorting or default, sort by PID
            _ => a.pid.cmp(&b.pid),
        };

        match direction {
            SortDirection::Ascending => base_ordering,
            SortDirection::Descending => base_ordering.reverse(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_local_mode() {
        // Test case 1: Local mode
        let mut state = AppState::new();
        state.is_local_mode = true;
        assert!(state.is_local_mode);

        // Test case 2: Remote mode
        state.is_local_mode = false;
        assert!(!state.is_local_mode);

        // Test case 3: Default is local mode
        let default_state = AppState::new();
        assert!(default_state.is_local_mode);
    }
}
