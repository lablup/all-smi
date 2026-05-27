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

//! Bounded fdinfo file reads for the Intel DRM client walker.

use std::io::Read;
use std::path::Path;

/// Real DRM fdinfo files are a few kilobytes at most. Keep a generous
/// cap so a future kernel change or synthetic procfs cannot make one
/// file read allocate without bound during a process scan.
const MAX_FDINFO_BYTES: u64 = 64 * 1024;

pub(super) fn read_fdinfo_to_string(path: &Path) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    let mut limited = file.take(MAX_FDINFO_BYTES + 1);
    let mut content = String::new();
    limited.read_to_string(&mut content).ok()?;
    if content.len() as u64 > MAX_FDINFO_BYTES {
        return None;
    }
    Some(content)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_fdinfo_to_string_reads_normal_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fdinfo");
        std::fs::write(&path, "drm-driver: i915\n").unwrap();

        assert_eq!(
            read_fdinfo_to_string(&path).as_deref(),
            Some("drm-driver: i915\n")
        );
    }

    #[test]
    fn read_fdinfo_to_string_rejects_oversized_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fdinfo");
        let oversized = vec![b'x'; (MAX_FDINFO_BYTES + 1) as usize];
        std::fs::write(&path, oversized).unwrap();

        assert!(read_fdinfo_to_string(&path).is_none());
    }
}
