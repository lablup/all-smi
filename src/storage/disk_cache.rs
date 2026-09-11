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

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, TryRecvError};
use std::time::{Duration, Instant};

use sysinfo::{DiskRefreshKind, Disks};

use crate::storage::info::StorageInfo;
use crate::utils::filter_docker_aware_disks;

/// How often the mount table is enumerated again.
pub const LIST_REFRESH_INTERVAL: Duration = Duration::from_secs(30);

/// How long the first call of a [`DiskCache::new`] cache waits for the
/// initial list. Enumeration normally takes tens of milliseconds; a volume
/// that hangs longer than this leaves the first rows empty until the
/// background enumeration finishes, instead of stalling the caller.
/// [`DiskCache::with_blocking_first_list`] does not use this bound.
const INITIAL_LIST_WAIT: Duration = Duration::from_secs(2);

/// Filesystem types whose capacity is not re-read between list refreshes.
const NETWORK_FILE_SYSTEMS: &[&str] = &[
    // macOS
    "smbfs",
    "afpfs",
    "webdav",
    "nfs",
    // Linux
    "nfs4",
    "cifs",
    "smb3",
    "9p",
    "ceph",
    "glusterfs",
    "fuse.glusterfs",
    "fuse.sshfs",
    "lustre",
];

fn is_network_file_system(file_system: &str) -> bool {
    NETWORK_FILE_SYSTEMS.contains(&file_system)
}

/// What one list refresh recorded about a macOS volume's available space.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Anchor {
    /// Available space the fresh list reported (purgeable space counted as
    /// free).
    available: u64,
    /// `statfs` free space right after the list was built.
    free: u64,
}

/// Available space now, from `anchor` and `statfs` free space now.
///
/// Moves the anchored value by exactly the change in `statfs` free space
/// since the anchor, saturating at zero and clamped to `total`.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn anchored_available(anchor: Anchor, free_now: u64, total: u64) -> u64 {
    let available = if free_now >= anchor.free {
        anchor.available.saturating_add(free_now - anchor.free)
    } else {
        anchor.available.saturating_sub(anchor.free - free_now)
    };
    available.min(total)
}

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
}

/// One enumeration of the mount table and the rows it produces.
struct Listing {
    disks: Disks,
    volumes: Vec<Volume>,
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
        Self { disks, volumes }
    }

    /// Bring the capacity of every shown volume up to date.
    fn refresh_capacity(&mut self) {
        for volume in self.volumes.iter_mut().filter(|volume| !volume.network) {
            let Some(disk) = self.disks.list_mut().get_mut(volume.index) else {
                continue;
            };

            #[cfg(target_os = "macos")]
            if let Some(anchor) = volume.anchor
                && let Some(free_now) = statfs_free_bytes(disk.mount_point())
            {
                volume.available_bytes = anchored_available(anchor, free_now, volume.total_bytes);
            }

            #[cfg(not(target_os = "macos"))]
            {
                disk.refresh_specifics(DiskRefreshKind::nothing().with_storage());
                volume.total_bytes = disk.total_space();
                volume.available_bytes = disk.available_space();
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
            })
        })
        .collect()
}

#[cfg(target_os = "macos")]
fn anchor_for(disk: &sysinfo::Disk) -> Option<Anchor> {
    Some(Anchor {
        available: disk.available_space(),
        free: statfs_free_bytes(disk.mount_point())?,
    })
}

#[cfg(not(target_os = "macos"))]
fn anchor_for(_disk: &sysinfo::Disk) -> Option<Anchor> {
    None
}

/// Free space `statfs` reports for the volume mounted at `mount_point`.
///
/// `None` when the call fails or when `mount_point` is no longer a mount
/// point. A volume unmounted from a directory that stays behind would
/// otherwise report the free space of the filesystem holding that directory,
/// moving the anchored value by an unrelated amount until the next list
/// drops the row.
#[cfg(target_os = "macos")]
fn statfs_free_bytes(mount_point: &std::path::Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;

    let path = std::ffi::CString::new(mount_point.as_os_str().as_bytes()).ok()?;
    let mut stat = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: `path` is a NUL-terminated string that outlives the call, and
    // `stat` is a writable `statfs` that the call fills when it succeeds.
    if unsafe { libc::statfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return None;
    }
    // SAFETY: `statfs` returned 0, so it initialized `stat`.
    let stat = unsafe { stat.assume_init() };
    // SAFETY: the kernel NUL-terminates `f_mntonname` inside its fixed-size
    // array, which lives as long as `stat`.
    let mounted_on = unsafe { std::ffi::CStr::from_ptr(stat.f_mntonname.as_ptr()) };
    if mounted_on != path.as_c_str() {
        return None;
    }
    Some(stat.f_bavail.saturating_mul(u64::from(stat.f_bsize)))
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
    /// When the most recent enumeration started.
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
            enumerated_at: None,
            waited_for_first_list: false,
            first_list_wait,
        }
    }

    /// Storage rows for this tick.
    pub fn storage_info(&mut self, hostname: &str) -> Vec<StorageInfo> {
        self.update_listing();
        let Some(listing) = self.listing.as_mut() else {
            return Vec::new();
        };
        listing.refresh_capacity();

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

    /// Start a list refresh when one is due and swap in one that finished.
    fn update_listing(&mut self) {
        if self.listing.is_none() && self.first_list_wait.is_none() {
            // No bound on the first list: build it here, as a direct
            // enumeration would. Later lists come from the background.
            self.enumerated_at = Some(Instant::now());
            self.listing = Some(Listing::enumerate());
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
                self.listing = Some(Listing::enumerate());
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
                self.listing = Some(listing);
                self.pending = None;
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
