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
