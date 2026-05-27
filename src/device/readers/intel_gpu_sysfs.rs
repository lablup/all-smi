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

//! Lower-level sysfs helpers used by the Intel client GPU reader.
//!
//! Split out of [`super::intel_gpu_linux`] so each file stays small and
//! the dynamic counter readers can be unit-tested without pulling the
//! main reader struct into the test harness.

use std::path::Path;

/// Memory variant of the GPU. Mirrors
/// `super::intel_gpu_linux::IntelGpuVariant` but kept private to this
/// module so the public API surface stays in the main reader file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub enum MemoryVariant {
    /// Has dedicated VRAM advertised via `mem_info_vram_total` (i915) or
    /// `tile0/vram0/total_bytes` (xe).
    Discrete,
    /// No dedicated VRAM — Iris Xe / Xe-LPG / Arc iGPU.
    Integrated,
}

/// Read used/total VRAM bytes for the device. Integrated GPUs always
/// return `(0, 0)` because the kernel does not pre-reserve a budget
/// (GTT pages are allocated on demand). Returning a fabricated value
/// derived from system RAM would mis-represent the actual GPU memory
/// situation, so we intentionally surface zero and let the reader add a
/// `detail["Memory"]` note explaining the value.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn read_memory_bytes(device_dir: &Path, variant: MemoryVariant) -> (u64, u64) {
    if variant == MemoryVariant::Integrated {
        return (0, 0);
    }

    // i915 path first (older driver, more common on Arc A-series).
    let mut total = read_u64(&device_dir.join("mem_info_vram_total")).unwrap_or(0);
    let mut used = read_u64(&device_dir.join("mem_info_vram_used")).unwrap_or(0);

    // xe path: `tile0/vram0/total_bytes` and `used_bytes`. We only look
    // at tile0 because consumer Intel discrete GPUs are single-tile;
    // datacenter Flex/Max parts (xe with multiple tiles) are out of
    // scope for the *client* GPU reader.
    if total == 0 {
        total = read_u64(&device_dir.join("tile0").join("vram0").join("total_bytes")).unwrap_or(0);
    }
    if used == 0 {
        used = read_u64(&device_dir.join("tile0").join("vram0").join("used_bytes")).unwrap_or(0);
    }

    (used, total)
}

/// Read the current GT0 frequency in MHz.
///
/// i915 exposes the value directly in MHz under `gt_cur_freq_mhz`.
/// The newer `xe` driver exposes it under `tile0/gt0/freq0/cur_freq`
/// — older xe builds report Hz, newer ones report MHz. Heuristic: any
/// value above 100_000 is interpreted as Hz (the highest Intel client
/// GPU clocks are under 3 GHz, so a true MHz reading can never exceed
/// 100_000).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn read_frequency_mhz(device_dir: &Path) -> u32 {
    if let Some(mhz) = read_u32(&device_dir.join("gt_cur_freq_mhz")) {
        return mhz;
    }
    if let Some(raw) = read_u64(
        &device_dir
            .join("tile0")
            .join("gt0")
            .join("freq0")
            .join("cur_freq"),
    ) {
        let mhz = if raw > 100_000 { raw / 1_000_000 } else { raw };
        if mhz <= u64::from(u32::MAX) {
            return mhz as u32;
        }
    }
    0
}

/// Walk `device/hwmon/hwmon*/temp1_input` (milli-Celsius). Returns the
/// first parseable value divided by 1000. On failure or absence the
/// reader returns `0`; the caller documents that "no thermal data" in
/// `detail` so consumers don't confuse zero with "literally 0 degrees".
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn read_temperature_celsius(device_dir: &Path) -> u32 {
    let hwmon_root = device_dir.join("hwmon");
    let iter = match std::fs::read_dir(&hwmon_root) {
        Ok(i) => i,
        Err(_) => return 0,
    };
    for entry in iter.flatten() {
        if let Some(milli) = read_u64(&entry.path().join("temp1_input")) {
            return (milli / 1000) as u32;
        }
    }
    0
}

/// Walk `device/hwmon/hwmon*/power1_average` (microwatts). Returns the
/// first parseable value divided by 1_000_000 (W). On absence returns
/// `0.0`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn read_power_watts(device_dir: &Path) -> f64 {
    let hwmon_root = device_dir.join("hwmon");
    let iter = match std::fs::read_dir(&hwmon_root) {
        Ok(i) => i,
        Err(_) => return 0.0,
    };
    for entry in iter.flatten() {
        if let Some(uw) = read_u64(&entry.path().join("power1_average")) {
            return uw as f64 / 1_000_000.0;
        }
    }
    0.0
}

/// Walk `device/hwmon/hwmon*/fan1_input` (RPM). Returns the first
/// parseable non-zero value.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn read_fan_rpm(device_dir: &Path) -> Option<u32> {
    let hwmon_root = device_dir.join("hwmon");
    let iter = std::fs::read_dir(&hwmon_root).ok()?;
    for entry in iter.flatten() {
        if let Some(rpm) = read_u32(&entry.path().join("fan1_input"))
            && rpm > 0
        {
            return Some(rpm);
        }
    }
    None
}

/// Read `path` as a decimal u64. Whitespace-trimmed; returns `None` on
/// any I/O or parse failure.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn read_u64(path: &Path) -> Option<u64> {
    std::fs::read_to_string(path)
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
}

/// Read `path` as a decimal u32. Whitespace-trimmed; returns `None` on
/// any I/O or parse failure.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn read_u32(path: &Path) -> Option<u32> {
    std::fs::read_to_string(path)
        .ok()?
        .trim()
        .parse::<u32>()
        .ok()
}

/// Returns `true` when `path` contains a strictly positive u64. Used by
/// the variant classifier — a missing or zero file is not a discrete
/// GPU.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn has_nonzero_u64(path: &Path) -> bool {
    read_u64(path).map(|v| v > 0).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn read_memory_integrated_returns_zero() {
        let dir = tempdir().unwrap();
        let (used, total) = read_memory_bytes(dir.path(), MemoryVariant::Integrated);
        assert_eq!(used, 0);
        assert_eq!(total, 0);
    }

    #[test]
    fn read_memory_discrete_via_i915() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("mem_info_vram_total"), "17179869184\n").unwrap();
        fs::write(dir.path().join("mem_info_vram_used"), "1073741824\n").unwrap();
        let (used, total) = read_memory_bytes(dir.path(), MemoryVariant::Discrete);
        assert_eq!(total, 17_179_869_184);
        assert_eq!(used, 1_073_741_824);
    }

    #[test]
    fn read_memory_discrete_via_xe() {
        let dir = tempdir().unwrap();
        let xe = dir.path().join("tile0").join("vram0");
        fs::create_dir_all(&xe).unwrap();
        fs::write(xe.join("total_bytes"), "12884901888\n").unwrap();
        fs::write(xe.join("used_bytes"), "536870912\n").unwrap();
        let (used, total) = read_memory_bytes(dir.path(), MemoryVariant::Discrete);
        assert_eq!(total, 12_884_901_888);
        assert_eq!(used, 536_870_912);
    }

    #[test]
    fn frequency_prefers_i915_path() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("gt_cur_freq_mhz"), "2100\n").unwrap();
        assert_eq!(read_frequency_mhz(dir.path()), 2100);
    }

    #[test]
    fn frequency_xe_hz_path() {
        let dir = tempdir().unwrap();
        let freq = dir.path().join("tile0").join("gt0").join("freq0");
        fs::create_dir_all(&freq).unwrap();
        fs::write(freq.join("cur_freq"), "2500000000\n").unwrap();
        assert_eq!(read_frequency_mhz(dir.path()), 2500);
    }

    #[test]
    fn frequency_xe_mhz_path() {
        let dir = tempdir().unwrap();
        let freq = dir.path().join("tile0").join("gt0").join("freq0");
        fs::create_dir_all(&freq).unwrap();
        fs::write(freq.join("cur_freq"), "2300\n").unwrap();
        assert_eq!(read_frequency_mhz(dir.path()), 2300);
    }

    #[test]
    fn temperature_handles_milli_celsius() {
        let dir = tempdir().unwrap();
        let hwmon = dir.path().join("hwmon").join("hwmon3");
        fs::create_dir_all(&hwmon).unwrap();
        fs::write(hwmon.join("temp1_input"), "72500\n").unwrap();
        assert_eq!(read_temperature_celsius(dir.path()), 72);
    }

    #[test]
    fn temperature_missing_returns_zero() {
        let dir = tempdir().unwrap();
        assert_eq!(read_temperature_celsius(dir.path()), 0);
    }

    #[test]
    fn power_handles_microwatts() {
        let dir = tempdir().unwrap();
        let hwmon = dir.path().join("hwmon").join("hwmon2");
        fs::create_dir_all(&hwmon).unwrap();
        fs::write(hwmon.join("power1_average"), "185500000\n").unwrap();
        let w = read_power_watts(dir.path());
        assert!((w - 185.5).abs() < 0.01, "got {w}");
    }

    #[test]
    fn fan_rpm_handles_hwmon() {
        let dir = tempdir().unwrap();
        let hwmon = dir.path().join("hwmon").join("hwmon2");
        fs::create_dir_all(&hwmon).unwrap();
        fs::write(hwmon.join("fan1_input"), "1730\n").unwrap();
        assert_eq!(read_fan_rpm(dir.path()), Some(1730));
    }

    #[test]
    fn has_nonzero_u64_works() {
        let dir = tempdir().unwrap();
        let f = dir.path().join("x");
        fs::write(&f, "0\n").unwrap();
        assert!(!has_nonzero_u64(&f));
        fs::write(&f, "42\n").unwrap();
        assert!(has_nonzero_u64(&f));
        // Missing file.
        assert!(!has_nonzero_u64(&dir.path().join("missing")));
    }
}
