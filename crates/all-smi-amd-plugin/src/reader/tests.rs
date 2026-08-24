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
mod tests {
    use super::*;

    #[test]
    fn test_max_version_component_validation() {
        // Test that MAX_VERSION_COMPONENT is reasonable for Linux kernel versions
        const {
            assert!(
                MAX_VERSION_COMPONENT >= 99,
                "Should support two-digit version components"
            );
            assert!(
                MAX_VERSION_COMPONENT <= 9999,
                "Should not be excessively large"
            );
        }

        // Common kernel version components should be valid
        let common_versions = vec![
            (6, 12, 0),      // Linux 6.12.0
            (5, 15, 0),      // Linux 5.15.0 LTS
            (30, 10, 1),     // AMD driver version
            (999, 999, 999), // Maximum allowed
        ];

        for (major, minor, patch) in common_versions {
            assert!(
                major <= MAX_VERSION_COMPONENT,
                "Major version {major} should be valid"
            );
            assert!(
                minor <= MAX_VERSION_COMPONENT,
                "Minor version {minor} should be valid"
            );
            assert!(
                patch <= MAX_VERSION_COMPONENT,
                "Patch version {patch} should be valid"
            );
        }
    }

    #[test]
    fn test_version_validation_rejects_invalid() {
        // Test that we reject unreasonable version numbers
        let invalid_versions = vec![
            (1000, 0, 0), // Major too high
            (0, 1000, 0), // Minor too high
            (0, 0, 1000), // Patch too high
            (-1, 0, 0),   // Negative major
            (0, -1, 0),   // Negative minor
            (0, 0, -1),   // Negative patch
        ];

        for (major, minor, patch) in invalid_versions {
            let major_valid = (0..=MAX_VERSION_COMPONENT).contains(&major);
            let minor_valid = (0..=MAX_VERSION_COMPONENT).contains(&minor);
            let patch_valid = (0..=MAX_VERSION_COMPONENT).contains(&patch);

            assert!(
                !(major_valid && minor_valid && patch_valid),
                "Version {major}.{minor}.{patch} should be invalid"
            );
        }
    }

    #[test]
    fn test_memory_validation_constants() {
        // Test memory validation constants are reasonable
        assert_eq!(
            MAX_GPU_MEMORY_BYTES,
            512 * 1024 * 1024 * 1024,
            "Max GPU memory should be 512GB"
        );

        // Current largest AMD GPU is MI325X with 288GB, ensure we support it
        let mi325x_memory: u64 = 288 * 1024 * 1024 * 1024;
        assert!(
            mi325x_memory < MAX_GPU_MEMORY_BYTES,
            "Should support MI325X 288GB memory"
        );

        // Future-proof for potential 400GB models
        let future_memory: u64 = 400 * 1024 * 1024 * 1024;
        assert!(
            future_memory < MAX_GPU_MEMORY_BYTES,
            "Should have headroom for future GPUs"
        );
    }

    #[test]
    fn test_gpu_metric_validation_constants() {
        // Test that validation constants are reasonable
        assert_eq!(MAX_GPU_UTILIZATION, 100.0, "Max utilization should be 100%");
        assert_eq!(
            MAX_GPU_POWER_WATTS, 1000.0,
            "Max power should support high-end GPUs"
        );
        assert_eq!(
            MAX_GPU_TEMP_CELSIUS, 125,
            "Max temp should be above thermal limits"
        );
        assert_eq!(
            MAX_GPU_FREQ_MHZ, 5000,
            "Max frequency should support boost clocks"
        );

        // Real-world values should be within limits
        let mi300x_power = 750.0; // MI300X max TDP
        assert!(
            mi300x_power < MAX_GPU_POWER_WATTS,
            "Should support MI300X power draw"
        );

        let typical_boost_freq = 2500; // Typical AMD GPU boost
        assert!(
            typical_boost_freq < MAX_GPU_FREQ_MHZ,
            "Should support typical boost frequencies"
        );
    }

    #[test]
    fn clamp_fan_rpm_bounds_a_garbled_sensor_reading() {
        // A corrupted or overflowed `libamdgpu_top` sample must never reach
        // `GpuInfo::fan_speed_rpm` (and, from there, the exporter and the
        // TUI) unclamped.
        assert_eq!(clamp_fan_rpm(Some(u32::MAX)), Some(MAX_GPU_FAN_RPM));
        // A real-world reading passes through unchanged.
        assert_eq!(clamp_fan_rpm(Some(1450)), Some(1450));
        // No tachometer stays `None`, not a clamped zero.
        assert_eq!(clamp_fan_rpm(None), None);
    }
}
