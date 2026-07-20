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
//! Surfaces device identity, memory, frequency, temperature, power,
//! and utilization. Engine-busy is delta-computed from
//! sysfs counters by [`super::intel_gpu_engine`]: `max(render,
//! compute)` becomes `GpuInfo.utilization`, the per-class breakdown
//! lands in `detail["Engine: <class>"]`. The first refresh per card
//! is a seeding call returning `0.0`; real values appear from the
//! second refresh. When the kernel exposes no engine counters,
//! `detail["Utilization"]` carries the note in
//! `intel_gpu_engine::ENGINE_UNAVAILABLE_NOTE`. Xe cards then fall back to
//! GT active residency derived from `gtidle/idle_residency_ms`; the PMU
//! fallback remains deferred. Intel client GPUs have no MIG/vGPU equivalent.
//!
//! ## Memory semantics
//!
//! Discrete GPUs report dedicated VRAM via `device/mem_info_vram_total`
//! (i915) or `device/tile0/vram0/total_bytes` (xe). Integrated GPUs have no
//! dedicated VRAM, so the reader records `total_memory = 0` and explains that
//! memory is shared system memory.

use crate::device::GpuReader;
use crate::device::readers::common_cache::{DeviceStaticInfo, MAX_DEVICES};
use crate::device::readers::intel_gpu_engine::{
    EngineState, apply_engine_readout, refresh_with_lock,
};
use crate::device::readers::intel_gpu_fdinfo::{
    build_intel_drm_basenames, build_intel_process_infos, sum_card_vram_from_fdinfo,
};
use crate::device::readers::intel_gpu_gtidle::{
    GtidleState, apply_fallback as apply_gtidle_fallback,
};
use crate::device::readers::intel_gpu_names::{
    classify_intel_architecture, resolve_intel_gpu_name,
};
use crate::device::readers::intel_gpu_sysfs::{
    MemoryVariant, has_nonzero_u64, read_fan_rpm, read_frequency_mhz, read_memory_bytes,
    read_power_watts, read_resource2_total_bytes, read_temperature_celsius,
};
use crate::device::types::{GpuInfo, ProcessInfo};
use crate::utils::get_hostname;
use chrono::Local;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

// GPU metric validation constants — Intel client GPUs are smaller than
// datacenter accelerators, so we use tighter caps than the AMD reader.
// These prevent obviously bogus driver values (e.g. an integer parse
// glitch returning u32::MAX) from poisoning consumers.
const MAX_GPU_POWER_WATTS: f64 = 750.0; // largest Arc Pro variants stay <250W
const MAX_GPU_TEMP_CELSIUS: u32 = 125; // package max across i915/xe
const MAX_GPU_FREQ_MHZ: u32 = 5000;
const MAX_GPU_MEMORY_BYTES: u64 = 96 * 1024 * 1024 * 1024; // 96GB headroom
const MAX_GPU_UTILIZATION: f64 = 100.0; // Engine-busy is clamped to 100% per engine

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
    /// Engine-busy delta tracker. Mirrors the AMD reader's
    /// `vram_usage: Mutex<VramUsage>` pattern (including poisoning
    /// recovery via `refresh_with_lock`).
    engine_state: Mutex<EngineState>,
    /// Per-card Xe GT idle-residency delta tracker.
    gtidle_state: Mutex<GtidleState>,
    /// Per-card Level Zero handle state (issue #248). Mirrors
    /// `engine_state` — delta-tracked behind a `Mutex` for power
    /// readings, only present when `--features level_zero` is active.
    #[cfg(feature = "level_zero")]
    level_zero_state: Mutex<crate::device::readers::intel_gpu_level_zero::LevelZeroState>,
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
    /// Map of `/dev/dri/<basename>` to the index of the owning Intel
    /// card. Cached at construction time because the DRM minor layout
    /// is static across the lifetime of the kernel. See
    /// [`build_intel_drm_basenames`] for derivation details.
    intel_drm_basenames: HashMap<String, usize>,
    /// Root used for the per-process fdinfo walk. Production is
    /// `/proc`; tests inject a synthetic procfs tree under tempdir.
    proc_root: PathBuf,
}

impl Default for IntelGpuReader {
    fn default() -> Self {
        Self::new()
    }
}

impl IntelGpuReader {
    pub fn new() -> Self {
        Self::new_with_roots(Path::new("/sys/class/drm"), Path::new("/proc"))
    }

    /// Constructor used by tests: walk an arbitrary `cardN` root rather
    /// than the real `/sys/class/drm`. Production code uses
    /// [`IntelGpuReader::new`].
    #[cfg(test)]
    fn new_from_root(drm_root: &Path) -> Self {
        Self::new_with_roots(drm_root, Path::new("/proc"))
    }

    /// Internal constructor accepting arbitrary DRM and proc roots; production code uses default paths via [`IntelGpuReader::new`].
    fn new_with_roots(drm_root: &Path, proc_root: &Path) -> Self {
        let cards = discover_cards(drm_root);
        let card_refs: Vec<(PathBuf, usize)> = cards
            .iter()
            .enumerate()
            .map(|(i, c)| (c.card_path.clone(), i))
            .collect();
        let intel_drm_basenames = build_intel_drm_basenames(&card_refs, drm_root);
        Self {
            cards,
            intel_drm_basenames,
            proc_root: proc_root.to_path_buf(),
        }
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
            // The `"Utilization"` detail entry is populated dynamically
            // by `get_gpu_info` via the engine-busy refresh path.
            if card.variant == MemoryVariant::Integrated {
                detail.insert(
                    "Memory".to_string(),
                    "Shared system memory (no dedicated VRAM)".to_string(),
                );
            }
            // Baseline `Metrics Source`; L0 augmentation may upgrade it.
            detail.insert(
                "Metrics Source".to_string(),
                "sysfs (engine counters)".to_string(),
            );

            DeviceStaticInfo::with_details(name, None, detail)
        })
    }
}

impl GpuReader for IntelGpuReader {
    fn get_gpu_info(&self) -> Vec<GpuInfo> {
        let hostname = get_hostname();
        let time = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
        let mut out = Vec::with_capacity(self.cards.len());

        for (card_index, card) in self.cards.iter().enumerate() {
            let static_info = self.ensure_static_info(card);
            let mut detail = static_info.detail.clone();

            let device_dir = card.card_path.join("device");
            let (mut used_memory, total_memory) = read_memory_bytes(&device_dir, card.variant);
            let frequency = read_frequency_mhz(&device_dir);
            let temperature = read_temperature_celsius(&device_dir);
            let power_consumption = read_power_watts(&device_dir);
            let fan_rpm = read_fan_rpm(&device_dir);

            // Round-trip values through the validation caps so that a
            // garbled sysfs file can never propagate u32::MAX into the
            // exporter. See the AMD reader for the same defence-in-depth
            // pattern.
            let temperature = temperature.min(MAX_GPU_TEMP_CELSIUS);
            let frequency = frequency.min(MAX_GPU_FREQ_MHZ);
            let power_consumption = power_consumption.clamp(0.0, MAX_GPU_POWER_WATTS);
            let total_memory = total_memory.min(MAX_GPU_MEMORY_BYTES);
            used_memory = used_memory.min(total_memory);

            sources::decorate_static_sources(
                &mut detail,
                total_memory,
                temperature,
                power_consumption,
                frequency,
                fan_rpm,
            );

            // xe driver: fall back to fdinfo for VRAM usage when
            // card-level sysfs counters are unavailable.
            if used_memory == 0 && total_memory > 0 && card.driver == "xe" {
                let fdinfo_vram = sum_card_vram_from_fdinfo(
                    card_index,
                    &self.intel_drm_basenames,
                    &self.proc_root,
                );
                if fdinfo_vram > 0 {
                    used_memory = fdinfo_vram.min(total_memory);
                    detail.insert("Source: Memory".to_string(), "fdinfo (xe)".to_string());
                }
            }

            // Engine-busy refresh — guarded by a per-card mutex.
            let readout = refresh_with_lock(&card.engine_state, &device_dir);
            let mut utilization = readout.primary_utilization.clamp(0.0, MAX_GPU_UTILIZATION);
            apply_engine_readout(&mut detail, &readout);
            sources::decorate_utilization_source(&mut detail, &readout);
            apply_gtidle_fallback(
                &card.driver,
                &readout,
                &card.gtidle_state,
                &device_dir,
                &mut detail,
                &mut utilization,
            );

            let uuid = build_uuid(card, &device_dir);

            out.push(GpuInfo {
                uuid,
                time: time.clone(),
                name: static_info.name.clone(),
                device_type: "GPU".to_string(),
                host_id: hostname.clone(),
                hostname: hostname.clone(),
                instance: hostname.clone(),
                utilization,
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

            // Level Zero augmentation runs *after* the baseline
            // `GpuInfo` is pushed. On hosts without the L0 runtime, or
            // for cards L0 cannot bind to, this is a noop and the
            // sysfs baseline remains unchanged.
            #[cfg(feature = "level_zero")]
            level_zero_glue::augment(card, &mut out, &device_dir);
        }

        out
    }

    fn get_process_info(&self) -> Vec<ProcessInfo> {
        // Build the {card_index -> uuid} map fresh each call so the
        // UUID seen by per-process consumers matches whatever
        // `get_gpu_info()` would emit at the same instant. The PCI
        // bus is stable across the kernel's lifetime, but recomputing
        // is microsecond-cheap and keeps the contract explicit.
        let mut card_uuids = HashMap::with_capacity(self.cards.len());
        for (idx, card) in self.cards.iter().enumerate() {
            card_uuids.insert(idx, build_uuid(card, &card.card_path.join("device")));
        }
        build_intel_process_infos(&self.intel_drm_basenames, &card_uuids, &self.proc_root)
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
            engine_state: Mutex::new(EngineState::empty()),
            gtidle_state: Mutex::new(GtidleState::empty()),
            #[cfg(feature = "level_zero")]
            level_zero_state: Mutex::new(
                crate::device::readers::intel_gpu_level_zero::LevelZeroState::empty(),
            ),
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
        || read_resource2_total_bytes(device_dir) > 0
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

#[path = "intel_gpu_linux/detection.rs"]
mod detection;

/// Check whether at least one Intel client GPU is present on this Linux
/// host. Walks `/sys/class/drm/card*` first (cheap), then falls back to
/// `lspci -n` so containers without `/sys` access still work.
///
/// Distinguishes Intel **GPUs** from Habana / Gaudi (vendor `0x1da3`,
/// not Intel) and from Intel network/storage devices by requiring the
/// PCI driver to be `i915` or `xe`. Defends against false positives on
/// hosts that have an Intel-vendor PCI device which is not a GPU at all.
pub fn has_intel_client_gpu() -> bool {
    detection::has_intel_client_gpu_from_root(Path::new("/sys/class/drm"))
}

#[cfg(feature = "level_zero")]
#[path = "intel_gpu_linux/level_zero_glue.rs"]
mod level_zero_glue;

#[path = "intel_gpu_linux/sources.rs"]
mod sources;

#[cfg(test)]
#[path = "intel_gpu_linux/tests.rs"]
mod tests;
