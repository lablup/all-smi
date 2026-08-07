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

//! DXGI adapter enumeration.
//!
//! DXGI is the authoritative source for how much dedicated video memory
//! a card has. `Win32_VideoController.AdapterRAM`, which the WMI-only
//! readers used before this module existed, is a `uint32` in the WMI
//! schema and therefore saturates or wraps on anything above 4 GB, which
//! today is most discrete cards.
//! `DXGI_ADAPTER_DESC1::DedicatedVideoMemory` is `SIZE_T` and reports
//! the true figure.
//!
//! DXGI also hands out the adapter `LUID` and the PCI vendor / device
//! identifiers, which is what lets [`super::ids::match_adapter`] pair a
//! WMI row with a PDH counter instance.

use super::ids::{AdapterIdentity, AdapterLuid};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ADAPTER_DESC1, DXGI_ADAPTER_FLAG, DXGI_ADAPTER_FLAG_SOFTWARE,
    DXGI_MEMORY_SEGMENT_GROUP_LOCAL, DXGI_QUERY_VIDEO_MEMORY_INFO, IDXGIAdapter1, IDXGIAdapter3,
    IDXGIFactory1,
};
use windows::core::Interface;

/// One physical adapter as DXGI describes it.
#[derive(Clone, Debug)]
pub struct DxgiAdapter {
    pub identity: AdapterIdentity,
    /// `DXGI_ADAPTER_DESC1::DedicatedVideoMemory`, in bytes.
    pub dedicated_video_memory: u64,
    /// `DXGI_QUERY_VIDEO_MEMORY_INFO::Budget` for the local memory
    /// segment, in bytes, when `IDXGIAdapter3` is available.
    ///
    /// This is **process-scoped**: it is the budget the OS grants *this*
    /// process, not a system-wide figure. It is surfaced as a detail
    /// field for diagnostics and is deliberately not used as the
    /// device's used-memory value; the system-wide number comes from the
    /// PDH `GPU Adapter Memory` counter instead.
    pub process_budget: Option<u64>,
    /// `DXGI_QUERY_VIDEO_MEMORY_INFO::CurrentUsage`, in bytes. Same
    /// process-scoped caveat as [`Self::process_budget`].
    pub process_current_usage: Option<u64>,
}

/// Enumerate every hardware adapter DXGI knows about.
///
/// Software adapters (the Microsoft Basic Render Driver, and the WARP
/// device that a headless or RDP session presents) are filtered out:
/// they have no meaningful memory or utilization story and would
/// otherwise be matched against a real WMI row by the ordinal fallback.
///
/// Returns an empty vector rather than an error on any failure. A host
/// where DXGI cannot be reached is a host that should quietly fall back
/// to the WMI baseline, not one that should emit diagnostics on every
/// poll.
pub fn enumerate() -> Vec<DxgiAdapter> {
    let factory: IDXGIFactory1 = match unsafe { CreateDXGIFactory1() } {
        Ok(factory) => factory,
        Err(_) => return Vec::new(),
    };

    let mut adapters = Vec::new();
    for index in 0.. {
        // Enumeration ends with DXGI_ERROR_NOT_FOUND. Any other error is
        // equally terminal for our purposes.
        let adapter: IDXGIAdapter1 = match unsafe { factory.EnumAdapters1(index) } {
            Ok(adapter) => adapter,
            Err(_) => break,
        };

        let desc: DXGI_ADAPTER_DESC1 = match unsafe { adapter.GetDesc1() } {
            Ok(desc) => desc,
            Err(_) => continue,
        };

        if DXGI_ADAPTER_FLAG(desc.Flags as i32) == DXGI_ADAPTER_FLAG_SOFTWARE {
            continue;
        }

        let (process_budget, process_current_usage) = query_video_memory(&adapter);

        adapters.push(DxgiAdapter {
            identity: AdapterIdentity {
                luid: AdapterLuid {
                    high: desc.AdapterLuid.HighPart,
                    low: desc.AdapterLuid.LowPart,
                },
                vendor_id: desc.VendorId,
                device_id: desc.DeviceId,
                description: widestring_to_string(&desc.Description),
            },
            dedicated_video_memory: desc.DedicatedVideoMemory as u64,
            process_budget,
            process_current_usage,
        });
    }

    adapters
}

/// Read the local-segment video memory info, when the adapter supports
/// `IDXGIAdapter3` (Windows 10 and later).
fn query_video_memory(adapter: &IDXGIAdapter1) -> (Option<u64>, Option<u64>) {
    let Ok(adapter3) = adapter.cast::<IDXGIAdapter3>() else {
        return (None, None);
    };
    let mut info = DXGI_QUERY_VIDEO_MEMORY_INFO::default();
    // Node index 0: all-smi does not model linked-adapter (SLI /
    // CrossFire) node topology, and node 0 is the only one present on a
    // single-GPU system.
    if unsafe { adapter3.QueryVideoMemoryInfo(0, DXGI_MEMORY_SEGMENT_GROUP_LOCAL, &mut info) }
        .is_err()
    {
        return (None, None);
    }
    (Some(info.Budget), Some(info.CurrentUsage))
}

/// Convert a fixed-size, NUL-padded UTF-16 buffer to a `String`.
fn widestring_to_string(buffer: &[u16]) -> String {
    let end = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..end]).trim().to_string()
}
