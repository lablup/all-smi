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

use super::capacity::{CapacityResult, CapacityWorker, refresh_capacities};
use super::volume::anchored_available;
#[cfg(target_os = "macos")]
use super::volume::statfs_free_bytes;
use super::*;

const GB: u64 = 1_000_000_000;

/// An anchor as a list refresh would record it: 100 GB available counting
/// purgeable space, 90 GB free by `statfs`, on a 4 TB volume.
const ANCHOR: Anchor = Anchor {
    available: 100 * GB,
    free: 90 * GB,
};
const TOTAL: u64 = 4_000 * GB;

#[test]
fn at_the_anchor_the_list_value_stands() {
    assert_eq!(anchored_available(ANCHOR, 90 * GB, TOTAL), 100 * GB);
}

#[test]
fn a_write_lowers_available_space_by_what_it_wrote() {
    assert_eq!(anchored_available(ANCHOR, 89 * GB, TOTAL), 99 * GB);
}

#[test]
fn a_delete_raises_available_space_by_what_it_freed() {
    assert_eq!(anchored_available(ANCHOR, 95 * GB, TOTAL), 105 * GB);
}

#[test]
fn available_space_never_goes_below_zero() {
    let anchor = Anchor {
        available: GB,
        free: 5 * GB,
    };
    assert_eq!(anchored_available(anchor, 0, TOTAL), 0);
}

#[test]
fn available_space_never_exceeds_the_total() {
    assert_eq!(anchored_available(ANCHOR, 4_500 * GB, TOTAL), TOTAL);
    assert_eq!(anchored_available(ANCHOR, u64::MAX, TOTAL), TOTAL);
}

/// A new list replaces the extrapolation: purgeable space the system
/// reclaimed meanwhile shows up exactly at the next anchor, and later writes
/// move the new anchor.
#[test]
fn a_new_anchor_replaces_the_extrapolation() {
    // 10 GB written since the first anchor.
    assert_eq!(anchored_available(ANCHOR, 80 * GB, TOTAL), 90 * GB);

    // The next list reports 93 GB: 3 GB of purgeable space was freed up.
    let next = Anchor {
        available: 93 * GB,
        free: 80 * GB,
    };
    assert_eq!(anchored_available(next, 80 * GB, TOTAL), 93 * GB);
    assert_eq!(anchored_available(next, 79 * GB, TOTAL), 92 * GB);
}

#[test]
fn network_filesystems_are_recognized() {
    for fs in [
        "smbfs",
        "afpfs",
        "webdav",
        "nfs",
        "nfs4",
        "cifs",
        "smb3",
        "fuse.sshfs",
        "gpfs",
        "beegfs",
        "wekafs",
        "panfs",
        "pvfs2",
        "afs",
        "fuse.ceph-fuse",
        "fuse.juicefs",
        "fuse.rclone",
        "fuse.s3fs",
        "fuse.gcsfuse",
    ] {
        assert!(is_network_file_system(fs), "{fs}");
    }
    for fs in [
        "apfs", "hfs", "ext4", "xfs", "btrfs", "zfs", "overlay", "tmpfs", "ntfs", "fuseblk",
    ] {
        assert!(!is_network_file_system(fs), "{fs}");
    }
}

/// The rows the storage panel showed before this cache existed.
fn reference_rows(hostname: &str) -> Vec<StorageInfo> {
    let disks = Disks::new_with_refreshed_list();
    let mut filtered = filter_docker_aware_disks(&disks);
    filtered.sort_by(|a, b| {
        a.mount_point()
            .to_string_lossy()
            .cmp(&b.mount_point().to_string_lossy())
    });
    filtered
        .iter()
        .enumerate()
        .map(|(index, disk)| StorageInfo {
            mount_point: disk.mount_point().to_string_lossy().to_string(),
            total_bytes: disk.total_space(),
            available_bytes: disk.available_space(),
            host_id: hostname.to_string(),
            hostname: hostname.to_string(),
            index: index as u32,
        })
        .collect()
}

/// Same rows, order, indices and totals as a fresh enumeration, and
/// available space that tracks it. Available space is compared with some
/// slack because other processes write to these volumes between the reads.
#[test]
fn rows_match_a_fresh_enumeration() {
    let mut cache = DiskCache::new();
    let rows = cache.storage_info("host-a");
    let reference = reference_rows("host-a");

    let mounts = |rows: &[StorageInfo]| -> Vec<(u32, String)> {
        rows.iter()
            .map(|row| (row.index, row.mount_point.clone()))
            .collect()
    };
    if mounts(&rows) != mounts(&reference) {
        // The mount table changed between the two enumerations; nothing to
        // compare against.
        return;
    }

    for (row, expected) in rows.iter().zip(&reference) {
        assert_eq!(row.hostname, "host-a");
        assert_eq!(row.host_id, "host-a");
        assert_eq!(row.total_bytes, expected.total_bytes, "{}", row.mount_point);
        assert!(
            row.available_bytes <= row.total_bytes,
            "{}",
            row.mount_point
        );
        let slack = (expected.total_bytes / 100).max(2 * GB);
        assert!(
            row.available_bytes.abs_diff(expected.available_bytes) <= slack,
            "{}: {} against {}",
            row.mount_point,
            row.available_bytes,
            expected.available_bytes
        );
    }
}

/// Between list refreshes a tick reuses the list instead of enumerating.
#[test]
fn ticks_between_list_refreshes_reuse_the_list() {
    let mut cache = DiskCache::new();
    let first = cache.storage_info("h");
    let enumerated_at = cache.enumerated_at;
    assert!(cache.listing.is_some(), "the first call waits for a list");
    assert!(cache.pending.is_none());

    let second = cache.storage_info("h");
    assert_eq!(cache.enumerated_at, enumerated_at, "no second enumeration");
    assert!(cache.pending.is_none());
    let mounts = |rows: &[StorageInfo]| -> Vec<String> {
        rows.iter().map(|row| row.mount_point.clone()).collect()
    };
    assert_eq!(mounts(&first), mounts(&second));
}

/// Once the interval has passed the list is enumerated again, off the tick.
#[test]
fn the_list_is_enumerated_again_once_due() {
    let mut cache = DiskCache::new();
    let _ = cache.storage_info("h");
    let Some(long_ago) = Instant::now().checked_sub(LIST_REFRESH_INTERVAL + Duration::from_secs(1))
    else {
        return;
    };
    cache.enumerated_at = Some(long_ago);

    let _ = cache.storage_info("h");
    let restarted = cache.enumerated_at.expect("an enumeration started");
    assert!(restarted > long_ago);
    assert!(restarted.elapsed() < LIST_REFRESH_INTERVAL);
}

/// The interval counts from when the previous list arrived, not from when
/// that enumeration started: a slow enumeration must not make the next one
/// due the instant it lands.
#[test]
fn the_interval_counts_from_when_the_list_arrived() {
    let mut cache = DiskCache::new();
    let _ = cache.storage_info("h");

    let Some(long_ago) = Instant::now().checked_sub(LIST_REFRESH_INTERVAL + Duration::from_secs(1))
    else {
        return;
    };

    let (sender, receiver) = mpsc::channel::<Listing>();
    cache.pending = Some(receiver);
    cache.enumerated_at = Some(long_ago);
    sender.send(Listing::enumerate()).expect("receiver alive");

    let _ = cache.storage_info("h");
    assert!(cache.pending.is_none());
    let arrived = cache.enumerated_at.expect("the list arrived");
    assert!(arrived.elapsed() < LIST_REFRESH_INTERVAL);

    let _ = cache.storage_info("h");
    assert!(
        cache.pending.is_none(),
        "no new enumeration started right after the slow one landed"
    );
}

/// An enumeration that never finishes, as on a hung network mount, leaves
/// the tick on the previous list instead of waiting for it. That holds for a
/// blocking cache too once it has its first list.
#[test]
fn a_hung_enumeration_does_not_hold_up_the_tick() {
    for mut cache in [DiskCache::new(), DiskCache::with_blocking_first_list()] {
        let before = cache.storage_info("h");

        let (sender, receiver) = mpsc::channel::<Listing>();
        cache.pending = Some(receiver);
        let started = Instant::now();
        let during = cache.storage_info("h");

        assert!(started.elapsed() < Duration::from_millis(500));
        assert_eq!(before.len(), during.len());
        assert!(cache.pending.is_some(), "still waiting for the enumeration");
        drop(sender);
    }
}

/// An enumeration thread that died leaves the previous list in place.
#[test]
fn a_failed_enumeration_keeps_the_previous_list() {
    let mut cache = DiskCache::new();
    let before = cache.storage_info("h");

    let (sender, receiver) = mpsc::channel::<Listing>();
    drop(sender);
    cache.pending = Some(receiver);
    let after = cache.storage_info("h");

    assert!(cache.pending.is_none());
    assert_eq!(before.len(), after.len());
}

/// With no list yet, the first call waits a bounded time and later calls do
/// not wait at all.
#[test]
fn the_first_list_is_waited_for_once_and_briefly() {
    let mut cache = DiskCache::new();
    cache.first_list_wait = Some(Duration::from_millis(50));
    cache.enumerated_at = Some(Instant::now());
    let (sender, receiver) = mpsc::channel::<Listing>();
    cache.pending = Some(receiver);

    let started = Instant::now();
    assert!(cache.storage_info("h").is_empty());
    assert!(started.elapsed() >= Duration::from_millis(50));

    let started = Instant::now();
    assert!(cache.storage_info("h").is_empty());
    assert!(started.elapsed() < Duration::from_millis(50));

    // The list shows up on the first tick after it arrives.
    sender.send(Listing::enumerate()).expect("receiver alive");
    let _ = cache.storage_info("h");
    assert!(cache.listing.is_some());
    assert!(cache.pending.is_none());
}

/// A blocking cache builds its first list on the calling thread and returns
/// it whole: no background thread, no bounded wait spent.
#[test]
fn a_blocking_cache_returns_its_first_list_from_the_caller() {
    let mut cache = DiskCache::with_blocking_first_list();
    let rows = cache.storage_info("h");

    let listing = cache.listing.as_ref().expect("the first call built a list");
    assert_eq!(rows.len(), listing.volumes.len());
    assert!(
        cache.pending.is_none(),
        "the first list did not go to a thread"
    );
    assert!(cache.enumerated_at.is_some());
    assert!(!cache.waited_for_first_list);
}

/// A path inside a volume is not that volume's mount point, so `statfs`
/// reports another filesystem for it, and that reading must not move an
/// anchor.
#[test]
#[cfg(target_os = "macos")]
fn statfs_is_read_only_for_a_mount_point() {
    assert!(statfs_free_bytes(std::path::Path::new("/")).is_some());
    assert_eq!(statfs_free_bytes(std::path::Path::new("/usr/bin")), None);
}

/// On macOS every shown local volume carries an anchor, and a tick reports
/// the anchored value.
#[test]
#[cfg(target_os = "macos")]
fn local_volumes_are_anchored_on_macos() {
    let mut listing = Listing::enumerate();
    for volume in listing.volumes.iter().filter(|volume| !volume.network) {
        let anchor = volume.anchor.expect("statfs works on a mounted volume");
        assert_eq!(anchor.available, volume.available_bytes);
    }

    let requests = listing.capacity_requests();
    let disks = listing
        .disks
        .as_mut()
        .expect("a fresh listing holds its disks");
    let capacities = refresh_capacities(disks, &requests);
    assert_eq!(
        capacities.len(),
        requests.len(),
        "every local volume refreshed"
    );
    listing.apply_capacities(&capacities);
    for volume in &listing.volumes {
        assert!(volume.available_bytes <= volume.total_bytes);
    }
}

/// A worker's refresh reads the same totals the list reported and an
/// available space that tracks it, on every platform.
#[test]
fn a_capacity_refresh_matches_the_list() {
    let mut listing = Listing::enumerate();
    let before: Vec<(u64, u64)> = listing
        .volumes
        .iter()
        .map(|volume| (volume.total_bytes, volume.available_bytes))
        .collect();
    let requests = listing.capacity_requests();
    assert!(
        requests
            .iter()
            .all(|request| listing.volumes[request.volume].index == request.disk)
    );

    let disks = listing
        .disks
        .as_mut()
        .expect("a fresh listing holds its disks");
    let capacities = refresh_capacities(disks, &requests);
    listing.apply_capacities(&capacities);
    for (volume, (total, available)) in listing.volumes.iter().zip(before) {
        assert_eq!(volume.total_bytes, total, "{}", volume.mount_point);
        let slack = (total / 100).max(2 * GB);
        assert!(
            volume.available_bytes.abs_diff(available) <= slack,
            "{}: {} against {available}",
            volume.mount_point,
            volume.available_bytes
        );
    }
}

/// Between list refreshes each tick refreshes capacity through the worker,
/// which hands the `Disks` back with its result.
#[test]
fn ticks_refresh_capacity_through_the_worker() {
    let mut cache = DiskCache::new();
    let _ = cache.storage_info("h");
    let _ = cache.storage_info("h");
    let worker = cache
        .capacity
        .as_ref()
        .expect("the first tick started the worker");
    assert!(!worker.in_flight, "the refresh finished within the budget");
    let listing = cache.listing.as_ref().expect("a list");
    assert!(
        listing.disks.is_some(),
        "the disks came back with the result"
    );
}

/// The worker for the fake-injection tests below: its job sender goes
/// nowhere and its results come from `results`.
fn fake_worker(
    results: mpsc::Receiver<CapacityResult>,
) -> (CapacityWorker, mpsc::Receiver<capacity::CapacityJob>) {
    let (jobs, job_receiver) = mpsc::channel();
    (
        CapacityWorker {
            jobs,
            results,
            in_flight: true,
        },
        job_receiver,
    )
}

/// A capacity read that never returns, as on a volume that stopped
/// answering, leaves the tick on the previous values after at most the
/// budget, for a loop cache and a blocking one alike.
#[test]
fn a_hung_capacity_refresh_returns_the_previous_values_within_the_budget() {
    for mut cache in [DiskCache::new(), DiskCache::with_blocking_first_list()] {
        let before = cache.storage_info("h");
        let (_never_answers, results) = mpsc::channel::<CapacityResult>();
        let (worker, _jobs) = fake_worker(results);
        cache.capacity = Some(worker);
        let _held_by_the_hung_job = cache
            .listing
            .as_mut()
            .and_then(|listing| listing.disks.take());

        let started = Instant::now();
        let during = cache.storage_info("h");
        let elapsed = started.elapsed();

        // The wait itself is exactly the budget; the slack is for scheduling
        // on a loaded runner, not for a longer wait.
        assert!(
            elapsed < CAPACITY_REFRESH_BUDGET * 3,
            "storage_info took {elapsed:?} against a {CAPACITY_REFRESH_BUDGET:?} budget"
        );
        assert!(
            elapsed >= CAPACITY_REFRESH_BUDGET,
            "the tick waited out the budget"
        );
        let rows = |rows: &[StorageInfo]| -> Vec<(String, u64, u64)> {
            rows.iter()
                .map(|row| {
                    (
                        row.mount_point.clone(),
                        row.total_bytes,
                        row.available_bytes,
                    )
                })
                .collect()
        };
        assert_eq!(
            rows(&during),
            rows(&before),
            "the previous values were reported"
        );
        let worker = cache.capacity.as_ref().expect("the worker is kept");
        assert!(worker.in_flight, "no second refresh was started");
    }
}

/// A refresh that finishes after the budget is applied by the first tick
/// after it lands, which starts no refresh of its own.
#[test]
fn a_late_capacity_result_is_applied_on_the_next_tick() {
    let mut cache = DiskCache::new();
    let before = cache.storage_info("h");
    let listing = cache.listing.as_mut().expect("a list");
    if listing.volumes.is_empty() {
        return;
    }
    let disks = listing.disks.take().expect("the listing holds its disks");
    let generation = listing.generation;
    let total = listing.volumes[0].total_bytes;

    let (answer, results) = mpsc::channel::<CapacityResult>();
    let (worker, _jobs) = fake_worker(results);
    cache.capacity = Some(worker);
    let late = std::thread::spawn(move || {
        std::thread::sleep(CAPACITY_REFRESH_BUDGET * 4);
        let _ = answer.send(CapacityResult {
            generation,
            disks,
            capacities: vec![(0, total, 12_345)],
        });
    });

    let during = cache.storage_info("h");
    assert_eq!(during[0].available_bytes, before[0].available_bytes);

    late.join().expect("the late refresh thread");
    let after = cache.storage_info("h");
    assert_eq!(
        after[0].available_bytes, 12_345,
        "the late result was applied"
    );
    assert_eq!(after[0].total_bytes, total);
    let worker = cache.capacity.as_ref().expect("the worker is kept");
    assert!(
        !worker.in_flight,
        "the tick that took a late result started no job"
    );
    let listing = cache.listing.as_ref().expect("a list");
    assert!(listing.disks.is_some(), "the disks came back");
}

/// A result from before the list was replaced is discarded: its values
/// belong to rows that no longer exist.
#[test]
fn a_capacity_result_for_a_replaced_list_is_discarded() {
    let mut cache = DiskCache::new();
    let before = cache.storage_info("h");
    let listing = cache.listing.as_mut().expect("a list");
    if listing.volumes.is_empty() {
        return;
    }
    let disks = listing.disks.take().expect("the listing holds its disks");
    let stale_generation = listing.generation.wrapping_sub(1);
    let total = listing.volumes[0].total_bytes;

    let (answer, results) = mpsc::channel::<CapacityResult>();
    let (worker, _jobs) = fake_worker(results);
    cache.capacity = Some(worker);
    answer
        .send(CapacityResult {
            generation: stale_generation,
            disks,
            capacities: vec![(0, total, 12_345)],
        })
        .expect("receiver alive");

    let after = cache.storage_info("h");
    assert_eq!(after[0].available_bytes, before[0].available_bytes);
    let listing = cache.listing.as_ref().expect("a list");
    assert!(
        listing.disks.is_none(),
        "the stale disks were dropped with the result"
    );
}

/// A new list gets a new generation, so in-flight results for the old one
/// can be told apart.
#[test]
fn each_installed_list_gets_its_own_generation() {
    let mut cache = DiskCache::new();
    let _ = cache.storage_info("h");
    let first = cache.listing.as_ref().expect("a list").generation;
    cache.install_listing(Listing::enumerate());
    let second = cache.listing.as_ref().expect("a list").generation;
    assert_ne!(first, second);
}

/// On Linux a volume whose mount point moved to another filesystem since
/// the list was built keeps its previous values rather than reporting the
/// filesystem now under that directory.
#[test]
#[cfg(target_os = "linux")]
fn a_mount_point_on_another_device_keeps_its_previous_values() {
    use super::capacity::{mount_device, mount_identity_matches};

    let root = std::path::Path::new("/");
    let device = mount_device(root).expect("stat works on /");
    assert!(mount_identity_matches(Some(device), root));
    assert!(!mount_identity_matches(Some(device ^ 1), root));
    assert!(
        mount_identity_matches(None, root),
        "with no recorded device the mount is refreshed as before"
    );

    let mut listing = Listing::enumerate();
    // Only volumes whose device was recorded can be told apart from their
    // parent; one whose `stat` failed at list time is refreshed as before.
    let mut requests = listing.capacity_requests();
    requests.retain(|request| request.device.is_some());
    if requests.is_empty() {
        return;
    }
    let disks = listing
        .disks
        .as_mut()
        .expect("a fresh listing holds its disks");
    let refreshed = refresh_capacities(disks, &requests);
    assert_eq!(
        refreshed.len(),
        requests.len(),
        "every local volume refreshed"
    );

    for request in &mut requests {
        request.device = request.device.map(|device| device ^ 1);
    }
    let kept = refresh_capacities(disks, &requests);
    assert!(
        kept.is_empty(),
        "no volume on a changed device was refreshed"
    );
}
