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

use axum::{routing::get, Router};
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

use crate::api::handlers::{metrics_handler, SharedState};
use crate::app_state::AppState;
use crate::cli::ApiArgs;
use crate::device::{get_cpu_readers, get_gpu_readers, get_memory_readers};
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
        if let Ok(metadata) = std::fs::metadata("/var/run") {
            if metadata.is_dir() {
                // Try to create a test file to check write permission
                let test_path = PathBuf::from("/var/run/.all-smi-test");
                if std::fs::write(&test_path, b"").is_ok() {
                    let _ = std::fs::remove_file(&test_path);
                    return var_run_path;
                }
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
pub async fn run_api_mode(args: &ApiArgs) {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "all_smi=debug,tower_http=debug".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    println!("Starting API mode...");
    let state = SharedState::new(RwLock::new(AppState::new()));
    let state_clone = state.clone();
    let processes = args.processes;
    let interval = args.interval;

    // Spawn background task for collecting metrics
    tokio::spawn(async move {
        let gpu_readers = get_gpu_readers();
        let cpu_readers = get_cpu_readers();
        let memory_readers = get_memory_readers();
        let mut disks = Disks::new_with_refreshed_list();
        loop {
            let all_gpu_info = gpu_readers
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

            // Refresh disk info in-place instead of creating a new Disks instance
            disks.refresh(true);
            let storage_info = collect_storage_info_from(&disks);

            let mut state = state_clone.write().await;
            state.gpu_info = all_gpu_info;
            state.cpu_info = all_cpu_info;
            state.memory_info = all_memory_info;
            state.process_info = all_processes;
            state.storage_info = storage_info;
            if state.loading {
                state.loading = false;
            }

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

        let port = args.port;
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
        run_tcp_listener(app, args.port).await;
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
    if let Err(e) = axum::serve(listener, app).await {
        tracing::error!("TCP server error: {e}");
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
    if let Some(parent) = path.parent() {
        if !parent.exists() {
            if let Err(e) = std::fs::create_dir_all(parent) {
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
        }
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

    // Set up socket cleanup on shutdown
    let path_clone = path.clone();
    let cleanup_handle = tokio::spawn(async move {
        tokio::signal::ctrl_c()
            .await
            .expect("Failed to listen for Ctrl+C");
        cleanup_socket(&path_clone);
    });

    // Serve the application
    if let Err(e) = axum::serve(listener, app).await {
        tracing::error!("Unix socket server error: {e}");
    }

    // Cancel cleanup handle and do cleanup
    cleanup_handle.abort();
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
    if let Some(parent) = socket_path.parent() {
        if !parent.exists() {
            if let Err(e) = std::fs::create_dir_all(parent) {
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
        }
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

    // Set up socket cleanup on shutdown
    let path_clone = socket_path.clone();
    let cleanup_handle = tokio::spawn(async move {
        tokio::signal::ctrl_c()
            .await
            .expect("Failed to listen for Ctrl+C");
        cleanup_socket(&path_clone);
    });

    // Run both servers concurrently
    tokio::select! {
        result = axum::serve(tcp_listener, app) => {
            if let Err(e) = result {
                tracing::error!("TCP server error: {e}");
            }
        }
        result = axum::serve(unix_listener, app_clone) => {
            if let Err(e) = result {
                tracing::error!("Unix socket server error: {e}");
            }
        }
    }

    // Cancel cleanup handle and do cleanup
    cleanup_handle.abort();
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
