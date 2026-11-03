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

//! Platform-neutral CPU tier topology (issue #473).
//!
//! Every heterogeneous CPU is described by one ordered, fastest-first list of
//! [`CpuTier`](crate::device::CpuTier) entries, regardless of how the platform
//! discovered it: Apple Silicon perflevels, Intel hybrid PMUs, arm64 MIDR
//! part numbers, or a firmware capacity table.
//!
//! Everything in this module is pure string and integer handling with no OS
//! dependency, so it is compiled on every target and exercised by the Linux
//! test runner. The platform readers (`cpu_linux`, `cpu_windows_arm`,
//! `cpu_macos`) own the OS calls and call into these helpers.

#[cfg(any(target_os = "linux", test))]
use std::collections::BTreeMap;

use crate::device::CoreType;
#[cfg(any(target_os = "linux", target_os = "windows", test))]
use crate::device::CoreUtilization;

/// Relative tier of a known ARM core design. Higher is faster.
///
/// Used by the Windows registry reader and by the Linux MIDR detector; the
/// other targets compile neither, so the item is gated with them.
#[cfg(any(target_os = "linux", target_os = "windows", test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CoreTier {
    /// Little cores (Cortex-A5xx).
    Little,
    /// Mid cores (Cortex-A7xx).
    Mid,
    /// Big cores (Cortex-X).
    Big,
}

#[cfg(any(target_os = "linux", target_os = "windows", test))]
impl CoreTier {
    /// Canonical display label for this tier: `P`, `M` or `E`.
    pub fn label(self) -> &'static str {
        match self {
            CoreTier::Big => "P",
            CoreTier::Mid => "M",
            CoreTier::Little => "E",
        }
    }
}

/// ARM part numbers (`MIDR_EL1.PartNum`) of big.LITTLE / DynamIQ cores and
/// their tier. Extend this table for new designs; an unknown part number
/// returns `None` and leaves the core untiered.
#[cfg(any(target_os = "linux", target_os = "windows", test))]
pub const ARM_PART_TIERS: &[(u32, CoreTier)] = &[
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

/// Upper bound on the number of CPU ids a single cpumask string may expand
/// to. The kernel's `%*pbl` output describes real processors, so anything
/// near this is already far beyond a plausible machine; the bound keeps a
/// malformed range such as `0-4294967295` from allocating unbounded memory.
#[cfg(any(target_os = "linux", test))]
const MAX_CPUMASK_ENTRIES: usize = 8192;

/// Look up the tier of an ARM part number. Unknown parts return `None`.
#[cfg(any(target_os = "linux", target_os = "windows", test))]
pub fn tier_of(part: u32) -> Option<CoreTier> {
    ARM_PART_TIERS
        .iter()
        .find(|(known, _)| *known == part)
        .map(|(_, tier)| *tier)
}

/// Parse a kernel `%*pbl` cpumask string such as `0-3,8-11` into a sorted,
/// deduplicated list of CPU ids.
///
/// Surrounding whitespace is tolerated, empty entries are ignored, and a
/// malformed or oversized entry is skipped rather than rejected wholesale, so
/// one bad token does not discard the rest of a valid mask.
#[cfg(any(target_os = "linux", test))]
pub fn parse_cpumask_list(mask: &str) -> Vec<u32> {
    let mut ids = Vec::new();

    for entry in mask.trim().split(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }

        match entry.split_once('-') {
            Some((start, end)) => {
                let (Ok(start), Ok(end)) = (start.trim().parse::<u32>(), end.trim().parse::<u32>())
                else {
                    continue;
                };
                if end < start {
                    continue;
                }
                // Compare in `u64` so a 32-bit target cannot overflow on a
                // full `u32` range before the bound is checked.
                let span = u64::from(end) - u64::from(start) + 1;
                if span > MAX_CPUMASK_ENTRIES.saturating_sub(ids.len()) as u64 {
                    continue;
                }
                ids.extend(start..=end);
            }
            None => {
                if let Ok(id) = entry.parse::<u32>()
                    && ids.len() < MAX_CPUMASK_ENTRIES
                {
                    ids.push(id);
                }
            }
        }
    }

    ids.sort_unstable();
    ids.dedup();
    ids
}

/// Decode the ARM `MIDR_EL1.PartNum` field from the sysfs string form
/// (`0x%016llx`).
///
/// Returns `None` for malformed input, for a zero register value (the arm64
/// kernel writes an empty or zero file when `reg_midr` is unknown), and for a
/// zero part number.
#[cfg(any(target_os = "linux", test))]
pub fn decode_midr(text: &str) -> Option<u32> {
    let trimmed = text.trim();
    let hex = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))?;
    if hex.is_empty() || hex.len() > 16 {
        return None;
    }
    let raw = u64::from_str_radix(hex, 16).ok()?;
    if raw == 0 {
        return None;
    }
    let part = ((raw >> 4) & 0xfff) as u32;
    (part != 0).then_some(part)
}

/// Group `(cpu_id, capacity)` pairs by distinct capacity.
///
/// Returns `(capacity, cpu_ids)` groups ordered by capacity descending, so
/// index 0 is the fastest cluster. CPU ids within a group are sorted
/// ascending and deduplicated.
#[cfg(any(target_os = "linux", test))]
pub fn group_cpus_by_capacity(capacities: &[(u32, u32)]) -> Vec<(u32, Vec<u32>)> {
    let mut by_capacity: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    for &(cpu_id, capacity) in capacities {
        by_capacity.entry(capacity).or_default().push(cpu_id);
    }

    let mut groups: Vec<(u32, Vec<u32>)> = by_capacity.into_iter().collect();
    groups.sort_by_key(|group| std::cmp::Reverse(group.0));
    for (_, cpu_ids) in &mut groups {
        cpu_ids.sort_unstable();
        cpu_ids.dedup();
    }
    groups
}

/// Canonical sort rank for a tier label.
///
/// `S` = 0, `P` = 1, `M` = 2, `E` = 3, `E4`.. = their number, and any
/// unknown label = 255 so it sorts last without being discarded.
pub fn tier_rank(label: &str) -> u8 {
    match label {
        "S" => 0,
        "P" => 1,
        "M" => 2,
        "E" => 3,
        other => other
            .strip_prefix('E')
            .and_then(|rest| rest.parse::<u8>().ok())
            .unwrap_or(u8::MAX),
    }
}

/// Label for the tier at `position` (0-based) in a capacity-ordered list.
///
/// `0` -> `P`, `1` -> `M`, `2` -> `E`, and `r >= 3` -> `E{r+1}`. The extra
/// tiers are numbered after the fixed `E` rank so labels stay unique and the
/// canonical rank order (`S`, `P`, `M`, `E`, `E4`, ...) stays stable.
#[cfg(any(target_os = "linux", test))]
pub fn tier_label_for_rank(position: usize) -> String {
    match position {
        0 => "P".to_string(),
        1 => "M".to_string(),
        2 => "E".to_string(),
        other => format!("E{}", other + 1),
    }
}

/// Average utilization of the cores assigned to tier `tier_index`.
///
/// A core participates when its [`CoreUtilization::tier_index`] equals the
/// tier position. Returns `0.0` when no core carries the index.
#[cfg(any(target_os = "linux", target_os = "windows", test))]
pub fn tier_utilization(per_core: &[CoreUtilization], tier_index: usize) -> f64 {
    let Some(position) = u8::try_from(tier_index).ok() else {
        return 0.0;
    };

    let mut sum = 0.0;
    let mut count = 0u32;
    for core in per_core {
        if core.tier_index == Some(position) {
            sum += core.utilization;
            count += 1;
        }
    }

    if count == 0 {
        0.0
    } else {
        sum / f64::from(count)
    }
}

/// Coarse legacy class for a tier at `position` in a list of `total` tiers.
///
/// The fastest tier is `Performance` and every other tier is `Efficiency`.
/// A single tier (which the platform detectors never emit, but an older or
/// hostile remote server may send) is `Standard`, matching the pre-tier
/// behaviour. The exact tier lives in [`CpuTier::label`](crate::device::CpuTier)
/// and [`CoreUtilization::tier_index`].
pub fn coarse_core_type(position: usize, total: usize) -> CoreType {
    if total <= 1 {
        CoreType::Standard
    } else if position == 0 {
        CoreType::Performance
    } else {
        CoreType::Efficiency
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn core(id: u32, tier: Option<u8>, utilization: f64) -> CoreUtilization {
        CoreUtilization {
            core_id: id,
            core_type: CoreType::Standard,
            utilization,
            tier_index: tier,
        }
    }

    #[test]
    fn parses_single_id() {
        assert_eq!(parse_cpumask_list("7"), vec![7]);
    }

    #[test]
    fn parses_a_range() {
        assert_eq!(parse_cpumask_list("0-3"), vec![0, 1, 2, 3]);
    }

    #[test]
    fn parses_a_mixed_list_and_deduplicates() {
        assert_eq!(
            parse_cpumask_list("8-11,0-3,5,5"),
            vec![0, 1, 2, 3, 5, 8, 9, 10, 11]
        );
    }

    #[test]
    fn cpumask_tolerates_whitespace_and_empty_entries() {
        assert_eq!(parse_cpumask_list("  0-1 , ,4  "), vec![0, 1, 4]);
        assert!(parse_cpumask_list("").is_empty());
        assert!(parse_cpumask_list("  ").is_empty());
        assert!(parse_cpumask_list(",,").is_empty());
    }

    #[test]
    fn cpumask_skips_malformed_entries_but_keeps_valid_ones() {
        assert_eq!(parse_cpumask_list("abc,3-1,2-4,zz"), vec![2, 3, 4]);
        assert!(parse_cpumask_list("3-1").is_empty());
        assert!(parse_cpumask_list("not-a-mask").is_empty());
    }

    #[test]
    fn cpumask_rejects_an_oversized_range() {
        // The kernel never emits this; a hostile or corrupted file must not
        // expand into billions of entries.
        assert!(parse_cpumask_list("0-4294967295").is_empty());
    }

    #[test]
    fn decodes_a_midr_with_the_part_number() {
        // Cortex-X925 on an NVIDIA N1X.
        assert_eq!(decode_midr("0x00000000410fd853"), Some(0xD85));
    }

    #[test]
    fn decode_midr_rejects_bad_input() {
        assert_eq!(decode_midr(""), None);
        assert_eq!(decode_midr("0x"), None);
        assert_eq!(decode_midr("0x0000000000000000"), None);
        assert_eq!(decode_midr("0x000000000000000f"), None); // part 0
        assert_eq!(decode_midr("0xZZ"), None);
        assert_eq!(decode_midr("410fd853"), None); // missing prefix
        assert_eq!(decode_midr("0x00000000000000001"), None); // too long
    }

    #[test]
    fn groups_capacities_descending() {
        let pairs = [(0, 1024), (1, 512), (2, 1024), (3, 256)];
        let groups = group_cpus_by_capacity(&pairs);
        assert_eq!(
            groups,
            vec![(1024, vec![0, 2]), (512, vec![1]), (256, vec![3])]
        );
    }

    #[test]
    fn groups_equal_capacities_into_one() {
        let groups = group_cpus_by_capacity(&[(0, 1024), (1, 1024)]);
        assert_eq!(groups, vec![(1024, vec![0, 1])]);
        assert!(group_cpus_by_capacity(&[]).is_empty());
    }

    #[test]
    fn canonical_tier_ranks() {
        assert_eq!(tier_rank("S"), 0);
        assert_eq!(tier_rank("P"), 1);
        assert_eq!(tier_rank("M"), 2);
        assert_eq!(tier_rank("E"), 3);
        assert_eq!(tier_rank("E4"), 4);
        assert_eq!(tier_rank("E12"), 12);
        assert_eq!(tier_rank("unknown"), u8::MAX);
        assert_eq!(tier_rank("E"), 3);
        assert_eq!(tier_rank("Eabc"), u8::MAX);
    }

    #[test]
    fn capacity_labels_follow_the_fixed_vocabulary() {
        assert_eq!(tier_label_for_rank(0), "P");
        assert_eq!(tier_label_for_rank(1), "M");
        assert_eq!(tier_label_for_rank(2), "E");
        assert_eq!(tier_label_for_rank(3), "E4");
        assert_eq!(tier_label_for_rank(4), "E5");
    }

    #[test]
    fn tier_utilization_averages_only_the_matching_cores() {
        let cores = [
            core(0, Some(0), 10.0),
            core(1, Some(0), 30.0),
            core(2, Some(1), 90.0),
            core(3, None, 70.0),
        ];
        assert_eq!(tier_utilization(&cores, 0), 20.0);
        assert_eq!(tier_utilization(&cores, 1), 90.0);
        assert_eq!(tier_utilization(&cores, 2), 0.0);
        assert_eq!(tier_utilization(&[], 0), 0.0);
    }

    #[test]
    fn arm_part_table_lookup() {
        assert_eq!(tier_of(0xD85), Some(CoreTier::Big));
        assert_eq!(tier_of(0xD87), Some(CoreTier::Mid));
        assert_eq!(tier_of(0xD80), Some(CoreTier::Little));
        assert_eq!(tier_of(0x123), None);
        assert_eq!(CoreTier::Big.label(), "P");
        assert_eq!(CoreTier::Mid.label(), "M");
        assert_eq!(CoreTier::Little.label(), "E");
    }

    #[test]
    fn coarse_types_keep_the_legacy_mapping() {
        assert_eq!(coarse_core_type(0, 0), CoreType::Standard);
        assert_eq!(coarse_core_type(0, 1), CoreType::Standard);
        assert_eq!(coarse_core_type(0, 2), CoreType::Performance);
        assert_eq!(coarse_core_type(1, 2), CoreType::Efficiency);
        assert_eq!(coarse_core_type(2, 3), CoreType::Efficiency);
    }
}
