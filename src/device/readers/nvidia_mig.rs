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

//! NVIDIA MIG (Multi-Instance GPU) information collection.
//!
//! This module queries NVML's MIG APIs and returns one [`MigGpuInfo`] row per
//! physical GPU that answers the MIG mode query — regardless of whether MIG
//! mode is currently on. On consumer cards, older datacenter GPUs
//! (pre-Ampere), and machines without MIG support the reader returns an empty
//! vector — the feature MUST be a silent no-op there.
//!
//! # Error handling contract
//!
//! * Any NVML error (`NotSupported`, `FunctionNotFound`, `Uninitialized`, …)
//!   for a given call is swallowed and treated as "feature unavailable".
//! * A physical GPU is included in the output whenever `mig_mode()` returns
//!   a value — enabled or disabled. Surfacing disabled rows lets downstream
//!   consumers observe the current MIG state (and its runtime transitions)
//!   via `all_smi_gpu_mig_mode = 0`. The pending mode is intentionally
//!   ignored: we monitor what is live now, not what would happen after a
//!   reboot.
//! * Per-instance enumeration is skipped entirely when MIG mode is disabled
//!   (there are no instances to read). For enabled GPUs it uses
//!   `mig_device_count()` (the maximum slot count) and skips slots that
//!   return `NotSupported` / `InvalidArg`, which is how NVML reports "this
//!   slot is not currently provisioned".
//! * Per-instance metric reads are independently best-effort — a single
//!   failed `utilization_rates` call does not drop the whole instance row.

use std::ffi::CStr;
use std::os::raw::c_uint;

use nvml_wrapper::Nvml;
use nvml_wrapper::error::nvml_try;

use crate::device::types::{MigGpuInfo, MigInstanceInfo};
use crate::utils::get_hostname;

/// NVML reports the maximum theoretical MIG slot count via
/// `nvmlDeviceGetMaxMigDeviceCount`. Real GPUs cap at 7 instances on
/// A100/H100, so we clamp at a defensive upper bound to avoid pathological
/// loops on a misbehaving driver.
const MAX_MIG_INSTANCES_PER_DEVICE: u32 = 64;

/// Defensive upper bound on the MIG profile name buffer. Real names like
/// `1g.5gb` / `7g.40gb` fit comfortably; we allocate 64 bytes for safety.
const MIG_NAME_BUFFER: usize = 64;

/// Collect MIG information for every physical NVIDIA GPU on the host.
///
/// Returns an empty vector when:
/// * NVML reports zero devices.
/// * No device responds successfully to `mig_mode()` (i.e. no MIG-capable
///   GPU is present — consumer cards / pre-Ampere datacenter GPUs).
///
/// A row is emitted for every MIG-capable GPU, with `mig_mode` reflecting
/// the current live state and `instances` empty whenever MIG mode is
/// disabled. This lets consumers observe disabled GPUs in dashboards and
/// catch runtime MIG toggles via `all_smi_gpu_mig_mode = 0`.
pub fn collect_mig_info(nvml: &Nvml) -> Vec<MigGpuInfo> {
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

        // Probe MIG mode. `NotSupported` is the expected response on consumer
        // cards and pre-Ampere datacenter GPUs — silently skip those, they
        // have no MIG story at all to report.
        let mode = match device.mig_mode() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let mig_enabled = mode.current == 1;

        let gpu_uuid = device.uuid().unwrap_or_else(|_| format!("GPU-{index}"));
        let gpu_name = device.name().unwrap_or_else(|_| "Unknown GPU".to_string());

        // Disabled MIG mode => no instances to enumerate. Keep the host row
        // around so the exporter still emits `all_smi_gpu_mig_mode = 0` for
        // it and downstream consumers can track runtime toggles.
        let instances = if mig_enabled {
            enumerate_mig_instances(nvml, &device)
        } else {
            Vec::new()
        };

        out.push(MigGpuInfo {
            host_id: hostname.clone(),
            hostname: hostname.clone(),
            instance: hostname.clone(),
            gpu_index: index,
            gpu_uuid,
            gpu_name,
            mig_mode: mig_enabled,
            instances,
        });
    }

    out
}

/// Enumerate live MIG instances under a parent physical GPU.
///
/// Returns an empty vector if `mig_device_count` errors (driver too old, MIG
/// disabled mid-call, missing permissions). Per-slot errors during the
/// enumeration loop are silently skipped — they are NVML's normal way of
/// signalling "no instance provisioned at this index".
fn enumerate_mig_instances(nvml: &Nvml, parent: &nvml_wrapper::Device) -> Vec<MigInstanceInfo> {
    let max_count = match parent.mig_device_count() {
        Ok(c) => c.min(MAX_MIG_INSTANCES_PER_DEVICE),
        Err(_) => return Vec::new(),
    };

    let mut out = Vec::with_capacity(max_count as usize);

    for slot in 0..max_count {
        let mig_device = match parent.mig_device_by_index(slot) {
            Ok(d) => d,
            // `NotSupported` / `InvalidArg` here means the slot is unprovisioned.
            // SELinux / cgroup misconfiguration also lands in this branch and
            // produces a silent skip per the contract documented at the top.
            Err(_) => continue,
        };

        out.push(collect_single_mig_instance(nvml, &mig_device, slot));
    }

    out
}

/// Collect metadata for a single MIG instance. All sub-queries are
/// `.ok()`-wrapped so that a failed read degrades to a default value rather
/// than killing the whole row.
fn collect_single_mig_instance(
    nvml: &Nvml,
    device: &nvml_wrapper::Device,
    instance_id: u32,
) -> MigInstanceInfo {
    let uuid = device.uuid().unwrap_or_default();

    let mem = device.memory_info().ok();
    let memory_used_bytes = mem.as_ref().map(|m| m.used).unwrap_or(0);
    let memory_total_bytes = mem.as_ref().map(|m| m.total).unwrap_or(0);

    let util = device.utilization_rates().ok();
    let utilization_gpu = util.as_ref().map(|u| u.gpu);
    let utilization_memory = util.as_ref().map(|u| u.memory);

    let gpu_instance_id = mig_gpu_instance_id(nvml, device);
    let compute_instance_id = mig_compute_instance_id(nvml, device);

    // Best-effort profile name. NVML exposes the per-instance name via the
    // generic `nvmlDeviceGetName` call when invoked on a MIG handle.
    let profile_name = device
        .name()
        .ok()
        .map(|s| extract_profile_suffix(&s))
        .unwrap_or_default();

    MigInstanceInfo {
        instance_id,
        gpu_instance_id,
        compute_instance_id,
        uuid,
        profile_name,
        utilization_gpu,
        utilization_memory,
        memory_used_bytes,
        memory_total_bytes,
    }
}

/// Query the GPU instance id for a MIG handle via raw NVML FFI.
///
/// nvml-wrapper 0.12 does not expose `nvmlDeviceGetGpuInstanceId` as a
/// high-level method (it is only present indirectly inside `ProcessInfo`),
/// so we call the symbol ourselves and degrade silently on any failure.
fn mig_gpu_instance_id(nvml: &Nvml, device: &nvml_wrapper::Device) -> Option<u32> {
    let sym = nvml.lib().nvmlDeviceGetGpuInstanceId.as_ref().ok()?;
    let mut id: c_uint = 0;
    // SAFETY: `Device::handle()` returns the same `nvmlDevice_t` NVML
    // expects. `id` is a valid u32 on the stack; NVML writes one u32.
    let result = unsafe { sym(device.handle(), &mut id) };
    nvml_try(result).ok()?;
    Some(id)
}

/// Query the compute instance id for a MIG handle via raw NVML FFI.
///
/// Same rationale as [`mig_gpu_instance_id`] — the high-level wrapper does
/// not expose it directly in 0.12, so we go through the raw symbol and
/// degrade silently on any failure.
fn mig_compute_instance_id(nvml: &Nvml, device: &nvml_wrapper::Device) -> Option<u32> {
    let sym = nvml.lib().nvmlDeviceGetComputeInstanceId.as_ref().ok()?;
    let mut id: c_uint = 0;
    // SAFETY: same invariants as `mig_gpu_instance_id` above — valid
    // device handle, valid out-pointer, NVML writes one u32.
    let result = unsafe { sym(device.handle(), &mut id) };
    nvml_try(result).ok()?;
    Some(id)
}

/// NVML returns full names like "MIG 1g.5gb Device" or "NVIDIA A100 MIG 2g.10gb".
/// Extract the slice profile fragment (`1g.5gb`) for compact display. Returns
/// the original string when no recognisable fragment is found.
fn extract_profile_suffix(raw: &str) -> String {
    // Heuristic: the slice fragment is always of the form `<digits>g.<digits>gb`.
    for token in raw.split_whitespace() {
        if is_mig_profile_token(token) {
            return token.to_string();
        }
    }
    raw.to_string()
}

/// `true` when `token` looks like a MIG slice profile (`1g.5gb`, `7g.80gb`,
/// `1c.1g.5gb` for compute-only slices, etc.). Rejects everything else.
fn is_mig_profile_token(token: &str) -> bool {
    let token = token.trim();
    if token.is_empty() || token.len() > MIG_NAME_BUFFER {
        return false;
    }
    // Must contain `g.` and end in `gb`.
    if !token.contains("g.") || !token.ends_with("gb") {
        return false;
    }
    // First character must be a digit (e.g. `1g.5gb`).
    token.chars().next().is_some_and(|c| c.is_ascii_digit())
}

/// Decode a NUL-terminated NVML C string buffer. Used by the few raw-FFI
/// helpers above when nvml-wrapper does not provide a Rust accessor; kept
/// here in the MIG module so it stays close to its sole caller.
#[allow(dead_code)]
fn decode_cstr(buf: &[i8]) -> String {
    // SAFETY: NVML guarantees NUL termination within the supplied buffer.
    let cstr = unsafe { CStr::from_ptr(buf.as_ptr() as *const _) };
    cstr.to_str().unwrap_or("").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_profile_suffix_returns_canonical_token() {
        assert_eq!(extract_profile_suffix("MIG 1g.5gb Device"), "1g.5gb");
        assert_eq!(extract_profile_suffix("NVIDIA A100 MIG 7g.40gb"), "7g.40gb");
        assert_eq!(extract_profile_suffix("3g.20gb"), "3g.20gb");
    }

    #[test]
    fn extract_profile_suffix_returns_input_when_no_token_present() {
        assert_eq!(
            extract_profile_suffix("Some Random Name"),
            "Some Random Name"
        );
        assert_eq!(extract_profile_suffix(""), "");
    }

    #[test]
    fn is_mig_profile_token_accepts_valid_slices() {
        assert!(is_mig_profile_token("1g.5gb"));
        assert!(is_mig_profile_token("2g.10gb"));
        assert!(is_mig_profile_token("3g.20gb"));
        assert!(is_mig_profile_token("7g.40gb"));
        assert!(is_mig_profile_token("7g.80gb"));
    }

    #[test]
    fn is_mig_profile_token_rejects_non_profiles() {
        assert!(!is_mig_profile_token(""));
        assert!(!is_mig_profile_token("MIG"));
        assert!(!is_mig_profile_token("1g.5g"));
        assert!(!is_mig_profile_token("g.5gb"));
        assert!(!is_mig_profile_token("xg.ygb"));
    }
}
