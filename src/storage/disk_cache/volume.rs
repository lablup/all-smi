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

//! What one shown volume is and how its available space is read between
//! list refreshes: the network filesystem list, the macOS anchor (see the
//! [`disk_cache`](super) module docs), and the mount identity checks.

/// Filesystem types whose capacity is not re-read between list refreshes.
pub(super) const NETWORK_FILE_SYSTEMS: &[&str] = &[
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
    // Cluster filesystems common on GPU cluster nodes
    "gpfs",
    "beegfs",
    "wekafs",
    "panfs",
    "pvfs2",
    "afs",
    // Cloud-backed FUSE filesystems
    "fuse.ceph-fuse",
    "fuse.juicefs",
    "fuse.rclone",
    "fuse.s3fs",
    "fuse.gcsfuse",
];

pub(super) fn is_network_file_system(file_system: &str) -> bool {
    NETWORK_FILE_SYSTEMS.contains(&file_system)
}

/// What one list refresh recorded about a macOS volume's available space.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Anchor {
    /// Available space the fresh list reported (purgeable space counted as
    /// free).
    pub(super) available: u64,
    /// `statfs` free space right after the list was built.
    pub(super) free: u64,
}

/// Available space now, from `anchor` and `statfs` free space now.
///
/// Moves the anchored value by exactly the change in `statfs` free space
/// since the anchor, saturating at zero and clamped to `total`.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(super) fn anchored_available(anchor: Anchor, free_now: u64, total: u64) -> u64 {
    let available = if free_now >= anchor.free {
        anchor.available.saturating_add(free_now - anchor.free)
    } else {
        anchor.available.saturating_sub(anchor.free - free_now)
    };
    available.min(total)
}

#[cfg(target_os = "macos")]
pub(super) fn anchor_for(disk: &sysinfo::Disk) -> Option<Anchor> {
    Some(Anchor {
        available: disk.available_space(),
        free: statfs_free_bytes(disk.mount_point())?,
    })
}

#[cfg(not(target_os = "macos"))]
pub(super) fn anchor_for(_disk: &sysinfo::Disk) -> Option<Anchor> {
    None
}

#[cfg(target_os = "linux")]
pub(super) fn device_for(disk: &sysinfo::Disk) -> Option<u64> {
    super::capacity::mount_device(disk.mount_point())
}

#[cfg(not(target_os = "linux"))]
pub(super) fn device_for(_disk: &sysinfo::Disk) -> Option<u64> {
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
pub(super) fn statfs_free_bytes(mount_point: &std::path::Path) -> Option<u64> {
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
    // `f_mntonname` is a fixed-size array the kernel NUL-terminates. Compare
    // up to the first NUL, bounded by the array, without relying on it.
    let mounted_on = stat
        .f_mntonname
        .iter()
        .map(|&c| c as u8)
        .take_while(|&byte| byte != 0);
    if !mounted_on.eq(path.as_bytes().iter().copied()) {
        return None;
    }
    Some(stat.f_bavail.saturating_mul(u64::from(stat.f_bsize)))
}
