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
use regex::Regex;
use tokio::sync::Mutex;

use crate::app_state::AppState;
use crate::cli::ViewArgs;
use crate::common::config::EnergyConfig;
use crate::metrics::energy::EnergyKey;
use crate::network::metrics_parser::MetricsParser;
use crate::view::data_collection::remote_collector::RemoteCollectorBuilder;
use crate::view::data_collection::strategy::{CollectionConfig, DataCollectionStrategy};
use crate::view::frame_renderer::FrameRenderer;
use crate::view::render_snapshot::RenderSnapshot;
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

fn metrics_regex() -> Regex {
    Regex::new(r"^all_smi_([^\{]+)\{([^}]+)\} ([\d\.]+)$").expect("valid metrics regex")
}

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
    let parsed =
        MetricsParser::new().parse_metrics_with_chassis(METRICS, "node-a:9090", &metrics_regex());

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
        let energy_key = EnergyKey::chassis("node-a");
        let session_joules = state.energy.integrator().session_joules(&energy_key);
        let lifetime_joules = state.energy.integrator().lifetime_joules(&energy_key);
        assert!(session_joules > 0.0);
        assert_eq!(lifetime_joules, session_joules);
        assert!(
            session_joules < 900_000.0,
            "remote lifetime counter must not become viewer session energy"
        );
        state.current_tab = state
            .tabs
            .iter()
            .position(|tab| tab == &host_id)
            .expect("host tab");
        RenderSnapshot::capture(&mut state)
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
        RenderSnapshot::capture(&mut state)
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
