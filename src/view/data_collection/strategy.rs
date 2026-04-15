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

use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::app_state::{AppState, ConnectionStatus};
use crate::device::{
    ChassisInfo, CpuInfo, GpuInfo, MemoryInfo, MigGpuInfo, ProcessInfo, VgpuHostInfo,
};
use crate::storage::info::StorageInfo;

/// Result type for data collection operations
pub type CollectionResult = Result<CollectionData, CollectionError>;

/// Data collected from either local or remote sources
#[derive(Clone)]
pub struct CollectionData {
    pub gpu_info: Vec<GpuInfo>,
    pub cpu_info: Vec<CpuInfo>,
    pub memory_info: Vec<MemoryInfo>,
    pub process_info: Vec<ProcessInfo>,
    pub storage_info: Vec<StorageInfo>,
    pub chassis_info: Vec<ChassisInfo>,
    /// Per-GPU vGPU information. Empty on non-vGPU hosts.
    pub vgpu_info: Vec<VgpuHostInfo>,
    /// Per-GPU MIG information. Empty on non-MIG hosts.
    pub mig_info: Vec<MigGpuInfo>,
    pub connection_statuses: Vec<ConnectionStatus>,
}

impl CollectionData {
    pub fn new() -> Self {
        Self {
            gpu_info: Vec::new(),
            cpu_info: Vec::new(),
            memory_info: Vec::new(),
            process_info: Vec::new(),
            storage_info: Vec::new(),
            chassis_info: Vec::new(),
            vgpu_info: Vec::new(),
            mig_info: Vec::new(),
            connection_statuses: Vec::new(),
        }
    }
}

impl Default for CollectionData {
    fn default() -> Self {
        Self::new()
    }
}

/// Error types for data collection
#[derive(Debug, thiserror::Error)]
#[allow(dead_code)]
pub enum CollectionError {
    #[error("Connection error: {0}")]
    ConnectionError(String),

    #[error("Parse error: {0}")]
    ParseError(String),

    #[error("I/O error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Other error: {0}")]
    Other(String),
}

/// Configuration for data collection
#[derive(Debug, Clone)]
pub struct CollectionConfig {
    pub interval: u64,
    pub first_iteration: bool,
    pub hosts: Vec<String>,
}

impl Default for CollectionConfig {
    fn default() -> Self {
        Self {
            interval: 2,
            first_iteration: true,
            hosts: Vec::new(),
        }
    }
}

/// Strategy interface for data collection
#[async_trait]
#[allow(dead_code)]
pub trait DataCollectionStrategy: Send + Sync {
    /// Collect data according to the strategy
    async fn collect(&self, config: &CollectionConfig) -> CollectionResult;

    /// Update the application state with collected data
    async fn update_state(
        &self,
        app_state: Arc<Mutex<AppState>>,
        data: CollectionData,
        config: &CollectionConfig,
    );

    /// Get the strategy type name for logging
    fn strategy_type(&self) -> &str;

    /// Check if the strategy is ready for collection
    async fn is_ready(&self) -> bool {
        true
    }
}
