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

use axum::{Router, routing::get};
use std::time::Duration;
use sysinfo::Disks;
use tokio::net::TcpListener;
use tokio::sync::RwLock;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[cfg(unix)]
use std::path::PathBuf;
#[cfg(unix)]
use tokio::net::UnixListener;

use crate::api::handlers::{SharedState, metrics_handler};
use crate::app_state::AppState;
use crate::cli::ApiArgs;
use crate::common::config_file::Settings;
use crate::device::{create_chassis_reader, get_cpu_readers, get_gpu_readers, get_memory_readers};
use crate::storage::info::StorageInfo;
use crate::utils::{filter_docker_aware_disks, get_hostname};

/// Get the default Unix domain socket path for the current platform.
/// - Linux: /var/run/all-smi.sock (fallback to /tmp/all-smi.sock if no permission)
/// - macOS: /tmp/all-smi.sock
#[cfg(unix)]
fn get_default_socket_path() -> PathBuf {
    #[cfg(target_os = "linux")]
    {
        let var_run_path = PathBuf::from("/var/run/all-smi.sock");
        // Check if we can write to /var/run
        if let Ok(metadata) = std::fs::metadata("/var/run")
            && metadata.is_dir()
        {
            // Try to create a test file to check write permission
            let test_path = PathBuf::from("/var/run/.all-smi-test");
            if std::fs::write(&test_path, b"").is_ok() {
                let _ = std::fs::remove_file(&test_path);
                return var_run_path;
            }
        }
        // Fallback to /tmp
        PathBuf::from("/tmp/all-smi.sock")
    }

    #[cfg(target_os = "macos")]
    {
        PathBuf::from("/tmp/all-smi.sock")
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        PathBuf::from("/tmp/all-smi.sock")
    }
}

/// Remove stale socket file if it exists.
/// This is necessary because Unix sockets leave files on disk that prevent rebinding.
/// Uses atomic remove to avoid TOCTOU race conditions.
#[cfg(unix)]
fn remove_stale_socket(path: &std::path::Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => {
            tracing::info!("Removed stale socket file: {}", path.display());
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // File doesn't exist, that's fine
            Ok(())
        }
        Err(e) => Err(e),
    }
}

/// Set restrictive permissions (0o600) on the socket file.
/// This ensures only the owner can connect to the socket.
#[cfg(unix)]
fn set_socket_permissions(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let permissions = std::fs::Permissions::from_mode(0o600);
    std::fs::set_permissions(path, permissions)
}

/// Run the API server with TCP and optionally Unix Domain Socket listeners.
pub async fn run_api_mode(args: &ApiArgs, settings: &Settings) {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "all_smi=debug,tower_http=debug".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    println!("Starting API mode...");
    let mut initial_state = AppState::new();
    // Apply the fully-merged energy configuration (file + env) so the
    // WAL path, cost toggle, etc. honour the user's config.toml.
    initial_state.energy_config = settings.energy.clone();
    // Replay any persisted energy WAL so Prometheus counters stay
    // monotonic across restarts (issue #191). Failures are logged
    // but do not block startup — the integrator simply begins at
    // zero on this host.
    if initial_state.energy_config.wal_enabled {
        let wal_path = initial_state.energy_config.wal_path.clone();
        match crate::metrics::energy_wal::replay_from_path(
            std::path::Path::new(&wal_path),
            initial_state.energy.integrator_mut(),
        ) {
            Ok(index) => {
                if !index.is_empty() {
                    tracing::info!(
                        "energy WAL: replayed {} records from {wal_path}",
                        index.len()
                    );
                }
                initial_state.energy_wal_replay = index;
            }
            Err(e) => {
                tracing::warn!("energy WAL: replay from {wal_path} failed: {e}");
            }
        }
    }
    let state = SharedState::new(RwLock::new(initial_state));
    let state_clone = state.clone();
    let processes = args.processes;
    // args.interval was resolved against settings in main.rs; fall back
    // defensively to 3 (compiled default) when the caller somehow
    // passed `None`.
    let interval = args.interval.unwrap_or(3);

    // Spawn the WAL flush task if enabled. The returned handle owns a
    // oneshot sender used by the Ctrl+C / SIGTERM path so the task can
    // perform a final `flush_and_fsync` before the process exits
    // (issue #191).
    let wal_flush_handle = {
        let state = state.clone();
        let state_read = state.read().await;
        let cfg = state_read.energy_config.clone();
        drop(state_read);
        if cfg.wal_enabled {
            Some(crate::metrics::energy_wal::spawn_wal_flush_task(
                state,
                cfg.wal_path.clone(),
                crate::metrics::energy_wal::DEFAULT_FLUSH_INTERVAL,
            ))
        } else {
            None
        }
    };

    // Spawn background task for collecting metrics
    tokio::spawn(async move {
        let gpu_readers = get_gpu_readers();
        let cpu_readers = get_cpu_readers();
        let memory_readers = get_memory_readers();
        let chassis_reader = create_chassis_reader();
        let mut disks = Disks::new_with_refreshed_list();
        loop {
            let all_gpu_info: Vec<_> = gpu_readers
                .iter()
                .flat_map(|reader| reader.get_gpu_info())
                .collect();

            let all_cpu_info = cpu_readers
                .iter()
                .flat_map(|reader| reader.get_cpu_info())
                .collect();

            let all_memory_info = memory_readers
                .iter()
                .flat_map(|reader| reader.get_memory_info())
                .collect();

            let all_processes = if processes {
                gpu_readers
                    .iter()
                    .flat_map(|reader| reader.get_process_info())
                    .collect()
            } else {
                Vec::new()
            };

            // Collect chassis-level info (DMI, thermals, power)
            let chassis_info: Vec<_> = chassis_reader
                .get_chassis_info()
                .into_iter()
                .map(|mut ci| {
                    // Aggregate GPU power into chassis total if not already set
                    if ci.total_power_watts.is_none() {
                        let total_gpu_power: f64 =
                            all_gpu_info.iter().map(|g| g.power_consumption).sum();
                        if total_gpu_power > 0.0 {
                            ci.total_power_watts = Some(total_gpu_power);
                        }
                    }
                    ci
                })
                .collect();

            // Refresh disk info in-place instead of creating a new Disks instance
            disks.refresh(true);
            let storage_info = collect_storage_info_from(&disks);

            let mut state = state_clone.write().await;
            state.gpu_info = all_gpu_info;
            state.cpu_info = all_cpu_info;
            state.memory_info = all_memory_info;
            state.process_info = all_processes;
            state.chassis_info = chassis_info;
            state.storage_info = storage_info;
            if state.loading {
                state.loading = false;
            }

            // Integrate power samples into the energy accountant so
            // `all_smi_energy_consumed_joules_total` reflects reality
            // in `api` mode (issue #191). The code mirrors
            // `view::data_collection::aggregator::update_energy_counters`
            // so both surfaces share the same integration contract.
            integrate_power_samples(&mut state);

            drop(state);
            tokio::time::sleep(Duration::from_secs(interval)).await;
        }
    });

    // Create the router with shared state
    let app = Router::new()
        .route("/metrics", get(metrics_handler))
        .with_state(state)
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers(Any),
        )
        .layer(TraceLayer::new_for_http());

    // Determine which listeners to start
    #[cfg(unix)]
    {
        let socket_path = args.socket.as_ref().map(|s| {
            if s.is_empty() {
                get_default_socket_path()
            } else {
                PathBuf::from(s)
            }
        });

        let port = args.port.unwrap_or(9090);
        match (port, socket_path) {
            // Both TCP and UDS (port > 0 with socket)
            (1..=u16::MAX, Some(path)) => {
                run_dual_listeners(app, port, path).await;
            }
            // UDS only (port == 0 with socket)
            (0, Some(path)) => {
                run_unix_listener(app, path).await;
            }
            // TCP only (port > 0, no socket)
            (1..=u16::MAX, None) => {
                run_tcp_listener(app, port).await;
            }
            // No listeners - error (port == 0, no socket)
            (0, None) => {
                tracing::error!(
                    "No listeners configured. Use --port or --socket to specify a listener."
                );
                eprintln!(
                    "Error: No listeners configured. Use --port or --socket to specify a listener."
                );
            }
        }
    }

    #[cfg(not(unix))]
    {
        run_tcp_listener(app, args.port.unwrap_or(9090)).await;
    }

    // Signal the WAL flush task to perform a final flush and fsync
    // before we exit, so the last batch of pending Joule deltas is not
    // lost (issue #191). Has to run AFTER the listeners return because
    // that is the moment Ctrl+C / SIGTERM has propagated through axum.
    if let Some(handle) = wal_flush_handle {
        handle.shutdown().await;
    }
}

/// Run only the TCP listener
async fn run_tcp_listener(app: Router, port: u16) {
    let listener = match TcpListener::bind(&format!("0.0.0.0:{port}")).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!("Failed to bind TCP listener on port {port}: {e}");
            eprintln!("Error: Failed to bind TCP listener on port {port}: {e}");
            return;
        }
    };
    tracing::info!(
        "API server listening on {}",
        listener
            .local_addr()
            .unwrap_or_else(|_| "unknown".parse().unwrap())
    );
    if let Err(e) = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
    {
        tracing::error!("TCP server error: {e}");
    }
}

/// Complete when the process receives Ctrl+C on any platform, or a
/// `SIGTERM` on Unix. Callers use this to let `axum::serve` return so
/// the parent function can run post-shutdown cleanup (energy WAL flush,
/// socket cleanup, etc.).
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(e) => {
                tracing::warn!("failed to install SIGTERM handler: {e}");
                // Fall back to ctrl_c only.
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
}

/// Run only the Unix Domain Socket listener
#[cfg(unix)]
async fn run_unix_listener(app: Router, path: PathBuf) {
    // Remove stale socket file if it exists
    if let Err(e) = remove_stale_socket(&path) {
        tracing::warn!("Failed to remove stale socket file: {e}");
    }

    // Create parent directory if it doesn't exist
    if let Some(parent) = path.parent()
        && !parent.exists()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        tracing::error!(
            "Failed to create socket directory {}: {e}",
            parent.display()
        );
        eprintln!(
            "Error: Failed to create socket directory {}: {e}",
            parent.display()
        );
        return;
    }

    let listener = match UnixListener::bind(&path) {
        Ok(l) => l,
        Err(e) => {
            tracing::error!("Failed to bind Unix socket at {}: {e}", path.display());
            eprintln!(
                "Error: Failed to bind Unix socket at {}: {e}",
                path.display()
            );
            return;
        }
    };

    // Set restrictive permissions (0o600) on the socket file
    if let Err(e) = set_socket_permissions(&path) {
        tracing::warn!("Failed to set socket permissions: {e}");
    }

    tracing::info!("API server listening on Unix socket: {}", path.display());

    // Serve the application with graceful shutdown so the caller can
    // run post-serve cleanup (WAL flush, socket cleanup) once we see a
    // SIGTERM / Ctrl+C.
    if let Err(e) = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
    {
        tracing::error!("Unix socket server error: {e}");
    }

    cleanup_socket(&path);
}

/// Run both TCP and Unix Domain Socket listeners simultaneously
#[cfg(unix)]
async fn run_dual_listeners(app: Router, port: u16, socket_path: PathBuf) {
    // Remove stale socket file if it exists
    if let Err(e) = remove_stale_socket(&socket_path) {
        tracing::warn!("Failed to remove stale socket file: {e}");
    }

    // Create parent directory if it doesn't exist
    if let Some(parent) = socket_path.parent()
        && !parent.exists()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        tracing::error!(
            "Failed to create socket directory {}: {e}",
            parent.display()
        );
        eprintln!(
            "Error: Failed to create socket directory {}: {e}",
            parent.display()
        );
        return;
    }

    // Create TCP listener
    let tcp_listener = match TcpListener::bind(&format!("0.0.0.0:{port}")).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!("Failed to bind TCP listener on port {port}: {e}");
            eprintln!("Error: Failed to bind TCP listener on port {port}: {e}");
            return;
        }
    };

    // Create Unix listener
    let unix_listener = match UnixListener::bind(&socket_path) {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(
                "Failed to bind Unix socket at {}: {e}",
                socket_path.display()
            );
            eprintln!(
                "Error: Failed to bind Unix socket at {}: {e}",
                socket_path.display()
            );
            return;
        }
    };

    // Set restrictive permissions (0o600) on the socket file
    if let Err(e) = set_socket_permissions(&socket_path) {
        tracing::warn!("Failed to set socket permissions: {e}");
    }

    tracing::info!(
        "API server listening on TCP {} and Unix socket {}",
        tcp_listener
            .local_addr()
            .unwrap_or_else(|_| "unknown".parse().unwrap()),
        socket_path.display()
    );

    // Clone the app for the second server
    let app_clone = app.clone();

    // Run both servers concurrently; each installs its own graceful
    // shutdown listener so the select returns on SIGTERM / Ctrl+C and
    // the caller can run post-serve cleanup.
    tokio::select! {
        result = axum::serve(tcp_listener, app)
            .with_graceful_shutdown(shutdown_signal()) => {
            if let Err(e) = result {
                tracing::error!("TCP server error: {e}");
            }
        }
        result = axum::serve(unix_listener, app_clone)
            .with_graceful_shutdown(shutdown_signal()) => {
            if let Err(e) = result {
                tracing::error!("Unix socket server error: {e}");
            }
        }
    }

    cleanup_socket(&socket_path);
}

/// Clean up the Unix domain socket file.
/// Uses atomic remove to avoid TOCTOU race conditions.
#[cfg(unix)]
fn cleanup_socket(path: &std::path::Path) {
    match std::fs::remove_file(path) {
        Ok(()) => {
            tracing::info!("Cleaned up socket file: {}", path.display());
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // File already removed, that's fine
        }
        Err(e) => {
            tracing::warn!("Failed to remove socket file on shutdown: {e}");
        }
    }
}

/// Integrate the current in-state power samples into the energy
/// accountant. Run once per collection cycle in `api` mode (issue
/// #191). On the first sample for each `(host, device)` pair, the
/// function consults `state.energy_wal_replay` to seed the lifetime
/// counter with any previously-recorded value so Prometheus stays
/// monotonic across restarts.
fn integrate_power_samples(state: &mut AppState) {
    use crate::metrics::energy::EnergyKey;
    use std::time::Instant;

    let now = Instant::now();

    // Collect (key, watts) pairs first so we do not hold an immutable
    // borrow over state.*_info while taking the mutable borrow on
    // state.energy.
    let mut samples: Vec<(EnergyKey, f64)> =
        Vec::with_capacity(state.gpu_info.len() + state.cpu_info.len() + state.chassis_info.len());
    for gpu in &state.gpu_info {
        samples.push((
            EnergyKey::gpu(gpu.hostname.clone(), gpu.uuid.clone()),
            gpu.power_consumption,
        ));
    }
    for cpu in &state.cpu_info {
        if let Some(power) = cpu.power_consumption {
            samples.push((EnergyKey::cpu(cpu.hostname.clone()), power));
        }
    }
    for chassis in &state.chassis_info {
        if let Some(power) = chassis.total_power_watts {
            samples.push((EnergyKey::chassis(chassis.hostname.clone()), power));
        }
    }

    let wal_index = &mut state.energy_wal_replay;
    let integrator = state.energy.integrator_mut();
    for (key, watts) in samples {
        if !integrator.has_samples(&key) && !wal_index.is_empty() {
            wal_index.seed_if_matches(&key, integrator);
        }
        integrator.record_sample(key, now, watts);
    }
}

/// Collect storage/disk information from a pre-existing Disks instance.
/// The caller is responsible for calling `refresh_list()` before this function.
fn collect_storage_info_from(disks: &Disks) -> Vec<StorageInfo> {
    let mut storage_info = Vec::new();
    let hostname = get_hostname();

    let mut filtered_disks = filter_docker_aware_disks(disks);
    filtered_disks.sort_by(|a, b| {
        a.mount_point()
            .to_string_lossy()
            .cmp(&b.mount_point().to_string_lossy())
    });

    for (index, disk) in filtered_disks.iter().enumerate() {
        let mount_point_str = disk.mount_point().to_string_lossy();
        storage_info.push(StorageInfo {
            mount_point: mount_point_str.to_string(),
            total_bytes: disk.total_space(),
            available_bytes: disk.available_space(),
            host_id: hostname.clone(),
            hostname: hostname.clone(),
            index: index as u32,
        });
    }

    storage_info
}
