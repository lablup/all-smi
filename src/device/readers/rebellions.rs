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

use crate::device::GpuReader;
use crate::device::common::execute_command_default;
use crate::device::common::parsers::{parse_power, parse_temperature, parse_utilization};
use crate::device::readers::common_cache::{DetailBuilder, DeviceStaticInfo};
use crate::device::readers::detail_keys::CARD_POWER_WATTS_DETAIL_KEY;
use crate::device::types::{GPU_METRIC_UNAVAILABLE, GpuInfo, ProcessInfo};
use crate::utils::get_hostname;
use chrono::Local;
use once_cell::sync::Lazy;
use serde::Deserialize;
use serde::de::{self, Deserializer, Visitor};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

/// Custom deserializer that accepts both string and integer for the `npu` field.
/// This ensures backward compatibility across different SDK versions:
/// - SDK 1.x: outputs `"npu": "0"` (string)
/// - SDK 2.0.x: outputs `"npu": 0` (integer)
fn deserialize_string_or_u32<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: Deserializer<'de>,
{
    struct StringOrU32Visitor;

    impl<'de> Visitor<'de> for StringOrU32Visitor {
        type Value = u32;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("a string or u32")
        }

        fn visit_u64<E: de::Error>(self, v: u64) -> Result<u32, E> {
            u32::try_from(v).map_err(|_| E::custom(format!("u64 {v} out of range for u32")))
        }

        fn visit_str<E: de::Error>(self, v: &str) -> Result<u32, E> {
            v.parse()
                .map_err(|_| E::custom(format!("failed to parse '{v}' as u32")))
        }
    }

    deserializer.deserialize_any(StringOrU32Visitor)
}

/// JSON structures for Rebellions device information
#[derive(Debug, Deserialize)]
struct RblnPciInfo {
    #[allow(dead_code)]
    dev: String,
    bus_id: String,
    numa_node: String,
    link_speed: String,
    link_width: String,
}

#[derive(Debug, Deserialize)]
struct RblnMemoryInfo {
    used: String,
    total: String,
}

#[derive(Debug, Deserialize)]
struct RblnDevice {
    #[serde(deserialize_with = "deserialize_string_or_u32")]
    npu: u32,
    name: String,
    /// Board serial, shared by every die of one physical card. Groups dies
    /// into cards for power accounting (see [`card_power_roles`]). Defaulted
    /// so a device that omits it is read as a card of its own instead of
    /// failing the whole response and hiding every NPU on the host.
    #[serde(default)]
    sid: String,
    uuid: String,
    device: String,
    status: String,
    fw_ver: String,
    pci: RblnPciInfo,
    temperature: String,
    card_power: String,
    pstate: String,
    memory: RblnMemoryInfo,
    util: String,
    board_info: String,
    /// Physical slot/location index reported by the driver. Surfaced as the
    /// `Location` detail so the Prometheus exporter can label the device with
    /// the real value instead of a constant.
    location: u32,
}

#[derive(Debug, Deserialize)]
struct RblnResponse {
    #[serde(rename = "KMD_version")]
    kmd_version: String,
    devices: Vec<RblnDevice>,
    #[serde(default, deserialize_with = "deserialize_contexts_lossy")]
    contexts: Vec<RblnContext>,
}

#[derive(Debug, Deserialize)]
struct RblnContext {
    #[allow(dead_code)]
    ctx_id: String,
    /// Index of the device owning this context. `rbln-stat` 3.0.0 emits this as
    /// a JSON integer; a string is accepted too so other builds keep working.
    #[serde(deserialize_with = "deserialize_string_or_u32")]
    npu: u32,
    /// Process name. The tool spells this `process`; `cmd` is kept as an alias.
    #[serde(alias = "cmd")]
    process: String,
    /// PID, emitted as a quoted string by `rbln-stat` 3.0.0.
    #[serde(deserialize_with = "deserialize_string_or_u32")]
    pid: u32,
    /// Device memory held by this context, as a human-readable string such as
    /// "3.5GiB" or "72.0MiB" -- unlike the device-level memory fields, which
    /// are bare byte counts.
    #[serde(alias = "memory")]
    memalloc: String,
}

/// Deserialize process contexts independently so one malformed or newly
/// shaped context cannot hide every otherwise valid device in the response.
fn deserialize_contexts_lossy<'de, D>(deserializer: D) -> Result<Vec<RblnContext>, D::Error>
where
    D: Deserializer<'de>,
{
    let contexts = Vec::<serde_json::Value>::deserialize(deserializer)?;
    Ok(contexts
        .into_iter()
        .filter_map(|context| match serde_json::from_value(context) {
            Ok(context) => Some(context),
            Err(error) => {
                eprintln!("Skipping malformed Rebellions process context: {error}");
                None
            }
        })
        .collect())
}

/// Type alias for the cached command information
type CommandCache = Arc<Mutex<Option<(String, PathBuf)>>>;

/// Cache for rebellions command path
static RBLN_COMMAND_CACHE: Lazy<CommandCache> = Lazy::new(|| Arc::new(Mutex::new(None)));

pub struct RebellionsNpuReader {
    /// Cached KMD (driver) version
    kmd_version: OnceLock<String>,
    /// Cached static device information per UUID
    device_static_info: OnceLock<HashMap<String, DeviceStaticInfo>>,
}

impl Default for RebellionsNpuReader {
    fn default() -> Self {
        Self::new()
    }
}

impl RebellionsNpuReader {
    pub fn new() -> Self {
        Self {
            kmd_version: OnceLock::new(),
            device_static_info: OnceLock::new(),
        }
    }

    /// Initialize static device cache on first access
    fn ensure_static_cache_initialized(&self, response: &RblnResponse) {
        // Initialize KMD version
        self.kmd_version
            .get_or_init(|| response.kmd_version.clone());

        // Initialize device static info
        self.device_static_info.get_or_init(|| {
            let mut device_map = HashMap::new();
            // Use common MAX_DEVICES constant from common_cache module
            const MAX_DEVICES: usize = crate::device::readers::common_cache::MAX_DEVICES;
            let devices_to_process: Vec<_> = response.devices.iter().take(MAX_DEVICES).collect();

            for device in devices_to_process {
                // Build detail HashMap using DetailBuilder
                let detail = DetailBuilder::new()
                    .insert("Serial ID", &device.sid)
                    .insert("Firmware Version", &device.fw_ver)
                    .insert("Device Path", &device.device)
                    .insert("Board Info", &device.board_info)
                    .insert("Location", device.location.to_string())
                    .insert_pci_info(
                        Some(&device.pci.bus_id),
                        None, // Rebellions doesn't provide PCIe generation separately
                        Some(&device.pci.link_width),
                    )
                    .insert("PCI Link Speed", &device.pci.link_speed)
                    .insert("PCI NUMA Node", &device.pci.numa_node)
                    .build();

                let static_info = DeviceStaticInfo::with_details(
                    device.name.clone(),
                    Some(device.uuid.clone()),
                    detail,
                );

                device_map.insert(device.uuid.clone(), static_info);
            }
            device_map
        });
    }

    /// Get cached KMD version
    fn get_kmd_version(&self) -> Option<String> {
        self.kmd_version.get().cloned()
    }

    /// Get cached static device info
    fn get_device_static_info(&self, uuid: &str) -> Option<&DeviceStaticInfo> {
        self.device_static_info.get().and_then(|map| map.get(uuid))
    }

    /// Determine which command to use (rbln-stat or rbln-smi)
    fn get_rebellions_command() -> Option<(String, PathBuf)> {
        // Check cache first
        if let Ok(cache) = RBLN_COMMAND_CACHE.lock()
            && let Some(ref cached) = *cache
        {
            return Some(cached.clone());
        }

        // Check specific paths first
        const PATHS: &[&str] = &[
            "/usr/local/bin/rbln-stat",
            "/usr/bin/rbln-stat",
            "/usr/local/bin/rbln-smi",
            "/usr/bin/rbln-smi",
        ];

        for path in PATHS {
            if Path::new(path).exists() {
                let cmd_name = Path::new(path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("rbln-stat")
                    .to_string();
                let result = (cmd_name, PathBuf::from(path));

                // Cache the result
                if let Ok(mut cache) = RBLN_COMMAND_CACHE.lock() {
                    *cache = Some(result.clone());
                }

                return Some(result);
            }
        }

        // Check if commands are available in PATH
        for cmd in &["rbln-stat", "rbln-smi"] {
            if let Ok(output) = execute_command_default("which", &[cmd])
                && output.status == 0
                && let Some(path) = absolute_command_path(&output.stdout)
            {
                let result = (cmd.to_string(), path);

                // Cache the result
                if let Ok(mut cache) = RBLN_COMMAND_CACHE.lock() {
                    *cache = Some(result.clone());
                }

                return Some(result);
            }
        }

        None
    }

    /// Get NPU info using rbln-stat or rbln-smi
    fn get_npu_info_internal(&self) -> Vec<GpuInfo> {
        let (_command, path) = match Self::get_rebellions_command() {
            Some(cmd) => cmd,
            None => return Vec::new(),
        };

        // Validate path before execution to prevent path traversal
        let path_str = match path.to_str() {
            Some(s) if path.is_absolute() && !s.contains("..") => s,
            Some(s) => {
                eprintln!("Suspicious path detected: {s}");
                return Vec::new();
            }
            None => {
                eprintln!("Invalid path for Rebellions command");
                return Vec::new();
            }
        };

        let output = match execute_command_default(path_str, &["--json"]) {
            Ok(output) => output,
            Err(_) => return Vec::new(),
        };

        let response: RblnResponse = match serde_json::from_str(&output.stdout) {
            Ok(resp) => resp,
            Err(_) => return Vec::new(),
        };

        let time = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
        let hostname = get_hostname();

        self.gpu_info_from_response(response, &time, &hostname)
    }

    /// Convert one parsed `rbln-stat --json` response into one `GpuInfo` per
    /// device entry.
    ///
    /// This is the whole conversion step of a poll with the command execution
    /// taken out, so tests drive the exact path production takes: the static
    /// cache is initialised from the first response it sees, and every later
    /// response reads its static details from that cache.
    fn gpu_info_from_response(
        &self,
        response: RblnResponse,
        time: &str,
        hostname: &str,
    ) -> Vec<GpuInfo> {
        // Initialize static cache on first call
        self.ensure_static_cache_initialized(&response);

        let kmd_version = self
            .get_kmd_version()
            .unwrap_or_else(|| response.kmd_version.clone());

        // Decided over the whole poll, since a die's role depends on its
        // siblings.
        let roles = card_power_roles(&response.devices);

        response
            .devices
            .into_iter()
            .zip(roles)
            .filter_map(|(device, role)| {
                // Try to get cached static info, fall back to current device data if not available
                let static_info = self.get_device_static_info(&device.uuid);

                create_gpu_info_from_device(device, static_info, &kmd_version, role, time, hostname)
            })
            .collect()
    }

    /// Get process info from rbln-stat/rbln-smi
    fn get_process_info_internal(&self) -> Vec<ProcessInfo> {
        let (_command, path) = match Self::get_rebellions_command() {
            Some(cmd) => cmd,
            None => return Vec::new(),
        };

        // Validate path before execution to prevent path traversal
        let path_str = match path.to_str() {
            Some(s) if path.is_absolute() && !s.contains("..") => s,
            Some(s) => {
                eprintln!("Suspicious path detected: {s}");
                return Vec::new();
            }
            None => {
                eprintln!("Invalid path for Rebellions command");
                return Vec::new();
            }
        };

        let output = match execute_command_default(path_str, &["--json"]) {
            Ok(output) => output,
            Err(_) => return Vec::new(),
        };

        let response: RblnResponse = match serde_json::from_str(&output.stdout) {
            Ok(resp) => resp,
            Err(_) => return Vec::new(),
        };

        let uuid_by_npu: std::collections::HashMap<u32, String> = response
            .devices
            .iter()
            .map(|device| (device.npu, device.uuid.clone()))
            .collect();

        response
            .contexts
            .into_iter()
            .map(|ctx| create_process_info_from_context(ctx, &uuid_by_npu))
            .collect()
    }
}

impl GpuReader for RebellionsNpuReader {
    fn get_gpu_info(&self) -> Vec<GpuInfo> {
        self.get_npu_info_internal()
    }

    fn get_process_info(&self) -> Vec<ProcessInfo> {
        self.get_process_info_internal()
    }
}

// Helper functions

/// How one die's row accounts for the power of the card it sits on.
#[derive(Debug, Clone, Copy, PartialEq)]
struct CardPowerRole {
    /// The row carries the card's power in `power_consumption`. Exactly one
    /// die per card does; the others read `GPU_METRIC_UNAVAILABLE`, so every
    /// sum over `power_consumption` counts the card once.
    reports_card_power: bool,
    /// The card's power in watts as counted on its reporting die, published
    /// under [`CARD_POWER_WATTS_DETAIL_KEY`] on every die of a card with
    /// more than one die. `None` on a single-die card, whose
    /// `power_consumption` already shows it, and when the reporting die's
    /// `card_power` does not parse.
    shared_card_watts: Option<f64>,
}

impl CardPowerRole {
    /// A die that is a whole card on its own (every ATOM Plus device).
    const SINGLE_DIE_CARD: Self = Self {
        reports_card_power: true,
        shared_card_watts: None,
    };
}

/// Decide, for every device of one poll, which die reports its card's power.
///
/// `rbln-stat` enumerates dies, not cards, and every die repeats its card's
/// `card_power`. ATOM Plus has one die per card, so one row per die is also
/// one row per card; ATOM Max has four, and summing every row counted each
/// card four times.
///
/// Dies are grouped into cards by `sid`, the board serial: the only key that
/// yields one group per physical card on both ATOM Plus and ATOM Max.
/// `group_id` puts every ATOM Plus device into a single group, `location` is
/// a die position within a board, and `npu` repeats once per card on ATOM
/// Max. A device whose `sid` is empty has no siblings to match and is treated
/// as its own card.
///
/// Within a card the reporting die is the one with the lowest kernel device
/// index parsed from `device` (`"rbln12"` is 12). A die whose name does not
/// parse sorts after every die that does, and ties fall back to the order
/// `rbln-stat` listed them in. The rule keys on the device rather than on its
/// list position because `rbln-stat` does not list devices in index order
/// (the ATOM Plus capture reads rbln0, rbln2, rbln3, ...), and the reporter
/// must not hop between dies from one poll to the next: its
/// `all_smi_gpu_power_consumption_watts` series and its energy-counter key
/// are per device uuid, and both have to stay continuous.
fn card_power_roles(devices: &[RblnDevice]) -> Vec<CardPowerRole> {
    let mut roles = vec![CardPowerRole::SINGLE_DIE_CARD; devices.len()];

    let mut cards: HashMap<&str, Vec<usize>> = HashMap::new();
    for (position, device) in devices.iter().enumerate() {
        let sid = device.sid.trim();
        if !sid.is_empty() {
            cards.entry(sid).or_default().push(position);
        }
    }

    for dies in cards.values().filter(|dies| dies.len() > 1) {
        // `Option` orders `None` first, the opposite of the rule, so the
        // key puts "did not parse" in front explicitly.
        let Some(&reporter) = dies.iter().min_by_key(|&&position| {
            let index = kernel_device_index(&devices[position].device);
            (index.is_none(), index.unwrap_or(0), position)
        }) else {
            continue;
        };
        let card_watts = parse_power(&devices[reporter].card_power)
            .filter(|watts| watts.is_finite() && *watts >= 0.0);
        for &position in dies {
            roles[position] = CardPowerRole {
                reports_card_power: position == reporter,
                shared_card_watts: card_watts,
            };
        }
    }

    roles
}

/// Kernel device index of an `rbln<N>` device name, or `None` for any other
/// spelling.
fn kernel_device_index(device: &str) -> Option<u32> {
    device.trim().strip_prefix("rbln")?.parse().ok()
}

fn create_gpu_info_from_device(
    device: RblnDevice,
    static_info: Option<&DeviceStaticInfo>,
    kmd_version: &str,
    role: CardPowerRole,
    time: &str,
    hostname: &str,
) -> Option<GpuInfo> {
    // Use cached static info if available, otherwise build from current device data
    let (uuid, name, mut detail) = if let Some(info) = static_info {
        (
            info.uuid.clone().unwrap_or_else(|| device.uuid.clone()),
            info.name.clone(),
            info.detail.clone(),
        )
    } else {
        // Build detail HashMap if no cache available (first call)
        let detail = DetailBuilder::new()
            .insert("Serial ID", &device.sid)
            .insert("Firmware Version", &device.fw_ver)
            .insert("Device Path", &device.device)
            .insert("Board Info", &device.board_info)
            .insert("Location", device.location.to_string())
            .insert_pci_info(Some(&device.pci.bus_id), None, Some(&device.pci.link_width))
            .insert("PCI Link Speed", &device.pci.link_speed)
            .insert("PCI NUMA Node", &device.pci.numa_node)
            .build();

        (device.uuid.clone(), device.name.clone(), detail)
    };

    // Add KMD version (might be updated between calls)
    detail.insert("KMD Version".to_string(), kmd_version.to_string());

    // Dynamic values
    detail.insert("Status".to_string(), device.status.clone());
    detail.insert("Performance State".to_string(), device.pstate.clone());
    // The card value shown on every die of a multi-die card. Dynamic, so it
    // is inserted here on every poll and never lands in the static cache.
    if let Some(watts) = role.shared_card_watts {
        detail.insert(
            CARD_POWER_WATTS_DETAIL_KEY.to_string(),
            format!("{watts:.2}"),
        );
    }

    // Add unified AI acceleration library labels
    detail.insert("lib_name".to_string(), "RBLN-SDK".to_string());
    detail.insert("lib_version".to_string(), kmd_version.to_string());

    // Parse dynamic metrics
    let temperature = parse_temp_safe(&device.temperature);
    // `card_power` is per card; only the card's reporting die counts it.
    let power = if role.reports_card_power {
        parse_power_safe(&device.card_power)
    } else {
        GPU_METRIC_UNAVAILABLE
    };
    let utilization = parse_util_safe(&device.util);
    let (used_memory, total_memory) = parse_memory(&device.memory);

    Some(GpuInfo {
        uuid,
        time: time.to_string(),
        name,
        device_type: "NPU".to_string(),
        host_id: hostname.to_string(),
        hostname: hostname.to_string(),
        instance: hostname.to_string(),
        utilization,
        ane_utilization: 0.0,
        dla_utilization: None,
        tensorcore_utilization: None,
        temperature,
        used_memory,
        total_memory,
        frequency: 0, // Rebellions doesn't report frequency
        power_consumption: power,
        gpu_core_count: None,
        // Rebellions NPUs expose a single temperature sensor without NVML-style
        // threshold / P-state or NVIDIA hardware-detail
        // (NUMA/GSP/NvLink/GPM) metadata. Leave the new fields unavailable.
        temperature_threshold_slowdown: None,
        temperature_threshold_shutdown: None,
        temperature_threshold_max_operating: None,
        temperature_threshold_acoustic: None,
        performance_state: None,
        fan_speed_rpm: None,
        numa_node_id: None,
        gsp_firmware_mode: None,
        gsp_firmware_version: None,
        nvlink_remote_devices: Vec::new(),
        gpm_metrics: None,
        detail,
    })
}

fn create_process_info_from_context(
    ctx: RblnContext,
    uuid_by_npu: &std::collections::HashMap<u32, String>,
) -> ProcessInfo {
    let used_memory = parse_rbln_memory_bytes(&ctx.memalloc).unwrap_or_else(|| {
        eprintln!(
            "Failed to parse memory for process {}: {}",
            ctx.pid, ctx.memalloc
        );
        0
    });

    // Join back to the device that owns the context. On ATOM Max `npu` repeats
    // once per card, so this can be ambiguous there -- rbln-stat's JSON exposes
    // no per-context card key to disambiguate with.
    let device_uuid = uuid_by_npu
        .get(&ctx.npu)
        .cloned()
        .unwrap_or_else(|| format!("rbln{}", ctx.npu));

    ProcessInfo {
        device_id: ctx.npu as usize,
        device_uuid,
        pid: ctx.pid,
        process_name: extract_process_name(&ctx.process),
        used_memory,
        cpu_percent: 0.0,
        memory_percent: 0.0,
        memory_rss: 0,
        memory_vms: 0,
        user: String::new(),
        state: String::new(),
        start_time: String::new(),
        cpu_time: 0,
        command: ctx.process,
        ppid: 0,
        threads: 0,
        uses_gpu: true,
        priority: 0,
        nice_value: 0,
        gpu_utilization: 0.0,
    }
}

// Helper function to parse temperature with fallback
fn parse_temp_safe(temp_str: &str) -> u32 {
    parse_temperature(temp_str).unwrap_or_else(|| {
        eprintln!("Failed to parse temperature: {temp_str}");
        0
    })
}

// Helper function to parse power with fallback
fn parse_power_safe(power_str: &str) -> f64 {
    parse_power(power_str).unwrap_or_else(|| {
        eprintln!("Failed to parse power: {power_str}");
        0.0
    })
}

// Helper function to parse utilization with fallback
fn parse_util_safe(util_str: &str) -> f64 {
    parse_utilization(util_str).unwrap_or_else(|| {
        eprintln!("Failed to parse utilization: {util_str}");
        0.0
    })
}

/// Parse a memory value reported by `rbln-stat` / `rbln-smi` into bytes.
///
/// The tool reports a bare byte count with no unit suffix: a 16 GB ATOM Plus
/// card reads `"total": "16877879296"` (15.72 GiB). Routing that through the
/// shared `parse_memory_mb_to_bytes` helper multiplied it by 1 MiB a second
/// time and reported 15.72 PiB per card — large enough to be obviously wrong
/// on inspection, small enough not to overflow, so it rendered silently.
///
/// An explicit `MB` / `MiB` suffix is still honoured so that an SDK release
/// which starts labelling the unit is not read as a byte count.
fn parse_rbln_memory_bytes(mem_str: &str) -> Option<u64> {
    // Device-level fields are bare byte counts; per-context `memalloc` is a
    // human-readable string like "3.5GiB". Longest suffixes first so that "MB"
    // is never shadowed by "B".
    const UNITS: &[(&str, u64)] = &[
        ("TiB", 1 << 40),
        ("GiB", 1 << 30),
        ("MiB", 1 << 20),
        ("KiB", 1 << 10),
        ("TB", 1 << 40),
        ("GB", 1 << 30),
        ("MB", 1 << 20),
        ("KB", 1 << 10),
        ("B", 1),
    ];

    let value = mem_str.trim();

    for (suffix, multiplier) in UNITS {
        if let Some(number) = value.strip_suffix(suffix) {
            let scaled = number.trim().parse::<f64>().ok()? * (*multiplier as f64);
            // `as u64` saturates rather than wrapping, so reject out-of-range
            // values explicitly instead of silently clamping to u64::MAX.
            // `u64::MAX as f64` rounds up to 2^64, so an equality check against
            // that value is also out of range even though `u64::MAX` itself is
            // valid. Bare integer inputs bypass f64 and retain exact support
            // for the full u64 range.
            if !scaled.is_finite() || scaled < 0.0 || scaled >= 2_f64.powi(64) {
                return None;
            }
            return Some(scaled as u64);
        }
    }

    value.parse::<u64>().ok()
}

fn parse_memory(mem: &RblnMemoryInfo) -> (u64, u64) {
    let used = parse_rbln_memory_bytes(&mem.used).unwrap_or_else(|| {
        eprintln!("Failed to parse used memory: {}", mem.used);
        0
    });

    let total = parse_rbln_memory_bytes(&mem.total).unwrap_or_else(|| {
        eprintln!("Failed to parse total memory: {}", mem.total);
        0
    });

    (used, total)
}

fn extract_process_name(cmd: &str) -> String {
    cmd.split_whitespace()
        .next()
        .and_then(|path| path.split('/').next_back())
        .unwrap_or("unknown")
        .to_string()
}

fn absolute_command_path(which_stdout: &str) -> Option<PathBuf> {
    let path = PathBuf::from(
        which_stdout
            .lines()
            .find(|line| !line.trim().is_empty())?
            .trim(),
    );
    let path_str = path.to_str()?;
    (path.is_absolute() && !path_str.contains("..")).then_some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verbatim `rbln-stat --json` output from a live 8x RBLN-CA22 (ATOM Plus)
    /// node running KMD 3.0.0, trimmed to the first two devices. Key names,
    /// value spellings and JSON types are exactly as the tool emits them:
    /// `npu` and `location` are numbers, every measurement is a string,
    /// `memory` is a bare byte count and `card_power` is in microwatts.
    const ATOM_PLUS_JSON: &str = r#"{
      "KMD_version": "3.0.0",
      "devices": [
        {
          "npu": 0,
          "name": "RBLN-CA22",
          "sid": "0000000022513338",
          "uuid": "4126c167-c7a6-4d0a-80dd-ffbf3641d1b0",
          "device": "rbln0",
          "status": "normal",
          "fw_ver": "3.0.0",
          "pci": {
            "dev": "0x1220",
            "bus_id": "0000:03:00.0",
            "numa_node": "0",
            "link_speed": "32.0GT/s",
            "link_width": "16"
          },
          "temperature": "31C",
          "card_power": "17521800uW",
          "pstate": "P14",
          "memory": {
            "used": "0",
            "total": "16877879296"
          },
          "util": "0.0",
          "board_info": "0005000c",
          "location": 5
        },
        {
          "npu": 1,
          "name": "RBLN-CA22",
          "sid": "0000000022513339",
          "uuid": "a58a772b-1a27-4df3-823d-bd1d26627f74",
          "device": "rbln1",
          "status": "normal",
          "fw_ver": "3.0.0",
          "pci": {
            "dev": "0x1220",
            "bus_id": "0000:04:00.0",
            "numa_node": "0",
            "link_speed": "32.0GT/s",
            "link_width": "16"
          },
          "temperature": "33C",
          "card_power": "18307255uW",
          "pstate": "P14",
          "memory": {
            "used": "0",
            "total": "16877879296"
          },
          "util": "0.0",
          "board_info": "0005000c",
          "location": 5
        }
      ],
      "contexts": []
    }"#;

    /// `rbln-stat -g -j` on the same node: identical apart from a per-device
    /// `group_id` the reader has no field for. Deserialization must keep
    /// ignoring unknown keys.
    const ATOM_PLUS_GROUPED_JSON: &str = r#"{
      "KMD_version": "3.0.0",
      "devices": [
        {
          "npu": 0,
          "name": "RBLN-CA22",
          "sid": "0000000022513338",
          "uuid": "4126c167-c7a6-4d0a-80dd-ffbf3641d1b0",
          "device": "rbln0",
          "status": "normal",
          "fw_ver": "3.0.0",
          "group_id": "1",
          "pci": {
            "dev": "0x1220",
            "bus_id": "0000:03:00.0",
            "numa_node": "0",
            "link_speed": "32.0GT/s",
            "link_width": "16"
          },
          "temperature": "31C",
          "card_power": "17521800uW",
          "pstate": "P14",
          "memory": {
            "used": "0",
            "total": "16877879296"
          },
          "util": "0.0",
          "board_info": "0005000c",
          "location": 5
        }
      ],
      "contexts": []
    }"#;

    /// Total HBM of one RBLN-CA22, in bytes (15.72 GiB).
    const ATOM_PLUS_TOTAL_MEMORY_BYTES: u64 = 16_877_879_296;

    fn parse_response(json: &str) -> RblnResponse {
        serde_json::from_str(json).expect("rbln-stat output must deserialize")
    }

    fn gpu_info_at(json: &str, index: usize) -> GpuInfo {
        let response = parse_response(json);
        let kmd_version = response.kmd_version.clone();
        let device = response
            .devices
            .into_iter()
            .nth(index)
            .expect("device index in range");

        create_gpu_info_from_device(
            device,
            None,
            &kmd_version,
            CardPowerRole::SINGLE_DIE_CARD,
            "2025-09-12 11:18:00",
            "atom-plus-01",
        )
        .expect("device must convert to GpuInfo")
    }

    #[test]
    fn real_output_deserializes() {
        let response = parse_response(ATOM_PLUS_JSON);
        assert_eq!(response.kmd_version, "3.0.0");
        assert_eq!(response.devices.len(), 2);
        assert_eq!(response.devices[0].name, "RBLN-CA22");
        assert_eq!(response.devices[0].device, "rbln0");
        assert!(response.contexts.is_empty());
    }

    #[test]
    fn malformed_context_does_not_hide_devices_or_valid_processes() {
        let json = ATOM_PLUS_JSON.replace("\"contexts\": []", r#""contexts": [{"ctx_id":"ok","npu":0,"process":"worker","pid":"42","memalloc":"1MiB"},{"ctx_id":"bad","npu":{"unexpected":true},"process":"worker","pid":"43","memalloc":"1MiB"}]"#);
        let response = parse_response(&json);
        assert_eq!(
            response.devices.len(),
            2,
            "device rows must remain available"
        );
        assert_eq!(
            response.contexts.len(),
            1,
            "only the malformed context is skipped"
        );
    }

    #[test]
    fn grouped_output_deserializes_despite_the_extra_group_id() {
        let response = parse_response(ATOM_PLUS_GROUPED_JSON);
        assert_eq!(response.devices.len(), 1);
        assert_eq!(
            response.devices[0].uuid,
            "4126c167-c7a6-4d0a-80dd-ffbf3641d1b0"
        );
    }

    /// Regression: `memory.total` is a bare byte count, but it used to go
    /// through `parse_memory_mb_to_bytes`, which multiplied it by 1 MiB a
    /// second time and reported 15.72 PiB per card.
    #[test]
    fn memory_is_read_as_bytes_not_mebibytes() {
        let info = gpu_info_at(ATOM_PLUS_JSON, 0);

        assert_eq!(info.total_memory, ATOM_PLUS_TOTAL_MEMORY_BYTES);
        assert_eq!(info.used_memory, 0);
        assert!(
            info.total_memory < 1 << 40,
            "a 16 GB card must not report {} bytes (>= 1 TiB)",
            info.total_memory
        );
    }

    /// Regression: `card_power` is in microwatts. `parse_power` left the `u`
    /// behind, the parse failed, and the fallback reported 0.0 W for every
    /// Rebellions NPU ever polled.
    #[test]
    fn card_power_microwatts_are_read_as_watts() {
        let first = gpu_info_at(ATOM_PLUS_JSON, 0);
        assert!(
            (first.power_consumption - 17.5218).abs() < 1e-9,
            "expected ~17.52 W, got {}",
            first.power_consumption
        );
        assert!(
            first.power_consumption > 1.0,
            "an idling ATOM Plus draws ~17.5 W, never 0.0 W"
        );

        let second = gpu_info_at(ATOM_PLUS_JSON, 1);
        assert!(
            (second.power_consumption - 18.307255).abs() < 1e-9,
            "expected ~18.31 W, got {}",
            second.power_consumption
        );
    }

    #[test]
    fn temperature_and_utilization_are_read() {
        let first = gpu_info_at(ATOM_PLUS_JSON, 0);
        assert_eq!(first.temperature, 31);
        assert_eq!(first.utilization, 0.0);
        assert_eq!(first.device_type, "NPU");
        assert_eq!(first.name, "RBLN-CA22");

        let second = gpu_info_at(ATOM_PLUS_JSON, 1);
        assert_eq!(second.temperature, 33);
    }

    /// The detail keys the Prometheus exporter reads. These are a contract
    /// between `readers::rebellions` and `api::metrics::npu::rebellions`;
    /// renaming one without the other silently empties the metrics.
    #[test]
    fn detail_carries_the_keys_the_exporter_reads() {
        let info = gpu_info_at(ATOM_PLUS_JSON, 0);

        assert_eq!(
            info.detail.get("Firmware Version").map(String::as_str),
            Some("3.0.0")
        );
        assert_eq!(
            info.detail.get("KMD Version").map(String::as_str),
            Some("3.0.0")
        );
        assert_eq!(
            info.detail.get("Serial ID").map(String::as_str),
            Some("0000000022513338")
        );
        assert_eq!(
            info.detail.get("Status").map(String::as_str),
            Some("normal")
        );
        assert_eq!(
            info.detail.get("Performance State").map(String::as_str),
            Some("P14")
        );
        assert_eq!(info.detail.get("Location").map(String::as_str), Some("5"));
        assert_eq!(
            info.detail.get("lib_name").map(String::as_str),
            Some("RBLN-SDK")
        );
    }

    /// The cached path (second poll onwards) must carry the same static
    /// details as the uncached one.
    #[test]
    fn cached_static_info_matches_the_uncached_path() {
        let reader = RebellionsNpuReader::new();
        let response = parse_response(ATOM_PLUS_JSON);
        reader.ensure_static_cache_initialized(&response);

        let cached = reader
            .get_device_static_info("4126c167-c7a6-4d0a-80dd-ffbf3641d1b0")
            .expect("device must be cached by uuid");

        assert_eq!(cached.name, "RBLN-CA22");
        assert_eq!(
            cached.detail.get("Serial ID").map(String::as_str),
            Some("0000000022513338")
        );
        assert_eq!(cached.detail.get("Location").map(String::as_str), Some("5"));
        assert_eq!(reader.get_kmd_version().as_deref(), Some("3.0.0"));
    }

    #[test]
    fn memory_parser_accepts_bare_bytes_and_explicit_suffixes() {
        assert_eq!(
            parse_rbln_memory_bytes("16877879296"),
            Some(ATOM_PLUS_TOTAL_MEMORY_BYTES)
        );
        assert_eq!(parse_rbln_memory_bytes(" 0 "), Some(0));
        // Honoured in case a future SDK starts labelling the unit.
        assert_eq!(parse_rbln_memory_bytes("1024MiB"), Some(1024 * 1024 * 1024));
        assert_eq!(parse_rbln_memory_bytes("1024MB"), Some(1024 * 1024 * 1024));
        assert_eq!(parse_rbln_memory_bytes("not a number"), None);
        // No panic on a value that would overflow when scaled.
        assert_eq!(parse_rbln_memory_bytes("18446744073709551615MB"), None);
        assert_eq!(parse_rbln_memory_bytes("18446744073709551616B"), None);
        assert_eq!(
            parse_rbln_memory_bytes("18446744073709551615"),
            Some(u64::MAX)
        );
    }

    #[test]
    fn process_name_is_the_executable_basename() {
        assert_eq!(extract_process_name("/usr/bin/python3 train.py"), "python3");
        assert_eq!(extract_process_name("rbln-serve"), "rbln-serve");
    }

    #[test]
    fn command_discovery_keeps_only_safe_absolute_paths() {
        // A rooted path without a drive letter is not absolute on Windows, so
        // `which` output for the Linux-only rbln tools is only accepted on Unix.
        #[cfg(unix)]
        assert_eq!(
            absolute_command_path("/opt/rebellions/bin/rbln-stat\n"),
            Some(PathBuf::from("/opt/rebellions/bin/rbln-stat"))
        );
        assert_eq!(absolute_command_path("rbln-stat\n"), None);
        assert_eq!(absolute_command_path("/opt/../tmp/rbln-stat\n"), None);
        assert_eq!(absolute_command_path("\n"), None);
    }
}

#[cfg(all(test, feature = "cli"))]
#[path = "rebellions_atom_max_tests.rs"]
mod atom_max_tests;

#[cfg(test)]
mod loaded_node_tests {
    use super::*;

    /// Verbatim `rbln-stat --json` from an 8-card ATOM Plus node running vLLM.
    /// Before the `RblnContext` fields were corrected, this input failed to
    /// deserialize entirely, so `get_npu_info` returned an empty vector and
    /// every device disappeared from all-smi the moment a workload started.
    const LOADED: &str = include_str!("../../../tests/fixtures/rbln-stat-loaded.json");

    fn parse() -> RblnResponse {
        serde_json::from_str(LOADED).expect("output from a busy node must deserialize")
    }

    #[test]
    fn devices_survive_when_contexts_are_present() {
        let response = parse();
        assert_eq!(
            response.devices.len(),
            8,
            "devices must not vanish under load"
        );
        assert_eq!(response.contexts.len(), 8);
    }

    #[test]
    fn context_fields_match_what_the_tool_emits() {
        let response = parse();
        // npu is a JSON integer, pid a quoted string, and the process and
        // memory keys are `process` / `memalloc` -- all four differed from
        // what the struct used to declare.
        let ctx = response
            .contexts
            .iter()
            .find(|c| c.ctx_id == "10001")
            .expect("vLLM engine context");
        assert_eq!(ctx.npu, 0);
        assert_eq!(ctx.pid, 2733390);
        assert_eq!(ctx.process, "VLLM::EngineCore");
        assert_eq!(ctx.memalloc, "3.5GiB");
    }

    #[test]
    fn context_memory_is_parsed_from_its_human_readable_suffix() {
        assert_eq!(parse_rbln_memory_bytes("3.5GiB"), Some(3_758_096_384));
        assert_eq!(parse_rbln_memory_bytes("72.0MiB"), Some(75_497_472));
        assert_eq!(parse_rbln_memory_bytes("58.0MiB"), Some(60_817_408));
    }

    #[test]
    fn processes_are_joined_to_the_device_that_owns_them() {
        let response = parse();
        let uuid_by_npu: std::collections::HashMap<u32, String> = response
            .devices
            .iter()
            .map(|device| (device.npu, device.uuid.clone()))
            .collect();
        let expected_uuid = uuid_by_npu.get(&0).cloned().expect("device 0");

        let processes: Vec<ProcessInfo> = response
            .contexts
            .into_iter()
            .map(|ctx| create_process_info_from_context(ctx, &uuid_by_npu))
            .collect();

        assert_eq!(processes.len(), 8);
        let engine = processes
            .iter()
            .find(|p| p.pid == 2733390 && p.used_memory > 1 << 30)
            .expect("vLLM engine process");
        assert_eq!(engine.process_name, "VLLM::EngineCore");
        assert_eq!(engine.used_memory, 3_758_096_384);
        assert_eq!(engine.device_id, 0);
        assert_eq!(engine.device_uuid, expected_uuid);
        assert!(engine.uses_gpu);
    }
}
