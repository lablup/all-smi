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

//! Adapter grouping and GPU matching for the ADL reader.
//!
//! Everything here is platform-independent on purpose, the same split
//! `windows_gpu_perf::ids` uses: the FFI call that fills in
//! [`ffi::AdapterInfo`] lives in [`super::loader`] behind
//! `cfg(target_os = "windows")`, but the logic that turns those rows
//! into an attribution decision compiles and is tested on the Linux
//! runner, the only runner this repository has.
//!
//! ## Grouping
//!
//! One physical card exposes several ADL adapter indices, one per
//! display output, all reporting identical telemetry. The rows are
//! therefore grouped by PCI bus/device/function into one
//! [`CardGroup`] per card before any matching happens.
//!
//! ## Matching, strongest first
//!
//! 1. **Exact PNP instance path.** ADL's `strPNPString` is the same
//!    Windows `PNPDeviceID` that `amd_windows` stores as the GPU uuid,
//!    so the join is exact and distinguishes two identical cards.
//! 2. **PCI vendor and device**, parsed from both sides with
//!    `windows_gpu_perf::ids::parse_pnp_device_id`. Used only when the
//!    pair is unambiguous on *both* sides: exactly one unmatched GPU
//!    and exactly one unclaimed card carry that identity. Requiring
//!    the full vendor+device pair honours the `match_adapter` rule
//!    from the #346 review that anything weaker than PCI identity must
//!    be vendor-constrained, and giving up on ambiguity honours its
//!    conclusion that declining to attribute beats attributing
//!    wrongly.
//!
//! There is deliberately no ordinal or name fallback here: unlike the
//! DXGI matching this feeds temperature and power, where a swap
//! between two cards is silent and misleading.

use std::collections::HashSet;

use super::ffi;
use crate::device::readers::windows_gpu_perf::ids::{PciIds, parse_pnp_device_id};

/// One ADL adapter row in owned, parsed form.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdlAdapter {
    /// The ADL adapter index, the handle sensor queries take.
    pub index: i32,
    pub bus: i32,
    pub device: i32,
    pub function: i32,
    pub adapter_name: String,
    /// Windows PNP device instance path; empty when the driver left it
    /// blank.
    pub pnp_string: String,
}

/// One physical card: every ADL index that resolves to the same PCI
/// bus/device/function.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CardGroup {
    pub bus: i32,
    pub device: i32,
    pub function: i32,
    pub adapter_name: String,
    pub pnp_string: String,
    /// Sorted, deduplicated ADL indices. All report the same
    /// telemetry, so a caller tries them in order until one answers.
    pub indices: Vec<i32>,
}

/// A GPU that matched a card exactly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CardMatch {
    /// Position in the caller's GPU slice.
    pub gpu_index: usize,
    /// The matched card's ADL indices, in the order to try them.
    pub adl_indices: Vec<i32>,
}

/// The attribution decision for one poll.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AttributionPlan {
    /// Exactly one AMD GPU: attribute the process-wide sample to it
    /// without consulting `AdapterInfo` at all. This arm is what keeps
    /// the single-GPU behavior identical to before #353, including
    /// when `AdapterInfo` is unavailable or fails verification.
    SoleGpu,
    /// Two or more AMD GPUs with at least one exact card match. GPUs
    /// absent from the list keep their DXGI/PDH baseline.
    PerCard(Vec<CardMatch>),
    /// Nothing can be attributed: no GPUs, no validated adapter rows,
    /// or no unambiguous match. The honest baseline stands.
    Decline,
}

/// Parse validated `AdapterInfo` rows into owned adapters.
///
/// Rows are expected to have passed
/// [`ffi::AdapterInfoArray::validated`] already; a string that still
/// fails to parse becomes empty rather than aborting, since an empty
/// string simply cannot match anything downstream.
pub fn parse_adapters(entries: &[ffi::AdapterInfo]) -> Vec<AdlAdapter> {
    entries
        .iter()
        .map(|entry| AdlAdapter {
            index: entry.i_adapter_index,
            bus: entry.i_bus_number,
            device: entry.i_device_number,
            function: entry.i_function_number,
            adapter_name: ffi::adl_string(&entry.str_adapter_name)
                .unwrap_or_default()
                .to_string(),
            pnp_string: ffi::adl_string(&entry.str_pnp_string)
                .unwrap_or_default()
                .to_string(),
        })
        .collect()
}

/// Whether a row's bus/device/function is a plausible PCI address.
///
/// This is the data-quality counterpart of the layout check in
/// `AdapterInfo::looks_sane`: a row with an implausible address is
/// excluded from grouping (it cannot be attributed to), but it does
/// not disable attribution for the machine's other, plausible rows.
fn plausible_bdf(adapter: &AdlAdapter) -> bool {
    (0..=255).contains(&adapter.bus)
        && (0..=31).contains(&adapter.device)
        && (0..=7).contains(&adapter.function)
}

/// Group adapter rows into one [`CardGroup`] per physical card.
///
/// Groups are sorted by bus/device/function and their indices sorted
/// ascending, so the output is deterministic regardless of the order
/// the driver enumerated in.
pub fn group_by_card(adapters: &[AdlAdapter]) -> Vec<CardGroup> {
    let mut groups: Vec<CardGroup> = Vec::new();
    for adapter in adapters {
        if !plausible_bdf(adapter) {
            continue;
        }
        match groups.iter_mut().find(|group| {
            group.bus == adapter.bus
                && group.device == adapter.device
                && group.function == adapter.function
        }) {
            Some(group) => {
                group.indices.push(adapter.index);
                // Prefer any row that actually carries the string; a
                // secondary display-output row can leave one blank.
                if group.pnp_string.is_empty() && !adapter.pnp_string.is_empty() {
                    group.pnp_string = adapter.pnp_string.clone();
                }
                if group.adapter_name.is_empty() && !adapter.adapter_name.is_empty() {
                    group.adapter_name = adapter.adapter_name.clone();
                }
            }
            None => groups.push(CardGroup {
                bus: adapter.bus,
                device: adapter.device,
                function: adapter.function,
                adapter_name: adapter.adapter_name.clone(),
                pnp_string: adapter.pnp_string.clone(),
                indices: vec![adapter.index],
            }),
        }
    }
    for group in &mut groups {
        group.indices.sort_unstable();
        group.indices.dedup();
    }
    groups.sort_by_key(|group| (group.bus, group.device, group.function));
    groups
}

/// Decide what, if anything, ADL readouts may be attributed to.
///
/// `gpu_uuids` are the reader's GPU uuids in slice order, which
/// `amd_windows` populates from the WMI `PNPDeviceID` (or a synthetic
/// `AMD-GPU-{n}` that matches nothing, which is the correct outcome
/// for it). `adapters` is the validated adapter inventory, or `None`
/// when it is unavailable or failed layout verification.
///
/// Every returned `gpu_index` is a valid index into `gpu_uuids`, each
/// GPU appears at most once, and no two GPUs share a card. Any state
/// that would violate that (duplicate identities on either side)
/// declines outright.
pub fn plan_attribution(gpu_uuids: &[&str], adapters: Option<&[AdlAdapter]>) -> AttributionPlan {
    match gpu_uuids.len() {
        0 => return AttributionPlan::Decline,
        1 => return AttributionPlan::SoleGpu,
        _ => {}
    }
    let Some(adapters) = adapters else {
        return AttributionPlan::Decline;
    };
    let groups = group_by_card(adapters);
    if groups.is_empty() {
        return AttributionPlan::Decline;
    }

    // gpu position -> group position.
    let mut assigned: Vec<Option<usize>> = vec![None; gpu_uuids.len()];

    // Phase 1: exact PNP instance path, case-insensitive (WMI and ADL
    // do not guarantee matching case). A uuid matching several groups
    // is treated as no match; duplicate PNP strings across cards mean
    // the identity cannot be trusted.
    for (gpu_index, uuid) in gpu_uuids.iter().enumerate() {
        if uuid.is_empty() {
            continue;
        }
        let hits: Vec<usize> = groups
            .iter()
            .enumerate()
            .filter(|(_, group)| {
                !group.pnp_string.is_empty() && group.pnp_string.eq_ignore_ascii_case(uuid)
            })
            .map(|(position, _)| position)
            .collect();
        if hits.len() == 1 {
            assigned[gpu_index] = Some(hits[0]);
        }
    }
    if has_duplicate_assignment(&assigned) {
        // Two GPUs resolved to one card: duplicate uuids in WMI or
        // duplicate PNP strings in ADL. Nothing derived from that
        // state is trustworthy.
        return AttributionPlan::Decline;
    }

    // Phase 2: PCI vendor+device, for GPUs the exact join missed.
    // Both the peer set (unmatched GPUs) and the candidate set
    // (unclaimed groups) are snapshotted before the loop so the
    // uniqueness tests are order-independent.
    let claimed: HashSet<usize> = assigned.iter().flatten().copied().collect();
    let gpu_ids: Vec<Option<PciIds>> = gpu_uuids
        .iter()
        .map(|uuid| parse_pnp_device_id(uuid))
        .collect();
    let group_ids: Vec<Option<PciIds>> = groups
        .iter()
        .map(|group| parse_pnp_device_id(&group.pnp_string))
        .collect();
    let unmatched: Vec<usize> = (0..gpu_uuids.len())
        .filter(|&gpu_index| assigned[gpu_index].is_none())
        .collect();
    for &gpu_index in &unmatched {
        let Some(ids) = gpu_ids[gpu_index] else {
            continue;
        };
        // Two unmatched identical cards cannot be told apart at this
        // strength; guessing between them is exactly what this module
        // must never do.
        let peers = unmatched
            .iter()
            .filter(|&&other| gpu_ids[other] == Some(ids))
            .count();
        if peers != 1 {
            continue;
        }
        let candidates: Vec<usize> = (0..groups.len())
            .filter(|position| !claimed.contains(position))
            .filter(|&position| group_ids[position] == Some(ids))
            .collect();
        if candidates.len() == 1 {
            assigned[gpu_index] = Some(candidates[0]);
        }
    }
    if has_duplicate_assignment(&assigned) {
        return AttributionPlan::Decline;
    }

    let matches: Vec<CardMatch> = assigned
        .iter()
        .enumerate()
        .filter_map(|(gpu_index, group)| {
            group.map(|position| CardMatch {
                gpu_index,
                adl_indices: groups[position].indices.clone(),
            })
        })
        .collect();
    if matches.is_empty() {
        AttributionPlan::Decline
    } else {
        AttributionPlan::PerCard(matches)
    }
}

fn has_duplicate_assignment(assigned: &[Option<usize>]) -> bool {
    let taken: Vec<usize> = assigned.iter().flatten().copied().collect();
    let unique: HashSet<usize> = taken.iter().copied().collect();
    taken.len() != unique.len()
}

/// One diagnostic line for a raw adapter row, valid or not.
///
/// Used by the `amd.adl.adapters` doctor check, whose whole purpose is
/// field verification of the transcribed layout: the strings are
/// rendered lossily and quoted so that garbage bytes, the signature of
/// a wrong layout, survive into the report instead of being hidden.
pub fn describe_raw_entry(slot: usize, entry: &ffi::AdapterInfo) -> String {
    format!(
        "[{slot}] index={} bus={} device={} function={} vendor={} name={:?} pnp={:?}",
        entry.i_adapter_index,
        entry.i_bus_number,
        entry.i_device_number,
        entry.i_function_number,
        entry.i_vendor_id,
        ffi::adl_string_lossy(&entry.str_adapter_name),
        ffi::adl_string_lossy(&entry.str_pnp_string),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adapter(index: i32, bdf: (i32, i32, i32), pnp: &str) -> AdlAdapter {
        AdlAdapter {
            index,
            bus: bdf.0,
            device: bdf.1,
            function: bdf.2,
            adapter_name: "AMD Radeon RX 7900 XTX".to_string(),
            pnp_string: pnp.to_string(),
        }
    }

    const DGPU_PNP: &str = r"PCI\VEN_1002&DEV_744C&SUBSYS_0E3A1002&REV_C8\6&2C6B35A1&0&00000019";
    const APU_PNP: &str = r"PCI\VEN_1002&DEV_164E&SUBSYS_00000000&REV_C1\4&2FD5AB1F&0&0041";

    #[test]
    fn several_indices_for_one_card_collapse_to_one_group() {
        // One card, three display outputs, enumerated out of order.
        let adapters = vec![
            adapter(2, (3, 0, 0), DGPU_PNP),
            adapter(0, (3, 0, 0), DGPU_PNP),
            adapter(1, (3, 0, 0), DGPU_PNP),
        ];
        let groups = group_by_card(&adapters);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].indices, vec![0, 1, 2]);
        assert_eq!(groups[0].pnp_string, DGPU_PNP);
    }

    #[test]
    fn two_cards_stay_two_groups_and_order_is_deterministic() {
        let adapters = vec![
            adapter(3, (8, 0, 0), DGPU_PNP),
            adapter(0, (4, 0, 0), APU_PNP),
            adapter(4, (8, 0, 0), DGPU_PNP),
            adapter(1, (4, 0, 0), APU_PNP),
        ];
        let groups = group_by_card(&adapters);
        assert_eq!(groups.len(), 2);
        // Sorted by BDF regardless of enumeration order.
        assert_eq!((groups[0].bus, groups[0].indices.clone()), (4, vec![0, 1]));
        assert_eq!((groups[1].bus, groups[1].indices.clone()), (8, vec![3, 4]));
    }

    #[test]
    fn an_implausible_bdf_row_is_excluded_without_poisoning_the_rest() {
        let adapters = vec![
            adapter(0, (3, 0, 0), DGPU_PNP),
            // Garbage BDF: not a PCI address anything could match.
            adapter(1, (-1, 900, 3), APU_PNP),
        ];
        let groups = group_by_card(&adapters);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].bus, 3);
    }

    #[test]
    fn a_blank_pnp_on_a_secondary_output_is_backfilled_from_a_sibling() {
        let adapters = vec![
            AdlAdapter {
                pnp_string: String::new(),
                ..adapter(0, (3, 0, 0), "")
            },
            adapter(1, (3, 0, 0), DGPU_PNP),
        ];
        let groups = group_by_card(&adapters);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].pnp_string, DGPU_PNP);
    }

    #[test]
    fn a_single_gpu_attributes_without_adapterinfo_exactly_as_before() {
        // The regression pin for pre-#353 behavior: one AMD GPU is
        // SoleGpu no matter what the adapter inventory says, including
        // when it is absent (old driver, failed layout verification).
        assert_eq!(
            plan_attribution(&["anything"], None),
            AttributionPlan::SoleGpu
        );
        let adapters = vec![adapter(0, (3, 0, 0), DGPU_PNP)];
        assert_eq!(
            plan_attribution(&[DGPU_PNP], Some(&adapters)),
            AttributionPlan::SoleGpu
        );
        assert_eq!(
            plan_attribution(&["AMD-GPU-0"], Some(&adapters)),
            AttributionPlan::SoleGpu
        );
    }

    #[test]
    fn zero_gpus_and_missing_or_invalid_inventory_decline() {
        assert_eq!(plan_attribution(&[], None), AttributionPlan::Decline);
        // Two GPUs but no validated inventory: the exact situation a
        // failed layout verification produces. Must fall back to the
        // baseline, not guess.
        assert_eq!(
            plan_attribution(&[DGPU_PNP, APU_PNP], None),
            AttributionPlan::Decline
        );
        // An inventory whose every row was implausible is as good as
        // no inventory.
        let garbage = vec![adapter(0, (-1, 900, 3), DGPU_PNP)];
        assert_eq!(
            plan_attribution(&[DGPU_PNP, APU_PNP], Some(&garbage)),
            AttributionPlan::Decline
        );
    }

    #[test]
    fn the_apu_plus_dgpu_laptop_matches_both_cards_exactly() {
        // The issue's motivating configuration: a Ryzen APU and a
        // Radeon dGPU, each with several display-output rows.
        let adapters = vec![
            adapter(0, (4, 0, 0), APU_PNP),
            adapter(1, (4, 0, 0), APU_PNP),
            adapter(2, (8, 0, 0), DGPU_PNP),
            adapter(3, (8, 0, 0), DGPU_PNP),
        ];
        // GPU order deliberately disagrees with adapter order, and the
        // uuid case differs: the join must be exact and case-blind.
        let uuids = [DGPU_PNP.to_lowercase(), APU_PNP.to_string()];
        let uuid_refs: Vec<&str> = uuids.iter().map(String::as_str).collect();
        let plan = plan_attribution(&uuid_refs, Some(&adapters));
        assert_eq!(
            plan,
            AttributionPlan::PerCard(vec![
                CardMatch {
                    gpu_index: 0,
                    adl_indices: vec![2, 3],
                },
                CardMatch {
                    gpu_index: 1,
                    adl_indices: vec![0, 1],
                },
            ])
        );
    }

    #[test]
    fn a_gpu_the_inventory_does_not_know_keeps_its_baseline() {
        let adapters = vec![adapter(0, (8, 0, 0), DGPU_PNP)];
        let plan = plan_attribution(&[DGPU_PNP, "AMD-GPU-1"], Some(&adapters));
        // Partial attribution: the matched card is augmented, the
        // unknown one is simply left alone.
        assert_eq!(
            plan,
            AttributionPlan::PerCard(vec![CardMatch {
                gpu_index: 0,
                adl_indices: vec![0],
            }])
        );
    }

    #[test]
    fn a_card_matched_only_by_pci_ids_still_matches_when_unambiguous() {
        // The PNP strings disagree (a driver rendering the instance
        // suffix differently), but each vendor+device pair is unique
        // on both sides, so the fallback may pair them.
        let adapters = vec![
            adapter(0, (4, 0, 0), r"PCI\VEN_1002&DEV_164E&SUBSYS_0&REV_C1\OTHER"),
            adapter(1, (8, 0, 0), r"PCI\VEN_1002&DEV_744C&SUBSYS_0&REV_C8\OTHER"),
        ];
        let plan = plan_attribution(&[DGPU_PNP, APU_PNP], Some(&adapters));
        assert_eq!(
            plan,
            AttributionPlan::PerCard(vec![
                CardMatch {
                    gpu_index: 0,
                    adl_indices: vec![1],
                },
                CardMatch {
                    gpu_index: 1,
                    adl_indices: vec![0],
                },
            ])
        );
    }

    #[test]
    fn two_identical_cards_with_mismatched_pnp_strings_decline() {
        // Same vendor+device twice on both sides, and the exact join
        // failed: there is no way to know which is which. Declining is
        // required; ordinal guessing here would silently swap the two
        // cards' temperatures.
        let uuid_a = r"PCI\VEN_1002&DEV_744C&SUBSYS_0\6&AAAA&0&19";
        let uuid_b = r"PCI\VEN_1002&DEV_744C&SUBSYS_0\6&BBBB&0&19";
        let adapters = vec![
            adapter(0, (3, 0, 0), r"PCI\VEN_1002&DEV_744C&SUBSYS_0\6&CCCC&0&19"),
            adapter(1, (8, 0, 0), r"PCI\VEN_1002&DEV_744C&SUBSYS_0\6&DDDD&0&19"),
        ];
        assert_eq!(
            plan_attribution(&[uuid_a, uuid_b], Some(&adapters)),
            AttributionPlan::Decline
        );
    }

    #[test]
    fn the_fallback_never_pairs_across_vendors_or_devices() {
        // A vendor id that disagrees (an Intel iGPU uuid arriving in
        // an AMD reader through some future refactoring accident) must
        // find nothing, per the match_adapter rule.
        let intel_uuid = r"PCI\VEN_8086&DEV_56A0&SUBSYS_0\3&11583659&0&10";
        let adapters = vec![
            adapter(0, (4, 0, 0), APU_PNP),
            adapter(1, (8, 0, 0), DGPU_PNP),
        ];
        let plan = plan_attribution(&[intel_uuid, DGPU_PNP], Some(&adapters));
        // The AMD dGPU still matches exactly; the foreign uuid matches
        // nothing rather than being ordinal-guessed onto the APU.
        assert_eq!(
            plan,
            AttributionPlan::PerCard(vec![CardMatch {
                gpu_index: 1,
                adl_indices: vec![1],
            }])
        );
    }

    #[test]
    fn duplicate_identities_on_either_side_decline_entirely() {
        // Two GPUs with the same uuid resolving to one card is a
        // corrupt-identity state; attributing either would be a guess.
        let adapters = vec![adapter(0, (8, 0, 0), DGPU_PNP)];
        assert_eq!(
            plan_attribution(&[DGPU_PNP, DGPU_PNP], Some(&adapters)),
            AttributionPlan::Decline
        );
    }

    #[test]
    fn synthetic_uuids_match_nothing() {
        // `amd_windows` synthesizes `AMD-GPU-{n}` when WMI has no
        // PNPDeviceID; those carry no identity and must never match.
        let adapters = vec![
            adapter(0, (4, 0, 0), APU_PNP),
            adapter(1, (8, 0, 0), DGPU_PNP),
        ];
        assert_eq!(
            plan_attribution(&["AMD-GPU-0", "AMD-GPU-1"], Some(&adapters)),
            AttributionPlan::Decline
        );
    }

    #[test]
    fn ffi_rows_round_trip_into_owned_adapters() {
        let mut entry = ffi::AdapterInfo {
            i_adapter_index: 3,
            i_bus_number: 8,
            i_device_number: 0,
            i_function_number: 0,
            i_vendor_id: 1002,
            ..ffi::AdapterInfo::default()
        };
        entry.str_adapter_name[..22].copy_from_slice(b"AMD Radeon RX 7900 XTX");
        entry.str_pnp_string[..DGPU_PNP.len()].copy_from_slice(DGPU_PNP.as_bytes());

        let parsed = parse_adapters(&[entry]);
        assert_eq!(
            parsed,
            vec![AdlAdapter {
                index: 3,
                bus: 8,
                device: 0,
                function: 0,
                adapter_name: "AMD Radeon RX 7900 XTX".to_string(),
                pnp_string: DGPU_PNP.to_string(),
            }]
        );

        // A row whose string field is garbage (only reachable if a
        // caller skips validation) degrades to an empty string, which
        // matches nothing, rather than panicking or carrying garbage
        // into the matcher.
        entry.str_pnp_string = [0xFF; ffi::ADL_MAX_PATH];
        assert_eq!(parse_adapters(&[entry])[0].pnp_string, "");
    }

    #[test]
    fn raw_rows_render_with_quoted_strings_for_the_doctor() {
        let mut entry = ffi::AdapterInfo {
            i_adapter_index: 2,
            i_bus_number: 8,
            i_device_number: 0,
            i_function_number: 0,
            i_vendor_id: 1002,
            ..ffi::AdapterInfo::default()
        };
        entry.str_adapter_name[..4].copy_from_slice(b"card");
        entry.str_pnp_string[..3].copy_from_slice(b"PCI");
        let line = describe_raw_entry(0, &entry);
        assert_eq!(
            line,
            "[0] index=2 bus=8 device=0 function=0 vendor=1002 name=\"card\" pnp=\"PCI\""
        );
    }
}
