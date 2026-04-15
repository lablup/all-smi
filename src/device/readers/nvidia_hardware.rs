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
//! [`HardwareDetailCache`] memoises the three static-per-device fields —
//! NUMA node id, GSP firmware mode, and GSP firmware version — in independent
//! per-field caches so each field is fetched only once per process lifetime.
//!
//! Cache insertion is conditional on the NVML result:
//! - `Ok(value)` → cached as `Some(value)`.
//! - `Err(NotSupported | FunctionNotFound)` → cached as `None` (permanently
//!   unavailable; will not be retried).
//! - Any other error (transient: `Unknown`, `GpuIsLost`, etc.) → NOT cached;
//!   the next poll will retry that field independently.
//!
//! This guarantees a transient failure on field A (e.g. GSP mode during a
//! driver hiccup) does not permanently lock field B (e.g. NUMA node) to
//! `None`, and vice versa.
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

/// Determines whether an NVML error should be treated as a permanent
/// "not available" condition (so `None` can be cached) or as a transient
/// failure that should be retried on the next poll.
///
/// `NotSupported` and `FunctionNotFound` are driver/hardware capabilities
/// that cannot change at runtime, so caching `None` is correct.  All other
/// errors (e.g. `Unknown`, `GpuIsLost`, `DriverNotLoaded`) are transient
/// and must NOT be cached so the next poll retries.
fn is_permanent_unavailable(err: &NvmlError) -> bool {
    matches!(err, NvmlError::NotSupported | NvmlError::FunctionNotFound)
}

/// Per-field cache entry.  `Some(value)` means the field was read
/// successfully.  `None` means the driver permanently does not support
/// this field (e.g. `NotSupported`).  The entry being absent from the map
/// means the field has never been fetched or only experienced transient
/// errors so far.
type FieldCache<T> = Mutex<HashMap<u32, Option<T>>>;

/// Cache keyed by NVML device index. Each hardware-detail field has its
/// own independent cache so a transient error on one field does not
/// permanently lock another field to `None`.
///
/// Cache insertion semantics per field:
/// - NVML returns `Ok(value)` → cache `Some(value)`.
/// - NVML returns a *permanent* error (`NotSupported`, `FunctionNotFound`)
///   → cache `None` (permanently unavailable).
/// - NVML returns a *transient* error (anything else) → do NOT insert;
///   the next poll will retry.
pub struct HardwareDetailCache {
    numa: FieldCache<i32>,
    gsp_mode: FieldCache<u8>,
    gsp_version: FieldCache<String>,
}

impl Default for HardwareDetailCache {
    fn default() -> Self {
        Self::new()
    }
}

impl HardwareDetailCache {
    pub fn new() -> Self {
        Self {
            numa: Mutex::new(HashMap::new()),
            gsp_mode: Mutex::new(HashMap::new()),
            gsp_version: Mutex::new(HashMap::new()),
        }
    }

    /// Fetch hardware details for `index`, consulting and updating the
    /// per-field caches independently.  A transient NVML error on one field
    /// leaves that field uncached so the next poll will retry it; a
    /// permanent error (`NotSupported`, `FunctionNotFound`) caches `None`
    /// so we stop querying it on every poll.
    pub fn get_or_fetch(&self, device: &nvml_wrapper::Device, index: u32) -> HardwareDetails {
        HardwareDetails {
            numa_node_id: self
                .get_or_fetch_field(&self.numa, index, || numa_node_id_result(device)),
            gsp_firmware_mode: self
                .get_or_fetch_field(&self.gsp_mode, index, || gsp_firmware_mode_result(device)),
            gsp_firmware_version: self.get_or_fetch_field(&self.gsp_version, index, || {
                gsp_firmware_version_result(device)
            }),
        }
    }

    /// Generic per-field cache lookup and conditional insertion.
    ///
    /// * If the map already contains an entry for `index` (even `None`),
    ///   return it immediately without calling `fetch`.
    /// * Otherwise call `fetch`:
    ///   - `Ok(v)`  → cache `Some(v)`, return `Some(v)`.
    ///   - `Err(e)` where `e` is a permanent-unavailable variant → cache
    ///     `None`, return `None`.
    ///   - `Err(e)` where `e` is transient → do NOT cache, return `None`
    ///     (the next poll will retry).
    fn get_or_fetch_field<T, F>(&self, cache: &FieldCache<T>, index: u32, fetch: F) -> Option<T>
    where
        T: Clone,
        F: FnOnce() -> Result<T, NvmlError>,
    {
        // Probe: return immediately if any entry (even `None`) is cached.
        if let Ok(map) = cache.lock()
            && let Some(cached) = map.get(&index)
        {
            return cached.clone();
        }

        // Miss: call the NVML function.
        match fetch() {
            Ok(value) => {
                if let Ok(mut map) = cache.lock() {
                    map.insert(index, Some(value.clone()));
                }
                Some(value)
            }
            Err(ref e) if is_permanent_unavailable(e) => {
                if let Ok(mut map) = cache.lock() {
                    map.insert(index, None);
                }
                None
            }
            Err(_transient) => {
                // Do NOT cache — retry next poll.
                None
            }
        }
    }
}

/// Read the NUMA node id via NVML, returning the raw `Result` so the
/// cache layer can distinguish transient from permanent errors.
///
/// Canonicalises the sentinel `u32::MAX` (which some driver versions return
/// when no NUMA topology is present) to `NvmlError::NotSupported` so it is
/// treated as permanently unavailable and cached as `None`.
fn numa_node_id_result(device: &nvml_wrapper::Device) -> Result<i32, NvmlError> {
    // `Device::numa_node_id()` is available on all platforms in
    // nvml-wrapper 0.12.1 — no `cfg(target_os = "linux")` gate is needed.
    // Returns `u32` per the C API: negative values are not possible, but
    // drivers sometimes return the all-bits-set sentinel when no NUMA
    // topology is present.
    let raw = device.numa_node_id()?;
    // Treat the classic "no NUMA" sentinel as permanently unsupported.
    if raw == u32::MAX {
        return Err(NvmlError::NotSupported);
    }
    i32::try_from(raw).map_err(|_| NvmlError::NotSupported)
}

/// Encode the GSP firmware mode as a 3-valued byte matching the
/// `all_smi_gsp_firmware_mode` gauge contract (0=disabled, 1=enabled,
/// 2=default), returning the raw `Result` for cache-layer error
/// classification.
fn gsp_firmware_mode_result(device: &nvml_wrapper::Device) -> Result<u8, NvmlError> {
    let mode = device.gsp_firmware_mode()?;
    // `mode.default == true` takes precedence: the driver reports that
    // firmware operates in its default mode regardless of the enabled flag.
    if mode.default {
        Ok(2)
    } else if mode.enabled {
        Ok(1)
    } else {
        Ok(0)
    }
}

/// Read the GSP firmware version string, returning the raw `Result` for
/// cache-layer error classification.
///
/// Trims trailing NUL bytes NVML leaves in the buffer — the high-level
/// wrapper already handles this, but the defensive trim future-proofs
/// against any buffer encoding surprises.
fn gsp_firmware_version_result(device: &nvml_wrapper::Device) -> Result<String, NvmlError> {
    let raw = device.gsp_firmware_version()?;
    let trimmed = raw.trim_end_matches('\0').trim().to_string();
    if trimmed.is_empty() {
        return Err(NvmlError::NotSupported);
    }
    Ok(trimmed)
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
            Ok(false) => continue,
            // `InvalidArg` means the index is past the physical link count —
            // stop probing early because higher indices will also fail.
            // `NotSupported` means this GPU has no NvLink hardware at all —
            // stop immediately.
            // Any other error (transient: `Unknown`, `GpuLost`) skips this
            // link but continues probing higher indices so that a transient
            // failure on link N does not silently hide links N+1..MAX.
            Err(NvmlError::InvalidArg | NvmlError::NotSupported) => break,
            Err(_transient) => continue,
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

    // ------------------------------------------------------------------
    // HardwareDetailCache field-level caching behaviour
    // ------------------------------------------------------------------

    /// Verify that a permanent-unavailable error (`NotSupported`) is cached
    /// as `None` so the fetcher is never called again.
    #[test]
    fn field_cache_permanent_error_caches_none() {
        let cache: FieldCache<i32> = Mutex::new(HashMap::new());
        let hw = HardwareDetailCache::new();

        let mut call_count = 0u32;
        let result = hw.get_or_fetch_field(&cache, 0, || {
            call_count += 1;
            Err(NvmlError::NotSupported)
        });
        assert!(result.is_none());
        assert_eq!(call_count, 1);

        // Second call must hit the cache — fetcher must NOT be called.
        let result2 = hw.get_or_fetch_field(&cache, 0, || {
            call_count += 1;
            Ok(42) // would override None if caching were broken
        });
        assert!(result2.is_none(), "cached None should persist");
        assert_eq!(call_count, 1, "fetcher must not be called on cache hit");
    }

    /// Verify that a transient error does NOT get cached, so the next poll
    /// retries the field.
    #[test]
    fn field_cache_transient_error_is_not_cached() {
        let cache: FieldCache<i32> = Mutex::new(HashMap::new());
        let hw = HardwareDetailCache::new();

        let mut call_count = 0u32;

        // First call: transient error — should NOT be cached.
        let result = hw.get_or_fetch_field(&cache, 0, || {
            call_count += 1;
            Err(NvmlError::Unknown)
        });
        assert!(result.is_none());
        assert_eq!(call_count, 1);

        // Second call: success — fetcher must be called again because the
        // transient error was not cached.
        let result2 = hw.get_or_fetch_field(&cache, 0, || {
            call_count += 1;
            Ok(7)
        });
        assert_eq!(result2, Some(7));
        assert_eq!(
            call_count, 2,
            "fetcher must be called again after transient miss"
        );
    }

    /// Verify that a successful fetch is cached and returns the same value
    /// without calling the fetcher a second time.
    #[test]
    fn field_cache_success_is_cached() {
        let cache: FieldCache<i32> = Mutex::new(HashMap::new());
        let hw = HardwareDetailCache::new();

        let mut call_count = 0u32;
        let _ = hw.get_or_fetch_field(&cache, 0, || {
            call_count += 1;
            Ok(42)
        });
        let result2 = hw.get_or_fetch_field(&cache, 0, || {
            call_count += 1;
            Ok(99)
        });
        assert_eq!(result2, Some(42));
        assert_eq!(call_count, 1, "fetcher must not be called on cache hit");
    }

    /// Verify that per-field caches are independent: a transient error on
    /// one field (e.g. gsp_mode) does not affect cached values on another
    /// field (e.g. numa).
    #[test]
    fn field_caches_are_independent() {
        let hw = HardwareDetailCache::new();

        // Pre-populate gsp_mode with a successful value.
        let _ = hw.get_or_fetch_field(&hw.gsp_mode, 0, || Ok(2u8));

        // Simulate a transient error on numa — must not clear the gsp_mode cache.
        let numa_result = hw.get_or_fetch_field(&hw.numa, 0, || Err(NvmlError::Unknown));
        assert!(numa_result.is_none());

        // gsp_mode cache must still hold its value.
        let mut mode_calls = 0u32;
        let mode_result = hw.get_or_fetch_field(&hw.gsp_mode, 0, || {
            mode_calls += 1;
            Ok(99u8) // should never be reached
        });
        assert_eq!(mode_result, Some(2u8));
        assert_eq!(
            mode_calls, 0,
            "gsp_mode fetcher must not be called — already cached"
        );
    }

    /// Verify `is_permanent_unavailable` classifies the correct variants.
    #[test]
    fn permanent_unavailable_classification() {
        assert!(is_permanent_unavailable(&NvmlError::NotSupported));
        assert!(is_permanent_unavailable(&NvmlError::FunctionNotFound));
        assert!(!is_permanent_unavailable(&NvmlError::Unknown));
        assert!(!is_permanent_unavailable(&NvmlError::GpuLost));
        assert!(!is_permanent_unavailable(&NvmlError::DriverNotLoaded));
    }

    // ------------------------------------------------------------------
    // Remote-device type mapping
    // ------------------------------------------------------------------

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
