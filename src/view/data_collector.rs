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
use tokio::sync::{Mutex, Notify, mpsc};

use crate::app_state::AppState;
use crate::cli::ViewArgs;
use crate::common::config::{AppConfig, EnvConfig};
use crate::network::webhook::{enqueue as enqueue_webhook, spawn_webhook_worker};
use crate::ui::alerts::WebhookPayload;
use crate::ui::notification::NotificationType;

// Re-export for backward compatibility
pub use super::data_collection::{
    CollectionConfig, DataCollectionStrategy, LocalCollector, RemoteCollectorBuilder, SshStrategy,
};

pub struct DataCollector {
    app_state: Arc<Mutex<AppState>>,
    /// Optional notification handle to wake the UI loop when data changes.
    data_notify: Option<Arc<Notify>>,
    /// Bounded webhook sender spun up lazily on first transition so that
    /// tests and no-webhook installs pay zero cost.
    webhook_tx: std::sync::OnceLock<mpsc::Sender<WebhookPayload>>,
}

impl DataCollector {
    #[allow(dead_code)] // Available for callers that do not need event-driven wakeups
    pub fn new(app_state: Arc<Mutex<AppState>>) -> Self {
        Self {
            app_state,
            data_notify: None,
            webhook_tx: std::sync::OnceLock::new(),
        }
    }

    /// Create a data collector with a notification handle for event-driven UI wakeups.
    pub fn with_notify(app_state: Arc<Mutex<AppState>>, notify: Arc<Notify>) -> Self {
        Self {
            app_state,
            data_notify: Some(notify),
            webhook_tx: std::sync::OnceLock::new(),
        }
    }

    /// Signal the UI loop that new data is available.
    fn notify_ui(&self) {
        if let Some(ref notify) = self.data_notify {
            notify.notify_one();
        }
    }

    /// Run the threshold alerter over the latest GPU snapshot.
    ///
    /// This is the bridge between the collector and the alert subsystem
    /// introduced in issue #186. Each transition is:
    /// 1. Pushed onto [`AppState::alert_history`] for the `A` panel.
    /// 2. Converted to a toast notification.
    /// 3. Optionally serialised and enqueued for the async webhook worker.
    /// 4. Audibly announced with a terminal bell when
    ///    [`AlertConfig::bell_on_critical`] is set and the transition
    ///    lands on `crit`.
    async fn evaluate_alerts(&self) {
        let transitions;
        let webhook_url;
        let bell_on_critical;
        {
            let mut state = self.app_state.lock().await;
            bell_on_critical = state.alerter.config().bell_on_critical;
            webhook_url = state.alerter.config().webhook_url.clone();

            let snapshot = state.gpu_info.clone();
            transitions = state.alerter.evaluate(&snapshot);
            for t in &transitions {
                let notification_type = match t.to {
                    crate::ui::alerts::AlertLevel::Crit => NotificationType::Error,
                    crate::ui::alerts::AlertLevel::Warn => NotificationType::Warning,
                    crate::ui::alerts::AlertLevel::Ok => NotificationType::Status,
                };
                let _ = state.notifications.show_with_duration(
                    t.message.clone(),
                    notification_type,
                    AppConfig::NOTIFICATION_DURATION_SECS,
                );
                state.push_alert_transition(t.clone());
            }
        }

        if transitions.is_empty() {
            return;
        }

        if bell_on_critical
            && transitions
                .iter()
                .any(|t| t.to == crate::ui::alerts::AlertLevel::Crit)
        {
            // Audible bell. The raw write bypasses the crossterm buffer
            // used by the UI loop so we offload it to the blocking pool:
            // a paused or very slow terminal would otherwise stall the
            // tokio executor on `write_all`/`flush`. BEL is a zero-width
            // control code so a stray byte interleaved into a frame is
            // visually harmless.
            tokio::task::spawn_blocking(|| {
                use std::io::Write;
                let mut out = std::io::stdout();
                let _ = out.write_all(b"\x07");
                let _ = out.flush();
            });
        }

        if !webhook_url.is_empty() {
            // NOTE (issue #192 config reload): OnceLock captures the URL
            // from the first evaluation tick. If AlertConfig.webhook_url
            // is later mutated via Alerter::set_config the worker keeps
            // pointing at the old URL. Config reload must rebuild the
            // DataCollector instead of mutating the URL in place. This
            // is intentional while the config-reload issue is pending.
            let tx = self
                .webhook_tx
                .get_or_init(|| spawn_webhook_worker(webhook_url.clone()));
            for t in &transitions {
                let payload = WebhookPayload::from(t);
                enqueue_webhook(tx, payload);
            }
        }

        self.notify_ui();
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
            // Run the threshold alerter against the freshly updated
            // gpu_info so transitions arrive on the same tick as the data
            // that produced them.
            self.evaluate_alerts().await;

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
                    self.evaluate_alerts().await;
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

    /// Drive the SSH transport (`view --ssh`, issue #194).
    ///
    /// The caller supplies a fully-constructed [`SshStrategy`]; this
    /// method owns the collection loop (polling + sleep), the
    /// `AppState` update, and the alert-evaluation wiring.  Separating
    /// strategy construction from the loop keeps the runner's CLI
    /// parsing simple and prevents a misconfigured `--ssh-*` flag from
    /// having to be revalidated on every tick.
    pub async fn run_ssh_mode(&self, args: ViewArgs, strategy: std::sync::Arc<SshStrategy>) {
        let target_count = strategy.target_count();
        loop {
            let config = CollectionConfig {
                interval: args
                    .interval
                    .unwrap_or_else(|| EnvConfig::adaptive_interval(target_count.max(1))),
                first_iteration: false,
                hosts: Vec::new(),
            };

            match strategy.collect(&config).await {
                Ok(data) => {
                    strategy
                        .update_state(self.app_state.clone(), data, &config)
                        .await;
                    self.notify_ui();
                    self.evaluate_alerts().await;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "ssh collect failed");
                }
            }

            let interval = args
                .interval
                .unwrap_or_else(|| EnvConfig::adaptive_interval(target_count.max(1)));
            tokio::time::sleep(Duration::from_secs(interval)).await;
        }
    }
}
