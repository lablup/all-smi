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

use super::*;

#[test]
fn memory_percent_is_zero_when_total_is_unknown() {
    assert_eq!(memory_percent(1024, 0), 0.0);
    assert_eq!(memory_percent(512, 2048), 25.0);
}

pub(in crate::device::process_list) fn process(
    pid: u32,
    device_uuid: &str,
    used_memory: u64,
    uses_gpu: bool,
) -> ProcessInfo {
    ProcessInfo {
        device_id: 0,
        device_uuid: device_uuid.to_string(),
        pid,
        process_name: format!("proc-{pid}"),
        used_memory,
        cpu_percent: 1.0,
        memory_percent: 1.0,
        memory_rss: 1024,
        memory_vms: 2048,
        user: "user".to_string(),
        state: "S".to_string(),
        start_time: "0".to_string(),
        cpu_time: 0,
        command: "cmd".to_string(),
        ppid: 1,
        threads: 1,
        uses_gpu,
        priority: 0,
        nice_value: 0,
        gpu_utilization: 0.0,
    }
}

#[test]
fn multi_gpu_pid_preserved_as_multiple_rows() {
    let all_processes = vec![process(123, "GPU", 0, true)];
    let gpu_processes = vec![
        process(123, "GPU-A", 1024, true),
        process(123, "GPU-B", 2048, true),
    ];

    let merged = merge_gpu_processes(all_processes, gpu_processes);

    assert_eq!(merged.len(), 2);
    assert!(
        merged
            .iter()
            .any(|p| p.pid == 123 && p.device_uuid == "GPU-A" && p.used_memory == 1024)
    );
    assert!(
        merged
            .iter()
            .any(|p| p.pid == 123 && p.device_uuid == "GPU-B" && p.used_memory == 2048)
    );
}

#[test]
fn duplicate_same_pid_device_coalesced() {
    let all_processes = vec![process(123, "GPU", 0, true)];
    let mut gpu_a_low = process(123, "GPU-A", 1024, true);
    gpu_a_low.gpu_utilization = 10.0;
    let mut gpu_a_high = process(123, "GPU-A", 2048, true);
    gpu_a_high.gpu_utilization = 20.0;

    let merged = merge_gpu_processes(all_processes, vec![gpu_a_low, gpu_a_high]);

    assert_eq!(merged.len(), 1);
    let row = &merged[0];
    assert_eq!(row.device_uuid, "GPU-A");
    assert_eq!(row.used_memory, 2048);
    assert_eq!(row.gpu_utilization, 20.0);
}

#[test]
fn non_gpu_processes_preserved() {
    let non_gpu = process(999, "", 0, false);
    let merged = merge_gpu_processes(vec![non_gpu.clone()], Vec::new());

    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0].pid, 999);
    assert!(!merged[0].uses_gpu);
}

#[test]
fn gpu_row_without_baseline_process_still_emitted() {
    let orphan_gpu = process(777, "GPU-ORPHAN", 4096, true);
    let merged = merge_gpu_processes(Vec::new(), vec![orphan_gpu.clone()]);

    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0].pid, 777);
    assert_eq!(merged[0].device_uuid, "GPU-ORPHAN");
    assert_eq!(merged[0].used_memory, 4096);
}

/// Issue #504: baseline rows are moved, not cloned, and the result is the
/// same rows in the same order as before.
#[test]
fn merge_keeps_every_baseline_row_and_splits_multi_device_pids() {
    let mut all_processes: Vec<ProcessInfo> = (1..=1000)
        .map(|pid| {
            let mut row = process(pid, "", 0, false);
            row.command = format!("worker --id {pid}");
            row
        })
        .collect();
    all_processes.reverse();
    let gpu_processes = vec![
        process(10, "GPU-A", 100, true),
        process(10, "GPU-B", 200, true),
        process(500, "GPU-A", 300, true),
    ];

    let merged = merge_gpu_processes(all_processes, gpu_processes);

    // 998 baseline-only rows (PIDs 10 and 500 have GPU rows) plus three GPU
    // rows, PID 10 once per device.
    assert_eq!(merged.len(), 1001);
    let keys: Vec<(u32, &str)> = merged
        .iter()
        .map(|row| (row.pid, row.device_uuid.as_str()))
        .collect();
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(keys, sorted, "rows stay ordered by (pid, device_uuid)");
    assert_eq!(
        keys.iter()
            .filter(|(pid, _)| *pid == 10)
            .collect::<Vec<_>>(),
        vec![&(10, "GPU-A"), &(10, "GPU-B")]
    );
    for row in &merged {
        assert_eq!(row.command, format!("worker --id {}", row.pid));
        match (row.pid, row.device_uuid.as_str()) {
            (10, "GPU-A") => assert_eq!(row.used_memory, 100),
            (10, "GPU-B") => assert_eq!(row.used_memory, 200),
            (500, "GPU-A") => assert_eq!(row.used_memory, 300),
            (_, uuid) => {
                assert_eq!(uuid, "");
                assert!(!row.uses_gpu);
            }
        }
    }
}

#[test]
fn process_row_refresh_kind_reads_only_what_rows_use() {
    let kind = process_row_refresh_kind();
    assert!(kind.cpu());
    assert!(kind.memory());
    assert!(!kind.tasks());
    assert!(!kind.disk_usage());
    assert_eq!(kind.user(), UpdateKind::OnlyIfNotSet);
    assert_eq!(kind.cmd(), UpdateKind::OnlyIfNotSet);
    assert_eq!(kind.exe(), UpdateKind::Never);
    assert_eq!(kind.environ(), UpdateKind::Never);
    assert_eq!(kind.cwd(), UpdateKind::Never);
    assert_eq!(kind.root(), UpdateKind::Never);
}

/// Issue #504: with the shared refresh kind a child's row carries its full
/// command line (the TUI kind had no `cmd` and showed `[sleep]`), and no row
/// is a userland thread (`everything()` turned tasks on).
#[cfg(target_os = "linux")]
#[test]
fn process_rows_have_full_commands_and_no_thread_rows() {
    use std::sync::mpsc;
    use sysinfo::ProcessesToUpdate;

    struct KillOnDrop(std::process::Child);
    impl Drop for KillOnDrop {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    let child = KillOnDrop(
        crate::utils::command::new_command("sleep")
            .arg("30")
            .spawn()
            .expect("spawn sleep"),
    );
    let child_pid = child.0.id();

    let (tid_tx, tid_rx) = mpsc::channel();
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let helper = std::thread::spawn(move || {
        // SAFETY: `gettid` has no preconditions.
        let tid = unsafe { libc::gettid() } as u32;
        tid_tx.send(tid).unwrap();
        let _ = stop_rx.recv();
    });
    let helper_tid = tid_rx.recv().unwrap();

    let mut system = crate::utils::system::new_system();
    system.refresh_processes_specifics(ProcessesToUpdate::All, true, process_row_refresh_kind());
    let rows = get_all_processes(&system, &HashSet::new());

    stop_tx.send(()).unwrap();
    helper.join().unwrap();

    let child_row = rows
        .iter()
        .find(|row| row.pid == child_pid)
        .expect("child row");
    assert_eq!(child_row.command, "sleep 30");
    assert!(
        rows.iter().all(|row| row.pid != helper_tid),
        "thread {helper_tid} must not be a process row"
    );
    assert!(rows.iter().any(|row| row.pid == std::process::id()));

    // `rows_for_pids` builds the same row for just the requested PIDs.
    let only = rows_for_pids(&system, &HashSet::from([child_pid, helper_tid]));
    assert_eq!(only.len(), 1);
    assert_eq!(only[0].pid, child_pid);
    assert_eq!(only[0].command, "sleep 30");
    assert!(only[0].uses_gpu);
}

/// A reader whose `get_process_info` returns fixed rows (issue #505).
struct FakeReader {
    rows: Vec<ProcessInfo>,
    system_list: bool,
}

impl crate::device::traits::GpuReader for FakeReader {
    fn get_gpu_info(&self) -> Vec<crate::device::types::GpuInfo> {
        Vec::new()
    }

    fn get_process_info(&self) -> Vec<ProcessInfo> {
        self.rows.clone()
    }

    fn includes_system_processes(&self) -> bool {
        self.system_list
    }
}

fn fake(rows: Vec<ProcessInfo>, system_list: bool) -> Box<dyn crate::device::traits::GpuReader> {
    Box::new(FakeReader { rows, system_list })
}

fn baseline_rows() -> Vec<ProcessInfo> {
    [10, 20, 30]
        .into_iter()
        .map(|pid| process(pid, "", 0, false))
        .collect()
}

fn keys(rows: &[ProcessInfo]) -> Vec<(u32, String, bool)> {
    rows.iter()
        .map(|r| (r.pid, r.device_uuid.clone(), r.uses_gpu))
        .collect()
}

#[test]
fn collect_rows_dedups_two_system_list_readers() {
    let readers = vec![
        fake(
            merge_gpu_processes(baseline_rows(), vec![process(10, "uuid-a", 100, true)]),
            true,
        ),
        fake(
            merge_gpu_processes(
                baseline_rows(),
                vec![
                    process(20, "uuid-b", 200, true),
                    process(10, "uuid-b", 300, true),
                ],
            ),
            true,
        ),
    ];
    let mut calls = 0;
    let rows = collect::collect_process_rows_with(&readers, |pids| {
        calls += 1;
        assert_eq!(pids, &HashSet::from([10, 20]));
        baseline_rows()
    });

    assert_eq!(calls, 1);
    assert_eq!(
        keys(&rows),
        vec![
            (10, "uuid-a".to_string(), true),
            (10, "uuid-b".to_string(), true),
            (20, "uuid-b".to_string(), true),
            (30, String::new(), false),
        ]
    );
    let memory: Vec<u64> = rows.iter().map(|r| r.used_memory).collect();
    assert_eq!(memory, vec![100, 300, 200, 0]);
}

#[test]
fn collect_rows_has_no_baseline_row_for_gpu_only_reader_pid() {
    let readers = vec![
        fake(
            merge_gpu_processes(baseline_rows(), vec![process(10, "uuid-a", 100, true)]),
            true,
        ),
        fake(vec![process(20, "npu-0", 50, true)], false),
    ];
    let mut calls = 0;
    let rows = collect::collect_process_rows_with(&readers, |_| {
        calls += 1;
        baseline_rows()
    });

    assert_eq!(calls, 1);
    assert!(
        !rows.iter().any(|r| r.pid == 20 && !r.uses_gpu),
        "PID 20 has a GPU row, so it must not also have a baseline row"
    );
    assert_eq!(
        keys(&rows),
        vec![
            (10, "uuid-a".to_string(), true),
            (20, "npu-0".to_string(), true),
            (30, String::new(), false),
        ]
    );
    // The baseline enriches the GPU-only reader's row.
    assert_eq!(rows[1].process_name, "proc-20");
}

#[test]
fn collect_rows_without_system_list_reader_is_deduped_gpu_union() {
    let readers = vec![
        fake(
            vec![
                process(10, "npu-0", 100, true),
                process(11, "npu-1", 10, true),
            ],
            false,
        ),
        fake(
            vec![
                process(10, "npu-0", 150, true),
                process(12, "gpu-0", 20, true),
            ],
            false,
        ),
    ];
    let mut calls = 0;
    let rows = collect::collect_process_rows_with(&readers, |_| {
        calls += 1;
        Vec::new()
    });

    assert_eq!(calls, 0);
    assert_eq!(
        keys(&rows),
        vec![
            (10, "npu-0".to_string(), true),
            (11, "npu-1".to_string(), true),
            (12, "gpu-0".to_string(), true),
        ]
    );
    assert_eq!(rows[0].used_memory, 150);
}

#[test]
fn collect_rows_skips_system_pass_for_gpu_only_readers() {
    let readers = vec![fake(vec![process(10, "npu-0", 100, true)], false)];
    let before = crate::utils::system::global_system_calls_on_this_thread();
    let rows = collect_process_rows(&readers);
    assert_eq!(
        crate::utils::system::global_system_calls_on_this_thread(),
        before
    );
    assert_eq!(rows.len(), 1);
}

#[test]
fn collect_rows_runs_one_system_pass_for_system_list_readers() {
    let readers = vec![fake(Vec::new(), true), fake(Vec::new(), true)];
    let before = crate::utils::system::global_system_calls_on_this_thread();
    let rows = collect_process_rows(&readers);
    assert_eq!(
        crate::utils::system::global_system_calls_on_this_thread(),
        before + 1
    );
    assert!(rows.iter().any(|r| r.pid == std::process::id()));
}
