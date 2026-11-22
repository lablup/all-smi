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

//! Tests for what the Neuron reader keeps between calls: one `neuron-ls`
//! per tick, and the static sysfs identity per device.

use super::tests::NEURON_LS_TRN1_2XLARGE;
use super::*;
use std::fs;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A reader whose `neuron-ls` is a counting fake printing the trn1 fixture.
fn fake_reader(sysfs_root: &Path, driver_version: &Path) -> (NeuronReader, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    let reader = NeuronReader::new_with_roots(
        Box::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            Some(NEURON_LS_TRN1_2XLARGE.to_string())
        }),
        sysfs_root,
        driver_version,
    );
    (reader, calls)
}

/// The sysfs tree of the trn1 fixture's one device with two cores.
fn make_sysfs_device(root: &Path) -> PathBuf {
    let device = root.join("neuron0");
    let arch = device.join("info/architecture");
    fs::create_dir_all(&arch).unwrap();
    fs::write(device.join("info/serial_number"), "9ff5434815c8bd80\n").unwrap();
    fs::write(arch.join("arch_type"), "NDv2\n").unwrap();
    fs::write(arch.join("device_name"), "Trainium1\n").unwrap();
    fs::write(arch.join("instance_type"), "Trn1\n").unwrap();
    fs::create_dir_all(device.join("stats/power")).unwrap();
    fs::write(
        device.join("stats/power/utilization"),
        "POWER_STATUS_VALID,1789180860,0.00,0.00,0.00\n",
    )
    .unwrap();
    for core in 0..2 {
        let core_dir = device.join(format!("neuron_core{core}"));
        fs::create_dir_all(core_dir.join("info/architecture")).unwrap();
        fs::write(core_dir.join("info/architecture/arch_type"), "NCv2\n").unwrap();
        fs::create_dir_all(core_dir.join("stats/memory_usage/device_mem")).unwrap();
        fs::write(
            core_dir.join("stats/memory_usage/device_mem/present"),
            "1048576\n",
        )
        .unwrap();
    }
    device
}

#[test]
fn gpu_info_and_process_info_share_one_neuron_ls() {
    let dir = tempfile::tempdir().unwrap();
    make_sysfs_device(dir.path());
    let (reader, calls) = fake_reader(dir.path(), &dir.path().join("version"));
    let t0 = Instant::now();
    reader.ls_devices.set_test_now(t0);

    let rows = reader.get_gpu_info();
    reader
        .ls_devices
        .set_test_now(t0 + COALESCE_WINDOW - Duration::from_millis(1));
    let processes = reader.get_process_info();

    assert_eq!(rows.len(), 2);
    assert!(processes.is_empty());
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // The next tick runs `neuron-ls` again.
    reader.ls_devices.set_test_now(t0 + COALESCE_WINDOW);
    assert_eq!(reader.get_gpu_info().len(), 2);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn a_failed_neuron_ls_is_not_cached() {
    let dir = tempfile::tempdir().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    let reader = NeuronReader::new_with_roots(
        Box::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            None
        }),
        dir.path(),
        &dir.path().join("version"),
    );

    assert!(reader.get_gpu_info().is_empty());
    assert!(reader.get_process_info().is_empty());
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn static_identity_is_read_once_and_dynamic_files_every_call() {
    let dir = tempfile::tempdir().unwrap();
    let device = make_sysfs_device(dir.path());
    let (reader, _calls) = fake_reader(dir.path(), &dir.path().join("version"));

    let first = reader.get_gpu_info();
    assert_eq!(first[0].uuid, "neuron-9ff5434815c8bd80-nc0");
    assert_eq!(first[0].name, "AWS Trainium1");
    assert_eq!(first[0].used_memory, 1_048_576);
    assert_eq!(
        first[0].detail.get("core_architecture").map(String::as_str),
        Some("NCv2")
    );

    // Remove every identity file and move the dynamic ones.
    fs::remove_file(device.join("info/serial_number")).unwrap();
    fs::remove_dir_all(device.join("info/architecture")).unwrap();
    fs::remove_dir_all(device.join("neuron_core0/info")).unwrap();
    fs::write(
        device.join("stats/power/utilization"),
        "POWER_STATUS_VALID,1789180865,12.50,11.00,13.00\n",
    )
    .unwrap();
    fs::write(
        device.join("neuron_core0/stats/memory_usage/device_mem/present"),
        "2097152\n",
    )
    .unwrap();

    let second = reader.get_gpu_info();
    // Identity is unchanged: the files were not opened again.
    assert_eq!(second[0].uuid, first[0].uuid);
    assert_eq!(second[0].name, first[0].name);
    for key in [
        "serial_number",
        "architecture",
        "device_name",
        "instance_type",
        "core_architecture",
    ] {
        assert_eq!(second[0].detail.get(key), first[0].detail.get(key), "{key}");
        assert!(second[0].detail.contains_key(key), "{key}");
    }
    // The dynamic files were.
    assert_eq!(second[0].used_memory, 2_097_152);
    assert_eq!(
        second[0]
            .detail
            .get("power_utilization_raw")
            .map(String::as_str),
        Some("POWER_STATUS_VALID,1789180865,12.50,11.00,13.00")
    );
}

#[test]
fn identity_is_not_cached_before_the_device_directory_exists() {
    let dir = tempfile::tempdir().unwrap();
    let (reader, _calls) = fake_reader(dir.path(), &dir.path().join("version"));

    // No sysfs directory yet: the serial reads as absent and falls back to
    // the PCI address, and nothing is kept.
    let before = reader.get_gpu_info();
    assert_eq!(before[0].uuid, "neuron-0000:00:1e.0-nc0");

    make_sysfs_device(dir.path());
    let after = reader.get_gpu_info();
    assert_eq!(after[0].uuid, "neuron-9ff5434815c8bd80-nc0");
}

#[test]
fn process_rows_use_the_cached_serial() {
    let dir = tempfile::tempdir().unwrap();
    let device = make_sysfs_device(dir.path());
    let with_process = NEURON_LS_TRN1_2XLARGE.replace(
        r#""neuron_processes": []"#,
        r#""neuron_processes": [{"pid": 4242, "command": "/usr/bin/python3 train.py"}]"#,
    );
    let reader = NeuronReader::new_with_roots(
        Box::new(move || Some(with_process.clone())),
        dir.path(),
        &dir.path().join("version"),
    );

    let rows = reader.get_gpu_info();
    fs::remove_file(device.join("info/serial_number")).unwrap();
    let processes = reader.get_process_info();

    assert_eq!(processes.len(), 1);
    assert_eq!(processes[0].device_uuid, rows[0].uuid);
    assert_eq!(processes[0].device_uuid, "neuron-9ff5434815c8bd80-nc0");
}

#[test]
fn driver_version_is_kept_once_it_has_been_read() {
    let dir = tempfile::tempdir().unwrap();
    make_sysfs_device(dir.path());
    let version = dir.path().join("version");
    let (reader, _calls) = fake_reader(dir.path(), &version);

    // Absent on the first poll: not cached as absent.
    assert!(
        !reader.get_gpu_info()[0]
            .detail
            .contains_key("driver_version")
    );

    fs::write(&version, "2.26.5.0\n").unwrap();
    assert_eq!(
        reader.get_gpu_info()[0]
            .detail
            .get("driver_version")
            .map(String::as_str),
        Some("2.26.5.0")
    );

    fs::remove_file(&version).unwrap();
    assert_eq!(
        reader.get_gpu_info()[0]
            .detail
            .get("driver_version")
            .map(String::as_str),
        Some("2.26.5.0")
    );
}
