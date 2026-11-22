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
use super::data_collection::hostfile::{HostfileWatcher, merge_hosts};
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

            // Split-borrow through the guard so the alerter reads the GPU
            // rows in place instead of from a per-tick clone.
            transitions = {
                let AppState {
                    alerter, gpu_info, ..
                } = &mut *state;
                alerter.evaluate(gpu_info)
            };
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
            // The render loop reuses its copy of the alerter, the alert
            // history and the toasts until data_version moves, and this
            // runs after the collector already bumped it for the new data.
            if !transitions.is_empty() {
                state.mark_data_changed();
            }
        }

        if transitions.is_empty() {
            return;
        }
        self.notify_ui();

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

        // Local mode has no remote nodes, so the cadence comes from
        // `local_interval`. Resolved once: `args.interval` is fixed for the
        // process and the adaptive value is a constant per platform.
        let interval = args.interval.unwrap_or_else(EnvConfig::local_interval);

        loop {
            let mut config = CollectionConfig {
                interval,
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

            tokio::time::sleep(Duration::from_secs(interval)).await;
        }
    }

    pub async fn run_remote_mode(
        &self,
        args: ViewArgs,
        hosts: Vec<String>,
        hostfile: Option<String>,
    ) {
        // Load the hostfile once up front; a bad path is fatal, as before.
        // Later ticks go through the watcher, which re-reads the file only
        // when its modification time or length changed (issue #500).
        let mut watcher = match hostfile.as_deref() {
            Some(file_path) => match HostfileWatcher::open(file_path) {
                Ok((watcher, warnings)) => {
                    for warning in warnings {
                        tracing::warn!(hostfile = file_path, "{warning}");
                    }
                    Some(watcher)
                }
                Err(e) => {
                    eprintln!("Error loading hosts from file {file_path}: {e}");
                    return;
                }
            },
            None => None,
        };

        let initial_hosts = merge_hosts(&hosts, watcher.as_ref().map_or(&[], |w| w.hosts()));
        let collector = RemoteCollectorBuilder::new()
            .with_hosts(initial_hosts)
            .build();

        loop {
            // `--hosts` first, then hostfile hosts, de-duplicated by
            // endpoint identity so a host listed in both is scraped once.
            let hosts_list = match watcher.as_mut() {
                Some(watcher) => merge_hosts(&hosts, watcher.refresh().await),
                None => hosts.clone(),
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
                    // The TUI owns the terminal; stderr output would tear
                    // the frame.
                    tracing::warn!(error = %e, "remote collection failed");
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
