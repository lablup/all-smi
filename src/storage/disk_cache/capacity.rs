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

//! The per-tick capacity refresh, run on a worker thread with a time budget.
//!
//! A `statfs`/`statvfs` against a volume that stopped answering blocks for
//! as long as the kernel lets it, and only the filesystem types on
//! [`NETWORK_FILE_SYSTEMS`](super::volume::NETWORK_FILE_SYSTEMS) are exempt from the
//! per-tick refresh. Running the refresh on the caller's thread therefore
//! stalled the whole tick, in the view collector, the API loop and
//! [`LocalStorageReader`](crate::storage::LocalStorageReader) alike. So the
//! refresh runs on one worker thread per [`DiskCache`](super::DiskCache):
//! each tick hands it the listing's `Disks` together with what it needs to
//! know about each shown volume ([`CapacityJob`]), waits at most
//! [`CAPACITY_REFRESH_BUDGET`] for the answer ([`CapacityResult`]), and when
//! the answer does not arrive in time reports the previous values and moves
//! on. A refresh that is still running is never joined by a second one: the
//! next tick simply checks whether it has finished. A later list refresh
//! brings its own `Disks`, so a result from before it is discarded together
//! with the `Disks` it carries (the [`generation`](CapacityJob::generation)
//! no longer matches).
//!
//! On Linux each volume also records the device its mount point was on when
//! the list was built. A volume unmounted between list refreshes leaves its
//! directory behind on the parent filesystem, and a plain `statvfs` on that
//! directory would report the parent's capacity for up to
//! [`LIST_REFRESH_INTERVAL`](super::LIST_REFRESH_INTERVAL); when the device
//! no longer matches the previous values are kept, the same outcome the
//! macOS path gets from its `f_mntonname` check.

use std::sync::mpsc::{self, Receiver, Sender};

use super::volume::Anchor;
#[cfg(target_os = "macos")]
use super::volume::{anchored_available, statfs_free_bytes};

#[cfg(not(target_os = "macos"))]
use sysinfo::DiskRefreshKind;
use sysinfo::Disks;

/// How long one tick waits for the capacity refresh before reporting the
/// previous values. A refresh normally takes well under a millisecond.
pub const CAPACITY_REFRESH_BUDGET: std::time::Duration = std::time::Duration::from_millis(50);

/// What the worker needs to refresh one shown volume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CapacityRequest {
    /// Position of the volume in the listing's rows.
    pub(super) volume: usize,
    /// Position of the volume in the listing's `Disks`.
    pub(super) disk: usize,
    /// The volume's total capacity, which the macOS path keeps from the
    /// list.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(super) total_bytes: u64,
    /// The macOS anchor recorded at list time.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(super) anchor: Option<Anchor>,
    /// On Linux, the device the mount point was on at list time.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub(super) device: Option<u64>,
}

/// One tick's refresh, handed to the worker.
pub(super) struct CapacityJob {
    /// The listing the job belongs to, so a result that arrives after the
    /// list was replaced can be recognized and discarded.
    pub(super) generation: u64,
    /// The listing's `Disks`, lent to the worker for the refresh and
    /// returned with the result.
    pub(super) disks: Disks,
    pub(super) requests: Vec<CapacityRequest>,
}

/// What the worker sends back.
pub(super) struct CapacityResult {
    pub(super) generation: u64,
    pub(super) disks: Disks,
    /// `(volume position, total bytes, available bytes)` for each volume the
    /// refresh could read. Volumes it could not are left out and keep their
    /// previous values.
    pub(super) capacities: Vec<(usize, u64, u64)>,
}

/// Refresh the capacity of every requested volume in `disks`.
///
/// This is the whole of what the worker thread does per job; it is a plain
/// function so the tests can run it on the calling thread.
pub(super) fn refresh_capacities(
    disks: &mut Disks,
    requests: &[CapacityRequest],
) -> Vec<(usize, u64, u64)> {
    let mut capacities = Vec::with_capacity(requests.len());
    for request in requests {
        let Some(disk) = disks.list_mut().get_mut(request.disk) else {
            continue;
        };

        #[cfg(target_os = "macos")]
        if let Some(anchor) = request.anchor
            && let Some(free_now) = statfs_free_bytes(disk.mount_point())
        {
            capacities.push((
                request.volume,
                request.total_bytes,
                anchored_available(anchor, free_now, request.total_bytes),
            ));
        }

        #[cfg(not(target_os = "macos"))]
        {
            #[cfg(target_os = "linux")]
            if !mount_identity_matches(request.device, disk.mount_point()) {
                continue;
            }
            disk.refresh_specifics(DiskRefreshKind::nothing().with_storage());
            capacities.push((request.volume, disk.total_space(), disk.available_space()));
        }
    }
    capacities
}

/// The device holding `path`, as `st_dev` reports it.
#[cfg(target_os = "linux")]
pub(super) fn mount_device(path: &std::path::Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;

    std::fs::metadata(path).ok().map(|metadata| metadata.dev())
}

/// Whether the filesystem mounted at `mount_point` is still the one whose
/// device was `recorded` when the list was built.
///
/// A volume unmounted since then leaves `mount_point` on the parent
/// filesystem, whose device differs. When no device could be recorded the
/// mount is refreshed as it always was, since there is nothing to compare
/// against.
#[cfg(target_os = "linux")]
pub(super) fn mount_identity_matches(recorded: Option<u64>, mount_point: &std::path::Path) -> bool {
    match recorded {
        Some(device) => mount_device(mount_point) == Some(device),
        None => true,
    }
}

/// The worker thread and the channels to it.
pub(super) struct CapacityWorker {
    pub(super) jobs: Sender<CapacityJob>,
    pub(super) results: Receiver<CapacityResult>,
    /// Whether a job has been sent whose result has not been received.
    pub(super) in_flight: bool,
}

impl CapacityWorker {
    /// Start the worker thread. `None` when no thread could be started.
    pub(super) fn spawn() -> Option<Self> {
        let (jobs, job_receiver) = mpsc::channel::<CapacityJob>();
        let (result_sender, results) = mpsc::channel::<CapacityResult>();
        std::thread::Builder::new()
            .name("all-smi-disk-capacity".to_string())
            .spawn(move || {
                // Ends when the cache that owns the job sender is dropped.
                while let Ok(mut job) = job_receiver.recv() {
                    let capacities = refresh_capacities(&mut job.disks, &job.requests);
                    let result = CapacityResult {
                        generation: job.generation,
                        disks: job.disks,
                        capacities,
                    };
                    if result_sender.send(result).is_err() {
                        break;
                    }
                }
            })
            .ok()?;
        Some(Self {
            jobs,
            results,
            in_flight: false,
        })
    }
}
