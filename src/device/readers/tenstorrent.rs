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
use crate::device::process_list::{get_all_processes, merge_gpu_processes};
use crate::device::readers::common_cache::{DetailBuilder, DeviceStaticInfo};
use crate::device::types::{GpuInfo, ProcessInfo};
use crate::utils::{get_hostname, with_global_system};
use chrono::Local;
use luwen_api::ChipDetectOptions;
use luwen_api::chip::{Chip, ChipImpl, Telemetry};
use luwen_def::Arch;
use luwen_pci::detect_chips_silent;
use once_cell::sync::Lazy;
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

/// Collection method for Tenstorrent NPU metrics
#[derive(Debug, Clone, Copy)]
pub enum CollectionMethod {
    /// Read directly from device files in /dev
    DeviceFile,
}

/// Configuration for Tenstorrent reader
pub struct TenstorrentConfig {
    /// Primary method to use for collecting metrics (reserved for future use)
    pub _primary_method: CollectionMethod,
}

impl Default for TenstorrentConfig {
    fn default() -> Self {
        Self {
            _primary_method: CollectionMethod::DeviceFile,
        }
    }
}

// Global status for error messages
static TENSTORRENT_STATUS: Mutex<Option<String>> = Mutex::new(None);

// Tenstorrent-specific static device information
#[derive(Clone)]
struct TenstorrentStaticInfo {
    total_memory: u64,
    tdp_limit: f64,
}

// Cache entry containing both chip and its static info
struct CachedChipInfo {
    chip: Chip,
    static_info: DeviceStaticInfo,
    tenstorrent_info: TenstorrentStaticInfo,
}

// Cache for initialized chips and their static info to avoid re-initialization on every measurement
static INITIALIZED_CHIPS: Lazy<Mutex<Option<Vec<CachedChipInfo>>>> = Lazy::new(|| Mutex::new(None));

pub struct TenstorrentReader {
    _config: TenstorrentConfig,
}

impl Default for TenstorrentReader {
    fn default() -> Self {
        Self::new()
    }
}

impl TenstorrentReader {
    pub fn new() -> Self {
        Self {
            _config: TenstorrentConfig::default(),
        }
    }

    #[allow(dead_code)]
    pub fn with_config(config: TenstorrentConfig) -> Self {
        Self { _config: config }
    }

    /// Get or initialize chips with caching
    fn ensure_chips_initialized() {
        let mut chips_guard = match INITIALIZED_CHIPS.lock() {
            Ok(guard) => guard,
            Err(e) => {
                eprintln!("Failed to acquire lock for Tenstorrent chips: {e}");
                return;
            }
        };

        if chips_guard.is_some() {
            return;
        }

        // luwen 0.8.x has sunset Grayskull and panics (`unimplemented!()` in luwen-kmd)
        // when it opens one. Detection opens every node under /dev/tenstorrent, so a
        // single Grayskull card would crash collection for the whole host. Detect it
        // from sysfs *before* calling luwen and skip detection entirely. This is what
        // protects the release binary, where `panic = "abort"` makes the catch_unwind
        // below unable to recover.
        if grayskull_present() {
            set_tenstorrent_status(
                "Skipped Tenstorrent detection: Grayskull is no longer supported by the luwen 0.8.x backend".to_string(),
            );
            *chips_guard = Some(Vec::new());
            return;
        }

        // Detect and initialize chips. The Grayskull pre-check above already returned,
        // so luwen should not reach its sunset `unimplemented!()` path here. Keep the
        // call wrapped in `catch_unwind` as defense in depth: any other unexpected panic
        // inside luwen degrades to a status message instead of unwinding through the
        // reader and poisoning the chip-cache lock.
        let options = ChipDetectOptions {
            local_only: true,
            ..Default::default()
        };
        let detect_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            detect_chips_silent(options)
        }));
        let uninit_chips = match detect_result {
            Ok(Ok(chips)) => chips,
            Ok(Err(e)) => {
                set_tenstorrent_status(format!("Failed to detect Tenstorrent chips: {e}"));
                return;
            }
            Err(_) => {
                set_tenstorrent_status(
                    "Skipped Tenstorrent detection: a connected device is unsupported by luwen 0.8.x (e.g. Grayskull, which upstream has sunset)".to_string(),
                );
                return;
            }
        };

        let cached_chips: Vec<CachedChipInfo> = uninit_chips
            .into_iter()
            .filter_map(|uninit_chip| {
                // Initialize the chip
                match uninit_chip.init(&mut |_| Ok::<(), std::convert::Infallible>(())) {
                    Ok(chip) => {
                        let (static_info, tenstorrent_info) = extract_static_info(&chip)?;
                        Some(CachedChipInfo {
                            chip,
                            static_info,
                            tenstorrent_info,
                        })
                    }
                    Err(_) => None, // Drop the chip on init failure (InitError::PlatformError can occur even with an Infallible callback).
                }
            })
            .collect();

        if cached_chips.is_empty() {
            set_tenstorrent_status("No Tenstorrent chips detected".to_string());
        } else {
            clear_tenstorrent_status();
        }

        *chips_guard = Some(cached_chips);
    }

    /// Invalidate cache to force re-detection on next access
    #[allow(dead_code)]
    pub fn invalidate_cache() {
        match INITIALIZED_CHIPS.lock() {
            Ok(mut chips_guard) => {
                *chips_guard = None;
            }
            _ => {
                eprintln!("Failed to acquire lock to invalidate Tenstorrent cache");
            }
        }
    }

    /// Get NPU processes (currently returns empty - Tenstorrent doesn't provide process info)
    fn get_npu_processes(&self) -> (Vec<ProcessInfo>, HashSet<u32>) {
        (Vec::new(), HashSet::new())
    }
}

impl GpuReader for TenstorrentReader {
    fn get_gpu_info(&self) -> Vec<GpuInfo> {
        Self::ensure_chips_initialized();

        let chips_guard = match INITIALIZED_CHIPS.lock() {
            Ok(guard) => guard,
            Err(e) => {
                eprintln!("Failed to acquire lock for Tenstorrent chips: {e}");
                return Vec::new();
            }
        };
        let cached_chips = match chips_guard.as_ref() {
            Some(chips) => chips,
            None => return Vec::new(),
        };

        let time = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
        let hostname = get_hostname();

        cached_chips
            .iter()
            .enumerate()
            .filter_map(|(index, cached)| {
                create_gpu_info(
                    &cached.chip,
                    &cached.static_info,
                    &cached.tenstorrent_info,
                    index,
                    &time,
                    &hostname,
                )
            })
            .collect()
    }

    fn get_process_info(&self) -> Vec<ProcessInfo> {
        use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, UpdateKind};

        // Get NPU processes (currently empty for Tenstorrent)
        let (npu_processes, npu_pids) = self.get_npu_processes();

        // Use global system instance to avoid file descriptor leak
        let all_processes = with_global_system(|system| {
            system.refresh_processes_specifics(
                ProcessesToUpdate::All,
                true,
                ProcessRefreshKind::everything().with_user(UpdateKind::Always),
            );
            system.refresh_memory();

            // Get all system processes
            get_all_processes(system, &npu_pids)
        });

        // Merge NPU information while preserving per-device rows.
        merge_gpu_processes(all_processes, npu_processes)
    }

    fn get_gpu_processes(&self) -> (Vec<ProcessInfo>, HashSet<u32>) {
        self.get_npu_processes()
    }
}

// Helper functions

fn set_tenstorrent_status(message: String) {
    if let Ok(mut status) = TENSTORRENT_STATUS.lock() {
        *status = Some(message);
    }
}

fn clear_tenstorrent_status() {
    if let Ok(mut status) = TENSTORRENT_STATUS.lock() {
        *status = None;
    }
}

/// Returns true if a Tenstorrent Grayskull device (PCI vendor 0x1e52, device
/// 0xfaca) is present, by scanning sysfs.
///
/// luwen 0.8.x sunset Grayskull: opening such a device hits `unimplemented!()`
/// in luwen-kmd and panics. Because detection opens every /dev/tenstorrent node,
/// a single Grayskull card would crash collection for the whole host (and abort
/// outright under the release `panic = "abort"` profile). We detect it from sysfs
/// first and skip luwen entirely. The check is conservative: it only reports true
/// on a positive vendor+device match, so Wormhole (0x401e) and Blackhole (0xb140)
/// hosts are never affected, and any sysfs read failure falls through to normal
/// detection.
fn grayskull_present() -> bool {
    const TT_VENDOR: &str = "0x1e52";
    const GRAYSKULL_DEVICE: &str = "0xfaca";

    let Ok(entries) = std::fs::read_dir("/sys/bus/pci/devices") else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let vendor = std::fs::read_to_string(path.join("vendor")).unwrap_or_default();
        if vendor.trim() != TT_VENDOR {
            continue;
        }
        let device = std::fs::read_to_string(path.join("device")).unwrap_or_default();
        if device.trim() == GRAYSKULL_DEVICE {
            return true;
        }
    }
    false
}

/// Get a user-friendly message about Tenstorrent status
#[allow(dead_code)]
pub fn get_tenstorrent_status_message() -> Option<String> {
    TENSTORRENT_STATUS.lock().ok()?.clone()
}

fn extract_static_info(chip: &Chip) -> Option<(DeviceStaticInfo, TenstorrentStaticInfo)> {
    // Get telemetry
    let telem = chip.get_telemetry().ok()?;

    // Get board type name
    let board_type = telem.try_board_type().unwrap_or("Unknown");
    #[allow(deprecated)]
    // Arch::Grayskull is deprecated upstream (legacy/unsupported); keep labeling it.
    let arch_name = match telem.arch {
        Arch::Grayskull => "Grayskull",
        Arch::Wormhole => "Wormhole",
        Arch::Blackhole => "Blackhole",
    };
    let device_name = format!("Tenstorrent {arch_name} {board_type}");

    let uuid = Some(telem.board_serial_number_hex());

    // Build detail map using DetailBuilder.
    //
    // The keys are the snake_case ones `api::metrics::npu::tenstorrent` looks
    // up, the same convention the dynamic telemetry below follows: a key the
    // exporter cannot read by its own spelling makes its metric dead. These
    // are static identity, so they are not registered in
    // `detail_keys::VOLATILE_DETAIL_KEYS` and still travel as
    // `all_smi_gpu_info` labels; `sanitize_label_name` maps the Title Case
    // spellings this replaces to the same label names, so the label set
    // changes only where the key itself changed (`PCIe Generation` became
    // `pcie_link_gen`).
    let mut builder = DetailBuilder::new()
        .insert("board_type", board_type)
        .insert("board_id", telem.board_serial_number_hex())
        .insert("arc_fw_version", telem.arc_fw_version())
        .insert("eth_fw_version", telem.eth_fw_version())
        .insert("fw_date", telem.firmware_date());

    // Extract PCIe information if available
    if let Ok(Some(device_info)) = chip.get_device_info() {
        let pcie_address = format!(
            "{:04x}:{:02x}:{:02x}.{:x}",
            device_info.domain, device_info.bus, device_info.slot, device_info.function
        );
        let pcie_link_width = device_info.pcie_current_link_width().to_string();
        let pcie_link_gen = device_info.pcie_current_link_gen().to_string();

        builder = builder
            .insert("pcie_address", &pcie_address)
            .insert("pcie_vendor_id", format!("0x{:04x}", device_info.vendor))
            .insert("pcie_device_id", format!("0x{:04x}", device_info.device_id))
            .insert("pci_bus_id", &pcie_address)
            // Explicit inserts rather than `insert_pci_info`, which writes
            // the Title Case keys Rebellions still reads. Bare numbers, not
            // "Gen4"/"x16": the exporter parses these with a plain `f64`
            // parse, and `ui::topology::format_pcie` adds its own `Gen`/`x`
            // prefixes when it renders them.
            .insert("pcie_link_gen", pcie_link_gen)
            .insert("pcie_link_width", pcie_link_width);
    }

    // Extract firmware versions
    let ddr_fw_version = if telem.ddr_fw_version != 0 {
        Some(format!(
            "{}.{}.{}",
            (telem.ddr_fw_version >> 16) & 0xFF,
            (telem.ddr_fw_version >> 8) & 0xFF,
            telem.ddr_fw_version & 0xFF
        ))
    } else {
        None
    };
    builder = builder.insert_optional("ddr_fw_version", ddr_fw_version);

    let spibootrom_fw_version = if telem.spibootrom_fw_version != 0 {
        Some(format!(
            "{}.{}.{}",
            (telem.spibootrom_fw_version >> 16) & 0xFF,
            (telem.spibootrom_fw_version >> 8) & 0xFF,
            telem.spibootrom_fw_version & 0xFF
        ))
    } else {
        None
    };
    builder = builder.insert_optional("spibootrom_fw_version", spibootrom_fw_version);

    // Determine memory size and TDP based on board type
    let (total_memory, tdp_limit) = determine_memory_and_tdp(board_type);

    let detail = builder.build();

    let static_info = DeviceStaticInfo::with_details(device_name, uuid, detail);
    let tenstorrent_info = TenstorrentStaticInfo {
        total_memory,
        tdp_limit,
    };

    Some((static_info, tenstorrent_info))
}

fn determine_memory_and_tdp(board_type: &str) -> (u64, f64) {
    match board_type {
        s if s.contains("e75") => (2 * 1024 * 1024 * 1024, 75.0), // 2GB, 75W
        s if s.contains("e150") => (8 * 1024 * 1024 * 1024, 200.0), // 8GB, 200W
        s if s.contains("e300") => (12 * 1024 * 1024 * 1024, 300.0), // 12GB, 300W
        s if s.contains("galaxy") => (32 * 1024 * 1024 * 1024, 200.0), // 32GB, 200W
        s if s.contains("n150") => (48 * 1024 * 1024 * 1024, 160.0), // 48GB, 160W
        s if s.contains("n300") => (96 * 1024 * 1024 * 1024, 300.0), // 96GB, 300W
        _ => (8 * 1024 * 1024 * 1024, 200.0),                     // Default: 8GB, 200W
    }
}

fn create_gpu_info(
    chip: &Chip,
    static_info: &DeviceStaticInfo,
    tenstorrent_info: &TenstorrentStaticInfo,
    _index: usize,
    time: &str,
    hostname: &str,
) -> Option<GpuInfo> {
    // Get current telemetry
    let telem = chip.get_telemetry().ok()?;

    // Build device details
    let detail = build_device_details(static_info, tenstorrent_info, &telem);

    // Get dynamic metrics with safe defaults
    let temperature = telem.asic_temperature().round() as u32;
    let power = calculate_power(&telem);
    let frequency = telem.ai_clk();
    let utilization = estimate_utilization(&telem, tenstorrent_info.tdp_limit);

    Some(GpuInfo {
        uuid: static_info
            .uuid
            .clone()
            .unwrap_or_else(|| "Unknown".to_string()),
        time: time.to_string(),
        name: static_info.name.clone(),
        device_type: "NPU".to_string(),
        host_id: hostname.to_string(),
        hostname: hostname.to_string(),
        instance: hostname.to_string(),
        utilization,
        ane_utilization: 0.0,
        dla_utilization: None,
        tensorcore_utilization: None,
        temperature,
        used_memory: 0, // TODO: Implement memory tracking
        total_memory: tenstorrent_info.total_memory,
        frequency,
        power_consumption: power,
        gpu_core_count: None,
        // Tenstorrent telemetry does not expose NVML-style thermal
        // thresholds, P-states, or NVIDIA hardware details
        // (NUMA/GSP/NvLink/GPM); leave the extended fields unavailable.
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

fn build_device_details(
    static_info: &DeviceStaticInfo,
    _tenstorrent_info: &TenstorrentStaticInfo,
    telem: &Telemetry,
) -> HashMap<String, String> {
    // Clone the static details from DeviceStaticInfo
    let mut detail = static_info.detail.clone();

    // Dynamic telemetry.
    //
    // The keys are the snake_case ones `api::metrics::npu::tenstorrent`
    // already looks up, and the values are bare numbers because
    // `CommonNpuExporter::parse_numeric_value` is a plain `f64` parse: a
    // `"800MHz"` or `"45.0°C"` string is rejected, which is why the whole
    // `all_smi_tenstorrent_*` telemetry family used to be declared but never
    // emitted. These readings change on every poll, so they are registered in
    // `detail_keys::VOLATILE_DETAIL_KEYS` and reach Prometheus only through
    // those gauges, never as `all_smi_gpu_info` labels.
    detail.insert("voltage".to_string(), format!("{:.3}", telem.voltage()));
    detail.insert("current".to_string(), format!("{:.2}", telem.current()));
    detail.insert(
        "asic_temperature".to_string(),
        format!("{:.1}", telem.asic_temperature()),
    );
    detail.insert(
        "vreg_temperature".to_string(),
        format!("{:.1}", telem.vreg_temperature()),
    );

    if telem.board_temperature != 0 {
        detail.insert(
            "inlet_temperature".to_string(),
            format!("{:.1}", telem.inlet_temperature()),
        );
    }

    detail.insert("aiclk_mhz".to_string(), telem.ai_clk().to_string());
    detail.insert("arcclk_mhz".to_string(), telem.arc_clk().to_string());
    detail.insert("axiclk_mhz".to_string(), telem.axi_clk().to_string());

    // Counters, status registers and limits luwen carries in `Telemetry`
    // but that used to have no `detail` entry at all, so their whole
    // exporter half was declared and documented without ever firing.
    //
    // The shapes are the ones each exporter site parses:
    // `CommonNpuExporter::parse_hex_register` strips one leading `0x` and
    // accepts at most eight hex digits, so `faults`, `throttler` and
    // `ddr_status` are written as `0x` plus eight hex digits; everything
    // else goes through the strict `parse_numeric_value`, so it is a bare
    // decimal number with no unit suffix. `pcie_status` and the two
    // ethernet statuses are label values of the matching info series and
    // share the register form.
    detail.insert("faults".to_string(), format!("0x{:08x}", telem.faults));
    detail.insert(
        "throttler".to_string(),
        format!("0x{:08x}", telem.throttler),
    );
    detail.insert(
        "ddr_status".to_string(),
        format!("0x{:08x}", telem.ddr_status),
    );
    detail.insert(
        "pcie_status".to_string(),
        format!("0x{:08x}", telem.pcie_status),
    );
    detail.insert(
        "eth_status0".to_string(),
        format!("0x{:08x}", telem.eth_status0),
    );
    detail.insert(
        "eth_status1".to_string(),
        format!("0x{:08x}", telem.eth_status1),
    );

    detail.insert("arc0_health".to_string(), telem.arc0_health.to_string());
    detail.insert("arc3_health".to_string(), telem.arc3_health.to_string());
    // The raw heartbeat counter, not `telemetry_heartbeat()`: that helper
    // returns `arc0_health` on non-Blackhole arches, which this map already
    // carries under its own key.
    detail.insert("heartbeat".to_string(), telem.timer_heartbeat.to_string());
    detail.insert("fan_speed".to_string(), telem.fan_speed.to_string());
    detail.insert("fan_rpm".to_string(), telem.fan_rpm.to_string());
    detail.insert("tdp_limit".to_string(), telem.tdp.to_string());
    detail.insert("tdc_limit".to_string(), telem.tdc.to_string());
    detail.insert("thermal_limit".to_string(), telem.thm_limits.to_string());

    // Boards without the sensor carry no key at all, so the DRAM info
    // metric is omitted rather than exported with a `0` speed.
    if let Some(speed) = telem.ddr_speed {
        detail.insert("dram_speed".to_string(), speed.to_string());
    }

    // Add unified AI acceleration library labels if not already present
    detail
        .entry("lib_name".to_string())
        .or_insert("Luwen".to_string());
    if let Some(arc_fw) = detail.get("arc_fw_version") {
        detail.insert("lib_version".to_string(), arc_fw.clone());
    }

    detail
}

fn calculate_power(telem: &Telemetry) -> f64 {
    // Calculate power from voltage and current
    // Use telem.power() which internally does voltage * current
    telem.power()
}

fn estimate_utilization(telem: &Telemetry, tdp_limit: f64) -> f64 {
    // Primary method: Power-based utilization
    let power = calculate_power(telem);
    let power_utilization = (power / tdp_limit * 100.0).min(100.0);

    // Secondary method: Clock frequency based
    // Assume max AI clock is around 1000-1200 MHz for most Tenstorrent chips
    let ai_clk = telem.ai_clk() as f64;
    let max_clk = 1200.0; // Conservative max frequency
    let clock_utilization = (ai_clk / max_clk * 100.0).min(100.0);

    // Tertiary method: Heartbeat counter as activity indicator
    // The heartbeat counter increments when the chip is active
    let heartbeat = telem.telemetry_heartbeat();
    let heartbeat_active = if heartbeat > 0 { 1.0 } else { 0.0 };

    // Combine methods with weighted average
    // Power is most reliable (60%), clock is secondary (30%), heartbeat is tertiary (10%)
    (power_utilization * 0.6 + clock_utilization * 0.3 + heartbeat_active * 10.0).min(100.0)
}

#[cfg(all(test, feature = "cli"))]
#[path = "tenstorrent_telemetry_tests.rs"]
mod telemetry_tests;
