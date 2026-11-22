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

//! Static/dynamic `/proc/cpuinfo` split and cached machine string (issue #504).

use super::*;
use crate::device::container_info::parse_cpu_stat_with_container_limits;
use crate::device::cpu_linux::{CONTAINER_INFO, LinuxCpuReader};
use crate::device::{CpuPlatformType, CpuReader};

/// aarch64 `/proc/cpuinfo` as the kernel prints it: no `model name`, no
/// `cpu MHz`, only implementer and part.
const AARCH64_CPUINFO: &str = "processor\t: 0
BogoMIPS\t: 2000.00
Features\t: fp asimd evtstrm
CPU implementer\t: 0x4e
CPU architecture: 8
CPU variant\t: 0x0
CPU part\t: 0x004
CPU revision\t: 0

processor\t: 1
BogoMIPS\t: 2000.00
CPU implementer\t: 0x4e
CPU part\t: 0x004
";

const X86_CPUINFO_WITH_MHZ: &str = "processor\t: 0
model name\t: Intel(R) Xeon(R) CPU
cpu MHz\t\t: 2000.000
processor\t: 1
cpu MHz\t\t: 3000.000
processor\t: 2
cpu MHz\t\t: 2500.000
processor\t: 3
cpu MHz\t\t: 1500.000
";

fn write(root: &Path, relative: &str, content: &str) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

#[test]
fn arm_model_is_synthesized_from_the_cached_machine_string() {
    let (model, architecture, platform, _, _, threads, _, _, _) =
        parse_cpuinfo(AARCH64_CPUINFO, "aarch64", &CONTAINER_INFO).unwrap();
    assert_eq!(model, "NVIDIA ARM Processor (Part: 0x004)");
    assert_eq!(architecture, "aarch64");
    assert_eq!(platform, CpuPlatformType::Arm);
    assert_eq!(threads, 2);
}

#[test]
fn a_non_arm_machine_does_not_synthesize_a_model() {
    let (model, architecture, _, _, _, _, _, _, _) =
        parse_cpuinfo(AARCH64_CPUINFO, "x86_64", &CONTAINER_INFO).unwrap();
    assert_eq!(model, "");
    assert_eq!(architecture, "x86_64");
}

#[test]
fn reader_machine_matches_uname() {
    let reader = LinuxCpuReader::new();
    assert!(!reader.machine.is_empty());
    assert_eq!(reader.machine, machine_string());
    // `uname -m` prints the same `utsname.machine` field.
    if let Ok(output) = crate::utils::command::new_command("uname")
        .arg("-m")
        .output()
        && output.status.success()
    {
        assert_eq!(
            reader.machine,
            String::from_utf8_lossy(&output.stdout).trim()
        );
    }
    let info = reader.get_cpu_info();
    if let Some(info) = info.first() {
        assert_eq!(info.architecture, reader.machine);
    }
}

#[test]
fn frequency_source_averages_scaling_cur_freq_and_follows_changes() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for (cpu, khz) in [
        (0, "1000000"),
        (1, "2000000"),
        (2, "3000000"),
        (3, "4000000"),
    ] {
        write(
            root,
            &format!("devices/system/cpu/cpu{cpu}/cpufreq/scaling_cur_freq"),
            &format!("{khz}\n"),
        );
    }

    let source = FrequencySource::resolve(root, None);
    assert!(!source.is_empty());
    assert_eq!(source.sample(), Some(2500));

    write(
        root,
        "devices/system/cpu/cpu3/cpufreq/scaling_cur_freq",
        "8000000\n",
    );
    assert_eq!(source.sample(), Some(3500));

    // A cpuset restricts the average to its CPUs.
    let restricted = FrequencySource::resolve(root, Some(&[0, 1]));
    assert_eq!(restricted.sample(), Some(1500));
}

#[test]
fn frequency_source_honours_the_online_mask() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for (cpu, khz) in [(0, "1000000"), (1, "3000000"), (2, "9000000")] {
        write(
            root,
            &format!("devices/system/cpu/cpu{cpu}/cpufreq/scaling_cur_freq"),
            khz,
        );
    }
    write(root, "devices/system/cpu/online", "0-1\n");
    assert_eq!(FrequencySource::resolve(root, None).sample(), Some(2000));
}

#[test]
fn reader_frequency_moves_while_static_fields_stay() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for cpu in 0..4 {
        write(
            root,
            &format!("devices/system/cpu/cpu{cpu}/cpufreq/scaling_cur_freq"),
            "2000000\n",
        );
    }
    let reader = LinuxCpuReader {
        frequency: FrequencySource::resolve(root, None),
        ..LinuxCpuReader::new()
    };

    let first = reader.get_cpu_info().pop().expect("cpu info");
    assert_eq!(first.base_frequency_mhz, 2000);

    write(
        root,
        "devices/system/cpu/cpu0/cpufreq/scaling_cur_freq",
        "6000000\n",
    );
    let second = reader.get_cpu_info().pop().expect("cpu info");
    assert_eq!(second.base_frequency_mhz, 3000);
    for info in [&first, &second] {
        assert!(
            info.per_socket_info
                .iter()
                .all(|socket| socket.frequency_mhz == info.base_frequency_mhz)
        );
    }

    assert_eq!(first.cpu_model, second.cpu_model);
    assert_eq!(first.architecture, second.architecture);
    assert_eq!(first.platform_type, second.platform_type);
    assert_eq!(first.socket_count, second.socket_count);
    assert_eq!(first.total_cores, second.total_cores);
    assert_eq!(first.total_threads, second.total_threads);
    assert_eq!(first.cache_size_mb, second.cache_size_mb);
    assert_eq!(first.max_frequency_mhz, second.max_frequency_mhz);
}

#[test]
fn without_cpufreq_the_cpu_mhz_fallback_matches_the_full_parser() {
    let empty = tempfile::tempdir().unwrap();
    assert!(FrequencySource::resolve(empty.path(), None).is_empty());

    let cpuset = CONTAINER_INFO.cpuset_cpus.as_deref();
    let (_, _, _, _, _, _, base, _, _) =
        parse_cpuinfo(X86_CPUINFO_WITH_MHZ, "x86_64", &CONTAINER_INFO).unwrap();
    if let Some(fallback) = parse_cpu_mhz(X86_CPUINFO_WITH_MHZ, cpuset) {
        assert_eq!(fallback, base);
    }

    assert_eq!(parse_cpu_mhz(X86_CPUINFO_WITH_MHZ, None), Some(2250));
    assert_eq!(
        parse_cpu_mhz(X86_CPUINFO_WITH_MHZ, Some(&[1, 3])),
        Some(2250)
    );
    assert_eq!(
        parse_cpu_mhz(X86_CPUINFO_WITH_MHZ, Some(&[1, 2])),
        Some(2750)
    );
    assert_eq!(parse_cpu_mhz(X86_CPUINFO_WITH_MHZ, Some(&[9])), None);
    assert_eq!(parse_cpu_mhz(AARCH64_CPUINFO, None), None);
}

#[test]
fn container_core_list_is_resolved_once_and_stable_across_samples() {
    let stat = "cpu  10000 0 20000 70000 0 0 0 0 0 0
cpu0 2500 0 5000 17500 0 0 0 0 0 0
cpu1 2500 0 5000 17500 0 0 0 0 0 0
cpu2 2500 0 5000 17500 0 0 0 0 0 0
cpu3 2500 0 5000 17500 0 0 0 0 0 0
intr 0
";
    let mut reader = LinuxCpuReader::new();
    reader.container_active_cores = parse_cpu_stat_with_container_limits(stat, &CONTAINER_INFO).1;

    let ids = |reader: &LinuxCpuReader| {
        reader
            .container_core_utilization()
            .into_iter()
            .map(|core| (core.core_id, core.tier_index))
            .collect::<Vec<_>>()
    };
    let first = ids(&reader);
    let second = ids(&reader);
    assert_eq!(first, second);
    // Display ids are renumbered sequentially from the active host cores.
    assert!(
        first
            .iter()
            .enumerate()
            .all(|(position, (core_id, _))| *core_id == position as u32)
    );
    assert!(first.len() <= reader.container_active_cores.len());
}

#[test]
fn lscpu_cache_sizes_parse_l3_before_l2() {
    assert_eq!(
        cache_size_from_lscpu("L2 cache: 2 MiB\nL3 cache: 32 MiB (2 instances)\n"),
        Some(32)
    );
    assert_eq!(cache_size_from_lscpu("  L2: 4096 KiB\n"), Some(4));
    assert_eq!(cache_size_from_lscpu("L3: 1 GiB\n"), Some(1024));
    assert_eq!(cache_size_from_lscpu("L1d: 64 KiB\n"), None);
}
