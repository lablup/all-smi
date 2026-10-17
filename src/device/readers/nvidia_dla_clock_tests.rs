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

use super::*;

/// `D3DKMT_ADAPTERTYPE` bits the N1X driver (616.62) reports.
const N1X_GPU_TYPE: u32 = 0xA32B;
const N1X_NPU_TYPE: u32 = 0x2B01;
const BASIC_RENDER_TYPE: u32 = 0x0105;

fn candidate(low: u32, vendor_id: Option<u32>, adapter_type: Option<u32>) -> AdapterCandidate {
    AdapterCandidate {
        luid: AdapterLuid::new(0, low),
        vendor_id,
        adapter_type,
    }
}

/// The enumeration `D3DKMTEnumAdapters3` returned on SPARK-WIN01: GPU,
/// NPU, Microsoft Basic Render Driver.
fn n1x_adapters() -> Vec<AdapterCandidate> {
    vec![
        candidate(0xEEB1, Some(0x10DE), Some(N1X_GPU_TYPE)),
        candidate(0x10EFA, Some(0x4144_564E), Some(N1X_NPU_TYPE)),
        candidate(0x10DF8, Some(0x1414), Some(BASIC_RENDER_TYPE)),
    ]
}

#[test]
fn acpi_vendor_constant_matches_the_driver_value() {
    assert_eq!(ACPI_VENDOR_NVDA, 0x4144_564E);
}

#[test]
fn n1x_bits_decode_as_expected() {
    assert_ne!(N1X_NPU_TYPE & ADAPTER_TYPE_COMPUTE_ONLY, 0);
    assert_eq!(N1X_NPU_TYPE & ADAPTER_TYPE_DISPLAY_SUPPORTED, 0);
    assert_eq!(N1X_GPU_TYPE & ADAPTER_TYPE_COMPUTE_ONLY, 0);
    assert_ne!(N1X_GPU_TYPE & ADAPTER_TYPE_DISPLAY_SUPPORTED, 0);
}

#[test]
fn selects_the_n1x_npu() {
    assert_eq!(
        select_npu_adapter(&n1x_adapters()),
        Some(AdapterLuid::new(0, 0x10EFA))
    );
}

#[test]
fn no_npu_yields_none() {
    let without_npu: Vec<_> = n1x_adapters()
        .into_iter()
        .filter(|c| c.luid.low != 0x10EFA)
        .collect();
    assert_eq!(select_npu_adapter(&without_npu), None);
    assert_eq!(select_npu_adapter(&[]), None);
}

#[test]
fn nvidia_pci_compute_only_gpu_is_not_an_npu() {
    // A data-center GPU in MCDM mode is compute-only NVIDIA hardware too,
    // but PCI-enumerated.
    let mcdm_gpu = candidate(0x2000, Some(0x10DE), Some(0x0801));
    assert_eq!(select_npu_adapter(&[mcdm_gpu]), None);
}

#[test]
fn failed_queries_disqualify_a_candidate() {
    assert_eq!(
        select_npu_adapter(&[candidate(0x10EFA, None, Some(N1X_NPU_TYPE))]),
        None
    );
    assert_eq!(
        select_npu_adapter(&[candidate(0x10EFA, Some(ACPI_VENDOR_NVDA), None)]),
        None
    );
}

#[test]
fn display_capable_nvda_adapter_is_not_an_npu() {
    let bits = N1X_NPU_TYPE | ADAPTER_TYPE_DISPLAY_SUPPORTED;
    assert_eq!(
        select_npu_adapter(&[candidate(1, Some(ACPI_VENDOR_NVDA), Some(bits))]),
        None
    );
}

#[test]
fn ambiguous_npus_yield_none() {
    let npu = |low| candidate(low, Some(ACPI_VENDOR_NVDA), Some(N1X_NPU_TYPE));
    assert_eq!(select_npu_adapter(&[npu(1), npu(2)]), None);
}

#[test]
fn node_clock_converts_the_probed_n1x_values() {
    assert_eq!(
        node_clock_from_hz(430_000_000, 1_562_000_000, 1_562_000_000),
        NodeClock {
            current_mhz: Some(430),
            max_mhz: Some(1562),
        }
    );
}

#[test]
fn node_clock_truncates_like_nvidia_smi() {
    // The driver's DVFS steps on N1X, printed by nvidia-smi as 1105 MHz.
    let clock = node_clock_from_hz(1_105_999_000, 1_562_999_999, 0);
    assert_eq!(clock.current_mhz, Some(1105));
    assert_eq!(clock.max_mhz, Some(1562));
}

#[test]
fn node_clock_drops_zero_and_implausible_values() {
    assert_eq!(node_clock_from_hz(0, 0, 0), NodeClock::default());
    assert_eq!(node_clock_from_hz(999_999, 0, 0).current_mhz, None);
    assert_eq!(node_clock_from_hz(u64::MAX, 0, 0).current_mhz, None);
    assert_eq!(node_clock_from_hz(200_000_000_000, 0, 0).current_mhz, None);
}

#[test]
fn node_clock_falls_back_to_the_overclock_maximum() {
    assert_eq!(node_clock_from_hz(0, 0, 1_600_000_000).max_mhz, Some(1600));
}

#[test]
fn node_clock_overrides_nvml_and_keeps_other_fields() {
    let nvml = DlaReading {
        utilization: Some(3.0),
        clock_mhz: Some(999),
        memory_clock_mhz: Some(9400),
        ..Default::default()
    };
    let merged = apply_node_clock(
        nvml,
        Some(NodeClock {
            current_mhz: Some(430),
            max_mhz: Some(1562),
        }),
    );
    assert_eq!(merged.clock_mhz, Some(430));
    assert_eq!(merged.max_clock_mhz, Some(1562));
    assert_eq!(merged.utilization, Some(3.0));
    assert_eq!(merged.memory_clock_mhz, Some(9400));
}

#[test]
fn missing_node_clock_leaves_the_nvml_reading() {
    let nvml = DlaReading {
        clock_mhz: Some(999),
        ..Default::default()
    };
    assert_eq!(apply_node_clock(nvml, None), nvml);
    assert_eq!(
        apply_node_clock(nvml, Some(NodeClock::default())).clock_mhz,
        Some(999)
    );
}
