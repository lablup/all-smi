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

//! Disk list shared by every storage collection path.
//!
//! Enumerating the mount table is what storage collection costs. On macOS
//! `Disks::new_with_refreshed_list` runs `getfsstat` and asks CoreFoundation
//! for every volume's properties, network shares included, which took 20 to
//! 37 ms per tick on an M5 Max, and `Disks::refresh` does the same work.
//! [`DiskCache`] enumerates once up front and then at most every
//! [`LIST_REFRESH_INTERVAL`], on a background thread whose result is swapped
//! in when it is ready, so a hung mount never holds up a tick. Between list
//! refreshes each tick refreshes only the capacity of the volumes it shows.
//! A volume mounted in the meantime appears with the next list.
//!
//! ## Available space between list refreshes on macOS
//!
//! sysinfo's per-disk refresh does not work for this on macOS. It re-reads
//! the `CFURL` it created when it built the list, CFURL caches resource
//! values on the URL object, and so it returns the list-time value forever
//! (a 1 GB write left it unchanged). A fresh read of the value sysinfo
//! reports, `kCFURLVolumeAvailableCapacityForImportantUsageKey`, which counts
//! purgeable space as free, costs 6 to 15 ms per volume. `statfs` costs about
//! a microsecond but leaves purgeable space out (91.16 GB against 101.28 GB
//! on the same volume).
//!
//! So every list refresh records an anchor per volume: the exact available
//! space from the fresh list, and `statfs` free space taken right after it.
//! Each tick reports the anchor moved by exactly as much as `statfs` free
//! space has moved since, never below zero and never above the volume's total
//! (see [`anchored_available`]). Writes and deletes show on the next tick.
//! Changes in purgeable space, such as local snapshots or caches the system
//! may reclaim, surface at the next anchor, at most [`LIST_REFRESH_INTERVAL`]
//! later. At an anchor the value is exactly what a fresh list reports.
//!
//! On other platforms the per-disk refresh runs a live `statvfs`, so it is
//! used on every tick as it is.
//!
//! ## The first list
//!
//! A cache has no list until its first enumeration finishes, and the two
//! kinds of caller need different things from that first call:
//!
//! - [`DiskCache::new`], used by the view and API collection loops, waits at
//!   most 2 s for the first list and returns no rows if it is not ready by
//!   then, so a mount that hangs at startup cannot stall a tick. The rows
//!   appear on the first tick after the enumeration finishes.
//! - [`DiskCache::with_blocking_first_list`], used by
//!   [`LocalStorageReader`](crate::storage::LocalStorageReader), enumerates
//!   the first list on the calling thread and returns it however long that
//!   takes, the same contract `Disks::new_with_refreshed_list` gives. A
//!   library caller's first call is never empty just because enumeration
//!   was slow.
//!
//! After the first list both behave the same: later lists are enumerated on
//! a background thread and never waited for.
//!
//! ## Network filesystems
//!
//! Volumes on [`NETWORK_FILE_SYSTEMS`] keep the capacity from the last list
//! refresh instead of being queried on the tick: a `statfs` against a hung
//! share can block indefinitely, and a value up to 30 s old is acceptable for
//! them. (sysinfo already leaves non-local volumes out of the list on macOS,
//! and CIFS/NFS out of it on Linux, but other network filesystems get in.)
//!
//! ## The per-tick refresh is bounded (issue #414)
//!
//! Any other volume that stops answering would still have stalled the tick,
//! so the per-tick capacity refresh runs on one worker thread and a tick
//! waits at most [`CAPACITY_REFRESH_BUDGET`] for it, reporting the previous
//! values when it does not finish in time. See [`capacity`] for the
//! protocol, and for the Linux mount-identity check that keeps a volume
//! unmounted between list refreshes from reporting its parent's capacity.

mod capacity;
mod volume;

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, TryRecvError};
use std::time::{Duration, Instant};

use sysinfo::{DiskRefreshKind, Disks};

use crate::storage::info::StorageInfo;
use crate::utils::filter_docker_aware_disks;

pub use capacity::CAPACITY_REFRESH_BUDGET;
use capacity::{CapacityJob, CapacityRequest, CapacityResult, CapacityWorker, refresh_capacities};
use volume::{Anchor, anchor_for, device_for, is_network_file_system};

/// How often the mount table is enumerated again.
pub const LIST_REFRESH_INTERVAL: Duration = Duration::from_secs(30);

/// How long the first call of a [`DiskCache::new`] cache waits for the
/// initial list. Enumeration normally takes tens of milliseconds; a volume
/// that hangs longer than this leaves the first rows empty until the
/// background enumeration finishes, instead of stalling the caller.
/// [`DiskCache::with_blocking_first_list`] does not use this bound.
const INITIAL_LIST_WAIT: Duration = Duration::from_secs(2);

/// One row the storage panel shows, in display order.
struct Volume {
    /// Position of the volume in the listing's `Disks`.
    index: usize,
    mount_point: String,
    network: bool,
    total_bytes: u64,
    available_bytes: u64,
    /// `None` when `statfs` failed at list time; the list value then stands
    /// until the next list refresh.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    anchor: Option<Anchor>,
    /// On Linux, the device the mount point was on at list time (see
    /// [`capacity`]); `None` elsewhere or when it could not be read.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    device: Option<u64>,
}

/// One enumeration of the mount table and the rows it produces.
struct Listing {
    /// `None` while the capacity worker holds them for a refresh.
    disks: Option<Disks>,
    volumes: Vec<Volume>,
    /// Assigned when the listing is installed in a cache; a capacity result
    /// carrying another generation belongs to a replaced list.
    generation: u64,
}

impl Listing {
    /// Enumerate the mount table and work out the rows shown from it.
    ///
    /// Storage capacity is the only refresh kind requested: nothing reads a
    /// disk's kind or I/O counters, and both cost extra IOKit calls.
    fn enumerate() -> Self {
        let disks =
            Disks::new_with_refreshed_list_specifics(DiskRefreshKind::nothing().with_storage());
        let volumes = shown_volumes(&disks);
        Self {
            disks: Some(disks),
            volumes,
            generation: 0,
        }
    }

    /// What the worker needs for each volume whose capacity is refreshed on
    /// the tick.
    fn capacity_requests(&self) -> Vec<CapacityRequest> {
        self.volumes
            .iter()
            .enumerate()
            .filter(|(_, volume)| !volume.network)
            .map(|(position, volume)| CapacityRequest {
                volume: position,
                disk: volume.index,
                total_bytes: volume.total_bytes,
                anchor: volume.anchor,
                device: volume.device,
            })
            .collect()
    }

    /// Take the capacities of a finished refresh into the rows.
    fn apply_capacities(&mut self, capacities: &[(usize, u64, u64)]) {
        for &(position, total_bytes, available_bytes) in capacities {
            if let Some(volume) = self.volumes.get_mut(position) {
                volume.total_bytes = total_bytes;
                volume.available_bytes = available_bytes;
            }
        }
    }
}

/// The rows shown from `disks`: Docker-aware filtered, sorted by mount point,
/// exactly as storage collection has always selected them.
fn shown_volumes(disks: &Disks) -> Vec<Volume> {
    let mut shown = filter_docker_aware_disks(disks);
    shown.sort_by(|a, b| {
        a.mount_point()
            .to_string_lossy()
            .cmp(&b.mount_point().to_string_lossy())
    });

    shown
        .into_iter()
        .filter_map(|disk| {
            let index = disks.list().iter().position(|d| std::ptr::eq(d, disk))?;
            let network = is_network_file_system(&disk.file_system().to_string_lossy());
            Some(Volume {
                index,
                mount_point: disk.mount_point().to_string_lossy().into_owned(),
                network,
                total_bytes: disk.total_space(),
                available_bytes: disk.available_space(),
                anchor: if network { None } else { anchor_for(disk) },
                device: if network { None } else { device_for(disk) },
            })
        })
        .collect()
}

/// Start enumerating on a background thread. `None` when no thread could be
/// started.
fn spawn_enumeration() -> Option<Receiver<Listing>> {
    let (sender, receiver) = mpsc::channel();
    std::thread::Builder::new()
        .name("all-smi-disk-list".to_string())
        .spawn(move || {
            // The receiver may be gone by the time a slow enumeration ends.
            let _ = sender.send(Listing::enumerate());
        })
        .ok()?;
    Some(receiver)
}

/// Disk list and per-tick capacity for one storage collection path.
///
/// The view and API collection loops and [`LocalStorageReader`] each own one
/// and call [`storage_info`](Self::storage_info) once per tick. Rows are
/// selected, ordered and numbered exactly as `Disks::new_with_refreshed_list`
/// plus `filter_docker_aware_disks` always did; see the module docs for how
/// fresh each field is and how the two constructors differ on the first call.
///
/// [`LocalStorageReader`]: crate::storage::LocalStorageReader
pub struct DiskCache {
    listing: Option<Listing>,
    /// A list refresh running on a background thread.
    pending: Option<Receiver<Listing>>,
    /// The per-tick capacity refresh's worker, started on first use.
    capacity: Option<CapacityWorker>,
    /// The generation the next installed listing gets.
    next_generation: u64,
    /// When the most recent enumeration started, moved to when its list
    /// arrived once it does, so the next enumeration is due
    /// [`LIST_REFRESH_INTERVAL`] after the last list arrived, however long
    /// enumerating took.
    enumerated_at: Option<Instant>,
    /// Whether the bounded wait for the first list has been spent.
    waited_for_first_list: bool,
    /// How long the first call may wait for the first list, or `None` to
    /// build it on the calling thread however long that takes.
    first_list_wait: Option<Duration>,
}

impl Default for DiskCache {
    fn default() -> Self {
        Self::new()
    }
}

impl DiskCache {
    /// An empty cache for a collection loop. The first
    /// [`storage_info`](Self::storage_info) call starts enumerating the mount
    /// table in the background and waits at most 2 s for it; if the list is
    /// not ready by then that call returns no rows, and the rows appear on
    /// the first call after it is.
    pub fn new() -> Self {
        Self::with_first_list_wait(Some(INITIAL_LIST_WAIT))
    }

    /// An empty cache whose first [`storage_info`](Self::storage_info) call
    /// enumerates the mount table on the calling thread and returns the
    /// complete list, however long that takes, as
    /// `Disks::new_with_refreshed_list` does. Later calls behave as with
    /// [`new`](Self::new). [`LocalStorageReader`] uses this so a library
    /// caller never gets an empty first list from a slow enumeration.
    ///
    /// [`LocalStorageReader`]: crate::storage::LocalStorageReader
    pub fn with_blocking_first_list() -> Self {
        Self::with_first_list_wait(None)
    }

    fn with_first_list_wait(first_list_wait: Option<Duration>) -> Self {
        Self {
            listing: None,
            pending: None,
            capacity: None,
            next_generation: 0,
            enumerated_at: None,
            waited_for_first_list: false,
            first_list_wait,
        }
    }

    /// Storage rows for this tick.
    pub fn storage_info(&mut self, hostname: &str) -> Vec<StorageInfo> {
        self.update_listing();
        self.refresh_capacity();
        let Some(listing) = self.listing.as_ref() else {
            return Vec::new();
        };

        listing
            .volumes
            .iter()
            .enumerate()
            .map(|(index, volume)| StorageInfo {
                mount_point: volume.mount_point.clone(),
                total_bytes: volume.total_bytes,
                available_bytes: volume.available_bytes,
                host_id: hostname.to_string(),
                hostname: hostname.to_string(),
                index: index as u32,
            })
            .collect()
    }

    /// Make `listing` the one shown, under a new generation.
    fn install_listing(&mut self, mut listing: Listing) {
        listing.generation = self.next_generation;
        self.next_generation = self.next_generation.wrapping_add(1);
        self.listing = Some(listing);
        self.enumerated_at = Some(Instant::now());
    }

    /// Bring the shown volumes' capacity up to date, within
    /// [`CAPACITY_REFRESH_BUDGET`] (module docs and [`capacity`]).
    ///
    /// One job per tick at most: a tick that finds the previous job still
    /// running only checks whether it has finished, and a tick that receives
    /// a late result does not start another.
    fn refresh_capacity(&mut self) {
        let Some(listing) = self.listing.as_mut() else {
            return;
        };
        let deadline = Instant::now() + CAPACITY_REFRESH_BUDGET;

        let worker = match self.capacity.as_mut() {
            Some(worker) => worker,
            None => match CapacityWorker::spawn() {
                Some(worker) => self.capacity.insert(worker),
                None => {
                    // No thread to run it on: refresh here, unbounded, as
                    // before the worker existed.
                    let requests = listing.capacity_requests();
                    if let Some(disks) = listing.disks.as_mut() {
                        let capacities = refresh_capacities(disks, &requests);
                        listing.apply_capacities(&capacities);
                    }
                    return;
                }
            },
        };

        if !worker.in_flight {
            let Some(disks) = listing.disks.take() else {
                // The `Disks` are with a job that never came back; the next
                // list brings new ones.
                return;
            };
            let job = CapacityJob {
                generation: listing.generation,
                disks,
                requests: listing.capacity_requests(),
            };
            if worker.jobs.send(job).is_err() {
                // The worker thread is gone; start a fresh one next tick.
                self.capacity = None;
                return;
            }
            worker.in_flight = true;
        }

        let wait = deadline.saturating_duration_since(Instant::now());
        match worker.results.recv_timeout(wait) {
            Ok(result) => {
                worker.in_flight = false;
                Self::take_capacity_result(listing, result);
            }
            Err(RecvTimeoutError::Timeout) => {
                tracing::debug!(
                    budget_ms = CAPACITY_REFRESH_BUDGET.as_millis() as u64,
                    "storage capacity refresh exceeded its budget; reporting the previous values"
                );
            }
            Err(RecvTimeoutError::Disconnected) => {
                // The worker thread died with the job. The `Disks` it held
                // are lost until the next list; a fresh worker serves it.
                self.capacity = None;
            }
        }
    }

    /// Apply a finished refresh to `listing`, or discard it when it belongs
    /// to a list that has since been replaced.
    fn take_capacity_result(listing: &mut Listing, result: CapacityResult) {
        if result.generation != listing.generation {
            return;
        }
        listing.disks = Some(result.disks);
        listing.apply_capacities(&result.capacities);
    }

    /// Start a list refresh when one is due and swap in one that finished.
    fn update_listing(&mut self) {
        if self.listing.is_none() && self.first_list_wait.is_none() {
            // No bound on the first list: build it here, as a direct
            // enumeration would. Later lists come from the background.
            self.install_listing(Listing::enumerate());
            return;
        }

        let due = self
            .enumerated_at
            .is_none_or(|started| started.elapsed() >= LIST_REFRESH_INTERVAL);
        if self.pending.is_none() && due {
            self.enumerated_at = Some(Instant::now());
            self.pending = spawn_enumeration();
            if self.pending.is_none() && self.listing.is_none() {
                // No thread to run it on, and no list to show meanwhile.
                self.install_listing(Listing::enumerate());
            }
        }

        let Some(receiver) = self.pending.as_ref() else {
            return;
        };
        let received = match self.first_list_wait {
            // Nothing to show until the first list exists, so wait for it,
            // but only once and only for a bounded time.
            Some(limit) if self.listing.is_none() && !self.waited_for_first_list => {
                self.waited_for_first_list = true;
                receiver.recv_timeout(limit).map_err(|err| match err {
                    RecvTimeoutError::Timeout => TryRecvError::Empty,
                    RecvTimeoutError::Disconnected => TryRecvError::Disconnected,
                })
            }
            _ => receiver.try_recv(),
        };

        match received {
            Ok(listing) => {
                self.pending = None;
                self.install_listing(listing);
            }
            // Still enumerating: keep showing the previous list.
            Err(TryRecvError::Empty) => {}
            // The enumeration thread died. Keep the previous list; the next
            // refresh is due on the usual schedule.
            Err(TryRecvError::Disconnected) => self.pending = None,
        }
    }
}

#[cfg(test)]
#[path = "disk_cache/tests.rs"]
mod tests;
