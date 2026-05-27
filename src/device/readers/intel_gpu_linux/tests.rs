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

//! Unit tests for the Intel client GPU reader. Pulled out of
//! `intel_gpu_linux.rs` to keep that file under the 500-line budget.

use super::*;
use crate::device::readers::intel_gpu_sysfs::MemoryVariant;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::tempdir;

/// Build a synthetic `cardN` sysfs node beneath `root`.
///
/// The driver symlink target's *file name* is what
/// [`super::resolve_driver`] returns, so we create a directory named
/// exactly `driver` and point the `device/driver` symlink at it. The
/// real kernel link target looks like
/// `/sys/bus/pci/drivers/i915` — only the basename matters.
fn make_card(root: &Path, idx: u32, vendor: &str, driver: &str, device_id: &str) -> PathBuf {
    let card = root.join(format!("card{idx}"));
    let device = card.join("device");
    fs::create_dir_all(&device).unwrap();
    fs::write(device.join("vendor"), format!("{vendor}\n")).unwrap();
    fs::write(device.join("device"), format!("{device_id}\n")).unwrap();
    let drivers_dir = root.join("_drivers");
    fs::create_dir_all(&drivers_dir).unwrap();
    let driver_target = drivers_dir.join(driver);
    fs::create_dir_all(&driver_target).unwrap();
    std::os::unix::fs::symlink(&driver_target, device.join("driver")).unwrap();
    card
}

#[test]
fn discover_skips_non_card_entries() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    make_card(root, 0, "0x8086", "i915", "0x56A0");
    // A non-card directory that mustn't be picked up.
    fs::create_dir_all(root.join("renderD128").join("device")).unwrap();
    fs::write(
        root.join("renderD128").join("device").join("vendor"),
        "0x8086\n",
    )
    .unwrap();

    let cards = discover_cards(root);
    assert_eq!(cards.len(), 1);
    assert_eq!(cards[0].driver, "i915");
    assert_eq!(cards[0].device_id, 0x56A0);
}

#[test]
fn discover_excludes_habana_vendor() {
    // Habana / Gaudi devices have vendor `0x1da3`, not `0x8086`, and
    // are not driven by i915/xe. They MUST NOT appear in the Intel
    // GPU reader output even though Habana is owned by Intel.
    let dir = tempdir().unwrap();
    let root = dir.path();
    make_card(root, 0, "0x1da3", "habanalabs", "0x1020");

    let cards = discover_cards(root);
    assert!(cards.is_empty());
}

#[test]
fn discover_requires_i915_or_xe_driver() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    // Intel-vendor PCI device without a graphics driver (e.g. a
    // hypothetical Intel-vendor accelerator). Must be rejected.
    make_card(root, 0, "0x8086", "some_other_driver", "0x1234");

    let cards = discover_cards(root);
    assert!(cards.is_empty());
}

#[test]
fn classify_variant_discrete_via_i915() {
    let dir = tempdir().unwrap();
    let card = make_card(dir.path(), 0, "0x8086", "i915", "0x56A0");
    let device = card.join("device");
    fs::write(device.join("mem_info_vram_total"), "17179869184\n").unwrap();

    assert_eq!(classify_variant(&device), MemoryVariant::Discrete);
}

#[test]
fn classify_variant_discrete_via_xe() {
    let dir = tempdir().unwrap();
    let card = make_card(dir.path(), 0, "0x8086", "xe", "0xE20B");
    let device = card.join("device");
    let xe_dir = device.join("tile0").join("vram0");
    fs::create_dir_all(&xe_dir).unwrap();
    fs::write(xe_dir.join("total_bytes"), "12884901888\n").unwrap();

    assert_eq!(classify_variant(&device), MemoryVariant::Discrete);
}

#[test]
fn classify_variant_integrated_when_no_vram() {
    let dir = tempdir().unwrap();
    let card = make_card(dir.path(), 0, "0x8086", "i915", "0x7D40");
    let device = card.join("device");

    assert_eq!(classify_variant(&device), MemoryVariant::Integrated);
}

#[test]
fn get_gpu_info_populates_basic_fields() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    let card = make_card(root, 0, "0x8086", "i915", "0x56A0");
    let device = card.join("device");
    fs::write(device.join("mem_info_vram_total"), "17179869184\n").unwrap();
    fs::write(device.join("mem_info_vram_used"), "4294967296\n").unwrap();
    fs::write(device.join("gt_cur_freq_mhz"), "1950\n").unwrap();
    let hwmon = device.join("hwmon").join("hwmon0");
    fs::create_dir_all(&hwmon).unwrap();
    fs::write(hwmon.join("temp1_input"), "68000\n").unwrap();
    fs::write(hwmon.join("power1_average"), "150000000\n").unwrap();

    let reader = IntelGpuReader::new_from_root(root);
    let info = reader.get_gpu_info();
    assert_eq!(info.len(), 1);
    let g = &info[0];
    assert_eq!(g.device_type, "GPU");
    assert!(g.name.contains("Arc A770"));
    assert_eq!(g.total_memory, 17_179_869_184);
    assert_eq!(g.used_memory, 4_294_967_296);
    assert_eq!(g.frequency, 1950);
    assert_eq!(g.temperature, 68);
    assert!((g.power_consumption - 150.0).abs() < 0.01);
    assert_eq!(g.utilization, 0.0);
    assert_eq!(
        g.detail.get("Variant").map(String::as_str),
        Some("Discrete")
    );
    assert_eq!(g.detail.get("Driver").map(String::as_str), Some("i915"));
    // No engine sysfs entries in this fixture -> reader must surface
    // the explanatory note, NOT the obsolete `intel_gpu_top` text.
    assert_eq!(
        g.detail.get("Utilization").map(String::as_str),
        Some("Engine counters unavailable (kernel does not expose engine busy)")
    );
    // The pre-issue-#246 placeholder must not leak back in.
    assert_ne!(
        g.detail.get("Utilization").map(String::as_str),
        Some("Requires intel_gpu_top (perf engine counters)")
    );
    // Architecture / SYCL classification — derived from the resolved
    // marketing name. Arc A770 is Alchemist (SYCL-capable).
    assert_eq!(
        g.detail.get("Architecture").map(String::as_str),
        Some("Alchemist (Xe-HPG, A-series)")
    );
    assert_eq!(
        g.detail.get("SYCL Capable").map(String::as_str),
        Some("Yes")
    );
    // `Metrics Source` advertises which backend produced the metrics.
    // With the L0 feature off (or no L0 runtime on the host) this MUST
    // stay at the sysfs baseline so operators can see the augmentation
    // never ran. The level_zero augmentation upgrades it to
    // `"sysfs + Level Zero"` only when an L0 readout carries data —
    // which requires real hardware (issue #248 deferred AC).
    assert_eq!(
        g.detail.get("Metrics Source").map(String::as_str),
        Some("sysfs (engine counters)")
    );
    // The Intel reader populates NVIDIA-only fields with None / empty
    // defaults — verify the contract so consumers can render them as
    // "unavailable" rather than misinterpreting zeros.
    assert!(g.temperature_threshold_slowdown.is_none());
    assert!(g.performance_state.is_none());
    assert!(g.nvlink_remote_devices.is_empty());
    assert!(g.gpm_metrics.is_none());
}

#[test]
fn get_gpu_info_integrated_reports_zero_memory() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    make_card(root, 0, "0x8086", "i915", "0x7D40");

    let reader = IntelGpuReader::new_from_root(root);
    let info = reader.get_gpu_info();
    assert_eq!(info.len(), 1);
    assert_eq!(info[0].total_memory, 0);
    assert_eq!(info[0].used_memory, 0);
    assert_eq!(
        info[0].detail.get("Variant").map(String::as_str),
        Some("Integrated")
    );
    assert!(
        info[0].detail.contains_key("Memory"),
        "integrated GPUs should explain the shared-memory situation"
    );
    // Meteor Lake / Core Ultra iGPU is Xe-LPG and SYCL-capable.
    assert_eq!(
        info[0].detail.get("Architecture").map(String::as_str),
        Some("Xe-LPG (Meteor Lake)")
    );
    assert_eq!(
        info[0].detail.get("SYCL Capable").map(String::as_str),
        Some("Yes")
    );
}

#[test]
fn get_gpu_info_seeding_emits_seeding_note_when_engines_exist() {
    // When engine counters are discoverable, the very first refresh
    // is a seeding call: baselines are stamped, utilization stays at
    // 0.0, and the detail map carries the seeding note instead of
    // the no-counters note.
    let dir = tempdir().unwrap();
    let root = dir.path();
    let card = make_card(root, 0, "0x8086", "i915", "0x56A0");
    // Add an i915 engine counter so discovery finds something.
    let engine_root = card.join("engine").join("rcs0");
    fs::create_dir_all(&engine_root).unwrap();
    fs::write(engine_root.join("busy"), "0\n").unwrap();

    let reader = IntelGpuReader::new_from_root(root);
    let info = reader.get_gpu_info();
    assert_eq!(info.len(), 1);
    let g = &info[0];
    assert_eq!(g.utilization, 0.0);
    assert_eq!(
        g.detail.get("Utilization").map(String::as_str),
        Some("Engine counters seeded (utilization available next refresh)")
    );
    // No per-engine entries yet — they appear from the *second* call.
    assert!(
        g.detail.keys().all(|k| !k.starts_with("Engine: ")),
        "seeding call must not produce Engine: detail keys yet, got: {:?}",
        g.detail.keys().collect::<Vec<_>>()
    );
}

#[test]
fn get_gpu_info_second_call_surfaces_engine_percent() {
    // After a seeding call, the second refresh — with a non-zero busy
    // counter update — must compute a real utilization and add per-
    // engine `Engine: <class>` entries to the detail map. We can't
    // inject a fake clock into the production reader, so the assertion
    // here is on what the second call *can* observe: the counter
    // delta is positive and the readout is no longer in the seeding
    // state.
    let dir = tempdir().unwrap();
    let root = dir.path();
    let card = make_card(root, 0, "0x8086", "i915", "0x56A0");
    let engine_root = card.join("engine").join("rcs0");
    fs::create_dir_all(&engine_root).unwrap();
    fs::write(engine_root.join("busy"), "0\n").unwrap();

    let reader = IntelGpuReader::new_from_root(root);
    let _seed = reader.get_gpu_info(); // seeding call
    // Bump the counter to a value larger than any plausible wall-clock
    // delta so the resulting percentage clamps to 100. This avoids a
    // flaky assertion that depends on how long the test runner takes
    // between the two `get_gpu_info` calls.
    fs::write(engine_root.join("busy"), "10000000000000\n").unwrap();
    let info = reader.get_gpu_info();
    assert_eq!(info.len(), 1);
    let g = &info[0];
    // Utilization should reflect a positive engine-busy fraction.
    assert!(
        g.utilization > 0.0,
        "second call must report non-zero utilization, got {}",
        g.utilization
    );
    // Per-engine detail entry must exist for the render engine.
    assert!(
        g.detail.contains_key("Engine: render"),
        "missing Engine: render entry. detail = {:?}",
        g.detail
    );
    // The static `Utilization` note is removed once live data is
    // available.
    assert!(
        !g.detail.contains_key("Utilization"),
        "Utilization note should be cleared when engine data is live, got: {:?}",
        g.detail.get("Utilization")
    );
}

#[test]
fn has_intel_client_gpu_positive() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    make_card(root, 0, "0x8086", "i915", "0x56A0");
    assert!(super::detection::has_intel_client_gpu_from_root(root));
}

#[test]
fn has_intel_client_gpu_rejects_amd() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    make_card(root, 0, "0x1002", "amdgpu", "0x73BF");
    assert!(!super::detection::has_intel_client_gpu_from_root(root));
}

#[test]
fn line_matches_intel_gpu_positive_3d() {
    // Class 0302 (3D controller), Intel vendor.
    let line = "03:00.0 0302: 8086:56a0 (rev 08)";
    assert!(super::detection::line_matches_intel_gpu(line));
}

#[test]
fn line_matches_intel_gpu_positive_vga() {
    let line = "00:02.0 0300: 8086:7d40";
    assert!(super::detection::line_matches_intel_gpu(line));
}

#[test]
fn line_matches_intel_gpu_rejects_intel_nic() {
    // Intel NIC, vendor 8086 but class 0200 (Ethernet).
    let line = "02:00.0 0200: 8086:15bb";
    assert!(!super::detection::line_matches_intel_gpu(line));
}

#[test]
fn line_matches_intel_gpu_rejects_other_vendor_vga() {
    let line = "01:00.0 0300: 10de:2204";
    assert!(!super::detection::line_matches_intel_gpu(line));
}

// ----- get_process_info integration tests (synthetic procfs) -----

/// Build a `cardN` + `renderD<M>` pair sharing a fake PCI device under
/// `drm_root`. The `device` symlink target is what
/// `build_intel_drm_basenames` follows to correlate render nodes to
/// cards, so both nodes point at the same `_pci/<bus>` directory.
fn make_card_and_render(drm_root: &Path, idx: u32, render_minor: u32, pci_bus: &str) {
    let pci_dir = drm_root.join("_pci").join(pci_bus);
    fs::create_dir_all(&pci_dir).unwrap();

    let card = make_card(drm_root, idx, "0x8086", "i915", "0x56A0");
    // Replace the default `device` directory (a real dir from make_card)
    // with a symlink to the PCI bus so build_intel_drm_basenames can
    // resolve the bus identifier. The make_card helper created
    // device/ as a directory; we move its contents into pci_dir then
    // replace it with a symlink.
    let device_dir = card.join("device");
    // Move existing files into pci_dir so the symlink target has the
    // same sysfs surface the rest of the reader expects.
    for entry in fs::read_dir(&device_dir).unwrap().flatten() {
        let target = pci_dir.join(entry.file_name());
        fs::rename(entry.path(), target).unwrap();
    }
    fs::remove_dir(&device_dir).unwrap();
    std::os::unix::fs::symlink(&pci_dir, device_dir).unwrap();

    // Render node points at the same PCI device.
    let render = drm_root.join(format!("renderD{render_minor}"));
    fs::create_dir_all(&render).unwrap();
    std::os::unix::fs::symlink(&pci_dir, render.join("device")).unwrap();
}

/// Create a `/proc/<pid>/fd/<n>` symlink pointing at
/// `<proc_root>/_dri/<basename>` plus its `fdinfo/<n>` file.
fn make_proc_fd_for_pid(proc_root: &Path, pid: u32, fd: u32, dri_basename: &str, fdinfo: &str) {
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
fn get_process_info_returns_empty_when_no_intel_cards() {
    // No Intel cards enumerated -> get_process_info MUST return empty
    // without touching /proc. Guarantees no regression for AMD-only /
    // NVIDIA-only hosts that incidentally have Intel-vendor non-GPU
    // PCI devices.
    let drm = tempdir().unwrap();
    let proc = tempdir().unwrap();
    let reader = IntelGpuReader::new_with_roots(drm.path(), proc.path());
    let infos = reader.get_process_info();
    assert!(infos.is_empty());
}

#[test]
fn get_process_info_collects_fdinfo_from_render_node() {
    // End-to-end pipeline: enumerate one Intel card, walk a synthetic
    // /proc, parse the fdinfo block, and yield a populated ProcessInfo
    // with the expected used_memory.
    let drm = tempdir().unwrap();
    let proc = tempdir().unwrap();
    make_card_and_render(drm.path(), 0, 128, "0000:03:00.0");
    make_proc_fd_for_pid(
        proc.path(),
        std::process::id(),
        3,
        "renderD128",
        "drm-driver: i915\n\
         drm-pdev: 0000:03:00.0\n\
         drm-client-id: 42\n\
         drm-resident-local0: 16384 kB\n",
    );

    let reader = IntelGpuReader::new_with_roots(drm.path(), proc.path());
    let infos = reader.get_process_info();
    assert_eq!(infos.len(), 1, "expected one Intel-GPU-using process");
    let info = &infos[0];
    assert_eq!(info.pid, std::process::id());
    assert_eq!(info.device_id, 0);
    // UUID format matches `build_uuid`: `Intel-GPU-<pci_bus>`.
    assert_eq!(info.device_uuid, "Intel-GPU-0000:03:00.0");
    assert_eq!(info.used_memory, 16_384 * 1024);
    assert!(info.uses_gpu);
    // Stretch-goal stays deferred: per-process engine-time stays 0.0.
    assert_eq!(info.gpu_utilization, 0.0);
}

#[test]
fn get_process_info_default_filter_keeps_uses_gpu_processes() {
    // The default `GpuReader::get_gpu_processes` impl filters by
    // `uses_gpu == true`. Verify the Intel reader is compatible with
    // that filter (every emitted entry must have `uses_gpu` set).
    use crate::device::traits::GpuReader as _;

    let drm = tempdir().unwrap();
    let proc = tempdir().unwrap();
    make_card_and_render(drm.path(), 0, 128, "0000:03:00.0");
    make_proc_fd_for_pid(
        proc.path(),
        std::process::id(),
        3,
        "renderD128",
        "drm-driver: i915\ndrm-client-id: 1\ndrm-resident-local0: 4096 kB\n",
    );

    let reader = IntelGpuReader::new_with_roots(drm.path(), proc.path());
    let (filtered, pids) = reader.get_gpu_processes();
    assert_eq!(filtered.len(), 1);
    assert!(pids.contains(&std::process::id()));
}
