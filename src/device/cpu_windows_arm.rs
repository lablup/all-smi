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

use crate::device::cpu_topology::{self, CoreTier, tier_of};
use crate::device::{CoreType, CpuPlatformType, CpuTier};

/// Static, ordered tier topology for a Windows on ARM part.
///
/// `tiers` is empty for a homogeneous part, an x86 CPU, or an unknown part
/// number, and carries `P`/`M`/`E` fastest first otherwise. `per_core` keeps
/// the raw per-logical-processor tier so a reader can refresh counters
/// without re-parsing the registry strings.
#[derive(Debug, Clone, Default)]
pub struct WindowsTierTopology {
    /// Raw tier of each logical processor. `Some` even when the part is
    /// homogeneous; `tiers` is what decides whether the CPU is heterogeneous.
    pub per_core: Vec<Option<CoreTier>>,
    /// Ordered, fastest-first tiers. Empty unless at least two tiers have
    /// cores.
    pub tiers: Vec<CpuTier>,
}

impl WindowsTierTopology {
    /// Tier position for each logical processor: the index into [`Self::tiers`]
    /// for the core's raw tier, or `None` for an untiered core.
    pub fn tier_index_by_core(&self) -> Vec<Option<u8>> {
        self.per_core
            .iter()
            .map(|tier| {
                tier.and_then(|tier| {
                    self.tiers
                        .iter()
                        .position(|entry| entry.label == tier.label())
                        .map(|position| position as u8)
                })
            })
            .collect()
    }
}

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

/// Build the ordered tier topology from the per-processor registry
/// identifiers (issue #473).
///
/// `Big` maps to `P`, `Mid` to `M` and `Little` to `E`. Only when at least two
/// distinct tiers have cores does the list become non-empty; a homogeneous
/// part stays `Standard`.
pub fn tier_topology_from_identifiers(identifiers: &[Option<String>]) -> WindowsTierTopology {
    let per_core: Vec<Option<CoreTier>> = identifiers
        .iter()
        .map(|id| {
            id.as_deref()
                .and_then(parse_arm_part_number)
                .and_then(tier_of)
        })
        .collect();

    let present: Vec<CoreTier> = [CoreTier::Big, CoreTier::Mid, CoreTier::Little]
        .into_iter()
        .filter(|tier| per_core.contains(&Some(*tier)))
        .collect();

    if present.len() < 2 {
        return WindowsTierTopology {
            per_core,
            tiers: Vec::new(),
        };
    }

    let total = present.len();
    let tiers: Vec<CpuTier> = present
        .iter()
        .enumerate()
        .map(|(position, tier)| CpuTier {
            label: tier.label().to_string(),
            core_type: cpu_topology::coarse_core_type(position, total),
            core_count: per_core.iter().filter(|core| **core == Some(*tier)).count() as u32,
            utilization: 0.0,
            frequency_mhz: None,
            l2_cache_mb: None,
        })
        .collect();

    WindowsTierTopology { per_core, tiers }
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

    /// The N1X is a two-cluster part (Big + Mid), so the coarse P/E result is
    /// unchanged while the exact tier list separates `P` from `M`.
    #[test]
    fn n1x_two_tier_topology_is_unchanged() {
        let identifiers: Vec<Option<String>> = (0..20)
            .map(|core| {
                let x925 = (5..10).contains(&core) || (15..20).contains(&core);
                Some(if x925 { X925 } else { A725 }.to_string())
            })
            .collect();
        let topology = tier_topology_from_identifiers(&identifiers);
        assert_eq!(topology.tiers.len(), 2);
        assert_eq!(topology.tiers[0].label, "P");
        assert_eq!(topology.tiers[0].core_count, 10);
        assert_eq!(topology.tiers[1].label, "M");
        assert_eq!(topology.tiers[1].core_count, 10);
        assert_eq!(
            core_types_from_identifiers(&identifiers),
            topology
                .per_core
                .iter()
                .map(|tier| match tier {
                    Some(CoreTier::Big) => CoreType::Performance,
                    Some(_) => CoreType::Efficiency,
                    None => CoreType::Standard,
                })
                .collect::<Vec<_>>()
        );
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

        // The exact tier list keeps all three tiers (issue #473).
        let topology = tier_topology_from_identifiers(&ids);
        assert_eq!(
            topology
                .tiers
                .iter()
                .map(|tier| (tier.label.as_str(), tier.core_count))
                .collect::<Vec<_>>(),
            vec![("P", 1), ("M", 1), ("E", 1)]
        );
        assert_eq!(
            topology.tier_index_by_core(),
            vec![Some(0), Some(1), Some(2)]
        );
        assert_eq!(
            topology.per_core,
            vec![
                Some(CoreTier::Big),
                Some(CoreTier::Mid),
                Some(CoreTier::Little)
            ]
        );
    }

    #[test]
    fn homogeneous_parts_have_no_tier_list() {
        for identifiers in [
            vec![Some(A725.to_string()); 4],
            vec![Some("ARMv8 (64-bit) Family 8 Model 1 Revision 201".to_string()); 4],
            vec![Some("Intel64 Family 6 Model 183 Stepping 1".to_string()); 4],
            vec![None; 4],
        ] {
            let topology = tier_topology_from_identifiers(&identifiers);
            assert!(topology.tiers.is_empty());
            assert!(topology.tier_index_by_core().iter().all(Option::is_none));
        }
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
