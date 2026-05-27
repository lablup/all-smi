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

//! Unit tests for the Intel DRM client `fdinfo` parser, DRM-minor
//! mapping, `/proc/<pid>/fd/` walker, and the per-process aggregate
//! collector.
//!
//! All filesystem fixtures are built under a `tempfile::tempdir` —
//! tests never touch the real `/proc` or `/sys/class/drm`.

use super::*;
use std::fs;
use std::path::Path;

// ----- parse_fdinfo -----

#[test]
fn parse_fdinfo_i915_discrete() {
    // Real i915 fdinfo on an Arc A770 (discrete VRAM + GTT).
    let content = "\
pos:    0
flags:  02100002
mnt_id: 25
ino:    1234
drm-driver: i915
drm-pdev:   0000:03:00.0
drm-client-id: 42
drm-total-system:   65536 kB
drm-total-local0:   524288 kB
drm-resident-system: 32768 kB
drm-resident-local0: 262144 kB
drm-engine-render: 1234567890 ns
";
    let info = parse_fdinfo(content).expect("i915 fdinfo should parse");
    assert_eq!(info.drm_driver, "i915");
    assert_eq!(info.drm_pdev.as_deref(), Some("0000:03:00.0"));
    assert_eq!(info.drm_client_id, Some(42));
    // 32768 kB + 262144 kB = 294912 kB = 302_039_040 bytes
    assert_eq!(info.resident_bytes, (32_768u64 + 262_144) * 1024);
}

#[test]
fn parse_fdinfo_i915_integrated() {
    // Integrated GPUs do not expose local0/vram0; only system/GTT.
    let content = "\
drm-driver: i915
drm-pdev: 0000:00:02.0
drm-client-id: 7
drm-resident-system: 12288 kB
";
    let info = parse_fdinfo(content).expect("i915 integrated fdinfo should parse");
    assert_eq!(info.drm_driver, "i915");
    assert_eq!(info.resident_bytes, 12_288 * 1024);
}

#[test]
fn parse_fdinfo_xe_discrete() {
    // xe driver schema — different key names, same shape.
    let content = "\
drm-driver: xe
drm-pdev: 0000:03:00.0
drm-client-id: 99
drm-total-vram0: 1048576 kB
drm-total-gtt: 65536 kB
drm-resident-vram0: 524288 kB
drm-resident-gtt: 32768 kB
drm-engine-rcs: 9876543210 ns
";
    let info = parse_fdinfo(content).expect("xe fdinfo should parse");
    assert_eq!(info.drm_driver, "xe");
    assert_eq!(info.drm_client_id, Some(99));
    assert_eq!(info.resident_bytes, (524_288u64 + 32_768) * 1024);
}

#[test]
fn parse_fdinfo_non_drm_returns_none() {
    // A regular socket fdinfo has no drm-driver line.
    let content = "\
pos: 0
flags: 02
mnt_id: 25
ino: 998877
";
    assert!(parse_fdinfo(content).is_none());
}

#[test]
fn parse_fdinfo_rejects_foreign_driver() {
    // amdgpu fdinfo MUST NOT be accepted by the Intel parser.
    let content = "\
drm-driver: amdgpu
drm-pdev: 0000:0a:00.0
drm-client-id: 1
drm-resident-vram0: 4096 kB
";
    assert!(parse_fdinfo(content).is_none());
}

#[test]
fn parse_fdinfo_tolerates_truncated_content() {
    // Truncated mid-line is what we see when a process is exiting
    // while we read the file. Must not panic.
    let content = "drm-driver: i915\ndrm-resident-syste";
    let info = parse_fdinfo(content).expect("truncated fdinfo should still parse the header");
    assert_eq!(info.drm_driver, "i915");
    assert_eq!(info.resident_bytes, 0);
}

#[test]
fn parse_fdinfo_skips_malformed_lines() {
    // Lines with non-numeric values are skipped, others still parse.
    let content = "\
drm-driver: i915
drm-pdev: 0000:00:02.0
drm-client-id: not_a_number
drm-resident-system: bogus
drm-resident-local0: 16384 kB
";
    let info = parse_fdinfo(content).expect("malformed lines should be skipped, not fatal");
    assert_eq!(info.drm_driver, "i915");
    assert_eq!(info.drm_client_id, None);
    assert_eq!(info.resident_bytes, 16_384 * 1024);
}

#[test]
fn parse_fdinfo_kb_multiplier_is_1024() {
    // 1 kB in /proc DRM fdinfo is 1024 bytes per kernel convention
    // (kernel emits "kB" — same as /proc/meminfo). The parser must
    // multiply, not return the raw kB count.
    let content = "\
drm-driver: i915
drm-resident-system: 1 kB
";
    let info = parse_fdinfo(content).unwrap();
    assert_eq!(info.resident_bytes, 1024);
}

#[test]
fn parse_fdinfo_empty_string_returns_none() {
    assert!(parse_fdinfo("").is_none());
}

// ----- build_intel_drm_basenames -----

/// Synthesise a `/sys/class/drm/` layout: a `cardN` with a `device`
/// symlink pointing at a fake PCI device directory, and optionally a
/// matching `renderD<M>` whose `device` symlink points at the same PCI
/// device. Returns the absolute path of the synthesised cardN entry.
fn make_drm_layout(drm_root: &Path, card_idx: u32, render_minor: Option<u32>, pci_bus: &str) {
    // Per-card PCI device parent dir.
    let pci_devices = drm_root.join("_pci_devices");
    fs::create_dir_all(&pci_devices).unwrap();
    let pci_dir = pci_devices.join(pci_bus);
    fs::create_dir_all(&pci_dir).unwrap();

    // cardN
    let card = drm_root.join(format!("card{card_idx}"));
    fs::create_dir_all(&card).unwrap();
    std::os::unix::fs::symlink(&pci_dir, card.join("device")).unwrap();

    // renderD<M> (optional)
    if let Some(minor) = render_minor {
        let render = drm_root.join(format!("renderD{minor}"));
        fs::create_dir_all(&render).unwrap();
        std::os::unix::fs::symlink(&pci_dir, render.join("device")).unwrap();
    }
}

#[test]
fn build_intel_drm_basenames_maps_card_and_render_to_same_index() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    make_drm_layout(root, 0, Some(128), "0000:03:00.0");

    let card_path = root.join("card0");
    let basenames = build_intel_drm_basenames(&[(card_path, 0)], root);

    assert_eq!(basenames.get("card0").copied(), Some(0));
    assert_eq!(
        basenames.get("renderD128").copied(),
        Some(0),
        "render node MUST map to the same card index; otherwise Vulkan/oneAPI workloads are missed"
    );
}

#[test]
fn build_intel_drm_basenames_two_cards_two_render_nodes() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    make_drm_layout(root, 0, Some(128), "0000:03:00.0");
    make_drm_layout(root, 1, Some(129), "0000:04:00.0");

    let basenames =
        build_intel_drm_basenames(&[(root.join("card0"), 0), (root.join("card1"), 1)], root);

    assert_eq!(basenames.get("card0").copied(), Some(0));
    assert_eq!(basenames.get("renderD128").copied(), Some(0));
    assert_eq!(basenames.get("card1").copied(), Some(1));
    assert_eq!(basenames.get("renderD129").copied(), Some(1));
}

#[test]
fn build_intel_drm_basenames_ignores_non_intel_render_nodes() {
    // A renderD entry belonging to a PCI device the reader did NOT
    // pre-enumerate (e.g. an AMD GPU's render node) must be skipped.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // Intel card.
    make_drm_layout(root, 0, Some(128), "0000:03:00.0");
    // Unknown (e.g. AMD) render node — not in our intel_cards slice.
    make_drm_layout(root, 1, Some(129), "0000:04:00.0");

    let basenames = build_intel_drm_basenames(&[(root.join("card0"), 0)], root);

    assert_eq!(basenames.get("card0").copied(), Some(0));
    assert_eq!(basenames.get("renderD128").copied(), Some(0));
    assert!(
        !basenames.contains_key("card1"),
        "non-Intel card must not appear in the basename map"
    );
    assert!(
        !basenames.contains_key("renderD129"),
        "non-Intel render node must not appear in the basename map"
    );
}

#[test]
fn build_intel_drm_basenames_ignores_connector_children() {
    // The kernel exposes `card0-eDP-1`, `card0-HDMI-A-1`, etc. as
    // child directories. Those must NOT enter the basename map.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    make_drm_layout(root, 0, Some(128), "0000:00:02.0");
    fs::create_dir_all(root.join("card0-eDP-1")).unwrap();
    fs::create_dir_all(root.join("card0-HDMI-A-1")).unwrap();

    let basenames = build_intel_drm_basenames(&[(root.join("card0"), 0)], root);
    assert!(!basenames.contains_key("card0-eDP-1"));
    assert!(!basenames.contains_key("card0-HDMI-A-1"));
}

// ----- intel_drm_fds_for_pid -----

/// Construct a `/proc/<pid>/fd/<fd>` symlink pointing at a fake
/// `/dev/dri/<basename>` plus the matching `fdinfo` file. The dev path
/// only needs the basename to match — readlink() returns whatever the
/// symlink points at, and the walker matches on file_name().
fn make_proc_fd(proc_root: &Path, pid: u32, fd: u32, dri_basename: &str, fdinfo_content: &str) {
    let fd_dir = proc_root.join(pid.to_string()).join("fd");
    let fdinfo_dir = proc_root.join(pid.to_string()).join("fdinfo");
    fs::create_dir_all(&fd_dir).unwrap();
    fs::create_dir_all(&fdinfo_dir).unwrap();

    // Build a fake /dev/dri/<basename> path. The target does not need
    // to exist for read_link to return its name.
    let dev_target = proc_root.join("_dri").join(dri_basename);
    fs::create_dir_all(dev_target.parent().unwrap()).unwrap();
    // Touch a file so the path is actually valid (some lints may
    // complain about dangling symlinks but Linux does not).
    fs::write(&dev_target, b"").unwrap();
    std::os::unix::fs::symlink(&dev_target, fd_dir.join(fd.to_string())).unwrap();

    fs::write(fdinfo_dir.join(fd.to_string()), fdinfo_content).unwrap();
}

#[test]
fn intel_drm_fds_for_pid_finds_render_node_fd() {
    let dir = tempfile::tempdir().unwrap();
    let proc_root = dir.path();
    make_proc_fd(proc_root, 1234, 7, "renderD128", "drm-driver: i915\n");

    let basenames = HashMap::from([("renderD128".to_string(), 0_usize)]);
    let fds = intel_drm_fds_for_pid(1234, &basenames, proc_root);
    assert_eq!(fds.len(), 1);
    assert_eq!(fds[0].fd_num, 7);
    assert_eq!(fds[0].card_index, 0);
    assert!(fds[0].fdinfo_path.ends_with("1234/fdinfo/7"));
}

#[test]
fn intel_drm_fds_for_pid_skips_non_dri_fds() {
    // A regular socket / pipe fd target must not be mistaken for a
    // DRM fd.
    let dir = tempfile::tempdir().unwrap();
    let proc_root = dir.path();
    make_proc_fd(proc_root, 1234, 3, "renderD128", "drm-driver: i915\n");
    // fd=4 points at a fake socket — basename does not match the map.
    let dev_target = proc_root.join("_socket").join("socket:[42]");
    fs::create_dir_all(dev_target.parent().unwrap()).unwrap();
    fs::write(&dev_target, b"").unwrap();
    let fd_dir = proc_root.join("1234").join("fd");
    std::os::unix::fs::symlink(&dev_target, fd_dir.join("4")).unwrap();

    let basenames = HashMap::from([("renderD128".to_string(), 0_usize)]);
    let fds = intel_drm_fds_for_pid(1234, &basenames, proc_root);
    assert_eq!(fds.len(), 1);
    assert_eq!(fds[0].fd_num, 3);
}

#[test]
fn intel_drm_fds_for_pid_missing_fd_dir_returns_empty() {
    // PID with no /proc entry at all -> empty, never panics.
    let dir = tempfile::tempdir().unwrap();
    let proc_root = dir.path();
    let basenames = HashMap::from([("renderD128".to_string(), 0_usize)]);
    let fds = intel_drm_fds_for_pid(99999, &basenames, proc_root);
    assert!(fds.is_empty());
}

// ----- collect_intel_gpu_processes (end-to-end) -----

#[test]
fn collect_intel_gpu_processes_basic() {
    let dir = tempfile::tempdir().unwrap();
    let proc_root = dir.path();
    make_proc_fd(
        proc_root,
        1234,
        3,
        "renderD128",
        "drm-driver: i915\ndrm-pdev: 0000:03:00.0\ndrm-client-id: 1\ndrm-resident-local0: 16384 kB\n",
    );

    let basenames = HashMap::from([("renderD128".to_string(), 0_usize)]);
    let usages = collect_intel_gpu_processes(&basenames, proc_root);
    assert_eq!(usages.len(), 1);
    assert_eq!(usages[0].pid, 1234);
    assert_eq!(usages[0].card_index, 0);
    assert_eq!(usages[0].used_memory_bytes, 16_384 * 1024);
}

#[test]
fn collect_intel_gpu_processes_dedupes_by_client_id() {
    // Same process holds two fds to the same DRM client. We must
    // NOT double-count the resident memory.
    let dir = tempfile::tempdir().unwrap();
    let proc_root = dir.path();
    let fdinfo = "drm-driver: i915\ndrm-pdev: 0000:03:00.0\ndrm-client-id: 7\ndrm-resident-local0: 65536 kB\n";
    make_proc_fd(proc_root, 4242, 3, "renderD128", fdinfo);
    make_proc_fd(proc_root, 4242, 4, "renderD128", fdinfo);

    let basenames = HashMap::from([("renderD128".to_string(), 0_usize)]);
    let usages = collect_intel_gpu_processes(&basenames, proc_root);
    assert_eq!(usages.len(), 1);
    assert_eq!(
        usages[0].used_memory_bytes,
        65_536 * 1024,
        "two fds sharing one drm-client-id must report the per-client size once, not twice"
    );
}

#[test]
fn collect_intel_gpu_processes_sums_distinct_clients() {
    // Same process, two distinct DRM clients (e.g. two contexts):
    // those represent separate allocations and DO sum.
    let dir = tempfile::tempdir().unwrap();
    let proc_root = dir.path();
    make_proc_fd(
        proc_root,
        4242,
        3,
        "renderD128",
        "drm-driver: i915\ndrm-client-id: 7\ndrm-resident-local0: 16384 kB\n",
    );
    make_proc_fd(
        proc_root,
        4242,
        4,
        "renderD128",
        "drm-driver: i915\ndrm-client-id: 8\ndrm-resident-local0: 32768 kB\n",
    );

    let basenames = HashMap::from([("renderD128".to_string(), 0_usize)]);
    let usages = collect_intel_gpu_processes(&basenames, proc_root);
    assert_eq!(usages.len(), 1);
    assert_eq!(
        usages[0].used_memory_bytes,
        (16_384 + 32_768) * 1024,
        "two distinct client ids in one process must sum"
    );
}

#[test]
fn collect_intel_gpu_processes_groups_by_card() {
    // Same process holds fds to TWO different cards.
    let dir = tempfile::tempdir().unwrap();
    let proc_root = dir.path();
    make_proc_fd(
        proc_root,
        4242,
        3,
        "renderD128",
        "drm-driver: i915\ndrm-client-id: 1\ndrm-resident-local0: 16384 kB\n",
    );
    make_proc_fd(
        proc_root,
        4242,
        4,
        "renderD129",
        "drm-driver: xe\ndrm-client-id: 2\ndrm-resident-vram0: 32768 kB\n",
    );

    let basenames = HashMap::from([
        ("renderD128".to_string(), 0_usize),
        ("renderD129".to_string(), 1_usize),
    ]);
    let mut usages = collect_intel_gpu_processes(&basenames, proc_root);
    usages.sort_by_key(|u| u.card_index);
    assert_eq!(usages.len(), 2);
    assert_eq!(usages[0].card_index, 0);
    assert_eq!(usages[0].used_memory_bytes, 16_384 * 1024);
    assert_eq!(usages[1].card_index, 1);
    assert_eq!(usages[1].used_memory_bytes, 32_768 * 1024);
}

#[test]
fn collect_intel_gpu_processes_empty_basenames_returns_empty() {
    // No Intel cards enumerated -> we MUST short-circuit and return
    // empty without walking /proc. Verifies the no-regression path
    // for AMD-only / NVIDIA-only hosts.
    let dir = tempfile::tempdir().unwrap();
    let proc_root = dir.path();
    make_proc_fd(
        proc_root,
        1234,
        3,
        "renderD128",
        "drm-driver: i915\ndrm-client-id: 1\ndrm-resident-local0: 16384 kB\n",
    );

    let basenames: HashMap<String, usize> = HashMap::new();
    let usages = collect_intel_gpu_processes(&basenames, proc_root);
    assert!(usages.is_empty());
}

#[test]
fn collect_intel_gpu_processes_skips_processes_without_drm_fds() {
    // PID 1234 has no DRM fds, PID 4242 does. Only 4242 should appear.
    let dir = tempfile::tempdir().unwrap();
    let proc_root = dir.path();
    // PID 1234 has only a non-DRM fd.
    let fd_dir = proc_root.join("1234").join("fd");
    fs::create_dir_all(&fd_dir).unwrap();
    let other_target = proc_root.join("_other").join("file");
    fs::create_dir_all(other_target.parent().unwrap()).unwrap();
    fs::write(&other_target, b"").unwrap();
    std::os::unix::fs::symlink(&other_target, fd_dir.join("3")).unwrap();
    // PID 4242 has a real DRM fd.
    make_proc_fd(
        proc_root,
        4242,
        3,
        "renderD128",
        "drm-driver: i915\ndrm-client-id: 1\ndrm-resident-local0: 16384 kB\n",
    );

    let basenames = HashMap::from([("renderD128".to_string(), 0_usize)]);
    let usages = collect_intel_gpu_processes(&basenames, proc_root);
    assert_eq!(usages.len(), 1);
    assert_eq!(usages[0].pid, 4242);
}

#[test]
fn collect_intel_gpu_processes_tolerates_missing_client_id() {
    // Very old kernels emit fdinfo without drm-client-id. We must
    // still credit the memory (without dedup), not crash.
    let dir = tempfile::tempdir().unwrap();
    let proc_root = dir.path();
    make_proc_fd(
        proc_root,
        1234,
        3,
        "renderD128",
        "drm-driver: i915\ndrm-resident-local0: 16384 kB\n",
    );

    let basenames = HashMap::from([("renderD128".to_string(), 0_usize)]);
    let usages = collect_intel_gpu_processes(&basenames, proc_root);
    assert_eq!(usages.len(), 1);
    assert_eq!(usages[0].used_memory_bytes, 16_384 * 1024);
}
