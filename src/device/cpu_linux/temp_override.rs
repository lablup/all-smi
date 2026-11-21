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

//! `ALL_SMI_CPU_TEMP_SENSOR` override parsing and lookup (issue #495).
//!
//! Accepted forms:
//! - `<driver>:temp<N>`: input N of the first hwmon instance named `<driver>`
//!   (lowest index first) whose `temp<N>_input` reads.
//! - `<driver>:<label>`: the first readable input whose `temp*_label` equals
//!   `<label>` exactly, on any hwmon instance named `<driver>`.
//! - An absolute path under `/sys` to a `temp*_input` file or a thermal zone
//!   `temp` file. The `/sys` prefix maps onto the source's sysfs root, so tests
//!   can point a fake tree at the same values production uses.

use std::path::{Component, Path, PathBuf};

use super::{hwmon_inputs, hwmon_instances, hwmon_label, read_millicelsius};

/// Environment variable that selects the CPU temperature sensor explicitly.
pub(super) const CPU_TEMP_SENSOR_ENV: &str = "ALL_SMI_CPU_TEMP_SENSOR";

/// Resolve an override value to a readable file under `root`.
///
/// Returns the file on success, or a human-readable rejection reason.
pub(super) fn resolve_override(root: &Path, value: &str) -> Result<PathBuf, String> {
    if value.starts_with('/') {
        return resolve_path_override(root, value);
    }
    let Some((driver, spec)) = value.split_once(':') else {
        return Err(malformed());
    };
    if driver.is_empty() || spec.is_empty() {
        return Err(malformed());
    }
    let instances = hwmon_instances(root, driver);
    if instances.is_empty() {
        return Err(format!("no hwmon device is named `{driver}`"));
    }
    if let Some(n) = parse_temp_index(spec) {
        let file = format!("temp{n}_input");
        return instances
            .iter()
            .map(|dir| dir.join(&file))
            .find(|p| read_millicelsius(p).is_some())
            .ok_or_else(|| format!("no readable {file} on hwmon device `{driver}`"));
    }
    instances
        .iter()
        .find_map(|dir| {
            hwmon_inputs(dir)
                .into_iter()
                .filter(|(n, _)| hwmon_label(dir, *n).as_deref() == Some(spec))
                .find(|(_, p)| read_millicelsius(p).is_some())
                .map(|(_, p)| p)
        })
        .ok_or_else(|| format!("no readable input labelled `{spec}` on hwmon device `{driver}`"))
}

fn malformed() -> String {
    "expected `<driver>:temp<N>`, `<driver>:<label>`, or an absolute path under /sys".to_string()
}

/// `temp<N>` with a decimal N, nothing else.
fn parse_temp_index(spec: &str) -> Option<u32> {
    parse_digits(spec.strip_prefix("temp")?)
}

fn resolve_path_override(root: &Path, value: &str) -> Result<PathBuf, String> {
    let mapped = map_override_path(root, value)?;
    if read_millicelsius(&mapped).is_none() {
        return Err(format!("{value} is not readable as a temperature"));
    }
    Ok(mapped)
}

/// Map an absolute `/sys/...` override onto `root` without touching the
/// filesystem. Symlinks are not resolved: sysfs links such as
/// `/sys/class/hwmon/hwmonN` point into `/sys/devices`, so either spelling of a
/// real sysfs file stays under the root.
pub(super) fn map_override_path(root: &Path, value: &str) -> Result<PathBuf, String> {
    let path = Path::new(value);
    let rel = path
        .strip_prefix("/sys")
        .map_err(|_| "path is outside /sys".to_string())?;
    // `..` could climb out of /sys, and `.` has no use here; accept only plain
    // names so the mapped path always stays under the root.
    if !rel.components().all(|c| matches!(c, Component::Normal(_))) {
        return Err("path must not contain `.` or `..` components".to_string());
    }
    let file = rel.file_name().and_then(|f| f.to_str()).unwrap_or("");
    let parent = rel
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|f| f.to_str())
        .unwrap_or("");
    let is_hwmon_input = file
        .strip_prefix("temp")
        .and_then(|rest| rest.strip_suffix("_input"))
        .and_then(parse_digits)
        .is_some();
    let is_zone_temp = file == "temp" && parent.starts_with("thermal_zone");
    if !is_hwmon_input && !is_zone_temp {
        return Err("path is not a temp*_input or thermal zone temp file".to_string());
    }
    Ok(root.join(rel))
}

fn parse_digits(s: &str) -> Option<u32> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}
