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

//! Identifier parsing and sample aggregation for the Windows GPU
//! performance layer.
//!
//! Everything here is platform-independent on purpose. The DXGI and PDH
//! FFI lives in the sibling `dxgi` / `pdh` modules behind
//! `#[cfg(target_os = "windows")]`, but the string parsing and the
//! aggregation arithmetic compile and run everywhere.
//!
//! That split matters more in this crate than it usually would: no CI
//! job compiles all-smi for Windows at all (the only Windows workflow
//! job is gated behind an unset repository variable and has never
//! executed), so any logic hidden behind a `cfg(windows)` gate ships
//! with zero automated coverage. Keeping the parsing and the maths out
//! here means the Linux test runner exercises the part that is actually
//! easy to get wrong.

use std::collections::HashMap;

/// Windows `LUID`, the locally unique identifier the display stack uses
/// to name a graphics adapter.
///
/// DXGI reports it as `DXGI_ADAPTER_DESC1::AdapterLuid`, and PDH embeds
/// it in every GPU counter instance name. Correlating the two is what
/// lets a utilization sample be attributed to a specific card.
///
/// The value is only unique until the next reboot, so it is used purely
/// as an in-process join key and never persisted or exported.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct AdapterLuid {
    /// `LUID::HighPart`. Signed in the Win32 headers, and almost always
    /// zero in practice.
    pub high: i32,
    /// `LUID::LowPart`.
    pub low: u32,
}

impl AdapterLuid {
    pub fn new(high: i32, low: u32) -> Self {
        Self { high, low }
    }
}

/// PCI vendor and device identifiers parsed out of a WMI `PNPDeviceID`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PciIds {
    pub vendor: u32,
    pub device: u32,
}

/// A parsed `\GPU Engine(...)` counter instance name.
#[derive(Clone, Debug, PartialEq)]
pub struct GpuEngineInstance {
    pub pid: u32,
    pub luid: AdapterLuid,
    pub phys: u32,
    pub eng: u32,
    pub engtype: String,
}

/// A parsed `\GPU Process Memory(...)` counter instance name.
#[derive(Clone, Debug, PartialEq)]
pub struct GpuProcessMemoryInstance {
    pub pid: u32,
    pub luid: AdapterLuid,
    pub phys: u32,
}

/// A parsed `\GPU Adapter Memory(...)` counter instance name.
///
/// Note the absence of a pid: this counter family is per-adapter and
/// system-wide, which is exactly why it, rather than DXGI's
/// process-scoped `QueryVideoMemoryInfo`, is the source for used VRAM.
#[derive(Clone, Debug, PartialEq)]
pub struct GpuAdapterMemoryInstance {
    pub luid: AdapterLuid,
    pub phys: u32,
}

/// Minimal identity of a DXGI adapter.
///
/// Split out from the FFI struct so [`match_adapter`] can be unit-tested
/// on a host with no DXGI.
#[derive(Clone, Debug, PartialEq)]
pub struct AdapterIdentity {
    pub luid: AdapterLuid,
    pub vendor_id: u32,
    pub device_id: u32,
    pub description: String,
}

// ---------------------------------------------------------------------
// Token helpers
// ---------------------------------------------------------------------

/// Consume `expected` as the next token, returning the remaining tokens.
fn expect<'t, 's>(tokens: &'t [&'s str], expected: &str) -> Option<&'t [&'s str]> {
    match tokens.split_first() {
        Some((first, rest)) if first.eq_ignore_ascii_case(expected) => Some(rest),
        _ => None,
    }
}

/// Pop the next token.
fn take<'t, 's>(tokens: &'t [&'s str]) -> Option<(&'s str, &'t [&'s str])> {
    tokens.split_first().map(|(first, rest)| (*first, rest))
}

/// Parse `0x0000ABCD`, or a bare hex string, into a `u32`.
fn parse_hex_u32(text: &str) -> Option<u32> {
    let body = text
        .strip_prefix("0x")
        .or_else(|| text.strip_prefix("0X"))
        .unwrap_or(text);
    if body.is_empty() {
        return None;
    }
    u32::from_str_radix(body, 16).ok()
}

/// Build an [`AdapterLuid`] from the two hex tokens of a counter
/// instance name.
///
/// The counter name renders `HighPart` first and `LowPart` second, both
/// as unsigned hex, even though `HighPart` is signed in the Win32
/// headers. Parse wide and reinterpret rather than rejecting the high
/// bit.
fn parse_luid_pair(high: &str, low: &str) -> Option<AdapterLuid> {
    Some(AdapterLuid {
        high: parse_hex_u32(high)? as i32,
        low: parse_hex_u32(low)?,
    })
}

// ---------------------------------------------------------------------
// Instance-name parsers
// ---------------------------------------------------------------------

/// Parse `pid_<pid>_luid_<hi>_<lo>_phys_<n>_eng_<n>_engtype_<type>`.
pub fn parse_gpu_engine_instance(instance: &str) -> Option<GpuEngineInstance> {
    let tokens: Vec<&str> = instance.split('_').collect();
    let rest = expect(&tokens, "pid")?;
    let (pid, rest) = take(rest)?;
    let rest = expect(rest, "luid")?;
    let (high, rest) = take(rest)?;
    let (low, rest) = take(rest)?;
    let rest = expect(rest, "phys")?;
    let (phys, rest) = take(rest)?;
    let rest = expect(rest, "eng")?;
    let (eng, rest) = take(rest)?;
    let rest = expect(rest, "engtype")?;
    // Shipping engine-type names carry no underscore ("3D", "Compute",
    // "VideoDecode"), but rejoin the tail defensively so a future
    // multi-word type is not silently truncated to its first word.
    //
    // Test the joined string rather than the token slice: a name ending
    // in a bare `engtype_` splits to a single empty token, which is not
    // an empty slice but is still a nameless engine.
    let engtype = rest.join("_");
    if engtype.is_empty() {
        return None;
    }
    Some(GpuEngineInstance {
        pid: pid.parse().ok()?,
        luid: parse_luid_pair(high, low)?,
        phys: phys.parse().ok()?,
        eng: eng.parse().ok()?,
        engtype,
    })
}

/// Parse `pid_<pid>_luid_<hi>_<lo>_phys_<n>`.
pub fn parse_gpu_process_memory_instance(instance: &str) -> Option<GpuProcessMemoryInstance> {
    let tokens: Vec<&str> = instance.split('_').collect();
    let rest = expect(&tokens, "pid")?;
    let (pid, rest) = take(rest)?;
    let rest = expect(rest, "luid")?;
    let (high, rest) = take(rest)?;
    let (low, rest) = take(rest)?;
    let rest = expect(rest, "phys")?;
    let (phys, _rest) = take(rest)?;
    Some(GpuProcessMemoryInstance {
        pid: pid.parse().ok()?,
        luid: parse_luid_pair(high, low)?,
        phys: phys.parse().ok()?,
    })
}

/// Parse `luid_<hi>_<lo>_phys_<n>`.
pub fn parse_gpu_adapter_memory_instance(instance: &str) -> Option<GpuAdapterMemoryInstance> {
    let tokens: Vec<&str> = instance.split('_').collect();
    let rest = expect(&tokens, "luid")?;
    let (high, rest) = take(rest)?;
    let (low, rest) = take(rest)?;
    let rest = expect(rest, "phys")?;
    let (phys, _rest) = take(rest)?;
    Some(GpuAdapterMemoryInstance {
        luid: parse_luid_pair(high, low)?,
        phys: phys.parse().ok()?,
    })
}

/// Extract PCI vendor and device identifiers from a WMI `PNPDeviceID`.
///
/// The field looks like
/// `PCI\VEN_1002&DEV_744C&SUBSYS_00000000&REV_C8\6&1a2b3c4d&0&00000019`.
/// Only the `VEN_` and `DEV_` fragments are of interest; everything else
/// varies with slot and revision and is useless as a join key.
///
/// Returns `None` for non-PCI enumerations (for example the
/// `ROOT\BasicDisplay` entry that a headless or RDP session presents),
/// which is the correct answer: those have no DXGI counterpart worth
/// matching.
pub fn parse_pnp_device_id(pnp: &str) -> Option<PciIds> {
    let upper = pnp.to_ascii_uppercase();
    Some(PciIds {
        vendor: extract_hex_after(&upper, "VEN_")?,
        device: extract_hex_after(&upper, "DEV_")?,
    })
}

fn extract_hex_after(haystack: &str, marker: &str) -> Option<u32> {
    let start = haystack.find(marker)? + marker.len();
    let digits: String = haystack[start..]
        .chars()
        .take_while(|c| c.is_ascii_hexdigit())
        .collect();
    if digits.is_empty() {
        return None;
    }
    u32::from_str_radix(&digits, 16).ok()
}

// ---------------------------------------------------------------------
// Aggregation
// ---------------------------------------------------------------------

/// Engine types that count toward the headline device utilization.
///
/// Video decode, encode, and copy engines are deliberately excluded. A
/// desktop compositor decoding a video stream keeps those engines busy
/// while the shader cores idle, and folding them in would make an idle
/// machine report a large non-zero GPU load, which is not what the
/// gauge in the TUI means.
pub const UTILIZATION_ENGINE_TYPES: &[&str] = &["3D", "Compute"];

pub fn is_utilization_engine_type(engtype: &str) -> bool {
    UTILIZATION_ENGINE_TYPES
        .iter()
        .any(|candidate| engtype.eq_ignore_ascii_case(candidate))
}

/// Reduce raw `\GPU Engine(*)\Utilization Percentage` samples to one
/// utilization figure per adapter.
///
/// The reduction is two steps, and the asymmetry between them is
/// deliberate:
///
/// 1. **Sum across processes** for a given engine. Each sample is one
///    process's share of that engine's time, so their sum is the
///    engine's total busy fraction.
/// 2. **Take the maximum across engines** of an adapter rather than
///    summing them. A card exposes several 3D and Compute engine
///    instances; adding them together yields figures far above 100% and
///    would peg every gauge in the UI. The maximum is what Task
///    Manager's headline GPU percentage reports, so this agrees with the
///    number a Windows user sees in the tool sitting next to us.
///
/// Results are clamped to `0..=100`: PDH can return small negative
/// values or slight overshoots on the sample right after a counter is
/// added, and a negative utilization would underflow the gauge maths
/// downstream.
pub fn aggregate_engine_utilization(
    samples: impl IntoIterator<Item = (GpuEngineInstance, f64)>,
) -> HashMap<AdapterLuid, f64> {
    // Key on the full counter identity rather than just the engine
    // index. In shipping drivers one index carries one engine type, so
    // this is equivalent, but keying on the identity means an unexpected
    // pairing merges nothing silently.
    let mut per_engine: HashMap<(AdapterLuid, u32, u32, String), f64> = HashMap::new();
    for (instance, value) in samples {
        if !is_utilization_engine_type(&instance.engtype) {
            continue;
        }
        if !value.is_finite() {
            continue;
        }
        let key = (
            instance.luid,
            instance.phys,
            instance.eng,
            instance.engtype.to_ascii_uppercase(),
        );
        *per_engine.entry(key).or_insert(0.0) += value;
    }

    let mut per_adapter: HashMap<AdapterLuid, f64> = HashMap::new();
    for ((luid, _, _, _), busy) in per_engine {
        let slot = per_adapter.entry(luid).or_insert(0.0);
        if busy > *slot {
            *slot = busy;
        }
    }
    for value in per_adapter.values_mut() {
        *value = value.clamp(0.0, 100.0);
    }
    per_adapter
}

/// Sum `\GPU Adapter Memory(*)\Dedicated Usage` samples per adapter.
///
/// Summing is right here, unlike for engines: an adapter can expose
/// several `phys` segments and their dedicated allocations are disjoint.
pub fn aggregate_adapter_memory(
    samples: impl IntoIterator<Item = (GpuAdapterMemoryInstance, f64)>,
) -> HashMap<AdapterLuid, u64> {
    let mut per_adapter: HashMap<AdapterLuid, f64> = HashMap::new();
    for (instance, value) in samples {
        if !value.is_finite() || value < 0.0 {
            continue;
        }
        *per_adapter.entry(instance.luid).or_insert(0.0) += value;
    }
    per_adapter
        .into_iter()
        .map(|(luid, bytes)| (luid, bytes as u64))
        .collect()
}

// ---------------------------------------------------------------------
// Adapter matching
// ---------------------------------------------------------------------

/// Pair one `Win32_VideoController` row with the DXGI adapter that
/// describes the same card.
///
/// Three strategies are tried, strongest first:
///
/// 1. **PCI vendor and device** parsed from `PNPDeviceID`. Exact.
///    When two identical cards are installed both adapters match, so
///    `ordinal` picks within the matching subset.
/// 2. **Description equality**, case-insensitive, then a containment
///    check. WMI and DXGI usually report byte-identical marketing
///    names, but driver updates occasionally add or drop a suffix.
/// 3. **Ordinal position** in the full adapter list, as a last resort.
///
/// `ordinal` is the caller's index among the WMI rows it is iterating.
///
/// Note that `ordinal` indexes a *vendor-filtered* list (the AMD reader
/// only iterates AMD controllers) while `adapters` is the *unfiltered*
/// DXGI enumeration, so the two are not generally aligned. Every
/// weaker-than-PCI strategy is therefore constrained to adapters whose
/// vendor id agrees with the WMI row, and when the vendor cannot be
/// determined at all the function gives up rather than guessing.
///
/// Returning `None` costs only the augmentation: the caller keeps the
/// WMI baseline, which is honestly empty. Guessing wrong is worse than
/// that, because it silently reports one card's memory and utilization
/// against another.
pub fn match_adapter<'a>(
    adapters: &'a [AdapterIdentity],
    pnp_device_id: Option<&str>,
    name: &str,
    ordinal: usize,
) -> Option<&'a AdapterIdentity> {
    if adapters.is_empty() {
        return None;
    }

    let ids = pnp_device_id.and_then(parse_pnp_device_id);

    // Strongest: exact PCI vendor and device.
    if let Some(ids) = ids {
        let exact: Vec<&AdapterIdentity> = adapters
            .iter()
            .filter(|a| a.vendor_id == ids.vendor && a.device_id == ids.device)
            .collect();
        match exact.len() {
            0 => {}
            1 => return Some(exact[0]),
            // Two identical cards. Their WMI rows and DXGI entries are
            // both vendor-homogeneous here, so the ordinal is meaningful
            // within this subset.
            _ => return Some(exact.get(ordinal).copied().unwrap_or(exact[0])),
        }
    }

    // Everything below may only consider adapters from the same vendor.
    // Without a vendor there is nothing to constrain a guess with.
    let ids = ids?;
    let same_vendor: Vec<&AdapterIdentity> = adapters
        .iter()
        .filter(|a| a.vendor_id == ids.vendor)
        .collect();
    if same_vendor.is_empty() {
        return None;
    }

    let trimmed = name.trim();
    if !trimmed.is_empty() {
        if let Some(hit) = same_vendor
            .iter()
            .find(|a| a.description.trim().eq_ignore_ascii_case(trimmed))
        {
            return Some(hit);
        }
        let lowered = trimmed.to_lowercase();
        if let Some(hit) = same_vendor.iter().find(|a| {
            let description = a.description.trim().to_lowercase();
            // An empty description would make `lowered.contains` true
            // for every row and swallow the whole list, so require a
            // real string on both sides.
            !description.is_empty()
                && (description.contains(&lowered) || lowered.contains(&description))
        }) {
            return Some(hit);
        }
    }

    // Last resort, still inside the vendor subset.
    same_vendor.get(ordinal).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_real_gpu_engine_instance_name() {
        let parsed = parse_gpu_engine_instance(
            "pid_9540_luid_0x00000000_0x0000D3F5_phys_0_eng_3_engtype_3D",
        )
        .expect("well-formed instance name should parse");
        assert_eq!(parsed.pid, 9540);
        assert_eq!(parsed.luid, AdapterLuid::new(0, 0xD3F5));
        assert_eq!(parsed.phys, 0);
        assert_eq!(parsed.eng, 3);
        assert_eq!(parsed.engtype, "3D");
    }

    #[test]
    fn parses_multiword_engine_types() {
        let parsed = parse_gpu_engine_instance(
            "pid_1_luid_0x00000000_0x00001234_phys_0_eng_1_engtype_VideoDecode",
        )
        .unwrap();
        assert_eq!(parsed.engtype, "VideoDecode");
        assert!(!is_utilization_engine_type(&parsed.engtype));
    }

    #[test]
    fn preserves_a_negative_luid_high_part() {
        // HighPart is signed; the counter name renders it as unsigned
        // hex. Round-tripping 0xFFFFFFFF must land on -1, not fail.
        let parsed = parse_gpu_engine_instance(
            "pid_7_luid_0xFFFFFFFF_0x0000ABCD_phys_0_eng_0_engtype_Compute",
        )
        .unwrap();
        assert_eq!(parsed.luid, AdapterLuid::new(-1, 0xABCD));
    }

    #[test]
    fn rejects_malformed_engine_instance_names() {
        // Each of these is a shape we could plausibly be handed by a
        // future driver or a different counter family; none should panic
        // or produce a bogus LUID.
        for bad in [
            "",
            "pid_9540",
            "pid_abc_luid_0x0_0x1_phys_0_eng_0_engtype_3D",
            "luid_0x00000000_0x0000D3F5_phys_0",
            "pid_1_luid_0x0_0x1_phys_0_eng_0_engtype_",
            "pid_1_luid_0xZZ_0x1_phys_0_eng_0_engtype_3D",
        ] {
            assert!(
                parse_gpu_engine_instance(bad).is_none(),
                "expected {bad:?} to be rejected"
            );
        }
    }

    #[test]
    fn parses_process_and_adapter_memory_instances() {
        let process =
            parse_gpu_process_memory_instance("pid_4242_luid_0x00000000_0x0000D3F5_phys_0")
                .unwrap();
        assert_eq!(process.pid, 4242);
        assert_eq!(process.luid, AdapterLuid::new(0, 0xD3F5));

        let adapter =
            parse_gpu_adapter_memory_instance("luid_0x00000000_0x0000D3F5_phys_0").unwrap();
        assert_eq!(adapter.luid, AdapterLuid::new(0, 0xD3F5));
        assert_eq!(adapter.phys, 0);

        // The two families must not accept each other's shape.
        assert!(parse_gpu_adapter_memory_instance("pid_1_luid_0x0_0x1_phys_0").is_none());
        assert!(parse_gpu_process_memory_instance("luid_0x0_0x1_phys_0").is_none());
    }

    #[test]
    fn parses_pnp_device_ids() {
        let ids =
            parse_pnp_device_id(r"PCI\VEN_1002&DEV_744C&SUBSYS_00000000&REV_C8\6&1a2b&0&00000019")
                .unwrap();
        assert_eq!(ids.vendor, 0x1002);
        assert_eq!(ids.device, 0x744C);

        // Lower-case hex and a different vendor.
        let intel = parse_pnp_device_id(r"pci\ven_8086&dev_56a0&subsys_00000000").unwrap();
        assert_eq!(intel.vendor, 0x8086);
        assert_eq!(intel.device, 0x56A0);

        // Non-PCI enumerations have no counterpart to match.
        assert!(parse_pnp_device_id(r"ROOT\BasicDisplay\0000").is_none());
        assert!(parse_pnp_device_id("").is_none());
    }

    fn engine(pid: u32, luid: u32, eng: u32, engtype: &str) -> GpuEngineInstance {
        GpuEngineInstance {
            pid,
            luid: AdapterLuid::new(0, luid),
            phys: 0,
            eng,
            engtype: engtype.to_string(),
        }
    }

    #[test]
    fn sums_processes_within_an_engine_and_maxes_across_engines() {
        // Engine 0 is 30% + 25% busy across two processes; engine 1 is
        // 40%. The adapter figure must be 55 (the busier engine), not 95
        // (the sum of both).
        let samples = vec![
            (engine(100, 0xAAAA, 0, "3D"), 30.0),
            (engine(200, 0xAAAA, 0, "3D"), 25.0),
            (engine(100, 0xAAAA, 1, "Compute"), 40.0),
        ];
        let aggregated = aggregate_engine_utilization(samples);
        assert_eq!(aggregated.len(), 1);
        let value = aggregated[&AdapterLuid::new(0, 0xAAAA)];
        assert!((value - 55.0).abs() < f64::EPSILON, "got {value}");
    }

    #[test]
    fn keeps_adapters_separate_and_ignores_non_compute_engines() {
        let samples = vec![
            (engine(1, 0xAAAA, 0, "3D"), 70.0),
            (engine(1, 0xBBBB, 0, "3D"), 10.0),
            // Video engines must not contribute at all.
            (engine(1, 0xBBBB, 1, "VideoDecode"), 95.0),
            (engine(1, 0xBBBB, 2, "Copy"), 88.0),
        ];
        let aggregated = aggregate_engine_utilization(samples);
        assert_eq!(aggregated[&AdapterLuid::new(0, 0xAAAA)], 70.0);
        assert_eq!(aggregated[&AdapterLuid::new(0, 0xBBBB)], 10.0);
    }

    #[test]
    fn clamps_out_of_range_and_drops_non_finite_samples() {
        let samples = vec![
            (engine(1, 0xAAAA, 0, "3D"), 80.0),
            (engine(2, 0xAAAA, 0, "3D"), 80.0), // sums to 160, clamps to 100
            (engine(1, 0xBBBB, 0, "3D"), -5.0), // negative clamps to 0
            (engine(1, 0xCCCC, 0, "3D"), f64::NAN),
            (engine(1, 0xCCCC, 0, "3D"), f64::INFINITY),
        ];
        let aggregated = aggregate_engine_utilization(samples);
        assert_eq!(aggregated[&AdapterLuid::new(0, 0xAAAA)], 100.0);
        assert_eq!(aggregated[&AdapterLuid::new(0, 0xBBBB)], 0.0);
        // Every sample for CCCC was non-finite, so it contributes no
        // entry rather than a NaN one.
        assert!(!aggregated.contains_key(&AdapterLuid::new(0, 0xCCCC)));
    }

    #[test]
    fn sums_adapter_memory_across_segments() {
        let samples = vec![
            (
                GpuAdapterMemoryInstance {
                    luid: AdapterLuid::new(0, 0xAAAA),
                    phys: 0,
                },
                1024.0,
            ),
            (
                GpuAdapterMemoryInstance {
                    luid: AdapterLuid::new(0, 0xAAAA),
                    phys: 1,
                },
                2048.0,
            ),
        ];
        let aggregated = aggregate_adapter_memory(samples);
        assert_eq!(aggregated[&AdapterLuid::new(0, 0xAAAA)], 3072);
    }

    fn identity(luid: u32, vendor: u32, device: u32, description: &str) -> AdapterIdentity {
        AdapterIdentity {
            luid: AdapterLuid::new(0, luid),
            vendor_id: vendor,
            device_id: device,
            description: description.to_string(),
        }
    }

    #[test]
    fn matches_on_pci_ids_before_anything_else() {
        let adapters = vec![
            identity(1, 0x8086, 0x56A0, "Intel(R) Arc(TM) A770 Graphics"),
            identity(2, 0x1002, 0x744C, "AMD Radeon RX 7900 XTX"),
        ];
        // The name deliberately disagrees with the PNPDeviceID; the PCI
        // ids must win.
        let hit = match_adapter(
            &adapters,
            Some(r"PCI\VEN_1002&DEV_744C&SUBSYS_0&REV_C8"),
            "Intel(R) Arc(TM) A770 Graphics",
            0,
        )
        .unwrap();
        assert_eq!(hit.luid, AdapterLuid::new(0, 2));
    }

    #[test]
    fn disambiguates_identical_cards_by_ordinal() {
        let adapters = vec![
            identity(1, 0x1002, 0x744C, "AMD Radeon RX 7900 XTX"),
            identity(2, 0x1002, 0x744C, "AMD Radeon RX 7900 XTX"),
        ];
        let pnp = Some(r"PCI\VEN_1002&DEV_744C");
        assert_eq!(
            match_adapter(&adapters, pnp, "AMD Radeon RX 7900 XTX", 0)
                .unwrap()
                .luid,
            AdapterLuid::new(0, 1)
        );
        assert_eq!(
            match_adapter(&adapters, pnp, "AMD Radeon RX 7900 XTX", 1)
                .unwrap()
                .luid,
            AdapterLuid::new(0, 2)
        );
    }

    #[test]
    fn falls_back_to_name_within_the_same_vendor() {
        let adapters = vec![
            identity(1, 0x8086, 0x56A0, "Intel(R) Arc(TM) A770 Graphics"),
            // Same vendor as the WMI row below, but a device id the row
            // does not carry, so only the name can pair them.
            identity(2, 0x1002, 0x1234, "AMD Radeon RX 7900 XTX"),
        ];
        let hit = match_adapter(
            &adapters,
            Some(r"PCI\VEN_1002&DEV_744C"),
            "AMD Radeon RX 7900 XTX",
            0,
        )
        .unwrap();
        assert_eq!(hit.luid, AdapterLuid::new(0, 2));
    }

    #[test]
    fn never_pairs_across_vendors() {
        // The heart of the mis-attribution risk: `ordinal` indexes the
        // caller's vendor-filtered WMI rows while `adapters` is the full
        // DXGI list, so an unconstrained ordinal fallback would hand an
        // AMD row the Intel iGPU's memory and utilization. Reporting
        // nothing (and keeping the honest WMI baseline) is required.
        let adapters = vec![
            identity(1, 0x8086, 0x56A0, "Intel(R) Arc(TM) A770 Graphics"),
            identity(2, 0x10DE, 0x2684, "NVIDIA GeForce RTX 4090"),
        ];
        assert!(
            match_adapter(
                &adapters,
                Some(r"PCI\VEN_1002&DEV_744C"),
                "AMD Radeon RX 7900 XTX",
                0
            )
            .is_none()
        );
    }

    #[test]
    fn declines_to_guess_without_a_vendor() {
        let adapters = vec![identity(1, 0x1002, 0x744C, "AMD Radeon RX 7900 XTX")];
        // `amd_windows` synthesizes `AMD-GPU-{idx}` as the uuid when a
        // controller has no PNPDeviceID; that parses to no vendor, so
        // there is nothing to constrain a match with.
        assert!(match_adapter(&adapters, None, "AMD Radeon RX 7900 XTX", 0).is_none());
        assert!(match_adapter(&adapters, Some("AMD-GPU-0"), "AMD Radeon RX 7900 XTX", 0).is_none());
    }

    #[test]
    fn an_empty_adapter_description_does_not_swallow_every_row() {
        // `widestring_to_string` yields "" for a NUL-first Description.
        // A naive containment check treats "" as a substring of
        // everything, so one such adapter would capture every WMI row.
        let adapters = vec![
            identity(1, 0x1002, 0x1111, ""),
            identity(2, 0x1002, 0x2222, "AMD Radeon RX 7900 XTX"),
        ];
        let hit = match_adapter(
            &adapters,
            Some(r"PCI\VEN_1002&DEV_744C"),
            "AMD Radeon RX 7900 XTX",
            9,
        )
        .unwrap();
        assert_eq!(hit.luid, AdapterLuid::new(0, 2));
    }

    #[test]
    fn out_of_range_and_empty_inputs_yield_nothing() {
        let adapters = vec![identity(1, 0x1002, 0x744C, "AMD Radeon RX 7900 XTX")];
        assert!(match_adapter(&adapters, Some(r"PCI\VEN_1002&DEV_9999"), "Unknown", 9).is_none());
        assert!(match_adapter(&[], None, "anything", 0).is_none());
    }
}
