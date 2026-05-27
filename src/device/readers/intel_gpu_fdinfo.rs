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

//! Per-process GPU memory accounting for Intel client GPUs via DRM
//! client `fdinfo` (`/proc/<pid>/fdinfo/<fd>`).
//!
//! Since Linux 5.19 (i915) and the initial xe upstreaming, the kernel
//! exposes a per-DRM-client memory and engine accounting block in each
//! open file descriptor's `fdinfo` entry. The exact key names differ
//! between drivers but the shape is the same. This module:
//!
//! 1. Parses one fdinfo file into a structured [`FdInfo`].
//! 2. Walks `/proc` to find every PID that has an Intel DRM fd open,
//!    correlates each fd back to a reader-known card index, and returns
//!    the resident-memory total per `(pid, card_index)`.
//!
//! The module is **stateless** — point-in-time memory accounting needs
//! no delta tracker. That keeps `IntelGpuCard` struct shape unchanged
//! and avoids colliding with the Level Zero work tracked in issue #248.
//!
//! Per-process **engine-time** utilization (the stretch goal of issue
//! #247) is intentionally deferred and would live in a sibling
//! delta-tracking module that mirrors `intel_gpu_engine`.
//!
//! ## fdinfo schema cheat-sheet
//!
//! The kernel always emits `drm-driver` and `drm-pdev` for any DRM
//! client. Memory keys vary by driver:
//!
//! - `i915` (Linux >=5.19):
//!   - `drm-resident-system: NNNN kB`  — currently-resident GTT
//!   - `drm-resident-local0: NNNN kB`  — currently-resident VRAM (discrete only)
//! - `xe` (newer Arc / Battlemage / Lunar Lake / Meteor Lake):
//!   - `drm-resident-gtt: NNNN kB`    — currently-resident GTT
//!   - `drm-resident-vram0: NNNN kB`  — currently-resident VRAM (discrete only)
//!
//! Values are in kB (1024 bytes). The kernel emits engine counters as
//! `ns` — we ignore those in v1.
//!
//! ## drm-client-id deduplication
//!
//! A single process can hold many fds to the same DRM client (e.g. via
//! `dup(2)` or by passing the fd across a fork). Each such fd's fdinfo
//! reports the SAME resident-memory block — summing them blindly would
//! double-count by a factor of N. We dedupe by `drm-client-id` within a
//! `(pid, card_index)` group, keeping one entry per distinct client.
//! Multiple distinct clients in the same process (e.g. a multi-context
//! workload) DO sum, since they represent distinct allocations.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

#[path = "intel_gpu_fdinfo/io.rs"]
mod io;
use io::read_fdinfo_to_string;

/// Hard cap on process enumeration. Matches the spirit of
/// `MAX_DEVICES = 256` in [`crate::device::readers::common_cache`] —
/// defends against a runaway `/proc` walk on degenerate hosts. A real
/// Intel-GPU-using workload tops out in the low tens of GPU clients
/// even on heavily containerised hosts.
const MAX_GPU_PROCESSES: usize = 4096;

/// Parsed identity + memory block from one DRM client `fdinfo` file.
///
/// We capture only the fields actually used by callers. Engine-time
/// counters live in `drm-engine-*` keys but are NOT parsed here — the
/// per-process engine-time stretch goal lives in a separate module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FdInfo {
    /// Driver name from `drm-driver:`. Empty when the file is not a
    /// DRM client fd at all.
    pub drm_driver: String,
    /// PCI device address from `drm-pdev:`, e.g. `"0000:03:00.0"`.
    /// `None` when the field is absent (e.g. very old kernels that
    /// shipped fdinfo without `drm-pdev`).
    pub drm_pdev: Option<String>,
    /// Stable per-client identifier from `drm-client-id:`. Used to
    /// dedupe fds that point at the same DRM client. `None` when the
    /// kernel did not emit it (extremely old fdinfo builds).
    pub drm_client_id: Option<u64>,
    /// Sum of currently-resident memory in bytes (VRAM + GTT/system).
    /// We deliberately use the `drm-resident-*` family rather than
    /// `drm-total-*` because the latter counts every allocation that
    /// ever existed, including freed pages. Resident is what `top` and
    /// `intel_gpu_top -p` report.
    pub resident_bytes: u64,
}

/// Parse one fdinfo file's contents.
///
/// Returns `None` if the content does not look like a DRM client fdinfo
/// at all (no `drm-driver:` line) or if the driver is neither `i915`
/// nor `xe`. Returns `Some(FdInfo)` with `resident_bytes = 0` when the
/// kernel is too old to expose the resident-* keys — the caller can
/// still use the entry's `drm_client_id` for dedup and treat the entry
/// as a known-but-unmeasurable client.
///
/// Malformed entries (truncated lines, non-numeric values) are tolerated:
/// the parser skips offending lines, never panics, and returns the best
/// effort recovered from the rest of the file.
pub fn parse_fdinfo(content: &str) -> Option<FdInfo> {
    let mut drm_driver: Option<String> = None;
    let mut drm_pdev: Option<String> = None;
    let mut drm_client_id: Option<u64> = None;
    let mut resident_bytes: u64 = 0;

    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();

        match key {
            "drm-driver" => drm_driver = Some(value.to_string()),
            "drm-pdev" => drm_pdev = Some(value.to_string()),
            "drm-client-id" => drm_client_id = value.parse::<u64>().ok(),
            // Memory accounting — sum across all `drm-resident-*` keys.
            //
            // The kernel emits at most one entry per memory region per
            // driver. Summing covers both schemas without branching:
            //   i915: drm-resident-system + drm-resident-local0
            //   xe:   drm-resident-gtt    + drm-resident-vram0
            // Integrated cards expose only the system / GTT key and
            // the other one is absent, which is exactly what we want
            // (no double counting against system memory).
            k if k.starts_with("drm-resident-") => {
                if let Some(bytes) = parse_memory_value(value) {
                    resident_bytes = resident_bytes.saturating_add(bytes);
                }
            }
            _ => {}
        }
    }

    let drm_driver = drm_driver?;
    // Reject drivers we do not handle. The fdinfo contract is shared
    // (DRM-GEM stat keys) but the per-driver memory key names differ;
    // accepting a foreign driver here would let AMD/NVIDIA processes
    // leak into the Intel reader's output on hybrid hosts.
    if drm_driver != "i915" && drm_driver != "xe" {
        return None;
    }

    Some(FdInfo {
        drm_driver,
        drm_pdev,
        drm_client_id,
        resident_bytes,
    })
}

/// Parse a memory value of the form `"NNNN kB"` (or rare variants:
/// `"NNNN KiB"`, plain bytes). Returns the byte count, or `None` for an
/// unparseable value. Tolerates trailing whitespace and arbitrary case
/// on the unit suffix. The kernel currently always emits `kB`.
fn parse_memory_value(value: &str) -> Option<u64> {
    let mut tokens = value.split_whitespace();
    let number: u64 = tokens.next()?.parse().ok()?;
    let unit = tokens.next().unwrap_or("");
    let multiplier = match unit.to_ascii_lowercase().as_str() {
        // kernel emits "kB" — match that case-insensitively
        "kb" | "kib" => 1024,
        "mb" | "mib" => 1024 * 1024,
        "gb" | "gib" => 1024 * 1024 * 1024,
        "b" | "" => 1,
        _ => return None,
    };
    Some(number.saturating_mul(multiplier))
}

/// Build the lookup from a `/dev/dri/<basename>` device name (e.g.
/// `card0`, `renderD128`) to the Intel card index it belongs to.
///
/// Both the primary (`cardN`) and the render node (`renderD<M>`) for a
/// given PCI device are entered into the map, because user-space
/// processes preferentially open the render node (no master/setmaster
/// permission flow). Without the render-node entries we would miss
/// virtually every modern Vulkan / oneAPI / ffmpeg workload.
///
/// The mapping is built by walking `drm_root` (default
/// `/sys/class/drm`) once and matching each `cardN` / `renderD<M>` to
/// its parent PCI device. Two DRM minors belong to the same card iff
/// the basename of their `device` symlink target matches.
///
/// `intel_cards` is the slice of `(card_path, card_index)` already
/// enumerated by the reader at construction time — passing the slice
/// keeps `IntelGpuCard`'s privacy intact.
pub fn build_intel_drm_basenames(
    intel_cards: &[(PathBuf, usize)],
    drm_root: &Path,
) -> HashMap<String, usize> {
    let mut basenames: HashMap<String, usize> = HashMap::new();

    // Resolve each Intel card's PCI bus identifier (the basename of the
    // `device` symlink target, e.g. `0000:03:00.0`). The cardN node
    // itself is recorded under its own basename.
    let mut pci_to_index: HashMap<String, usize> = HashMap::new();
    for (card_path, idx) in intel_cards {
        if let Some(basename) = card_path.file_name().and_then(|n| n.to_str()) {
            basenames.insert(basename.to_string(), *idx);
        }
        if let Some(bus) = pci_bus_for_drm_node(card_path) {
            pci_to_index.insert(bus, *idx);
        }
    }

    // Walk the DRM tree once to find every `renderD<M>` (and any extra
    // `cardN`) entry. Match by PCI bus to the cards we already know.
    let Ok(entries) = std::fs::read_dir(drm_root) else {
        return basenames;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        // We only care about `cardN` (canonical primary node) and
        // `renderD<M>` (render-only secondary node). Skip connector
        // children like `card0-eDP-1` and the `version` regular file.
        if !is_card_node(name) && !is_render_node(name) {
            continue;
        }
        if basenames.contains_key(name) {
            continue;
        }
        let Some(bus) = pci_bus_for_drm_node(&path) else {
            continue;
        };
        if let Some(idx) = pci_to_index.get(&bus) {
            basenames.insert(name.to_string(), *idx);
        }
    }

    basenames
}

/// Read the PCI bus identifier for a `/sys/class/drm/<node>` entry by
/// resolving its `device` symlink. Returns `None` when the symlink is
/// missing (synthetic fixtures sometimes skip it) or unreadable.
fn pci_bus_for_drm_node(drm_node: &Path) -> Option<String> {
    let link = std::fs::read_link(drm_node.join("device")).ok()?;
    link.file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_string())
}

fn is_card_node(name: &str) -> bool {
    if let Some(rest) = name.strip_prefix("card") {
        !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit())
    } else {
        false
    }
}

fn is_render_node(name: &str) -> bool {
    if let Some(rest) = name.strip_prefix("renderD") {
        !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit())
    } else {
        false
    }
}

/// One entry per Intel-DRM file descriptor a process holds.
///
/// `fd_num` identifies which `/proc/<pid>/fd/<n>` entry produced this
/// record, `fdinfo_path` is the matching `/proc/<pid>/fdinfo/<fd>`
/// file the caller should `read_to_string`, and `card_index` is the
/// reader-known index of the Intel card the fd points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntelDrmFd {
    pub fd_num: u32,
    pub fdinfo_path: PathBuf,
    pub card_index: usize,
}

/// Walk `/proc/<pid>/fd/` looking for fds that point at one of the
/// known Intel DRM nodes.
///
/// Permission errors (`EACCES` for fds owned by another user) are
/// silently skipped per process — we never `eprintln!` per-process
/// noise because a multi-tenant host may have hundreds of foreign
/// processes that legitimately deny enumeration. Top-level errors
/// (`/proc/<pid>/fd/` itself unreadable) yield an empty Vec.
///
/// `intel_drm_basenames` is the map produced by
/// [`build_intel_drm_basenames`]. `proc_root` is normally `/proc`; the
/// parameter exists so tests can drive a synthetic procfs.
pub fn intel_drm_fds_for_pid(
    pid: u32,
    intel_drm_basenames: &HashMap<String, usize>,
    proc_root: &Path,
) -> Vec<IntelDrmFd> {
    let fd_dir = proc_root.join(pid.to_string()).join("fd");
    let Ok(entries) = std::fs::read_dir(&fd_dir) else {
        return Vec::new();
    };

    let mut out: Vec<IntelDrmFd> = Vec::new();
    for entry in entries.flatten() {
        let fd_path = entry.path();
        let Some(fd_name) = fd_path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Ok(fd_num) = fd_name.parse::<u32>() else {
            continue;
        };
        let Ok(target) = std::fs::read_link(&fd_path) else {
            continue;
        };
        let Some(target_name) = target.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(card_index) = intel_drm_basenames.get(target_name).copied() else {
            continue;
        };
        let fdinfo_path = proc_root.join(pid.to_string()).join("fdinfo").join(fd_name);
        out.push(IntelDrmFd {
            fd_num,
            fdinfo_path,
            card_index,
        });
    }
    out
}

/// Per-process resident-memory aggregate, ready to fold into a
/// `ProcessInfo`. One entry per `(pid, card_index)` pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuProcessUsage {
    pub pid: u32,
    pub card_index: usize,
    pub used_memory_bytes: u64,
}

/// Collect Intel-GPU-using processes by walking `/proc`.
///
/// For each PID on the host: scan `/proc/<pid>/fd/`, identify fds that
/// reference a known Intel DRM node, parse the matching
/// `/proc/<pid>/fdinfo/<fd>`, dedupe by `drm-client-id`, and sum the
/// resident bytes across distinct clients per card.
///
/// Returns one [`GpuProcessUsage`] per `(pid, card_index)` pair that
/// has at least one open Intel DRM client.
///
/// Cost-shape note: the walk is O(`processes` * `fds_per_process`)
/// with two `read_dir` and one `read_link` per fd. On a busy host
/// with 1k processes and ~10 fds each this is well under 10ms; matches
/// what the AMD reader does via `libamdgpu_top::FdInfoStat`.
pub fn collect_intel_gpu_processes(
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

#[path = "intel_gpu_fdinfo/enrichment.rs"]
mod enrichment;
pub use enrichment::build_intel_process_infos;

#[cfg(test)]
#[path = "intel_gpu_fdinfo/tests.rs"]
mod tests;
