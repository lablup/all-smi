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

//! The one `/proc` walk behind both Intel fdinfo consumers.
//!
//! The per-process rows and the card-level VRAM fallback read the same
//! files: every `/proc/<pid>/fd` entry is `readlink`ed to find Intel DRM
//! fds, and each match's `fdinfo` is parsed. They differ only in how the
//! parsed blocks are grouped and deduplicated. [`scan_intel_drm_fds`] does
//! the walk once and returns one [`DrmFdRecord`] per parsed fd;
//! [`fold_process_usage`] and [`fold_vram_by_card`] are the two groupings,
//! so a caller that needs both in one tick pays for one walk.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use super::io::read_fdinfo_to_string;
use super::{GpuProcessUsage, MAX_GPU_PROCESSES, intel_drm_fds_for_pid, parse_fdinfo};

/// One Intel DRM fd whose `fdinfo` parsed, as seen by one walk of `/proc`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrmFdRecord {
    pub pid: u32,
    /// Reader-known index of the Intel card the fd points at.
    pub card_index: usize,
    /// `drm-client-id`, `None` on kernels that do not emit it.
    pub drm_client_id: Option<u64>,
    /// Resident bytes of the client, VRAM plus GTT/system.
    pub resident_bytes: u64,
    /// Resident device-local VRAM bytes only.
    pub resident_vram_bytes: u64,
    /// The `/proc/<pid>/fdinfo/<fd>` file this record was parsed from. Keys
    /// the "no client id" bucket of both folds.
    pub fdinfo_path: PathBuf,
}

#[cfg(test)]
thread_local! {
    static SCAN_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Number of [`scan_intel_drm_fds`] walks started on the calling thread.
/// Per thread, so tests running in parallel do not see each other's scans.
#[cfg(test)]
pub(crate) fn scans_on_this_thread() -> usize {
    SCAN_COUNT.with(|count| count.get())
}

/// Walk `/proc` once and return a record for every Intel DRM fd whose
/// `fdinfo` could be read and parsed.
///
/// Fds whose `fdinfo` is unreadable (permission, process exit, oversized) or
/// belongs to a foreign DRM driver yield no record. At most
/// [`MAX_GPU_PROCESSES`] processes holding an Intel DRM fd are visited.
///
/// Cost: O(`processes` * `fds_per_process`), two `read_dir` and one
/// `read_link` per fd. On a busy host with 1k processes and ~10 fds each
/// this is well under 10 ms.
pub fn scan_intel_drm_fds(
    intel_drm_basenames: &HashMap<String, usize>,
    proc_root: &Path,
) -> Vec<DrmFdRecord> {
    #[cfg(test)]
    SCAN_COUNT.with(|count| count.set(count.get() + 1));

    let mut records = Vec::new();
    // Empty card map means no Intel GPUs were enumerated; nothing to
    // do. This is the steady-state for an AMD-only or NVIDIA-only host.
    if intel_drm_basenames.is_empty() {
        return records;
    }
    let Ok(entries) = std::fs::read_dir(proc_root) else {
        return records;
    };

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
            records.push(DrmFdRecord {
                pid,
                card_index: fd.card_index,
                drm_client_id: info.drm_client_id,
                resident_bytes: info.resident_bytes,
                resident_vram_bytes: info.resident_vram_bytes,
                fdinfo_path: fd.fdinfo_path,
            });
        }
    }
    records
}

/// Per-process resident memory: one [`GpuProcessUsage`] per `(pid, card)`
/// that has at least one record.
///
/// Within a `(pid, card)` group, fds sharing a `drm-client-id` are one
/// client and count once, at the largest resident value seen (the block can
/// race with the kernel's writer, and the largest is the most recent
/// complete snapshot). Distinct client ids sum. An fd without a client id
/// cannot be deduplicated and is credited on its own, keyed by its fdinfo
/// path. A client inherited across a fork is credited to both pids.
pub fn fold_process_usage(records: &[DrmFdRecord]) -> Vec<GpuProcessUsage> {
    type Key = (u32, usize);
    let mut per_client: HashMap<Key, HashMap<u64, u64>> = HashMap::new();
    let mut no_client_id: HashMap<Key, HashMap<&Path, u64>> = HashMap::new();

    for record in records {
        let key = (record.pid, record.card_index);
        let slot = match record.drm_client_id {
            Some(client_id) => per_client
                .entry(key)
                .or_default()
                .entry(client_id)
                .or_insert(0),
            None => no_client_id
                .entry(key)
                .or_default()
                .entry(record.fdinfo_path.as_path())
                .or_insert(0),
        };
        *slot = (*slot).max(record.resident_bytes);
    }

    let keys: HashSet<Key> = per_client
        .keys()
        .chain(no_client_id.keys())
        .copied()
        .collect();

    let mut out: Vec<GpuProcessUsage> = keys
        .into_iter()
        .map(|(pid, card_index)| {
            let by_client = per_client.get(&(pid, card_index));
            let by_fd = no_client_id.get(&(pid, card_index));
            let total = by_client
                .into_iter()
                .flat_map(|clients| clients.values())
                .chain(by_fd.into_iter().flat_map(|fds| fds.values()))
                .fold(0_u64, |total, bytes| total.saturating_add(*bytes));
            GpuProcessUsage {
                pid,
                card_index,
                used_memory_bytes: total,
            }
        })
        .collect();

    // Stable ordering keeps downstream consumers' output deterministic
    // across refreshes: PID-major, then by card index.
    out.sort_by_key(|u| (u.pid, u.card_index));
    out
}

/// Resident VRAM bytes per card.
///
/// Deduplication intentionally differs from [`fold_process_usage`]:
/// `drm-client-id` is unique per DRM device open, so each client counts
/// once per card (largest value across every fd referencing it, in any
/// process). Crediting a fork-inherited fd to both pids is right for
/// per-process rows but would double count a card-level total. Fds without
/// a client id are credited per fdinfo path.
pub fn fold_vram_by_card(records: &[DrmFdRecord]) -> HashMap<usize, u64> {
    let mut per_client: HashMap<usize, HashMap<u64, u64>> = HashMap::new();
    let mut no_client_id: HashMap<usize, HashMap<&Path, u64>> = HashMap::new();

    for record in records {
        let slot = match record.drm_client_id {
            Some(client_id) => per_client
                .entry(record.card_index)
                .or_default()
                .entry(client_id)
                .or_insert(0),
            None => no_client_id
                .entry(record.card_index)
                .or_default()
                .entry(record.fdinfo_path.as_path())
                .or_insert(0),
        };
        *slot = (*slot).max(record.resident_vram_bytes);
    }

    let mut out: HashMap<usize, u64> = HashMap::new();
    for (card_index, bytes) in per_client
        .iter()
        .flat_map(|(card, clients)| clients.values().map(move |bytes| (*card, *bytes)))
        .chain(
            no_client_id
                .iter()
                .flat_map(|(card, fds)| fds.values().map(move |bytes| (*card, *bytes))),
        )
    {
        let slot = out.entry(card_index).or_insert(0);
        *slot = slot.saturating_add(bytes);
    }
    out
}
