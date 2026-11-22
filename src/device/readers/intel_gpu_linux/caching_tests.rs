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

//! Tests for the per-tick and per-card caches of the Linux Intel reader:
//! the shared `/proc` fd scan, the held hwmon directory list, and the
//! energy cache path.

use super::*;
use crate::device::readers::intel_gpu_fdinfo::scans_on_this_thread;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::tempdir;

/// An xe card with a render node whose sysfs reports a VRAM total but no
/// `used_bytes`: the Battlemage shape that sends `used_memory` to the
/// fdinfo fallback. Returns the card's PCI device directory.
fn make_xe_card_needing_fdinfo(drm_root: &Path, idx: u32, render_minor: u32, bus: &str) -> PathBuf {
    let pci_dir = drm_root.join("_pci").join(bus);
    fs::create_dir_all(&pci_dir).unwrap();
    fs::write(pci_dir.join("vendor"), "0x8086\n").unwrap();
    fs::write(pci_dir.join("device"), "0xE20B\n").unwrap();
    let driver_target = drm_root.join("_drivers").join("xe");
    fs::create_dir_all(&driver_target).unwrap();
    std::os::unix::fs::symlink(&driver_target, pci_dir.join("driver")).unwrap();
    let vram = pci_dir.join("tile0").join("vram0");
    fs::create_dir_all(&vram).unwrap();
    fs::write(vram.join("total_bytes"), "12884901888\n").unwrap();

    let card = drm_root.join(format!("card{idx}"));
    fs::create_dir_all(&card).unwrap();
    std::os::unix::fs::symlink(&pci_dir, card.join("device")).unwrap();
    let render = drm_root.join(format!("renderD{render_minor}"));
    fs::create_dir_all(&render).unwrap();
    std::os::unix::fs::symlink(&pci_dir, render.join("device")).unwrap();
    pci_dir
}

fn make_proc_fd(proc_root: &Path, pid: u32, fd: u32, dri_basename: &str, fdinfo: &str) {
    let fd_dir = proc_root.join(pid.to_string()).join("fd");
    let fdinfo_dir = proc_root.join(pid.to_string()).join("fdinfo");
    fs::create_dir_all(&fd_dir).unwrap();
    fs::create_dir_all(&fdinfo_dir).unwrap();
    let target = proc_root.join("_dri").join(dri_basename);
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::write(&target, b"").unwrap();
    std::os::unix::fs::symlink(&target, fd_dir.join(fd.to_string())).unwrap();
    fs::write(fdinfo_dir.join(fd.to_string()), fdinfo).unwrap();
}

#[test]
fn gpu_info_and_process_rows_share_one_proc_walk() {
    let drm = tempdir().unwrap();
    let proc = tempdir().unwrap();
    make_xe_card_needing_fdinfo(drm.path(), 0, 128, "0000:03:00.0");
    make_proc_fd(
        proc.path(),
        std::process::id(),
        3,
        "renderD128",
        "drm-driver: xe\ndrm-client-id: 5\ndrm-resident-vram0: 2048 kB\ndrm-resident-gtt: 1024 kB\n",
    );
    let reader = IntelGpuReader::new_with_roots(drm.path(), proc.path());
    // All three calls land in one window, whatever the test's speed.
    reader.drm_fd_scan.set_test_now(std::time::Instant::now());
    let before = scans_on_this_thread();

    let devices = reader.get_gpu_info();
    let (rows, pids) = reader.get_gpu_processes();
    let processes = reader.get_process_info();

    // The card-level fallback counts VRAM only; the process row counts
    // every resident page. Both come from the same walk.
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].used_memory, 2048 * 1024);
    assert_eq!(
        devices[0].detail.get("Source: Memory").map(String::as_str),
        Some("fdinfo (xe)")
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].used_memory, (2048 + 1024) * 1024);
    assert!(pids.contains(&std::process::id()));
    assert_eq!(processes.len(), 1);
    assert_eq!(processes[0].used_memory, (2048 + 1024) * 1024);
    assert_eq!(scans_on_this_thread() - before, 1);
}

#[test]
fn hwmon_directory_is_listed_once_and_again_after_a_rebind() {
    let drm = tempdir().unwrap();
    let proc = tempdir().unwrap();
    let pci_dir = make_xe_card_needing_fdinfo(drm.path(), 0, 128, "0000:03:00.0");
    let hwmon0 = pci_dir.join("hwmon").join("hwmon0");
    fs::create_dir_all(&hwmon0).unwrap();
    fs::write(hwmon0.join("temp2_input"), "61000\n").unwrap();
    let reader = IntelGpuReader::new_with_roots(drm.path(), proc.path());

    assert_eq!(reader.get_gpu_info()[0].temperature, 61);
    let held = reader.cards[0].hwmon_dirs.lock().unwrap().clone();
    assert_eq!(held.len(), 1);
    assert!(held[0].ends_with("hwmon0"));

    // A directory that appears later is not picked up while the held one
    // still exists: the list is reused, not rebuilt.
    let hwmon9 = pci_dir.join("hwmon").join("hwmon9");
    fs::create_dir_all(&hwmon9).unwrap();
    fs::write(hwmon9.join("fan1_input"), "1500\n").unwrap();
    assert_eq!(reader.get_gpu_info()[0].fan_speed_rpm, None);

    // After a rebind the old index is gone, so the list is rebuilt and the
    // sensors are found under the new one.
    fs::remove_dir_all(&hwmon0).unwrap();
    fs::write(hwmon9.join("temp2_input"), "64000\n").unwrap();
    let info = reader.get_gpu_info();
    assert_eq!(info[0].temperature, 64);
    assert_eq!(info[0].fan_speed_rpm, Some(1500));
}

#[test]
fn energy_cache_path_is_resolved_once_per_card() {
    let drm = tempdir().unwrap();
    let proc = tempdir().unwrap();
    // No `power1_average` and no `energy1_input`: the energy path is
    // resolved, then the delta returns early without writing the cache.
    make_xe_card_needing_fdinfo(drm.path(), 0, 128, "0000:03:00.0");
    let reader = IntelGpuReader::new_with_roots(drm.path(), proc.path());
    assert!(reader.cards[0].energy_cache_path.get().is_none());

    reader.get_gpu_info();
    let first = reader.cards[0].energy_cache_path.get().cloned();
    assert!(first.is_some());

    reader.get_gpu_info();
    assert_eq!(reader.cards[0].energy_cache_path.get().cloned(), first);
    // The path is named after the PCI address the symlink resolves to.
    if let Some(Some(path)) = first {
        assert!(
            path.to_string_lossy()
                .ends_with("energy-hwmon-0000:03:00.0")
        );
    }
}
