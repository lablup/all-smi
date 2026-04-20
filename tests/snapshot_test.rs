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
//
//! Integration tests for the `snapshot` subcommand library entry point.
//!
//! These tests exercise the end-to-end collect → serialize pipeline using a
//! synthetic [`SnapshotCollector`] implementation so the tests do not depend
//! on any real hardware being present in the CI environment.

#![cfg(feature = "cli")]

use std::collections::HashMap;
use std::fs;
use std::sync::Arc;
use std::time::Duration;

use all_smi::cli::{SnapshotFormat, SnapshotIncludes};
use all_smi::device::{ChassisInfo, CpuInfo, CpuPlatformType, GpuInfo, MemoryInfo, ProcessInfo};
use all_smi::snapshot::{
    SnapshotCollector, SnapshotHardFailure, SnapshotOptions, run_with_collector,
};
use all_smi::storage::info::StorageInfo;

/// A test collector that returns the pre-supplied vectors unchanged.
/// Each field can be swapped to emulate partial failures, empty sections, etc.
struct MockCollector {
    hostname: String,
    gpus: Vec<GpuInfo>,
    cpus: Vec<CpuInfo>,
    memory: Vec<MemoryInfo>,
    chassis: Vec<ChassisInfo>,
    processes: Vec<ProcessInfo>,
    storage: Vec<StorageInfo>,
}

impl MockCollector {
    fn empty() -> Self {
        Self {
            hostname: "mockhost".to_string(),
            gpus: Vec::new(),
            cpus: Vec::new(),
            memory: Vec::new(),
            chassis: Vec::new(),
            processes: Vec::new(),
            storage: Vec::new(),
        }
    }
}

impl SnapshotCollector for MockCollector {
    fn hostname(&self) -> String {
        self.hostname.clone()
    }
    fn collect_gpus(&self) -> Vec<GpuInfo> {
        self.gpus.clone()
    }
    fn collect_cpus(&self) -> Vec<CpuInfo> {
        self.cpus.clone()
    }
    fn collect_memory(&self) -> Vec<MemoryInfo> {
        self.memory.clone()
    }
    fn collect_chassis(&self) -> Vec<ChassisInfo> {
        self.chassis.clone()
    }
    fn collect_processes(&self) -> Vec<ProcessInfo> {
        self.processes.clone()
    }
    fn collect_storage(&self) -> Vec<StorageInfo> {
        self.storage.clone()
    }
}

fn mock_gpu(name: &str, util: f64, temp: u32) -> GpuInfo {
    GpuInfo {
        uuid: format!("GPU-{name}-UUID"),
        time: "2026-04-20T00:00:00Z".to_string(),
        name: name.to_string(),
        device_type: "GPU".to_string(),
        host_id: "mockhost".to_string(),
        hostname: "mockhost".to_string(),
        instance: "mockhost:9090".to_string(),
        utilization: util,
        ane_utilization: 0.0,
        dla_utilization: None,
        tensorcore_utilization: None,
        temperature: temp,
        used_memory: 1024 * 1024 * 1024, // 1 GiB
        total_memory: 8 * 1024 * 1024 * 1024,
        frequency: 1500,
        power_consumption: 250.0,
        gpu_core_count: None,
        temperature_threshold_slowdown: None,
        temperature_threshold_shutdown: None,
        temperature_threshold_max_operating: None,
        temperature_threshold_acoustic: None,
        performance_state: None,
        numa_node_id: None,
        gsp_firmware_mode: None,
        gsp_firmware_version: None,
        nvlink_remote_devices: Vec::new(),
        gpm_metrics: None,
        detail: HashMap::new(),
    }
}

fn mock_cpu() -> CpuInfo {
    CpuInfo {
        host_id: "mockhost".to_string(),
        hostname: "mockhost".to_string(),
        instance: "mockhost:9090".to_string(),
        cpu_model: "Mock CPU".to_string(),
        architecture: "x86_64".to_string(),
        platform_type: CpuPlatformType::Intel,
        socket_count: 1,
        total_cores: 8,
        total_threads: 16,
        base_frequency_mhz: 3000,
        max_frequency_mhz: 4500,
        cache_size_mb: 16,
        utilization: 25.0,
        temperature: Some(55),
        power_consumption: Some(65.0),
        per_socket_info: Vec::new(),
        apple_silicon_info: None,
        per_core_utilization: Vec::new(),
        time: "2026-04-20T00:00:00Z".to_string(),
    }
}

fn mock_memory() -> MemoryInfo {
    MemoryInfo {
        host_id: "mockhost".to_string(),
        hostname: "mockhost".to_string(),
        instance: "mockhost:9090".to_string(),
        total_bytes: 32 * 1024 * 1024 * 1024,
        used_bytes: 8 * 1024 * 1024 * 1024,
        available_bytes: 24 * 1024 * 1024 * 1024,
        free_bytes: 24 * 1024 * 1024 * 1024,
        buffers_bytes: 0,
        cached_bytes: 0,
        swap_total_bytes: 0,
        swap_used_bytes: 0,
        swap_free_bytes: 0,
        utilization: 25.0,
        time: "2026-04-20T00:00:00Z".to_string(),
    }
}

fn base_options(
    format: SnapshotFormat,
    includes: SnapshotIncludes,
    out: Option<String>,
) -> SnapshotOptions {
    SnapshotOptions {
        format,
        pretty: Some(false),
        includes,
        query: Vec::new(),
        samples: 1,
        interval: Duration::from_secs(0),
        timeout_per_reader: Duration::from_millis(1_000),
        output: out,
    }
}

#[tokio::test]
async fn json_single_sample_contains_expected_schema() {
    let collector = Arc::new(MockCollector {
        gpus: vec![mock_gpu("A100", 80.0, 65)],
        cpus: vec![mock_cpu()],
        memory: vec![mock_memory()],
        ..MockCollector::empty()
    });
    let path = std::env::temp_dir().join(format!("snapshot-json-{}.json", std::process::id()));
    let opts = base_options(
        SnapshotFormat::Json,
        SnapshotIncludes {
            gpu: true,
            cpu: true,
            memory: true,
            ..Default::default()
        },
        Some(path.to_string_lossy().into_owned()),
    );
    run_with_collector(opts, collector)
        .await
        .expect("snapshot run");
    let contents = fs::read_to_string(&path).expect("read snapshot file");
    let parsed: serde_json::Value = serde_json::from_str(&contents).expect("valid JSON");
    assert_eq!(parsed["schema"], serde_json::json!(1));
    assert!(parsed["timestamp"].is_string());
    assert_eq!(parsed["hostname"], "mockhost");
    assert_eq!(parsed["gpus"][0]["name"], "A100");
    assert_eq!(parsed["cpus"][0]["cpu_model"], "Mock CPU");
    assert!(parsed["memory"].is_array());
    // Absent sections must be missing, not empty.
    assert!(parsed.get("chassis").is_none());
    assert!(parsed.get("processes").is_none());
    assert!(parsed.get("storage").is_none());
    let _ = fs::remove_file(&path);
}

#[tokio::test]
async fn json_multi_sample_emits_array() {
    let collector = Arc::new(MockCollector {
        memory: vec![mock_memory()],
        ..MockCollector::empty()
    });
    let path = std::env::temp_dir().join(format!("snapshot-json-arr-{}.json", std::process::id()));
    let opts = SnapshotOptions {
        samples: 3,
        ..base_options(
            SnapshotFormat::Json,
            SnapshotIncludes {
                memory: true,
                ..Default::default()
            },
            Some(path.to_string_lossy().into_owned()),
        )
    };
    run_with_collector(opts, collector)
        .await
        .expect("snapshot run");
    let contents = fs::read_to_string(&path).expect("read snapshot file");
    let parsed: serde_json::Value = serde_json::from_str(&contents).expect("valid JSON");
    let arr = parsed.as_array().expect("top-level array when samples > 1");
    assert_eq!(arr.len(), 3);
    for entry in arr {
        assert_eq!(entry["schema"], serde_json::json!(1));
        assert!(entry["memory"].is_array());
        assert_eq!(entry["memory"][0]["utilization"], serde_json::json!(25.0));
    }
    let _ = fs::remove_file(&path);
}

#[tokio::test]
async fn csv_query_columns_resolve_against_gpus() {
    let collector = Arc::new(MockCollector {
        gpus: vec![mock_gpu("A100", 80.0, 65), mock_gpu("H100", 92.0, 70)],
        ..MockCollector::empty()
    });
    let path = std::env::temp_dir().join(format!("snapshot-csv-{}.csv", std::process::id()));
    let opts = SnapshotOptions {
        query: vec![
            "index".to_string(),
            "name".to_string(),
            "utilization".to_string(),
            "temperature".to_string(),
        ],
        ..base_options(
            SnapshotFormat::Csv,
            SnapshotIncludes {
                gpu: true,
                ..Default::default()
            },
            Some(path.to_string_lossy().into_owned()),
        )
    };
    run_with_collector(opts, collector)
        .await
        .expect("snapshot run");
    let contents = fs::read_to_string(&path).expect("read snapshot file");
    let mut lines = contents.lines();
    assert_eq!(lines.next(), Some("index,name,utilization,temperature"));
    assert_eq!(lines.next(), Some("0,A100,80.0,65"));
    assert_eq!(lines.next(), Some("1,H100,92.0,70"));
    assert_eq!(lines.next(), None);
    let _ = fs::remove_file(&path);
}

#[tokio::test]
async fn csv_missing_paths_yield_empty_cells() {
    let collector = Arc::new(MockCollector {
        gpus: vec![mock_gpu("A100", 80.0, 60)],
        ..MockCollector::empty()
    });
    let path =
        std::env::temp_dir().join(format!("snapshot-csv-missing-{}.csv", std::process::id()));
    let opts = SnapshotOptions {
        query: vec![
            "name".to_string(),
            "detail.cuda_version".to_string(),
            "does.not.exist".to_string(),
        ],
        ..base_options(
            SnapshotFormat::Csv,
            SnapshotIncludes {
                gpu: true,
                ..Default::default()
            },
            Some(path.to_string_lossy().into_owned()),
        )
    };
    run_with_collector(opts, collector)
        .await
        .expect("snapshot run");
    let contents = fs::read_to_string(&path).expect("read snapshot file");
    let mut lines = contents.lines();
    assert_eq!(
        lines.next(),
        Some("name,detail.cuda_version,does.not.exist")
    );
    assert_eq!(lines.next(), Some("A100,,"));
    let _ = fs::remove_file(&path);
}

#[tokio::test]
async fn hard_failure_when_all_sections_empty() {
    let collector = Arc::new(MockCollector::empty());
    let path = std::env::temp_dir().join(format!("snapshot-hardfail-{}.json", std::process::id()));
    let opts = base_options(
        SnapshotFormat::Json,
        SnapshotIncludes {
            gpu: true,
            cpu: true,
            memory: true,
            chassis: true,
            ..Default::default()
        },
        Some(path.to_string_lossy().into_owned()),
    );
    let result = run_with_collector(opts, collector).await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.downcast_ref::<SnapshotHardFailure>().is_some(),
        "expected SnapshotHardFailure, got: {err}"
    );
    // File should not have been created because the serializer is skipped
    // when the collection is a hard failure.
    assert!(!path.exists(), "hard-failure path must not write output");
}

#[tokio::test]
async fn include_only_memory_omits_other_sections() {
    let collector = Arc::new(MockCollector {
        gpus: vec![mock_gpu("A100", 80.0, 65)],
        cpus: vec![mock_cpu()],
        memory: vec![mock_memory()],
        ..MockCollector::empty()
    });
    let path = std::env::temp_dir().join(format!("snapshot-memonly-{}.json", std::process::id()));
    let opts = base_options(
        SnapshotFormat::Json,
        SnapshotIncludes {
            memory: true,
            ..Default::default()
        },
        Some(path.to_string_lossy().into_owned()),
    );
    run_with_collector(opts, collector)
        .await
        .expect("snapshot run");
    let parsed: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&path).expect("read snapshot file"))
            .expect("valid JSON");
    assert!(parsed.get("memory").is_some());
    assert!(
        parsed.get("gpus").is_none(),
        "include=memory must not emit gpus key"
    );
    assert!(parsed.get("cpus").is_none());
    assert!(parsed.get("chassis").is_none());
    let _ = fs::remove_file(&path);
}

#[tokio::test]
async fn prometheus_output_reuses_api_exporter_format() {
    let collector = Arc::new(MockCollector {
        gpus: vec![mock_gpu("A100", 80.0, 65)],
        memory: vec![mock_memory()],
        ..MockCollector::empty()
    });
    let path = std::env::temp_dir().join(format!("snapshot-prom-{}.prom", std::process::id()));
    let opts = base_options(
        SnapshotFormat::Prometheus,
        SnapshotIncludes {
            gpu: true,
            memory: true,
            ..Default::default()
        },
        Some(path.to_string_lossy().into_owned()),
    );
    run_with_collector(opts, collector)
        .await
        .expect("snapshot run");
    let contents = fs::read_to_string(&path).expect("read snapshot file");
    // Prometheus exposition must carry HELP/TYPE lines and the canonical
    // `all_smi_*` metric names the `/metrics` endpoint emits.
    assert!(contents.contains("# HELP all_smi_gpu_utilization"));
    assert!(contents.contains("# TYPE all_smi_gpu_utilization gauge"));
    assert!(contents.contains("all_smi_gpu_utilization{"));
    assert!(contents.contains("all_smi_memory_total_bytes{"));
    let _ = fs::remove_file(&path);
}

#[tokio::test]
async fn prometheus_output_is_byte_identical_to_api_exporter_for_same_data() {
    // Acceptance criterion: "snapshot --format prometheus byte-for-byte
    // matches a single scrape of api mode's /metrics for the same data".
    //
    // The `/metrics` HTTP handler is a thin wrapper around
    // `render_prometheus_exposition`, which is also what the snapshot path
    // calls. We assert byte equality against that helper fed the same
    // section vectors the `AppState` would carry at scrape time — proving
    // the paths are equivalent without needing to spin up an axum server.
    use all_smi::api::metrics::render::{MetricsRenderInputs, render_prometheus_exposition};
    use all_smi::utils::RuntimeEnvironment;

    let gpus = vec![mock_gpu("A100", 80.0, 65)];
    let cpus = vec![mock_cpu()];
    let memory = vec![mock_memory()];

    let runtime_env = RuntimeEnvironment::default();
    let empty_vgpu = Vec::new();
    let empty_mig = Vec::new();
    let inputs = MetricsRenderInputs {
        gpu_info: &gpus,
        process_info: &[],
        cpu_info: &cpus,
        memory_info: &memory,
        storage_info: &[],
        runtime_environment: &runtime_env,
        chassis_info: &[],
        vgpu_info: &empty_vgpu,
        mig_info: &empty_mig,
    };
    let expected = render_prometheus_exposition(&inputs);

    let collector = Arc::new(MockCollector {
        gpus: gpus.clone(),
        cpus: cpus.clone(),
        memory: memory.clone(),
        ..MockCollector::empty()
    });
    let path = std::env::temp_dir().join(format!("snapshot-byteid-{}.prom", std::process::id()));
    let opts = base_options(
        SnapshotFormat::Prometheus,
        SnapshotIncludes {
            gpu: true,
            cpu: true,
            memory: true,
            ..Default::default()
        },
        Some(path.to_string_lossy().into_owned()),
    );
    run_with_collector(opts, collector)
        .await
        .expect("snapshot run");
    let contents = fs::read_to_string(&path).expect("read snapshot file");
    assert_eq!(
        contents, expected,
        "snapshot prometheus output must be byte-identical to the shared renderer"
    );
    let _ = fs::remove_file(&path);
}

#[tokio::test]
async fn output_dash_is_treated_as_stdout() {
    // "-" is the documented alias for stdout. We cannot easily capture
    // stdout in a test, but we can assert that run_with_collector does
    // not treat "-" as a literal filename (which would create a file
    // named "-" in the cwd).
    let collector = Arc::new(MockCollector {
        memory: vec![mock_memory()],
        ..MockCollector::empty()
    });
    let opts = base_options(
        SnapshotFormat::Json,
        SnapshotIncludes {
            memory: true,
            ..Default::default()
        },
        Some("-".to_string()),
    );
    run_with_collector(opts, collector)
        .await
        .expect("snapshot run");
    // The only way to fail the assertion below is if we accidentally
    // created a "-" file in the repo root.
    assert!(
        !std::path::Path::new("-").exists(),
        "`--output -` must not create a literal dash file"
    );
}

#[tokio::test]
async fn non_finite_f64_fields_serialize_as_null_without_aborting() {
    // Regression: `serde_json::to_string` refuses to emit `NaN`/`±Inf`, so
    // without sanitization a single flaky driver field aborts the whole
    // snapshot. Assert that the snapshot succeeds and the offending fields
    // appear as `null` in the JSON output — consumers that need numeric
    // defaults can substitute them themselves.
    let mut gpu = mock_gpu("A100", 0.0, 0);
    gpu.utilization = f64::NAN;
    gpu.power_consumption = f64::INFINITY;
    gpu.ane_utilization = f64::NEG_INFINITY;

    let collector = Arc::new(MockCollector {
        gpus: vec![gpu],
        ..MockCollector::empty()
    });
    let path = std::env::temp_dir().join(format!("snapshot-nan-{}.json", std::process::id()));
    let opts = base_options(
        SnapshotFormat::Json,
        SnapshotIncludes {
            gpu: true,
            ..Default::default()
        },
        Some(path.to_string_lossy().into_owned()),
    );
    run_with_collector(opts, collector)
        .await
        .expect("snapshot run must not fail on NaN/Inf");
    let contents = fs::read_to_string(&path).expect("read snapshot file");
    let parsed: serde_json::Value =
        serde_json::from_str(&contents).expect("JSON must parse even with non-finite inputs");
    let gpu0 = &parsed["gpus"][0];
    assert!(gpu0["utilization"].is_null(), "NaN must become null");
    assert!(gpu0["power_consumption"].is_null(), "+Inf must become null");
    assert!(gpu0["ane_utilization"].is_null(), "-Inf must become null");
    // Sibling finite fields must still round-trip.
    assert_eq!(gpu0["name"], "A100");
    let _ = fs::remove_file(&path);
}

#[tokio::test]
async fn non_finite_f64_fields_survive_csv_path() {
    // Same scenario as the JSON path but via CSV, where the old
    // `unwrap_or(Value::Null)` pattern would have silently erased the
    // whole device instead of emitting empty cells for the bad fields.
    let mut gpu = mock_gpu("H100", 0.0, 70);
    gpu.utilization = f64::NAN;

    let collector = Arc::new(MockCollector {
        gpus: vec![gpu],
        ..MockCollector::empty()
    });
    let path = std::env::temp_dir().join(format!("snapshot-nan-{}.csv", std::process::id()));
    let opts = SnapshotOptions {
        query: vec![
            "name".to_string(),
            "utilization".to_string(),
            "temperature".to_string(),
        ],
        ..base_options(
            SnapshotFormat::Csv,
            SnapshotIncludes {
                gpu: true,
                ..Default::default()
            },
            Some(path.to_string_lossy().into_owned()),
        )
    };
    run_with_collector(opts, collector)
        .await
        .expect("snapshot CSV run must not fail on NaN");
    let contents = fs::read_to_string(&path).expect("read snapshot file");
    let mut lines = contents.lines();
    assert_eq!(lines.next(), Some("name,utilization,temperature"));
    // Device row must still be present; utilization cell should be empty
    // (because `null` stringifies to the empty cell), temperature intact.
    assert_eq!(lines.next(), Some("H100,,70"));
    let _ = fs::remove_file(&path);
}

#[cfg(unix)]
#[tokio::test]
async fn output_file_has_owner_only_permissions() {
    // `--output` now uses `OpenOptions::mode(0o600)` on Unix. Verify the
    // mode bits on the emitted file so future regressions that reintroduce
    // world-readable output show up in CI.
    use std::os::unix::fs::PermissionsExt;

    let collector = Arc::new(MockCollector {
        memory: vec![mock_memory()],
        ..MockCollector::empty()
    });
    let path = std::env::temp_dir().join(format!("snapshot-perms-{}.json", std::process::id()));
    let opts = base_options(
        SnapshotFormat::Json,
        SnapshotIncludes {
            memory: true,
            ..Default::default()
        },
        Some(path.to_string_lossy().into_owned()),
    );
    run_with_collector(opts, collector)
        .await
        .expect("snapshot run");
    let mode = fs::metadata(&path)
        .expect("stat output")
        .permissions()
        .mode();
    // The low 9 bits should be exactly 0o600 (rw- --- ---).
    assert_eq!(
        mode & 0o777,
        0o600,
        "snapshot file must be owner-only; got {mode:o}"
    );
    let _ = fs::remove_file(&path);
}

#[cfg(unix)]
#[tokio::test]
async fn output_refuses_to_follow_symlinks() {
    // Plant a *dangling* symlink at the tmp sibling path (the only place
    // our opener actually creates a file). Without `O_NOFOLLOW` the
    // `OpenOptions::create(true).open(...)` call would follow the symlink
    // and create the backing file at the attacker-controlled target;
    // with `O_NOFOLLOW` it errors out with `ELOOP` instead.
    //
    // We can't use `Path::exists()` as a test fixture helper here because
    // `exists()` returns `false` for dangling symlinks, which is exactly
    // the case we want to trigger the `O_NOFOLLOW` branch on.
    let target = std::env::temp_dir().join(format!("snapshot-sym-{}.json", std::process::id()));
    let sibling_tmp = {
        let mut p = target.as_os_str().to_os_string();
        p.push(".tmp");
        std::path::PathBuf::from(p)
    };
    let attacker_target = std::env::temp_dir().join(format!(
        "snapshot-sym-target-does-not-exist-{}.json",
        std::process::id()
    ));
    let _ = fs::remove_file(&target);
    let _ = fs::remove_file(&sibling_tmp);
    let _ = fs::remove_file(&attacker_target);
    std::os::unix::fs::symlink(&attacker_target, &sibling_tmp).expect("plant symlink");
    // Sanity-check the fixture: the symlink exists as a symlink but its
    // target does not exist, so `Path::exists()` returns `false` and the
    // `pick_tmp_path` collision-retry branch will NOT kick in.
    assert!(
        !sibling_tmp.exists(),
        "dangling symlink should report !exists()"
    );
    assert!(
        fs::symlink_metadata(&sibling_tmp)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
    );

    let collector = Arc::new(MockCollector {
        memory: vec![mock_memory()],
        ..MockCollector::empty()
    });
    let opts = base_options(
        SnapshotFormat::Json,
        SnapshotIncludes {
            memory: true,
            ..Default::default()
        },
        Some(target.to_string_lossy().into_owned()),
    );
    let result = run_with_collector(opts, collector).await;
    assert!(
        result.is_err(),
        "opening a symlinked tmp path must fail under O_NOFOLLOW"
    );
    // The attacker target MUST NOT have been created via follow-through.
    assert!(
        !attacker_target.exists(),
        "O_NOFOLLOW must have prevented creating {}",
        attacker_target.display()
    );
    let _ = fs::remove_file(&sibling_tmp);
    let _ = fs::remove_file(&target);
    let _ = fs::remove_file(&attacker_target);
}
