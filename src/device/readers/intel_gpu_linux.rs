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

//! Intel client GPU reader for Linux using sysfs
//!
//! Enumerates Intel **client** GPUs — both discrete Intel Arc (A-series /
//! B-series "Battlemage") and integrated graphics (Iris Xe, Xe-LPG, Arc iGPU
//! on Core Ultra / Meteor Lake) — by walking `/sys/class/drm/card*` for
//! devices whose vendor is `0x8086` and whose driver is `i915` or `xe`.
//!
//! ## Scope (v1)
//!
//! This implementation surfaces device identity (name, PCI device ID),
//! memory totals and used bytes where the driver exposes them, current
//! frequency, temperature, and instantaneous power. Engine-busy utilization
//! (the `engine/*/busy` perf counters) requires sampling deltas across
//! polling intervals and is **deferred** — `utilization` is reported as
//! `0.0` and the `detail` map carries a `"Utilization"` note pointing the
//! user at `intel_gpu_top` for live engine-busy figures. Intel client GPUs
//! have no MIG/vGPU equivalent so the `GpuReader` default `Vec::new()`
//! returns apply unchanged.
//!
//! ## Memory semantics
//!
//! Discrete GPUs report dedicated VRAM via `device/mem_info_vram_total`
//! (i915) or `device/tile0/vram0/total_bytes` (xe). Integrated GPUs have
//! no dedicated VRAM — the reader records `total_memory = 0` and writes a
//! `"Memory"` detail explaining the value is shared system memory. The
//! reader never fabricates a number from `MemTotal` because that
//! misrepresents the actual GPU memory budget (the kernel allocates GTT
//! pages on demand and the budget is a soft cap, not a fixed reservation).

use crate::device::GpuReader;
use crate::device::common::execute_command_default;
use crate::device::readers::common_cache::{DeviceStaticInfo, MAX_DEVICES};
use crate::device::readers::intel_gpu_names::{
    classify_intel_architecture, resolve_intel_gpu_name,
};
use crate::device::readers::intel_gpu_sysfs::{
    MemoryVariant, has_nonzero_u64, read_frequency_mhz, read_memory_bytes, read_power_watts,
    read_temperature_celsius,
};
use crate::device::types::{GpuInfo, ProcessInfo};
use crate::utils::get_hostname;
use chrono::Local;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

// GPU metric validation constants — Intel client GPUs are smaller than
// datacenter accelerators, so we use tighter caps than the AMD reader.
// These prevent obviously bogus driver values (e.g. an integer parse
// glitch returning u32::MAX) from poisoning consumers.
const MAX_GPU_POWER_WATTS: f64 = 750.0; // largest Arc Pro variants stay <250W
const MAX_GPU_TEMP_CELSIUS: u32 = 125; // package max across i915/xe
const MAX_GPU_FREQ_MHZ: u32 = 5000;
const MAX_GPU_MEMORY_BYTES: u64 = 96 * 1024 * 1024 * 1024; // 96GB headroom

// Per-card sysfs anchor.  We hold the absolute card path (e.g.
// `/sys/class/drm/card0`) plus a one-time-cached identity (the name and
// `detail` map) so subsequent refreshes only re-read the dynamic counters.
struct IntelGpuCard {
    /// Card index as exposed by the kernel (`0` for `card0`, …). Used for
    /// stable UUIDs when no PCI bus identifier is available.
    index: u32,
    /// Absolute path to `/sys/class/drm/cardN`.
    card_path: PathBuf,
    /// Driver name (`i915` or `xe`). Empty when the driver symlink could
    /// not be resolved — in that case the reader still emits the GPU but
    /// skips xe-only or i915-only paths.
    driver: String,
    /// PCI device identifier (numeric `device` value, e.g. `0xE20B`).
    device_id: u32,
    /// Classification populated at construction time.
    variant: MemoryVariant,
    /// Cached static info (name + base `detail` map). Filled on first
    /// `get_gpu_info` call so that `IntelGpuReader::new` stays cheap.
    static_info: OnceLock<DeviceStaticInfo>,
}

/// Render the discrete/integrated classification as the string we put in
/// `detail["Variant"]`.
fn variant_label(variant: MemoryVariant) -> &'static str {
    match variant {
        MemoryVariant::Discrete => "Discrete",
        MemoryVariant::Integrated => "Integrated",
    }
}

/// The reader itself. Holds a snapshot of cards discovered at
/// construction time. Hot-plug is not supported in v1 — matching the AMD
/// reader pattern, which also samples device list at `new()`.
pub struct IntelGpuReader {
    cards: Vec<IntelGpuCard>,
}

impl Default for IntelGpuReader {
    fn default() -> Self {
        Self::new()
    }
}

impl IntelGpuReader {
    pub fn new() -> Self {
        Self::new_from_root(Path::new("/sys/class/drm"))
    }

    /// Constructor used by tests: walk an arbitrary `cardN` root rather
    /// than the real `/sys/class/drm`. Production code uses
    /// [`IntelGpuReader::new`].
    fn new_from_root(drm_root: &Path) -> Self {
        let cards = discover_cards(drm_root);
        Self { cards }
    }

    /// Compute the per-card static identity once and cache it.
    fn ensure_static_info<'a>(&self, card: &'a IntelGpuCard) -> &'a DeviceStaticInfo {
        card.static_info.get_or_init(|| {
            let device_dir = card.card_path.join("device");
            let name = resolve_device_name(&device_dir, card.device_id);

            let mut detail = HashMap::new();
            detail.insert("Device ID".to_string(), format!("{:#06x}", card.device_id));
            detail.insert(
                "Variant".to_string(),
                variant_label(card.variant).to_string(),
            );
            if !card.driver.is_empty() {
                detail.insert("Driver".to_string(), card.driver.clone());
            }
            if let Some(bus) = read_pci_bus_id(&device_dir) {
                detail.insert("PCI Bus".to_string(), bus);
            }
            // Architecture / SYCL classification — derived from the
            // marketing name so downstream consumers (Backend.AI's
            // accelerator-selection layer, the llama.cpp SYCL backend
            // picker, etc.) can rely on all-smi as a single source of
            // truth instead of reimplementing the same name-pattern
            // table. The classifier is intentionally pure-string so it
            // stays platform-agnostic and shareable with the Windows
            // reader.
            let arch = classify_intel_architecture(&name);
            detail.insert("Architecture".to_string(), arch.label().to_string());
            detail.insert(
                "SYCL Capable".to_string(),
                arch.sycl_capable_label().to_string(),
            );
            // Document the v1 scope limitation in-band so library
            // consumers see *why* utilization is always reported as
            // zero rather than thinking the GPU is idle.
            detail.insert(
                "Utilization".to_string(),
                "Requires intel_gpu_top (perf engine counters)".to_string(),
            );
            if card.variant == MemoryVariant::Integrated {
                detail.insert(
                    "Memory".to_string(),
                    "Shared system memory (no dedicated VRAM)".to_string(),
                );
            }

            DeviceStaticInfo::with_details(name, None, detail)
        })
    }
}

impl GpuReader for IntelGpuReader {
    fn get_gpu_info(&self) -> Vec<GpuInfo> {
        let hostname = get_hostname();
        let time = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
        let mut out = Vec::with_capacity(self.cards.len());

        for card in &self.cards {
            let static_info = self.ensure_static_info(card);
            let mut detail = static_info.detail.clone();

            let device_dir = card.card_path.join("device");
            let (used_memory, total_memory) = read_memory_bytes(&device_dir, card.variant);
            let frequency = read_frequency_mhz(&device_dir);
            let temperature = read_temperature_celsius(&device_dir);
            let power_consumption = read_power_watts(&device_dir);

            // Round-trip values through the validation caps so that a
            // garbled sysfs file can never propagate u32::MAX into the
            // exporter. See the AMD reader for the same defence-in-depth
            // pattern.
            let temperature = temperature.min(MAX_GPU_TEMP_CELSIUS);
            let frequency = frequency.min(MAX_GPU_FREQ_MHZ);
            let power_consumption = power_consumption.clamp(0.0, MAX_GPU_POWER_WATTS);
            let total_memory = total_memory.min(MAX_GPU_MEMORY_BYTES);
            let used_memory = used_memory.min(total_memory);

            // Surface the raw memory budget as a detail entry too so a
            // remote operator inspecting the `detail` map can see what
            // the driver reported even when `total_memory` was clamped
            // (this is purely defensive — no current Intel client GPU
            // is anywhere near the 96GB cap).
            if total_memory > 0 {
                detail.insert("VRAM Total".to_string(), format!("{total_memory} bytes"));
            }

            let uuid = build_uuid(card, &device_dir);

            out.push(GpuInfo {
                uuid,
                time: time.clone(),
                name: static_info.name.clone(),
                device_type: "GPU".to_string(),
                host_id: hostname.clone(),
                hostname: hostname.clone(),
                instance: hostname.clone(),
                utilization: 0.0,
                ane_utilization: 0.0,
                dla_utilization: None,
                tensorcore_utilization: None,
                temperature,
                used_memory,
                total_memory,
                frequency,
                power_consumption,
                gpu_core_count: None,
                // Intel client GPUs do not expose NVML-style thermal
                // thresholds or P-states; leave those `None` so the UI
                // renders them as unavailable rather than as zero.
                temperature_threshold_slowdown: None,
                temperature_threshold_shutdown: None,
                temperature_threshold_max_operating: None,
                temperature_threshold_acoustic: None,
                performance_state: None,
                // NVIDIA-only hardware details.
                numa_node_id: None,
                gsp_firmware_mode: None,
                gsp_firmware_version: None,
                nvlink_remote_devices: Vec::new(),
                gpm_metrics: None,
                detail,
            });
        }

        out
    }

    fn get_process_info(&self) -> Vec<ProcessInfo> {
        // Per-process GPU memory accounting on Intel requires walking
        // `/proc/<pid>/fdinfo/*` for DRM clients and parsing the
        // driver-specific `drm-engine-*` / `drm-memory-local` keys. The
        // engine counter format differs between `i915` and `xe`, and
        // the implementation needs a delta-tracker to be useful — we
        // defer that to a follow-up just like utilization.
        Vec::new()
    }
}

// ---------- Discovery ----------

fn discover_cards(drm_root: &Path) -> Vec<IntelGpuCard> {
    let entries = match std::fs::read_dir(drm_root) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };

    let mut cards = Vec::new();
    for entry in entries.flatten() {
        if cards.len() >= MAX_DEVICES {
            break;
        }
        let path = entry.path();
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };

        // Match `cardN` exactly (not `card0-eDP-1` connector nodes).
        if !is_card_node(&name) {
            continue;
        }

        let device_dir = path.join("device");
        if !is_intel_vendor(&device_dir) {
            continue;
        }

        // Resolve the driver to confirm this is `i915` or `xe`. A bare
        // Intel vendor ID without a graphics driver attached (e.g. a
        // future Intel-vendor accelerator) MUST NOT be claimed by this
        // reader; that's what the Habana-vendor `0x1da3` separation in
        // `has_gaudi()` exists to prevent for the inverse case.
        let driver = resolve_driver(&device_dir);
        if driver != "i915" && driver != "xe" {
            continue;
        }

        let device_id = read_device_id(&device_dir).unwrap_or(0);
        let variant = classify_variant(&device_dir);

        let index = parse_card_index(&name);

        cards.push(IntelGpuCard {
            index,
            card_path: path,
            driver,
            device_id,
            variant,
            static_info: OnceLock::new(),
        });
    }

    // Stable ordering by card index so UUID assignment and the reader
    // output stay deterministic across runs on the same machine.
    cards.sort_by_key(|c| c.index);
    cards
}

fn is_card_node(name: &str) -> bool {
    if let Some(rest) = name.strip_prefix("card") {
        !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit())
    } else {
        false
    }
}

fn parse_card_index(name: &str) -> u32 {
    name.strip_prefix("card")
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0)
}

fn is_intel_vendor(device_dir: &Path) -> bool {
    match std::fs::read_to_string(device_dir.join("vendor")) {
        Ok(s) => s.trim().eq_ignore_ascii_case("0x8086"),
        Err(_) => false,
    }
}

fn resolve_driver(device_dir: &Path) -> String {
    // `/sys/class/drm/cardN/device/driver` is a symlink to
    // `/sys/bus/pci/drivers/<driver>`; the file name is the driver name.
    match std::fs::read_link(device_dir.join("driver")) {
        Ok(target) => target
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string(),
        Err(_) => String::new(),
    }
}

fn read_device_id(device_dir: &Path) -> Option<u32> {
    let s = std::fs::read_to_string(device_dir.join("device")).ok()?;
    parse_hex_u32(s.trim())
}

fn parse_hex_u32(s: &str) -> Option<u32> {
    let stripped = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .unwrap_or(s);
    u32::from_str_radix(stripped, 16).ok()
}

fn read_pci_bus_id(device_dir: &Path) -> Option<String> {
    // The PCI bus id is the last path segment of the `device` symlink
    // target, e.g. `…/0000:03:00.0`. Falls back to None when the link is
    // missing (synthetic fixtures don't bother to create it).
    let link = std::fs::read_link(device_dir).ok()?;
    link.file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_string())
}

fn build_uuid(card: &IntelGpuCard, device_dir: &Path) -> String {
    // Prefer the PCI bus id when present, fall back to card index. The
    // resulting UUID is stable across the lifetime of the kernel.
    if let Some(bus) = read_pci_bus_id(device_dir) {
        format!("Intel-GPU-{bus}")
    } else {
        format!("Intel-GPU-card{}", card.index)
    }
}

fn classify_variant(device_dir: &Path) -> MemoryVariant {
    if has_nonzero_u64(&device_dir.join("mem_info_vram_total"))
        || has_nonzero_u64(&device_dir.join("tile0").join("vram0").join("total_bytes"))
    {
        MemoryVariant::Discrete
    } else {
        MemoryVariant::Integrated
    }
}

// ---------- Static identity ----------

fn resolve_device_name(device_dir: &Path, device_id: u32) -> String {
    // `device/label` exists on a handful of integrated SKUs that carry a
    // pre-cooked marketing string; it's rare but free to check.
    if let Ok(label) = std::fs::read_to_string(device_dir.join("label")) {
        let trimmed = label.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    resolve_intel_gpu_name(device_id)
}

// ---------- Detection helper ----------

/// Check whether at least one Intel client GPU is present on this Linux
/// host. Walks `/sys/class/drm/card*` first (cheap), then falls back to
/// `lspci -n` so containers without `/sys` access still work.
///
/// Distinguishes Intel **GPUs** from Habana / Gaudi (vendor `0x1da3`,
/// not Intel) and from Intel network/storage devices by requiring the
/// PCI driver to be `i915` or `xe`. Defends against false positives on
/// hosts that have an Intel-vendor PCI device which is not a GPU at all.
#[cfg(target_os = "linux")]
pub fn has_intel_client_gpu() -> bool {
    has_intel_client_gpu_from_root(Path::new("/sys/class/drm"))
}

#[cfg(target_os = "linux")]
fn has_intel_client_gpu_from_root(drm_root: &Path) -> bool {
    if let Ok(entries) = std::fs::read_dir(drm_root) {
        for entry in entries.flatten() {
            let path = entry.path();
            let name = match path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n.to_string(),
                None => continue,
            };
            if !is_card_node(&name) {
                continue;
            }
            let device_dir = path.join("device");
            if !is_intel_vendor(&device_dir) {
                continue;
            }
            let driver = resolve_driver(&device_dir);
            if driver == "i915" || driver == "xe" {
                return true;
            }
        }
    }

    // Fallback: `lspci -n` parsing for hosts without `/sys` access (some
    // unprivileged containers). Class codes 0300/0301/0302/0380 cover
    // VGA / XGA / 3D / Display controllers respectively. We need to
    // confirm vendor `8086` AND a graphics class — a NIC at vendor 8086
    // would fail the class check.
    if let Ok(output) = execute_command_default("lspci", &["-n"])
        && output.status == 0
    {
        for line in output.stdout.lines() {
            if line_matches_intel_gpu(line) {
                return true;
            }
        }
    }
    false
}

fn line_matches_intel_gpu(line: &str) -> bool {
    // `lspci -n` lines look like:
    //   `03:00.0 0300: 8086:56a0 (rev 08)`
    // Pull out the class (`0300`) and the vendor (`8086`) tokens.
    let mut tokens = line.split_whitespace();
    let _bdf = tokens.next();
    let class = tokens.next().unwrap_or("").trim_end_matches(':');
    let vendor_device = tokens.next().unwrap_or("");

    let class_match = matches!(class, "0300" | "0301" | "0302" | "0380");
    if !class_match {
        return false;
    }
    vendor_device.split(':').next() == Some("8086")
}

#[cfg(test)]
#[path = "intel_gpu_linux/tests.rs"]
mod tests;
