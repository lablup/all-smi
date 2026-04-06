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

use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, Notify};

use crate::app_state::AppState;
use crate::cli::ViewArgs;
use crate::common::config::EnvConfig;

// Re-export for backward compatibility
pub use super::data_collection::{
    CollectionConfig, DataCollectionStrategy, LocalCollector, RemoteCollectorBuilder,
};

pub struct DataCollector {
    app_state: Arc<Mutex<AppState>>,
    /// Optional notification handle to wake the UI loop when data changes.
    data_notify: Option<Arc<Notify>>,
}

impl DataCollector {
    #[allow(dead_code)] // Available for callers that do not need event-driven wakeups
    pub fn new(app_state: Arc<Mutex<AppState>>) -> Self {
        Self {
            app_state,
            data_notify: None,
        }
    }

    /// Create a data collector with a notification handle for event-driven UI wakeups.
    pub fn with_notify(app_state: Arc<Mutex<AppState>>, notify: Arc<Notify>) -> Self {
        Self {
            app_state,
            data_notify: Some(notify),
        }
    }

    /// Signal the UI loop that new data is available.
    fn notify_ui(&self) {
        if let Some(ref notify) = self.data_notify {
            notify.notify_one();
        }
    }

    pub async fn run_local_mode(&self, args: ViewArgs) {
        let mut profiler = crate::utils::StartupProfiler::new();
        profiler.checkpoint("Starting local mode data collection");

        let collector = LocalCollector::new();
        let mut first_iteration = true;

        loop {
            let mut config = CollectionConfig {
                interval: args
                    .interval
                    .unwrap_or_else(|| EnvConfig::adaptive_interval(1)),
                first_iteration,
                hosts: Vec::new(),
            };

            // Special handling for first iteration with app_state
            let data = if first_iteration {
                profiler.checkpoint("Starting first data collection");
                match collector
                    .collect_with_app_state(self.app_state.clone(), &config)
                    .await
                {
                    Ok(data) => {
                        profiler.checkpoint("First data collection complete");
                        profiler.finish();
                        data
                    }
                    Err(e) => {
                        eprintln!("Error collecting data: {e}");
                        tokio::time::sleep(Duration::from_secs(config.interval)).await;
                        continue;
                    }
                }
            } else {
                match collector.collect(&config).await {
                    Ok(data) => data,
                    Err(e) => {
                        eprintln!("Error collecting data: {e}");
                        tokio::time::sleep(Duration::from_secs(config.interval)).await;
                        continue;
                    }
                }
            };

            // Update state with collected data
            collector
                .update_state(self.app_state.clone(), data, &config)
                .await;
            self.notify_ui();

            if first_iteration {
                first_iteration = false;
                config.first_iteration = false;
            }

            // Use adaptive interval for local mode
            let interval = args
                .interval
                .unwrap_or_else(|| EnvConfig::adaptive_interval(1));
            tokio::time::sleep(Duration::from_secs(interval)).await;
        }
    }

    pub async fn run_remote_mode(
        &self,
        args: ViewArgs,
        mut hosts: Vec<String>,
        hostfile: Option<String>,
    ) {
        // Strip protocol prefix from command line hosts
        hosts = hosts
            .into_iter()
            .map(|host| {
                if let Some(stripped) = host.strip_prefix("http://") {
                    stripped.to_string()
                } else if let Some(stripped) = host.strip_prefix("https://") {
                    stripped.to_string()
                } else {
                    host
                }
            })
            .collect();

        // Load hosts from file if specified
        let mut builder = RemoteCollectorBuilder::new().with_hosts(hosts.clone());

        if let Some(ref file_path) = hostfile {
            match builder.load_hosts_from_file(file_path) {
                Ok(b) => builder = b,
                Err(e) => {
                    eprintln!("Error loading hosts from file {file_path}: {e}");
                    return;
                }
            }
        }

        let collector = builder.build();

        loop {
            // Get the current hosts from builder with validation
            let hosts_list = if let Some(file_path) = &hostfile {
                let mut hosts_vec = hosts.clone();

                // Validate file path
                match std::fs::metadata(file_path) {
                    Ok(metadata) => {
                        const MAX_FILE_SIZE: u64 = 10 * 1024 * 1024; // 10MB
                        if metadata.len() > MAX_FILE_SIZE {
                            eprintln!("Warning: Hostfile too large, skipping reload");
                            hosts_vec
                        } else if let Ok(content) = std::fs::read_to_string(file_path) {
                            const MAX_HOSTS: usize = 1000;
                            let file_hosts: Vec<String> = content
                                .lines()
                                .map(|s| s.trim())
                                .filter(|s| !s.is_empty())
                                .filter(|s| !s.starts_with('#'))
                                .take(MAX_HOSTS)
                                .filter_map(|s| {
                                    let host = if let Some(stripped) = s.strip_prefix("http://") {
                                        stripped.to_string()
                                    } else if let Some(stripped) = s.strip_prefix("https://") {
                                        stripped.to_string()
                                    } else {
                                        s.to_string()
                                    };

                                    // Basic host validation
                                    if host.chars().all(|c| {
                                        c.is_ascii() && (c.is_alphanumeric() || ".-:_".contains(c))
                                    }) {
                                        Some(host)
                                    } else {
                                        None
                                    }
                                })
                                .collect();
                            hosts_vec.extend(file_hosts);
                            hosts_vec
                        } else {
                            hosts_vec
                        }
                    }
                    Err(e) => {
                        eprintln!("Warning: Cannot access hostfile: {e}");
                        hosts_vec
                    }
                }
            } else {
                hosts.clone()
            };

            let config = CollectionConfig {
                interval: args
                    .interval
                    .unwrap_or_else(|| EnvConfig::adaptive_interval(hosts_list.len())),
                first_iteration: false,
                hosts: hosts_list.clone(),
            };

            match collector.collect(&config).await {
                Ok(data) => {
                    collector
                        .update_state(self.app_state.clone(), data, &config)
                        .await;
                    self.notify_ui();
                }
                Err(e) => {
                    eprintln!("Error collecting remote data: {e}");
                }
            }

            // Use adaptive interval for remote mode based on node count
            let interval = args
                .interval
                .unwrap_or_else(|| EnvConfig::adaptive_interval(hosts_list.len()));
            tokio::time::sleep(Duration::from_secs(interval)).await;
        }
    }
}
