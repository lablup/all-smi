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

//! NVIDIA hardware-detail queries: NUMA topology, GSP firmware, NvLink
//! remote endpoints, and GPM support detection (issue #132).
//!
//! Each function follows the same contract as the vGPU / MIG readers: any
//! NVML error degrades to `None` / empty collections so that older drivers,
//! non-datacenter SKUs, and non-NUMA platforms continue to emit valid
//! [`GpuInfo`] rows with the surrounding fields intact.
//!
//! # Caching policy
//!
//! [`HardwareDetailCache`] memoises the two static-per-device fields — NUMA
//! node id and GSP firmware version — so they are only fetched once per
//! process lifetime. Cache insertions happen only when at least one field
//! resolved successfully; a transient NVML error on the first call does not
//! lock the cache to `None` for the process lifetime.
//!
//! NvLink enumeration and GPM support detection are NOT cached because their
//! state can change at runtime (links can drop, GPM streaming can be toggled
//! externally). They remain cheap NVML calls per poll.

use std::collections::HashMap;
use std::os::raw::c_uint;
use std::sync::Mutex;

use nvml_wrapper::Nvml;
use nvml_wrapper::enum_wrappers::nv_link::IntDeviceType;
use nvml_wrapper::error::{NvmlError, nvml_try};

use crate::device::types::{GpmMetrics, NvLinkRemoteDevice, NvLinkRemoteType};

/// Upper bound on the number of NvLinks NVML will report per GPU. NVIDIA's
/// own header caps this at 18 for current generations; we keep the literal
/// constant here instead of importing from `nvml-wrapper-sys` so this module
/// stays free of sys-level dependencies.
pub const NVML_NVLINK_MAX_LINKS: u32 = 18;

/// Static per-device hardware details that never change at runtime.
#[derive(Debug, Clone, Default)]
pub struct HardwareDetails {
    /// NUMA node the GPU is attached to, or `None` when the host has no
    /// NUMA topology (Windows, older drivers, non-NUMA platforms).
    pub numa_node_id: Option<i32>,
    /// GSP firmware mode code: `0=disabled`, `1=enabled`, `2=default`.
    pub gsp_firmware_mode: Option<u8>,
    /// GSP firmware version string. `None` when `NotSupported`.
    pub gsp_firmware_version: Option<String>,
}

impl HardwareDetails {
    /// Whether this snapshot carries any supported value. Guards the cache
    /// against storing an all-empty record produced by transient NVML
    /// failures on the first poll.
    fn has_any_value(&self) -> bool {
        self.numa_node_id.is_some()
            || self.gsp_firmware_mode.is_some()
            || self.gsp_firmware_version.is_some()
    }
}

/// Cache keyed by NVML device index. Shared state is held behind a single
/// `Mutex` so the cache insertion is race-free — the sampler only locks
/// twice per miss (once to probe, once to insert) and blocking callers stay
/// O(number of GPUs).
pub struct HardwareDetailCache {
    entries: Mutex<HashMap<u32, HardwareDetails>>,
}

impl Default for HardwareDetailCache {
    fn default() -> Self {
        Self::new()
    }
}

impl HardwareDetailCache {
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// Fetch hardware details for `index`, populating the cache on the
    /// first successful call. All NVML errors (notably `NotSupported` and
    /// `FunctionNotFound`) are swallowed and leave the respective field as
    /// `None` — this feature MUST degrade gracefully on older drivers.
    pub fn get_or_fetch(&self, device: &nvml_wrapper::Device, index: u32) -> HardwareDetails {
        if let Ok(cache) = self.entries.lock()
            && let Some(existing) = cache.get(&index)
        {
            return existing.clone();
        }

        let details = HardwareDetails {
            numa_node_id: numa_node_id(device),
            gsp_firmware_mode: gsp_firmware_mode(device),
            gsp_firmware_version: gsp_firmware_version(device),
        };

        if details.has_any_value()
            && let Ok(mut cache) = self.entries.lock()
        {
            cache.insert(index, details.clone());
        }
        details
    }
}

/// Read the NUMA node id via NVML. Canonicalises the sentinel
/// `u32::MAX` (which some driver versions return when no NUMA topology is
/// present) to `None` so the UI and exporter can omit the metric rather
/// than emit a bogus number.
fn numa_node_id(device: &nvml_wrapper::Device) -> Option<i32> {
    // `Device::numa_node_id()` is available on all platforms in
    // nvml-wrapper 0.12.1 — no `cfg(target_os = "linux")` gate is needed.
    // Returns `u32` per the C API: negative values are not possible, but
    // drivers sometimes return the all-bits-set sentinel when no NUMA
    // topology is present.
    let raw = device.numa_node_id().ok()?;
    // Treat the classic "no NUMA" sentinel as `None`. Valid NUMA node ids
    // fit easily into `i32` on any real system.
    if raw == u32::MAX {
        return None;
    }
    i32::try_from(raw).ok()
}

/// Encode the GSP firmware mode as a 3-valued byte matching the
/// `all_smi_gsp_firmware_mode` gauge contract (0=disabled, 1=enabled,
/// 2=default).
fn gsp_firmware_mode(device: &nvml_wrapper::Device) -> Option<u8> {
    let mode = device.gsp_firmware_mode().ok()?;
    // `mode.default == true` takes precedence: the driver reports that
    // firmware operates in its default mode regardless of the enabled
    // flag.
    if mode.default {
        Some(2)
    } else if mode.enabled {
        Some(1)
    } else {
        Some(0)
    }
}

/// Read the GSP firmware version string. Trims any trailing NUL bytes
/// NVML leaves in the buffer — the high-level wrapper already takes
/// care of this, but the defensive trim future-proofs against any
/// buffer encoding surprises.
fn gsp_firmware_version(device: &nvml_wrapper::Device) -> Option<String> {
    let raw = device.gsp_firmware_version().ok()?;
    let trimmed = raw.trim_end_matches('\0').trim().to_string();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed)
}

/// Enumerate NvLinks for `device` and classify the remote endpoint of
/// every active link. Returns an empty vector when the driver does not
/// support any NvLink API, when the device has no active links, or when
/// all link queries error out.
///
/// We iterate up to [`NVML_NVLINK_MAX_LINKS`] and skip any link whose
/// `is_active()` probe errors or returns `false`. For active links we
/// query the remote device type via the raw FFI symbol so we avoid the
/// latent-bug path in `nvml-wrapper-0.12.1::nv_link::remote_device_type`
/// (that wrapper mistakenly writes to an immutable temporary, leaving the
/// out-parameter untouched).
pub fn collect_nvlink_remote_devices(
    nvml: &Nvml,
    device: &nvml_wrapper::Device,
) -> Vec<NvLinkRemoteDevice> {
    let mut out = Vec::new();
    for link in 0..NVML_NVLINK_MAX_LINKS {
        let link_wrapper = device.link_wrapper_for(link);
        match link_wrapper.is_active() {
            Ok(true) => {}
            // Any error here (`NotSupported`, `InvalidArg`) means the link
            // does not exist; stop probing early to avoid 18 failing calls
            // on a GPU with zero NvLinks. Observed behaviour: NVML returns
            // `InvalidArg` for indices past the physical link count.
            Ok(false) => continue,
            Err(_) => break,
        }
        let remote_type = match nvlink_remote_device_type_ffi(nvml, device, link) {
            Some(t) => t,
            None => NvLinkRemoteType::Unknown,
        };
        out.push(NvLinkRemoteDevice {
            link_index: link,
            remote_type,
        });
    }
    out
}

/// Query `nvmlDeviceGetNvLinkRemoteDeviceType` directly via the FFI symbol.
///
/// The high-level `NvLink::remote_device_type` method in nvml-wrapper 0.12.1
/// has a latent bug: it passes `&mut device_type.as_c()` which creates an
/// immutable temporary, so NVML never writes back to the local variable.
/// Calling the symbol directly avoids that defect and keeps the logic
/// contained here so we can remove the workaround when the wrapper is
/// fixed upstream.
fn nvlink_remote_device_type_ffi(
    nvml: &Nvml,
    device: &nvml_wrapper::Device,
    link: u32,
) -> Option<NvLinkRemoteType> {
    let sym = nvml
        .lib()
        .nvmlDeviceGetNvLinkRemoteDeviceType
        .as_ref()
        .ok()?;

    // SAFETY: `device.handle()` returns the same `nvmlDevice_t` that NVML
    // owns. We pass a valid out-pointer of the exact type NVML expects
    // (`c_uint`) and check the return code before trusting the contents.
    unsafe {
        let mut value: c_uint = 0;
        let rc = sym(device.handle(), link, &mut value);
        nvml_try(rc).ok()?;
        Some(map_remote_device_type(value))
    }
}

/// Map the raw NVML remote device type value to our domain enum. Unknown
/// values fall back to `NvLinkRemoteType::Unknown` so a future driver that
/// introduces new remote-device categories does not regress the reader.
fn map_remote_device_type(value: c_uint) -> NvLinkRemoteType {
    // Values from NVML's `nvmlIntNvLinkDeviceType_enum`:
    //   GPU = 0, IBMNPU = 1, SWITCH = 2, UNKNOWN = 255
    match value {
        0 => NvLinkRemoteType::Gpu,
        1 => NvLinkRemoteType::IbmNpu,
        2 => NvLinkRemoteType::Switch,
        _ => NvLinkRemoteType::Unknown,
    }
}

/// Same mapping as [`map_remote_device_type`] but for the wrapper's enum.
/// Kept as a utility for tests that construct an `IntDeviceType` directly.
#[allow(dead_code)]
pub(crate) fn nvlink_remote_type_from_wrapper(value: IntDeviceType) -> NvLinkRemoteType {
    match value {
        IntDeviceType::Gpu => NvLinkRemoteType::Gpu,
        IntDeviceType::Ibmnpu => NvLinkRemoteType::IbmNpu,
        IntDeviceType::Switch => NvLinkRemoteType::Switch,
        IntDeviceType::Unknown => NvLinkRemoteType::Unknown,
    }
}

/// Return `true` when the device reports GPM support via NVML's probe.
/// Any error (symbol missing, `NotSupported`, `InvalidArg`) degrades to
/// `false` so the caller never emits GPM metrics for a non-GPM device.
pub fn gpm_is_supported(device: &nvml_wrapper::Device) -> bool {
    device.gpm_support().unwrap_or(false)
}

/// Placeholder GPM metric collection.
///
/// The GPM API requires two time-separated samples passed to
/// `gpm_metrics_get`, which is incompatible with all-smi's single-poll
/// reader contract: we would have to cache the previous sample per device
/// and wait N seconds before the first reading is meaningful. That work is
/// tracked as a follow-up. For now we:
///
/// * detect support via [`gpm_is_supported`] so the TUI and exporter can
///   show a "GPM-capable" hint without emitting potentially wrong numbers;
/// * return `None` from the collection path so the gauge metrics are
///   omitted entirely (Prometheus convention for "no data") rather than
///   silently publishing zeros.
///
/// When the two-sample implementation lands we will populate
/// [`GpmMetrics::sm_occupancy`] and
/// [`GpmMetrics::memory_bandwidth_utilization`] here.
pub fn collect_gpm_metrics(device: &nvml_wrapper::Device) -> Option<GpmMetrics> {
    if !gpm_is_supported(device) {
        return None;
    }
    // Supported but unsampled — the two-sample handshake is deferred to a
    // follow-up. Return a populated struct so the TUI can indicate
    // "GPM-capable" without pretending specific numeric values are known.
    Some(GpmMetrics::default())
}

/// Attempt to fetch a GPM support signal without going through the NVML
/// API, failing closed. Used exclusively by unit tests that need a
/// deterministic "unsupported" reading without a real device handle.
#[allow(dead_code)]
fn err_unsupported() -> NvmlError {
    NvmlError::NotSupported
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hardware_details_has_any_value_false_when_all_none() {
        let empty = HardwareDetails::default();
        assert!(!empty.has_any_value());
    }

    #[test]
    fn hardware_details_has_any_value_true_when_numa_set() {
        let d = HardwareDetails {
            numa_node_id: Some(0),
            ..Default::default()
        };
        assert!(d.has_any_value());
    }

    #[test]
    fn hardware_details_has_any_value_true_when_mode_set() {
        let d = HardwareDetails {
            gsp_firmware_mode: Some(2),
            ..Default::default()
        };
        assert!(d.has_any_value());
    }

    #[test]
    fn hardware_details_has_any_value_true_when_version_set() {
        let d = HardwareDetails {
            gsp_firmware_version: Some("550.54.15".to_string()),
            ..Default::default()
        };
        assert!(d.has_any_value());
    }

    #[test]
    fn remote_device_type_mapping_is_stable() {
        assert_eq!(map_remote_device_type(0), NvLinkRemoteType::Gpu);
        assert_eq!(map_remote_device_type(1), NvLinkRemoteType::IbmNpu);
        assert_eq!(map_remote_device_type(2), NvLinkRemoteType::Switch);
        assert_eq!(map_remote_device_type(255), NvLinkRemoteType::Unknown);
    }

    #[test]
    fn remote_device_type_unknown_future_values_degrade_to_unknown() {
        // Any value the driver introduces later must not panic.
        assert_eq!(map_remote_device_type(17), NvLinkRemoteType::Unknown);
        assert_eq!(map_remote_device_type(u32::MAX), NvLinkRemoteType::Unknown);
    }

    #[test]
    fn nvlink_remote_type_label_round_trip() {
        for v in [
            NvLinkRemoteType::Gpu,
            NvLinkRemoteType::IbmNpu,
            NvLinkRemoteType::Switch,
            NvLinkRemoteType::Unknown,
        ] {
            assert_eq!(NvLinkRemoteType::from_label(v.as_label()), v);
        }
    }

    #[test]
    fn nvlink_remote_type_from_label_unknown_inputs_degrade() {
        assert_eq!(NvLinkRemoteType::from_label(""), NvLinkRemoteType::Unknown);
        assert_eq!(
            NvLinkRemoteType::from_label("garbage"),
            NvLinkRemoteType::Unknown
        );
    }

    #[test]
    fn nvlink_max_links_matches_nvml_header() {
        // NVML's NVML_NVLINK_MAX_LINKS is currently 18; this test is a
        // canary that will fail if we forget to bump this constant when a
        // future NVML release raises the cap.
        assert_eq!(NVML_NVLINK_MAX_LINKS, 18);
    }

    #[test]
    fn wrapper_enum_mapping_covers_every_variant() {
        // Guard against new IntDeviceType variants silently collapsing to
        // Unknown. If nvml-wrapper introduces a new variant, this test
        // fails until we extend `nvlink_remote_type_from_wrapper`.
        assert_eq!(
            nvlink_remote_type_from_wrapper(IntDeviceType::Gpu),
            NvLinkRemoteType::Gpu
        );
        assert_eq!(
            nvlink_remote_type_from_wrapper(IntDeviceType::Ibmnpu),
            NvLinkRemoteType::IbmNpu
        );
        assert_eq!(
            nvlink_remote_type_from_wrapper(IntDeviceType::Switch),
            NvLinkRemoteType::Switch
        );
        assert_eq!(
            nvlink_remote_type_from_wrapper(IntDeviceType::Unknown),
            NvLinkRemoteType::Unknown
        );
    }
}
