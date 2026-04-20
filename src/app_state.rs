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

use crate::common::config::AlertConfig;
use crate::device::{
    ChassisInfo, CpuInfo, GpuInfo, MemoryInfo, MigGpuInfo, ProcessInfo, VgpuHostInfo,
};
use crate::storage::info::StorageInfo;
use crate::ui::alerts::{AlertTransition, Alerter};
use crate::ui::filter_dsl::Expr as FilterExpr;
use crate::ui::notification::NotificationManager;
use crate::utils::RuntimeEnvironment;
use std::cmp::Ordering;
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

/// Input mode for the `/` filter bar.
///
/// The UI loop routes keyboard events differently depending on this state:
/// - `Idle`: normal navigation/quit keys.
/// - `Editing`: every printable key goes into `filter_buffer`, most hotkeys
///   become literal text (e.g. `q` does not quit), and `Enter`/`ESC`
///   commit/clear the query.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum FilterInputMode {
    #[default]
    Idle,
    Editing,
}

/// Maximum number of previous queries kept for `Ctrl-R` recall.
pub const FILTER_RECENT_MAX: usize = 5;

/// Maximum number of alert transitions retained in the in-memory ring
/// buffer. Fixed by the issue's acceptance criteria.
pub const ALERT_HISTORY_MAX: usize = 50;

/// Playback control block for `view --replay` (issue #187).
///
/// Separate struct so live-mode callers can treat `AppState::replay` as a
/// simple `Option<ReplayState>`: when it is `None` replay keybindings are
/// inert and the status bar draws the usual hotkey strip. When `Some`,
/// the event handler accepts SPACE/`[`/`]`/`+`/`-`/`j`/`k`/`g`/`L` and
/// the ReplayDriver thread consumes the `pending_seek`/`pending_step`
/// commands it stashes here.
#[derive(Clone, Debug)]
pub struct ReplayState {
    pub paused: bool,
    pub speed: f32,
    /// Sequence number of the currently-displayed data frame (0-based).
    pub current_seq: u64,
    /// Total data frames materialized so far. Lower bound until EOF.
    pub total_frames: u64,
    /// Elapsed time from frame 0 to the currently-displayed frame.
    pub elapsed: Duration,
    /// Whether the end of the stream has been reached.
    pub at_eof: bool,
    /// Whether the `L` loop toggle is active.
    pub replay_loop: bool,
    /// Absolute seek requested by the event handler (from frame 0).
    /// Consumed by the ReplayDriver on its next tick.
    pub pending_seek: Option<Duration>,
    /// Relative step requested by `]` / `[`. `+1` = one frame forward,
    /// `-1` = one frame back.
    pub pending_step: Option<i32>,
    /// Whether the `g <HH:MM:SS> Enter` timecode editor is open.
    pub timecode_input_mode: bool,
    /// Partial timecode buffer while the editor is open.
    pub timecode_buffer: String,
    /// Parse error shown inline in the status bar when the editor commits
    /// an invalid timecode.
    pub timecode_error: Option<String>,
}

impl ReplayState {
    /// Discrete speed ladder for the `+` / `-` controls.
    pub const SPEED_LADDER: &'static [f32] = &[0.25, 0.5, 1.0, 2.0, 4.0, 8.0];

    /// Cycle speed up (`+`) or down (`-`) through the ladder. Picks the
    /// nearest-then-next step so off-ladder starting speeds still
    /// progress.
    pub fn cycle_speed(&mut self, up: bool) {
        let current = self.speed;
        // Find current index (nearest match).
        let mut best_idx = 0usize;
        let mut best_delta = f32::INFINITY;
        for (i, s) in Self::SPEED_LADDER.iter().enumerate() {
            let d = (s - current).abs();
            if d < best_delta {
                best_delta = d;
                best_idx = i;
            }
        }
        let new_idx = if up {
            (best_idx + 1).min(Self::SPEED_LADDER.len() - 1)
        } else {
            best_idx.saturating_sub(1)
        };
        self.speed = Self::SPEED_LADDER[new_idx];
    }
}

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
    /// Per-GPU vGPU host info for NVIDIA vGPU-enabled hosts.
    /// Empty on bare-metal or non-NVIDIA systems.
    pub vgpu_info: Vec<VgpuHostInfo>,
    /// Per-GPU MIG host info for NVIDIA MIG-enabled hosts (A100/A30/H100/H200).
    /// Empty on consumer cards, pre-Ampere datacenter GPUs, and non-MIG hosts.
    pub mig_info: Vec<MigGpuInfo>,
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
    pub utilization_history: VecDeque<f64>,
    pub memory_history: VecDeque<f64>,
    pub temperature_history: VecDeque<f64>,
    pub package_power_history: VecDeque<f64>,
    pub ane_power_history: VecDeque<f64>,
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
    /// Actual number of visible process rows in the last rendered frame.
    /// Updated by the renderer so the event handler can scroll correctly.
    pub visible_process_rows: usize,
    /// Compiled filter expression (issue #186). `None` means no filter is
    /// active — all rows render at full strength.
    pub filter_query: Option<FilterExpr>,
    /// Current filter input. While [`FilterInputMode::Editing`] is active,
    /// this holds the raw text the operator is typing; otherwise it
    /// mirrors the committed query (or is empty).
    pub filter_buffer: String,
    /// Input mode for the filter bar. See [`FilterInputMode`].
    pub filter_input_mode: FilterInputMode,
    /// Most recent successful queries for `Ctrl-R` recall (newest first).
    pub filter_recent: VecDeque<String>,
    /// Index into [`Self::filter_recent`] selected by the most recent
    /// `Ctrl-R` press. `None` while no recall cycle is in progress.
    pub filter_recall_index: Option<usize>,
    /// Inline parse error shown on the filter bar when the operator types
    /// an invalid query. Cleared on next keystroke or ESC.
    pub filter_error: Option<String>,
    /// Counter for the live-preview matched-rows display.
    pub filter_preview_count: Option<(usize, usize)>,
    /// When true, non-matching rows are hidden rather than dimmed. Future
    /// config-file toggle; defaults to "dim".
    pub filter_hide_nonmatching: bool,
    /// Threshold-alert state machine. Re-evaluated once per collection
    /// tick inside the UI loop.
    pub alerter: Alerter,
    /// Ring buffer of the last [`ALERT_HISTORY_MAX`] transitions for the
    /// `A` panel. Newest first.
    pub alert_history: VecDeque<AlertTransition>,
    /// When true, render the alert history panel instead of the main
    /// device area.
    pub alert_panel_open: bool,
    /// Playback state for `view --replay`. `None` in live modes. When
    /// `Some`, the event handler routes replay-mode keys (SPACE, `]`/`[`,
    /// `+`/`-`, `j`/`k`, `g`, `L`) to the embedded [`ReplayState`] and
    /// the status bar draws the `REPLAY | ts/total | speed | state`
    /// indicator instead of the normal hotkey strip.
    pub replay: Option<ReplayState>,
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
            vgpu_info: Vec::new(),
            mig_info: Vec::new(),
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
            utilization_history: VecDeque::new(),
            memory_history: VecDeque::new(),
            temperature_history: VecDeque::new(),
            package_power_history: VecDeque::new(),
            ane_power_history: VecDeque::new(),
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
            runtime_environment: RuntimeEnvironment::default(),
            data_version: 0,
            gpu_filter_enabled: false, // GPU filter disabled by default
            visible_process_rows: 0,
            filter_query: None,
            filter_buffer: String::new(),
            filter_input_mode: FilterInputMode::Idle,
            filter_recent: VecDeque::with_capacity(FILTER_RECENT_MAX),
            filter_recall_index: None,
            filter_error: None,
            filter_preview_count: None,
            filter_hide_nonmatching: false,
            alerter: Alerter::new(AlertConfig::default()),
            alert_history: VecDeque::with_capacity(ALERT_HISTORY_MAX),
            alert_panel_open: false,
            replay: None,
        }
    }

    /// Helper used by the event handler: push a query onto the
    /// most-recent list, dedupe against the previous entry, and cap at
    /// [`FILTER_RECENT_MAX`].
    pub fn push_recent_filter(&mut self, query: String) {
        if query.trim().is_empty() {
            return;
        }
        // Dedupe consecutive duplicates.
        if self.filter_recent.front().map(|s| s.as_str()) == Some(query.as_str()) {
            return;
        }
        self.filter_recent.push_front(query);
        while self.filter_recent.len() > FILTER_RECENT_MAX {
            self.filter_recent.pop_back();
        }
    }

    /// Helper used by the event handler: append a transition to the ring
    /// buffer while keeping its length at [`ALERT_HISTORY_MAX`].
    pub fn push_alert_transition(&mut self, t: AlertTransition) {
        self.alert_history.push_front(t);
        while self.alert_history.len() > ALERT_HISTORY_MAX {
            self.alert_history.pop_back();
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
            SortCriteria::Nice => a
                .nice_value
                .cmp(&b.nice_value)
                .then_with(|| a.pid.cmp(&b.pid)),
            SortCriteria::VirtualMemory => a
                .memory_vms
                .cmp(&b.memory_vms)
                .then_with(|| a.pid.cmp(&b.pid)),
            SortCriteria::ResidentMemory => a
                .memory_rss
                .cmp(&b.memory_rss)
                .then_with(|| a.pid.cmp(&b.pid)),
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
            SortCriteria::GpuMemoryUsage => a
                .used_memory
                .cmp(&b.used_memory)
                .then_with(|| a.pid.cmp(&b.pid)),
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

    #[test]
    fn test_gpu_filter_default() {
        let state = AppState::new();
        // GPU filter should be disabled by default
        assert!(!state.gpu_filter_enabled);
    }

    #[test]
    fn test_filter_state_defaults() {
        let state = AppState::new();
        assert!(state.filter_query.is_none());
        assert_eq!(state.filter_input_mode, FilterInputMode::Idle);
        assert!(state.filter_buffer.is_empty());
        assert!(state.filter_recent.is_empty());
        assert!(state.filter_error.is_none());
        assert!(!state.alert_panel_open);
    }

    #[test]
    fn test_push_recent_filter_deduplicates_consecutive() {
        let mut state = AppState::new();
        state.push_recent_filter("temp>80".to_string());
        state.push_recent_filter("temp>80".to_string());
        assert_eq!(state.filter_recent.len(), 1);
    }

    #[test]
    fn test_push_recent_filter_caps_at_max() {
        let mut state = AppState::new();
        for i in 0..10 {
            state.push_recent_filter(format!("temp>{i}"));
        }
        assert_eq!(state.filter_recent.len(), FILTER_RECENT_MAX);
        // Newest first.
        assert_eq!(state.filter_recent[0], "temp>9");
    }

    #[test]
    fn test_push_alert_transition_caps_ring() {
        use crate::ui::alerts::{AlertLevel, AlertTransition, RuleKind};
        use chrono::Local;

        let mut state = AppState::new();
        for i in 0..60 {
            state.push_alert_transition(AlertTransition {
                timestamp: Local::now(),
                host: format!("h{i}"),
                gpu_index: Some(0),
                rule: RuleKind::Temperature,
                from: AlertLevel::Ok,
                to: AlertLevel::Warn,
                value: 85.0,
                threshold: 80.0,
                message: format!("msg{i}"),
                card_key: format!("GPU-{i}"),
            });
        }
        assert_eq!(state.alert_history.len(), ALERT_HISTORY_MAX);
        // Newest is in front.
        assert_eq!(state.alert_history[0].host, "h59");
    }

    #[test]
    fn test_gpu_filter_toggle() {
        let mut state = AppState::new();
        assert!(!state.gpu_filter_enabled);

        // Enable filter
        state.gpu_filter_enabled = true;
        assert!(state.gpu_filter_enabled);

        // Disable filter
        state.gpu_filter_enabled = false;
        assert!(!state.gpu_filter_enabled);
    }

    #[test]
    fn test_data_version_increment() {
        let mut state = AppState::new();
        let initial_version = state.data_version;

        state.mark_data_changed();
        assert_eq!(state.data_version, initial_version + 1);

        state.mark_data_changed();
        assert_eq!(state.data_version, initial_version + 2);
    }

    fn create_test_process(pid: u32, used_memory: u64) -> ProcessInfo {
        ProcessInfo {
            device_id: 0,
            device_uuid: "test-uuid".to_string(),
            pid,
            used_memory,
            process_name: format!("process_{pid}"),
            user: "testuser".to_string(),
            state: "S".to_string(),
            command: format!("/usr/bin/process_{pid}"),
            cpu_percent: 10.0,
            memory_percent: 5.0,
            gpu_utilization: 0.0,
            priority: 20,
            nice_value: 0,
            memory_vms: 1024 * 1024,
            memory_rss: 512 * 1024,
            cpu_time: 100,
            start_time: "00:00:00".to_string(),
            ppid: 1,
            threads: 1,
            uses_gpu: used_memory > 0,
        }
    }

    #[test]
    fn test_sort_processes_by_pid_with_stability() {
        // Test that sorting is stable - equal primary keys should be sorted by PID
        let p1 = create_test_process(100, 1024);
        let p2 = create_test_process(200, 1024);
        let p3 = create_test_process(50, 1024);

        let criteria = SortCriteria::GpuMemoryUsage;

        // All have same GPU memory, so they should be sorted by PID as secondary key
        // In descending order, higher PID comes first (reversed from ascending)
        let ordering = criteria.sort_processes(&p1, &p2, SortDirection::Descending);
        assert_eq!(
            ordering,
            Ordering::Greater,
            "p1 (pid 100) should come after p2 (pid 200) in descending order"
        );

        // In ascending order, lower PID comes first
        let ordering = criteria.sort_processes(&p3, &p1, SortDirection::Ascending);
        assert_eq!(
            ordering,
            Ordering::Less,
            "p3 (pid 50) should come before p1 (pid 100) in ascending order"
        );
    }

    #[test]
    fn test_sort_processes_by_gpu_memory() {
        let p1 = create_test_process(100, 1024);
        let p2 = create_test_process(200, 2048);

        let criteria = SortCriteria::GpuMemoryUsage;

        // In descending order, higher memory should come first
        let ordering = criteria.sort_processes(&p1, &p2, SortDirection::Descending);
        assert_eq!(
            ordering,
            Ordering::Greater,
            "p1 (1024 MB) should come after p2 (2048 MB) in descending order"
        );

        // In ascending order, lower memory should come first
        let ordering = criteria.sort_processes(&p1, &p2, SortDirection::Ascending);
        assert_eq!(
            ordering,
            Ordering::Less,
            "p1 (1024 MB) should come before p2 (2048 MB) in ascending order"
        );
    }

    #[test]
    fn test_sort_processes_by_cpu_percent_with_stability() {
        let mut p1 = create_test_process(100, 0);
        let mut p2 = create_test_process(200, 0);
        let mut p3 = create_test_process(50, 0);

        p1.cpu_percent = 50.0;
        p2.cpu_percent = 50.0;
        p3.cpu_percent = 50.0;

        let criteria = SortCriteria::CpuPercent;

        // All have same CPU%, so they should be sorted by PID as secondary key
        // In ascending order, lower PID comes first
        let ordering = criteria.sort_processes(&p1, &p2, SortDirection::Ascending);
        assert_eq!(
            ordering,
            Ordering::Less,
            "p1 (pid 100) should come before p2 (pid 200) when CPU% is equal (ascending)"
        );

        // In descending order, higher PID comes first (reversed)
        let ordering = criteria.sort_processes(&p3, &p1, SortDirection::Descending);
        assert_eq!(
            ordering,
            Ordering::Greater,
            "p3 (pid 50) should come after p1 (pid 100) in descending order"
        );
    }

    #[test]
    fn test_sort_processes_multiple_criteria() {
        let mut p1 = create_test_process(100, 1024);
        let mut p2 = create_test_process(200, 2048);
        let mut p3 = create_test_process(50, 1024);

        p1.memory_percent = 10.0;
        p2.memory_percent = 20.0;
        p3.memory_percent = 10.0;

        // Test MemoryPercent criteria
        let criteria = SortCriteria::MemoryPercent;
        let ordering = criteria.sort_processes(&p1, &p2, SortDirection::Descending);
        assert_eq!(
            ordering,
            Ordering::Greater,
            "p1 (10%) should come after p2 (20%) in descending order"
        );

        // p1 and p3 have same memory%, should be sorted by PID
        // In descending order, the order is reversed: lower PID (p3=50) > higher PID (p1=100)
        // So p1 (100) compared to p3 (50): base ordering = Less (100 > 50 in PID cmp)
        // After reverse for descending: Greater
        // Wait, let me think again:
        // base_ordering: a.pid.cmp(&b.pid) where a=p1(100), b=p3(50) -> 100.cmp(&50) = Greater
        // After reverse for descending: Less
        let ordering = criteria.sort_processes(&p1, &p3, SortDirection::Descending);
        assert_eq!(
            ordering,
            Ordering::Less,
            "p1 (pid 100) should come before p3 (pid 50) in descending sort (reversed from ascending)"
        );

        // In ascending order, lower PID comes first
        let ordering = criteria.sort_processes(&p1, &p3, SortDirection::Ascending);
        assert_eq!(
            ordering,
            Ordering::Greater,
            "p1 (pid 100) should come after p3 (pid 50) in ascending order"
        );
    }
}
