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

//! Tests for the shared `/proc` scan and its two folds.
//!
//! The two `legacy_*` functions below are the walkers as they were before
//! the scan was shared, kept verbatim as oracles: each did its own `/proc`
//! walk and its own grouping. The folds over one shared scan must return
//! exactly what they return on every fixture.

use super::io::read_fdinfo_to_string;
use super::*;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

// ----- oracles: the pre-refactor walkers, unchanged -----

fn legacy_collect_intel_gpu_processes(
    intel_drm_basenames: &HashMap<String, usize>,
    proc_root: &Path,
) -> Vec<GpuProcessUsage> {
    // Empty card map means no Intel GPUs were enumerated; nothing to
    // do. This is the steady-state for an AMD-only or NVIDIA-only host.
    if intel_drm_basenames.is_empty() {
        return Vec::new();
    }

    let Ok(entries) = std::fs::read_dir(proc_root) else {
        return Vec::new();
    };

    // Aggregate state: per (pid, card_index, drm_client_id) -> bytes.
    // We keep the most recent (highest) resident value across fds with
    // the same client id, because the resident block can race with the
    // kernel's writer; the largest value is the most recent successful
    // snapshot. Different client ids in the same (pid, card) DO sum.
    type Key = (u32, usize);
    let mut per_client: HashMap<Key, HashMap<u64, u64>> = HashMap::new();
    // For fds without a client-id, we cannot dedupe, so we credit each
    // fd separately under a synthetic "no client id" bucket keyed by
    // fdinfo path. Same-key entries overwrite (max), different keys
    // sum. In practice modern kernels always emit drm-client-id so
    // this branch is a safety net only.
    let mut no_client_id: HashMap<Key, HashMap<PathBuf, u64>> = HashMap::new();
    let mut process_count: usize = 0;

    for entry in entries.flatten() {
        if process_count >= MAX_GPU_PROCESSES {
            break;
        }
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Ok(pid) = name.parse::<u32>() else {
            continue;
        };

        let fds = intel_drm_fds_for_pid(pid, intel_drm_basenames, proc_root);
        if fds.is_empty() {
            continue;
        }
        process_count += 1;

        for fd in fds {
            let Some(content) = read_fdinfo_to_string(&fd.fdinfo_path) else {
                continue; // permission / TOCTOU process exit / oversized fdinfo
            };
            let Some(info) = parse_fdinfo(&content) else {
                continue;
            };
            let key = (pid, fd.card_index);
            match info.drm_client_id {
                Some(cid) => {
                    let entry = per_client.entry(key).or_default().entry(cid).or_insert(0);
                    if info.resident_bytes > *entry {
                        *entry = info.resident_bytes;
                    }
                }
                None => {
                    let entry = no_client_id
                        .entry(key)
                        .or_default()
                        .entry(fd.fdinfo_path)
                        .or_insert(0);
                    if info.resident_bytes > *entry {
                        *entry = info.resident_bytes;
                    }
                }
            }
        }
    }

    // Fold into the final aggregate: sum distinct clients per (pid, card).
    let mut keys: HashSet<Key> = HashSet::new();
    for k in per_client.keys() {
        keys.insert(*k);
    }
    for k in no_client_id.keys() {
        keys.insert(*k);
    }

    let mut out: Vec<GpuProcessUsage> = Vec::with_capacity(keys.len());
    for (pid, card_index) in keys {
        let mut total: u64 = 0;
        if let Some(by_client) = per_client.get(&(pid, card_index)) {
            for bytes in by_client.values() {
                total = total.saturating_add(*bytes);
            }
        }
        if let Some(by_fd) = no_client_id.get(&(pid, card_index)) {
            for bytes in by_fd.values() {
                total = total.saturating_add(*bytes);
            }
        }
        out.push(GpuProcessUsage {
            pid,
            card_index,
            used_memory_bytes: total,
        });
    }

    // Stable ordering keeps downstream consumers' output deterministic
    // across refreshes — PID-major, then by card index.
    out.sort_by_key(|u| (u.pid, u.card_index));
    out
}

fn legacy_sum_vram_by_card_from_fdinfo(
    intel_drm_basenames: &HashMap<String, usize>,
    proc_root: &Path,
) -> HashMap<usize, u64> {
    let mut out: HashMap<usize, u64> = HashMap::new();
    if intel_drm_basenames.is_empty() {
        return out;
    }
    let Ok(entries) = std::fs::read_dir(proc_root) else {
        return out;
    };

    // Per-card aggregation: client-id keyed VRAM bytes (max-wins) plus
    // a per-fdinfo-path bucket for fds whose fdinfo lacks a client id.
    let mut per_client: HashMap<usize, HashMap<u64, u64>> = HashMap::new();
    let mut no_client_id: HashMap<usize, HashMap<PathBuf, u64>> = HashMap::new();
    let mut process_count: usize = 0;

    for entry in entries.flatten() {
        if process_count >= MAX_GPU_PROCESSES {
            break;
        }
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Ok(pid) = name.parse::<u32>() else {
            continue;
        };

        let fds = intel_drm_fds_for_pid(pid, intel_drm_basenames, proc_root);
        if fds.is_empty() {
            continue;
        }
        process_count += 1;

        for fd in fds {
            let Some(content) = read_fdinfo_to_string(&fd.fdinfo_path) else {
                continue;
            };
            let Some(info) = parse_fdinfo(&content) else {
                continue;
            };
            match info.drm_client_id {
                Some(cid) => {
                    let entry = per_client
                        .entry(fd.card_index)
                        .or_default()
                        .entry(cid)
                        .or_insert(0);
                    *entry = (*entry).max(info.resident_vram_bytes);
                }
                None => {
                    let entry = no_client_id
                        .entry(fd.card_index)
                        .or_default()
                        .entry(fd.fdinfo_path)
                        .or_insert(0);
                    *entry = (*entry).max(info.resident_vram_bytes);
                }
            }
        }
    }

    for (card_index, by_client) in &per_client {
        let slot = out.entry(*card_index).or_insert(0);
        for bytes in by_client.values() {
            *slot = slot.saturating_add(*bytes);
        }
    }
    for (card_index, by_fd) in &no_client_id {
        let slot = out.entry(*card_index).or_insert(0);
        for bytes in by_fd.values() {
            *slot = slot.saturating_add(*bytes);
        }
    }
    out
}

// ----- fixtures -----

/// One fd of a synthetic procfs: a `/proc/<pid>/fd/<fd>` symlink to a
/// `/dev/dri/<basename>` stand-in plus its `fdinfo` file.
fn make_proc_fd(proc_root: &Path, pid: u32, fd: u32, dri_basename: &str, fdinfo_content: &str) {
    let fd_dir = proc_root.join(pid.to_string()).join("fd");
    let fdinfo_dir = proc_root.join(pid.to_string()).join("fdinfo");
    fs::create_dir_all(&fd_dir).unwrap();
    fs::create_dir_all(&fdinfo_dir).unwrap();
    let dev_target = proc_root.join("_dri").join(dri_basename);
    fs::create_dir_all(dev_target.parent().unwrap()).unwrap();
    fs::write(&dev_target, b"").unwrap();
    // A later fixture may reuse a (pid, fd) of an earlier one when several
    // are built into one tree; the later one wins.
    let link = fd_dir.join(fd.to_string());
    let _ = fs::remove_file(&link);
    std::os::unix::fs::symlink(&dev_target, &link).unwrap();
    fs::write(fdinfo_dir.join(fd.to_string()), fdinfo_content).unwrap();
}

type Fixture = (&'static str, fn(&Path));

/// The procfs layouts the per-process and per-card tests in `tests.rs`
/// build, one entry per scenario, plus one tree holding all of them at once.
fn fixtures() -> Vec<Fixture> {
    vec![
        ("basic: one pid, one client", |root| {
            make_proc_fd(
                root,
                4242,
                3,
                "renderD128",
                "drm-driver: i915\ndrm-client-id: 7\ndrm-resident-system: 1024 kB\n",
            );
        }),
        ("one client duplicated across fds of a pid", |root| {
            let fdinfo = "drm-driver: xe\ndrm-client-id: 9\ndrm-resident-gtt: 512 kB\ndrm-resident-vram0: 2048 kB\n";
            make_proc_fd(root, 100, 3, "renderD128", fdinfo);
            make_proc_fd(root, 100, 4, "renderD128", fdinfo);
            make_proc_fd(root, 100, 5, "card0", fdinfo);
        }),
        ("distinct clients in one pid", |root| {
            make_proc_fd(
                root,
                200,
                3,
                "renderD128",
                "drm-driver: xe\ndrm-client-id: 1\ndrm-resident-vram0: 100 kB\n",
            );
            make_proc_fd(
                root,
                200,
                4,
                "renderD128",
                "drm-driver: xe\ndrm-client-id: 2\ndrm-resident-vram0: 300 kB\ndrm-resident-gtt: 50 kB\n",
            );
        }),
        ("two cards", |root| {
            make_proc_fd(
                root,
                300,
                3,
                "renderD128",
                "drm-driver: xe\ndrm-client-id: 1\ndrm-resident-vram0: 4096 kB\n",
            );
            make_proc_fd(
                root,
                300,
                4,
                "renderD129",
                "drm-driver: xe\ndrm-client-id: 1\ndrm-resident-vram0: 8192 kB\n",
            );
            make_proc_fd(
                root,
                301,
                3,
                "renderD129",
                "drm-driver: i915\ndrm-client-id: 5\ndrm-resident-local0: 16 kB\n",
            );
        }),
        ("fork-inherited fd: one client in two pids", |root| {
            let fdinfo = "drm-driver: xe\ndrm-client-id: 7\ndrm-resident-vram0: 65536 kB\n";
            make_proc_fd(root, 4242, 3, "renderD128", fdinfo);
            make_proc_fd(root, 4243, 3, "renderD128", fdinfo);
        }),
        ("fds without a client id", |root| {
            let fdinfo = "drm-driver: xe\ndrm-resident-vram0: 1000 kB\n";
            make_proc_fd(root, 500, 3, "renderD128", fdinfo);
            make_proc_fd(root, 500, 4, "renderD128", fdinfo);
            make_proc_fd(
                root,
                500,
                5,
                "renderD128",
                "drm-driver: xe\ndrm-client-id: 3\ndrm-resident-vram0: 10 kB\n",
            );
        }),
        ("foreign driver, non-DRM fd, pid without DRM fds", |root| {
            make_proc_fd(
                root,
                600,
                3,
                "renderD128",
                "drm-driver: amdgpu\ndrm-client-id: 1\ndrm-resident-vram0: 999 kB\n",
            );
            make_proc_fd(root, 600, 4, "renderD128", "pos:\t0\nflags:\t02\n");
            make_proc_fd(root, 601, 3, "null", "pos:\t0\n");
            make_proc_fd(
                root,
                602,
                3,
                "renderD128",
                "drm-driver: xe\ndrm-client-id: 4\ndrm-resident-gtt: 64 kB\n",
            );
        }),
        ("same client id, differing values (max wins)", |root| {
            make_proc_fd(
                root,
                700,
                3,
                "renderD128",
                "drm-driver: xe\ndrm-client-id: 8\ndrm-resident-vram0: 100 kB\n",
            );
            make_proc_fd(
                root,
                700,
                4,
                "renderD128",
                "drm-driver: xe\ndrm-client-id: 8\ndrm-resident-vram0: 700 kB\n",
            );
            make_proc_fd(
                root,
                701,
                9,
                "renderD128",
                "drm-driver: xe\ndrm-client-id: 8\ndrm-resident-vram0: 400 kB\n",
            );
        }),
        ("empty procfs", |_root| {}),
    ]
}

fn basenames() -> HashMap<String, usize> {
    HashMap::from([
        ("card0".to_string(), 0_usize),
        ("renderD128".to_string(), 0_usize),
        ("card1".to_string(), 1_usize),
        ("renderD129".to_string(), 1_usize),
    ])
}

fn assert_folds_match_the_legacy_walkers(label: &str, proc_root: &Path) {
    let basenames = basenames();
    let records = scan_intel_drm_fds(&basenames, proc_root);

    assert_eq!(
        fold_process_usage(&records),
        legacy_collect_intel_gpu_processes(&basenames, proc_root),
        "per-process rows differ on fixture: {label}"
    );
    assert_eq!(
        fold_vram_by_card(&records),
        legacy_sum_vram_by_card_from_fdinfo(&basenames, proc_root),
        "per-card VRAM differs on fixture: {label}"
    );
}

#[test]
fn folds_over_one_scan_equal_the_two_separate_walkers() {
    for (label, build) in fixtures() {
        let dir = tempfile::tempdir().unwrap();
        build(dir.path());
        assert_folds_match_the_legacy_walkers(label, dir.path());
    }
}

#[test]
fn folds_equal_the_walkers_on_every_fixture_combined() {
    // All scenarios in one tree: pids repeat across fixtures (4242, for
    // one), so groups merge in ways no single scenario exercises.
    let dir = tempfile::tempdir().unwrap();
    for (_label, build) in fixtures() {
        build(dir.path());
    }
    assert_folds_match_the_legacy_walkers("all fixtures combined", dir.path());
}

#[test]
fn fork_inherited_client_is_credited_per_pid_but_once_per_card() {
    let dir = tempfile::tempdir().unwrap();
    let fdinfo = "drm-driver: xe\ndrm-client-id: 7\ndrm-resident-vram0: 65536 kB\n";
    make_proc_fd(dir.path(), 4242, 3, "renderD128", fdinfo);
    make_proc_fd(dir.path(), 4243, 3, "renderD128", fdinfo);

    let records = scan_intel_drm_fds(&basenames(), dir.path());
    assert_eq!(records.len(), 2);

    let per_process = fold_process_usage(&records);
    assert_eq!(per_process.len(), 2);
    assert!(
        per_process
            .iter()
            .all(|usage| usage.used_memory_bytes == 65_536 * 1024)
    );
    assert_eq!(
        fold_vram_by_card(&records).get(&0).copied(),
        Some(65_536 * 1024)
    );
}

#[test]
fn scan_records_carry_both_memory_figures() {
    let dir = tempfile::tempdir().unwrap();
    make_proc_fd(
        dir.path(),
        42,
        3,
        "renderD129",
        "drm-driver: xe\ndrm-client-id: 11\ndrm-resident-gtt: 512 kB\ndrm-resident-vram0: 2048 kB\n",
    );

    let records = scan_intel_drm_fds(&basenames(), dir.path());
    assert_eq!(
        records,
        vec![DrmFdRecord {
            pid: 42,
            card_index: 1,
            drm_client_id: Some(11),
            resident_bytes: (512 + 2048) * 1024,
            resident_vram_bytes: 2048 * 1024,
            fdinfo_path: dir.path().join("42").join("fdinfo").join("3"),
        }]
    );
}

#[test]
fn empty_basenames_scan_nothing() {
    let dir = tempfile::tempdir().unwrap();
    make_proc_fd(
        dir.path(),
        42,
        3,
        "renderD128",
        "drm-driver: xe\ndrm-client-id: 1\ndrm-resident-vram0: 1 kB\n",
    );
    assert!(scan_intel_drm_fds(&HashMap::new(), dir.path()).is_empty());
}
