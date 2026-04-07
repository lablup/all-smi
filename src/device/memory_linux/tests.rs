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
use crate::device::memory_linux::LinuxMemoryReader;
#[cfg(test)]
use crate::device::MemoryReader;

#[test]
fn test_memory_reader_creation() {
    let reader = LinuxMemoryReader::new();
    // Reader should be created successfully
    // Container info is detected during creation
    if let Some(container_info) = reader.container_info {
        if container_info.is_container {
            println!("Created memory reader in container environment");
        } else {
            println!("Created memory reader in non-container environment");
        }
    } else {
        println!("Created memory reader with no container info");
    }
}

#[test]
fn test_memory_info_retrieval() {
    let reader = LinuxMemoryReader::new();
    let memory_infos = reader.get_memory_info();

    assert!(!memory_infos.is_empty());

    let memory_info = &memory_infos[0];

    // Basic sanity checks
    assert!(memory_info.total_bytes > 0);
    assert!(memory_info.utilization >= 0.0 && memory_info.utilization <= 100.0);

    if let Some(container_info) = reader.container_info {
        if container_info.is_container {
            println!("Container memory:");
            println!("  Total: {} MB", memory_info.total_bytes / 1024 / 1024);
            println!("  Used: {} MB", memory_info.used_bytes / 1024 / 1024);
            println!("  Utilization: {:.2}%", memory_info.utilization);

            // In container, total should match container limit if available
            if let Some(limit) = container_info.memory_limit_bytes {
                assert_eq!(memory_info.total_bytes, limit);
            }
        } else {
            println!("System memory:");
            println!("  Total: {} MB", memory_info.total_bytes / 1024 / 1024);
            println!("  Used: {} MB", memory_info.used_bytes / 1024 / 1024);
            println!(
                "  Available: {} MB",
                memory_info.available_bytes / 1024 / 1024
            );
            println!("  Cached: {} MB", memory_info.cached_bytes / 1024 / 1024);
            println!("  Utilization: {:.2}%", memory_info.utilization);
        }
    } else {
        println!("No container info available");
    }
}

#[test]
fn test_container_memory_detection() {
    // Create a reader which will detect container environment
    let reader = LinuxMemoryReader::new();

    // The reader automatically detects container environment
    // and reads memory limits from cgroups if available
    let memory_infos = reader.get_memory_info();
    assert!(!memory_infos.is_empty());

    let memory_info = &memory_infos[0];

    // In a real container environment, the reader would report
    // container memory limits instead of host memory
    if let Some(container_info) = reader.container_info {
        if container_info.is_container {
            println!("Container memory detected:");
            if let Some(limit) = container_info.memory_limit_bytes {
                println!("  Memory limit: {} MB", limit / 1024 / 1024);
                // Total should match the container limit
                assert_eq!(memory_info.total_bytes, limit);
            }
        } else {
            println!("Host memory detected:");
            println!("  Total: {} MB", memory_info.total_bytes / 1024 / 1024);
        }
    } else {
        println!("No container info available");
    }
}
