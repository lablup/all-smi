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

//! Storage reader trait and implementations.
//!
//! This module provides the [`StorageReader`] trait for reading storage/disk
//! information and a [`LocalStorageReader`] implementation using `sysinfo::Disks`.

use std::sync::{Mutex, PoisonError};

use crate::storage::disk_cache::DiskCache;
use crate::storage::info::StorageInfo;
use crate::utils::get_hostname;

/// Trait for reading storage/disk information.
///
/// Implementations must be thread-safe (`Send + Sync`) to allow
/// concurrent access from multiple threads.
///
/// # Example
///
/// ```rust,no_run
/// use all_smi::storage::StorageReader;
///
/// fn print_storage(reader: &dyn StorageReader) {
///     for storage in reader.get_storage_info() {
///         println!("{}: {} bytes available", storage.mount_point, storage.available_bytes);
///     }
/// }
/// ```
#[allow(dead_code)] // Public API trait - used by library consumers
pub trait StorageReader: Send + Sync {
    /// Get information about all detected storage devices.
    ///
    /// Returns a vector of [`StorageInfo`] structs containing metrics for each
    /// detected storage device. The implementation filters out system directories
    /// and Docker-specific mounts.
    fn get_storage_info(&self) -> Vec<StorageInfo>;
}

/// Local storage reader using `sysinfo::Disks`.
///
/// This reader collects storage information from the local system using
/// the `sysinfo` crate. It applies Docker-aware filtering to exclude
/// system directories and Docker-specific bind mounts.
///
/// The disk list is kept between calls in a [`DiskCache`], so repeated calls
/// (the `record` loop, a library consumer polling) enumerate the mount table
/// at most every 30 s rather than on every call; a volume mounted in the
/// meantime appears within 30 s.
///
/// The first call enumerates the mount table on the calling thread and
/// returns the complete list however long that takes, as this reader always
/// has. The view and API collection loops use a cache whose first call waits
/// at most 2 s instead, so a hung mount cannot stall their first tick (see
/// [`DiskCache::with_blocking_first_list`] and [`DiskCache::new`]).
///
/// # Example
///
/// ```rust,no_run
/// use all_smi::storage::{LocalStorageReader, StorageReader};
///
/// let reader = LocalStorageReader::new();
/// for storage in reader.get_storage_info() {
///     let used_bytes = storage.total_bytes - storage.available_bytes;
///     let usage_percent = if storage.total_bytes > 0 {
///         (used_bytes as f64 / storage.total_bytes as f64) * 100.0
///     } else {
///         0.0
///     };
///     println!("{}: {:.1}% used", storage.mount_point, usage_percent);
/// }
/// ```
#[allow(dead_code)] // Public API struct - used by library consumers
pub struct LocalStorageReader {
    hostname: String,
    disks: Mutex<DiskCache>,
}

impl LocalStorageReader {
    /// Create a new local storage reader.
    ///
    /// The hostname is cached at creation time to avoid repeated lookups.
    #[allow(dead_code)] // Public API constructor - used by library consumers
    pub fn new() -> Self {
        Self {
            hostname: get_hostname(),
            disks: Mutex::new(DiskCache::with_blocking_first_list()),
        }
    }
}

impl Default for LocalStorageReader {
    fn default() -> Self {
        Self::new()
    }
}

impl StorageReader for LocalStorageReader {
    fn get_storage_info(&self) -> Vec<StorageInfo> {
        // The cache holds plain disk data that stays usable after a panic in
        // another caller, so recover from poisoning instead of propagating it.
        self.disks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .storage_info(&self.hostname)
    }
}

/// Create a storage reader for the local system.
///
/// This is a factory function that returns a boxed [`StorageReader`] trait object,
/// allowing for future implementations of remote or mock readers.
///
/// # Example
///
/// ```rust,no_run
/// use all_smi::storage::create_storage_reader;
///
/// let reader = create_storage_reader();
/// let storage_info = reader.get_storage_info();
/// println!("Found {} storage device(s)", storage_info.len());
/// ```
#[allow(dead_code)] // Public API factory function - used by library consumers
pub fn create_storage_reader() -> Box<dyn StorageReader> {
    Box::new(LocalStorageReader::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_local_storage_reader_creation() {
        let reader = LocalStorageReader::new();
        // Should not panic
        let _ = reader.get_storage_info();
    }

    #[test]
    fn test_local_storage_reader_default() {
        let reader = LocalStorageReader::default();
        let _ = reader.get_storage_info();
    }

    #[test]
    fn test_create_storage_reader() {
        let reader = create_storage_reader();
        // Should return at least the root filesystem on most systems
        let info = reader.get_storage_info();
        // We can't guarantee any specific storage on CI, but it shouldn't panic
        for storage in &info {
            assert!(!storage.mount_point.is_empty());
            assert!(!storage.hostname.is_empty());
        }
    }

    /// The first call returns the complete list, the same rows a fresh
    /// enumeration produces, rather than whatever a bounded wait allowed.
    #[test]
    fn the_first_call_returns_the_complete_list() {
        use crate::utils::filter_docker_aware_disks;
        use sysinfo::Disks;

        let reader = LocalStorageReader::new();
        let rows: Vec<String> = reader
            .get_storage_info()
            .into_iter()
            .map(|row| row.mount_point)
            .collect();

        let disks = Disks::new_with_refreshed_list();
        let mut expected: Vec<String> = filter_docker_aware_disks(&disks)
            .iter()
            .map(|disk| disk.mount_point().to_string_lossy().into_owned())
            .collect();
        expected.sort();
        if rows.len() != expected.len() {
            // The mount table changed between the two enumerations.
            return;
        }
        assert_eq!(rows, expected);
    }

    #[test]
    fn test_storage_reader_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<LocalStorageReader>();
    }

    #[test]
    fn test_storage_info_consistency() {
        let reader = LocalStorageReader::new();
        let info = reader.get_storage_info();

        for storage in &info {
            // available_bytes should not exceed total_bytes
            assert!(
                storage.available_bytes <= storage.total_bytes,
                "available_bytes ({}) should not exceed total_bytes ({})",
                storage.available_bytes,
                storage.total_bytes
            );

            // Index should be sequential
            // (this is checked implicitly by enumerate in the implementation)
        }

        // Check indices are sequential starting from 0
        for (expected_index, storage) in info.iter().enumerate() {
            assert_eq!(
                storage.index, expected_index as u32,
                "Storage index should be sequential"
            );
        }
    }
}
