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

//! Text exposition validity for the full `/metrics` body (issue #503).
//!
//! The Prometheus text format allows one `# HELP` and one `# TYPE` per
//! metric family, and requires all samples of a family to be contiguous.
//! The Prometheus server's own scrape parser is lenient about both, but the
//! Go `expfmt` parser behind `promtool check metrics` and every OpenMetrics
//! parser reject a body that repeats the metadata or interleaves families.
//! The validator below enforces the same rules without a new dependency.

use std::collections::{HashMap, HashSet};

use super::render::{MetricsRenderInputs, render_prometheus_exposition};
use crate::device::{CpuInfo, GpuInfo, MemoryInfo, ProcessInfo};
use crate::storage::info::StorageInfo;
use crate::utils::RuntimeEnvironment;

/// Walk a text exposition and return the first format violation.
///
/// Fails on a second HELP or TYPE for a name, on metadata or a sample for a
/// family that already ended (another family started after it), and on a
/// sample that does not belong to the family declared by the most recent
/// TYPE line. `_bucket`, `_sum` and `_count` suffixes are stripped only for
/// histogram and summary families, which are the only ones that carry them.
pub(super) fn validate_exposition(body: &str) -> Result<(), String> {
    let mut helps: HashSet<&str> = HashSet::new();
    let mut types: HashMap<&str, &str> = HashMap::new();
    let mut finished: HashSet<&str> = HashSet::new();
    let mut current: Option<&str> = None;

    // Move to `family`, closing the previous one. Errors when `family` was
    // already closed, which means its lines are not contiguous.
    fn enter<'a>(
        family: &'a str,
        current: &mut Option<&'a str>,
        finished: &mut HashSet<&'a str>,
        line_no: usize,
    ) -> Result<(), String> {
        if *current == Some(family) {
            return Ok(());
        }
        if finished.contains(family) {
            return Err(format!(
                "line {line_no}: family {family} reappears after another family started"
            ));
        }
        if let Some(previous) = current.replace(family) {
            finished.insert(previous);
        }
        Ok(())
    }

    for (idx, line) in body.lines().enumerate() {
        let line_no = idx + 1;
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("# HELP ") {
            let name = rest.split(' ').next().unwrap_or("");
            if !helps.insert(name) {
                return Err(format!("line {line_no}: second HELP line for {name}"));
            }
            enter(name, &mut current, &mut finished, line_no)?;
            continue;
        }
        if let Some(rest) = line.strip_prefix("# TYPE ") {
            let mut parts = rest.split(' ');
            let name = parts.next().unwrap_or("");
            let kind = parts.next().unwrap_or("");
            if types.insert(name, kind).is_some() {
                return Err(format!("line {line_no}: second TYPE line for {name}"));
            }
            enter(name, &mut current, &mut finished, line_no)?;
            continue;
        }
        if line.starts_with('#') {
            continue;
        }

        let sample_name = line
            .split(['{', ' '])
            .next()
            .filter(|name| !name.is_empty())
            .ok_or_else(|| format!("line {line_no}: sample without a name: {line}"))?;
        let family = match current {
            Some(family) if family == sample_name => family,
            Some(family)
                if matches!(types.get(family), Some(&("histogram" | "summary")))
                    && ["_bucket", "_sum", "_count"]
                        .iter()
                        .any(|suffix| sample_name.strip_suffix(suffix) == Some(family)) =>
            {
                family
            }
            _ => {
                return Err(format!(
                    "line {line_no}: sample {sample_name} does not follow its own TYPE line \
                     (current family: {current:?})"
                ));
            }
        };
        if !types.contains_key(family) {
            return Err(format!("line {line_no}: sample {sample_name} has no TYPE"));
        }
    }
    Ok(())
}

fn gpu(uuid: &str, name: &str, device_type: &str) -> GpuInfo {
    GpuInfo {
        uuid: uuid.to_string(),
        time: String::new(),
        name: name.to_string(),
        device_type: device_type.to_string(),
        host_id: "node-1".to_string(),
        hostname: "node-1".to_string(),
        instance: "node-1".to_string(),
        utilization: 50.0,
        ane_utilization: 0.0,
        dla_utilization: None,
        tensorcore_utilization: None,
        temperature: 70,
        used_memory: 1024,
        total_memory: 8192,
        frequency: 1500,
        power_consumption: 200.0,
        gpu_core_count: None,
        temperature_threshold_slowdown: Some(90),
        temperature_threshold_shutdown: Some(95),
        temperature_threshold_max_operating: Some(85),
        temperature_threshold_acoustic: Some(77),
        performance_state: Some(2),
        fan_speed_rpm: Some(1200),
        numa_node_id: Some(0),
        gsp_firmware_mode: None,
        gsp_firmware_version: None,
        nvlink_remote_devices: Vec::new(),
        gpm_metrics: None,
        detail: HashMap::new(),
    }
}

fn process(pid: u32) -> ProcessInfo {
    ProcessInfo {
        device_id: 0,
        device_uuid: "GPU-A".to_string(),
        pid,
        process_name: "python".to_string(),
        used_memory: 1024,
        cpu_percent: 12.5,
        memory_percent: 1.0,
        memory_rss: 0,
        memory_vms: 0,
        user: "alice".to_string(),
        state: "R".to_string(),
        start_time: "01:02:03".to_string(),
        cpu_time: 0,
        command: "python train.py".to_string(),
        ppid: 1,
        threads: 1,
        uses_gpu: true,
        priority: 20,
        nice_value: 0,
        gpu_utilization: 0.0,
    }
}

fn memory(index: u32) -> MemoryInfo {
    MemoryInfo {
        index,
        host_id: "node-1".to_string(),
        hostname: "node-1".to_string(),
        instance: "node-1".to_string(),
        total_bytes: 64 << 30,
        used_bytes: 16 << 30,
        available_bytes: 48 << 30,
        free_bytes: 40 << 30,
        buffers_bytes: 1 << 30,
        cached_bytes: 2 << 30,
        swap_total_bytes: 8 << 30,
        swap_used_bytes: 1 << 30,
        swap_free_bytes: 7 << 30,
        utilization: 25.0,
        time: String::new(),
    }
}

fn volume(index: u32, mount_point: &str) -> StorageInfo {
    StorageInfo {
        mount_point: mount_point.to_string(),
        total_bytes: 1 << 40,
        available_bytes: 1 << 39,
        host_id: "node-1".to_string(),
        hostname: "node-1".to_string(),
        index,
    }
}

fn cpu() -> CpuInfo {
    CpuInfo {
        host_id: "node-1".to_string(),
        hostname: "node-1".to_string(),
        instance: "node-1".to_string(),
        cpu_model: "Test CPU".to_string(),
        socket_count: 1,
        total_cores: 8,
        total_threads: 16,
        utilization: 30.0,
        ..CpuInfo::test_default()
    }
}

/// The multi-device host from the issue's acceptance criterion: 2 GPUs,
/// 2 NPUs, 3 processes, 2 memory entries, 3 volumes and 2 CPU entries.
struct Fixture {
    gpus: Vec<GpuInfo>,
    processes: Vec<ProcessInfo>,
    cpus: Vec<CpuInfo>,
    memory: Vec<MemoryInfo>,
    storage: Vec<StorageInfo>,
    env: RuntimeEnvironment,
}

impl Fixture {
    fn multi_device() -> Self {
        let mut engine_gpu = gpu("GPU-B", "NVIDIA A100", "GPU");
        // Dynamic, detail-derived families on one of the two GPUs only.
        engine_gpu
            .detail
            .insert("Engine: render".to_string(), "12 %".to_string());
        engine_gpu
            .detail
            .insert("Engine: video".to_string(), "3 %".to_string());
        Self {
            gpus: vec![
                gpu("GPU-A", "NVIDIA A100", "GPU"),
                engine_gpu,
                gpu("NPU-A", "Furiosa RNGD", "NPU"),
                gpu("NPU-B", "Furiosa RNGD", "NPU"),
            ],
            processes: vec![process(100), process(200), process(300)],
            cpus: vec![cpu(), cpu()],
            memory: vec![memory(0), memory(1)],
            storage: vec![volume(0, "/"), volume(1, "/data"), volume(2, "/scratch")],
            env: RuntimeEnvironment::default(),
        }
    }

    fn single_device() -> Self {
        Self {
            gpus: vec![gpu("GPU-A", "NVIDIA A100", "GPU")],
            processes: vec![process(100)],
            cpus: vec![cpu()],
            memory: vec![memory(0)],
            storage: vec![volume(0, "/")],
            env: RuntimeEnvironment::default(),
        }
    }

    fn render(&self) -> String {
        render_prometheus_exposition(&MetricsRenderInputs {
            gpu_info: &self.gpus,
            process_info: &self.processes,
            cpu_info: &self.cpus,
            memory_info: &self.memory,
            storage_info: &self.storage,
            runtime_environment: &self.env,
            chassis_info: &[],
            vgpu_info: &[],
            mig_info: &[],
            energy_integrator: None,
            ready: true,
        })
    }
}

fn count_lines(body: &str, line: &str) -> usize {
    body.lines().filter(|l| *l == line).count()
}

#[test]
fn validator_rejects_the_shapes_it_exists_to_catch() {
    let ok =
        "# HELP a A\n# TYPE a gauge\na{d=\"0\"} 1\na{d=\"1\"} 1\n# HELP b B\n# TYPE b gauge\nb 1\n";
    assert_eq!(validate_exposition(ok), Ok(()));

    // What the per-device exporters used to produce.
    let repeated =
        "# HELP a A\n# TYPE a gauge\na{d=\"0\"} 1\n# HELP a A\n# TYPE a gauge\na{d=\"1\"} 1\n";
    let err = validate_exposition(repeated).unwrap_err();
    assert!(err.contains("second HELP line for a"), "{err}");

    let second_type = "# TYPE a gauge\na 1\n# TYPE a gauge\n";
    assert!(validate_exposition(second_type).is_err());

    let interleaved = "# TYPE a gauge\na{d=\"0\"} 1\n# TYPE b gauge\nb 1\na{d=\"1\"} 1\n";
    let err = validate_exposition(interleaved).unwrap_err();
    assert!(err.contains("does not follow its own TYPE"), "{err}");

    let reappears = "# HELP a A\n# TYPE a gauge\na 1\n# TYPE b gauge\nb 1\n# HELP a A\n";
    assert!(validate_exposition(reappears).is_err());

    let histogram = "# TYPE h histogram\nh_bucket{le=\"+Inf\"} 1\nh_sum 1\nh_count 1\n";
    assert_eq!(validate_exposition(histogram), Ok(()));
    // The suffix is only meaningful on histogram and summary families.
    let gauge_with_suffix = "# TYPE h gauge\nh_count 1\n";
    assert!(validate_exposition(gauge_with_suffix).is_err());
}

#[test]
fn multi_device_exposition_is_valid_for_strict_parsers() {
    let body = Fixture::multi_device().render();
    if let Err(violation) = validate_exposition(&body) {
        panic!("{violation}\n{body}");
    }

    // Every family that has samples has exactly one HELP and one TYPE.
    let mut help_count: HashMap<&str, usize> = HashMap::new();
    let mut type_count: HashMap<&str, usize> = HashMap::new();
    let mut sampled: HashSet<&str> = HashSet::new();
    for line in body.lines() {
        if let Some(rest) = line.strip_prefix("# HELP ") {
            *help_count
                .entry(rest.split(' ').next().unwrap())
                .or_default() += 1;
        } else if let Some(rest) = line.strip_prefix("# TYPE ") {
            *type_count
                .entry(rest.split(' ').next().unwrap())
                .or_default() += 1;
        } else if !line.is_empty() {
            sampled.insert(line.split(['{', ' ']).next().unwrap());
        }
    }
    assert!(sampled.len() > 20, "fixture should exercise many families");
    for family in &sampled {
        assert_eq!(help_count.get(family), Some(&1), "HELP count for {family}");
        assert_eq!(type_count.get(family), Some(&1), "TYPE count for {family}");
    }
    // And no metadata for a family without samples (omit-on-no-data).
    for family in help_count.keys().chain(type_count.keys()) {
        assert!(
            sampled.contains(family),
            "{family} has metadata but no sample"
        );
    }

    // All devices are present in the multi-row families.
    for (family, rows) in [
        ("all_smi_gpu_memory_used_bytes", 4),
        ("all_smi_process_memory_used_bytes", 3),
        ("all_smi_memory_total_bytes", 2),
        ("all_smi_disk_total_bytes", 3),
        ("all_smi_cpu_utilization", 2),
        ("all_smi_gpu_engine_utilization", 2),
    ] {
        let prefix = format!("{family}{{");
        let got = body.lines().filter(|l| l.starts_with(&prefix)).count();
        assert_eq!(got, rows, "{family} rows in:\n{body}");
    }
}

#[test]
fn single_device_sample_lines_are_the_per_exporter_sample_lines() {
    use super::MetricExporter;
    use super::{
        cpu::CpuMetricExporter, disk::DiskMetricExporter, gpu::GpuMetricExporter,
        memory::MemoryMetricExporter, process::ProcessMetricExporter,
    };

    // For one device the per-device call order and the family order are the
    // same thing, so the body is exactly the concatenation it used to be:
    // each exporter's block, HELP, TYPE and sample per family. Comparing
    // against per-sample rendering through `metric()` would be circular, so
    // this pins the documented shape instead: every sample line directly
    // follows its own TYPE line.
    let fixture = Fixture::single_device();
    let body = fixture.render();
    assert_eq!(validate_exposition(&body), Ok(()));

    let standalone: Vec<String> = vec![
        GpuMetricExporter::new(&fixture.gpus).export_metrics(),
        ProcessMetricExporter::new(&fixture.processes).export_metrics(),
        CpuMetricExporter::new(&fixture.cpus).export_metrics(),
        MemoryMetricExporter::new(&fixture.memory).export_metrics(),
        DiskMetricExporter::new(&fixture.storage).export_metrics(),
    ];
    for block in &standalone {
        assert!(!block.is_empty());
        for line in block.lines() {
            assert_eq!(
                count_lines(&body, line),
                1,
                "line from a standalone exporter missing or duplicated: {line}"
            );
        }
    }
}

#[test]
fn a_family_with_no_reading_on_any_row_emits_no_metadata() {
    let mut fixture = Fixture::multi_device();
    for gpu in &mut fixture.gpus {
        gpu.fan_speed_rpm = None;
        gpu.temperature_threshold_acoustic = None;
    }
    let body = fixture.render();
    assert_eq!(validate_exposition(&body), Ok(()));
    for family in [
        "all_smi_gpu_fan_speed_rpm",
        "all_smi_gpu_temperature_threshold_acoustic_celsius",
    ] {
        assert!(!body.contains(family), "{family} must be omitted entirely");
    }

    // One row with a reading is enough for the family to appear, once.
    fixture.gpus[1].fan_speed_rpm = Some(900);
    let body = fixture.render();
    assert_eq!(validate_exposition(&body), Ok(()));
    assert_eq!(body.matches("# HELP all_smi_gpu_fan_speed_rpm ").count(), 1);
    assert_eq!(
        body.lines()
            .filter(|l| l.starts_with("all_smi_gpu_fan_speed_rpm{"))
            .count(),
        1
    );
}
