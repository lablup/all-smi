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

//! NVML handle lifetime on a real NVIDIA host (issues #488, #489).
//!
//! Every `nvmlInit_v2` followed by unloading `libnvidia-ml` leaves one
//! `eventfd` in the process. These tests construct and drop `AllSmi`
//! repeatedly and count `eventfd` descriptors in `/proc/self/fd`. They skip
//! with a printed reason unless NVML initialises on this host.

#![cfg(target_os = "linux")]

use std::sync::Mutex;

use all_smi::AllSmi;

/// Both tests count process-wide descriptors, so they must not overlap.
static SERIAL: Mutex<()> = Mutex::new(());

/// Iterations for the construct-and-drop loop. On `main` before the fix,
/// each iteration leaked one eventfd, so the growth is either about this
/// number or at most a small constant.
const ITERATIONS: usize = 2_000;

/// Allowed eventfd growth across the whole loop.
const MAX_GROWTH: usize = 4;

fn eventfd_count() -> usize {
    let Ok(entries) = std::fs::read_dir("/proc/self/fd") else {
        return 0;
    };
    entries
        .filter_map(|e| e.ok())
        .filter_map(|e| std::fs::read_link(e.path()).ok())
        .filter(|target| target.to_string_lossy().contains("eventfd"))
        .count()
}

/// Whether this host can run the test: NVML must initialise, and
/// `AllSmi` must actually build an NVML reader (the factory honours
/// `NO_GPU` / `SKIP_GPU_DETECTION`).
fn nvml_usable() -> Result<(), String> {
    if std::env::var_os("NO_GPU").is_some() || std::env::var_os("SKIP_GPU_DETECTION").is_some() {
        return Err("GPU detection disabled by environment".to_string());
    }
    if !std::path::Path::new("/dev/nvidiactl").exists() {
        return Err("no /dev/nvidiactl on this host".to_string());
    }
    // Probe through the library itself so this test does not load NVML a
    // second way. The probe's eventfds are created before the baseline is
    // taken, so they do not count against the bound.
    let smi = AllSmi::new().map_err(|e| format!("AllSmi::new() failed: {e}"))?;
    if smi.get_gpu_info().is_empty() {
        return Err("no GPU reported".to_string());
    }
    if let Some(status) = all_smi::device::get_nvml_status_message() {
        return Err(format!("NVML did not initialise ({status})"));
    }
    // The status message stays unset when AllSmi picked a reader that never
    // touches NVML (for example the Jetson reader), so also require that the
    // shared NVML handle exists in this process.
    if !all_smi::device::shared_nvml_present() {
        return Err("AllSmi did not initialise NVML (non-NVML reader in use)".to_string());
    }
    Ok(())
}

#[test]
fn repeated_allsmi_construction_does_not_leak_eventfds() {
    let _guard = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    if let Err(reason) = nvml_usable() {
        println!("skipping: {reason}");
        return;
    }

    let before = eventfd_count();
    let started = std::time::Instant::now();
    for _ in 0..ITERATIONS {
        let smi = AllSmi::new().expect("AllSmi::new() failed");
        assert!(!smi.get_gpu_info().is_empty(), "GPU disappeared mid-test");
    }
    let after = eventfd_count();
    println!(
        "eventfds before={before} after={after} over {ITERATIONS} iterations in {:?}",
        started.elapsed()
    );
    assert!(
        after <= before + MAX_GROWTH,
        "eventfd count grew from {before} to {after} over {ITERATIONS} AllSmi lifetimes"
    );
}

#[test]
fn live_instances_share_nvml_and_survive_a_sibling_drop() {
    let _guard = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    if let Err(reason) = nvml_usable() {
        println!("skipping: {reason}");
        return;
    }

    let first = AllSmi::new().expect("AllSmi::new() failed");
    let baseline = first.get_gpu_info().len();
    let before = eventfd_count();

    let second = AllSmi::new().expect("AllSmi::new() failed");
    assert_eq!(second.get_gpu_info().len(), baseline);
    // A second live instance reuses the first one's NVML session rather
    // than running its own init.
    assert_eq!(
        eventfd_count(),
        before,
        "second AllSmi initialised NVML again"
    );

    drop(first);
    assert_eq!(
        second.get_gpu_info().len(),
        baseline,
        "dropping one AllSmi broke NVML for the other"
    );
    assert!(all_smi::device::get_nvml_status_message().is_none());
}
