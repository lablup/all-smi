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
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::app_state::{AppState, ConnectionStatus};
use crate::common::config::AppConfig;
use crate::network::NetworkClient;
use crate::network::host_backoff::ScrapeTimeouts;
use crate::storage::info::StorageInfo;

use super::aggregator::DataAggregator;
use super::strategy::{
    CollectionConfig, CollectionData, CollectionError, CollectionResult, DataCollectionStrategy,
};

/// Extract the full host:port combination as unique identifier
fn extract_host_identifier(url: &str) -> String {
    crate::common::http_hosts::http_host_identifier(url)
}

pub struct RemoteCollector {
    network_client: NetworkClient,
    semaphore: Arc<tokio::sync::Semaphore>,
    aggregator: DataAggregator,
}

impl RemoteCollector {
    /// `max_connections` caps concurrent scrapes. Each tick spawns one task
    /// per configured host, so the effective concurrency is
    /// `min(current host count, max_connections)` and follows hostfile
    /// reloads without rebuilding the semaphore.
    pub fn new(max_connections: usize) -> Self {
        Self::with_network_client(NetworkClient::new(), max_connections)
    }

    pub(crate) fn with_network_client(
        network_client: NetworkClient,
        max_connections: usize,
    ) -> Self {
        Self {
            network_client,
            semaphore: Arc::new(tokio::sync::Semaphore::new(max_connections.max(1))),
            aggregator: DataAggregator::new(),
        }
    }

    #[allow(dead_code)]
    pub fn with_hosts(hosts: Vec<String>) -> Self {
        let _ = hosts;
        Self::new(AppConfig::MAX_CONCURRENT_CONNECTIONS)
    }

    fn deduplicate_storage_info(storage_info: Vec<StorageInfo>) -> Vec<StorageInfo> {
        let mut deduplicated_storage: HashMap<String, StorageInfo> = HashMap::new();
        for storage in storage_info {
            let dedup_key = format!("{}:{}", storage.hostname, storage.mount_point);
            deduplicated_storage.insert(dedup_key, storage);
        }
        let mut final_storage_info: Vec<StorageInfo> = deduplicated_storage.into_values().collect();

        // Sort by hostname first, then by mount point for consistent ordering
        final_storage_info.sort_by(|a, b| match a.hostname.cmp(&b.hostname) {
            std::cmp::Ordering::Equal => a.mount_point.cmp(&b.mount_point),
            other => other,
        });

        final_storage_info
    }

    fn update_connection_status(
        state: &mut AppState,
        connection_statuses: Vec<ConnectionStatus>,
        hosts: &[String],
    ) {
        // Follow the configured host list: a hostfile reload can add or
        // remove hosts between ticks, and tabs, the Users coverage count,
        // and the status map are all keyed off `known_hosts` (issue #500).
        let mut configured: Vec<String> = Vec::with_capacity(hosts.len());
        let mut configured_set: HashSet<String> = HashSet::with_capacity(hosts.len());
        for host in hosts {
            let host_id = extract_host_identifier(host);
            if configured_set.insert(host_id.clone()) {
                configured.push(host_id);
            }
        }
        if state.known_hosts != configured {
            state.known_hosts = configured;
        }
        state
            .connection_status
            .retain(|host_id, _| configured_set.contains(host_id));

        let mut reported: HashSet<String> = HashSet::with_capacity(connection_statuses.len());

        // Update connection status for each received status
        for mut status in connection_statuses {
            if !configured_set.contains(&status.host_id) {
                continue;
            }
            // Preserve actual_hostname from previous successful connection if current doesn't have it
            if status.actual_hostname.is_none()
                && let Some(existing_status) = state.connection_status.get(&status.host_id)
                && let Some(existing_hostname) = &existing_status.actual_hostname
            {
                status.actual_hostname = Some(existing_hostname.clone());
            }
            reported.insert(status.host_id.clone());
            state
                .connection_status
                .insert(status.host_id.clone(), status);
        }

        // A configured host without a status this tick did not answer. Mark
        // it failed even when the previous tick saw it connected, so a host
        // that goes dark cannot keep a stale "connected" entry.
        for host in hosts {
            let host_id = extract_host_identifier(host);
            if reported.contains(&host_id) {
                continue;
            }
            let status = state
                .connection_status
                .entry(host_id.clone())
                .or_insert_with(|| ConnectionStatus::new(host_id, host.clone()));
            status.mark_failure("No response received".to_string());
            reported.insert(status.host_id.clone());
        }

        // Rebuild the reverse lookup from the retained statuses.
        state.hostname_to_host_id.clear();
        for status in state.connection_status.values() {
            if let Some(actual_hostname) = &status.actual_hostname {
                state
                    .hostname_to_host_id
                    .insert(actual_hostname.clone(), status.host_id.clone());
            }
        }
    }

    fn update_remote_tabs(state: &mut AppState) {
        // Tab layout (issues #189, #190):
        //   [All, Users, Topology, <host1>, <host2>, ...]
        //
        // Cluster-level tabs cluster together at the left edge of the
        // row. Inserting them there adjusts the indices but the
        // subsequent `current_tab` snap-back guards against the previous
        // index being out of range after tabs shrink (e.g. all hosts
        // disconnected).
        let mut tabs = vec![
            "All".to_string(),
            crate::ui::tabs::USERS_TAB_NAME.to_string(),
            crate::ui::tabs::TOPOLOGY_TAB_NAME.to_string(),
        ];
        tabs.extend(state.known_hosts.clone());

        // Preserve the operator's current selection where possible:
        // if the tab name at `current_tab` still exists in the new
        // layout, jump to its new index. Otherwise clamp to 0 ("All").
        let previous_name = state.tabs.get(state.current_tab).cloned();
        state.tabs = tabs;
        if let Some(name) = previous_name
            && let Some(idx) = state.tabs.iter().position(|t| *t == name)
        {
            state.current_tab = idx;
        } else if state.current_tab >= state.tabs.len() {
            state.current_tab = 0;
        }

        // Invalidate the Topology tab's remembered host when the stashed
        // name is no longer present in the tab strip (e.g. the host
        // disconnected). The renderer will fall back to the first host
        // tab until the operator picks a new one.
        if let Some(last) = state.topology_last_host_tab.as_ref()
            && !state.tabs.iter().any(|t| t == last)
        {
            state.topology_last_host_tab = None;
        }
    }
}

#[async_trait]
impl DataCollectionStrategy for RemoteCollector {
    async fn collect(&self, config: &CollectionConfig) -> CollectionResult {
        if config.hosts.is_empty() {
            return Err(CollectionError::Other("No hosts configured".to_string()));
        }

        let remote = self
            .network_client
            .fetch_remote_metrics(&config.hosts, &self.semaphore)
            .await;

        let deduplicated_storage = Self::deduplicate_storage_info(remote.storage_info);

        Ok(CollectionData {
            gpu_info: remote.gpu_info,
            cpu_info: remote.cpu_info,
            memory_info: remote.memory_info,
            // Local `ProcessInfo` is only populated in local mode — the
            // remote path feeds `remote_process_info` instead (see
            // CollectionData docs).
            process_info: Vec::new(),
            storage_info: deduplicated_storage,
            chassis_info: remote.chassis_info,
            vgpu_info: remote.vgpu_info,
            mig_info: remote.mig_info,
            connection_statuses: remote.connection_statuses,
            remote_process_info: remote.process_info,
        })
    }

    async fn update_state(
        &self,
        app_state: Arc<Mutex<AppState>>,
        data: CollectionData,
        config: &CollectionConfig,
    ) {
        let mut state = app_state.lock().await;

        // Only update GPU info if we have valid data (not empty and has memory info)
        if !data.gpu_info.is_empty() && data.gpu_info.iter().any(|gpu| gpu.total_memory > 0) {
            state.gpu_info = data.gpu_info;
        } else if state.gpu_info.is_empty() {
            // If we don't have any existing GPU info and the new data is invalid,
            // still update to show something (but history won't be updated due to the check)
            state.gpu_info = data.gpu_info;
        }

        state.cpu_info = data.cpu_info;
        state.memory_info = data.memory_info;
        state.storage_info = data.storage_info;
        state.chassis_info = data.chassis_info;
        state.vgpu_info = data.vgpu_info;
        state.mig_info = data.mig_info;
        state.remote_process_info = data.remote_process_info;

        // Update connection status and maintain known hosts
        Self::update_connection_status(&mut state, data.connection_statuses, &config.hosts);

        // Update utilization history
        self.aggregator.update_utilization_history(&mut state);

        // Feed power samples into the energy integrator (issue #191).
        // In remote mode this exercises the same code path as local
        // mode — each host's energy accumulates under its own key.
        self.aggregator.update_energy_counters(&mut state);

        // Update tabs from all device hostnames (including disconnected ones)
        Self::update_remote_tabs(&mut state);

        state.process_info = Vec::new(); // No process info in remote mode
        state.loading = false;

        // Mark data as changed to trigger UI update AND invalidate
        // collector-keyed caches (e.g. Users-tab aggregation).
        state.mark_collector_data_changed();
    }

    fn strategy_type(&self) -> &str {
        "remote"
    }
}

pub struct RemoteCollectorBuilder {
    hosts: Vec<String>,
    max_connections: Option<usize>,
    timeouts: Option<ScrapeTimeouts>,
}

impl RemoteCollectorBuilder {
    pub fn new() -> Self {
        Self {
            hosts: Vec::new(),
            max_connections: None,
            timeouts: None,
        }
    }

    /// Hosts known at startup. Kept for API compatibility: the host list
    /// itself travels in `CollectionConfig` on every tick, and concurrency
    /// no longer depends on the startup count.
    pub fn with_hosts(mut self, hosts: Vec<String>) -> Self {
        self.hosts = hosts;
        self
    }

    #[allow(dead_code)]
    pub fn with_max_connections(mut self, max_connections: usize) -> Self {
        self.max_connections = Some(max_connections);
        self
    }

    /// Override the scrape time bounds (tests use short ones).
    #[allow(dead_code)]
    pub(crate) fn with_timeouts(mut self, timeouts: ScrapeTimeouts) -> Self {
        self.timeouts = Some(timeouts);
        self
    }

    pub fn build(self) -> RemoteCollector {
        let _ = self.hosts;
        let max_connections = self
            .max_connections
            .unwrap_or(AppConfig::MAX_CONCURRENT_CONNECTIONS);
        let client = match self.timeouts {
            Some(timeouts) => NetworkClient::with_timeouts(timeouts),
            None => NetworkClient::new(),
        };
        RemoteCollector::with_network_client(client, max_connections)
    }
}

impl Default for RemoteCollectorBuilder {
    fn default() -> Self {
        Self::new()
    }
}
