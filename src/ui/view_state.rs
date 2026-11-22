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

//! Read-only view of the state the TUI renderers consume.
//!
//! The renderers (`draw_tabs`, `draw_system_view`, `print_function_keys`,
//! `LayoutCalculator`, ...) are called from two places: the frame path,
//! which holds a lock-free `RenderSnapshot`, and the event handler, which
//! computes scroll bounds from the live `AppState`. Both paths must agree on
//! layout, so the renderers read through this trait instead of a concrete
//! type. That lets the frame path render straight from the snapshot without
//! rebuilding a throwaway `AppState` every frame (issue #499).

use std::collections::{HashMap, VecDeque};

use crate::app_state::{AppState, ConnectionStatus, FilterInputMode, ReplayState, SortCriteria};
use crate::device::{CpuInfo, GpuInfo, MemoryInfo, MigGpuInfo, ProcessInfo, VgpuHostInfo};
use crate::storage::info::StorageInfo;
use crate::ui::filter_dsl::Expr as FilterExpr;
use crate::ui::notification::NotificationManager;

/// The fields the shared renderers read. Every method is a plain accessor.
pub trait ViewState {
    fn is_local_mode(&self) -> bool;
    fn tabs(&self) -> &[String];
    fn current_tab(&self) -> usize;
    fn tab_scroll_offset(&self) -> usize;
    fn gpu_scroll_offset(&self) -> usize;
    fn frame_counter(&self) -> u64;

    fn gpu_info(&self) -> &[GpuInfo];
    fn cpu_info(&self) -> &[CpuInfo];
    fn memory_info(&self) -> &[MemoryInfo];
    fn process_info(&self) -> &[ProcessInfo];
    fn storage_info(&self) -> &[StorageInfo];
    fn vgpu_info(&self) -> &[VgpuHostInfo];
    fn mig_info(&self) -> &[MigGpuInfo];
    fn connection_status(&self) -> &HashMap<String, ConnectionStatus>;

    fn utilization_history(&self) -> &VecDeque<f64>;
    fn memory_history(&self) -> &VecDeque<f64>;
    fn temperature_history(&self) -> &VecDeque<f64>;
    fn package_power_history(&self) -> &VecDeque<f64>;
    fn ane_power_history(&self) -> &VecDeque<f64>;
    fn dla_utilization_history(&self) -> &VecDeque<f64>;
    fn cpu_utilization_history(&self) -> &VecDeque<f64>;
    fn system_memory_history(&self) -> &VecDeque<f64>;
    fn cpu_temperature_history(&self) -> &VecDeque<f64>;

    fn gpu_filter_enabled(&self) -> bool;
    fn sort_criteria(&self) -> SortCriteria;
    fn filter_query(&self) -> &Option<FilterExpr>;
    fn filter_buffer(&self) -> &str;
    fn filter_input_mode(&self) -> FilterInputMode;
    fn filter_error(&self) -> &Option<String>;
    fn filter_preview_count(&self) -> Option<(usize, usize)>;
    fn notifications(&self) -> &NotificationManager;
    fn replay(&self) -> &Option<ReplayState>;
}

impl ViewState for AppState {
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
        &self.gpu_info
    }
    fn cpu_info(&self) -> &[CpuInfo] {
        &self.cpu_info
    }
    fn memory_info(&self) -> &[MemoryInfo] {
        &self.memory_info
    }
    fn process_info(&self) -> &[ProcessInfo] {
        &self.process_info
    }
    fn storage_info(&self) -> &[StorageInfo] {
        &self.storage_info
    }
    fn vgpu_info(&self) -> &[VgpuHostInfo] {
        &self.vgpu_info
    }
    fn mig_info(&self) -> &[MigGpuInfo] {
        &self.mig_info
    }
    fn connection_status(&self) -> &HashMap<String, ConnectionStatus> {
        &self.connection_status
    }
    fn utilization_history(&self) -> &VecDeque<f64> {
        &self.utilization_history
    }
    fn memory_history(&self) -> &VecDeque<f64> {
        &self.memory_history
    }
    fn temperature_history(&self) -> &VecDeque<f64> {
        &self.temperature_history
    }
    fn package_power_history(&self) -> &VecDeque<f64> {
        &self.package_power_history
    }
    fn ane_power_history(&self) -> &VecDeque<f64> {
        &self.ane_power_history
    }
    fn dla_utilization_history(&self) -> &VecDeque<f64> {
        &self.dla_utilization_history
    }
    fn cpu_utilization_history(&self) -> &VecDeque<f64> {
        &self.cpu_utilization_history
    }
    fn system_memory_history(&self) -> &VecDeque<f64> {
        &self.system_memory_history
    }
    fn cpu_temperature_history(&self) -> &VecDeque<f64> {
        &self.cpu_temperature_history
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
