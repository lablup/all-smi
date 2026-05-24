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

//! Integration tests for the all-smi library API.

use all_smi::prelude::*;

#[test]
fn test_allsmi_creation() {
    let result = AllSmi::new();
    assert!(result.is_ok(), "AllSmi::new() should not fail");
}

#[test]
fn test_allsmi_with_config() {
    let config = AllSmiConfig::new().sample_interval(500).verbose(false);

    let result = AllSmi::with_config(config);
    assert!(result.is_ok(), "AllSmi::with_config() should not fail");
}

#[test]
fn test_gpu_info_does_not_panic() {
    let smi = AllSmi::new().expect("Failed to create AllSmi");
    let gpus = smi.get_gpu_info();
    // Should not panic, may return empty if no GPUs
    println!("Found {} GPU(s)", gpus.len());
}

#[test]
fn test_cpu_info_does_not_panic() {
    let smi = AllSmi::new().expect("Failed to create AllSmi");
    let cpus = smi.get_cpu_info();
    // Should not panic, should return at least one CPU on most systems
    println!("Found {} CPU(s)", cpus.len());
}

#[test]
fn test_memory_info_does_not_panic() {
    let smi = AllSmi::new().expect("Failed to create AllSmi");
    let memory = smi.get_memory_info();
    // Should not panic, should return memory info on all systems
    println!("Found {} memory info(s)", memory.len());
}

#[test]
fn test_process_info_does_not_panic() {
    let smi = AllSmi::new().expect("Failed to create AllSmi");
    let processes = smi.get_process_info();
    // Should not panic, may return empty if no GPU processes
    println!("Found {} GPU process(es)", processes.len());
}

#[test]
fn test_chassis_info_does_not_panic() {
    let smi = AllSmi::new().expect("Failed to create AllSmi");
    let chassis = smi.get_chassis_info();
    // Should not panic, may return None on some platforms
    println!("Chassis info: {:?}", chassis.is_some());
}

#[test]
fn test_helper_methods() {
    let smi = AllSmi::new().expect("Failed to create AllSmi");

    // These should not panic
    let _ = smi.gpu_reader_count();
    let _ = smi.has_gpus();
    let _ = smi.has_cpu_monitoring();
    let _ = smi.has_memory_monitoring();
}

#[test]
fn test_allsmi_is_send() {
    fn assert_send<T: Send>() {}
    assert_send::<AllSmi>();
}

#[test]
fn test_allsmi_is_sync() {
    fn assert_sync<T: Sync>() {}
    assert_sync::<AllSmi>();
}

#[test]
fn test_error_is_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Error>();
}

#[test]
fn test_config_builder_pattern() {
    let config = AllSmiConfig::new().sample_interval(100).verbose(true);

    assert_eq!(config.sample_interval_ms, 100);
    assert!(config.verbose);
}

#[test]
fn test_device_type_display() {
    assert_eq!(DeviceType::NvidiaGpu.to_string(), "NVIDIA GPU");
    assert_eq!(DeviceType::AmdGpu.to_string(), "AMD GPU");
    assert_eq!(DeviceType::AppleSiliconGpu.to_string(), "Apple Silicon GPU");
    assert_eq!(DeviceType::IntelGaudi.to_string(), "Intel Gaudi");
    assert_eq!(DeviceType::GoogleTpu.to_string(), "Google TPU");
}

#[test]
fn test_error_display() {
    let err = Error::PlatformInit("test error".to_string());
    assert!(err.to_string().contains("Platform initialization failed"));
    assert!(err.to_string().contains("test error"));

    let err = Error::NoDevicesFound;
    assert!(err.to_string().contains("No supported devices found"));

    let err = Error::DeviceAccess("device 0".to_string());
    assert!(err.to_string().contains("Device access error"));

    let err = Error::PermissionDenied("root required".to_string());
    assert!(err.to_string().contains("Permission denied"));

    let err = Error::NotSupported("feature X".to_string());
    assert!(err.to_string().contains("not supported"));
}

#[test]
fn test_prelude_exports() {
    // Verify all expected types are exported via prelude
    fn _check_types() {
        let _: fn() -> Result<AllSmi> = || AllSmi::new();
        let _: AllSmiConfig = AllSmiConfig::default();
        let _: DeviceType = DeviceType::NvidiaGpu;
    }
}

#[test]
fn test_multiple_allsmi_instances() {
    // Create multiple instances to verify no global state conflicts
    let smi1 = AllSmi::new().expect("First AllSmi instance");
    let smi2 = AllSmi::new().expect("Second AllSmi instance");

    // Both should work
    let _ = smi1.get_gpu_info();
    let _ = smi2.get_gpu_info();
}

#[test]
fn test_allsmi_drop() {
    // Create and immediately drop to verify cleanup
    {
        let smi = AllSmi::new().expect("AllSmi instance");
        let _ = smi.get_gpu_info();
        // smi goes out of scope here and Drop is called
    }

    // Create another to verify we can reinitialize
    let smi = AllSmi::new().expect("Second AllSmi instance after drop");
    let _ = smi.get_cpu_info();
}

// =============================================================================
// Targeted refresh and stable correlation IDs (issue #211)
// =============================================================================

#[test]
fn test_get_gpu_by_uuid_miss_returns_none() {
    let smi = AllSmi::new().expect("Failed to create AllSmi");
    let absent = smi.get_gpu_by_uuid("definitely-not-a-real-uuid-00000000");
    assert!(
        absent.is_none(),
        "get_gpu_by_uuid for an unknown UUID must return None"
    );
}

#[test]
fn test_get_gpu_by_uuid_hit_roundtrips_for_first_device() {
    // If the host happens to expose at least one GPU/NPU, looking it up by
    // its own uuid must return Some with the same uuid. On hosts without
    // any accelerator this becomes a no-op (the assertion body never runs)
    // — which is fine; the miss case is covered above.
    let smi = AllSmi::new().expect("Failed to create AllSmi");
    let snapshot = smi.get_gpu_info();
    if let Some(first) = snapshot.first() {
        let fetched = smi
            .get_gpu_by_uuid(&first.uuid)
            .expect("a device that was just enumerated must still be present");
        assert_eq!(fetched.uuid, first.uuid);
    }
}

#[test]
fn test_refresh_gpu_unknown_uuid_returns_false_and_does_not_mutate() {
    let smi = AllSmi::new().expect("Failed to create AllSmi");
    let mut snapshot = smi.get_gpu_info();
    if let Some(first) = snapshot.first_mut() {
        // Tamper the uuid so the refresh path cannot find it.
        let original_uuid = first.uuid.clone();
        let original_name = first.name.clone();
        first.uuid = "no-such-uuid-211-test".to_string();

        let found = smi.refresh_gpu(first);
        assert!(
            !found,
            "refresh_gpu must return false when the uuid is unknown"
        );
        // The struct is left untouched on miss.
        assert_eq!(
            first.uuid, "no-such-uuid-211-test",
            "tampered uuid must remain unchanged on miss"
        );
        assert_eq!(
            first.name, original_name,
            "other fields must remain unchanged on miss"
        );

        // Restore the original uuid and ensure a real refresh works.
        first.uuid = original_uuid;
        let found = smi.refresh_gpu(first);
        assert!(
            found,
            "refresh_gpu must return true for a uuid that is still present"
        );
    }
}

#[test]
fn test_cpu_index_is_stable_across_consecutive_calls() {
    let smi = AllSmi::new().expect("Failed to create AllSmi");
    let first = smi.get_cpu_info();
    let second = smi.get_cpu_info();

    assert_eq!(
        first.len(),
        second.len(),
        "CPU topology is static; CpuInfo count must match across calls"
    );

    for (a, b) in first.iter().zip(second.iter()) {
        assert_eq!(
            a.index, b.index,
            "CpuInfo.index must be stable across consecutive get_cpu_info() calls"
        );
        // The position-based assignment guarantees a dense 0..N indexing.
        assert!(
            (a.index as usize) < first.len(),
            "CpuInfo.index must be within the bounds of the returned vector"
        );
    }

    // First entry — if any — must be index 0 (dense, 0-based).
    if let Some(c) = first.first() {
        assert_eq!(c.index, 0, "first CpuInfo.index must be 0");
    }
}

#[test]
fn test_memory_index_is_stable_across_consecutive_calls() {
    let smi = AllSmi::new().expect("Failed to create AllSmi");
    let first = smi.get_memory_info();
    let second = smi.get_memory_info();

    assert_eq!(
        first.len(),
        second.len(),
        "Memory enumeration must be stable across calls"
    );

    for (a, b) in first.iter().zip(second.iter()) {
        assert_eq!(
            a.index, b.index,
            "MemoryInfo.index must be stable across consecutive get_memory_info() calls"
        );
    }

    if let Some(m) = first.first() {
        assert_eq!(m.index, 0, "first MemoryInfo.index must be 0");
    }
}

#[test]
fn test_get_cpu_by_index_hit_and_miss() {
    let smi = AllSmi::new().expect("Failed to create AllSmi");
    let cpus = smi.get_cpu_info();
    if let Some(first) = cpus.first() {
        let fetched = smi
            .get_cpu_by_index(first.index)
            .expect("a CPU that was just enumerated must still be retrievable by index");
        assert_eq!(fetched.index, first.index);
    }

    // Out-of-range index must miss.
    let bogus = smi.get_cpu_by_index(u32::MAX);
    assert!(bogus.is_none(), "out-of-range index must return None");
}

#[test]
fn test_refresh_cpu_in_place() {
    let smi = AllSmi::new().expect("Failed to create AllSmi");
    let mut cpus = smi.get_cpu_info();
    if let Some(first) = cpus.first_mut() {
        let original_index = first.index;
        let refreshed = smi.refresh_cpu(first);
        assert!(refreshed, "refresh_cpu must succeed for a known index");
        assert_eq!(
            first.index, original_index,
            "refresh_cpu must preserve the stable index"
        );
    }

    // Refresh a fabricated CpuInfo whose index does not exist.
    if let Some(template) = smi.get_cpu_info().first().cloned() {
        let mut bogus = template;
        bogus.index = u32::MAX;
        let result = smi.refresh_cpu(&mut bogus);
        assert!(
            !result,
            "refresh_cpu must return false for an unknown index"
        );
        assert_eq!(
            bogus.index,
            u32::MAX,
            "the original struct must remain untouched on miss"
        );
    }
}

#[test]
fn test_refresh_memory_in_place() {
    let smi = AllSmi::new().expect("Failed to create AllSmi");
    let mut mems = smi.get_memory_info();
    if let Some(first) = mems.first_mut() {
        let original_index = first.index;
        let refreshed = smi.refresh_memory(first);
        assert!(refreshed, "refresh_memory must succeed for a known index");
        assert_eq!(first.index, original_index);
    }
}
