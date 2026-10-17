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

//! Windows on ARM CPU identification (issue #459).
//!
//! Windows publishes one registry key per logical processor,
//! `HKLM\HARDWARE\DESCRIPTION\System\CentralProcessor\<N>`, whose
//! `Identifier` value names the core microarchitecture on ARM64:
//!
//! ```text
//! ARMv8 (64-bit) Family 8 Model D85 Revision   1    (Cortex-X925)
//! ARMv8 (64-bit) Family 8 Model D87 Revision   1    (Cortex-A725)
//! ```
//!
//! The `Model` field is the ARM `MIDR_EL1.PartNum`. On a heterogeneous
//! part such as the NVIDIA RTX Spark N1X (10x Cortex-X925 + 10x
//! Cortex-A725) it is the only per-core signal that tells the performance
//! cluster from the efficiency cluster.
//!
//! Everything here is pure string handling so it is tested on every
//! platform; the registry read lives in `cpu_windows`.

use crate::device::{CoreType, CpuPlatformType};

/// Relative tier of a known ARM core design. Higher is faster.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum CoreTier {
    /// Little cores (Cortex-A5xx).
    Little,
    /// Mid cores (Cortex-A7xx).
    Mid,
    /// Big cores (Cortex-X).
    Big,
}

/// ARM part numbers (`MIDR_EL1.PartNum`) of big.LITTLE / DynamIQ cores and
/// their tier. Extend this table for new designs; an unknown part number
/// stays [`CoreType::Standard`].
const ARM_PART_TIERS: &[(u32, CoreTier)] = &[
    (0xD05, CoreTier::Little), // Cortex-A55
    (0xD46, CoreTier::Little), // Cortex-A510
    (0xD80, CoreTier::Little), // Cortex-A520
    (0xD0D, CoreTier::Mid),    // Cortex-A77
    (0xD41, CoreTier::Mid),    // Cortex-A78
    (0xD4B, CoreTier::Mid),    // Cortex-A78C
    (0xD47, CoreTier::Mid),    // Cortex-A710
    (0xD4D, CoreTier::Mid),    // Cortex-A715
    (0xD81, CoreTier::Mid),    // Cortex-A720
    (0xD87, CoreTier::Mid),    // Cortex-A725
    (0xD44, CoreTier::Big),    // Cortex-X1
    (0xD4C, CoreTier::Big),    // Cortex-X1C
    (0xD48, CoreTier::Big),    // Cortex-X2
    (0xD4E, CoreTier::Big),    // Cortex-X3
    (0xD82, CoreTier::Big),    // Cortex-X4
    (0xD85, CoreTier::Big),    // Cortex-X925
];

/// Parse the `Model` part number out of a `CentralProcessor\N\Identifier`
/// string. Returns `None` for x86 identifiers and malformed input.
pub fn parse_arm_part_number(identifier: &str) -> Option<u32> {
    if !identifier
        .trim_start()
        .to_ascii_uppercase()
        .starts_with("ARM")
    {
        return None;
    }
    let mut tokens = identifier.split_whitespace();
    tokens.find(|token| token.eq_ignore_ascii_case("Model"))?;
    let model = tokens.next()?;
    u32::from_str_radix(model, 16).ok()
}

fn tier_of(part: u32) -> Option<CoreTier> {
    ARM_PART_TIERS
        .iter()
        .find(|(known, _)| *known == part)
        .map(|(_, tier)| *tier)
}

/// Map each logical processor's `Identifier` to a [`CoreType`].
///
/// Cores of the highest tier present become `Performance` and cores of any
/// lower known tier become `Efficiency`, which is how a two-cluster part
/// (N1X: X925 + A725) or a three-cluster one (X + A7xx + A5xx) splits into
/// P and E. A homogeneous part, an x86 CPU, or an unknown part number maps
/// to `Standard`; an entry that is `None` (the registry key could not be
/// read) is `Standard` too.
pub fn core_types_from_identifiers(identifiers: &[Option<String>]) -> Vec<CoreType> {
    let tiers: Vec<Option<CoreTier>> = identifiers
        .iter()
        .map(|id| {
            id.as_deref()
                .and_then(parse_arm_part_number)
                .and_then(tier_of)
        })
        .collect();
    let top = tiers.iter().flatten().max().copied();
    let heterogeneous = tiers.iter().flatten().any(|tier| Some(*tier) != top);

    tiers
        .iter()
        .map(|tier| match (heterogeneous, tier) {
            (true, Some(t)) if Some(*t) == top => CoreType::Performance,
            (true, Some(_)) => CoreType::Efficiency,
            _ => CoreType::Standard,
        })
        .collect()
}

/// Classify the CPU platform on Windows.
///
/// The brand string alone is not enough: `NVIDIA RTX Spark N1X (6144-core
/// GPU, 20-core CPU)` contains neither "arm" nor a vendor all-smi knows. An
/// ARM `Identifier`, or a native aarch64 build, settles it. The identifier
/// matters for an x64 build running under emulation on ARM, where
/// `target_arch` (and `PROCESSOR_ARCHITECTURE`) say x86_64.
pub fn classify_platform(
    cpu_model: &str,
    first_identifier: Option<&str>,
    target_is_aarch64: bool,
) -> CpuPlatformType {
    let model = cpu_model.to_lowercase();
    let arm_identifier =
        first_identifier.is_some_and(|id| id.trim_start().to_ascii_uppercase().starts_with("ARM"));
    if target_is_aarch64 || arm_identifier || model.contains("arm") {
        CpuPlatformType::Arm
    } else if model.contains("intel") {
        CpuPlatformType::Intel
    } else if model.contains("amd") {
        CpuPlatformType::Amd
    } else {
        CpuPlatformType::Other("Unknown".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const X925: &str = "ARMv8 (64-bit) Family 8 Model D85 Revision   1";
    const A725: &str = "ARMv8 (64-bit) Family 8 Model D87 Revision   1";
    const N1X_MODEL: &str = "NVIDIA RTX Spark N1X (6144-core GPU, 20-core CPU)";

    #[test]
    fn parses_the_n1x_identifiers() {
        assert_eq!(parse_arm_part_number(X925), Some(0xD85));
        assert_eq!(parse_arm_part_number(A725), Some(0xD87));
    }

    #[test]
    fn rejects_x86_and_malformed_identifiers() {
        assert_eq!(
            parse_arm_part_number("Intel64 Family 6 Model 183 Stepping 1"),
            None
        );
        assert_eq!(parse_arm_part_number("ARMv8 (64-bit) Family 8"), None);
        assert_eq!(parse_arm_part_number("ARMv8 Model ZZZ"), None);
        assert_eq!(parse_arm_part_number(""), None);
    }

    /// The N1X layout as the registry reports it: cores 0-4 and 10-14 are
    /// A725, cores 5-9 and 15-19 are X925.
    #[test]
    fn n1x_clusters_map_to_performance_and_efficiency() {
        let identifiers: Vec<Option<String>> = (0..20)
            .map(|core| {
                let x925 = (5..10).contains(&core) || (15..20).contains(&core);
                Some(if x925 { X925 } else { A725 }.to_string())
            })
            .collect();
        let types = core_types_from_identifiers(&identifiers);
        assert_eq!(types.len(), 20);
        for (core, core_type) in types.iter().enumerate() {
            let expected = if (5..10).contains(&core) || (15..20).contains(&core) {
                CoreType::Performance
            } else {
                CoreType::Efficiency
            };
            assert_eq!(*core_type, expected, "core {core}");
        }
    }

    #[test]
    fn three_cluster_parts_split_top_tier_from_the_rest() {
        let ids = [
            "ARMv8 (64-bit) Family 8 Model D82 Revision 0",
            "ARMv8 (64-bit) Family 8 Model D81 Revision 0",
            "ARMv8 (64-bit) Family 8 Model D80 Revision 0",
        ]
        .map(|s| Some(s.to_string()));
        assert_eq!(
            core_types_from_identifiers(&ids),
            vec![
                CoreType::Performance,
                CoreType::Efficiency,
                CoreType::Efficiency
            ]
        );
    }

    #[test]
    fn homogeneous_unknown_and_x86_parts_stay_standard() {
        let homogeneous = vec![Some(A725.to_string()); 4];
        assert!(
            core_types_from_identifiers(&homogeneous)
                .iter()
                .all(|t| *t == CoreType::Standard)
        );
        // Qualcomm Oryon reports a part number outside the table.
        let unknown = vec![Some("ARMv8 (64-bit) Family 8 Model 1 Revision 201".to_string()); 4];
        assert!(
            core_types_from_identifiers(&unknown)
                .iter()
                .all(|t| *t == CoreType::Standard)
        );
        let x86 = vec![Some("Intel64 Family 6 Model 183 Stepping 1".to_string()); 4];
        assert!(
            core_types_from_identifiers(&x86)
                .iter()
                .all(|t| *t == CoreType::Standard)
        );
    }

    #[test]
    fn unreadable_and_unknown_cores_in_a_mixed_part_stay_standard() {
        let ids = vec![
            Some(X925.to_string()),
            Some(A725.to_string()),
            None,
            Some("ARMv8 (64-bit) Family 8 Model FFF Revision 0".to_string()),
        ];
        assert_eq!(
            core_types_from_identifiers(&ids),
            vec![
                CoreType::Performance,
                CoreType::Efficiency,
                CoreType::Standard,
                CoreType::Standard
            ]
        );
    }

    #[test]
    fn n1x_is_classified_as_arm() {
        // Native aarch64 build.
        assert_eq!(
            classify_platform(N1X_MODEL, None, true),
            CpuPlatformType::Arm
        );
        // x64 build under emulation: the identifier still says ARM.
        assert_eq!(
            classify_platform(N1X_MODEL, Some(X925), false),
            CpuPlatformType::Arm
        );
    }

    #[test]
    fn x86_vendors_are_unchanged() {
        assert_eq!(
            classify_platform("Intel(R) Core(TM) i9-14900K", None, false),
            CpuPlatformType::Intel
        );
        assert_eq!(
            classify_platform("AMD Ryzen 9 7950X 16-Core Processor", None, false),
            CpuPlatformType::Amd
        );
        assert_eq!(
            classify_platform("Some CPU", None, false),
            CpuPlatformType::Other("Unknown".to_string())
        );
    }
}
