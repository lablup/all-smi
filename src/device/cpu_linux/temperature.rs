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

//! CPU temperature sensor selection for Linux (issue #492).
//!
//! The sensor is chosen by driver name instead of a fixed sysfs index, resolved
//! once per process, and afterwards only the single resolved file is read per
//! sample.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::utils::millicelsius_to_celsius;

/// hwmon `name` values that identify a CPU temperature driver, in priority order.
const CPU_HWMON_DRIVERS: &[&str] = &[
    "coretemp",
    "k10temp",
    "zenpower",
    "cpu_thermal",
    "cpu-thermal",
];

/// thermal zone `type` values that identify a CPU zone (exact, case-sensitive).
const CPU_THERMAL_ZONE_TYPES: &[&str] =
    &["x86_pkg_temp", "cpu-thermal", "cpu_thermal", "CPU-therm"];

/// Legacy fixed paths relative to the sysfs root, tried in order as a last resort.
const FALLBACK_PATHS: &[&str] = &[
    "class/thermal/thermal_zone0/temp",
    "class/thermal/thermal_zone1/temp",
    "class/hwmon/hwmon0/temp1_input",
    "class/hwmon/hwmon1/temp1_input",
];

/// Process-wide cache of the resolved CPU temperature file.
pub(super) struct CpuTempSource {
    root: PathBuf,
    resolved: OnceLock<Option<PathBuf>>,
    scans: AtomicUsize,
}

/// Production instance rooted at `/sys`.
pub(super) static CPU_TEMP_SOURCE: CpuTempSource = CpuTempSource::new(PathBuf::new());

impl CpuTempSource {
    pub(super) const fn new(root: PathBuf) -> Self {
        Self {
            root,
            resolved: OnceLock::new(),
            scans: AtomicUsize::new(0),
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

    /// Resolve lazily on first call, then read only the cached file.
    pub(super) fn read_celsius(&self) -> Option<u32> {
        let path = self.resolved.get_or_init(|| {
            self.scans.fetch_add(1, Ordering::SeqCst);
            resolve_cpu_temp_path(self.sys_root())
        });
        read_millicelsius(path.as_deref()?).map(millicelsius_to_celsius)
    }
}

fn read_millicelsius(path: &Path) -> Option<u32> {
    fs::read_to_string(path).ok()?.trim().parse::<u32>().ok()
}

fn read_trimmed(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_string())
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
    numbered_entries(&root.join("class/thermal"), "thermal_zone")
        .into_iter()
        .filter(|dir| {
            read_trimmed(&dir.join("type"))
                .is_some_and(|t| CPU_THERMAL_ZONE_TYPES.contains(&t.as_str()))
        })
        .map(|dir| dir.join("temp"))
        .find(|p| read_millicelsius(p).is_some())
}

/// Resolve the CPU temperature file under `root` (production: `/sys`).
///
/// Order: named hwmon driver, named thermal zone type, legacy fixed paths.
pub(super) fn resolve_cpu_temp_path(root: &Path) -> Option<PathBuf> {
    pick_from_hwmon(root)
        .or_else(|| pick_from_thermal_zones(root))
        .or_else(|| {
            FALLBACK_PATHS
                .iter()
                .map(|rel| root.join(rel))
                .find(|p| read_millicelsius(p).is_some())
        })
}

#[cfg(test)]
#[path = "temperature_tests.rs"]
mod tests;
