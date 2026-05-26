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
//! ## Limitations (v1 scope)
//!
//! Detailed metrics (utilization, temperature, fine-grained power) are
//! **not** available through WMI on Windows for Intel client GPUs.
//! Surfacing them requires Level Zero (`ze_*` API via the
//! `libze_intel_gpu` shared library) or `xpu-smi` for datacenter Flex /
//! Max. That follow-up is documented in the issue and intentionally
//! deferred — this reader returns `0` for those fields and adds a
//! `detail["Note"]` entry pointing at the future work, so consumers know
//! the missing values aren't a regression.

use crate::device::GpuReader;
use crate::device::types::{GpuInfo, ProcessInfo};
use crate::utils::get_hostname;
use chrono::Local;
use serde::Deserialize;
use std::collections::HashMap;
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

pub struct IntelWindowsGpuReader {}

impl Default for IntelWindowsGpuReader {
    fn default() -> Self {
        Self::new()
    }
}

impl IntelWindowsGpuReader {
    pub fn new() -> Self {
        Self {}
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
                    detail.insert(
                        "Variant".to_string(),
                        classify_intel_variant(&name).to_string(),
                    );
                    detail.insert(
                        "Note".to_string(),
                        "Detailed metrics require Level Zero / xpu-smi".to_string(),
                    );

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
        self.query_intel_gpus()
    }

    fn get_process_info(&self) -> Vec<ProcessInfo> {
        // Per-process GPU memory on Windows requires PDH / D3DKMT or
        // Level Zero. Not available via Win32_VideoController. Mirrors
        // the AMD-on-Windows reader.
        Vec::new()
    }
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
/// 2. The name contains at least one of the graphics-family tokens —
///    `arc`, `iris`, `uhd graphics`, `hd graphics`, `xe graphics`, or
///    matches the iGPU pattern `intel graphics`.
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
    // need ANY match.
    const FAMILY_TOKENS: &[&str] = &[
        "arc",
        "iris",
        "uhd graphics",
        "hd graphics",
        "xe graphics",
        "intel graphics",
    ];
    FAMILY_TOKENS.iter().any(|t| lower.contains(t))
}

/// Heuristic discrete-vs-integrated discriminator for Intel client
/// GPUs on Windows. We can't introspect VRAM reliably via WMI (the 32-bit
/// `AdapterRAM` field is unreliable, see above) so we fall back to a
/// name-pattern check that the test suite locks in.
///
/// The discriminator looks for an Arc model number — discrete Arc cards
/// always carry one (e.g. `A770`, `A750`, `B580`, `B570`), while the
/// Meteor Lake / Core Ultra iGPU is sold as "Intel(R) Arc(TM) Graphics"
/// with no number. Iris / UHD / HD Graphics / Xe Graphics are always
/// integrated.
fn classify_intel_variant(name: &str) -> &'static str {
    let lower = name.to_lowercase();
    if !lower.contains("arc") {
        return "Integrated";
    }
    // Heuristic: discrete Arc names contain a token like "a770", "b580"
    // etc. — a letter A/B/C followed by 3+ digits. Scan word boundaries.
    let has_model_number = lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(is_arc_model_token);
    if has_model_number {
        "Discrete"
    } else {
        "Integrated"
    }
}

/// `true` for tokens like `a770`, `a750`, `b580`, `c770` — a single
/// letter (current Arc generations are A/B; reserve C/D for forward
/// compatibility) followed by 3+ digits.
fn is_arc_model_token(token: &str) -> bool {
    let bytes = token.as_bytes();
    if bytes.len() < 4 {
        return false;
    }
    let first = bytes[0] as char;
    if !matches!(first, 'a' | 'b' | 'c' | 'd') {
        return false;
    }
    bytes[1..].iter().all(|b| b.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intel_arc_a770_recognised() {
        assert!(is_intel_gpu_name("Intel(R) Arc(TM) A770 Graphics"));
    }

    #[test]
    fn intel_arc_b580_recognised() {
        assert!(is_intel_gpu_name("Intel(R) Arc(TM) B580 Graphics"));
    }

    #[test]
    fn intel_iris_xe_recognised() {
        assert!(is_intel_gpu_name("Intel(R) Iris(R) Xe Graphics"));
    }

    #[test]
    fn intel_uhd_770_recognised() {
        assert!(is_intel_gpu_name("Intel(R) UHD Graphics 770"));
    }

    #[test]
    fn intel_hd_graphics_recognised() {
        assert!(is_intel_gpu_name("Intel(R) HD Graphics 530"));
    }

    #[test]
    fn meteor_lake_arc_igpu_recognised() {
        // Meteor Lake / Core Ultra iGPU ships as "Intel(R) Arc(TM)
        // Graphics" with no number.
        assert!(is_intel_gpu_name("Intel(R) Arc(TM) Graphics"));
    }

    #[test]
    fn intel_display_audio_excluded() {
        // Audio device — must NOT match even though "Intel" is in the name.
        assert!(!is_intel_gpu_name("Intel(R) Display Audio"));
    }

    #[test]
    fn intel_management_engine_excluded() {
        assert!(!is_intel_gpu_name(
            "Intel(R) Management Engine Interface #1"
        ));
    }

    #[test]
    fn intel_smart_sound_excluded() {
        assert!(!is_intel_gpu_name(
            "Intel(R) Smart Sound Technology (Intel(R) SST)"
        ));
    }

    #[test]
    fn non_intel_excluded() {
        assert!(!is_intel_gpu_name("NVIDIA GeForce RTX 4090"));
        assert!(!is_intel_gpu_name("AMD Radeon RX 7900 XTX"));
    }

    #[test]
    fn classify_arc_discrete() {
        assert_eq!(
            classify_intel_variant("Intel(R) Arc(TM) A770 Graphics"),
            "Discrete"
        );
        assert_eq!(
            classify_intel_variant("Intel(R) Arc(TM) B580 Graphics"),
            "Discrete"
        );
    }

    #[test]
    fn classify_iris_integrated() {
        assert_eq!(
            classify_intel_variant("Intel(R) Iris(R) Xe Graphics"),
            "Integrated"
        );
    }

    #[test]
    fn classify_uhd_integrated() {
        assert_eq!(
            classify_intel_variant("Intel(R) UHD Graphics 770"),
            "Integrated"
        );
    }

    #[test]
    fn classify_meteor_lake_arc_igpu_as_integrated() {
        // "Intel Arc Graphics" without a model number on Core Ultra is
        // the iGPU and must NOT be classified as Discrete.
        assert_eq!(
            classify_intel_variant("Intel(R) Arc(TM) Graphics"),
            "Integrated"
        );
    }

    #[test]
    fn arc_model_token_recognises_known_skus() {
        assert!(is_arc_model_token("a770"));
        assert!(is_arc_model_token("a750"));
        assert!(is_arc_model_token("a580"));
        assert!(is_arc_model_token("a380"));
        assert!(is_arc_model_token("b580"));
        assert!(is_arc_model_token("b570"));
    }

    #[test]
    fn arc_model_token_rejects_non_models() {
        assert!(!is_arc_model_token("arc"));
        assert!(!is_arc_model_token("tm"));
        assert!(!is_arc_model_token("graphics"));
        assert!(!is_arc_model_token("a"));
        // Single letter followed by <3 digits doesn't count as a model.
        assert!(!is_arc_model_token("a77"));
    }
}
