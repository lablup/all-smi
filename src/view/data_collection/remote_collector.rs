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
use regex::{Regex, RegexBuilder};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::app_state::{AppState, ConnectionStatus};
use crate::common::config::EnvConfig;
use crate::network::NetworkClient;
use crate::storage::info::StorageInfo;

use super::aggregator::DataAggregator;
use super::strategy::{
    CollectionConfig, CollectionData, CollectionError, CollectionResult, DataCollectionStrategy,
};

/// Extract hostname from URL, handling both simple hostnames and full URLs
fn extract_hostname_from_url(url: &str) -> String {
    // Handle full URLs like "http://remote1:9090"
    if url.starts_with("http://") || url.starts_with("https://") {
        if let Some(start) = url.find("://") {
            let after_protocol = &url[start + 3..];
            if let Some(end) = after_protocol.find('/') {
                after_protocol[..end].to_string()
            } else {
                after_protocol.to_string()
            }
        } else {
            url.to_string()
        }
    } else {
        // Handle simple hostname:port format
        url.to_string()
    }
}

/// Extract the full host:port combination as unique identifier
fn extract_host_identifier(url: &str) -> String {
    extract_hostname_from_url(url)
}

pub struct RemoteCollector {
    network_client: NetworkClient,
    semaphore: Arc<tokio::sync::Semaphore>,
    regex: Regex,
    aggregator: DataAggregator,
}

impl RemoteCollector {
    pub fn new(max_connections: usize) -> Self {
        // Use simpler quantifiers to avoid DFA explosion
        // The + quantifier is much more efficient than bounded quantifiers
        let regex = RegexBuilder::new(r"^all_smi_([^\{]+)\{([^}]+)\} ([\d\.]+)$")
            .size_limit(10_485_760) // 10MB size limit for DFA (increased for safety)
            .dfa_size_limit(10_485_760) // 10MB DFA limit
            .build()
            .expect("Failed to compile metrics regex");

        Self {
            network_client: NetworkClient::new(),
            semaphore: Arc::new(tokio::sync::Semaphore::new(max_connections)),
            regex,
            aggregator: DataAggregator::new(),
        }
    }

    #[allow(dead_code)]
    pub fn with_hosts(hosts: Vec<String>) -> Self {
        let max_connections = EnvConfig::max_concurrent_connections(hosts.len());
        Self::new(max_connections)
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
        // Initialize known hosts if not already set
        if state.known_hosts.is_empty() {
            state.known_hosts = hosts.iter().map(|h| extract_host_identifier(h)).collect();
        }

        // Clear the reverse lookup map before rebuilding it
        state.hostname_to_host_id.clear();

        // Update connection status for each received status
        for mut status in connection_statuses {
            // Preserve actual_hostname from previous successful connection if current doesn't have it
            if status.actual_hostname.is_none()
                && let Some(existing_status) = state.connection_status.get(&status.host_id)
                && let Some(existing_hostname) = &existing_status.actual_hostname
            {
                status.actual_hostname = Some(existing_hostname.clone());
            }

            // Update the reverse lookup map if we have an actual hostname
            if let Some(actual_hostname) = &status.actual_hostname {
                state
                    .hostname_to_host_id
                    .insert(actual_hostname.clone(), status.host_id.clone());
            }

            state
                .connection_status
                .insert(status.host_id.clone(), status);
        }

        // For hosts that didn't return a status (e.g., Ok(None) or Err cases),
        // mark them as failed if we don't have recent status
        for host in hosts {
            let host_id = extract_host_identifier(host);
            state
                .connection_status
                .entry(host_id.clone())
                .or_insert_with(|| {
                    let mut status = ConnectionStatus::new(host_id, host.clone());
                    status.mark_failure("No response received".to_string());
                    status
                });
        }
    }

    fn update_remote_tabs(state: &mut AppState) {
        // Always create "All" tab for consistent UI behavior
        let mut tabs = vec!["All".to_string()];
        tabs.extend(state.known_hosts.clone());

        state.tabs = tabs;
    }
}

#[async_trait]
impl DataCollectionStrategy for RemoteCollector {
    async fn collect(&self, config: &CollectionConfig) -> CollectionResult {
        if config.hosts.is_empty() {
            return Err(CollectionError::Other("No hosts configured".to_string()));
        }

        let (
            gpu_info,
            cpu_info,
            memory_info,
            storage_info,
            vgpu_info,
            mig_info,
            connection_statuses,
        ) = self
            .network_client
            .fetch_remote_data(&config.hosts, &self.semaphore, &self.regex)
            .await;

        let deduplicated_storage = Self::deduplicate_storage_info(storage_info);

        Ok(CollectionData {
            gpu_info,
            cpu_info,
            memory_info,
            process_info: Vec::new(), // No process info in remote mode
            storage_info: deduplicated_storage,
            chassis_info: Vec::new(), // TODO: Parse chassis info from remote metrics
            vgpu_info,
            mig_info,
            connection_statuses,
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
        state.vgpu_info = data.vgpu_info;
        state.mig_info = data.mig_info;

        // Update connection status and maintain known hosts
        Self::update_connection_status(&mut state, data.connection_statuses, &config.hosts);

        // Update utilization history
        self.aggregator.update_utilization_history(&mut state);

        // Update tabs from all device hostnames (including disconnected ones)
        Self::update_remote_tabs(&mut state);

        state.process_info = Vec::new(); // No process info in remote mode
        state.loading = false;

        // Mark data as changed to trigger UI update
        state.mark_data_changed();
    }

    fn strategy_type(&self) -> &str {
        "remote"
    }
}

pub struct RemoteCollectorBuilder {
    hosts: Vec<String>,
    max_connections: Option<usize>,
}

impl RemoteCollectorBuilder {
    pub fn new() -> Self {
        Self {
            hosts: Vec::new(),
            max_connections: None,
        }
    }

    pub fn with_hosts(mut self, hosts: Vec<String>) -> Self {
        self.hosts = hosts;
        self
    }

    #[allow(dead_code)]
    pub fn with_max_connections(mut self, max_connections: usize) -> Self {
        self.max_connections = Some(max_connections);
        self
    }

    pub fn load_hosts_from_file(mut self, file_path: &str) -> Result<Self, std::io::Error> {
        use std::path::Path;

        // Sanitize and validate file path
        let path = Path::new(file_path);

        // Resolve to absolute path and check it exists
        let canonical_path = path.canonicalize().map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("Invalid hostfile path: {e}"),
            )
        })?;

        // Ensure it's a file, not a directory
        if !canonical_path.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Hostfile path is not a regular file",
            ));
        }

        // Check file size to prevent loading huge files
        let metadata = std::fs::metadata(&canonical_path)?;
        const MAX_FILE_SIZE: u64 = 10 * 1024 * 1024; // 10MB max
        if metadata.len() > MAX_FILE_SIZE {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "Hostfile too large: {} bytes (max: {MAX_FILE_SIZE} bytes)",
                    metadata.len()
                ),
            ));
        }

        let content = std::fs::read_to_string(&canonical_path)?;

        // Limit number of hosts to prevent memory exhaustion
        const MAX_HOSTS: usize = 1000;
        let mut host_count = 0;

        let file_hosts: Vec<String> = content
            .lines()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .filter(|s| !s.starts_with('#'))
            .take(MAX_HOSTS)
            .filter_map(|s| {
                host_count += 1;
                if host_count > MAX_HOSTS {
                    eprintln!("Warning: Hostfile contains more than {MAX_HOSTS} hosts, truncating");
                    return None;
                }

                // Validate host format (basic validation)
                let host = if let Some(stripped) = s.strip_prefix("http://") {
                    stripped.to_string()
                } else if let Some(stripped) = s.strip_prefix("https://") {
                    stripped.to_string()
                } else {
                    s.to_string()
                };

                // Basic validation: must contain valid characters
                if host
                    .chars()
                    .all(|c| c.is_ascii() && (c.is_alphanumeric() || ".-:_".contains(c)))
                {
                    Some(host)
                } else {
                    eprintln!("Warning: Invalid host format skipped: {s}");
                    None
                }
            })
            .collect();

        self.hosts.extend(file_hosts);
        Ok(self)
    }

    pub fn build(self) -> RemoteCollector {
        let max_connections = self
            .max_connections
            .unwrap_or_else(|| EnvConfig::max_concurrent_connections(self.hosts.len()));

        RemoteCollector::new(max_connections)
    }
}

impl Default for RemoteCollectorBuilder {
    fn default() -> Self {
        Self::new()
    }
}
