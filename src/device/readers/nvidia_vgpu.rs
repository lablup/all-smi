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

//! NVIDIA vGPU information collection.
//!
//! This module queries NVML's vGPU APIs and returns [`VgpuHostInfo`] rows for
//! every physical GPU that participates in NVIDIA vGPU virtualization. On
//! bare-metal hosts (or when NVML returns `NotSupported` for any vGPU call)
//! the reader returns an empty vector — the feature MUST be a silent no-op.
//!
//! # Error handling contract
//!
//! * Any NVML error (`NotSupported`, `FunctionNotFound`, `Uninitialized`,
//!   …) for a given call is swallowed and treated as "feature unavailable".
//! * A physical GPU is included in the output only when `vgpu_host_mode()`
//!   succeeds. This is the canonical "is this GPU vGPU-capable?" probe.
//! * Individual vGPU instance queries that fail (e.g. UUID lookup) degrade
//!   to empty strings / `None`; the instance is still reported.

use std::collections::HashMap;
use std::ffi::CStr;
use std::os::raw::c_uint;

use nvml_wrapper::Nvml;
use nvml_wrapper::enum_wrappers::device::HostVgpuMode;
use nvml_wrapper::error::nvml_try;

use crate::device::types::{VgpuHostInfo, VgpuInfo};
use crate::utils::get_hostname;

/// Maximum buffer size for NVML vGPU UUID / VM id strings. NVML documents
/// the UUID length at 80 bytes; we allocate 96 for safety.
const VGPU_STRING_BUFFER: usize = 96;

/// Convert the high-level [`HostVgpuMode`] enum into the stable string label
/// we surface to the UI and Prometheus exporter.
fn host_mode_label(mode: HostVgpuMode) -> &'static str {
    match mode {
        HostVgpuMode::NonSriov => "NonSriov",
        HostVgpuMode::Sriov => "Sriov",
    }
}

/// Numeric encoding of the host vGPU mode for Prometheus. Matches the C enum
/// values in NVML headers so that downstream dashboards can correlate the
/// gauge with the `host_mode` label.
///
/// Used by the exporter in `api/metrics/vgpu.rs` via its string-based
/// mirror — this function is the canonical source in Rust-land.
#[allow(dead_code)] // Consumed only by tests; exporter mirrors the mapping on strings.
fn host_mode_code(mode: HostVgpuMode) -> u32 {
    match mode {
        HostVgpuMode::NonSriov => 0,
        HostVgpuMode::Sriov => 1,
    }
}

/// Read-only reference to the host vGPU mode encoded for Prometheus.
///
/// Called from `api/metrics/vgpu.rs` — kept near the other constants so the
/// mapping stays in a single place.
#[allow(dead_code)]
pub fn parse_host_mode_code(label: &str) -> u32 {
    match label {
        "Sriov" => 1,
        "NonSriov" => 0,
        _ => u32::MAX,
    }
}

/// Collect vGPU information for every physical NVIDIA GPU on the host.
///
/// Returns an empty vector when:
/// * NVML reports zero devices.
/// * No device responds to `vgpu_host_mode()` (bare-metal host or driver
///   too old to expose the vGPU API family).
pub fn collect_vgpu_info(nvml: &Nvml) -> Vec<VgpuHostInfo> {
    let mut out = Vec::new();

    let device_count = match nvml.device_count() {
        Ok(n) => n,
        Err(_) => return out,
    };

    let hostname = get_hostname();

    for index in 0..device_count {
        let device = match nvml.device_by_index(index) {
            Ok(d) => d,
            Err(_) => continue,
        };

        // Probe whether this device is vGPU-capable. `NotSupported` is the
        // normal response on bare-metal; we intentionally do not log.
        let host_mode = match device.vgpu_host_mode() {
            Ok(m) => m,
            Err(_) => continue,
        };

        let gpu_uuid = device.uuid().unwrap_or_else(|_| format!("GPU-{index}"));
        let gpu_name = device.name().unwrap_or_else(|_| "Unknown GPU".to_string());

        // Scheduler metadata — both calls are best-effort.
        let (scheduler_policy, scheduler_arr_mode) = match device.vgpu_scheduler_state() {
            Ok(state) => (state.scheduler_policy, state.arr_mode),
            Err(_) => (0, 0),
        };
        let is_arr_supported = device
            .vgpu_scheduler_capabilities()
            .map(|caps| caps.is_arr_mode_supported)
            .unwrap_or(false);

        // Collect active vGPU instances.
        let vgpus = match device.active_vgpus() {
            Ok(instances) => instances
                .into_iter()
                .map(|id| collect_single_vgpu(nvml, &device, id))
                .collect(),
            Err(_) => Vec::new(),
        };

        let mut detail = HashMap::new();
        detail.insert("vgpu_capable".to_string(), "true".to_string());
        if is_arr_supported {
            detail.insert("arr_supported".to_string(), "true".to_string());
        }

        out.push(VgpuHostInfo {
            host_id: hostname.clone(),
            hostname: hostname.clone(),
            instance: hostname.clone(),
            gpu_index: index,
            gpu_uuid,
            gpu_name,
            host_mode: host_mode_label(host_mode).to_string(),
            scheduler_policy,
            scheduler_arr_mode,
            is_arr_supported,
            vgpus,
            detail,
        });
    }

    out
}

/// Collect metadata for a single vGPU instance. All sub-queries are wrapped
/// so a failing call degrades to an empty / `None` field rather than killing
/// the whole collection.
fn collect_single_vgpu(nvml: &Nvml, device: &nvml_wrapper::Device, instance_id: u32) -> VgpuInfo {
    let uuid = vgpu_uuid(nvml, instance_id).unwrap_or_default();
    let vm_id = vgpu_vm_id(nvml, instance_id).unwrap_or_default();
    let fb_used_bytes = vgpu_fb_usage(nvml, instance_id).unwrap_or(0);
    let (vgpu_type_name, fb_total_bytes) = vgpu_type_info(nvml, instance_id).unwrap_or_default();

    // Accounting stats: if vgpu_accounting_pids returns a non-empty list,
    // take the most recent PID's stats as a representative utilization
    // reading. vGPU accounting only reports per-PID samples, so we
    // conservatively surface the first available sample.
    let (gpu_utilization, memory_utilization, is_active) =
        match device.vgpu_accounting_pids(instance_id) {
            Ok(pids) if !pids.is_empty() => {
                match device.vgpu_accounting_instance(instance_id, pids[0]) {
                    Ok(stats) => (
                        stats.gpu_utilization,
                        stats.memory_utilization,
                        stats.is_running,
                    ),
                    Err(_) => (None, None, true),
                }
            }
            _ => (None, None, false),
        };

    VgpuInfo {
        instance_id,
        uuid,
        vm_id,
        vgpu_type_name,
        fb_used_bytes,
        fb_total_bytes,
        gpu_utilization,
        memory_utilization,
        is_active,
    }
}

/// Safely query the UUID of a vGPU instance via the raw NVML FFI.
fn vgpu_uuid(nvml: &Nvml, instance_id: u32) -> Option<String> {
    let sym = nvml.lib().nvmlVgpuInstanceGetUUID.as_ref().ok()?;

    let mut buf = vec![0_i8; VGPU_STRING_BUFFER];
    // SAFETY: `buf` is valid for writes of at most `VGPU_STRING_BUFFER`
    // bytes; we pass the correct length to NVML. The call is #[cfg(unix)]
    // safe because we only compile this reader on platforms where NVML
    // lives, and NVML writes a NUL-terminated C string.
    let result = unsafe {
        sym(
            instance_id,
            buf.as_mut_ptr() as *mut _,
            VGPU_STRING_BUFFER as c_uint,
        )
    };
    nvml_try(result).ok()?;

    // SAFETY: NVML wrote a NUL-terminated C string into the buffer.
    let cstr = unsafe { CStr::from_ptr(buf.as_ptr() as *const _) };
    cstr.to_str().ok().map(|s| s.to_string())
}

/// Query the VM id for a vGPU instance. NVML reports VM id type via the
/// trailing out parameter, but for monitoring we only need the textual id.
fn vgpu_vm_id(nvml: &Nvml, instance_id: u32) -> Option<String> {
    let sym = nvml.lib().nvmlVgpuInstanceGetVmID.as_ref().ok()?;

    let mut buf = vec![0_i8; VGPU_STRING_BUFFER];
    let mut vm_id_type: c_uint = 0;
    // SAFETY: we pass correctly-sized pointers; NVML writes a NUL-terminated C
    // string and a u32 value.
    let result = unsafe {
        sym(
            instance_id,
            buf.as_mut_ptr() as *mut _,
            VGPU_STRING_BUFFER as c_uint,
            &mut vm_id_type,
        )
    };
    nvml_try(result).ok()?;

    // SAFETY: NVML populated a NUL-terminated C string.
    let cstr = unsafe { CStr::from_ptr(buf.as_ptr() as *const _) };
    cstr.to_str().ok().map(|s| s.to_string())
}

/// Query the framebuffer usage in bytes for a vGPU instance.
fn vgpu_fb_usage(nvml: &Nvml, instance_id: u32) -> Option<u64> {
    let sym = nvml.lib().nvmlVgpuInstanceGetFbUsage.as_ref().ok()?;

    let mut usage: u64 = 0;
    // SAFETY: `usage` is a valid u64 on the stack; NVML writes one u64.
    let result = unsafe { sym(instance_id, &mut usage) };
    nvml_try(result).ok()?;
    Some(usage)
}

/// Query the vGPU type name and framebuffer size for the given instance by
/// first resolving the type id, then looking up the type-level metadata.
fn vgpu_type_info(nvml: &Nvml, instance_id: u32) -> Option<(String, u64)> {
    // Step 1: resolve type id from instance id.
    let type_sym = nvml.lib().nvmlVgpuInstanceGetType.as_ref().ok()?;
    let mut type_id: c_uint = 0;
    // SAFETY: `type_id` is a valid u32 on the stack; NVML writes one u32.
    let result = unsafe { type_sym(instance_id, &mut type_id) };
    nvml_try(result).ok()?;

    // Step 2: resolve the type name (char buffer).
    let name_sym = nvml.lib().nvmlVgpuTypeGetName.as_ref().ok()?;
    let mut buf = vec![0_i8; VGPU_STRING_BUFFER];
    let mut size: c_uint = VGPU_STRING_BUFFER as c_uint;
    // SAFETY: same invariants as other NVML string queries above.
    let result = unsafe { name_sym(type_id, buf.as_mut_ptr() as *mut _, &mut size) };
    let name = if nvml_try(result).is_ok() {
        // SAFETY: NVML populated a NUL-terminated C string.
        unsafe { CStr::from_ptr(buf.as_ptr() as *const _) }
            .to_str()
            .unwrap_or("")
            .to_string()
    } else {
        String::new()
    };

    // Step 3: resolve the framebuffer total size.
    let fb_sym = nvml.lib().nvmlVgpuTypeGetFramebufferSize.as_ref().ok()?;
    let mut fb_total: u64 = 0;
    // SAFETY: `fb_total` is a valid u64 on the stack; NVML writes one u64.
    let result = unsafe { fb_sym(type_id, &mut fb_total) };
    nvml_try(result).ok()?;

    Some((name, fb_total))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_mode_label_maps_known_variants() {
        assert_eq!(host_mode_label(HostVgpuMode::NonSriov), "NonSriov");
        assert_eq!(host_mode_label(HostVgpuMode::Sriov), "Sriov");
    }

    #[test]
    fn host_mode_code_is_stable() {
        assert_eq!(host_mode_code(HostVgpuMode::NonSriov), 0);
        assert_eq!(host_mode_code(HostVgpuMode::Sriov), 1);
    }

    #[test]
    fn parse_host_mode_code_round_trips() {
        assert_eq!(parse_host_mode_code("NonSriov"), 0);
        assert_eq!(parse_host_mode_code("Sriov"), 1);
        assert_eq!(parse_host_mode_code("Disabled"), u32::MAX);
    }
}
