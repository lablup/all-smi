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

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use crate::app_state::{
    AppState, ConnectionStatus, FilterInputMode, ReplayState, SortCriteria, SortDirection,
    UsersTabState,
};
use crate::common::config::EnergyConfig;
use crate::device::{
    ChassisInfo, CpuInfo, GpuInfo, MemoryInfo, MigGpuInfo, ProcessInfo, VgpuHostInfo,
};
use crate::metrics::energy::EnergyAccountant;
use crate::network::metrics_parser::ParsedProcessRow;
use crate::storage::info::StorageInfo;
use crate::ui::aggregation::user::UserAggregationResult;
use crate::ui::alerts::{AlertTransition, Alerter};
use crate::ui::filter_dsl::Expr as FilterExpr;
use crate::ui::notification::NotificationManager;
use crate::ui::tabs::is_users_tab_active;
use crate::ui::topology::TopologyViewMode;
use crate::ui::view_state::ViewState;
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

/// Every collection the frame reads, copied out of `AppState` once per
/// data version and shared between frames through an `Arc`.
///
/// Rebuilding this is the expensive part of a capture (GPU detail maps,
/// process rows, histories), so [`RenderSnapshot::capture`] reuses the
/// previous copy until `AppState::data_version` moves.
pub struct HeavySnapshot {
    pub gpu_info: Vec<GpuInfo>,
    pub cpu_info: Vec<CpuInfo>,
    pub memory_info: Vec<MemoryInfo>,
    pub process_info: Vec<ProcessInfo>,
    pub chassis_info: Vec<ChassisInfo>,
    pub storage_info: Vec<StorageInfo>,
    /// Per-GPU vGPU host info (NVIDIA vGPU only). Empty on bare-metal.
    pub vgpu_info: Vec<VgpuHostInfo>,
    /// Per-GPU MIG host info (NVIDIA datacenter GPUs with MIG enabled). Empty
    /// on consumer cards, pre-Ampere architectures, and non-MIG hosts.
    pub mig_info: Vec<MigGpuInfo>,

    // Connection tracking (remote mode)
    pub connection_status: HashMap<String, ConnectionStatus>,
    pub hostname_to_host_id: HashMap<String, String>,

    // Runtime environment display
    pub runtime_environment: RuntimeEnvironment,

    // Dashboard history data
    pub utilization_history: VecDeque<f64>,
    pub memory_history: VecDeque<f64>,
    pub temperature_history: VecDeque<f64>,
    pub package_power_history: VecDeque<f64>,
    pub ane_power_history: VecDeque<f64>,
    pub dla_utilization_history: VecDeque<f64>,
    pub cpu_utilization_history: VecDeque<f64>,
    pub system_memory_history: VecDeque<f64>,
    pub cpu_temperature_history: VecDeque<f64>,

    // Alerter state (issue #186)
    pub alerter: Alerter,
    pub alert_history: VecDeque<AlertTransition>,

    /// Per-process rows from the remote `all_smi_process_*` families,
    /// read only by the Users-tab drill-down. Copied only while the Users
    /// tab is selected (issue #189); empty otherwise.
    pub remote_process_info: Vec<ParsedProcessRow>,

    /// Energy accountant so the chassis and energy renderers can read
    /// session / lifetime counters without holding the app-state lock
    /// (issue #191).
    pub energy: EnergyAccountant,
}

impl HeavySnapshot {
    fn capture(state: &AppState, with_users: bool) -> Self {
        Self {
            gpu_info: state.gpu_info.clone(),
            cpu_info: state.cpu_info.clone(),
            memory_info: state.memory_info.clone(),
            process_info: state.process_info.clone(),
            chassis_info: state.chassis_info.clone(),
            storage_info: state.storage_info.clone(),
            vgpu_info: state.vgpu_info.clone(),
            mig_info: state.mig_info.clone(),
            connection_status: state.connection_status.clone(),
            hostname_to_host_id: state.hostname_to_host_id.clone(),
            runtime_environment: state.runtime_environment.clone(),
            utilization_history: state.utilization_history.clone(),
            memory_history: state.memory_history.clone(),
            temperature_history: state.temperature_history.clone(),
            package_power_history: state.package_power_history.clone(),
            ane_power_history: state.ane_power_history.clone(),
            dla_utilization_history: state.dla_utilization_history.clone(),
            cpu_utilization_history: state.cpu_utilization_history.clone(),
            system_memory_history: state.system_memory_history.clone(),
            cpu_temperature_history: state.cpu_temperature_history.clone(),
            alerter: state.alerter.clone(),
            alert_history: state.alert_history.clone(),
            remote_process_info: if with_users {
                state.remote_process_info.clone()
            } else {
                Vec::new()
            },
            energy: state.energy.clone(),
        }
    }
}

/// The heavy part of the last capture, owned by the UI loop.
///
/// Keyed on `AppState::data_version` (not `collector_data_version`): UI-only
/// events bump it as well, which costs one rebuild per keypress but cannot
/// miss a writer. Whether the Users tab is selected is part of the key
/// because it decides whether `remote_process_info` is copied at all.
#[derive(Default)]
pub struct HeavyCache(Option<(u64, bool, Arc<HeavySnapshot>)>);

/// A snapshot of `AppState` containing only the data needed for one frame.
///
/// Created quickly under the mutex lock, then used without the lock held
/// for the full (potentially expensive) frame composition path.
///
/// Small UI state is copied per frame; the collections live in
/// [`HeavySnapshot`], shared through an `Arc` until the data version
/// changes. Mutable UI-only bookkeeping (e.g., the frame counter) is
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

    // Frame counter for animation (marquee, loading bar, LED grid pages)
    pub frame_counter: u64,

    // Sort state
    pub sort_criteria: SortCriteria,
    pub sort_direction: SortDirection,

    // Notifications (cloned for display; they expire by wall time)
    pub notifications: NotificationManager,

    // Loading status
    pub startup_status_lines: Vec<String>,

    // Data version for change detection
    pub data_version: u64,

    // Filter state (issue #186)
    pub filter_query: Option<FilterExpr>,
    pub filter_buffer: String,
    pub filter_input_mode: FilterInputMode,
    pub filter_error: Option<String>,
    pub filter_preview_count: Option<(usize, usize)>,
    pub filter_hide_nonmatching: bool,
    pub alert_panel_open: bool,

    // Replay state (issue #187)
    pub replay: Option<ReplayState>,

    // Users tab (issue #189)
    pub users_tab_state: UsersTabState,
    /// Memoised aggregation, shared with `AppState`. Empty unless the
    /// Users tab is selected, so other tabs never run `aggregate_users`.
    pub users_aggregation: Arc<UserAggregationResult>,
    /// Display order of `users_aggregation.users` (see
    /// `AppState::users_visible_order`). Empty unless the Users tab is
    /// selected.
    pub users_visible_order: Arc<[usize]>,

    // Topology tab (issue #190)
    /// Render mode selected by the Topology tab's `M` toggle.
    pub topology_view_mode: TopologyViewMode,
    /// Operator-selected host tab remembered for the Topology view. When
    /// `Some` and still present in `tabs`, the Topology renderer points at
    /// that host; otherwise it falls back to the first host tab.
    pub topology_last_host_tab: Option<String>,

    /// Energy configuration (price, currency, display toggles, issue #191).
    pub energy_config: EnergyConfig,

    /// Collections shared between frames of the same data version.
    pub heavy: Arc<HeavySnapshot>,
}

impl RenderSnapshot {
    /// Capture a snapshot from the live `AppState`.
    ///
    /// Small UI state is cloned every call. The collections are cloned
    /// only when `state.data_version` (or the Users-tab selection) differs
    /// from the entry in `heavy_cache`; otherwise the cached
    /// [`HeavySnapshot`] is shared. The caller should drop the `AppState`
    /// lock immediately after this returns.
    pub fn capture(state: &mut AppState, heavy_cache: &mut HeavyCache) -> Self {
        let on_users_tab = is_users_tab_active(state);

        // The Users-tab aggregation walks every remote process row, so it
        // is materialised (under the lock, memoised per collector version)
        // only while that tab is on screen.
        let (users_aggregation, users_visible_order) = if on_users_tab {
            let order = Arc::clone(state.users_visible_order());
            (Arc::clone(state.users_aggregation()), order)
        } else {
            (Arc::default(), Arc::from(Vec::new()))
        };

        let heavy = match &heavy_cache.0 {
            Some((version, with_users, heavy))
                if *version == state.data_version && *with_users == on_users_tab =>
            {
                Arc::clone(heavy)
            }
            _ => {
                let heavy = Arc::new(HeavySnapshot::capture(state, on_users_tab));
                heavy_cache.0 = Some((state.data_version, on_users_tab, Arc::clone(&heavy)));
                heavy
            }
        };

        Self {
            show_help: state.show_help,
            loading: state.loading,
            is_local_mode: state.is_local_mode,
            gpu_filter_enabled: state.gpu_filter_enabled,
            tabs: state.tabs.clone(),
            current_tab: state.current_tab,
            tab_scroll_offset: state.tab_scroll_offset,
            gpu_scroll_offset: state.gpu_scroll_offset,
            storage_scroll_offset: state.storage_scroll_offset,
            selected_process_index: state.selected_process_index,
            start_index: state.start_index,
            process_horizontal_scroll_offset: state.process_horizontal_scroll_offset,
            frame_counter: state.frame_counter,
            sort_criteria: state.sort_criteria,
            sort_direction: state.sort_direction,
            notifications: state.notifications.clone(),
            startup_status_lines: state.startup_status_lines.clone(),
            data_version: state.data_version,
            filter_query: state.filter_query.clone(),
            filter_buffer: state.filter_buffer.clone(),
            filter_input_mode: state.filter_input_mode,
            filter_error: state.filter_error.clone(),
            filter_preview_count: state.filter_preview_count,
            filter_hide_nonmatching: state.filter_hide_nonmatching,
            alert_panel_open: state.alert_panel_open,
            replay: state.replay.clone(),
            users_tab_state: state.users_tab_state.clone(),
            users_aggregation,
            users_visible_order,
            topology_view_mode: state.topology_view_mode,
            topology_last_host_tab: state.topology_last_host_tab.clone(),
            energy_config: state.energy_config.clone(),
            heavy,
        }
    }
}

impl ViewState for RenderSnapshot {
    fn is_local_mode(&self) -> bool {
        self.is_local_mode
    }
    fn tabs(&self) -> &[String] {
        &self.tabs
    }
    fn current_tab(&self) -> usize {
        self.current_tab
    }
    fn tab_scroll_offset(&self) -> usize {
        self.tab_scroll_offset
    }
    fn gpu_scroll_offset(&self) -> usize {
        self.gpu_scroll_offset
    }
    fn frame_counter(&self) -> u64 {
        self.frame_counter
    }
    fn gpu_info(&self) -> &[GpuInfo] {
        &self.heavy.gpu_info
    }
    fn cpu_info(&self) -> &[CpuInfo] {
        &self.heavy.cpu_info
    }
    fn memory_info(&self) -> &[MemoryInfo] {
        &self.heavy.memory_info
    }
    fn process_info(&self) -> &[ProcessInfo] {
        &self.heavy.process_info
    }
    fn storage_info(&self) -> &[StorageInfo] {
        &self.heavy.storage_info
    }
    fn vgpu_info(&self) -> &[VgpuHostInfo] {
        &self.heavy.vgpu_info
    }
    fn mig_info(&self) -> &[MigGpuInfo] {
        &self.heavy.mig_info
    }
    fn connection_status(&self) -> &HashMap<String, ConnectionStatus> {
        &self.heavy.connection_status
    }
    fn utilization_history(&self) -> &VecDeque<f64> {
        &self.heavy.utilization_history
    }
    fn memory_history(&self) -> &VecDeque<f64> {
        &self.heavy.memory_history
    }
    fn temperature_history(&self) -> &VecDeque<f64> {
        &self.heavy.temperature_history
    }
    fn package_power_history(&self) -> &VecDeque<f64> {
        &self.heavy.package_power_history
    }
    fn ane_power_history(&self) -> &VecDeque<f64> {
        &self.heavy.ane_power_history
    }
    fn dla_utilization_history(&self) -> &VecDeque<f64> {
        &self.heavy.dla_utilization_history
    }
    fn cpu_utilization_history(&self) -> &VecDeque<f64> {
        &self.heavy.cpu_utilization_history
    }
    fn system_memory_history(&self) -> &VecDeque<f64> {
        &self.heavy.system_memory_history
    }
    fn cpu_temperature_history(&self) -> &VecDeque<f64> {
        &self.heavy.cpu_temperature_history
    }
    fn gpu_filter_enabled(&self) -> bool {
        self.gpu_filter_enabled
    }
    fn sort_criteria(&self) -> SortCriteria {
        self.sort_criteria
    }
    fn filter_query(&self) -> &Option<FilterExpr> {
        &self.filter_query
    }
    fn filter_buffer(&self) -> &str {
        &self.filter_buffer
    }
    fn filter_input_mode(&self) -> FilterInputMode {
        self.filter_input_mode
    }
    fn filter_error(&self) -> &Option<String> {
        &self.filter_error
    }
    fn filter_preview_count(&self) -> Option<(usize, usize)> {
        self.filter_preview_count
    }
    fn notifications(&self) -> &NotificationManager {
        &self.notifications
    }
    fn replay(&self) -> &Option<ReplayState> {
        &self.replay
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A GPU row with every optional field empty, for render-path tests.
    pub(crate) fn test_gpu(uuid: &str, name: &str, hostname: &str) -> GpuInfo {
        GpuInfo {
            uuid: uuid.to_string(),
            time: String::new(),
            name: name.to_string(),
            device_type: "GPU".to_string(),
            host_id: hostname.to_string(),
            hostname: hostname.to_string(),
            instance: hostname.to_string(),
            utilization: 10.0,
            ane_utilization: 0.0,
            dla_utilization: None,
            tensorcore_utilization: None,
            temperature: 40,
            used_memory: 1024,
            total_memory: 4096,
            frequency: 1000,
            power_consumption: 50.0,
            gpu_core_count: None,
            temperature_threshold_slowdown: None,
            temperature_threshold_shutdown: None,
            temperature_threshold_max_operating: None,
            temperature_threshold_acoustic: None,
            performance_state: None,
            fan_speed_rpm: None,
            numa_node_id: None,
            gsp_firmware_mode: None,
            gsp_firmware_version: None,
            nvlink_remote_devices: Vec::new(),
            gpm_metrics: None,
            detail: HashMap::new(),
        }
    }

    #[test]
    fn test_snapshot_capture_preserves_flags() {
        let mut state = AppState::new();
        state.show_help = true;
        state.loading = false;
        state.is_local_mode = true;
        state.gpu_filter_enabled = true;

        let snapshot = RenderSnapshot::capture(&mut state, &mut HeavyCache::default());

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

        let snapshot = RenderSnapshot::capture(&mut state, &mut HeavyCache::default());

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

        let snapshot = RenderSnapshot::capture(&mut state, &mut HeavyCache::default());

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

        let snapshot = RenderSnapshot::capture(&mut state, &mut HeavyCache::default());

        assert_eq!(snapshot.sort_criteria, SortCriteria::Utilization);
        assert_eq!(snapshot.sort_direction, SortDirection::Ascending);
    }

    #[test]
    fn test_snapshot_capture_preserves_data_version() {
        let mut state = AppState::new();
        state.mark_data_changed();
        state.mark_data_changed();

        let snapshot = RenderSnapshot::capture(&mut state, &mut HeavyCache::default());
        assert_eq!(snapshot.data_version, 2);
    }

    #[test]
    fn test_snapshot_capture_keeps_history_order() {
        let mut state = AppState::new();
        state.utilization_history.push_back(50.0);
        state.utilization_history.push_back(75.0);
        state.memory_history.push_back(40.0);

        let snapshot = RenderSnapshot::capture(&mut state, &mut HeavyCache::default());

        let util: Vec<f64> = snapshot.utilization_history().iter().copied().collect();
        assert_eq!(util, vec![50.0, 75.0]);
        assert_eq!(snapshot.memory_history().len(), 1);
    }

    #[test]
    fn test_snapshot_is_independent_of_source_state() {
        let mut state = AppState::new();
        state.current_tab = 0;
        state.gpu_scroll_offset = 5;

        let snapshot = RenderSnapshot::capture(&mut state, &mut HeavyCache::default());

        // Mutate source state after snapshot
        state.current_tab = 2;
        state.gpu_scroll_offset = 99;

        // Snapshot should retain original values
        assert_eq!(snapshot.current_tab, 0);
        assert_eq!(snapshot.gpu_scroll_offset, 5);
    }

    #[test]
    fn view_state_reads_the_same_values_from_state_and_snapshot() {
        let mut state = AppState::new();
        state.is_local_mode = false;
        state.tabs = vec!["All".into(), "node-a".into()];
        state.current_tab = 1;
        state.gpu_scroll_offset = 3;
        state.sort_criteria = SortCriteria::GpuMemory;
        state.gpu_info = vec![test_gpu("GPU-0", "H100", "node-a")];
        state.utilization_history.push_back(60.0);

        let snapshot = RenderSnapshot::capture(&mut state, &mut HeavyCache::default());
        let live: &dyn ViewState = &state;
        let snap: &dyn ViewState = &snapshot;

        assert_eq!(live.is_local_mode(), snap.is_local_mode());
        assert_eq!(live.tabs(), snap.tabs());
        assert_eq!(live.current_tab(), snap.current_tab());
        assert_eq!(live.gpu_scroll_offset(), snap.gpu_scroll_offset());
        assert_eq!(live.sort_criteria(), snap.sort_criteria());
        assert_eq!(live.gpu_info().len(), snap.gpu_info().len());
        assert_eq!(live.gpu_info()[0].uuid, snap.gpu_info()[0].uuid);
        assert_eq!(live.utilization_history(), snap.utilization_history());
    }

    #[test]
    fn heavy_part_is_shared_until_data_version_changes() {
        let mut state = AppState::new();
        state.gpu_info = vec![
            test_gpu("GPU-0", "NVIDIA H100 80GB HBM3", "node-a"),
            test_gpu("GPU-1", "NVIDIA H100 80GB HBM3", "node-a"),
        ];
        state.mark_collector_data_changed();
        let mut cache = HeavyCache::default();

        let first = RenderSnapshot::capture(&mut state, &mut cache);
        let second = RenderSnapshot::capture(&mut state, &mut cache);
        assert!(
            Arc::ptr_eq(&first.heavy, &second.heavy),
            "an unchanged data_version must reuse the heavy part"
        );

        state.gpu_info[0].utilization = 99.0;
        state.mark_data_changed();
        let third = RenderSnapshot::capture(&mut state, &mut cache);
        assert!(
            !Arc::ptr_eq(&second.heavy, &third.heavy),
            "a data_version bump must rebuild the heavy part"
        );
        assert_eq!(third.heavy.gpu_info[0].utilization, 99.0);
        assert_eq!(first.heavy.gpu_info[0].utilization, 10.0);
    }

    fn users_tab_state() -> AppState {
        use crate::network::metrics_parser::ParsedProcessRow;
        let mut state = AppState::new();
        state.is_local_mode = false;
        state.tabs = vec![
            "All".into(),
            crate::ui::tabs::USERS_TAB_NAME.into(),
            "host-0".into(),
        ];
        for (pid, user) in [(1u32, "alice"), (2, "bob")] {
            state.remote_process_info.push(ParsedProcessRow {
                host: "host-0".into(),
                pid,
                user: user.into(),
                command: "x".into(),
                name: "x".into(),
                gpu_index: 0,
                gpu_uuid: "GPU-0".into(),
                gpu_memory_bytes: 1024,
                cpu_pct_tenths: 0,
                start_time_seconds: 10,
            });
        }
        state.mark_collector_data_changed();
        state
    }

    #[test]
    fn capture_off_the_users_tab_skips_the_aggregation() {
        let mut state = users_tab_state();
        state.current_tab = 0;
        let mut cache = HeavyCache::default();

        let snapshot = RenderSnapshot::capture(&mut state, &mut cache);
        assert_eq!(
            state.users_aggregation_cache.data_version, None,
            "aggregate_users ran although the Users tab is hidden"
        );
        assert!(snapshot.users_aggregation.users.is_empty());
        assert!(snapshot.heavy.remote_process_info.is_empty());

        // A later collector update while still on another tab stays cold.
        state.mark_collector_data_changed();
        let _ = RenderSnapshot::capture(&mut state, &mut cache);
        assert_eq!(state.users_aggregation_cache.data_version, None);
    }

    #[test]
    fn switching_to_the_users_tab_shows_current_data_on_the_first_frame() {
        let mut state = users_tab_state();
        state.current_tab = 0;
        let mut cache = HeavyCache::default();
        let _ = RenderSnapshot::capture(&mut state, &mut cache);

        // Tab switches do not have to bump data_version for this to work:
        // the Users-tab flag is part of the heavy cache key.
        state.current_tab = 1;
        let snapshot = RenderSnapshot::capture(&mut state, &mut cache);
        assert_eq!(snapshot.users_aggregation.users.len(), 2);
        assert_eq!(snapshot.users_visible_order.len(), 2);
        assert_eq!(snapshot.heavy.remote_process_info.len(), 2);
    }

    #[test]
    fn users_tab_frames_share_aggregation_and_order_without_data_change() {
        let mut state = users_tab_state();
        state.current_tab = 1;
        let mut cache = HeavyCache::default();

        let first = RenderSnapshot::capture(&mut state, &mut cache);
        state.frame_counter += 1;
        let second = RenderSnapshot::capture(&mut state, &mut cache);
        assert!(Arc::ptr_eq(
            &first.users_aggregation,
            &second.users_aggregation
        ));
        assert!(Arc::ptr_eq(
            &first.users_visible_order,
            &second.users_visible_order
        ));
    }

    #[test]
    fn test_snapshot_capture_preserves_topology_state() {
        use crate::ui::topology::TopologyViewMode;
        let mut state = AppState::new();
        state.topology_view_mode = TopologyViewMode::Matrix;
        state.topology_last_host_tab = Some("host2".to_string());

        let snapshot = RenderSnapshot::capture(&mut state, &mut HeavyCache::default());

        assert_eq!(snapshot.topology_view_mode, TopologyViewMode::Matrix);
        assert_eq!(
            snapshot.topology_last_host_tab.as_deref(),
            Some("host2"),
            "last_host_tab must survive the snapshot capture"
        );
    }
}
