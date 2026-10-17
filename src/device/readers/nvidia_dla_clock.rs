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

//! RTX Spark N1X DLA engine clock from D3DKMT node performance data
//! (issue #461).
//!
//! NVML answers `NotSupported` for every engine clock of the N1X NPU3
//! handle (driver 616.62), yet `nvidia-smi -q` prints a live `DLA Clocks`
//! section. It reads them from the kernel graphics stack:
//! `D3DKMTQueryAdapterInfo(KMTQAITYPE_NODEPERFDATA)` on the NPU's own
//! adapter returns the current and maximum engine frequency of node 0 in
//! hertz. This module does the same.
//!
//! The NPU is an MCDM compute-only adapter, so DXGI does not enumerate it
//! and NVML gives it no PCI identity. Its LUID is found with
//! `D3DKMTEnumAdapters3` asking for compute-only adapters, keeping the one
//! enumerated through ACPI with the `NVDA` vendor id (the GPU is a PCI
//! `0x10DE` device and is never compute-only on N1X).
//!
//! Like `windows_gpu_perf`, the selection and unit conversion are pure and
//! compiled under `test` on every platform; the FFI lives in [`ffi`] and
//! is Windows-only. Every failure degrades to "no D3DKMT clock", which
//! leaves the NVML reading untouched.

#[cfg(target_os = "windows")]
mod ffi;

#[cfg(target_os = "windows")]
pub(crate) use ffi::read_npu_node_clock;

use crate::device::readers::nvidia_soc::DlaReading;
use crate::device::readers::windows_gpu_perf::ids::AdapterLuid;

/// `D3DKMT_DEVICE_IDS::VendorID` of an ACPI-enumerated NVIDIA device: the
/// four ASCII bytes of the ACPI vendor id `NVDA` read as a little-endian
/// `u32` (`0x4144564E`). The N1X NPU reports this (device `200A`, from
/// `ACPI\NVDA200A`) where a PCI device would report `0x10DE`.
pub(crate) const ACPI_VENDOR_NVDA: u32 = u32::from_le_bytes(*b"NVDA");

/// `D3DKMT_ADAPTERTYPE::DisplaySupported`.
pub(crate) const ADAPTER_TYPE_DISPLAY_SUPPORTED: u32 = 1 << 1;

/// `D3DKMT_ADAPTERTYPE::ComputeOnly`.
pub(crate) const ADAPTER_TYPE_COMPUTE_ONLY: u32 = 1 << 11;

/// Highest frequency accepted from the driver, in MHz. Matches the remote
/// parser's plausibility bound for the DLA clock gauges.
const MAX_PLAUSIBLE_MHZ: u64 = 100_000;

/// What D3DKMT says about one enumerated adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AdapterCandidate {
    pub luid: AdapterLuid,
    /// `D3DKMT_DEVICE_IDS::VendorID`, `None` when the query failed.
    pub vendor_id: Option<u32>,
    /// `D3DKMT_ADAPTERTYPE` bit field, `None` when the query failed.
    pub adapter_type: Option<u32>,
}

impl AdapterCandidate {
    fn is_nvidia_npu(&self) -> bool {
        let compute_only = self.adapter_type.is_some_and(|bits| {
            bits & ADAPTER_TYPE_COMPUTE_ONLY != 0 && bits & ADAPTER_TYPE_DISPLAY_SUPPORTED == 0
        });
        compute_only && self.vendor_id == Some(ACPI_VENDOR_NVDA)
    }
}

/// Pick the N1X NPU adapter out of a compute-only-inclusive enumeration.
///
/// Requires exactly one ACPI `NVDA` compute-only adapter without display
/// support. No match is the normal answer on every host without an NPU;
/// several matches give no basis for attributing a clock to the one DLA
/// row, so they also yield `None` rather than a guess.
pub(crate) fn select_npu_adapter(candidates: &[AdapterCandidate]) -> Option<AdapterLuid> {
    let mut npus = candidates.iter().filter(|c| c.is_nvidia_npu());
    let first = npus.next()?;
    match npus.next() {
        None => Some(first.luid),
        Some(_) => None,
    }
}

/// Current and maximum engine clock of a D3DKMT node, in MHz.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct NodeClock {
    pub current_mhz: Option<u32>,
    pub max_mhz: Option<u32>,
}

/// Convert a hertz figure to whole MHz, dropping zero (not reported) and
/// anything past [`MAX_PLAUSIBLE_MHZ`].
///
/// Truncates like `nvidia-smi`: the N1X driver reports DVFS steps as
/// `1105999000` Hz, which `nvidia-smi` prints as `1105 MHz`.
fn hz_to_mhz(hz: u64) -> Option<u32> {
    let mhz = hz / 1_000_000;
    if mhz == 0 || mhz > MAX_PLAUSIBLE_MHZ {
        return None;
    }
    u32::try_from(mhz).ok()
}

/// Build a [`NodeClock`] from `D3DKMT_NODE_PERFDATA`'s `Frequency`,
/// `MaxFrequency` and `MaxFrequencyOC` fields (all in hertz). The
/// overclock maximum stands in only when the plain maximum is absent.
pub(crate) fn node_clock_from_hz(
    frequency: u64,
    max_frequency: u64,
    max_frequency_oc: u64,
) -> NodeClock {
    NodeClock {
        current_mhz: hz_to_mhz(frequency),
        max_mhz: hz_to_mhz(max_frequency).or_else(|| hz_to_mhz(max_frequency_oc)),
    }
}

/// Fold a D3DKMT node clock into a DLA reading. The D3DKMT figures are
/// what `nvidia-smi` prints, so they win; NVML's stay when D3DKMT has none.
pub(crate) fn apply_node_clock(reading: DlaReading, clock: Option<NodeClock>) -> DlaReading {
    let Some(clock) = clock else {
        return reading;
    };
    DlaReading {
        clock_mhz: clock.current_mhz.or(reading.clock_mhz),
        max_clock_mhz: clock.max_mhz.or(reading.max_clock_mhz),
        ..reading
    }
}

#[cfg(test)]
#[path = "nvidia_dla_clock_tests.rs"]
mod tests;
