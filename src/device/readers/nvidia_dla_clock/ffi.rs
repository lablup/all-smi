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

//! D3DKMT calls behind [`read_npu_node_clock`].
//!
//! The adapter is discovered once and its kernel handle kept open for the
//! life of the process, so a poll costs one `D3DKMTQueryAdapterInfo`.
//! Repeated query failures (a driver reset invalidates the handle) close
//! it and schedule a rediscovery after [`REDISCOVER_AFTER`].
//!
//! `D3DKMTEnumAdapters3` is resolved at run time rather than linked: the
//! `windows` crate imports it from `api-ms-win-dx-d3dkmt-l1-1-6.dll`, an
//! API set older Windows 10 builds lack, and a load-time import of a
//! missing API set would stop `all-smi.exe` from starting at all. The
//! other three entry points are plain `gdi32.dll` exports since Windows 8.

use std::ffi::c_void;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use libloading::os::windows::LOAD_LIBRARY_SEARCH_SYSTEM32;
use windows::Wdk::Graphics::Direct3D::{
    D3DKMT_ADAPTERINFO, D3DKMT_ADAPTERTYPE, D3DKMT_CLOSEADAPTER, D3DKMT_ENUMADAPTERS_FILTER,
    D3DKMT_ENUMADAPTERS3, D3DKMT_NODE_PERFDATA, D3DKMT_OPENADAPTERFROMLUID,
    D3DKMT_QUERY_DEVICE_IDS, D3DKMT_QUERYADAPTERINFO, D3DKMTCloseAdapter,
    D3DKMTOpenAdapterFromLuid, D3DKMTQueryAdapterInfo, KMTQAITYPE_ADAPTERTYPE,
    KMTQAITYPE_NODEPERFDATA, KMTQAITYPE_PHYSICALADAPTERDEVICEIDS, KMTQUERYADAPTERINFOTYPE,
};
use windows::Win32::Foundation::{LUID, NTSTATUS};

use super::{AdapterCandidate, NodeClock, node_clock_from_hz, select_npu_adapter};
use crate::device::readers::windows_gpu_perf::ids::AdapterLuid;

// Layouts the probe on N1X (Windows 11 ARM64, driver 616.62) confirmed;
// a binding change that alters them would make the kernel reject or
// misread the buffer, so fail the build instead.
#[cfg(target_pointer_width = "64")]
const _: () = assert!(std::mem::size_of::<D3DKMT_NODE_PERFDATA>() == 56);
const _: () = assert!(std::mem::size_of::<D3DKMT_QUERY_DEVICE_IDS>() == 28);
const _: () = assert!(std::mem::size_of::<D3DKMT_ADAPTERTYPE>() == 4);

/// `MAX_ENUM_ADAPTERS` from `d3dkmthk.h`.
const MAX_ENUM_ADAPTERS: usize = 16;

/// `D3DKMT_ENUMADAPTERS_FILTER::IncludeComputeOnly`.
const FILTER_INCLUDE_COMPUTE_ONLY: u64 = 1;

/// Consecutive query failures tolerated before the handle is dropped.
const MAX_QUERY_FAILURES: u8 = 3;

/// Wait before trying discovery again after it failed or the handle went bad.
const REDISCOVER_AFTER: Duration = Duration::from_secs(60);

type EnumAdapters3Fn = unsafe extern "system" fn(*mut D3DKMT_ENUMADAPTERS3) -> NTSTATUS;

enum State {
    /// Discovery not attempted yet, or due again at the given instant.
    Undiscovered { not_before: Option<Instant> },
    /// The NPU adapter is open.
    Open { handle: u32, failures: u8 },
}

static STATE: Mutex<State> = Mutex::new(State::Undiscovered { not_before: None });

/// Current and maximum engine clock of the N1X NPU, or `None` on any host
/// without one and whenever a D3DKMT call fails.
///
/// Callers read this before their NVML or `nvidia-smi` queries reach the
/// NPU. Those queries raise the engine to its next DVFS step (1105 MHz on
/// N1X) for about 10 ms, so a read inside that window reports the
/// monitor's own load rather than the clock `nvidia-smi` shows.
pub(crate) fn read_npu_node_clock() -> Option<NodeClock> {
    // Every transition below assigns a whole `State`, so the value behind
    // a poisoned lock is still valid; recovering keeps the D3DKMT clock and
    // the open handle instead of dropping both for the process lifetime.
    let mut state = STATE.lock().unwrap_or_else(PoisonError::into_inner);
    if let State::Undiscovered { not_before } = *state {
        if not_before.is_some_and(|t| Instant::now() < t) {
            return None;
        }
        match discover_npu().and_then(open_adapter) {
            Some(handle) => {
                *state = State::Open {
                    handle,
                    failures: 0,
                }
            }
            None => {
                tracing::debug!("D3DKMT: no NVIDIA NPU adapter; DLA clock stays on NVML");
                *state = State::Undiscovered {
                    not_before: Some(Instant::now() + REDISCOVER_AFTER),
                };
                return None;
            }
        }
    }
    let State::Open { handle, failures } = &mut *state else {
        return None;
    };
    match query_node_perf(*handle, 0) {
        Some(data) => {
            *failures = 0;
            Some(node_clock_from_hz(
                data.Frequency,
                data.MaxFrequency,
                data.MaxFrequencyOC,
            ))
        }
        None => {
            *failures += 1;
            if *failures >= MAX_QUERY_FAILURES {
                tracing::debug!("D3DKMT: NPU node perf query keeps failing; reopening later");
                close_adapter(*handle);
                *state = State::Undiscovered {
                    not_before: Some(Instant::now() + REDISCOVER_AFTER),
                };
            }
            None
        }
    }
}

/// Enumerate adapters including compute-only ones and pick the NPU.
fn discover_npu() -> Option<AdapterLuid> {
    // SAFETY: gdi32 is a KnownDLL already mapped into this process (the
    // other D3DKMT imports come from it); `LOAD_LIBRARY_SEARCH_SYSTEM32`
    // keeps the lookup inside System32 regardless.
    let library = unsafe {
        libloading::os::windows::Library::load_with_flags("gdi32.dll", LOAD_LIBRARY_SEARCH_SYSTEM32)
    }
    .ok()?;
    // SAFETY: the signature is `NTSTATUS D3DKMTEnumAdapters3(D3DKMT_ENUMADAPTERS3*)`
    // from `d3dkmthk.h`, matching the `windows` crate's own declaration.
    let enum_adapters3 = unsafe { library.get::<EnumAdapters3Fn>(b"D3DKMTEnumAdapters3\0") }
        .ok()
        .map(|symbol| *symbol)?;

    let mut adapters = [D3DKMT_ADAPTERINFO::default(); MAX_ENUM_ADAPTERS];
    let mut request = D3DKMT_ENUMADAPTERS3 {
        Filter: D3DKMT_ENUMADAPTERS_FILTER {
            Value: FILTER_INCLUDE_COMPUTE_ONLY,
        },
        NumAdapters: MAX_ENUM_ADAPTERS as u32,
        pAdapters: adapters.as_mut_ptr(),
    };
    // SAFETY: `pAdapters` points at `NumAdapters` writable entries, and the
    // library stays loaded for the duration of the call.
    if !unsafe { enum_adapters3(&mut request) }.is_ok() {
        return None;
    }
    let count = (request.NumAdapters as usize).min(MAX_ENUM_ADAPTERS);

    // Every enumerated handle is open and must be closed, whatever the
    // outcome; the chosen adapter is reopened by LUID afterwards.
    let candidates: Vec<AdapterCandidate> = adapters[..count]
        .iter()
        .map(|info| {
            let candidate = AdapterCandidate {
                luid: AdapterLuid {
                    high: info.AdapterLuid.HighPart,
                    low: info.AdapterLuid.LowPart,
                },
                vendor_id: query_vendor_id(info.hAdapter),
                adapter_type: query_adapter_type(info.hAdapter),
            };
            close_adapter(info.hAdapter);
            candidate
        })
        .collect();
    select_npu_adapter(&candidates)
}

fn open_adapter(luid: AdapterLuid) -> Option<u32> {
    let mut request = D3DKMT_OPENADAPTERFROMLUID {
        AdapterLuid: LUID {
            LowPart: luid.low,
            HighPart: luid.high,
        },
        hAdapter: 0,
    };
    // SAFETY: `request` is a valid, initialised in/out struct.
    unsafe { D3DKMTOpenAdapterFromLuid(&mut request) }
        .is_ok()
        .then_some(request.hAdapter)
}

fn close_adapter(handle: u32) {
    let request = D3DKMT_CLOSEADAPTER { hAdapter: handle };
    // SAFETY: `handle` came from a successful open or enumeration and is
    // closed exactly once; the status is irrelevant on teardown.
    let _ = unsafe { D3DKMTCloseAdapter(&request) };
}

/// `D3DKMTQueryAdapterInfo` into a plain-data output struct.
fn query<T: Copy>(handle: u32, kind: KMTQUERYADAPTERINFOTYPE, mut data: T) -> Option<T> {
    let mut request = D3DKMT_QUERYADAPTERINFO {
        hAdapter: handle,
        Type: kind,
        pPrivateDriverData: (&mut data as *mut T).cast::<c_void>(),
        PrivateDriverDataSize: std::mem::size_of::<T>() as u32,
    };
    // SAFETY: the private data pointer and size describe `data`, a live,
    // properly aligned value of the struct type `kind` expects; it is only
    // returned when the kernel reports success.
    unsafe { D3DKMTQueryAdapterInfo(&mut request) }
        .is_ok()
        .then_some(data)
}

fn query_node_perf(handle: u32, node: u32) -> Option<D3DKMT_NODE_PERFDATA> {
    let input = D3DKMT_NODE_PERFDATA {
        NodeOrdinal: node,
        ..Default::default()
    };
    query(handle, KMTQAITYPE_NODEPERFDATA, input)
}

fn query_vendor_id(handle: u32) -> Option<u32> {
    query(
        handle,
        KMTQAITYPE_PHYSICALADAPTERDEVICEIDS,
        D3DKMT_QUERY_DEVICE_IDS::default(),
    )
    .map(|ids| ids.DeviceIds.VendorID)
}

fn query_adapter_type(handle: u32) -> Option<u32> {
    query(
        handle,
        KMTQAITYPE_ADAPTERTYPE,
        D3DKMT_ADAPTERTYPE::default(),
    )
    // SAFETY: both union members are a plain `u32` view of the same
    // bits, so reading `Value` is always defined.
    .map(|kind| unsafe { kind.Anonymous.Value })
}
