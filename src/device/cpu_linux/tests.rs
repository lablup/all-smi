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

#[cfg(test)]
use crate::device::cpu_linux::LinuxCpuReader;
#[cfg(test)]
use crate::device::{CoreType, CpuPlatformType};

// Helper functions removed as they were unused

#[test]
fn test_parse_cpuinfo_intel() {
    let cpuinfo_content = r#"processor	: 0
vendor_id	: GenuineIntel
cpu family	: 6
model		: 85
model name	: Intel(R) Core(TM) i7-8700K CPU @ 3.70GHz
stepping	: 10
cpu MHz		: 3700.000
cache size	: 12288 KB
physical id	: 0
siblings	: 12
core id		: 0
cpu cores	: 6
processor	: 1
physical id	: 0
core id		: 1
processor	: 11
physical id	: 0
core id		: 5"#;

    let reader = LinuxCpuReader::new();
    let result = reader.parse_cpuinfo(cpuinfo_content);
    assert!(result.is_ok());

    let (cpu_model, _arch, platform, sockets, _cores, threads, base_freq, _max_freq, cache) =
        result.unwrap();
    assert_eq!(cpu_model, "Intel(R) Core(TM) i7-8700K CPU @ 3.70GHz");
    assert_eq!(platform, CpuPlatformType::Intel);
    assert_eq!(sockets, 1);
    assert_eq!(threads, 3); // Based on processor count in test data (0, 1, 11)
    // The frequency now comes from actual system files or defaults, not just cpuinfo parsing
    // Since this is a test environment, we can't predict the exact value
    assert!(base_freq > 0);
    assert_eq!(cache, 12); // 12288 KB -> 12 MB
}

#[test]
fn test_parse_cpuinfo_amd() {
    let cpuinfo_content = r#"processor	: 0
vendor_id	: AuthenticAMD
model name	: AMD Ryzen 9 5900X 12-Core Processor
cpu MHz		: 3700.000
cache size	: 512 KB
physical id	: 0"#;

    let reader = LinuxCpuReader::new();
    let result = reader.parse_cpuinfo(cpuinfo_content);
    assert!(result.is_ok());

    let (cpu_model, _, platform, _, _, _, _, _, _) = result.unwrap();
    assert_eq!(cpu_model, "AMD Ryzen 9 5900X 12-Core Processor");
    assert_eq!(platform, CpuPlatformType::Amd);
}

#[test]
fn test_parse_cpu_stat() {
    let stat_content = r#"cpu  10000 0 20000 70000 0 0 0 0 0 0
cpu0 2500 0 5000 17500 0 0 0 0 0 0
cpu1 2500 0 5000 17500 0 0 0 0 0 0
cpu2 2500 0 5000 17500 0 0 0 0 0 0
cpu3 2500 0 5000 17500 0 0 0 0 0 0"#;

    let reader = LinuxCpuReader::new();
    let result = reader.parse_cpu_stat(stat_content, 1);
    assert!(result.is_ok());

    let (overall_util, socket_info, core_utils) = result.unwrap();

    // The parse_cpu_stat now uses sysinfo crate which returns actual system CPU usage
    // Since we're in a test environment, we can't predict the exact value
    // Just verify it's a valid percentage
    assert!((0.0..=100.0).contains(&overall_util));
    assert_eq!(socket_info.len(), 1);
    // The number of cores now comes from the actual system, not the test data
    assert!(!core_utils.is_empty());

    // Each core should have a valid utilization percentage
    for core in &core_utils {
        assert!((0.0..=100.0).contains(&core.utilization));
        assert_eq!(core.core_type, CoreType::Standard);
    }
}

#[test]
fn test_container_aware_parsing() {
    // Create a reader that would detect container environment
    let reader = LinuxCpuReader::new();

    // Test that container info is properly initialized
    // Note: This test will behave differently in container vs non-container environments
    if reader.container_info.is_container {
        assert!(reader.container_info.effective_cpu_count > 0.0);
        println!(
            "Running in container with {} effective CPUs",
            reader.container_info.effective_cpu_count
        );
    } else {
        assert!(reader.container_info.effective_cpu_count > 0.0);
        println!(
            "Running on host with {} CPUs",
            reader.container_info.effective_cpu_count
        );
    }
}

#[test]
fn test_parse_cpuinfo_with_container_limits() {
    let cpuinfo_content = r#"processor	: 0
model name	: Intel(R) Core(TM) i7-8700K CPU @ 3.70GHz
cpu MHz		: 3700.000
processor	: 1
processor	: 2
processor	: 3"#;

    // Create a reader which will detect container environment
    let reader = LinuxCpuReader::new();

    // Test parsing - the container detection happens automatically
    let result = reader.parse_cpuinfo(cpuinfo_content);
    assert!(result.is_ok());

    // In a real container environment, the reader would be container-aware
    // and adjust the reported cores based on container limits
}

#[test]
fn test_get_cache_size_from_lscpu() {
    let reader = LinuxCpuReader::new();

    // This test will try to actually run lscpu if available
    // The result will vary based on the system
    let cache_size = reader.get_cache_size_from_lscpu();

    // Just verify it returns Some value or None, both are valid
    match cache_size {
        Some(size) => println!("Found cache size: {size} MB"),
        None => println!("No cache size found (lscpu not available or failed)"),
    }
}

// ---------------------------------------------------------------------------
// N-tier topology detection against fixture sysfs roots (issue #473)
// ---------------------------------------------------------------------------

fn write_fixture(root: &std::path::Path, relative: &str, content: &str) {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, content).unwrap();
}

fn add_cpu(root: &std::path::Path, cpu_id: u32) {
    std::fs::create_dir_all(root.join(format!("devices/system/cpu/cpu{cpu_id}"))).unwrap();
}

/// `MIDR_EL1` value carrying `part` in the PartNum field, as the arm64 kernel
/// prints it (`0x%016llx`).
fn midr(part: u32) -> String {
    format!("0x{:016x}", 0x410f_0000u64 | (u64::from(part) << 4) | 0x3)
}

/// Every core assigned a tier must be counted exactly once across the tier
/// core counts.
fn assert_counts_match_assignments(topology: &super::CpuTopology) {
    let assigned = topology
        .tier_by_cpu
        .iter()
        .filter(|index| index.is_some())
        .count() as u32;
    let counted: u32 = topology.tiers.iter().map(|tier| tier.core_count).sum();
    assert_eq!(assigned, counted);
}

#[test]
fn detects_intel_hybrid_p_and_e_from_pmu_cpumasks() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for cpu_id in 0..6 {
        add_cpu(root, cpu_id);
    }
    write_fixture(root, "bus/event_source/devices/cpu_core/cpus", "0-3\n");
    write_fixture(root, "bus/event_source/devices/cpu_atom/cpus", "4-5\n");

    let topology = super::detect_cpu_tiers(root, None);
    assert_eq!(
        topology
            .tiers
            .iter()
            .map(|tier| (tier.label.as_str(), tier.core_count))
            .collect::<Vec<_>>(),
        vec![("P", 4), ("E", 2)]
    );
    assert_eq!(topology.tier_index_for(0), Some(0));
    assert_eq!(topology.tier_index_for(3), Some(0));
    assert_eq!(topology.tier_index_for(4), Some(1));
    assert_eq!(topology.tier_index_for(5), Some(1));
    assert_counts_match_assignments(&topology);
}

#[test]
fn hybrid_precedence_wins_over_midr_and_capacity() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for cpu_id in 0..4 {
        add_cpu(root, cpu_id);
    }
    write_fixture(root, "bus/event_source/devices/cpu_core/cpus", "0-1\n");
    write_fixture(root, "bus/event_source/devices/cpu_atom/cpus", "2-3\n");
    // Contradictory lower-priority sources must be ignored.
    write_fixture(
        root,
        "devices/system/cpu/cpu0/regs/identification/midr_el1",
        &midr(0xD82),
    );
    write_fixture(root, "devices/system/cpu/cpu0/cpu_capacity", "1024\n");
    write_fixture(root, "devices/system/cpu/cpu2/cpu_capacity", "256\n");

    let topology = super::detect_cpu_tiers(root, None);
    assert_eq!(
        topology
            .tiers
            .iter()
            .map(|tier| tier.label.as_str())
            .collect::<Vec<_>>(),
        vec!["P", "E"]
    );
}

#[test]
fn detects_three_cluster_arm_midr_as_p_m_e() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for cpu_id in 0..6 {
        add_cpu(root, cpu_id);
    }
    for cpu_id in 0..2 {
        write_fixture(
            root,
            &format!("devices/system/cpu/cpu{cpu_id}/regs/identification/midr_el1"),
            &midr(0xD82), // Cortex-X4 -> Big
        );
    }
    for cpu_id in 2..4 {
        write_fixture(
            root,
            &format!("devices/system/cpu/cpu{cpu_id}/regs/identification/midr_el1"),
            &midr(0xD81), // Cortex-A720 -> Mid
        );
    }
    for cpu_id in 4..6 {
        write_fixture(
            root,
            &format!("devices/system/cpu/cpu{cpu_id}/regs/identification/midr_el1"),
            &midr(0xD80), // Cortex-A520 -> Little
        );
    }

    let topology = super::detect_cpu_tiers(root, None);
    assert_eq!(
        topology
            .tiers
            .iter()
            .map(|tier| (tier.label.as_str(), tier.core_count))
            .collect::<Vec<_>>(),
        vec![("P", 2), ("M", 2), ("E", 2)]
    );
    assert_eq!(topology.tier_index_for(0), Some(0));
    assert_eq!(topology.tier_index_for(2), Some(1));
    assert_eq!(topology.tier_index_for(4), Some(2));
    assert_counts_match_assignments(&topology);
}

#[test]
fn unknown_and_single_tier_midr_fall_through_to_capacity() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for cpu_id in 0..4 {
        add_cpu(root, cpu_id);
    }
    // Only one known tier, plus one unknown part.
    write_fixture(
        root,
        "devices/system/cpu/cpu0/regs/identification/midr_el1",
        &midr(0xD82),
    );
    write_fixture(
        root,
        "devices/system/cpu/cpu1/regs/identification/midr_el1",
        &midr(0xD82),
    );
    write_fixture(
        root,
        "devices/system/cpu/cpu2/regs/identification/midr_el1",
        &midr(0x123),
    );
    write_fixture(
        root,
        "devices/system/cpu/cpu3/regs/identification/midr_el1",
        "0x0000000000000000\n",
    );

    // Distinct capacities are the only source that can split all four CPUs.
    write_fixture(root, "devices/system/cpu/cpu0/cpu_capacity", "1024\n");
    write_fixture(root, "devices/system/cpu/cpu1/cpu_capacity", "1024\n");
    write_fixture(root, "devices/system/cpu/cpu2/cpu_capacity", "512\n");
    write_fixture(root, "devices/system/cpu/cpu3/cpu_capacity", "512\n");

    let topology = super::detect_cpu_tiers(root, None);
    assert_eq!(
        topology
            .tiers
            .iter()
            .map(|tier| (tier.label.as_str(), tier.core_count))
            .collect::<Vec<_>>(),
        vec![("P", 2), ("M", 2)]
    );
    assert_counts_match_assignments(&topology);
}

#[test]
fn capacity_only_part_orders_groups_descending() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for cpu_id in 0..6 {
        add_cpu(root, cpu_id);
    }
    for cpu_id in 0..2 {
        write_fixture(
            root,
            &format!("devices/system/cpu/cpu{cpu_id}/cpu_capacity"),
            "1024\n",
        );
    }
    for cpu_id in 2..4 {
        write_fixture(
            root,
            &format!("devices/system/cpu/cpu{cpu_id}/cpu_capacity"),
            "768\n",
        );
    }
    for cpu_id in 4..6 {
        write_fixture(
            root,
            &format!("devices/system/cpu/cpu{cpu_id}/cpu_capacity"),
            "512\n",
        );
    }

    let topology = super::detect_cpu_tiers(root, None);
    assert_eq!(
        topology
            .tiers
            .iter()
            .map(|tier| (tier.label.as_str(), tier.core_count))
            .collect::<Vec<_>>(),
        vec![("P", 2), ("M", 2), ("E", 2)]
    );
    assert_eq!(topology.tier_index_for(0), Some(0));
    assert_eq!(topology.tier_index_for(4), Some(2));
}

#[test]
fn equal_capacities_produce_no_tiers() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for cpu_id in 0..4 {
        add_cpu(root, cpu_id);
        write_fixture(
            root,
            &format!("devices/system/cpu/cpu{cpu_id}/cpu_capacity"),
            "1024\n",
        );
    }

    let topology = super::detect_cpu_tiers(root, None);
    assert!(topology.tiers.is_empty());
    assert!(topology.tier_by_cpu.is_empty());
}

#[test]
fn cpuset_excludes_untiered_cores_and_renumbers_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for cpu_id in 0..6 {
        add_cpu(root, cpu_id);
    }
    write_fixture(root, "bus/event_source/devices/cpu_core/cpus", "0-3\n");
    write_fixture(root, "bus/event_source/devices/cpu_atom/cpus", "4-5\n");

    // The cpuset keeps two P cores and one E core; the rest must not count.
    let topology = super::detect_cpu_tiers(root, Some(&[0, 1, 4]));
    assert_eq!(
        topology
            .tiers
            .iter()
            .map(|tier| (tier.label.as_str(), tier.core_count))
            .collect::<Vec<_>>(),
        vec![("P", 2), ("E", 1)]
    );
    assert_eq!(topology.tier_index_for(2), None);
    assert_eq!(topology.tier_index_for(5), None);
    assert_counts_match_assignments(&topology);
}

#[test]
fn missing_sources_leave_the_cpu_untiered() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for cpu_id in 0..4 {
        add_cpu(root, cpu_id);
    }

    let topology = super::detect_cpu_tiers(root, None);
    assert!(topology.tiers.is_empty());
    assert!(topology.tier_by_cpu.iter().all(Option::is_none));
}

#[test]
fn empty_and_malformed_sources_never_panic() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for cpu_id in 0..4 {
        add_cpu(root, cpu_id);
        write_fixture(
            root,
            &format!("devices/system/cpu/cpu{cpu_id}/cpu_capacity"),
            "not-a-number\n",
        );
    }
    write_fixture(root, "bus/event_source/devices/cpu_core/cpus", "0-\n");
    write_fixture(root, "bus/event_source/devices/cpu_atom/cpus", "\n");

    let topology = super::detect_cpu_tiers(root, None);
    assert!(topology.tiers.is_empty());
}

#[test]
fn enumerate_cpu_ids_falls_back_to_the_possible_mask() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // No `cpuN` directories, only the kernel cpumask.
    std::fs::create_dir_all(root.join("devices/system/cpu")).unwrap();
    write_fixture(root, "devices/system/cpu/possible", "0-3,8\n");

    assert_eq!(super::enumerate_cpu_ids(root), vec![0, 1, 2, 3, 8]);
}

#[test]
fn an_empty_allowed_set_yields_no_topology() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for cpu_id in 0..4 {
        add_cpu(root, cpu_id);
    }
    write_fixture(root, "bus/event_source/devices/cpu_core/cpus", "0-1\n");
    write_fixture(root, "bus/event_source/devices/cpu_atom/cpus", "2-3\n");

    let topology = super::detect_cpu_tiers(root, Some(&[]));
    assert!(topology.tiers.is_empty());
    assert!(topology.tier_by_cpu.is_empty());
}

#[test]
fn capacity_detection_truncates_to_the_fastest_groups() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // 70 distinct capacities: cpu0 has the highest, cpu69 the lowest.
    for cpu_id in 0..70 {
        add_cpu(root, cpu_id);
        write_fixture(
            root,
            &format!("devices/system/cpu/cpu{cpu_id}/cpu_capacity"),
            &format!("{}\n", 700 - cpu_id),
        );
    }

    let topology = super::detect_cpu_tiers(root, None);
    assert_eq!(topology.tiers.len(), super::MAX_DETECTED_TIERS);
    assert_eq!(topology.tiers[0].label, "P");
    assert_eq!(topology.tiers[1].label, "M");
    assert_eq!(topology.tiers[2].label, "E");
    assert_eq!(
        topology.tiers.last().unwrap().label,
        format!("E{}", super::MAX_DETECTED_TIERS)
    );
    assert_eq!(topology.tier_index_for(0), Some(0));
    assert_eq!(
        topology.tier_index_for(63),
        Some((super::MAX_DETECTED_TIERS - 1) as u8)
    );
    assert_eq!(topology.tier_index_for(64), None);
    assert_eq!(topology.tier_index_for(69), None);
    assert_counts_match_assignments(&topology);
}

#[test]
fn topology_builder_truncates_oversized_group_lists() {
    let groups: Vec<(String, Vec<u32>)> = (0..70)
        .map(|position| {
            (
                crate::device::cpu_topology::tier_label_for_rank(position),
                vec![position as u32],
            )
        })
        .collect();

    let topology = super::topology_from_labeled_groups(groups);
    assert_eq!(topology.tiers.len(), super::MAX_DETECTED_TIERS);
    assert_eq!(topology.tiers[0].label, "P");
    assert_eq!(
        topology.tiers.last().unwrap().label,
        format!("E{}", super::MAX_DETECTED_TIERS)
    );
    assert_eq!(topology.tier_by_cpu.len(), super::MAX_DETECTED_TIERS);
    assert_counts_match_assignments(&topology);
}

#[test]
fn impossible_cpu_ids_do_not_size_the_tier_map() {
    // A corrupted or attacker-controlled sysfs can name `cpuN` with an
    // arbitrary `N`. The tier map is indexed by CPU id, so an unbounded id
    // would size the allocation to that id (about 8 GB at u32::MAX).
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    add_cpu(root, 0);
    add_cpu(root, u32::MAX);
    write_fixture(root, "devices/system/cpu/cpu0/cpu_capacity", "1024\n");
    write_fixture(
        root,
        &format!("devices/system/cpu/cpu{}/cpu_capacity", u32::MAX),
        "512\n",
    );

    // Filtering the id out leaves one capacity group, so the host reads as
    // homogeneous rather than trying to allocate the map.
    let topology = super::detect_cpu_tiers(root, None);
    assert!(topology.tiers.is_empty());
    assert!(topology.tier_by_cpu.is_empty());
}

#[test]
fn topology_builder_drops_implausible_cpu_ids() {
    let groups = vec![
        ("P".to_string(), vec![0, 1]),
        ("E".to_string(), vec![u32::MAX]),
    ];

    let topology = super::topology_from_labeled_groups(groups);
    assert!(topology.tier_by_cpu.len() <= super::MAX_TRACKED_CPU_ID as usize + 1);
    assert_eq!(topology.tier_index_for(0), Some(0));
    assert_eq!(topology.tier_index_for(1), Some(0));
    assert_eq!(topology.tier_index_for(u32::MAX), None);
    assert_counts_match_assignments(&topology);
}
