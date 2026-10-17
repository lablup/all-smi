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

//! NVIDIA SoC handling for the NVML reader (issue #459).
//!
//! Two SoC families reach [`super::nvidia::NvidiaGpuReader`] with shapes a
//! discrete-card reader does not expect:
//!
//! * **DGX Spark GB10** (Linux): NVML reports no framebuffer at all, so
//!   the GPU is backed by system memory.
//! * **RTX Spark N1X** (Windows on ARM64): NVML reports a framebuffer
//!   carve-out of the LPDDR5X package, the GPU sits on NVLink-C2C rather
//!   than PCIe (NVML still answers the PCIe queries with a `Gen 1 x1`
//!   stub), and the integrated NPU3 accelerator is enumerated as a second
//!   NVML handle named `NVIDIA NPU` with a `DLA-` UUID.
//!
//! The NPU is not given a device row of its own. Like Jetson's DLA and the
//! Apple Neural Engine, it is an accelerator attached to the SoC GPU, so its
//! readings are folded into that GPU's row: utilization into
//! [`GpuInfo::dla_utilization`] and power / clocks into `detail` under the
//! `dla_*` keys from [`detail_keys`]. That also keeps the NPU out of the
//! device count and out of the GPU mean utilization.
//!
//! Everything that decides something is a pure function over plain values,
//! so it is unit-tested on every platform; the NVML calls are thin wrappers.

use std::os::raw::c_uint;

use nvml_wrapper::enum_wrappers::device::{Clock, ClockId};
use nvml_wrapper::enums::device::SampleValue;
use nvml_wrapper::error::{NvmlError, nvml_try};
use nvml_wrapper::struct_wrappers::device::FieldValueSample;
use nvml_wrapper::structs::device::FieldId;

use crate::device::readers::detail_keys;
use crate::device::types::GpuInfo;

/// Raw `nvmlBrandType_t` the N1X driver (616.62) returns for the NPU3
/// handle, which `nvidia-smi -q` prints as `Product Brand : NPU`.
/// nvml-wrapper 0.13 has no variant for it, so `Device::brand()` fails with
/// `UnexpectedVariant(19)` and the value is read through the raw symbol.
pub(crate) const NVML_BRAND_NPU_RAW: u32 = 19;

/// `NVML_FI_DEV_POWER_INSTANT`. The NPU's `nvmlDeviceGetPowerUsage` reads
/// the averaged figure, which stays at 0 W while the instantaneous field
/// reports the real draw (`0.88 W` idle on N1X).
const NVML_FI_DEV_POWER_INSTANT: u32 = 186;

/// What an NVML handle is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NvmlDeviceKind {
    /// A GPU that gets its own device row.
    Gpu,
    /// An integrated deep-learning accelerator (N1X NPU3) that is folded
    /// into its SoC GPU's row.
    Dla,
}

/// Classify an NVML handle from its name, UUID and raw brand value.
///
/// Any one signal is enough: the `DLA-` UUID prefix is the most specific,
/// the brand value covers a future driver that renames the device, and the
/// name covers a driver whose brand query fails.
pub(crate) fn classify_nvml_device(
    name: &str,
    uuid: Option<&str>,
    raw_brand: Option<u32>,
) -> NvmlDeviceKind {
    let dla_uuid = uuid.is_some_and(|u| {
        u.get(..4)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("DLA-"))
    });
    let npu_brand = raw_brand == Some(NVML_BRAND_NPU_RAW);
    let npu_name = name.trim().eq_ignore_ascii_case("NVIDIA NPU");
    if dla_uuid || npu_brand || npu_name {
        NvmlDeviceKind::Dla
    } else {
        NvmlDeviceKind::Gpu
    }
}

/// Return `true` when the device name identifies an NVIDIA SoC GPU whose
/// memory is part of the system package: DGX Spark GB10 on Linux and RTX
/// Spark N1X on Windows on ARM64.
pub(crate) fn is_soc_device_name(name: &str) -> bool {
    let name_lower = name.to_lowercase();
    ["gb10", "dgx spark", "rtx spark", "n1x"]
        .iter()
        .any(|needle| name_lower.contains(needle))
}

/// How a GPU's memory and interconnect should be reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MemoryClass {
    /// A discrete card: NVML memory and PCIe link metrics as reported.
    Discrete,
    /// A SoC GPU with no framebuffer of its own (GB10): memory is read
    /// from the host, and the PCIe stub is suppressed.
    UnifiedSystem,
    /// A SoC GPU with a framebuffer carve-out of the package memory (N1X):
    /// NVML's memory figures are kept, the PCIe stub is suppressed.
    UnifiedCarveOut,
}

impl MemoryClass {
    /// Whether the device sits on the SoC fabric rather than on PCIe.
    pub(crate) fn is_soc(self) -> bool {
        !matches!(self, MemoryClass::Discrete)
    }
}

/// Decide a GPU's [`MemoryClass`].
///
/// `c2c_soc_hint` is "NVLink-C2C is enabled and this is a Windows host".
/// C2C alone is not enough: Grace Hopper enables it too and owns real HBM
/// on a real PCIe link, but no C2C part with its own HBM runs Windows.
pub(crate) fn classify_memory(
    memory_total: u64,
    is_blackwell: bool,
    name: &str,
    c2c_soc_hint: bool,
) -> MemoryClass {
    let soc_name = is_soc_device_name(name);
    if memory_total == 0 && (is_blackwell || soc_name) {
        MemoryClass::UnifiedSystem
    } else if memory_total > 0 && (soc_name || c2c_soc_hint) {
        MemoryClass::UnifiedCarveOut
    } else {
        MemoryClass::Discrete
    }
}

/// Drop the `0` sentinel some drivers return for an unsupported
/// temperature threshold. N1X reports `GPU Max Operating T.Limit Temp
/// Specification : 0 C`; caching `Some(0)` would make every reading look
/// past the limit.
pub(crate) fn valid_threshold(value: Option<u32>) -> Option<u32> {
    value.filter(|&t| t > 0)
}

/// The NVML `nvmlBrandType_t` value as returned by the driver, including
/// values nvml-wrapper's enum does not know.
pub(crate) fn raw_brand(device: &nvml_wrapper::Device) -> Option<u32> {
    let sym = device.nvml().lib().nvmlDeviceGetBrand.as_ref().ok()?;
    // SAFETY: `device.handle()` is the live handle NVML owns for this
    // `Device`, and `brand` is a valid out-pointer of the exact C type the
    // symbol writes. The return code is checked before the value is used.
    unsafe {
        let mut brand: c_uint = 0;
        nvml_try(sym(device.handle(), &mut brand)).ok()?;
        Some(brand)
    }
}

/// Whether NVML reports NVLink-C2C as enabled for this GPU.
/// `nvmlDeviceGetC2cModeInfoV` is not wrapped by nvml-wrapper 0.13.
pub(crate) fn c2c_enabled(device: &nvml_wrapper::Device) -> bool {
    let Ok(sym) = device.nvml().lib().nvmlDeviceGetC2cModeInfoV.as_ref() else {
        return false;
    };
    // SAFETY: as in `raw_brand`; the out-struct is zero-initialised plain
    // data of the type the symbol expects, and only read after NVML reports
    // success.
    unsafe {
        // The struct type is inferred from the symbol's signature, because
        // nvml-wrapper does not re-export its sys bindings by name.
        let mut info = std::mem::zeroed();
        let ok = nvml_try(sym(device.handle(), &mut info)).is_ok();
        ok && info.isC2cEnabled != 0
    }
}

/// One poll's readings from a DLA handle.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct DlaReading {
    /// Engine utilization, 0..=100.
    pub utilization: Option<f64>,
    /// Instantaneous power in watts.
    pub power_watts: Option<f64>,
    /// Engine clock in MHz. NVML exposes no DLA engine clock on N1X
    /// (driver 616.62 answers `NotSupported` for graphics, SM and video);
    /// on Windows the D3DKMT node clock fills it in (issue #461).
    pub clock_mhz: Option<u32>,
    /// Maximum engine clock in MHz, from the same D3DKMT node data.
    pub max_clock_mhz: Option<u32>,
    /// Memory clock in MHz, the one clock NVML answers for the NPU.
    pub memory_clock_mhz: Option<u32>,
    /// Die temperature in Celsius.
    pub temperature: Option<u32>,
}

impl DlaReading {
    /// Combine two DLA handles paired with the same GPU: mean utilization,
    /// summed power, highest clocks and temperature.
    pub(crate) fn merge(self, other: DlaReading) -> DlaReading {
        fn either<T>(a: Option<T>, b: Option<T>, f: impl FnOnce(T, T) -> T) -> Option<T> {
            match (a, b) {
                (Some(a), Some(b)) => Some(f(a, b)),
                (a, b) => a.or(b),
            }
        }
        DlaReading {
            utilization: either(self.utilization, other.utilization, |a, b| (a + b) / 2.0),
            power_watts: either(self.power_watts, other.power_watts, |a, b| a + b),
            clock_mhz: either(self.clock_mhz, other.clock_mhz, u32::max),
            max_clock_mhz: either(self.max_clock_mhz, other.max_clock_mhz, u32::max),
            memory_clock_mhz: either(self.memory_clock_mhz, other.memory_clock_mhz, u32::max),
            temperature: either(self.temperature, other.temperature, u32::max),
        }
    }
}

/// NVML engine clock types tried for a DLA, in preference order.
const DLA_ENGINE_CLOCKS: [Clock; 3] = [Clock::Graphics, Clock::SM, Clock::Video];

/// Drop the clock types whose probe answered `NotSupported`. Any other
/// outcome, a value or a transient error, keeps the type in the per-poll
/// set, so only a definite "this handle has no such clock" is cached.
pub(crate) fn retain_supported_clocks(
    probes: impl IntoIterator<Item = (Clock, Result<u32, NvmlError>)>,
) -> Vec<Clock> {
    probes
        .into_iter()
        .filter(|(_, result)| !matches!(result, Err(NvmlError::NotSupported)))
        .map(|(clock, _)| clock)
        .collect()
}

/// Probe a DLA handle once for the engine clock types NVML answers, so
/// [`read_dla`] does not repeat calls the driver always refuses. Empty on
/// N1X with driver 616.62.
pub(crate) fn dla_engine_clocks(device: &nvml_wrapper::Device) -> Vec<Clock> {
    retain_supported_clocks(
        DLA_ENGINE_CLOCKS
            .into_iter()
            .map(|clock| (clock, device.clock(clock, ClockId::Current))),
    )
}

/// Read a DLA handle. Every query is best-effort. `engine_clocks` is the
/// set [`dla_engine_clocks`] found supported.
pub(crate) fn read_dla(device: &nvml_wrapper::Device, engine_clocks: &[Clock]) -> DlaReading {
    use nvml_wrapper::enum_wrappers::device::TemperatureSensor;

    let clock_mhz = engine_clocks.iter().copied().find_map(|clock| {
        device
            .clock(clock, ClockId::Current)
            .ok()
            .filter(|&mhz| mhz > 0)
    });
    let memory_clock_mhz = device
        .clock(Clock::Memory, ClockId::Current)
        .ok()
        .filter(|&mhz| mhz > 0);

    let instant_mw = device
        .field_values_for(&[FieldId(NVML_FI_DEV_POWER_INSTANT)])
        .ok()
        .and_then(|samples| samples.into_iter().next())
        .and_then(|sample| sample.ok())
        .and_then(|sample| sample_milliwatts(&sample));
    let power_watts = instant_mw
        .or_else(|| device.power_usage().ok().map(f64::from))
        .map(|mw| mw / 1000.0);

    DlaReading {
        utilization: device.utilization_rates().ok().map(|u| f64::from(u.gpu)),
        power_watts,
        clock_mhz,
        max_clock_mhz: None,
        memory_clock_mhz,
        temperature: device
            .temperature(TemperatureSensor::Gpu)
            .ok()
            .filter(|&t| t > 0),
    }
}

fn sample_milliwatts(sample: &FieldValueSample) -> Option<f64> {
    match sample.value.as_ref().ok()? {
        SampleValue::U32(v) => Some(f64::from(*v)),
        SampleValue::U64(v) => Some(*v as f64),
        SampleValue::I64(v) if *v >= 0 => Some(*v as f64),
        SampleValue::F64(v) if v.is_finite() && *v >= 0.0 => Some(*v),
        _ => None,
    }
}

/// Pick the GPU row a DLA reading belongs to.
///
/// `gpus` lists `(row index, is SoC GPU)` for every GPU row the reader
/// produced. The accelerator is on the same die as the SoC GPU, so that row
/// wins. A lone GPU row is the only possible host even when it was not
/// recognised as a SoC. Several non-SoC GPUs give no basis for a choice,
/// and the reading is dropped rather than attributed to the wrong card.
pub(crate) fn pair_dla_with_gpu(gpus: &[(usize, bool)]) -> Option<usize> {
    if let Some((row, _)) = gpus.iter().find(|(_, is_soc)| *is_soc) {
        return Some(*row);
    }
    match gpus {
        [(row, _)] => Some(*row),
        _ => None,
    }
}

/// Fold a DLA reading into its host GPU row.
pub(crate) fn apply_dla_reading(gpu: &mut GpuInfo, reading: &DlaReading, dla_uuid: Option<&str>) {
    gpu.dla_utilization = reading.utilization;
    if let Some(watts) = reading.power_watts {
        gpu.detail.insert(
            detail_keys::DLA_POWER_WATTS_DETAIL_KEY.to_string(),
            format!("{watts:.2}"),
        );
    }
    if let Some(mhz) = reading.clock_mhz {
        gpu.detail.insert(
            detail_keys::DLA_CLOCK_DETAIL_KEY.to_string(),
            mhz.to_string(),
        );
    }
    if let Some(mhz) = reading.max_clock_mhz {
        gpu.detail.insert(
            detail_keys::DLA_MAX_CLOCK_DETAIL_KEY.to_string(),
            mhz.to_string(),
        );
    }
    if let Some(mhz) = reading.memory_clock_mhz {
        gpu.detail.insert(
            detail_keys::DLA_CLOCK_MEMORY_DETAIL_KEY.to_string(),
            mhz.to_string(),
        );
    }
    if let Some(celsius) = reading.temperature {
        gpu.detail.insert(
            detail_keys::DLA_TEMPERATURE_DETAIL_KEY.to_string(),
            celsius.to_string(),
        );
    }
    if let Some(uuid) = dla_uuid {
        gpu.detail.insert(
            detail_keys::DLA_UUID_DETAIL_KEY.to_string(),
            uuid.to_string(),
        );
    }
}

/// System memory `(total_bytes, used_bytes)` for a GPU backed by it.
#[cfg(target_os = "windows")]
pub(crate) fn system_memory_for_uma() -> (u64, u64) {
    use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};

    let mut status = MEMORYSTATUSEX {
        dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    // SAFETY: `status` is a properly sized MEMORYSTATUSEX with `dwLength`
    // set as the API requires; it is only read after the call succeeds.
    if unsafe { GlobalMemoryStatusEx(&mut status) }.is_err() {
        return (0, 0);
    }
    memory_status_to_total_used(status.ullTotalPhys, status.ullAvailPhys)
}

/// System memory `(total_bytes, used_bytes)` for a GPU backed by it.
#[cfg(not(target_os = "windows"))]
pub(crate) fn system_memory_for_uma() -> (u64, u64) {
    super::nvidia::read_meminfo_memory("/proc/meminfo")
}

/// `(total, total - available)`, saturating.
#[cfg_attr(not(any(target_os = "windows", test)), allow(dead_code))]
pub(crate) fn memory_status_to_total_used(total: u64, available: u64) -> (u64, u64) {
    (total, total.saturating_sub(available))
}

#[cfg(test)]
#[path = "nvidia_soc_tests.rs"]
mod tests;
