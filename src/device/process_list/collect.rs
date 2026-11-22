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

//! One deduplicated process list for a whole reader set (issue #505).

use std::collections::HashSet;

use sysinfo::ProcessesToUpdate;

use super::{get_all_processes, merge_gpu_processes, process_row_refresh_kind};
use crate::device::traits::GpuReader;
use crate::device::types::ProcessInfo;
use crate::utils::system::with_global_system;

/// One process list for a reader set: GPU rows from every reader, at most one
/// system process pass, one merge.
///
/// Rows are unique per `(pid, device_uuid)`, a PID with no GPU row appears
/// exactly once, and a PID with a GPU row from any reader has no baseline
/// (`uses_gpu: false`) row. Non-GPU processes are listed only when at least
/// one reader reports [`GpuReader::includes_system_processes`]; otherwise the
/// result is the deduplicated union of the readers' GPU rows.
pub fn collect_process_rows(readers: &[Box<dyn GpuReader>]) -> Vec<ProcessInfo> {
    collect_process_rows_with(readers, |gpu_pids| {
        with_global_system(|system| {
            system.refresh_processes_specifics(
                ProcessesToUpdate::All,
                true,
                process_row_refresh_kind(),
            );
            system.refresh_memory();
            get_all_processes(system, gpu_pids)
        })
    })
}

/// [`collect_process_rows`] with the system process pass injected.
///
/// Same guarantees as [`collect_process_rows`]. `baseline` runs at most once,
/// and only when some reader reports
/// [`GpuReader::includes_system_processes`].
pub(crate) fn collect_process_rows_with<F>(
    readers: &[Box<dyn GpuReader>],
    baseline: F,
) -> Vec<ProcessInfo>
where
    F: FnOnce(&HashSet<u32>) -> Vec<ProcessInfo>,
{
    if !readers.iter().any(|r| r.includes_system_processes()) {
        // GPU-only readers return GPU rows from `get_process_info`. Use it
        // rather than `get_gpu_processes`, which on Intel Linux yields bare
        // fdinfo rows with no name, user, CPU, or RSS.
        let mut gpu_rows = Vec::new();
        for reader in readers {
            gpu_rows.extend(reader.get_process_info());
        }
        return merge_gpu_processes(Vec::new(), gpu_rows);
    }

    let mut gpu_rows = Vec::new();
    let mut gpu_pids = HashSet::new();
    for reader in readers {
        let (rows, pids) = reader.get_gpu_processes();
        gpu_rows.extend(rows);
        gpu_pids.extend(pids);
    }
    merge_gpu_processes(baseline(&gpu_pids), gpu_rows)
}
