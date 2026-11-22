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

//! Regression coverage for HTTP remote-view host identity and chassis energy.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::{Router, routing::get};
use tokio::sync::Mutex;

use crate::app_state::AppState;
use crate::cli::ViewArgs;
use crate::common::config::EnergyConfig;
use crate::metrics::energy::EnergyKey;
use crate::network::metrics_parser::MetricsParser;
use crate::view::data_collection::remote_collector::RemoteCollectorBuilder;
use crate::view::data_collection::strategy::{CollectionConfig, DataCollectionStrategy};
use crate::view::frame_renderer::FrameRenderer;
use crate::view::render_snapshot::{HeavyCache, RenderSnapshot};
use crate::view::view_cache::ViewCache;

const METRICS: &str = r#"
all_smi_gpu_info{gpu="Test GPU",instance="node-a",gpu_uuid="GPU-A",gpu_index="0",type="GPU"} 1
all_smi_gpu_memory_total_bytes{gpu="Test GPU",instance="node-a",gpu_uuid="GPU-A",gpu_index="0"} 17179869184
all_smi_gpu_memory_used_bytes{gpu="Test GPU",instance="node-a",gpu_uuid="GPU-A",gpu_index="0"} 4294967296
all_smi_gpu_utilization{gpu="Test GPU",instance="node-a",gpu_uuid="GPU-A",gpu_index="0"} 42
all_smi_gpu_power_consumption_watts{gpu="Test GPU",instance="node-a",gpu_uuid="GPU-A",gpu_index="0"} 180
all_smi_chassis_info{hostname="node-a",instance="node-a",product_name="Rack Node",vendor="Lablup",board="Board A",version="1",bios_version="2",platform="linux"} 1
all_smi_chassis_power_watts{hostname="node-a",instance="node-a"} 360
all_smi_chassis_cpu_power_watts{hostname="node-a",instance="node-a"} 120
all_smi_chassis_gpu_power_watts{hostname="node-a",instance="node-a"} 220
all_smi_chassis_inlet_temperature_celsius{hostname="node-a",instance="node-a"} 21.5
all_smi_chassis_outlet_temperature_celsius{hostname="node-a",instance="node-a"} 31.5
all_smi_chassis_fan_speed_rpm{hostname="node-a",instance="node-a",fan_id="0",fan_name="Front"} 4200
all_smi_energy_consumed_joules_total{host="node-a",scope="chassis"} 900000
"#;

async fn spawn_metrics_server() -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind metrics server");
    let addr = listener.local_addr().expect("metrics server address");
    let router = Router::new().route("/metrics", get(|| async { METRICS }));
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    addr
}

#[test]
fn chassis_metrics_round_trip_into_remote_model() {
    let parsed = MetricsParser::new().parse_metrics_with_chassis(METRICS, "node-a:9090");

    assert_eq!(parsed.chassis_info.len(), 1);
    let chassis = &parsed.chassis_info[0];
    assert_eq!(chassis.host_id, "node-a:9090");
    assert_eq!(chassis.hostname, "node-a");
    assert_eq!(chassis.total_power_watts, Some(360.0));
    assert_eq!(chassis.inlet_temperature, Some(21.5));
    assert_eq!(chassis.outlet_temperature, Some(31.5));
    assert_eq!(
        chassis.detail.get("Product Name").map(String::as_str),
        Some("Rack Node")
    );
    assert_eq!(
        chassis.detail.get("cpu_power_watts").map(String::as_str),
        Some("120")
    );
    assert_eq!(
        chassis.detail.get("gpu_power_watts").map(String::as_str),
        Some("220")
    );
    assert_eq!(chassis.fan_speeds.len(), 1);
    assert_eq!(chassis.fan_speeds[0].name, "Front");
    assert_eq!(chassis.fan_speeds[0].speed_rpm, 4200);
}

#[tokio::test]
async fn explicit_http_host_renders_devices_chassis_energy_and_cost() {
    let addr = spawn_metrics_server().await;
    let host = format!("http://{addr}");
    let host_id = addr.to_string();
    let config = CollectionConfig {
        interval: 1,
        first_iteration: true,
        hosts: vec![host.clone()],
    };
    let collector = RemoteCollectorBuilder::new()
        .with_hosts(config.hosts.clone())
        .with_max_connections(1)
        .build();
    let energy_config = EnergyConfig {
        // Deliberately high so a short integration interval produces a
        // non-zero two-decimal cost and verifies the cost branch itself.
        price_per_kwh: 3_600_000.0,
        currency: "USD".to_string(),
        show_cost: true,
        wal_enabled: false,
        ..Default::default()
    };
    let mut initial_state = AppState::with_energy_config(&energy_config);
    initial_state.is_local_mode = false;
    let state = Arc::new(Mutex::new(initial_state));

    let first = collector
        .collect(&config)
        .await
        .expect("first remote scrape");
    collector.update_state(state.clone(), first, &config).await;
    tokio::time::sleep(Duration::from_millis(30)).await;
    let second = collector
        .collect(&config)
        .await
        .expect("second remote scrape");
    collector.update_state(state.clone(), second, &config).await;

    let snapshot = {
        let mut state = state.lock().await;
        assert_eq!(state.known_hosts, vec![host_id.clone()]);
        assert_eq!(state.connection_status.len(), 1);
        assert!(state.connection_status[&host_id].is_connected);
        assert_eq!(state.hostname_to_host_id["node-a"], host_id);
        assert_eq!(state.gpu_info[0].host_id, host_id);
        assert_eq!(state.chassis_info[0].host_id, host_id);
        let energy_key = EnergyKey::chassis(host_id.clone());
        let session_joules = state.energy.integrator().session_joules(&energy_key);
        let lifetime_joules = state.energy.integrator().lifetime_joules(&energy_key);
        assert!(session_joules > 0.0);
        assert_eq!(lifetime_joules, session_joules);
        assert_eq!(
            state
                .energy
                .integrator()
                .session_joules(&EnergyKey::chassis("node-a")),
            0.0,
            "remote chassis energy must not use the exporter hostname as its identity"
        );
        assert!(
            session_joules < 900_000.0,
            "remote lifetime counter must not become viewer session energy"
        );
        state.current_tab = state
            .tabs
            .iter()
            .position(|tab| tab == &host_id)
            .expect("host tab");
        RenderSnapshot::capture(&mut state, &mut HeavyCache::default())
    };

    let mut cache = ViewCache::new();
    cache.update(&snapshot);
    assert_eq!(cache.gpu_indices().map(<[usize]>::len), Some(1));
    assert_eq!(
        cache
            .host_device_indices()
            .map(|devices| devices.chassis_indices.len()),
        Some(1)
    );

    let mut args = ViewArgs::empty();
    args.hosts = Some(vec![host]);
    let (output, _) = FrameRenderer::render_main(&snapshot, &args, 120, 50, Some(&cache));
    assert!(!output.contains("CONNECTION LOST"), "{output}");
    assert!(output.contains("Test GPU"), "{output}");
    assert!(output.contains("NODE"), "{output}");
    assert!(output.contains("Energy session:"), "{output}");
    assert!(output.contains('$'), "{output}");
    assert!(!output.contains("$0.00"), "{output}");
}

#[tokio::test]
async fn unreachable_explicit_http_host_keeps_disconnection_notice() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("reserve unreachable address");
    let addr = listener.local_addr().expect("unreachable address");
    drop(listener);

    let host = format!("http://{addr}");
    let host_id = addr.to_string();
    let config = CollectionConfig {
        interval: 1,
        first_iteration: true,
        hosts: vec![host.clone()],
    };
    let collector = RemoteCollectorBuilder::new()
        .with_hosts(config.hosts.clone())
        .with_max_connections(1)
        .build();
    let mut initial_state = AppState::new();
    initial_state.is_local_mode = false;
    let state = Arc::new(Mutex::new(initial_state));

    let data = collector
        .collect(&config)
        .await
        .expect("failed scrape result");
    collector.update_state(state.clone(), data, &config).await;
    let snapshot = {
        let mut state = state.lock().await;
        assert!(!state.connection_status[&host_id].is_connected);
        state.current_tab = state
            .tabs
            .iter()
            .position(|tab| tab == &host_id)
            .expect("unreachable host tab");
        RenderSnapshot::capture(&mut state, &mut HeavyCache::default())
    };

    let mut args = ViewArgs::empty();
    args.hosts = Some(vec![host]);
    let (output, _) = FrameRenderer::render_main(&snapshot, &args, 100, 35, None);
    assert!(output.contains("CONNECTION LOST"), "{output}");
    assert!(
        output.contains("Unable to retrieve node information"),
        "{output}"
    );
}

/// Serve `METRICS` with `Connection: close` until `hang` is set; afterwards
/// accept connections and never answer.
async fn spawn_switchable_server(hang: Arc<std::sync::atomic::AtomicBool>) -> SocketAddr {
    use std::sync::atomic::Ordering;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind switchable server");
    let addr = listener.local_addr().expect("switchable server address");
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((mut socket, _)) = listener.accept().await {
            if hang.load(Ordering::SeqCst) {
                held.push(socket);
                continue;
            }
            tokio::spawn(async move {
                let mut request = [0_u8; 2048];
                let _ = socket.read(&mut request).await;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{METRICS}",
                    METRICS.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.shutdown().await;
            });
        }
    });
    addr
}

/// Issue #500: a host aborted at the scrape deadline used to keep the
/// previous tick's `is_connected = true`, so the TUI never showed
/// "CONNECTION LOST" for it.
#[tokio::test]
async fn host_aborted_at_deadline_loses_its_connected_status() {
    use crate::network::host_backoff::ScrapeTimeouts;
    use std::sync::atomic::{AtomicBool, Ordering};

    let hang = Arc::new(AtomicBool::new(false));
    let addr = spawn_switchable_server(hang.clone()).await;
    let host = format!("http://{addr}");
    let host_id = addr.to_string();
    let config = CollectionConfig {
        interval: 1,
        first_iteration: true,
        hosts: vec![host.clone()],
    };
    let collector = RemoteCollectorBuilder::new()
        .with_max_connections(4)
        .with_timeouts(ScrapeTimeouts {
            connect: Duration::from_millis(500),
            request: Duration::from_secs(2),
            budget: Duration::from_millis(300),
        })
        .build();
    let mut initial_state = AppState::new();
    initial_state.is_local_mode = false;
    let state = Arc::new(Mutex::new(initial_state));

    let data = collector.collect(&config).await.expect("first scrape");
    collector.update_state(state.clone(), data, &config).await;
    assert!(state.lock().await.connection_status[&host_id].is_connected);

    hang.store(true, Ordering::SeqCst);
    let data = collector.collect(&config).await.expect("second scrape");
    collector.update_state(state.clone(), data, &config).await;

    let state = state.lock().await;
    let status = &state.connection_status[&host_id];
    assert!(!status.is_connected, "{status:?}");
    assert_eq!(status.connection_state.as_deref(), Some("timeout"));
    assert_eq!(
        status.actual_hostname.as_deref(),
        Some("node-a"),
        "the display name survives the disconnect"
    );
}

fn connected_status(host: &str) -> crate::app_state::ConnectionStatus {
    let mut status = crate::app_state::ConnectionStatus::new(
        crate::common::http_hosts::http_host_identifier(host),
        host.to_string(),
    );
    status.mark_success();
    status
}

fn empty_collection(hosts: &[String]) -> crate::view::data_collection::strategy::CollectionData {
    crate::view::data_collection::strategy::CollectionData {
        gpu_info: Vec::new(),
        cpu_info: Vec::new(),
        memory_info: Vec::new(),
        process_info: Vec::new(),
        storage_info: Vec::new(),
        chassis_info: Vec::new(),
        vgpu_info: Vec::new(),
        mig_info: Vec::new(),
        connection_statuses: hosts.iter().map(|h| connected_status(h)).collect(),
        remote_process_info: Vec::new(),
    }
}

/// Issue #500: tabs and statuses follow the configured host list, so a host
/// added by a hostfile reload gets a tab and a removed host leaves.
#[tokio::test]
async fn tabs_and_statuses_follow_hostfile_reloads() {
    let collector = RemoteCollectorBuilder::new().build();
    let mut initial_state = AppState::new();
    initial_state.is_local_mode = false;
    let state = Arc::new(Mutex::new(initial_state));

    let apply = |hosts: &[&str]| {
        let hosts: Vec<String> = hosts.iter().map(|h| (*h).to_string()).collect();
        let config = CollectionConfig {
            interval: 1,
            first_iteration: false,
            hosts: hosts.clone(),
        };
        (empty_collection(&hosts), config)
    };
    let host_tabs = |state: &AppState| state.tabs[3..].to_vec();
    let status_keys = |state: &AppState| {
        let mut keys: Vec<String> = state.connection_status.keys().cloned().collect();
        keys.sort();
        keys
    };

    let (data, config) = apply(&["http://a:9090"]);
    collector.update_state(state.clone(), data, &config).await;
    {
        let state = state.lock().await;
        assert_eq!(host_tabs(&state), vec!["a:9090"]);
        assert_eq!(status_keys(&state), vec!["a:9090"]);
    }

    let (data, config) = apply(&["http://a:9090", "http://b:9090"]);
    collector.update_state(state.clone(), data, &config).await;
    {
        let mut state = state.lock().await;
        assert_eq!(host_tabs(&state), vec!["a:9090", "b:9090"]);
        assert_eq!(status_keys(&state), vec!["a:9090", "b:9090"]);
        assert_eq!(state.known_hosts, vec!["a:9090", "b:9090"]);
        state.current_tab = state.tabs.iter().position(|t| t == "b:9090").unwrap();
    }

    let (data, config) = apply(&["http://b:9090", "http://c:9090"]);
    collector.update_state(state.clone(), data, &config).await;
    {
        let state = state.lock().await;
        assert_eq!(host_tabs(&state), vec!["b:9090", "c:9090"]);
        assert_eq!(status_keys(&state), vec!["b:9090", "c:9090"]);
        assert_eq!(
            state.tabs[state.current_tab], "b:9090",
            "the selected host tab stays selected"
        );
    }

    let (data, config) = apply(&["http://b:9090"]);
    collector.update_state(state.clone(), data, &config).await;
    let state = state.lock().await;
    assert_eq!(host_tabs(&state), vec!["b:9090"]);
    assert_eq!(status_keys(&state), vec!["b:9090"]);
    assert_eq!(state.tabs[state.current_tab], "b:9090");
}
