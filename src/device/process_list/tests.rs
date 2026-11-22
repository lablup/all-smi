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
