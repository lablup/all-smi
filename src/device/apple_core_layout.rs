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

//! Apple Silicon `hw.perflevelN` topology.
//!
//! The number of performance levels says nothing about what they contain:
//! M1-M4 expose Performance + Efficiency, M5 Pro/Max expose Super +
//! Performance, and base M5 exposes Super + Efficiency. Each level therefore
//! has to be mapped by its `hw.perflevelN.name`, not by its index.
//!
//! This module is pure parsing so it compiles on every test runner, including
//! the Linux CI hosts that never run on Apple Silicon.

use std::collections::BTreeMap;

use crate::device::CoreType;

const SUPER_SLOT: usize = 0;
const PERFORMANCE_SLOT: usize = 1;
const EFFICIENCY_SLOT: usize = 2;

fn slot(tier: CoreType) -> Option<usize> {
    match tier {
        CoreType::Super => Some(SUPER_SLOT),
        CoreType::Performance => Some(PERFORMANCE_SLOT),
        CoreType::Efficiency => Some(EFFICIENCY_SLOT),
        CoreType::Standard => None,
    }
}

/// Core counts and perflevel indexes for an Apple Silicon CPU.
///
/// A tier is present only when its count is non-zero. `level` and
/// `l2_cache_key` return `None` for an absent tier, which is what keeps the
/// L2 cache readers from attributing another cluster's cache to it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CoreLayout {
    levels: [Option<u32>; 3],
    counts: [u32; 3],
}

impl CoreLayout {
    /// Parse the stdout of
    /// `sysctl hw.perflevel0.name hw.perflevel0.physicalcpu hw.perflevel1.name hw.perflevel1.physicalcpu`.
    ///
    /// `sysctl` exits non-zero when one of the name keys is missing, so callers
    /// must ignore the exit status and pass stdout alone.
    pub fn from_sysctl_output(output: &str) -> Self {
        let mut names: BTreeMap<u32, String> = BTreeMap::new();
        let mut counts: BTreeMap<u32, u32> = BTreeMap::new();

        for line in output.lines() {
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            let Some(rest) = key.trim().strip_prefix("hw.perflevel") else {
                continue;
            };
            let Some((level, field)) = rest.split_once('.') else {
                continue;
            };
            let Ok(level) = level.parse::<u32>() else {
                continue;
            };
            let value = value.trim();
            match field {
                "name" if !value.is_empty() => {
                    names.insert(level, value.to_string());
                }
                "physicalcpu" => {
                    if let Ok(count) = value.parse::<u32>() {
                        counts.insert(level, count);
                    }
                }
                _ => {}
            }
        }

        let mut layout = Self::default();

        let mut assigned: Vec<(u32, CoreType)> = names
            .iter()
            .filter_map(|(level, name)| tier_from_name(name).map(|tier| (*level, tier)))
            .collect();

        if assigned.is_empty() {
            // Legacy M1-M3 on older macOS: no perflevel names at all.
            // perflevel0 is the P-cluster and perflevel1 the E-cluster.
            layout.set(PERFORMANCE_SLOT, 0, counts.get(&0).copied().unwrap_or(0));
            layout.set(EFFICIENCY_SLOT, 1, counts.get(&1).copied().unwrap_or(0));
            return layout;
        }

        assigned.sort_by_key(|(level, _)| *level);
        for (level, tier) in &assigned {
            let Some(index) = slot(*tier) else {
                continue;
            };
            layout.set(index, *level, counts.get(level).copied().unwrap_or(0));
        }

        // One unassigned level can be inferred from its named sibling: base M5
        // with a missing second name is Super + unnamed (Efficiency), and a
        // legacy chip with a Performance name and an unnamed sibling is
        // Performance + Efficiency. The sibling may carry an unrecognized
        // name, so look past every assigned level rather than only named ones.
        let inferred_index =
            if layout.levels[SUPER_SLOT].is_some() || layout.levels[PERFORMANCE_SLOT].is_some() {
                EFFICIENCY_SLOT
            } else {
                PERFORMANCE_SLOT
            };

        let assigned_levels: Vec<u32> = assigned.iter().map(|(level, _)| *level).collect();
        if layout.levels[inferred_index].is_none()
            && let Some(level) = counts
                .keys()
                .copied()
                .find(|level| !assigned_levels.contains(level))
        {
            layout.set(
                inferred_index,
                level,
                counts.get(&level).copied().unwrap_or(0),
            );
        }

        layout
    }

    fn set(&mut self, index: usize, level: u32, count: u32) {
        self.levels[index] = Some(level);
        self.counts[index] = count;
    }

    pub fn super_cores(&self) -> u32 {
        self.counts[SUPER_SLOT]
    }

    pub fn performance_cores(&self) -> u32 {
        self.counts[PERFORMANCE_SLOT]
    }

    pub fn efficiency_cores(&self) -> u32 {
        self.counts[EFFICIENCY_SLOT]
    }

    pub fn total_cores(&self) -> u32 {
        self.counts.iter().sum()
    }

    pub fn count(&self, tier: CoreType) -> u32 {
        slot(tier).map_or(0, |index| self.counts[index])
    }

    pub fn has(&self, tier: CoreType) -> bool {
        self.count(tier) > 0
    }

    /// Perflevel index that produced `tier`, only when the tier is present.
    pub fn level(&self, tier: CoreType) -> Option<u32> {
        let index = slot(tier)?;
        if self.counts[index] == 0 {
            return None;
        }
        self.levels[index]
    }

    /// sysctl key holding the L2 cache size for `tier`, only when present.
    pub fn l2_cache_key(&self, tier: CoreType) -> Option<String> {
        self.level(tier)
            .map(|level| format!("hw.perflevel{level}.l2cachesize"))
    }
}

fn tier_from_name(name: &str) -> Option<CoreType> {
    if name.eq_ignore_ascii_case("super") {
        Some(CoreType::Super)
    } else if name.eq_ignore_ascii_case("performance") {
        Some(CoreType::Performance)
    } else if name.eq_ignore_ascii_case("efficiency") {
        Some(CoreType::Efficiency)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(output: &str) -> CoreLayout {
        CoreLayout::from_sysctl_output(output)
    }

    #[test]
    fn m1_to_m4_performance_and_efficiency() {
        let layout = layout(
            "hw.perflevel0.name: Performance\n\
             hw.perflevel0.physicalcpu: 8\n\
             hw.perflevel1.name: Efficiency\n\
             hw.perflevel1.physicalcpu: 4\n",
        );

        assert_eq!(layout.super_cores(), 0);
        assert_eq!(layout.performance_cores(), 8);
        assert_eq!(layout.efficiency_cores(), 4);
        assert_eq!(layout.total_cores(), 12);
        assert_eq!(layout.level(CoreType::Performance), Some(0));
        assert_eq!(layout.level(CoreType::Efficiency), Some(1));
        assert_eq!(layout.level(CoreType::Super), None);
        assert_eq!(
            layout.l2_cache_key(CoreType::Performance).as_deref(),
            Some("hw.perflevel0.l2cachesize")
        );
        assert!(layout.has(CoreType::Performance));
        assert!(layout.has(CoreType::Efficiency));
        assert!(!layout.has(CoreType::Super));
        assert!(!layout.has(CoreType::Standard));
        assert_eq!(layout.count(CoreType::Performance), 8);
        assert_eq!(layout.count(CoreType::Standard), 0);
    }

    #[test]
    fn base_m5_super_and_efficiency() {
        let layout = layout(
            "hw.perflevel0.name: Super\n\
             hw.perflevel0.physicalcpu: 4\n\
             hw.perflevel1.name: Efficiency\n\
             hw.perflevel1.physicalcpu: 6\n",
        );

        assert_eq!(layout.super_cores(), 4);
        assert_eq!(layout.performance_cores(), 0);
        assert_eq!(layout.efficiency_cores(), 6);
        assert_eq!(layout.total_cores(), 10);
        assert_eq!(layout.level(CoreType::Super), Some(0));
        assert_eq!(layout.level(CoreType::Efficiency), Some(1));
        assert_eq!(layout.level(CoreType::Performance), None);
        assert_eq!(
            layout.l2_cache_key(CoreType::Efficiency).as_deref(),
            Some("hw.perflevel1.l2cachesize")
        );
        assert_eq!(layout.l2_cache_key(CoreType::Performance), None);
        assert!(layout.has(CoreType::Super));
        assert!(layout.has(CoreType::Efficiency));
        assert!(!layout.has(CoreType::Performance));
        assert_eq!(layout.count(CoreType::Super), 4);
        assert_eq!(layout.count(CoreType::Efficiency), 6);
        assert_eq!(layout.count(CoreType::Performance), 0);
    }

    #[test]
    fn m5_pro_max_super_and_performance() {
        let layout = layout(
            "hw.perflevel0.name: Super\n\
             hw.perflevel0.physicalcpu: 6\n\
             hw.perflevel1.name: Performance\n\
             hw.perflevel1.physicalcpu: 12\n",
        );

        assert_eq!(layout.super_cores(), 6);
        assert_eq!(layout.performance_cores(), 12);
        assert_eq!(layout.efficiency_cores(), 0);
        assert_eq!(layout.total_cores(), 18);
        assert_eq!(layout.level(CoreType::Super), Some(0));
        assert_eq!(layout.level(CoreType::Performance), Some(1));
        assert_eq!(layout.level(CoreType::Efficiency), None);
    }

    #[test]
    fn legacy_counts_without_names() {
        let layout = layout("hw.perflevel0.physicalcpu: 8\nhw.perflevel1.physicalcpu: 4\n");

        assert_eq!(layout.super_cores(), 0);
        assert_eq!(layout.performance_cores(), 8);
        assert_eq!(layout.efficiency_cores(), 4);
        assert_eq!(layout.level(CoreType::Performance), Some(0));
        assert_eq!(layout.level(CoreType::Efficiency), Some(1));
    }

    #[test]
    fn super_with_unnamed_sibling_is_efficiency() {
        // The name key is omitted, as `sysctl` prints nothing for it.
        let layout = layout(
            "hw.perflevel0.name: Super\nhw.perflevel0.physicalcpu: 4\nhw.perflevel1.physicalcpu: 6\n",
        );

        assert_eq!(layout.super_cores(), 4);
        assert_eq!(layout.performance_cores(), 0);
        assert_eq!(layout.efficiency_cores(), 6);
        assert_eq!(layout.level(CoreType::Efficiency), Some(1));
    }

    #[test]
    fn super_with_unrecognized_sibling_is_efficiency() {
        // A name `tier_from_name` does not know still occupies its level; it
        // must not block inference of the missing E tier.
        let layout = layout(
            "hw.perflevel0.name: Super\n\
             hw.perflevel0.physicalcpu: 4\n\
             hw.perflevel1.name: Something\n\
             hw.perflevel1.physicalcpu: 6\n",
        );

        assert_eq!(layout.super_cores(), 4);
        assert_eq!(layout.performance_cores(), 0);
        assert_eq!(layout.efficiency_cores(), 6);
        assert_eq!(layout.level(CoreType::Efficiency), Some(1));
        assert_eq!(
            layout.l2_cache_key(CoreType::Efficiency).as_deref(),
            Some("hw.perflevel1.l2cachesize")
        );
    }

    #[test]
    fn unrecognized_names_use_legacy_mapping() {
        let layout = layout(
            "hw.perflevel0.name: Whatever\n\
             hw.perflevel0.physicalcpu: 8\n\
             hw.perflevel1.name: Something\n\
             hw.perflevel1.physicalcpu: 4\n",
        );

        assert_eq!(layout.performance_cores(), 8);
        assert_eq!(layout.efficiency_cores(), 4);
        assert_eq!(layout.super_cores(), 0);
    }
}
