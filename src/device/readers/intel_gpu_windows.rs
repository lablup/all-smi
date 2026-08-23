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

//! Intel client GPU reader for Windows using WMI.
//!
//! Mirrors [`super::amd_windows`] closely — both readers query
//! `Win32_VideoController` and fill the same defensive `GpuInfo`
//! template. The only differences are the vendor / family filter and a
//! discrete-vs-integrated heuristic surfaced in `detail["Variant"]`.
//!
//! ## Layering
//!
//! `Win32_VideoController` publishes no utilization, no temperature, and
//! no power, so the WMI query is only a baseline that names the card.
//! Two layers stack on top of it, each outranking the last:
//!
//! 1. **WMI** — name, driver version, PNP id, and the name-derived
//!    architecture / variant classification.
//! 2. **DXGI + PDH** ([`super::windows_gpu_perf`]) — the 64-bit memory
//!    capacity WMI's 32-bit `AdapterRAM` cannot express, memory in use,
//!    and utilization. Vendor-neutral.
//! 3. **Level Zero Sysman** — temperature, power, frequency, fan.
//!
//! Layer 3 is compiled into every Windows build (the `all_smi_level_zero`
//! alias from `build.rs`, not the `level_zero` cargo feature): nothing
//! else on Windows supplies those four fields, and `ze_loader.dll` ships
//! with the Intel graphics driver. It is `dlopen`ed rather than linked, so
//! a host without the driver keeps the layer-2 readings and pays one
//! failed load for the lifetime of the process.
//!
//! Each field records where it came from in a `Source: <field>` detail
//! key, `detail["Metrics Source"]` accumulates the layers that
//! contributed (`"WMI + DXGI + PDH + Level Zero Sysman"` on a fully
//! instrumented host), and `detail["Note"]` names whatever is still
//! missing once every layer has run.

use crate::device::GpuReader;
use crate::device::readers::intel_gpu_names::{
    classify_intel_architecture, classify_intel_variant,
};
use crate::device::types::{GpuInfo, ProcessInfo};
use crate::utils::get_hostname;
use chrono::Local;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Mutex;
use wmi::WMIConnection;

// Thread-local WMI connection for reuse within the same thread —
// identical pattern to amd_windows.rs so we don't pay the COM init cost
// per request.
thread_local! {
    static WMI_CONNECTION: std::cell::RefCell<Option<WMIConnection>> =
        const { std::cell::RefCell::new(None) };
}

fn with_wmi_connection<T, F: FnOnce(&WMIConnection) -> T>(f: F) -> Option<T> {
    WMI_CONNECTION.with(|cell| {
        let mut conn_ref = cell.borrow_mut();
        if conn_ref.is_none() {
            match WMIConnection::new() {
                Ok(wmi_con) => {
                    *conn_ref = Some(wmi_con);
                }
                Err(e) => {
                    eprintln!("Intel GPU: Failed to create WMI connection: {e}");
                }
            }
        }
        conn_ref.as_ref().map(f)
    })
}

#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "PascalCase")]
struct Win32VideoController {
    name: Option<String>,
    adapter_r_a_m: Option<u64>,
    driver_version: Option<String>,
    video_processor: Option<String>,
    pnp_device_i_d: Option<String>,
    status: Option<String>,
    adapter_d_a_c_type: Option<String>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "PascalCase")]
struct VideoControllerName {
    name: Option<String>,
}

pub struct IntelWindowsGpuReader {
    /// Per-PNP-id Level Zero handle state. Keyed by `PNPDeviceID` so
    /// state persists across WMI iterations (each `get_gpu_info` call
    /// re-queries WMI, but L0 state — the energy-counter baseline in
    /// particular — must survive between calls so the delta-derived
    /// power reading is meaningful from the second refresh onward).
    /// Behind a `Mutex` because the public `&self` methods are called
    /// concurrently by the collector thread and the API server.
    #[cfg(all_smi_level_zero)]
    level_zero_state:
        Mutex<HashMap<String, crate::device::readers::intel_gpu_level_zero::LevelZeroState>>,
    /// Adapter LUID to `(device index, GPU uuid)`, recorded by
    /// `get_gpu_info` so `get_process_info` can attribute a PDH
    /// per-process row to the right card.
    adapter_index: Mutex<crate::device::readers::windows_gpu_perf::AdapterIndex>,
}

impl Default for IntelWindowsGpuReader {
    fn default() -> Self {
        Self::new()
    }
}

impl IntelWindowsGpuReader {
    pub fn new() -> Self {
        Self {
            #[cfg(all_smi_level_zero)]
            level_zero_state: Mutex::new(HashMap::new()),
            adapter_index: Mutex::new(Default::default()),
        }
    }

    fn query_intel_gpus(&self) -> Vec<GpuInfo> {
        with_wmi_connection(|wmi_con| {
            let mut gpu_list = Vec::new();

            let result: Result<Vec<Win32VideoController>, _> = wmi_con.raw_query(
                "SELECT Name, AdapterRAM, DriverVersion, VideoProcessor, PNPDeviceID, Status, AdapterDACType FROM Win32_VideoController",
            );

            if let Ok(controllers) = result {
                let hostname = get_hostname();
                let time = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();

                for (idx, controller) in controllers.iter().enumerate() {
                    let name = controller.name.clone().unwrap_or_default();
                    if !is_intel_gpu_name(&name) {
                        continue;
                    }

                    let uuid = controller
                        .pnp_device_i_d
                        .clone()
                        .unwrap_or_else(|| format!("Intel-GPU-{idx}"));

                    // LIMITATION: Win32_VideoController.AdapterRAM is a
                    // 32-bit uint32 in WMI, capped at 4GB. For an
                    // Intel Arc A770 16GB or B580 12GB the value will
                    // be clipped or wrapped — the same gotcha applies
                    // here as in amd_windows.rs. We warn on the same
                    // thresholds so downstream operators can identify
                    // it from logs.
                    let total_memory = controller.adapter_r_a_m.unwrap_or(0);
                    const FOUR_GB: u64 = 4 * 1024 * 1024 * 1024;
                    if total_memory == 0 {
                        eprintln!("Intel GPU '{name}': VRAM size unavailable (reported as 0)");
                    } else if total_memory >= FOUR_GB - (512 * 1024 * 1024) {
                        eprintln!(
                            "Intel GPU '{name}': VRAM reported as {total_memory} bytes, may be inaccurate for >4GB GPUs due to WMI 32-bit limitation"
                        );
                    }

                    let mut detail = HashMap::new();
                    if let Some(ref driver) = controller.driver_version {
                        detail.insert("Driver Version".to_string(), driver.clone());
                    }
                    if let Some(ref processor) = controller.video_processor {
                        detail.insert("Video Processor".to_string(), processor.clone());
                    }
                    if let Some(ref status) = controller.status {
                        detail.insert("Status".to_string(), status.clone());
                    }
                    if let Some(ref dac_type) = controller.adapter_d_a_c_type {
                        detail.insert("DAC Type".to_string(), dac_type.clone());
                    }
                    // `None` means the name carries a model number this
                    // table does not know. Leave the field unset rather
                    // than guessing: the DXGI memory layout fills it in
                    // `windows_gpu_perf::apply_to_gpu_info`, and it knows
                    // the answer for real (issue #364).
                    if let Some(variant) = classify_intel_variant(&name) {
                        detail.insert("Variant".to_string(), variant.to_string());
                    }
                    // Architecture / SYCL classification — shared with
                    // the Linux reader via `intel_gpu_names::classify_*`
                    // so a single source of truth drives downstream
                    // accelerator-selection logic (Backend.AI's
                    // accelerator picker, llama.cpp SYCL backend, etc.)
                    // on both Linux and Windows.
                    let arch = classify_intel_architecture(&name);
                    detail.insert("Architecture".to_string(), arch.label().to_string());
                    detail.insert(
                        "SYCL Capable".to_string(),
                        arch.sycl_capable_label().to_string(),
                    );
                    // `Metrics Source` advertises which backends
                    // produced the metrics; the DXGI/PDH and Level Zero
                    // layers append themselves as they run. The `Note`
                    // key is written afterwards by
                    // `annotate_missing_metrics`, once we know what those
                    // layers actually managed to supply.
                    detail.insert("Metrics Source".to_string(), "WMI".to_string());
                    detail.insert("Source: Utilization".to_string(), "unavailable".to_string());
                    detail.insert("Source: Temperature".to_string(), "unavailable".to_string());
                    detail.insert("Source: Power".to_string(), "unavailable".to_string());
                    detail.insert("Source: Frequency".to_string(), "unavailable".to_string());
                    detail.insert(
                        "Source: Memory".to_string(),
                        if total_memory > 0 { "WMI" } else { "unavailable" }.to_string(),
                    );
                    detail.insert("Source: Fan".to_string(), "unavailable".to_string());

                    gpu_list.push(GpuInfo {
                        uuid,
                        time: time.clone(),
                        name,
                        device_type: "GPU".to_string(),
                        host_id: hostname.clone(),
                        hostname: hostname.clone(),
                        instance: hostname.clone(),
                        utilization: 0.0,
                        ane_utilization: 0.0,
                        dla_utilization: None,
                        tensorcore_utilization: None,
                        temperature: 0,
                        used_memory: 0,
                        total_memory,
                        frequency: 0,
                        power_consumption: 0.0,
                        gpu_core_count: None,
                        // Intel-on-Windows surfaces nothing beyond the
                        // basic WMI query — NVML thermal thresholds /
                        // P-states and NVIDIA hardware details (NUMA,
                        // GSP firmware, NvLink, GPM) do not apply.
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
                    });
                }
            }

            gpu_list
        })
        .unwrap_or_default()
    }
}

impl GpuReader for IntelWindowsGpuReader {
    fn get_gpu_info(&self) -> Vec<GpuInfo> {
        let mut gpus = self.query_intel_gpus();
        // The vendor-neutral DXGI / PDH layer runs first so the Level
        // Zero augmentation below overwrites it wherever both produce a
        // reading. L0 talks to the driver directly and is the more
        // authoritative source for utilization and power; DXGI remains
        // the only source of a correct 64-bit VRAM size either way.
        let adapter_index = crate::device::readers::windows_gpu_perf::augment_gpus(&mut gpus);
        if let Ok(mut guard) = self.adapter_index.lock() {
            *guard = adapter_index;
        }
        #[cfg(all_smi_level_zero)]
        self.augment_with_level_zero(&mut gpus);
        for gpu in &mut gpus {
            annotate_missing_metrics(gpu);
        }
        gpus
    }

    fn get_process_info(&self) -> Vec<ProcessInfo> {
        // Per-process dedicated GPU memory comes from the PDH `GPU
        // Process Memory` counter, reusing the sample `get_gpu_info`
        // already took. Mirrors the AMD-on-Windows reader.
        use crate::device::readers::windows_gpu_perf;
        let Ok(adapter_index) = self.adapter_index.lock() else {
            return Vec::new();
        };
        windows_gpu_perf::process_rows_with(&adapter_index, || {
            let mut gpus = self.query_intel_gpus();
            windows_gpu_perf::augment_gpus(&mut gpus)
        })
    }
}

#[cfg(all_smi_level_zero)]
impl IntelWindowsGpuReader {
    /// Layer Level Zero metrics on top of the WMI baseline. Each
    /// Intel WMI controller is paired with an L0 device by ordinal
    /// position in the sorted-BDF enumeration — `Win32_VideoController.PNPDeviceID`
    /// does not expose the PCI bus / device / function in a stable,
    /// parseable form across driver versions, so ordinal matching is
    /// the most reliable strategy we can guarantee for v1. On
    /// single-Intel-GPU hosts (the overwhelming majority of Windows
    /// installations) this is a perfect 1:1 match.
    ///
    /// Multi-Intel-GPU Windows hosts are rare; when the WMI list and
    /// the L0 list are mismatched in length we still pair the prefix
    /// to keep the common cases working — the unpaired suffix simply
    /// gets the WMI-only baseline. A follow-up issue can introduce
    /// BDF parsing from `Win32_PnPEntity.LocationInformation` for
    /// stronger matching when needed.
    fn augment_with_level_zero(&self, gpus: &mut [GpuInfo]) {
        use crate::device::readers::intel_gpu_level_zero as l0;
        let bdfs = l0::enumerated_pci_bdfs();
        if bdfs.is_empty() {
            return;
        }
        let mut states = match self.level_zero_state.lock() {
            Ok(s) => s,
            Err(_) => return,
        };
        for (gpu, bdf) in gpus.iter_mut().zip(bdfs.iter()) {
            // Key state by GPU UUID (which the WMI path derived from
            // PNPDeviceID) so the per-card energy-counter baseline
            // survives across `get_gpu_info` calls.
            let state = states
                .entry(gpu.uuid.clone())
                .or_insert_with(l0::LevelZeroState::empty);
            if let Some(readout) = l0::refresh(state, bdf) {
                l0::apply_to_gpu_info(gpu, &readout, l0::ApplyPlatform::Windows);
            }
        }
    }
}

/// Name the metrics that are still missing once every layer has run.
///
/// The reader used to publish a blanket "Detailed metrics require Level
/// Zero / xpu-smi" on every poll, which is now wrong in both directions.
/// Level Zero is compiled into every Windows build, so it cannot be the
/// thing to go install; and on a host where the driver is present the note
/// fired anyway, next to the very fields it claimed were unavailable.
///
/// An integrated part legitimately exposes no Sysman thermal sensor, so
/// "nothing is missing" and "temperature is missing" are both normal
/// outcomes. Saying which one this machine is in is the useful part.
fn annotate_missing_metrics(gpu: &mut GpuInfo) {
    use crate::device::readers::detail_keys::missing_metric_sources;
    // Utilization is included because PDH can be absent (Server Core
    // without the counter set, a locked-down host) and then no layer
    // supplies it either.
    const REPORTED: &[&str] = &["Temperature", "Power", "Frequency", "Utilization"];
    let missing = missing_metric_sources(&gpu.detail, REPORTED);
    if missing.is_empty() {
        // Removing rather than leaving a stale note: `detail` is rebuilt
        // per poll today, but a reader that starts caching it would
        // otherwise keep publishing a note the machine has outgrown.
        gpu.detail.remove("Note");
        return;
    }
    // The build is never the answer on Windows, so the note points at the
    // two things an operator can actually check.
    gpu.detail.insert(
        "Note".to_string(),
        format!(
            "{} unavailable: install the Intel graphics driver (ze_loader.dll), \
             or this GPU exposes no such sensor",
            missing.join(", ")
        ),
    );
}

/// Detect Intel client GPU presence on Windows via WMI.
///
/// Filter logic is intentionally conservative — we keep only controllers
/// that contain `intel` **and** a graphics family token (`arc`, `iris`,
/// `xe graphics`, or any `uhd`/`hd graphics` form). That way controllers
/// like "Intel Display Audio" or "Intel(R) Management Engine Interface"
/// are excluded even though they share the "Intel" name.
pub fn has_intel_gpu_windows() -> bool {
    let wmi_con = match WMIConnection::new() {
        Ok(w) => w,
        Err(e) => {
            eprintln!("Intel GPU detection: Failed to create WMI connection: {e}");
            return false;
        }
    };

    let query_result: Result<Vec<VideoControllerName>, _> =
        wmi_con.raw_query("SELECT Name FROM Win32_VideoController");

    match query_result {
        Ok(controllers) => {
            for controller in controllers {
                if let Some(name) = &controller.name
                    && is_intel_gpu_name(name)
                {
                    return true;
                }
            }
        }
        Err(e) => {
            eprintln!("Intel GPU detection: WMI query failed: {e}");
            return false;
        }
    }

    false
}

/// Free function — factored out of the reader so unit tests can exercise
/// the filter logic without touching WMI.
///
/// Returns `true` when the controller name plausibly identifies an
/// Intel client GPU. Requires both:
///
/// 1. The name contains "intel" (case-insensitive).
/// 2. The name contains at least one of the graphics-family tokens
///    listed in `FAMILY_TOKENS` — covering both legacy (`hd graphics`,
///    `uhd graphics`, `iris`) and modern (`arc`, `xe graphics`,
///    `xe-lpg`, `battlemage`, `lunarlake`, `lunar lake`) marketing
///    names.
///
/// Step 2 deliberately excludes names like "Intel Display Audio",
/// "Intel(R) Management Engine Interface", and "Intel Smart Sound" —
/// those share the "Intel" name but are not GPUs.
pub fn is_intel_gpu_name(name: &str) -> bool {
    let lower = name.to_lowercase();
    if !lower.contains("intel") {
        return false;
    }
    // Common Intel GPU family tokens. Order doesn't matter — we just
    // need ANY match. The list mirrors the architecture matchers in
    // `intel_gpu_names::classify_intel_architecture` so any name the
    // classifier would label as a real Intel GPU also passes this
    // filter. New family names (e.g. future "Celestial" / "Druid" Arc
    // generations) need to be added here AND to the classifier.
    const FAMILY_TOKENS: &[&str] = &[
        "arc",
        "iris",
        "uhd graphics",
        "hd graphics",
        "xe graphics",
        "intel graphics",
        "xe-lpg",
        "battlemage",
        "lunarlake",
        "lunar lake",
        // Forward-looking. Panther Lake already passes on "arc" (it ships
        // as "Intel(R) Arc(TM) B390 GPU"), but a future part sold as plain
        // "Intel(R) Xe3 Graphics" would not. A bare "gpu" token is
        // deliberately absent: it would admit the non-graphics Intel
        // devices this filter exists to exclude.
        "xe2",
        "xe3",
        "panther lake",
        "pantherlake",
    ];
    FAMILY_TOKENS.iter().any(|t| lower.contains(t))
}

#[cfg(test)]
#[path = "intel_gpu_windows/tests.rs"]
mod tests;
