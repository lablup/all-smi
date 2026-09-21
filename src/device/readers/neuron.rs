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

//! AWS Neuron (Trainium / Inferentia) NPU reader.
//!
//! # Data sources
//!
//! Static inventory comes from `neuron-ls --json-output`, which is a
//! natural one-shot: it prints a top-level JSON array (one object per
//! Neuron device) and exits 0. Per-NeuronCore memory comes from the
//! driver's sysfs tree under `/sys/devices/virtual/neuron_device`, which
//! is world-readable and populated with no workload attached. Per-core
//! *utilization* is only observable through `neuron-monitor`, and only
//! while a Neuron runtime process is attached to the device, so it is
//! best-effort and reported absent otherwise.
//!
//! # Absolute binary paths
//!
//! `/opt/aws/neuron/bin` is prepended to `PATH` by the Neuron DLAMI
//! profile scripts only. Under `sudo`, a systemd unit, or a container
//! entrypoint the tools are `command not found` (exit 127), so every
//! invocation here names the absolute path.
//!
//! # Absence
//!
//! Built and verified against a `trn1.2xlarge` (1 Trainium device, 2
//! NeuronCores, driver `aws-neuronx-dkms 2.26.5.0`, tools
//! `aws-neuronx-tools 2.28.23.0`). That platform exposes no temperature
//! sensor anywhere — not through `neuron-ls`, not through
//! `neuron-monitor`, not through sysfs — so temperature is left absent
//! rather than invented. Power is left absent for a different reason:
//! `stats/power/utilization` exists but carries three unlabelled floats
//! that read `0.00` on real idle hardware, so the raw line is preserved
//! in `detail` instead of being guessed at.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Mutex, mpsc};
use std::time::{Duration, Instant};

use chrono::Local;
use serde::Deserialize;
use serde_json::Value;

use crate::device::GpuReader;
use crate::device::common::execute_command_default;
use crate::device::readers::common_cache::{DetailBuilder, MAX_DEVICES};
use crate::device::types::{GPU_METRIC_UNAVAILABLE, GpuInfo, ProcessInfo};
use crate::utils::command::new_command;
use crate::utils::get_hostname;

/// `neuron-ls`, always by absolute path (see module docs).
const NEURON_LS_BIN: &str = "/opt/aws/neuron/bin/neuron-ls";
/// `neuron-monitor`, always by absolute path (see module docs).
const NEURON_MONITOR_BIN: &str = "/opt/aws/neuron/bin/neuron-monitor";
/// Root of the `neuron` driver's sysfs tree.
const SYSFS_NEURON_ROOT: &str = "/sys/devices/virtual/neuron_device";
/// Driver version exported by the `neuron` kernel module.
const SYSFS_DRIVER_VERSION: &str = "/sys/module/neuron/version";

/// How long to wait for `neuron-monitor`'s first NDJSON record.
///
/// The tool has no `--once` / `--count` / `--period` flag but emits a
/// complete record immediately at t≈0 and then every 5 s. Matches the
/// 2 s bare-metal ceiling used by `run_command_fast_fail` so a wedged
/// monitor cannot stall a collection tick for longer than any other
/// vendor command would.
const MONITOR_FIRST_RECORD_TIMEOUT: Duration = Duration::from_secs(2);

/// Upper bound on one `neuron-monitor` NDJSON record. Real records on a
/// single-device instance are ~1.5 KB; a 32-device box with several
/// runtimes attached stays orders of magnitude below this.
const MONITOR_RECORD_CAP_BYTES: u64 = 4 * 1024 * 1024;

/// Maximum plausible NeuronCore count accepted from `neuron-ls`.
///
/// Current hardware stays well below this value. The generous ceiling
/// keeps future devices working while preventing corrupt tool output from
/// driving an effectively unbounded allocation and sysfs walk.
const MAX_NEURON_CORES_PER_DEVICE: u32 = 256;

/// Delay before retrying `neuron-monitor` after a transient failure.
const MONITOR_RETRY_INTERVAL: Duration = Duration::from_secs(30);

/// A failed monitor probe is suppressed briefly, not permanently: the
/// daemon and runtime may become available after this process starts.
static MONITOR_RETRY_AFTER: Mutex<Option<Instant>> = Mutex::new(None);

/// One element of the `neuron-ls --json-output` top-level array.
///
/// Field types follow the tool verbatim, including the two traps:
/// `neuron_device` is a JSON number while `numa_node` is a *string*
/// (`"-1"` on an instance with no NUMA topology), and `connected_to` is
/// `null` on single-device instances. `memory_size` is bare bytes
/// (34359738368 on a Trainium device — the human-readable table prints
/// "32 GB" but the number is 32 GiB).
#[derive(Debug, Clone, Deserialize)]
struct NeuronLsDevice {
    /// Driver enumeration order. Not an identity: see [`compose_uuid`].
    neuron_device: u32,
    #[serde(default)]
    bdf: String,
    #[serde(default)]
    cpu_affinity: String,
    #[serde(default)]
    numa_node: String,
    /// Device-to-device topology on multi-device instances. Shape is
    /// unverified here: the reference instance had exactly one device
    /// and reported `null`, so the value is carried through as opaque
    /// JSON rather than modelled.
    #[serde(default)]
    connected_to: Option<Value>,
    #[serde(default)]
    nc_count: u32,
    /// Device HBM size in bytes. Used as-is; never scaled.
    #[serde(default)]
    memory_size: u64,
    #[serde(default)]
    neuroncore_ids: Vec<u32>,
    /// Processes holding the device. Element shape is unverified (the
    /// reference instance had no workload attached, so this was `[]`),
    /// hence the lenient field set.
    #[serde(default)]
    neuron_processes: Vec<NeuronLsProcess>,
}

/// Entry of `neuron-ls`'s per-device `neuron_processes` array.
#[derive(Debug, Clone, Deserialize)]
struct NeuronLsProcess {
    #[serde(default)]
    pid: Option<u32>,
    #[serde(default, alias = "cmd", alias = "cmdline")]
    command: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

/// One `neuron-monitor` NDJSON record, trimmed to the fields this reader
/// consumes. Names follow AWS's own shipped frontends
/// (`neuron-monitor-prometheus.py`, `neuron-monitor-device-view.py`).
#[derive(Debug, Clone, Deserialize)]
struct MonitorRecord {
    #[serde(default)]
    neuron_runtime_data: Vec<MonitorRuntime>,
    #[serde(default)]
    neuron_hardware_info: Option<MonitorHardwareInfo>,
}

#[derive(Debug, Clone, Deserialize)]
struct MonitorRuntime {
    #[serde(default)]
    error: String,
    #[serde(default)]
    report: Option<MonitorReport>,
}

#[derive(Debug, Clone, Deserialize)]
struct MonitorReport {
    #[serde(default)]
    neuroncore_counters: Option<MonitorNeuroncoreCounters>,
}

#[derive(Debug, Clone, Deserialize)]
struct MonitorNeuroncoreCounters {
    #[serde(default)]
    error: String,
    /// Keyed by a **global flat** NeuronCore index rendered as a string:
    /// `nd_idx * neuroncore_per_device_count + nc_idx`. On a 16-device
    /// instance, device 3 core 1 is key `"7"`.
    #[serde(default)]
    neuroncores_in_use: HashMap<String, MonitorNeuroncoreUsage>,
}

#[derive(Debug, Clone, Deserialize)]
struct MonitorNeuroncoreUsage {
    #[serde(default)]
    neuroncore_utilization: f64,
}

#[derive(Debug, Clone, Deserialize)]
struct MonitorHardwareInfo {
    #[serde(default)]
    neuron_device_type: String,
    #[serde(default)]
    neuron_device_version: String,
    #[serde(default)]
    neuroncore_version: String,
    #[serde(default)]
    logical_neuroncore_config: Option<u32>,
}

/// Per-device facts read out of sysfs, all of them in one pass.
///
/// The first four are static identity: they are the same string on every
/// poll. `power_utilization_raw` is not. `read_sysfs_device_info` runs once
/// per device per poll, so that field is re-read every time, and the line it
/// holds carries both a sampling timestamp and the live utilization floats.
/// Treating it as identity is what put it on the `all_smi_gpu_info` label set
/// and gave every NeuronCore a fresh series per scrape, so it is registered
/// in `detail_keys::VOLATILE_DETAIL_KEYS`.
#[derive(Debug, Default, Clone)]
struct SysfsDeviceInfo {
    serial_number: Option<String>,
    arch_type: Option<String>,
    device_name: Option<String>,
    instance_type: Option<String>,
    /// Raw `stats/power/utilization` line, re-read on every poll. Moves on
    /// its own; never an identity label.
    power_utilization_raw: Option<String>,
}

pub struct NeuronReader;

impl Default for NeuronReader {
    fn default() -> Self {
        Self::new()
    }
}

impl NeuronReader {
    pub fn new() -> Self {
        Self
    }
}

impl GpuReader for NeuronReader {
    fn get_gpu_info(&self) -> Vec<GpuInfo> {
        let devices = match run_neuron_ls() {
            Some(stdout) => parse_neuron_ls_devices(&stdout),
            None => return Vec::new(),
        };
        if devices.is_empty() {
            return Vec::new();
        }

        let monitor = collect_monitor_snapshot();
        let time = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
        let hostname = get_hostname();
        let driver_version = read_sysfs_trimmed(Path::new(SYSFS_DRIVER_VERSION));

        let mut rows = Vec::new();
        for device in &devices {
            let ctx = DeviceContext {
                sysfs: read_sysfs_device_info(device.neuron_device),
                driver_version: driver_version.clone(),
                time: time.clone(),
                hostname: hostname.clone(),
            };
            rows.extend(core_rows(device, &ctx, monitor.as_ref()));
        }
        rows
    }

    fn get_process_info(&self) -> Vec<ProcessInfo> {
        let devices = match run_neuron_ls() {
            Some(stdout) => parse_neuron_ls_devices(&stdout),
            None => return Vec::new(),
        };
        device_processes(&devices)
    }
}

/// Per-device data shared by every NeuronCore row of that device.
struct DeviceContext {
    sysfs: SysfsDeviceInfo,
    driver_version: Option<String>,
    time: String,
    hostname: String,
}

/// A parsed `neuron-monitor` record plus the utilization map derived
/// from it.
struct MonitorSnapshot {
    /// Global flat NeuronCore index -> utilization percentage.
    utilization: HashMap<u32, f64>,
    hardware: Option<MonitorHardwareInfo>,
}

/// Run `neuron-ls --json-output` and return its stdout, or `None` when
/// no device is visible.
///
/// Measured failure modes on real hardware:
/// * device absent — exit 1, stdout empty and clean, one logfmt line on
///   stderr. "empty stdout + non-zero rc" is therefore a safe absence
///   test.
/// * unknown flag — exit 1 with the message on *stdout*, not stderr.
/// * tools not installed / not on `PATH` — exit 127.
fn run_neuron_ls() -> Option<String> {
    let output = execute_command_default(NEURON_LS_BIN, &["--json-output"]).ok()?;
    if output.status != 0 || output.stdout.trim().is_empty() {
        return None;
    }
    Some(output.stdout)
}

/// Parse the `neuron-ls --json-output` top-level array.
///
/// Empty (or absent) stdout yields an empty list rather than a
/// synthetic device row. The real output carries no trailing newline
/// after `]`, which `trim` handles either way.
fn parse_neuron_ls_devices(stdout: &str) -> Vec<NeuronLsDevice> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let values = match serde_json::from_str::<Vec<Value>>(trimmed) {
        Ok(values) => values,
        Err(error) => {
            eprintln!("Failed to parse neuron-ls JSON output: {error}");
            return Vec::new();
        }
    };
    values
        .into_iter()
        .take(MAX_DEVICES)
        .filter_map(|value| match serde_json::from_value(value) {
            Ok(device) => Some(device),
            Err(error) => {
                eprintln!("Skipping malformed neuron-ls device: {error}");
                None
            }
        })
        .collect()
}

/// Compatibility fallback for old `neuron-ls` output that omits
/// `neuroncore_ids`, matching AWS's device-view calculation.
fn calculated_flat_core_index(
    device_index: u32,
    cores_per_device: u32,
    core_position: u32,
) -> Option<u32> {
    device_index
        .checked_mul(cores_per_device)?
        .checked_add(core_position)
}

/// Resolve the authoritative global NeuronCore ID for one local position.
///
/// Current `neuron-ls` output supplies `neuroncore_ids` explicitly.
/// Prefer it so logical-core configuration and future non-uniform topology
/// cannot drift from the IDs used by `neuron-monitor`.
fn device_core_index(
    device: &NeuronLsDevice,
    cores_per_device: u32,
    core_position: u32,
) -> Option<u32> {
    device
        .neuroncore_ids
        .get(core_position as usize)
        .copied()
        .or_else(|| {
            calculated_flat_core_index(device.neuron_device, cores_per_device, core_position)
        })
}

/// Stable-ish device identity.
///
/// `neuron_device` is driver enumeration order, not an identity, so the
/// UUID is composed from the hardware serial when sysfs exposes one and
/// from the PCI BDF otherwise, plus the NeuronCore index. Whether either
/// component survives a reboot was not verified.
fn compose_uuid(serial: Option<&str>, bdf: &str, flat_core: u32) -> String {
    let base = match serial {
        Some(serial) if !serial.is_empty() => serial.to_string(),
        _ if !bdf.is_empty() => bdf.to_string(),
        _ => "unknown".to_string(),
    };
    format!("neuron-{base}-nc{flat_core}")
}

/// Human-readable device name, e.g. `AWS Trainium1`.
fn compose_name(sysfs: &SysfsDeviceInfo, monitor: Option<&MonitorHardwareInfo>) -> String {
    if let Some(name) = sysfs.device_name.as_deref().filter(|s| !s.is_empty()) {
        return format!("AWS {name}");
    }
    if let Some(kind) = monitor
        .map(|hw| hw.neuron_device_type.as_str())
        .filter(|s| !s.is_empty())
    {
        let mut chars = kind.chars();
        let capitalized = match chars.next() {
            Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
            None => kind.to_string(),
        };
        return format!("AWS Neuron {capitalized}");
    }
    "AWS Neuron Device".to_string()
}

/// NUMA node, canonicalised the same way the NVIDIA reader does: the
/// driver's `"-1"` means "no NUMA topology" and becomes `None` rather
/// than a negative number on screen.
fn parse_numa_node(raw: &str) -> Option<i32> {
    raw.trim().parse::<i32>().ok().filter(|node| *node >= 0)
}

fn sysfs_device_dir(device_index: u32) -> PathBuf {
    PathBuf::from(SYSFS_NEURON_ROOT).join(format!("neuron{device_index}"))
}

fn read_sysfs_trimmed(path: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(path).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn read_sysfs_u64(path: &Path) -> Option<u64> {
    read_sysfs_trimmed(path)?.parse::<u64>().ok()
}

fn read_sysfs_device_info(device_index: u32) -> SysfsDeviceInfo {
    let dir = sysfs_device_dir(device_index);
    SysfsDeviceInfo {
        serial_number: read_sysfs_trimmed(&dir.join("info/serial_number")),
        arch_type: read_sysfs_trimmed(&dir.join("info/architecture/arch_type")),
        device_name: read_sysfs_trimmed(&dir.join("info/architecture/device_name")),
        instance_type: read_sysfs_trimmed(&dir.join("info/architecture/instance_type")),
        power_utilization_raw: read_sysfs_trimmed(&dir.join("stats/power/utilization")),
    }
}

/// Bytes currently allocated in device HBM for one NeuronCore.
///
/// `present` is the live allocation (`peak` is the high-water mark and
/// `total` the cumulative sum), and it is populated with no workload
/// attached, which is why sysfs is preferred over `neuron-monitor` for
/// memory.
fn read_core_used_memory(device_index: u32, core_position: u32) -> Option<u64> {
    let path = sysfs_device_dir(device_index).join(format!(
        "neuron_core{core_position}/stats/memory_usage/device_mem/present"
    ));
    read_sysfs_u64(&path)
}

fn read_core_arch_type(device_index: u32, core_position: u32) -> Option<String> {
    let path = sysfs_device_dir(device_index).join(format!(
        "neuron_core{core_position}/info/architecture/arch_type"
    ));
    read_sysfs_trimmed(&path)
}

/// Spawn `neuron-monitor`, keep its first NDJSON record, and kill it.
///
/// There is no one-shot flag, so the child is terminated as soon as the
/// record is in hand rather than a background streaming subsystem being
/// introduced for it. Failures start a short backoff so a wedged tool does
/// not stall every refresh tick while transient failures can still recover.
fn neuron_monitor_first_record() -> Option<String> {
    if !Path::new(NEURON_MONITOR_BIN).exists() || monitor_backoff_active(Instant::now()) {
        return None;
    }

    let mut child = match new_command(NEURON_MONITOR_BIN)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => {
            record_monitor_failure(Instant::now());
            return None;
        }
    };

    let line = match child.stdout.take() {
        Some(stdout) => read_first_line(stdout),
        None => None,
    };

    let _ = child.kill();
    let _ = child.wait();

    match line {
        Some(line) => {
            clear_monitor_backoff();
            Some(line)
        }
        None => {
            record_monitor_failure(Instant::now());
            None
        }
    }
}

fn retry_deadline_is_active(retry_after: Option<Instant>, now: Instant) -> bool {
    retry_after.is_some_and(|deadline| now < deadline)
}

fn monitor_backoff_active(now: Instant) -> bool {
    MONITOR_RETRY_AFTER
        .lock()
        .map(|retry_after| retry_deadline_is_active(*retry_after, now))
        .unwrap_or(false)
}

fn record_monitor_failure(now: Instant) {
    if let Ok(mut retry_after) = MONITOR_RETRY_AFTER.lock() {
        *retry_after = now.checked_add(MONITOR_RETRY_INTERVAL);
    }
}

fn clear_monitor_backoff() {
    if let Ok(mut retry_after) = MONITOR_RETRY_AFTER.lock() {
        *retry_after = None;
    }
}

/// Read one line off a child's stdout with a deadline.
///
/// The read runs on a helper thread so a monitor that never speaks
/// cannot wedge the collection tick; killing the child closes the pipe,
/// which ends the helper.
fn read_first_line<R: Read + Send + 'static>(stdout: R) -> Option<String> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout.take(MONITOR_RECORD_CAP_BYTES));
        let mut line = String::new();
        let _ = reader.read_line(&mut line);
        let _ = tx.send(line);
    });
    rx.recv_timeout(MONITOR_FIRST_RECORD_TIMEOUT)
        .ok()
        .filter(|line| !line.trim().is_empty())
}

fn collect_monitor_snapshot() -> Option<MonitorSnapshot> {
    let line = neuron_monitor_first_record()?;
    match parse_monitor_record(&line) {
        Some(snapshot) => Some(snapshot),
        None => {
            record_monitor_failure(Instant::now());
            None
        }
    }
}

/// Turn one `neuron-monitor` record into a utilization map.
///
/// With no runtime attached, `neuron_runtime_data` is an empty array and
/// the map comes back empty — which is reported as absent utilization,
/// never as 0%.
fn parse_monitor_record(line: &str) -> Option<MonitorSnapshot> {
    let record: MonitorRecord = serde_json::from_str(line.trim()).ok()?;
    let mut utilization = HashMap::new();
    for runtime in &record.neuron_runtime_data {
        if !runtime.error.is_empty() {
            continue;
        }
        let Some(counters) = runtime
            .report
            .as_ref()
            .and_then(|report| report.neuroncore_counters.as_ref())
        else {
            continue;
        };
        if !counters.error.is_empty() {
            continue;
        }
        for (key, usage) in &counters.neuroncores_in_use {
            if let Ok(index) = key.parse::<u32>()
                && usage.neuroncore_utilization.is_finite()
                && (0.0..=100.0).contains(&usage.neuroncore_utilization)
            {
                utilization.insert(index, usage.neuroncore_utilization);
            }
        }
    }
    Some(MonitorSnapshot {
        utilization,
        hardware: record.neuron_hardware_info,
    })
}

/// Expand one Neuron device into one row per NeuronCore.
fn core_rows(
    device: &NeuronLsDevice,
    ctx: &DeviceContext,
    monitor: Option<&MonitorSnapshot>,
) -> Vec<GpuInfo> {
    let core_count = effective_core_count(device);
    if core_count == 0 {
        return Vec::new();
    }
    // Capacity is split evenly across the cores of a device: `neuron-ls`
    // reports HBM per *device* and every consumer of `total_memory`
    // sums rows, so charging the full device size to each core would
    // multiply the host's memory by the core count.
    let per_core_memory = device.memory_size / u64::from(core_count);
    let memory_remainder = device.memory_size % u64::from(core_count);

    (0..core_count)
        .filter_map(|position| {
            let flat = device_core_index(device, core_count, position)?;
            let memory = per_core_memory
                + if u64::from(position) < memory_remainder {
                    1
                } else {
                    0
                };
            Some(build_core_row(device, ctx, monitor, position, flat, memory))
        })
        .collect()
}

/// Cores on this device: `nc_count` when the tool reports one, else the
/// length of `neuroncore_ids`.
fn effective_core_count(device: &NeuronLsDevice) -> u32 {
    let reported = if device.nc_count > 0 {
        device.nc_count
    } else {
        u32::try_from(device.neuroncore_ids.len()).unwrap_or(u32::MAX)
    };
    reported.min(MAX_NEURON_CORES_PER_DEVICE)
}

fn build_core_row(
    device: &NeuronLsDevice,
    ctx: &DeviceContext,
    monitor: Option<&MonitorSnapshot>,
    core_position: u32,
    flat_core: u32,
    per_core_memory: u64,
) -> GpuInfo {
    let hardware = monitor.and_then(|snapshot| snapshot.hardware.as_ref());
    let utilization = monitor
        .and_then(|snapshot| snapshot.utilization.get(&flat_core).copied())
        .unwrap_or(GPU_METRIC_UNAVAILABLE);
    let used_memory = read_core_used_memory(device.neuron_device, core_position);

    let detail = build_detail(
        device,
        ctx,
        hardware,
        core_position,
        flat_core,
        utilization,
        used_memory,
    );

    GpuInfo {
        uuid: compose_uuid(ctx.sysfs.serial_number.as_deref(), &device.bdf, flat_core),
        time: ctx.time.clone(),
        name: compose_name(&ctx.sysfs, hardware),
        // Drives `CommonNpuExporter`, which emits the shared
        // `all_smi_npu_*` family for every "NPU" row.
        device_type: "NPU".to_string(),
        host_id: ctx.hostname.clone(),
        hostname: ctx.hostname.clone(),
        instance: ctx.hostname.clone(),
        utilization,
        ane_utilization: 0.0,
        dla_utilization: None,
        tensorcore_utilization: None,
        // Trainium exposes no temperature anywhere; `0` is this field's
        // unavailable marker (see `GpuInfo::temperature_reading`).
        temperature: 0,
        used_memory: used_memory.unwrap_or(0),
        total_memory: per_core_memory,
        // No clock probe exists for NeuronCores; `0` is the unavailable
        // marker for `frequency` (see `GpuInfo::frequency_reading`).
        frequency: 0,
        // `stats/power/utilization` exists but its three floats are
        // unlabelled and read 0.00 on idle hardware, so the reading is
        // marked absent and the raw line kept in `detail`.
        power_consumption: GPU_METRIC_UNAVAILABLE,
        gpu_core_count: None,
        // AWS Neuron has no NVML equivalent for thermal thresholds,
        // P-states, GSP firmware, NvLink topology, or GPM counters.
        temperature_threshold_slowdown: None,
        temperature_threshold_shutdown: None,
        temperature_threshold_max_operating: None,
        temperature_threshold_acoustic: None,
        performance_state: None,
        fan_speed_rpm: None,
        numa_node_id: parse_numa_node(&device.numa_node),
        gsp_firmware_mode: None,
        gsp_firmware_version: None,
        nvlink_remote_devices: Vec::new(),
        gpm_metrics: None,
        detail,
    }
}

fn build_detail(
    device: &NeuronLsDevice,
    ctx: &DeviceContext,
    hardware: Option<&MonitorHardwareInfo>,
    core_position: u32,
    flat_core: u32,
    utilization: f64,
    used_memory: Option<u64>,
) -> HashMap<String, String> {
    let mut builder = DetailBuilder::new()
        .insert("neuron_device_index", device.neuron_device.to_string())
        .insert("neuroncore_index", flat_core.to_string())
        .insert("neuroncore_local_index", core_position.to_string())
        .insert("neuroncore_count", effective_core_count(device).to_string())
        .insert("device_memory_bytes", device.memory_size.to_string())
        .insert(
            "utilization_source",
            if utilization >= 0.0 {
                "neuron-monitor"
            } else {
                "unavailable"
            },
        )
        .insert(
            "memory_usage_source",
            if used_memory.is_some() {
                "sysfs"
            } else {
                "unavailable"
            },
        )
        .insert_optional("pci_bdf", non_empty(&device.bdf))
        .insert_optional("cpu_affinity", non_empty(&device.cpu_affinity))
        .insert_optional("numa_node", non_empty(&device.numa_node))
        .insert_optional("serial_number", ctx.sysfs.serial_number.clone())
        .insert_optional("architecture", ctx.sysfs.arch_type.clone())
        .insert_optional("device_name", ctx.sysfs.device_name.clone())
        .insert_optional("instance_type", ctx.sysfs.instance_type.clone())
        .insert_optional(
            "core_architecture",
            read_core_arch_type(device.neuron_device, core_position),
        )
        .insert_optional(
            "power_utilization_raw",
            ctx.sysfs.power_utilization_raw.clone(),
        )
        .insert_optional("driver_version", ctx.driver_version.clone())
        .insert_lib_info("Neuron", ctx.driver_version.as_deref());

    if let Some(connected) = device.connected_to.as_ref().filter(|v| !v.is_null()) {
        builder = builder.insert("connected_to", connected.to_string());
    }
    if let Some(hardware) = hardware {
        builder = builder
            .insert_optional(
                "neuron_device_type",
                non_empty(&hardware.neuron_device_type),
            )
            .insert_optional(
                "neuron_device_version",
                non_empty(&hardware.neuron_device_version),
            )
            .insert_optional(
                "neuroncore_version",
                non_empty(&hardware.neuroncore_version),
            )
            .insert_optional(
                "logical_neuroncore_config",
                hardware.logical_neuroncore_config.map(|v| v.to_string()),
            );
    }

    builder.build()
}

fn non_empty(value: &str) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

/// Processes reported by `neuron-ls` as holding each device.
///
/// The element shape of `neuron_processes` could not be verified (no
/// workload was attached to the reference instance), so an entry without
/// a PID is dropped rather than emitted as PID 0.
fn device_processes(devices: &[NeuronLsDevice]) -> Vec<ProcessInfo> {
    let mut processes = Vec::new();
    for device in devices {
        if device.neuron_processes.is_empty() {
            continue;
        }
        let serial = read_sysfs_device_info(device.neuron_device).serial_number;
        let Some((device_id, device_uuid)) = process_device_identity(device, serial.as_deref())
        else {
            continue;
        };
        for entry in &device.neuron_processes {
            let Some(pid) = entry.pid else {
                continue;
            };
            let command = entry.command.clone().unwrap_or_default();
            let process_name = entry
                .name
                .clone()
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| basename(&command));
            processes.push(ProcessInfo {
                device_id,
                device_uuid: device_uuid.clone(),
                pid,
                process_name,
                used_memory: 0,
                cpu_percent: 0.0,
                memory_percent: 0.0,
                memory_rss: 0,
                memory_vms: 0,
                user: String::new(),
                state: String::new(),
                start_time: String::new(),
                cpu_time: 0,
                command,
                ppid: 0,
                threads: 0,
                uses_gpu: true,
                priority: 0,
                nice_value: 0,
                gpu_utilization: 0.0,
            });
        }
    }
    processes
}

/// Map device-scoped process data to the first NeuronCore row.
///
/// `neuron-ls` does not identify a core for each process, so core zero is
/// the only deterministic correlation target. It must use the same sysfs
/// serial preference as [`build_core_row`] or process and device UUIDs
/// diverge on real hardware.
fn process_device_identity(
    device: &NeuronLsDevice,
    serial: Option<&str>,
) -> Option<(usize, String)> {
    let core_count = effective_core_count(device);
    if core_count == 0 {
        return None;
    }
    let flat_core = device_core_index(device, core_count, 0)?;
    Some((
        flat_core as usize,
        compose_uuid(serial, &device.bdf, flat_core),
    ))
}

fn basename(command: &str) -> String {
    command
        .split_whitespace()
        .next()
        .and_then(|path| path.split('/').next_back())
        .unwrap_or("unknown")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verbatim `neuron-ls --json-output` from a `trn1.2xlarge`
    /// (1 Trainium device, 2 NeuronCores). No trailing newline after
    /// `]`, exactly as the tool emits it.
    const NEURON_LS_TRN1_2XLARGE: &str = r#"[
    {
        "neuron_device": 0,
        "bdf": "0000:00:1e.0",
        "cpu_affinity": "0-7",
        "numa_node": "-1",
        "connected_to": null,
        "nc_count": 2,
        "memory_size": 34359738368,
        "neuroncore_ids": [
            0,
            1
        ],
        "neuron_processes": []
    }
]"#;

    /// Verbatim first NDJSON record from `neuron-monitor` on the same
    /// instance with no runtime attached: `neuron_runtime_data` is empty
    /// and only system / instance / hardware info is populated.
    const MONITOR_NO_RUNTIME: &str = r#"{"neuron_runtime_data":[],"system_data":{"memory_info":{"period":0.00047258,"memory_total_bytes":33110204416,"memory_used_bytes":1284890624,"swap_total_bytes":0,"swap_used_bytes":0,"error":""}},"instance_info":{"instance_name":"","instance_id":"i-0421ea5496c852415","instance_type":"trn1.2xlarge","instance_availability_zone":"us-west-2d","instance_availability_zone_id":"usw2-az4","instance_region":"us-west-2","ami_id":"ami-01807ad0e6484b5a8","subnet_id":"subnet-079d91c9a71f159fe","error":""},"neuron_hardware_info":{"neuron_device_type":"trainium","neuron_device_version":"v2","neuroncore_version":"v2","neuron_device_count":1,"neuron_device_memory_size":34359738368,"neuroncore_per_device_count":2,"logical_neuroncore_config":1,"error":""}}"#;

    /// NOT captured output. Synthesised from AWS's own shipped
    /// frontends (`neuron-monitor-prometheus.py`,
    /// `neuron-monitor-device-view.py`), which are authoritative for
    /// these field names: no workload was run on the reference
    /// instance, so a populated `neuron_runtime_data` could not be
    /// captured. Core keys are deliberately the *global flat* indices.
    const MONITOR_WITH_RUNTIME: &str = r#"{"neuron_runtime_data":[{"neuron_runtime_tag":"12345","error":"","report":{"neuroncore_counters":{"period":1.0,"neuroncores_in_use":{"0":{"neuroncore_utilization":42.5},"1":{"neuroncore_utilization":7.25}},"error":""}}}],"neuron_hardware_info":{"neuron_device_type":"trainium","neuron_device_version":"v2","neuroncore_version":"v2","neuron_device_count":1,"neuron_device_memory_size":34359738368,"neuroncore_per_device_count":2,"logical_neuroncore_config":1,"error":""}}"#;

    #[test]
    fn neuron_ls_array_parses() {
        let devices = parse_neuron_ls_devices(NEURON_LS_TRN1_2XLARGE);
        assert_eq!(devices.len(), 1);
        let device = &devices[0];
        assert_eq!(device.neuron_device, 0);
        assert_eq!(device.bdf, "0000:00:1e.0");
        assert_eq!(device.cpu_affinity, "0-7");
        assert_eq!(device.nc_count, 2);
        assert_eq!(device.neuroncore_ids, vec![0, 1]);
        assert!(device.neuron_processes.is_empty());
    }

    #[test]
    fn memory_size_is_read_as_bare_bytes() {
        let devices = parse_neuron_ls_devices(NEURON_LS_TRN1_2XLARGE);
        // 34359738368 is exactly 32 GiB. The human-readable table
        // prints "32 GB", but the JSON number must be used as-is and
        // never multiplied by a unit factor.
        assert_eq!(devices[0].memory_size, 34_359_738_368);
        assert_eq!(devices[0].memory_size, 32 * 1024 * 1024 * 1024);
    }

    #[test]
    fn numa_node_string_parses_without_panicking() {
        let devices = parse_neuron_ls_devices(NEURON_LS_TRN1_2XLARGE);
        // `numa_node` is a JSON *string*, not a number.
        assert_eq!(devices[0].numa_node, "-1");
        // "-1" means "no NUMA topology" and is canonicalised to absent.
        assert_eq!(parse_numa_node(&devices[0].numa_node), None);
        assert_eq!(parse_numa_node("0"), Some(0));
        assert_eq!(parse_numa_node(" 3 "), Some(3));
        assert_eq!(parse_numa_node("not-a-number"), None);
    }

    #[test]
    fn connected_to_null_is_tolerated() {
        let devices = parse_neuron_ls_devices(NEURON_LS_TRN1_2XLARGE);
        assert!(
            devices[0]
                .connected_to
                .as_ref()
                .is_none_or(|value| value.is_null())
        );
    }

    #[test]
    fn absent_device_yields_empty_list_not_zero_rows() {
        // Device absent: exit 1 with empty stdout. The reader must
        // produce no rows at all rather than a row of zeros.
        assert!(parse_neuron_ls_devices("").is_empty());
        assert!(parse_neuron_ls_devices("   \n").is_empty());
        // An unknown flag prints a human message on stdout; it must not
        // be mistaken for a device list.
        assert!(parse_neuron_ls_devices("unknown flag `badflag'").is_empty());
    }

    #[test]
    fn malformed_device_does_not_hide_valid_inventory() {
        let mixed = r#"[
            {"neuron_device":0,"bdf":"0000:00:1e.0","nc_count":2,"memory_size":8,"neuroncore_ids":[0,1]},
            {"neuron_device":"future-format"},
            {"neuron_device":1,"bdf":"0000:00:1f.0","nc_count":2,"memory_size":8,"neuroncore_ids":[2,3]}
        ]"#;
        let devices = parse_neuron_ls_devices(mixed);
        assert_eq!(devices.len(), 2);
        assert_eq!(devices[0].neuron_device, 0);
        assert_eq!(devices[1].neuron_device, 1);
    }

    #[test]
    fn core_rows_split_device_memory_and_mark_absent_metrics() {
        let devices = parse_neuron_ls_devices(NEURON_LS_TRN1_2XLARGE);
        let ctx = DeviceContext {
            sysfs: SysfsDeviceInfo {
                serial_number: Some("9ff5434815c8bd80".to_string()),
                arch_type: Some("NDv2".to_string()),
                device_name: Some("Trainium1".to_string()),
                instance_type: Some("Trn1".to_string()),
                power_utilization_raw: Some(
                    "POWER_STATUS_VALID,1789180860,0.00,0.00,0.00".to_string(),
                ),
            },
            driver_version: Some("2.26.5.0".to_string()),
            time: "2026-09-12 02:41:18".to_string(),
            hostname: "ip-10-0-2-237".to_string(),
        };

        let rows = core_rows(&devices[0], &ctx, None);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].device_type, "NPU");
        assert_eq!(rows[0].name, "AWS Trainium1");
        assert_eq!(rows[0].uuid, "neuron-9ff5434815c8bd80-nc0");
        assert_eq!(rows[1].uuid, "neuron-9ff5434815c8bd80-nc1");
        // 32 GiB of device HBM split across two NeuronCores.
        assert_eq!(rows[0].total_memory, 16 * 1024 * 1024 * 1024);
        assert_eq!(rows[1].total_memory, 16 * 1024 * 1024 * 1024);
        // Absence contract: never 0 for something we could not source.
        assert_eq!(rows[0].utilization_reading(), None);
        assert_eq!(rows[0].power_consumption_reading(), None);
        assert_eq!(rows[0].temperature_reading(), None);
        assert_eq!(rows[0].frequency_reading(), None);
        assert_eq!(rows[0].numa_node_id, None);
        assert_eq!(
            rows[0].detail.get("utilization_source").map(String::as_str),
            Some("unavailable")
        );
        assert_eq!(
            rows[0]
                .detail
                .get("device_memory_bytes")
                .map(String::as_str),
            Some("34359738368")
        );
        // 13 NVIDIA-only fields stay absent.
        assert!(rows[0].temperature_threshold_slowdown.is_none());
        assert!(rows[0].performance_state.is_none());
        assert!(rows[0].gpm_metrics.is_none());
        assert!(rows[0].nvlink_remote_devices.is_empty());
    }

    /// Issue #425: `stats/power/utilization` is re-read on every poll and its
    /// second field is a driver sampling timestamp, so while the raw line was
    /// a label every NeuronCore started a fresh `all_smi_gpu_info` series on
    /// each scrape, even with the device completely idle. This renders two
    /// polls through the real exporter and pins the label set.
    #[test]
    fn the_raw_power_line_never_moves_the_neuron_identity_label_set() {
        use crate::api::metrics::MetricExporter;
        use crate::api::metrics::gpu::GpuMetricExporter;

        const IDLE: &str = "POWER_STATUS_VALID,1789180860,0.00,0.00,0.00";
        const BUSY: &str = "POWER_STATUS_VALID,1789180875,12.50,13.00,11.75";

        let devices = parse_neuron_ls_devices(NEURON_LS_TRN1_2XLARGE);
        let poll = |power_line: &str| {
            let ctx = DeviceContext {
                sysfs: SysfsDeviceInfo {
                    serial_number: Some("9ff5434815c8bd80".to_string()),
                    arch_type: Some("NDv2".to_string()),
                    device_name: Some("Trainium1".to_string()),
                    instance_type: Some("Trn1".to_string()),
                    power_utilization_raw: Some(power_line.to_string()),
                },
                driver_version: Some("2.26.5.0".to_string()),
                time: "2026-09-12 02:41:18".to_string(),
                hostname: "ip-10-0-2-237".to_string(),
            };
            let rows = core_rows(&devices[0], &ctx, None);
            let identity = GpuMetricExporter::new(&rows)
                .export_metrics()
                .lines()
                .find(|line| line.starts_with("all_smi_gpu_info{"))
                .expect("identity series missing")
                .to_string();
            (identity, rows)
        };

        let (idle_line, idle_rows) = poll(IDLE);
        let (busy_line, _) = poll(BUSY);
        assert_eq!(
            idle_line, busy_line,
            "the raw power line moved the identity label set between polls"
        );
        assert!(
            !idle_line.contains("power_utilization_raw=\""),
            "the raw power line reached the identity series: {idle_line}"
        );
        // Labels only: the TUI and the snapshot writers still read the value.
        assert_eq!(
            idle_rows[0]
                .detail
                .get("power_utilization_raw")
                .map(String::as_str),
            Some(IDLE)
        );
    }

    #[test]
    fn monitor_without_runtime_reports_no_utilization() {
        let snapshot = parse_monitor_record(MONITOR_NO_RUNTIME).expect("record parses");
        assert!(snapshot.utilization.is_empty());
        let hardware = snapshot.hardware.expect("hardware info present");
        assert_eq!(hardware.neuron_device_type, "trainium");
        assert_eq!(hardware.neuroncore_version, "v2");
        assert_eq!(hardware.logical_neuroncore_config, Some(1));
    }

    #[test]
    fn monitor_with_runtime_keys_cores_by_global_flat_index() {
        let snapshot = parse_monitor_record(MONITOR_WITH_RUNTIME).expect("record parses");
        assert_eq!(snapshot.utilization.get(&0).copied(), Some(42.5));
        assert_eq!(snapshot.utilization.get(&1).copied(), Some(7.25));

        let devices = parse_neuron_ls_devices(NEURON_LS_TRN1_2XLARGE);
        let ctx = DeviceContext {
            sysfs: SysfsDeviceInfo::default(),
            driver_version: None,
            time: String::new(),
            hostname: "host".to_string(),
        };
        let rows = core_rows(&devices[0], &ctx, Some(&snapshot));
        assert_eq!(rows[0].utilization_reading(), Some(42.5));
        assert_eq!(rows[1].utilization_reading(), Some(7.25));
        assert_eq!(
            rows[0].detail.get("utilization_source").map(String::as_str),
            Some("neuron-monitor")
        );
        // No serial in sysfs: identity falls back to the PCI BDF.
        assert_eq!(rows[0].uuid, "neuron-0000:00:1e.0-nc0");
    }

    #[test]
    fn invalid_monitor_utilization_is_omitted() {
        let invalid = r#"{"neuron_runtime_data":[{"report":{"neuroncore_counters":{"neuroncores_in_use":{"0":{"neuroncore_utilization":101.0},"1":{"neuroncore_utilization":-0.1}}}}}]}"#;
        let snapshot = parse_monitor_record(invalid).expect("record parses");
        assert!(snapshot.utilization.is_empty());
    }

    #[test]
    fn monitor_backoff_expires() {
        let now = Instant::now();
        assert!(!retry_deadline_is_active(None, now));
        assert!(retry_deadline_is_active(
            now.checked_add(Duration::from_secs(1)),
            now
        ));
        assert!(!retry_deadline_is_active(Some(now), now));
    }

    #[test]
    fn calculated_core_index_is_global_not_device_local() {
        // AWS's device-view frontend computes
        // `nd_idx * neuroncore_per_device_count + nc_idx_counter`, so on
        // a 16-device trn1.32xlarge device 3 core 1 is key "7".
        assert_eq!(calculated_flat_core_index(0, 2, 0), Some(0));
        assert_eq!(calculated_flat_core_index(0, 2, 1), Some(1));
        assert_eq!(calculated_flat_core_index(3, 2, 1), Some(7));
        assert_eq!(calculated_flat_core_index(15, 2, 1), Some(31));
        assert_eq!(calculated_flat_core_index(u32::MAX, 2, 0), None);
    }

    #[test]
    fn explicit_neuroncore_ids_override_calculated_indices() {
        let mut device = parse_neuron_ls_devices(NEURON_LS_TRN1_2XLARGE).remove(0);
        device.neuron_device = 3;
        device.neuroncore_ids = vec![42, 43];
        assert_eq!(device_core_index(&device, 2, 0), Some(42));
        assert_eq!(device_core_index(&device, 2, 1), Some(43));

        device.neuroncore_ids.clear();
        assert_eq!(device_core_index(&device, 2, 0), Some(6));
        assert_eq!(device_core_index(&device, 2, 1), Some(7));
    }

    #[test]
    fn core_count_falls_back_to_neuroncore_ids() {
        let mut device = parse_neuron_ls_devices(NEURON_LS_TRN1_2XLARGE).remove(0);
        assert_eq!(effective_core_count(&device), 2);
        device.nc_count = 0;
        assert_eq!(effective_core_count(&device), 2);
        device.neuroncore_ids.clear();
        assert_eq!(effective_core_count(&device), 0);
        let ctx = DeviceContext {
            sysfs: SysfsDeviceInfo::default(),
            driver_version: None,
            time: String::new(),
            hostname: "host".to_string(),
        };
        assert!(core_rows(&device, &ctx, None).is_empty());

        device.nc_count = u32::MAX;
        assert_eq!(effective_core_count(&device), MAX_NEURON_CORES_PER_DEVICE);
    }

    #[test]
    fn core_rows_preserve_memory_remainder() {
        let mut device = parse_neuron_ls_devices(NEURON_LS_TRN1_2XLARGE).remove(0);
        device.memory_size = 5;
        let ctx = DeviceContext {
            sysfs: SysfsDeviceInfo::default(),
            driver_version: None,
            time: String::new(),
            hostname: "host".to_string(),
        };
        let rows = core_rows(&device, &ctx, None);
        assert_eq!(rows.iter().map(|row| row.total_memory).sum::<u64>(), 5);
        assert_eq!(rows[0].total_memory, 3);
        assert_eq!(rows[1].total_memory, 2);
    }

    #[test]
    fn device_name_falls_back_to_monitor_hardware_info() {
        let snapshot = parse_monitor_record(MONITOR_NO_RUNTIME).expect("record parses");
        let name = compose_name(&SysfsDeviceInfo::default(), snapshot.hardware.as_ref());
        assert_eq!(name, "AWS Neuron Trainium");
        assert_eq!(
            compose_name(&SysfsDeviceInfo::default(), None),
            "AWS Neuron Device"
        );
    }

    #[test]
    fn processes_without_a_pid_are_dropped() {
        let devices = parse_neuron_ls_devices(NEURON_LS_TRN1_2XLARGE);
        assert!(device_processes(&devices).is_empty());

        let with_procs = r#"[{"neuron_device":0,"bdf":"0000:00:1e.0","numa_node":"-1","nc_count":2,
             "memory_size":34359738368,"neuroncore_ids":[0,1],
             "neuron_processes":[{"pid":4242,"command":"/usr/bin/python3 train.py"},{"command":"no-pid"}]}]"#;
        let processes = device_processes(&parse_neuron_ls_devices(with_procs));
        assert_eq!(processes.len(), 1);
        assert_eq!(processes[0].pid, 4242);
        assert_eq!(processes[0].process_name, "python3");
        assert_eq!(processes[0].device_id, 0);
        assert_eq!(processes[0].device_uuid, "neuron-0000:00:1e.0-nc0");
        assert!(processes[0].uses_gpu);

        let device = parse_neuron_ls_devices(with_procs).remove(0);
        assert_eq!(
            process_device_identity(&device, Some("serial-123")),
            Some((0, "neuron-serial-123-nc0".to_string()))
        );

        let mut no_cores = device;
        no_cores.nc_count = 0;
        no_cores.neuroncore_ids.clear();
        assert_eq!(process_device_identity(&no_cores, None), None);
        assert!(
            device_processes(std::slice::from_ref(&no_cores)).is_empty(),
            "a process must not reference a device row that cannot be emitted"
        );
    }
}
