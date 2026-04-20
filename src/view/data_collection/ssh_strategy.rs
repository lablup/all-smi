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

//! SSH-backed data-collection strategy for `view --ssh` (issue #194).
//!
//! Parallel to [`crate::view::data_collection::LocalCollector`] and
//! [`crate::view::data_collection::RemoteCollectorBuilder`] but sources
//! every frame over SSH. Per host, the strategy:
//!
//! 1. Opens a long-lived [`SshSession`] on first tick.
//! 2. Probes `all-smi --version`, `nvidia-smi --version`, and
//!    `rocm-smi --version` once to decide which transport to pin for
//!    the lifetime of the session.
//! 3. Every subsequent tick runs the pinned command, parses the output
//!    into [`GpuInfo`] records, and pushes them into the shared state.
//!
//! Connection errors downgrade a host's status to `disconnected` /
//! `auth-failed` / `timeout` but never crash the view loop — the
//! per-host state is the only casualty.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use tokio::sync::{Mutex, Semaphore};

use crate::app_state::{AppState, ConnectionStatus};
use crate::device::GpuInfo;
use crate::network::nvidia_smi_shim::{NVIDIA_SMI_COMMAND, parse_nvidia_smi_csv};
use crate::network::rocm_smi_shim::{ROCM_SMI_COMMAND, parse_rocm_smi_json};
use crate::network::ssh_client::{ConnectParams, ExecOutput, HostKeyVerifier, SshSession};
use crate::network::ssh_decision::{
    MIN_NATIVE_VERSION, ProbeOutcomes, ProbeResult, native_supported, select_transport,
};
use crate::network::ssh_host_key::build_verifier;
use crate::network::ssh_target::SshTarget;
use crate::network::ssh_transport::{SshFallbackPolicy, SshTransport, StrictHostKey};
use crate::snapshot::options::Snapshot;

use super::aggregator::DataAggregator;
use super::strategy::{
    CollectionConfig, CollectionData, CollectionError, CollectionResult, DataCollectionStrategy,
};

const NATIVE_SNAPSHOT_CMD: &str = "all-smi snapshot --format json --include gpu,cpu,memory,chassis";
const NATIVE_VERSION_CMD: &str = "all-smi --version";

/// Configuration a caller feeds the SSH strategy once, at startup.
#[derive(Clone)]
pub struct SshStrategyConfig {
    pub targets: Vec<SshTarget>,
    pub explicit_key: Option<PathBuf>,
    pub strict_host_key: StrictHostKey,
    pub known_hosts: Option<PathBuf>,
    pub connect_timeout: Duration,
    pub fallback_policy: SshFallbackPolicy,
    pub concurrency: usize,
}

/// Per-host state tracked by the strategy for the lifetime of the view.
#[derive(Clone)]
struct HostState {
    target: SshTarget,
    session: Option<SshSession>,
    transport: SshTransport,
    last_error: Option<String>,
    /// True after we have successfully authenticated at least once.
    ever_connected: bool,
}

/// SSH-backed strategy implementing [`DataCollectionStrategy`].
pub struct SshStrategy {
    config: SshStrategyConfig,
    hosts: Arc<Mutex<HashMap<String, HostState>>>,
    semaphore: Arc<Semaphore>,
    aggregator: DataAggregator,
    verifier: Arc<dyn HostKeyVerifier>,
}

impl SshStrategy {
    pub fn new(config: SshStrategyConfig) -> Self {
        let concurrency = config.concurrency.max(1);
        let verifier = build_verifier(config.strict_host_key, config.known_hosts.clone());
        let hosts: HashMap<String, HostState> = config
            .targets
            .iter()
            .map(|t| {
                (
                    t.host_id(),
                    HostState {
                        target: t.clone(),
                        session: None,
                        transport: SshTransport::Unsupported,
                        last_error: None,
                        ever_connected: false,
                    },
                )
            })
            .collect();
        Self {
            config,
            hosts: Arc::new(Mutex::new(hosts)),
            semaphore: Arc::new(Semaphore::new(concurrency)),
            aggregator: DataAggregator::new(),
            verifier,
        }
    }

    /// Total target count — exposed for the runner to size the adaptive
    /// interval against.
    pub fn target_count(&self) -> usize {
        self.config.targets.len()
    }

    /// Drive one collection tick across every configured SSH target.
    async fn collect_all(&self) -> CollectionData {
        let mut gpu_info = Vec::new();
        let mut connection_statuses = Vec::new();

        // Snapshot the current host map so we don't hold the lock while
        // awaiting per-host futures.
        let targets: Vec<SshTarget> = {
            let hosts = self.hosts.lock().await;
            hosts.values().map(|h| h.target.clone()).collect()
        };

        let mut handles = Vec::with_capacity(targets.len());
        for target in targets {
            let hosts = Arc::clone(&self.hosts);
            let semaphore = Arc::clone(&self.semaphore);
            let verifier = Arc::clone(&self.verifier);
            let config = self.config.clone();
            handles.push(tokio::spawn(async move {
                collect_one_host(hosts, semaphore, verifier, config, target).await
            }));
        }

        for handle in handles {
            match handle.await {
                Ok(Ok((gpus, status))) => {
                    gpu_info.extend(gpus);
                    connection_statuses.push(status);
                }
                Ok(Err((status,))) => {
                    connection_statuses.push(status);
                }
                Err(join_err) => {
                    tracing::warn!(error = %join_err, "ssh host task panicked");
                }
            }
        }

        CollectionData {
            gpu_info,
            cpu_info: Vec::new(),
            memory_info: Vec::new(),
            process_info: Vec::new(),
            storage_info: Vec::new(),
            chassis_info: Vec::new(),
            vgpu_info: Vec::new(),
            mig_info: Vec::new(),
            connection_statuses,
            remote_process_info: Vec::new(),
        }
    }
}

#[async_trait]
impl DataCollectionStrategy for SshStrategy {
    async fn collect(&self, _config: &CollectionConfig) -> CollectionResult {
        if self.config.targets.is_empty() {
            return Err(CollectionError::Other("No SSH targets configured".into()));
        }
        Ok(self.collect_all().await)
    }

    async fn update_state(
        &self,
        app_state: Arc<Mutex<AppState>>,
        data: CollectionData,
        _config: &CollectionConfig,
    ) {
        let mut state = app_state.lock().await;

        // Refresh tabs / known_hosts from the current SSH targets so
        // the UI always shows every configured host even when one of
        // them has not yet connected.
        if state.known_hosts.is_empty() {
            state.known_hosts = self.config.targets.iter().map(|t| t.host_id()).collect();
        }

        // Replace GPU info wholesale. SSH frames are complete each
        // tick — there is no "partial update" semantics to honour.
        state.gpu_info = data.gpu_info;
        state.cpu_info = data.cpu_info;
        state.memory_info = data.memory_info;
        state.chassis_info = data.chassis_info;
        state.vgpu_info = data.vgpu_info;
        state.mig_info = data.mig_info;
        state.storage_info = data.storage_info;
        state.remote_process_info = Vec::new();
        state.process_info = Vec::new();

        // Update connection status per host.
        state.hostname_to_host_id.clear();
        for status in &data.connection_statuses {
            if let Some(actual_hostname) = &status.actual_hostname {
                state
                    .hostname_to_host_id
                    .insert(actual_hostname.clone(), status.host_id.clone());
            }
        }
        for status in data.connection_statuses {
            state
                .connection_status
                .insert(status.host_id.clone(), status);
        }

        // Rebuild the tab strip to match the current target list, using
        // the same layout the RemoteCollector produces so the renderer
        // need not special-case SSH mode.
        let mut tabs = vec![
            "All".to_string(),
            crate::ui::tabs::USERS_TAB_NAME.to_string(),
            crate::ui::tabs::TOPOLOGY_TAB_NAME.to_string(),
        ];
        tabs.extend(state.known_hosts.clone());
        let previous_name = state.tabs.get(state.current_tab).cloned();
        state.tabs = tabs;
        if let Some(name) = previous_name
            && let Some(idx) = state.tabs.iter().position(|t| *t == name)
        {
            state.current_tab = idx;
        } else if state.current_tab >= state.tabs.len() {
            state.current_tab = 0;
        }

        self.aggregator.update_utilization_history(&mut state);
        self.aggregator.update_energy_counters(&mut state);

        state.loading = false;
        state.mark_collector_data_changed();
    }

    fn strategy_type(&self) -> &str {
        "ssh"
    }

    async fn is_ready(&self) -> bool {
        true
    }
}

/// Per-host collection driver. Returns `Ok((gpus, status))` on success
/// (status `is_connected = true`); returns `Err((status,))` when the
/// host failed — the status carries the error chip the UI renders.
async fn collect_one_host(
    hosts: Arc<Mutex<HashMap<String, HostState>>>,
    semaphore: Arc<Semaphore>,
    verifier: Arc<dyn HostKeyVerifier>,
    config: SshStrategyConfig,
    target: SshTarget,
) -> Result<(Vec<GpuInfo>, ConnectionStatus), (ConnectionStatus,)> {
    let host_id = target.host_id();
    let mut status = ConnectionStatus::new(host_id.clone(), target.display_label());
    status.actual_hostname = Some(target.host.clone());
    status.connection_state = Some("connecting".to_string());

    // Does this host already have a session? If so, reuse it.
    let existing = {
        let hosts_guard = hosts.lock().await;
        hosts_guard
            .get(&host_id)
            .and_then(|h| h.session.clone().map(|s| (s, h.transport)))
    };

    let (session, transport) = match existing {
        Some((session, transport)) => (session, transport),
        None => {
            let _permit = match semaphore.acquire().await {
                Ok(p) => p,
                Err(_) => {
                    status.mark_failure("ssh concurrency semaphore closed".into());
                    return Err((status,));
                }
            };
            // Re-check after acquiring the permit in case another task
            // filled the session while we were waiting.
            {
                let hosts_guard = hosts.lock().await;
                if let Some(h) = hosts_guard.get(&host_id)
                    && let Some(s) = h.session.clone()
                {
                    drop(hosts_guard);
                    let transport = lookup_transport(&hosts, &host_id).await;
                    return exec_and_parse(&hosts, &target, s, transport, status).await;
                }
            }

            let params = ConnectParams {
                target: target.clone(),
                explicit_key: config.explicit_key.clone(),
                strict_host_key: config.strict_host_key,
                connect_timeout: config.connect_timeout,
                inactivity: crate::network::ssh_client::DEFAULT_INACTIVITY,
                known_hosts: config.known_hosts.clone(),
            };

            match SshSession::connect(params, Arc::clone(&verifier)).await {
                Ok(session) => {
                    let transport = probe_transport(&session, &config.fallback_policy).await;
                    let mut hosts_guard = hosts.lock().await;
                    if let Some(h) = hosts_guard.get_mut(&host_id) {
                        h.session = Some(session.clone());
                        h.transport = transport;
                        h.last_error = None;
                        h.ever_connected = true;
                    }
                    (session, transport)
                }
                Err(e) => {
                    let label = e.ui_label();
                    let mut hosts_guard = hosts.lock().await;
                    if let Some(h) = hosts_guard.get_mut(&host_id) {
                        h.last_error = Some(e.to_string());
                    }
                    status.mark_failure(format!("{label}: {e}"));
                    return Err((status,));
                }
            }
        }
    };

    exec_and_parse(&hosts, &target, session, transport, status).await
}

async fn lookup_transport(
    hosts: &Arc<Mutex<HashMap<String, HostState>>>,
    host_id: &str,
) -> SshTransport {
    let hosts_guard = hosts.lock().await;
    hosts_guard
        .get(host_id)
        .map(|h| h.transport)
        .unwrap_or(SshTransport::Unsupported)
}

/// Run `all-smi --version` + optional shim `--version` probes over the
/// already-open SSH session and pick the best transport per the
/// decision tree.
async fn probe_transport(session: &SshSession, policy: &SshFallbackPolicy) -> SshTransport {
    let mut outcomes = ProbeOutcomes::default();

    // Native probe — bounded to 2 seconds.
    match session
        .exec_with_timeout(NATIVE_VERSION_CMD, Duration::from_secs(2))
        .await
    {
        Ok(out) if out.is_success() => {
            outcomes.native = if native_supported(out.stdout.lines().next().unwrap_or("")) {
                ProbeResult::Available
            } else {
                tracing::info!(
                    min_major = MIN_NATIVE_VERSION.0,
                    min_minor = MIN_NATIVE_VERSION.1,
                    "remote all-smi version too old; falling back to shim"
                );
                ProbeResult::NotAvailable
            };
        }
        _ => {
            outcomes.native = ProbeResult::NotAvailable;
        }
    }

    if outcomes.native == ProbeResult::Available {
        return SshTransport::Native;
    }

    if policy.try_nvidia_smi {
        outcomes.nvidia_smi = probe_simple(session, "nvidia-smi --version").await;
    }
    if policy.try_rocm_smi {
        outcomes.rocm_smi = probe_simple(session, "rocm-smi --version").await;
    }

    select_transport(&outcomes, policy)
}

async fn probe_simple(session: &SshSession, cmd: &str) -> ProbeResult {
    match session.exec_with_timeout(cmd, Duration::from_secs(2)).await {
        Ok(out) if out.is_success() && !out.stdout.trim().is_empty() => ProbeResult::Available,
        Ok(_) => ProbeResult::NotAvailable,
        Err(_) => ProbeResult::NotAvailable,
    }
}

async fn exec_and_parse(
    hosts: &Arc<Mutex<HashMap<String, HostState>>>,
    target: &SshTarget,
    session: SshSession,
    transport: SshTransport,
    mut status: ConnectionStatus,
) -> Result<(Vec<GpuInfo>, ConnectionStatus), (ConnectionStatus,)> {
    status.transport_chip = Some(transport.chip_label().to_string());
    let timestamp = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

    let cmd = match transport {
        SshTransport::Native => NATIVE_SNAPSHOT_CMD,
        SshTransport::NvidiaSmi => NVIDIA_SMI_COMMAND,
        SshTransport::RocmSmi => ROCM_SMI_COMMAND,
        SshTransport::Unsupported => {
            status.mark_success();
            status.last_error = Some("unsupported: no all-smi, nvidia-smi, or rocm-smi".into());
            return Ok((Vec::new(), status));
        }
    };

    let output = match session.exec(cmd).await {
        Ok(o) => o,
        Err(e) => {
            // Clear the cached session so the next tick will
            // reconnect rather than keep poking at a dead socket.
            clear_cached_session(hosts, &target.host_id()).await;
            status.mark_failure(format!("{}: {e}", e.ui_label()));
            return Err((status,));
        }
    };

    if !output.is_success() {
        // A non-zero exit is most often a local issue (missing binary,
        // wrong sudo, etc.) — do NOT invalidate the session for that.
        status.mark_failure(format!(
            "exit {:?}: {}",
            output.exit_status,
            truncate(&output.stderr, 240)
        ));
        return Err((status,));
    }

    let host_id = target.host_id();
    let hostname = target.hostname();
    let gpus = match transport {
        SshTransport::Native => parse_native_snapshot(&output, &host_id, hostname, &timestamp),
        SshTransport::NvidiaSmi => {
            parse_nvidia_smi_csv(&output.stdout, &host_id, hostname, &timestamp).map_err(|e| {
                tracing::warn!(host = %hostname, error = %e, "nvidia-smi parse failure");
                format!("nvidia-smi parse: {e}")
            })
        }
        SshTransport::RocmSmi => {
            parse_rocm_smi_json(&output.stdout, &host_id, hostname, &timestamp).map_err(|e| {
                tracing::warn!(host = %hostname, error = %e, "rocm-smi parse failure");
                format!("rocm-smi parse: {e}")
            })
        }
        SshTransport::Unsupported => Ok(Vec::new()),
    };

    match gpus {
        Ok(gpus) => {
            status.mark_success();
            Ok((gpus, status))
        }
        Err(e) => {
            status.mark_failure(e);
            Err((status,))
        }
    }
}

/// Parse the native `all-smi snapshot --format json` output. The
/// snapshot sets hostnames / host_ids per the remote machine, but we
/// re-label them to the SSH host identifier so the tab routing stays
/// consistent.
fn parse_native_snapshot(
    output: &ExecOutput,
    host_id: &str,
    hostname: &str,
    timestamp: &str,
) -> Result<Vec<GpuInfo>, String> {
    let snap: Snapshot =
        serde_json::from_str(&output.stdout).map_err(|e| format!("native snapshot JSON: {e}"))?;
    let mut gpus = snap.gpus.unwrap_or_default();
    for gpu in &mut gpus {
        gpu.host_id = host_id.to_string();
        gpu.hostname = hostname.to_string();
        gpu.time = timestamp.to_string();
        gpu.detail
            .insert("transport".to_string(), "ssh/native".to_string());
    }
    Ok(gpus)
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    // Slice on a char boundary so multi-byte stderr (e.g. remotes that
    // emit non-ASCII error messages) cannot panic the view loop.
    let mut end = 0;
    for (idx, _) in s.char_indices() {
        if idx > max {
            break;
        }
        end = idx;
    }
    let mut out = s[..end].to_string();
    out.push('…');
    out
}

/// Clear the cached [`SshSession`] for `host_id`. Called after an exec
/// that failed with a transport-level error so the next tick opens a
/// fresh connection rather than retrying a dead session.
async fn clear_cached_session(hosts: &Arc<Mutex<HashMap<String, HostState>>>, host_id: &str) {
    let mut hosts_guard = hosts.lock().await;
    if let Some(h) = hosts_guard.get_mut(host_id) {
        h.session = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(user: &str, host: &str, port: u16) -> SshTarget {
        SshTarget {
            user: user.to_string(),
            host: host.to_string(),
            port,
        }
    }

    #[test]
    fn strategy_new_stores_targets_unchanged() {
        let cfg = SshStrategyConfig {
            targets: vec![target("a", "h1", 22), target("b", "h2", 2222)],
            explicit_key: None,
            strict_host_key: StrictHostKey::Yes,
            known_hosts: None,
            connect_timeout: Duration::from_secs(5),
            fallback_policy: SshFallbackPolicy::default(),
            concurrency: 16,
        };
        let strat = SshStrategy::new(cfg);
        assert_eq!(strat.target_count(), 2);
    }

    #[test]
    fn truncate_shortens_long_strings() {
        let long = "x".repeat(500);
        let out = truncate(&long, 10);
        assert!(out.len() <= 20);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn truncate_preserves_short_strings() {
        assert_eq!(truncate("abc", 10), "abc");
    }

    #[test]
    fn truncate_does_not_panic_on_multibyte_boundary() {
        // Each `é` is two bytes in UTF-8.  Requesting a cut at a
        // byte index that lands in the middle of a character used to
        // panic; the fix walks `char_indices` so the cut always lands
        // on a valid boundary.
        let s = "éééééééé";
        let out = truncate(s, 3);
        assert!(out.ends_with('…'));
        // Make sure the truncated portion is valid UTF-8.
        assert!(out.is_char_boundary(out.len()));
    }

    #[tokio::test]
    async fn collect_empty_targets_errors() {
        let cfg = SshStrategyConfig {
            targets: Vec::new(),
            explicit_key: None,
            strict_host_key: StrictHostKey::Yes,
            known_hosts: None,
            connect_timeout: Duration::from_secs(5),
            fallback_policy: SshFallbackPolicy::default(),
            concurrency: 4,
        };
        let strat = SshStrategy::new(cfg);
        let result = strat.collect(&CollectionConfig::default()).await;
        match result {
            Err(CollectionError::Other(s)) => {
                assert!(s.contains("No SSH targets"));
            }
            Err(other) => panic!("expected Other err, got {other:?}"),
            Ok(_) => panic!("expected err, got Ok"),
        }
    }
}
