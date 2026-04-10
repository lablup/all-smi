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

//! Lightweight render snapshot captured from `AppState` under lock.
//!
//! The snapshot contains only the data needed for a single frame, allowing
//! the mutex to be released before expensive frame composition begins.
//! This keeps the critical section short so that background data collectors
//! are not blocked while the UI assembles its output.

use std::collections::HashMap;

use crate::app_state::{AppState, ConnectionStatus, SortCriteria, SortDirection};
use crate::device::{ChassisInfo, CpuInfo, GpuInfo, MemoryInfo, ProcessInfo};
use crate::storage::info::StorageInfo;
use crate::ui::notification::NotificationManager;
use crate::utils::RuntimeEnvironment;

/// Pre-computed rendering decisions captured from `AppState` under lock.
///
/// These booleans let the UI loop decide how to proceed without re-reading
/// shared state after the lock is released.
#[derive(Clone, Debug)]
pub struct RenderDecisions {
    pub force_clear: bool,
    #[allow(dead_code)] // Future caching: skip composition when nothing changed
    pub should_render: bool,
    #[allow(dead_code)] // Exposed for coordinator queries outside the main loop
    pub animations_needed: bool,
}

/// A snapshot of `AppState` containing only the data needed for one frame.
///
/// Created quickly under the mutex lock, then used without the lock held
/// for the full (potentially expensive) frame composition path.
///
/// The snapshot mirrors the `AppState` fields that are read during rendering.
/// Mutable UI-only bookkeeping (e.g., frame counters, scroll offsets) is
/// updated under the lock before the snapshot is taken, so the snapshot
/// itself is immutable from the render path's perspective.
#[derive(Clone)]
pub struct RenderSnapshot {
    // Mode and display flags
    pub show_help: bool,
    pub loading: bool,
    pub is_local_mode: bool,
    pub gpu_filter_enabled: bool,

    // Tab state
    pub tabs: Vec<String>,
    pub current_tab: usize,
    pub tab_scroll_offset: usize,

    // Scroll and selection state
    pub gpu_scroll_offset: usize,
    pub storage_scroll_offset: usize,
    pub selected_process_index: usize,
    pub start_index: usize,
    pub process_horizontal_scroll_offset: usize,

    // Scroll animation offsets (marquee text)
    pub device_name_scroll_offsets: HashMap<String, usize>,
    pub host_id_scroll_offsets: HashMap<String, usize>,
    pub cpu_name_scroll_offsets: HashMap<String, usize>,

    // Frame counter for animation
    pub frame_counter: u64,

    // Sort state
    pub sort_criteria: SortCriteria,
    pub sort_direction: SortDirection,

    // Device data
    pub gpu_info: Vec<GpuInfo>,
    pub cpu_info: Vec<CpuInfo>,
    pub memory_info: Vec<MemoryInfo>,
    pub process_info: Vec<ProcessInfo>,
    pub chassis_info: Vec<ChassisInfo>,
    pub storage_info: Vec<StorageInfo>,

    // Connection tracking (remote mode)
    pub connection_status: HashMap<String, ConnectionStatus>,
    pub hostname_to_host_id: HashMap<String, String>,

    // Runtime environment display
    pub runtime_environment: RuntimeEnvironment,

    // Dashboard history data (Vec instead of VecDeque for lighter clone)
    pub utilization_history: Vec<f64>,
    pub memory_history: Vec<f64>,
    pub temperature_history: Vec<f64>,
    pub cpu_utilization_history: Vec<f64>,
    pub system_memory_history: Vec<f64>,
    pub cpu_temperature_history: Vec<f64>,

    // Notifications (cloned for display)
    pub notifications: NotificationManager,

    // Loading status
    pub startup_status_lines: Vec<String>,

    // Data version for change detection
    pub data_version: u64,
}

impl RenderSnapshot {
    /// Capture a snapshot from the live `AppState`.
    ///
    /// This clones only the data needed for rendering. The caller should drop
    /// the `AppState` lock immediately after this returns.
    ///
    /// History VecDeques are converted to Vec to avoid cloning the deque
    /// ring-buffer internals; the rendering path only iterates forward.
    pub fn capture(state: &AppState) -> Self {
        Self {
            // Flags -- Copy types, no allocation
            show_help: state.show_help,
            loading: state.loading,
            is_local_mode: state.is_local_mode,
            gpu_filter_enabled: state.gpu_filter_enabled,

            // Tab state -- cheap Vec<String> clone
            tabs: state.tabs.clone(),
            current_tab: state.current_tab,
            tab_scroll_offset: state.tab_scroll_offset,

            // Scroll/selection -- Copy types
            gpu_scroll_offset: state.gpu_scroll_offset,
            storage_scroll_offset: state.storage_scroll_offset,
            selected_process_index: state.selected_process_index,
            start_index: state.start_index,
            process_horizontal_scroll_offset: state.process_horizontal_scroll_offset,

            // Scroll animation offsets
            device_name_scroll_offsets: state.device_name_scroll_offsets.clone(),
            host_id_scroll_offsets: state.host_id_scroll_offsets.clone(),
            cpu_name_scroll_offsets: state.cpu_name_scroll_offsets.clone(),

            // Frame counter
            frame_counter: state.frame_counter,

            // Sort state -- Copy types
            sort_criteria: state.sort_criteria,
            sort_direction: state.sort_direction,

            // Device data -- Vec clones (main cost of snapshot)
            gpu_info: state.gpu_info.clone(),
            cpu_info: state.cpu_info.clone(),
            memory_info: state.memory_info.clone(),
            process_info: state.process_info.clone(),
            chassis_info: state.chassis_info.clone(),
            storage_info: state.storage_info.clone(),

            // Connection tracking
            connection_status: state.connection_status.clone(),
            hostname_to_host_id: state.hostname_to_host_id.clone(),

            // Runtime environment
            runtime_environment: state.runtime_environment.clone(),

            // History -- convert VecDeque -> Vec (avoids deque ring-buffer clone)
            utilization_history: state.utilization_history.iter().copied().collect(),
            memory_history: state.memory_history.iter().copied().collect(),
            temperature_history: state.temperature_history.iter().copied().collect(),
            cpu_utilization_history: state.cpu_utilization_history.iter().copied().collect(),
            system_memory_history: state.system_memory_history.iter().copied().collect(),
            cpu_temperature_history: state.cpu_temperature_history.iter().copied().collect(),

            // Notifications
            notifications: state.notifications.clone(),

            // Loading status
            startup_status_lines: state.startup_status_lines.clone(),

            // Data version
            data_version: state.data_version,
        }
    }

    /// Reconstruct a temporary `AppState` from this snapshot.
    ///
    /// This is used for backward compatibility with existing UI functions
    /// (e.g., `draw_system_view`, `draw_tabs`, `print_function_keys`) that
    /// accept `&AppState`. The returned state is a read-only view and should
    /// never be written back to shared state.
    ///
    /// As UI functions are gradually migrated to accept `&RenderSnapshot`
    /// directly, uses of this method should decrease.
    pub fn as_app_state(&self) -> AppState {
        let mut state = AppState::new();

        // Flags
        state.show_help = self.show_help;
        state.loading = self.loading;
        state.is_local_mode = self.is_local_mode;
        state.gpu_filter_enabled = self.gpu_filter_enabled;

        // Tab state
        state.tabs = self.tabs.clone();
        state.current_tab = self.current_tab;
        state.tab_scroll_offset = self.tab_scroll_offset;

        // Scroll/selection
        state.gpu_scroll_offset = self.gpu_scroll_offset;
        state.storage_scroll_offset = self.storage_scroll_offset;
        state.selected_process_index = self.selected_process_index;
        state.start_index = self.start_index;
        state.process_horizontal_scroll_offset = self.process_horizontal_scroll_offset;

        // Scroll offsets
        state.device_name_scroll_offsets = self.device_name_scroll_offsets.clone();
        state.host_id_scroll_offsets = self.host_id_scroll_offsets.clone();
        state.cpu_name_scroll_offsets = self.cpu_name_scroll_offsets.clone();

        // Frame counter
        state.frame_counter = self.frame_counter;

        // Sort
        state.sort_criteria = self.sort_criteria;
        state.sort_direction = self.sort_direction;

        // Device data
        state.gpu_info = self.gpu_info.clone();
        state.cpu_info = self.cpu_info.clone();
        state.memory_info = self.memory_info.clone();
        state.process_info = self.process_info.clone();
        state.chassis_info = self.chassis_info.clone();
        state.storage_info = self.storage_info.clone();

        // Connection tracking
        state.connection_status = self.connection_status.clone();
        state.hostname_to_host_id = self.hostname_to_host_id.clone();

        // Runtime environment
        state.runtime_environment = self.runtime_environment.clone();

        // History (Vec -> VecDeque)
        state.utilization_history = self.utilization_history.iter().copied().collect();
        state.memory_history = self.memory_history.iter().copied().collect();
        state.temperature_history = self.temperature_history.iter().copied().collect();
        state.cpu_utilization_history = self.cpu_utilization_history.iter().copied().collect();
        state.system_memory_history = self.system_memory_history.iter().copied().collect();
        state.cpu_temperature_history = self.cpu_temperature_history.iter().copied().collect();

        // Notifications
        state.notifications = self.notifications.clone();

        // Loading
        state.startup_status_lines = self.startup_status_lines.clone();

        // Data version
        state.data_version = self.data_version;

        state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_snapshot_capture_preserves_flags() {
        let mut state = AppState::new();
        state.show_help = true;
        state.loading = false;
        state.is_local_mode = true;
        state.gpu_filter_enabled = true;

        let snapshot = RenderSnapshot::capture(&state);

        assert!(snapshot.show_help);
        assert!(!snapshot.loading);
        assert!(snapshot.is_local_mode);
        assert!(snapshot.gpu_filter_enabled);
    }

    #[test]
    fn test_snapshot_capture_preserves_tab_state() {
        let mut state = AppState::new();
        state.tabs = vec!["All".to_string(), "Node1".to_string(), "Node2".to_string()];
        state.current_tab = 1;
        state.tab_scroll_offset = 0;

        let snapshot = RenderSnapshot::capture(&state);

        assert_eq!(snapshot.tabs.len(), 3);
        assert_eq!(snapshot.current_tab, 1);
        assert_eq!(snapshot.tabs[1], "Node1");
    }

    #[test]
    fn test_snapshot_capture_preserves_scroll_state() {
        let mut state = AppState::new();
        state.gpu_scroll_offset = 5;
        state.storage_scroll_offset = 3;
        state.selected_process_index = 10;
        state.process_horizontal_scroll_offset = 20;

        let snapshot = RenderSnapshot::capture(&state);

        assert_eq!(snapshot.gpu_scroll_offset, 5);
        assert_eq!(snapshot.storage_scroll_offset, 3);
        assert_eq!(snapshot.selected_process_index, 10);
        assert_eq!(snapshot.process_horizontal_scroll_offset, 20);
    }

    #[test]
    fn test_snapshot_capture_preserves_sort_state() {
        let mut state = AppState::new();
        state.sort_criteria = SortCriteria::Utilization;
        state.sort_direction = SortDirection::Ascending;

        let snapshot = RenderSnapshot::capture(&state);

        assert_eq!(snapshot.sort_criteria, SortCriteria::Utilization);
        assert_eq!(snapshot.sort_direction, SortDirection::Ascending);
    }

    #[test]
    fn test_snapshot_capture_preserves_data_version() {
        let mut state = AppState::new();
        state.mark_data_changed();
        state.mark_data_changed();

        let snapshot = RenderSnapshot::capture(&state);
        assert_eq!(snapshot.data_version, 2);
    }

    #[test]
    fn test_snapshot_capture_converts_history_from_vecdeque() {
        let mut state = AppState::new();
        state.utilization_history.push_back(50.0);
        state.utilization_history.push_back(75.0);
        state.memory_history.push_back(40.0);

        let snapshot = RenderSnapshot::capture(&state);

        assert_eq!(snapshot.utilization_history, vec![50.0, 75.0]);
        assert_eq!(snapshot.memory_history, vec![40.0]);
    }

    #[test]
    fn test_snapshot_is_independent_of_source_state() {
        let mut state = AppState::new();
        state.current_tab = 0;
        state.gpu_scroll_offset = 5;

        let snapshot = RenderSnapshot::capture(&state);

        // Mutate source state after snapshot
        state.current_tab = 2;
        state.gpu_scroll_offset = 99;

        // Snapshot should retain original values
        assert_eq!(snapshot.current_tab, 0);
        assert_eq!(snapshot.gpu_scroll_offset, 5);
    }

    #[test]
    fn test_as_app_state_roundtrip() {
        let mut state = AppState::new();
        state.show_help = true;
        state.current_tab = 2;
        state.gpu_scroll_offset = 10;
        state.sort_criteria = SortCriteria::GpuMemory;
        state.data_version = 42;
        state.utilization_history.push_back(60.0);

        let snapshot = RenderSnapshot::capture(&state);
        let restored = snapshot.as_app_state();

        assert!(restored.show_help);
        assert_eq!(restored.current_tab, 2);
        assert_eq!(restored.gpu_scroll_offset, 10);
        assert_eq!(restored.sort_criteria, SortCriteria::GpuMemory);
        assert_eq!(restored.data_version, 42);
        assert_eq!(restored.utilization_history.len(), 1);
    }
}
