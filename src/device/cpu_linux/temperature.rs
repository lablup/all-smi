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

//! CPU temperature sensor selection for Linux (issues #492, #495, #497).
//!
//! The sensor is chosen by override, driver name, thermal zone type,
//! motherboard sensor label, or hwmon sensor type instead of a fixed sysfs
//! index. It is resolved once per process, and afterwards only the single
//! resolved file is read per sample.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::utils::millicelsius_to_celsius;

#[path = "temp_board.rs"]
mod temp_board;
#[path = "temp_override.rs"]
mod temp_override;
#[cfg_attr(not(feature = "cli"), allow(unused_imports))]
pub(crate) use temp_board::ASUS_WMI_SENSORS_DRIVER;
use temp_board::{pick_from_board_labels, pick_from_sensor_types};
#[cfg(test)]
use temp_override::map_override_path;
use temp_override::{CPU_TEMP_SENSOR_ENV, resolve_override};

/// hwmon `name` values that identify a CPU temperature driver, in priority order.
const CPU_HWMON_DRIVERS: &[&str] = &[
    "coretemp",
    "k10temp",
    "zenpower",
    "cpu_thermal",
    "cpu-thermal",
];

/// thermal zone `type` values that identify a CPU zone (exact, case-sensitive).
///
/// Zones are tried in tiers, and in zone index order within a tier: these
/// types first, then [`CPU_THERMAL_ZONE_SECONDARY_TYPES`], then the per-cluster
/// [`CPU_THERMAL_ZONE_FALLBACK_TYPES`]. Keeping the types added after #496 in a
/// lower tier means a host that resolved before still resolves to the same zone.
const CPU_THERMAL_ZONE_TYPES: &[&str] =
    &["x86_pkg_temp", "cpu-thermal", "cpu_thermal", "CPU-therm"];

/// ARM SoC zone types, tried only when no zone of [`CPU_THERMAL_ZONE_TYPES`] reads.
const CPU_THERMAL_ZONE_SECONDARY_TYPES: &[&str] = &[
    "package-thermal", // Rockchip RK3588 mainline
    "cpu0-thermal",    // Exynos5422 and others
    "soc-thermal",     // Rockchip/Amlogic vendor kernels
    "soc_thermal",     // Rockchip/Amlogic vendor kernels
    "BCPU-therm",      // NVIDIA DRIVE OS
];

/// Per-cluster thermal zone `type` values, tried only when no zone of
/// [`CPU_THERMAL_ZONE_TYPES`] or [`CPU_THERMAL_ZONE_SECONDARY_TYPES`] reads.
const CPU_THERMAL_ZONE_FALLBACK_TYPES: &[&str] = &[
    // RK3588 without `package-thermal`: the first big-core cluster stands in.
    "bigcore0-thermal",
];

/// Legacy fixed paths relative to the sysfs root, tried in order as a last resort.
const FALLBACK_PATHS: &[&str] = &[
    "class/thermal/thermal_zone0/temp",
    "class/thermal/thermal_zone1/temp",
    "class/hwmon/hwmon0/temp1_input",
    "class/hwmon/hwmon1/temp1_input",
];

/// Which selection rule produced the CPU temperature file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(feature = "cli"), allow(dead_code))]
pub(crate) enum TempRule {
    Override,
    Driver,
    ThermalZone,
    BoardLabel,
    SensorType,
    Legacy,
    None,
}

#[cfg_attr(not(feature = "cli"), allow(dead_code))]
impl TempRule {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            TempRule::Override => "override",
            TempRule::Driver => "driver",
            TempRule::ThermalZone => "thermal-zone",
            TempRule::BoardLabel => "board-label",
            TempRule::SensorType => "sensor-type",
            TempRule::Legacy => "legacy",
            TempRule::None => "none",
        }
    }
}

/// How the CPU temperature source was chosen, cached for the process lifetime.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(not(feature = "cli"), allow(dead_code))]
pub(crate) struct CpuTempResolution {
    pub(crate) rule: TempRule,
    /// hwmon `name`, or the thermal zone `type` for a thermal zone file.
    pub(crate) driver: Option<String>,
    /// hwmon `temp*_label` of the chosen input, when it has one.
    pub(crate) label: Option<String>,
    pub(crate) path: Option<PathBuf>,
    /// Set when an override was given but not used, with the reason.
    pub(crate) override_rejection: Option<String>,
}

/// Where the override value comes from.
enum OverrideInput {
    /// Production: read `ALL_SMI_CPU_TEMP_SENSOR` once, inside the resolution.
    Environment,
    /// Tests: a fixed value, so the process environment is never mutated.
    #[cfg(test)]
    Fixed(Option<String>),
}

/// Process-wide cache of the resolved CPU temperature file.
pub(crate) struct CpuTempSource {
    root: PathBuf,
    override_input: OverrideInput,
    resolved: OnceLock<CpuTempResolution>,
    scans: AtomicUsize,
}

/// Production instance rooted at `/sys`.
pub(crate) static CPU_TEMP_SOURCE: CpuTempSource = CpuTempSource::new(PathBuf::new());

impl CpuTempSource {
    pub(super) const fn new(root: PathBuf) -> Self {
        Self {
            root,
            override_input: OverrideInput::Environment,
            resolved: OnceLock::new(),
            scans: AtomicUsize::new(0),
        }
    }

    /// A source with a fixed override value instead of the environment.
    #[cfg(test)]
    pub(super) fn with_override(root: PathBuf, value: Option<&str>) -> Self {
        Self {
            override_input: OverrideInput::Fixed(value.map(str::to_string)),
            ..Self::new(root)
        }
    }

    fn sys_root(&self) -> &Path {
        if self.root.as_os_str().is_empty() {
            Path::new("/sys")
        } else {
            &self.root
        }
    }

    /// Number of sysfs scans performed so far (at most one).
    #[cfg(test)]
    pub(super) fn scan_count(&self) -> usize {
        self.scans.load(Ordering::SeqCst)
    }

    /// The cached resolution, resolving on the first call.
    pub(crate) fn resolution(&self) -> &CpuTempResolution {
        self.resolved.get_or_init(|| {
            self.scans.fetch_add(1, Ordering::SeqCst);
            let value = match &self.override_input {
                OverrideInput::Environment => {
                    std::env::var_os(CPU_TEMP_SENSOR_ENV).map(|v| v.to_string_lossy().into_owned())
                }
                #[cfg(test)]
                OverrideInput::Fixed(v) => v.clone(),
            };
            resolve_cpu_temp(self.sys_root(), value.as_deref())
        })
    }

    /// Resolve lazily on first call, then read only the cached file.
    pub(crate) fn read_celsius(&self) -> Option<u32> {
        let path = self.resolution().path.as_deref()?;
        read_millicelsius(path).map(millicelsius_to_celsius)
    }
}

/// Upper bound for the small text files read during selection. Real sysfs
/// values (`name`, `type`, `temp*_label`, `temp*_input`) are far shorter, so a
/// non-sysfs file reached through an override or a symlink cannot force a
/// large read.
const MAX_SYSFS_READ_BYTES: u64 = 64;

/// Read at most [`MAX_SYSFS_READ_BYTES`] bytes of `path` as UTF-8 text.
fn read_small(path: &Path) -> Option<String> {
    let mut buf = Vec::new();
    fs::File::open(path)
        .ok()?
        .take(MAX_SYSFS_READ_BYTES)
        .read_to_end(&mut buf)
        .ok()?;
    String::from_utf8(buf).ok()
}

fn read_millicelsius(path: &Path) -> Option<u32> {
    read_small(path)?.trim().parse::<u32>().ok()
}

fn read_trimmed(path: &Path) -> Option<String> {
    read_small(path).map(|s| s.trim().to_string())
}

/// List `<dir>/<prefix>N` entries sorted numerically by N.
fn numbered_entries(dir: &Path, prefix: &str) -> Vec<PathBuf> {
    let Ok(read_dir) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut entries: Vec<(u32, PathBuf)> = read_dir
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let index = name.strip_prefix(prefix)?.parse::<u32>().ok()?;
            Some((index, e.path()))
        })
        .collect();
    entries.sort_by_key(|(i, _)| *i);
    entries.into_iter().map(|(_, p)| p).collect()
}

/// `(N, tempN_input path)` for every temp input in a hwmon dir, sorted by N.
///
/// Only directory entries are listed; no input is read here. On large Xeons each
/// coretemp input read is an MSR read on that core, so reads are left to the
/// callers and happen only for the candidate under consideration.
fn hwmon_inputs(dir: &Path) -> Vec<(u32, PathBuf)> {
    let Ok(read_dir) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut inputs: Vec<(u32, PathBuf)> = read_dir
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let n = name
                .strip_prefix("temp")?
                .strip_suffix("_input")?
                .parse::<u32>()
                .ok()?;
            Some((n, e.path()))
        })
        .collect();
    inputs.sort_by_key(|(n, _)| *n);
    inputs
}

/// `tempN_label` of an input (a cheap text file, not a sensor read).
fn hwmon_label(dir: &Path, n: u32) -> Option<String> {
    read_trimmed(&dir.join(format!("temp{n}_label")))
}

fn hwmon_instances(root: &Path, driver: &str) -> Vec<PathBuf> {
    numbered_entries(&root.join("class/hwmon"), "hwmon")
        .into_iter()
        .filter(|dir| read_trimmed(&dir.join("name")).as_deref() == Some(driver))
        .collect()
}

/// First input of `inputs` (ascending N) whose label equals `want` and that reads.
fn first_readable_with_label(dir: &Path, inputs: &[(u32, PathBuf)], want: &str) -> Option<PathBuf> {
    inputs
        .iter()
        .filter(|(n, _)| hwmon_label(dir, *n).as_deref() == Some(want))
        .find(|(_, p)| read_millicelsius(p).is_some())
        .map(|(_, p)| p.clone())
}

/// coretemp: lowest readable package id across instances, else lowest readable input.
fn pick_coretemp(instances: &[PathBuf]) -> Option<PathBuf> {
    let per_instance: Vec<Vec<(u32, PathBuf)>> =
        instances.iter().map(|d| hwmon_inputs(d)).collect();

    // Labels first: only package candidates are read, in ascending id order.
    let mut packages: Vec<(u32, &PathBuf)> = Vec::new();
    for (dir, inputs) in instances.iter().zip(&per_instance) {
        for (n, path) in inputs {
            let id = hwmon_label(dir, *n)
                .as_deref()
                .and_then(|l| l.strip_prefix("Package id"))
                .and_then(|rest| rest.trim().parse::<u32>().ok());
            if let Some(id) = id {
                packages.push((id, path));
            }
        }
    }
    packages.sort_by_key(|(id, _)| *id); // stable: ties keep discovery order
    if let Some((_, p)) = packages
        .into_iter()
        .find(|(_, p)| read_millicelsius(p).is_some())
    {
        return Some(p.clone());
    }

    per_instance
        .iter()
        .flatten()
        .find(|(_, p)| read_millicelsius(p).is_some())
        .map(|(_, p)| p.clone())
}

fn pick_from_hwmon(root: &Path) -> Option<PathBuf> {
    for driver in CPU_HWMON_DRIVERS {
        let instances = hwmon_instances(root, driver);
        if instances.is_empty() {
            continue;
        }
        let picked = match *driver {
            "coretemp" => pick_coretemp(&instances),
            "k10temp" | "zenpower" => instances.iter().find_map(|dir| {
                let inputs = hwmon_inputs(dir);
                first_readable_with_label(dir, &inputs, "Tdie")
                    .or_else(|| first_readable_with_label(dir, &inputs, "Tctl"))
                    .or_else(|| {
                        inputs
                            .iter()
                            .find(|(n, p)| *n == 1 && read_millicelsius(p).is_some())
                            .map(|(_, p)| p.clone())
                    })
            }),
            _ => instances.iter().find_map(|dir| {
                hwmon_inputs(dir)
                    .into_iter()
                    .find(|(_, p)| read_millicelsius(p).is_some())
                    .map(|(_, p)| p)
            }),
        };
        if picked.is_some() {
            return picked;
        }
    }
    None
}

fn pick_from_thermal_zones(root: &Path) -> Option<PathBuf> {
    let mut zones: Vec<(u8, PathBuf)> =
        numbered_entries(&root.join("class/thermal"), "thermal_zone")
            .into_iter()
            .filter_map(|dir| {
                let ty = read_trimmed(&dir.join("type"))?;
                let tier = [
                    CPU_THERMAL_ZONE_TYPES,
                    CPU_THERMAL_ZONE_SECONDARY_TYPES,
                    CPU_THERMAL_ZONE_FALLBACK_TYPES,
                ]
                .iter()
                .position(|types| types.contains(&ty.as_str()))?;
                Some((tier as u8, dir.join("temp")))
            })
            .collect();
    zones.sort_by_key(|(tier, _)| *tier); // stable: keeps zone index order within a tier
    zones
        .into_iter()
        .map(|(_, p)| p)
        .find(|p| read_millicelsius(p).is_some())
}

fn pick_from_legacy(root: &Path) -> Option<PathBuf> {
    FALLBACK_PATHS
        .iter()
        .map(|rel| root.join(rel))
        .find(|p| read_millicelsius(p).is_some())
}

/// Driver and label of a chosen file, from its cheap sibling text files.
fn describe(path: &Path) -> (Option<String>, Option<String>) {
    let Some(dir) = path.parent() else {
        return (None, None);
    };
    let file = path.file_name().and_then(|f| f.to_str()).unwrap_or("");
    if file == "temp" {
        return (read_trimmed(&dir.join("type")), None);
    }
    let n = file
        .strip_prefix("temp")
        .and_then(|rest| rest.strip_suffix("_input"))
        .and_then(|digits| digits.parse::<u32>().ok());
    let label = n.and_then(|n| hwmon_label(dir, n));
    (read_trimmed(&dir.join("name")), label)
}

/// Resolve the CPU temperature source under `root` (production: `/sys`).
///
/// Order, first readable candidate wins: override, named hwmon driver, named
/// thermal zone type, motherboard sensor label, hwmon sensor type (PECI or AMD
/// TSI), `asus_wmi_sensors` board label, legacy fixed paths. An empty or
/// whitespace-only override counts as unset.
pub(super) fn resolve_cpu_temp(root: &Path, override_value: Option<&str>) -> CpuTempResolution {
    let mut override_rejection = None;
    let mut picked = None;
    if let Some(value) = override_value.map(str::trim).filter(|v| !v.is_empty()) {
        match resolve_override(root, value) {
            Ok(path) => picked = Some((TempRule::Override, path)),
            Err(reason) => {
                override_rejection =
                    Some(format!("{CPU_TEMP_SENSOR_ENV}={value} ignored: {reason}"))
            }
        }
    }
    let picked = picked
        .or_else(|| pick_from_hwmon(root).map(|p| (TempRule::Driver, p)))
        .or_else(|| pick_from_thermal_zones(root).map(|p| (TempRule::ThermalZone, p)))
        .or_else(|| pick_from_board_labels(root, false).map(|p| (TempRule::BoardLabel, p)))
        .or_else(|| pick_from_sensor_types(root).map(|p| (TempRule::SensorType, p)))
        .or_else(|| pick_from_board_labels(root, true).map(|p| (TempRule::BoardLabel, p)))
        .or_else(|| pick_from_legacy(root).map(|p| (TempRule::Legacy, p)));
    match picked {
        Some((rule, path)) => {
            let (driver, label) = describe(&path);
            CpuTempResolution {
                rule,
                driver,
                label,
                path: Some(path),
                override_rejection,
            }
        }
        None => CpuTempResolution {
            rule: TempRule::None,
            driver: None,
            label: None,
            path: None,
            override_rejection,
        },
    }
}

#[cfg(test)]
#[path = "temperature_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "temp_selection_tests.rs"]
mod selection_tests;

#[cfg(test)]
#[path = "temp_coverage_tests.rs"]
mod coverage_tests;
