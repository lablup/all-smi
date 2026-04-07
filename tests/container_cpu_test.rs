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

#[cfg(target_os = "linux")]
#[cfg(test)]
mod tests {
    use all_smi::device::container_info::{ContainerInfo, parse_cpu_stat_with_container_limits};

    #[test]
    fn test_container_cpu_limits_with_cpuset() {
        // Mock container info with cpuset limiting to CPUs 0,1
        let mut container_info = ContainerInfo::detect();
        container_info.is_container = true;
        container_info.cpuset_cpus = Some(vec![0, 1]);
        container_info.effective_cpu_count = 2.0;

        // Mock /proc/stat content with 4 CPUs
        let stat_content = r#"cpu  1000 0 2000 5000 0 0 0 0 0 0
cpu0 250 0 500 1250 0 0 0 0 0 0
cpu1 250 0 500 1250 0 0 0 0 0 0
cpu2 250 0 500 1250 0 0 0 0 0 0
cpu3 250 0 500 1250 0 0 0 0 0 0
"#;

        let (_utilization, active_cores) =
            parse_cpu_stat_with_container_limits(stat_content, &container_info);

        // Should only include CPUs 0 and 1
        assert_eq!(active_cores.len(), 2);
        assert!(active_cores.contains(&0));
        assert!(active_cores.contains(&1));
        assert!(!active_cores.contains(&2));
        assert!(!active_cores.contains(&3));
    }

    #[test]
    fn test_container_cpu_limits_without_cpuset() {
        // Mock container info without cpuset but with CPU quota
        let mut container_info = ContainerInfo::detect();
        container_info.is_container = true;
        container_info.cpuset_cpus = None;
        container_info.effective_cpu_count = 2.0;

        // Mock /proc/stat content with 4 CPUs
        let stat_content = r#"cpu  1000 0 2000 5000 0 0 0 0 0 0
cpu0 250 0 500 1250 0 0 0 0 0 0
cpu1 250 0 500 1250 0 0 0 0 0 0
cpu2 250 0 500 1250 0 0 0 0 0 0
cpu3 250 0 500 1250 0 0 0 0 0 0
"#;

        let (_utilization, active_cores) =
            parse_cpu_stat_with_container_limits(stat_content, &container_info);

        // Should limit to 2 CPUs based on effective_cpu_count
        assert_eq!(active_cores.len(), 2);
        assert!(active_cores.contains(&0));
        assert!(active_cores.contains(&1));
        // Should not include CPUs beyond the effective count
        assert!(!active_cores.contains(&2));
        assert!(!active_cores.contains(&3));
    }

    #[test]
    fn test_non_container_shows_all_cpus() {
        // Mock non-container environment
        let mut container_info = ContainerInfo::detect();
        container_info.is_container = false;
        container_info.cpuset_cpus = None;
        container_info.effective_cpu_count = 4.0;

        // Mock /proc/stat content with 4 CPUs
        let stat_content = r#"cpu  1000 0 2000 5000 0 0 0 0 0 0
cpu0 250 0 500 1250 0 0 0 0 0 0
cpu1 250 0 500 1250 0 0 0 0 0 0
cpu2 250 0 500 1250 0 0 0 0 0 0
cpu3 250 0 500 1250 0 0 0 0 0 0
"#;

        let (_utilization, active_cores) =
            parse_cpu_stat_with_container_limits(stat_content, &container_info);

        // Should include all 4 CPUs when not in a container
        assert_eq!(active_cores.len(), 4);
        assert!(active_cores.contains(&0));
        assert!(active_cores.contains(&1));
        assert!(active_cores.contains(&2));
        assert!(active_cores.contains(&3));
    }
}
