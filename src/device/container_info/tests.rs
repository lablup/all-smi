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
use crate::device::container_info::ContainerInfo;

#[test]
fn test_parse_cpuset_range() {
    // Test single CPU
    let result = ContainerInfo::parse_cpuset_range("0");
    assert_eq!(result, Some(vec![0]));

    // Test CPU range
    let result = ContainerInfo::parse_cpuset_range("0-3");
    assert_eq!(result, Some(vec![0, 1, 2, 3]));

    // Test multiple CPUs
    let result = ContainerInfo::parse_cpuset_range("0,2,4");
    assert_eq!(result, Some(vec![0, 2, 4]));

    // Test mixed range and individual CPUs
    let result = ContainerInfo::parse_cpuset_range("0-2,5,7-8");
    assert_eq!(result, Some(vec![0, 1, 2, 5, 7, 8]));

    // Test empty string
    let result = ContainerInfo::parse_cpuset_range("");
    assert_eq!(result, None);

    // Test invalid input
    let result = ContainerInfo::parse_cpuset_range("invalid");
    assert_eq!(result, None);
}

#[test]
fn test_calculate_effective_cpus() {
    // Test with no limits
    let effective = ContainerInfo::calculate_effective_cpus(None, None, None, &None);
    assert!(effective > 0.0); // Should be system CPU count

    // Test with quota limit (2 CPUs worth)
    let effective = ContainerInfo::calculate_effective_cpus(
        Some(200000), // 200ms quota
        Some(100000), // 100ms period
        None,
        &None,
    );
    assert_eq!(effective, 2.0);

    // Test with quota limit (0.5 CPUs)
    let effective = ContainerInfo::calculate_effective_cpus(
        Some(50000),  // 50ms quota
        Some(100000), // 100ms period
        None,
        &None,
    );
    assert_eq!(effective, 0.5);

    // Test with cpuset limit
    let cpuset = Some(vec![0, 1, 2, 3]);
    let effective = ContainerInfo::calculate_effective_cpus(None, None, None, &cpuset);
    assert_eq!(effective, 4.0);

    // Test with both quota and cpuset (quota more restrictive)
    let effective = ContainerInfo::calculate_effective_cpus(
        Some(100000), // 100ms quota = 1 CPU
        Some(100000), // 100ms period
        None,
        &cpuset,
    );
    assert_eq!(effective, 1.0); // Min of quota (1) and cpuset (4)

    // Test with both quota and cpuset (cpuset more restrictive)
    let cpuset = Some(vec![0, 1]);
    let effective = ContainerInfo::calculate_effective_cpus(
        Some(300000), // 300ms quota = 3 CPUs
        Some(100000), // 100ms period
        None,
        &cpuset,
    );
    assert_eq!(effective, 2.0); // Min of quota (3) and cpuset (2)
}

#[cfg(target_os = "linux")]
#[test]
fn test_container_detection() {
    // This test will pass/fail depending on whether it's run in a container
    let info = ContainerInfo::detect();

    // Just verify the struct is created properly
    assert!(info.effective_cpu_count > 0.0);

    if info.is_container {
        println!(
            "Running in container with {} effective CPUs",
            info.effective_cpu_count
        );
        if let Some(quota) = info.cpu_quota {
            println!("CPU quota: {quota}");
        }
        if let Some(period) = info.cpu_period {
            println!("CPU period: {period}");
        }
        if let Some(cpuset) = &info.cpuset_cpus {
            println!("CPUSet: {cpuset:?}");
        }
        if let Some(mem_limit) = info.memory_limit_bytes {
            println!("Memory limit: {} MB", mem_limit / 1024 / 1024);
        }
    } else {
        println!("Not running in a container");
    }
}

#[test]
fn test_memory_limit_detection() {
    // Test that memory limit detection works
    let info = ContainerInfo::detect();

    if info.is_container {
        // In a container, we should have memory limits
        if let Some(limit) = info.memory_limit_bytes {
            assert!(limit > 0);
            println!("Container memory limit: {limit} bytes");
        }
    } else {
        // Not in a container, memory limits should be None
        assert!(info.memory_limit_bytes.is_none());
    }
}

#[test]
fn test_get_current_memory_usage() {
    let info = ContainerInfo::detect();

    if info.is_container {
        // In a container, we should be able to get memory usage
        let usage = info.get_current_memory_usage();
        if let Some(usage_bytes) = usage {
            assert!(usage_bytes > 0);
            println!("Current memory usage: {} MB", usage_bytes / 1024 / 1024);
        }
    }
}
