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

/// Upper bound on the adapter count accepted from the driver.
///
/// A real machine has a handful of ADL rows (one per display output per
/// card; a four-card workstation with six outputs each is 24). The bound
/// keeps a garbage count from demanding an absurd `AdapterInfo`
/// allocation, and it keeps `ffi::AdapterInfoArray::input_size` far away
/// from `c_int` overflow. Kept here, not in `loader`, so
/// [`plausible_adapter_count`] and [`clamp_scan_count`] are testable on
/// the Linux runner even though every caller lives behind
/// `cfg(target_os = "windows")`.
pub const MAX_ADAPTER_ROWS: i32 = 64;

/// Whether a driver-reported adapter count is plausible enough to trust
/// for identity-bearing `AdapterInfo` attribution.
///
/// Used by `loader::probe_adapter_info`: a count outside
/// `1..=MAX_ADAPTER_ROWS` is treated as a failed call, which multi-GPU
/// attribution then answers by declining rather than guessing at a
/// buffer size for a count that does not look real.
pub fn plausible_adapter_count(count: i32) -> bool {
    (1..=MAX_ADAPTER_ROWS).contains(&count)
}

/// Clamp a driver-reported adapter count for the PMLog capability scan.
///
/// Unlike [`plausible_adapter_count`], an implausibly large count here is
/// clamped rather than rejected outright. `loader::scan_for_capable_adapter`
/// makes one `ADL2_Overdrive_Caps` call per index and may make one PMLog
/// read per index, all while holding the process-wide runtime lock, so a
/// garbage count would wedge the refresh loop rather than merely waste a
/// little work; clamping instead of rejecting keeps a machine with an
/// implausibly long adapter list working on its first rows, since the
/// scan only needs *one* PMLog-capable index, not an exhaustive one.
pub fn clamp_scan_count(count: i32) -> i32 {
    count.min(MAX_ADAPTER_ROWS)
}

/// Describe which failure shape a rejected `AdapterInfo` table shows,
/// for the `amd.adl.adapters` doctor check.
///
/// The failure shapes call for different corrections, so name the one
/// that was seen, most specific first:
///
/// - **Garbled rows**: written bytes that contradict the layout, the
///   unambiguous wrong-field-offset-or-stride error. A declared struct
///   *larger* than the driver's lands here too: more rows fit
///   `iInputSize` than the driver's own stride assumes, so the driver
///   writes at a smaller stride and row 1 onward reads misaligned.
/// - **Row 0 untouched**: the poison pre-fill is intact, the call
///   wrote nothing at all.
/// - **No populated rows**: the driver memset the buffer (or parts of
///   it) but described no adapter.
/// - **Duplicate adapter indices**: two populated rows claim the same
///   `iAdapterIndex`, which no healthy enumeration produces and a
///   stride mismatch does.
///
/// A trailing untouched run *with* a populated row 0, and blank rows in
/// any position, are no longer failures (see
/// `ffi::AdapterInfoArray::validated`); they do not reach this
/// function through the verification path.
pub fn describe_layout_failure(rows: &[ffi::AdapterInfo]) -> String {
    let states: Vec<ffi::RowState> = rows.iter().map(ffi::AdapterInfo::classify).collect();
    let count = |wanted: ffi::RowState| states.iter().filter(|state| **state == wanted).count();
    let garbled = count(ffi::RowState::Garbled);
    let untouched = count(ffi::RowState::Untouched);
    let populated = count(ffi::RowState::Populated);
    let total = rows.len();

    if garbled > 0 {
        return format!(
            "{garbled} of {total} row(s) hold written bytes that contradict the layout, \
             which is what a wrong field offset or stride produces (a declared AdapterInfo \
             larger than the driver's also lands here: the driver strides shorter and later \
             rows read misaligned)"
        );
    }
    if states.first() == Some(&ffi::RowState::Untouched) {
        return format!(
            "row 0 still carries the poison pre-fill ({untouched} of {total} row(s) \
             untouched): the call wrote nothing at all"
        );
    }
    if populated == 0 {
        return format!(
            "the driver memset the buffer but populated none of the {total} row(s): no \
             adapter was described"
        );
    }
    // The remaining rejection: duplicate iAdapterIndex among the
    // populated rows.
    "the populated rows repeat an iAdapterIndex value, which no healthy enumeration \
     produces and a stride mismatch does"
        .to_string()
}

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

/// Whether two ADL `strPNPString` values name the same physical device.
///
/// ADL does not repeat one instance path across a card's rows. Observed
/// on an AMD Radeon(TM) 8060S (driver 32.0.31035.1003): the primary row
/// carries the base device instance path, the path WMI reports as
/// `PNPDeviceID`, and each additional display-output row carries that
/// same path with a `&02`, `&03`, ... suffix appended. So sameness is
/// "one extends the other at a separator", not equality.
///
/// The `&` boundary is load-bearing. Without it two genuinely distinct
/// paths that merely share a prefix, `...&0&0041` and `...&0&00410`,
/// would read as one card. Comparison is ASCII-case-insensitive because
/// WMI and ADL do not agree on case.
fn shares_device_instance(a: &str, b: &str) -> bool {
    let (short, long) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    let (short, long) = (short.as_bytes(), long.as_bytes());
    if !long[..short.len()].eq_ignore_ascii_case(short) {
        return false;
    }
    long.len() == short.len() || long[short.len()] == b'&'
}

/// Group adapter rows into one [`CardGroup`] per physical card.
///
/// Groups are sorted by bus/device/function and their indices sorted
/// ascending, so the output is deterministic regardless of the order
/// the driver enumerated in.
///
/// A group whose rows carry two unrelated non-empty PNP instance paths
/// is dropped rather than returned. Grouping trusts the PCI
/// bus/device/function to identify a physical card, so a driver that
/// leaves those fields unfilled collapses every card into one group.
/// The caller then samples telemetry from the first index of the group
/// that answers PMLog, which can be a different physical card's index,
/// and one card's temperature, power, and fan would be reported for
/// another GPU with nothing in the output saying so. Two unrelated
/// instance paths under one PCI address is the observable signature of
/// that state, and dropping the group is the same answer the rest of
/// this module gives everywhere else: decline rather than guess.
///
/// "Unrelated" is judged by [`shares_device_instance`], not by string
/// equality: a card's display-output rows extend its base path rather
/// than repeating it (see that function). A row may also leave the path
/// blank, which is no evidence either way and is skipped. The group's
/// own `pnp_string` is the shortest path its rows carried, which is the
/// base path, and therefore the only form the exact join in
/// [`plan_attribution`] can match against a WMI `PNPDeviceID`.
///
/// Agreement is judged once per group against that base rather than
/// pairwise as rows arrive, so the verdict does not depend on which row
/// the driver enumerated first.
pub fn group_by_card(adapters: &[AdlAdapter]) -> Vec<CardGroup> {
    let mut groups: Vec<CardGroup> = Vec::new();
    // Parallel to `groups`: every non-empty instance path the group's
    // rows carried, checked for agreement once the group is complete.
    let mut paths: Vec<Vec<String>> = Vec::new();
    for adapter in adapters {
        if !plausible_bdf(adapter) {
            continue;
        }
        let existing = groups.iter().position(|group| {
            group.bus == adapter.bus
                && group.device == adapter.device
                && group.function == adapter.function
        });
        let position = match existing {
            Some(position) => {
                let group = &mut groups[position];
                group.indices.push(adapter.index);
                if group.adapter_name.is_empty() && !adapter.adapter_name.is_empty() {
                    group.adapter_name = adapter.adapter_name.clone();
                }
                position
            }
            None => {
                groups.push(CardGroup {
                    bus: adapter.bus,
                    device: adapter.device,
                    function: adapter.function,
                    adapter_name: adapter.adapter_name.clone(),
                    // Resolved below, once every row of this group has
                    // been seen and the shortest path is known.
                    pnp_string: String::new(),
                    indices: vec![adapter.index],
                });
                paths.push(Vec::new());
                groups.len() - 1
            }
        };
        if !adapter.pnp_string.is_empty() {
            paths[position].push(adapter.pnp_string.clone());
        }
    }
    let mut groups: Vec<CardGroup> = groups
        .into_iter()
        .zip(paths)
        .filter_map(|(mut group, paths)| {
            // No row carried a path: nothing contradicts anything, so
            // the group survives with an empty `pnp_string` that simply
            // matches no GPU downstream, exactly as before.
            let Some(base) = paths.iter().min_by_key(|path| path.len()) else {
                return Some(group);
            };
            if !paths.iter().all(|path| shares_device_instance(base, path)) {
                return None;
            }
            group.pnp_string = base.clone();
            Some(group)
        })
        .collect();
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
/// field verification of the transcribed layout. Each line names the
/// row's [`ffi::RowState`], which is the distinction the poison
/// pre-fill exists to make visible: a real-hardware dump then says
/// decisively whether a non-populated row was memset by the driver
/// (normal `iPresent`-style filtering) or never written at all (a
/// short write). Populated and garbled rows render their fields, the
/// strings lossily and quoted so that garbage bytes, the signature of
/// a wrong layout, survive into the report instead of being hidden;
/// blank and untouched rows have no field content worth printing, so
/// they render as their state alone.
pub fn describe_raw_entry(slot: usize, entry: &ffi::AdapterInfo) -> String {
    match entry.classify() {
        ffi::RowState::Untouched => {
            format!("[{slot}] UNTOUCHED (poison intact, driver never wrote)")
        }
        ffi::RowState::Blank => format!("[{slot}] BLANK (driver memset, not populated)"),
        state => {
            let tag = match state {
                ffi::RowState::Garbled => "GARBLED ",
                _ => "",
            };
            format!(
                "[{slot}] {tag}index={} bus={} device={} function={} vendor={} name={:?} pnp={:?}",
                entry.i_adapter_index,
                entry.i_bus_number,
                entry.i_device_number,
                entry.i_function_number,
                entry.i_vendor_id,
                ffi::adl_string_lossy(&entry.str_adapter_name),
                ffi::adl_string_lossy(&entry.str_pnp_string),
            )
        }
    }
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

    // Observed on a real host, 2026-08-18: AMD Radeon(TM) 8060S
    // Graphics (Strix Halo APU), driver 32.0.31035.1003, Windows 11.
    // ADL enumerated five rows for the single physical card, all
    // sharing bus 189 / device 0 / function 0, but each secondary
    // display-output row carried a *distinct* instance path: the base
    // path with an `&02`..`&05` suffix appended. Not blank, not merely
    // case-differing, genuinely different -- which is the one shape
    // `group_by_card` treats as a conflict and drops.
    const REAL_8060S_BASE: &str = r"PCI\VEN_1002&DEV_1586&SUBSYS_B0261F4C&REV_C1\4&2368981F&0&0041";

    #[test]
    fn real_display_output_rows_do_not_dissolve_their_card() {
        let mut adapters = vec![adapter(0, (189, 0, 0), REAL_8060S_BASE)];
        let suffixed: Vec<String> = (2..=5).map(|n| format!("{REAL_8060S_BASE}&0{n}")).collect();
        for (offset, pnp) in suffixed.iter().enumerate() {
            adapters.push(adapter(offset as i32 + 1, (189, 0, 0), pnp));
        }
        let groups = group_by_card(&adapters);
        assert_eq!(
            groups.len(),
            1,
            "five ADL rows of one physical card must yield one group"
        );
        assert_eq!(groups[0].indices, vec![0, 1, 2, 3, 4]);
        // The base path is the one WMI reports as PNPDeviceID, so it is
        // the only value the exact join in `plan_attribution` can match.
        assert_eq!(groups[0].pnp_string, REAL_8060S_BASE);
    }

    #[test]
    fn the_base_path_wins_however_the_driver_enumerated_the_rows() {
        // Display-output rows first, base path last. Grouping decides
        // against the shortest path once the group is complete, so the
        // verdict does not depend on enumeration order.
        let second = format!("{REAL_8060S_BASE}&02");
        let third = format!("{REAL_8060S_BASE}&03");
        let adapters = vec![
            adapter(1, (189, 0, 0), &second),
            adapter(2, (189, 0, 0), &third),
            adapter(0, (189, 0, 0), REAL_8060S_BASE),
        ];
        let groups = group_by_card(&adapters);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].indices, vec![0, 1, 2]);
        assert_eq!(groups[0].pnp_string, REAL_8060S_BASE);
    }

    #[test]
    fn a_shared_prefix_that_is_not_a_path_boundary_is_still_a_conflict() {
        // `...&0&0041` and `...&0&00410` are different devices that
        // happen to share a prefix. Only an extension at a `&` boundary
        // is a display-output sibling, so these must not merge.
        let lookalike = format!("{REAL_8060S_BASE}0");
        let adapters = vec![
            adapter(0, (189, 0, 0), REAL_8060S_BASE),
            adapter(1, (189, 0, 0), &lookalike),
        ];
        assert!(group_by_card(&adapters).is_empty());
    }

    #[test]
    fn display_output_rows_no_longer_block_multi_gpu_attribution() {
        // The configuration this feature exists for, in the row shape
        // real hardware produces: an APU and a dGPU, each enumerating a
        // base row plus a display-output row. Before grouping keyed on
        // the base path, both groups were dropped as conflicting and
        // this returned `Decline`, which made the whole feature inert.
        let apu_second = format!("{APU_PNP}&02");
        let dgpu_second = format!("{DGPU_PNP}&02");
        let adapters = vec![
            adapter(0, (4, 0, 0), APU_PNP),
            adapter(1, (4, 0, 0), &apu_second),
            adapter(2, (8, 0, 0), DGPU_PNP),
            adapter(3, (8, 0, 0), &dgpu_second),
        ];
        assert_eq!(
            plan_attribution(&[DGPU_PNP, APU_PNP], Some(&adapters)),
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
    fn a_bdf_group_with_contradictory_pnp_strings_is_dropped_rather_than_merged() {
        // A driver that leaves bus/device/function unfilled puts two
        // physical cards under one PCI address. Merging them yields a
        // card whose index list spans both, and the caller samples the
        // first index that answers, so one card's telemetry would be
        // reported for the other GPU with nothing in the output saying
        // so. Two different instance paths under one address is the
        // signature of that state.
        let adapters = vec![
            adapter(0, (0, 0, 0), APU_PNP),
            adapter(1, (0, 0, 0), DGPU_PNP),
        ];
        assert!(group_by_card(&adapters).is_empty());
        // With no trustworthy group left, attribution declines rather
        // than guessing.
        assert_eq!(
            plan_attribution(&[DGPU_PNP, APU_PNP], Some(&adapters)),
            AttributionPlan::Decline
        );
    }

    #[test]
    fn a_case_differing_pnp_string_on_a_sibling_row_is_not_a_conflict() {
        // WMI and ADL do not agree on case, and neither need two rows
        // of one card; only a genuinely different instance path is a
        // conflict.
        let lowered = DGPU_PNP.to_lowercase();
        let adapters = vec![
            adapter(0, (3, 0, 0), DGPU_PNP),
            adapter(1, (3, 0, 0), &lowered),
        ];
        let groups = group_by_card(&adapters);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].indices, vec![0, 1]);
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
    fn adapter_count_is_plausible_only_within_the_positive_bound() {
        assert!(!plausible_adapter_count(0));
        assert!(!plausible_adapter_count(-1));
        assert!(plausible_adapter_count(1));
        assert!(plausible_adapter_count(MAX_ADAPTER_ROWS));
        assert!(!plausible_adapter_count(MAX_ADAPTER_ROWS + 1));
    }

    #[test]
    fn scan_count_clamps_to_the_upper_bound_without_rejecting_it() {
        // Below the bound: untouched.
        assert_eq!(clamp_scan_count(3), 3);
        // At the bound: untouched.
        assert_eq!(clamp_scan_count(MAX_ADAPTER_ROWS), MAX_ADAPTER_ROWS);
        // Above the bound: clamped, not rejected, unlike
        // `plausible_adapter_count`.
        assert_eq!(clamp_scan_count(MAX_ADAPTER_ROWS + 1), MAX_ADAPTER_ROWS);
        assert_eq!(clamp_scan_count(10_000), MAX_ADAPTER_ROWS);
    }

    /// A populated `AdapterInfo` row: written and passing `looks_sane`,
    /// which is what `classify` requires for `RowState::Populated`.
    fn written_row(index: i32) -> ffi::AdapterInfo {
        let mut entry = ffi::AdapterInfo {
            i_adapter_index: index,
            i_bus_number: 3,
            ..ffi::AdapterInfo::default()
        };
        entry.str_adapter_name[..4].copy_from_slice(b"card");
        entry
    }

    /// A garbled row: written bytes whose strings cannot be parsed.
    fn garbled_row(index: i32) -> ffi::AdapterInfo {
        let mut entry = written_row(index);
        entry.str_pnp_string = [0xFF; ffi::ADL_MAX_PATH];
        entry
    }

    #[test]
    fn layout_failure_names_garbled_rows_first() {
        // Garbled bytes are the unambiguous layout error, so they win
        // over any other shape present in the same table.
        let rows = vec![written_row(0), garbled_row(1), ffi::AdapterInfo::poisoned()];
        let shape = describe_layout_failure(&rows);
        assert!(shape.contains("1 of 3"), "{shape}");
        assert!(shape.contains("contradict the layout"), "{shape}");
        assert!(shape.contains("stride"), "{shape}");
    }

    #[test]
    fn layout_failure_names_a_dead_call_when_row_zero_is_untouched() {
        let rows = vec![ffi::AdapterInfo::poisoned(), ffi::AdapterInfo::poisoned()];
        let shape = describe_layout_failure(&rows);
        assert!(shape.contains("poison"), "{shape}");
        assert!(shape.contains("2 of 2"), "{shape}");
        assert!(shape.contains("wrote nothing at all"), "{shape}");
    }

    #[test]
    fn layout_failure_names_an_empty_memset_when_nothing_was_populated() {
        let rows = vec![ffi::AdapterInfo::default(), ffi::AdapterInfo::default()];
        let shape = describe_layout_failure(&rows);
        assert!(shape.contains("memset"), "{shape}");
        assert!(shape.contains("no adapter was described"), "{shape}");
    }

    #[test]
    fn layout_failure_names_duplicate_indices_as_the_remaining_shape() {
        // Two populated rows with the same iAdapterIndex: the only
        // rejection left once nothing is garbled, row 0 is populated,
        // and something was populated.
        let rows = vec![written_row(0), written_row(0)];
        let shape = describe_layout_failure(&rows);
        assert!(shape.contains("repeat an iAdapterIndex"), "{shape}");
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
        // A populated row keeps the plain field format, untagged.
        assert_eq!(
            line,
            "[0] index=2 bus=8 device=0 function=0 vendor=1002 name=\"card\" pnp=\"PCI\""
        );
    }

    #[test]
    fn raw_rows_carry_their_state_so_the_dump_is_decisive() {
        // The poison fill exists so the eventual real-hardware dump can
        // settle whether a non-populated row was memset by the driver
        // or never written; the per-row tag is where that distinction
        // becomes visible to the operator.
        assert_eq!(
            describe_raw_entry(1, &ffi::AdapterInfo::default()),
            "[1] BLANK (driver memset, not populated)"
        );
        assert_eq!(
            describe_raw_entry(2, &ffi::AdapterInfo::poisoned()),
            "[2] UNTOUCHED (poison intact, driver never wrote)"
        );
        let line = describe_raw_entry(3, &garbled_row(7));
        assert!(line.starts_with("[3] GARBLED index=7"), "{line}");
        // The garbage bytes survive, lossily, as evidence.
        assert!(line.contains("pnp="), "{line}");
    }
}
