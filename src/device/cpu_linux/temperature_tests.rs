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
use crate::device::cpu_linux::LinuxCpuReader;
use std::fs;
use tempfile::TempDir;

pub(super) fn write(root: &Path, rel: &str, content: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

pub(super) fn hwmon(root: &Path, idx: u32, name: &str, temps: &[(u32, Option<&str>, u32)]) {
    write(root, &format!("class/hwmon/hwmon{idx}/name"), name);
    for (n, label, value) in temps {
        write(
            root,
            &format!("class/hwmon/hwmon{idx}/temp{n}_input"),
            &value.to_string(),
        );
        if let Some(l) = label {
            write(root, &format!("class/hwmon/hwmon{idx}/temp{n}_label"), l);
        }
    }
}

pub(super) fn zone(root: &Path, idx: u32, ty: &str, value: u32) {
    write(root, &format!("class/thermal/thermal_zone{idx}/type"), ty);
    write(
        root,
        &format!("class/thermal/thermal_zone{idx}/temp"),
        &value.to_string(),
    );
}

fn read(root: &Path) -> Option<u32> {
    let source = CpuTempSource::new(root.to_path_buf());
    source.read_celsius()
}

#[test]
fn coretemp_package_at_high_index_beats_junk() {
    let dir = TempDir::new().unwrap();
    let r = dir.path();
    zone(r, 0, "acpitz", 16000);
    hwmon(r, 0, "acpitz", &[(1, None, 16000)]);
    hwmon(
        r,
        12,
        "coretemp",
        &[(1, Some("Package id 0"), 55000), (2, Some("Core 0"), 50000)],
    );
    assert_eq!(read(r), Some(55));
}

#[test]
fn coretemp_picks_lowest_package_id_across_sockets() {
    let dir = TempDir::new().unwrap();
    let r = dir.path();
    hwmon(r, 2, "coretemp", &[(1, Some("Package id 1"), 70000)]);
    hwmon(r, 3, "coretemp", &[(1, Some("Package id 0"), 60000)]);
    assert_eq!(read(r), Some(60));
}

#[test]
fn coretemp_without_package_label_uses_lowest_input() {
    let dir = TempDir::new().unwrap();
    let r = dir.path();
    hwmon(
        r,
        1,
        "coretemp",
        &[(3, Some("Core 2"), 41000), (2, Some("Core 1"), 42000)],
    );
    assert_eq!(read(r), Some(42));
}

#[test]
fn k10temp_prefers_tdie_then_tctl_then_temp1() {
    let dir = TempDir::new().unwrap();
    let r = dir.path();
    zone(r, 0, "acpitz", 16000);
    hwmon(
        r,
        5,
        "k10temp",
        &[(1, Some("Tctl"), 80000), (2, Some("Tdie"), 60000)],
    );
    assert_eq!(read(r), Some(60));

    let dir = TempDir::new().unwrap();
    let r = dir.path();
    hwmon(
        r,
        5,
        "k10temp",
        &[(1, Some("Tctl"), 70000), (3, Some("Tccd1"), 50000)],
    );
    assert_eq!(read(r), Some(70));

    let dir = TempDir::new().unwrap();
    let r = dir.path();
    hwmon(r, 5, "k10temp", &[(1, None, 65000)]);
    assert_eq!(read(r), Some(65));
}

#[test]
fn zenpower_is_recognised() {
    let dir = TempDir::new().unwrap();
    let r = dir.path();
    hwmon(
        r,
        4,
        "zenpower",
        &[(1, Some("Tdie"), 48000), (2, Some("Tctl"), 70000)],
    );
    assert_eq!(read(r), Some(48));
}

#[test]
fn hwmon_indices_sort_numerically() {
    let dir = TempDir::new().unwrap();
    let r = dir.path();
    hwmon(r, 10, "k10temp", &[(1, Some("Tdie"), 10000)]);
    hwmon(r, 2, "k10temp", &[(1, Some("Tdie"), 20000)]);
    assert_eq!(read(r), Some(20));
}

#[test]
fn stuck_thermal_zone0_ignored_when_named_sensor_exists() {
    let dir = TempDir::new().unwrap();
    let r = dir.path();
    zone(r, 0, "acpitz", 16000);
    hwmon(r, 2, "k10temp", &[(1, Some("Tdie"), 52000)]);
    assert_eq!(read(r), Some(52));
}

#[test]
fn thermal_zone_type_match_is_exact() {
    let dir = TempDir::new().unwrap();
    let r = dir.path();
    zone(r, 0, "acpitz", 16000);
    zone(r, 1, "gpu-cpu-bridge", 30000);
    zone(r, 7, "x86_pkg_temp", 44000);
    assert_eq!(read(r), Some(44));
}

#[test]
fn fallback_uses_legacy_paths_in_order() {
    let dir = TempDir::new().unwrap();
    let r = dir.path();
    zone(r, 0, "acpitz", 39000);
    zone(r, 1, "other", 20000);
    hwmon(r, 0, "acpitz", &[(1, None, 10000)]);
    assert_eq!(read(r), Some(39));

    let dir = TempDir::new().unwrap();
    let r = dir.path();
    zone(r, 0, "acpitz", 0);
    write(r, "class/thermal/thermal_zone0/temp", "garbage");
    hwmon(r, 1, "nvme", &[(1, None, 33000)]);
    assert_eq!(read(r), Some(33));
}

#[test]
fn dead_named_sensor_is_skipped() {
    let dir = TempDir::new().unwrap();
    let r = dir.path();
    write(r, "class/hwmon/hwmon2/name", "coretemp");
    write(r, "class/hwmon/hwmon2/temp1_input", "not a number");
    write(r, "class/hwmon/hwmon2/temp1_label", "Package id 0");
    zone(r, 0, "acpitz", 39000);
    assert_eq!(read(r), Some(39));
}

#[test]
fn no_sensor_returns_none() {
    let dir = TempDir::new().unwrap();
    assert_eq!(read(dir.path()), None);
    assert_eq!(read(&dir.path().join("missing")), None);
}

#[test]
fn scan_runs_once_across_reads_and_readers_when_found() {
    let dir = TempDir::new().unwrap();
    let r = dir.path();
    hwmon(r, 2, "coretemp", &[(1, Some("Package id 0"), 55000)]);
    let source: &'static CpuTempSource = Box::leak(Box::new(CpuTempSource::new(r.to_path_buf())));
    assert_eq!(source.scan_count(), 0);
    for _ in 0..3 {
        assert_eq!(source.read_celsius(), Some(55));
    }
    assert_eq!(source.scan_count(), 1);
    // A later value change is read through the same cached file, no rescan.
    write(r, "class/hwmon/hwmon2/temp1_input", "61000");
    assert_eq!(source.read_celsius(), Some(61));
    assert_eq!(source.scan_count(), 1);
    // A broken file yields None for the sample without rescanning, and a better
    // sensor appearing later is not picked up.
    write(r, "class/hwmon/hwmon2/temp1_input", "x");
    hwmon(r, 3, "k10temp", &[(1, Some("Tdie"), 40000)]);
    assert_eq!(source.read_celsius(), None);
    assert_eq!(source.scan_count(), 1);
}

#[test]
fn scan_runs_once_when_no_sensor() {
    let dir = TempDir::new().unwrap();
    let source: &'static CpuTempSource =
        Box::leak(Box::new(CpuTempSource::new(dir.path().to_path_buf())));
    for _ in 0..3 {
        assert_eq!(source.read_celsius(), None);
    }
    // A sensor appearing afterwards is not discovered.
    hwmon(
        dir.path(),
        0,
        "coretemp",
        &[(1, Some("Package id 0"), 55000)],
    );
    assert_eq!(source.read_celsius(), None);
    assert_eq!(source.scan_count(), 1);
}

#[test]
fn scan_runs_once_across_reader_instances_when_found() {
    let dir = TempDir::new().unwrap();
    let r = dir.path();
    zone(r, 0, "acpitz", 16000);
    hwmon(r, 7, "coretemp", &[(1, Some("Package id 0"), 57000)]);
    let source: &'static CpuTempSource = Box::leak(Box::new(CpuTempSource::new(r.to_path_buf())));
    let readers: Vec<LinuxCpuReader> = (0..3)
        .map(|_| LinuxCpuReader::with_temp_source(source))
        .collect();
    // Construction stays cheap: nothing is resolved until the first read.
    assert_eq!(source.scan_count(), 0);
    for reader in &readers {
        for _ in 0..2 {
            assert_eq!(reader.get_cpu_temperature(), Some(57));
        }
    }
    let late = LinuxCpuReader::with_temp_source(source);
    assert_eq!(late.get_cpu_temperature(), Some(57));
    assert_eq!(source.scan_count(), 1);
}

#[test]
fn scan_runs_once_across_reader_instances_when_no_sensor() {
    let dir = TempDir::new().unwrap();
    let source: &'static CpuTempSource =
        Box::leak(Box::new(CpuTempSource::new(dir.path().to_path_buf())));
    let readers: Vec<LinuxCpuReader> = (0..3)
        .map(|_| LinuxCpuReader::with_temp_source(source))
        .collect();
    assert_eq!(source.scan_count(), 0);
    for reader in &readers {
        for _ in 0..2 {
            assert_eq!(reader.get_cpu_temperature(), None);
        }
    }
    let late = LinuxCpuReader::with_temp_source(source);
    assert_eq!(late.get_cpu_temperature(), None);
    assert_eq!(source.scan_count(), 1);
}

#[test]
fn dead_package_falls_through_to_next_package() {
    let dir = TempDir::new().unwrap();
    let r = dir.path();
    hwmon(r, 0, "coretemp", &[(1, Some("Package id 0"), 50000)]);
    write(r, "class/hwmon/hwmon0/temp1_input", "garbage");
    hwmon(r, 1, "coretemp", &[(1, Some("Package id 1"), 61000)]);
    assert_eq!(read(r), Some(61));
}

#[test]
fn dead_only_package_falls_through_to_lowest_core() {
    let dir = TempDir::new().unwrap();
    let r = dir.path();
    hwmon(
        r,
        0,
        "coretemp",
        &[
            (1, Some("Package id 0"), 0),
            (2, Some("Core 0"), 44000),
            (3, Some("Core 1"), 45000),
        ],
    );
    write(r, "class/hwmon/hwmon0/temp1_input", "");
    assert_eq!(read(r), Some(44));
}

#[test]
fn k10temp_dead_tdie_falls_through_to_tctl() {
    let dir = TempDir::new().unwrap();
    let r = dir.path();
    hwmon(
        r,
        0,
        "k10temp",
        &[(1, Some("Tctl"), 70000), (2, Some("Tdie"), 0)],
    );
    write(r, "class/hwmon/hwmon0/temp2_input", "x");
    assert_eq!(read(r), Some(70));
}

#[test]
fn small_reads_are_capped_and_real_values_unchanged() {
    let dir = TempDir::new().unwrap();
    let r = dir.path();
    write(r, "ok", "48000\n");
    assert_eq!(read_millicelsius(&r.join("ok")), Some(48000));
    write(r, "name", "nct6798\n");
    assert_eq!(read_trimmed(&r.join("name")).as_deref(), Some("nct6798"));
    // A large file yields at most the bound, never the whole content.
    write(r, "big", &"a".repeat(1_000_000));
    assert_eq!(
        read_trimmed(&r.join("big")).map(|s| s.len()),
        Some(MAX_SYSFS_READ_BYTES as usize)
    );
    assert_eq!(read_millicelsius(&r.join("big")), None);
}
