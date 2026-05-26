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

//! Friendly-name lookup for Intel client GPU PCI device IDs.
//!
//! Kept in its own module so [`super::intel_gpu_linux`] stays under the
//! 500-line budget. The table intentionally covers the families called
//! out in issue #244 — Arc A-series (Alchemist), Arc B-series
//! (Battlemage), Iris Xe on Tiger / Alder / Raptor Lake, and the Arc
//! iGPU on Core Ultra / Meteor Lake — plus a generic fallback for IDs we
//! have not catalogued. We deliberately do **not** vendor the full Intel
//! PCI ID database; for the curious, the canonical source is
//! <https://gitlab.freedesktop.org/mesa/mesa/-/blob/main/include/pci_ids/i915_pci_ids.h>
//! and the Linux kernel's `i915_pci.c` / `xe_pci.c`. Unknown IDs render
//! as `Intel Graphics (device 0xXXXX)` so the GPU is still detected and
//! the operator can identify it from the device ID.

/// Map a PCI device ID (low 16 bits) to a friendly marketing string.
///
/// Returns an empty `String` when the ID is not in the curated table —
/// the caller substitutes the generic `Intel Graphics (device 0xXXXX)`
/// fallback. Keeping the "unknown" sentinel out of this function lets
/// the table stay pure-data and easy to extend.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn intel_gpu_marketing_name(device_id: u32) -> String {
    let id = device_id & 0xFFFF;
    match id {
        // ---- Arc A-series "Alchemist" (DG2). Range 0x5690-0x56BF.
        0x5690..=0x5692 => "Intel Arc A770M / A730M / A550M".to_string(),
        0x5693..=0x5695 => "Intel Arc A370M / A350M".to_string(),
        0x56A0 | 0x56A1 => "Intel Arc A770".to_string(),
        0x56A2 => "Intel Arc A750".to_string(),
        0x56A3 | 0x56A4 => "Intel Arc A580".to_string(),
        0x56A5 | 0x56A6 => "Intel Arc A380 / A310".to_string(),
        0x56B0..=0x56B3 => "Intel Arc Pro A-series".to_string(),
        0x56BA..=0x56BD => "Intel Arc A-series (mobile)".to_string(),

        // ---- Arc B-series "Battlemage" (BMG-G21).
        // Public IDs for B570/B580 cluster around 0xE20B-0xE20D.
        0xE202 | 0xE20B | 0xE20C | 0xE20D | 0xE210 | 0xE211 | 0xE212 | 0xE215 | 0xE216 => {
            "Intel Arc B-series (Battlemage)".to_string()
        }

        // ---- Xe-LPG / Arc iGPU on Core Ultra (Meteor Lake). 0x7D40-0x7DFF.
        0x7D40 | 0x7D41 | 0x7D45 | 0x7D55 | 0x7DD5 => {
            "Intel Arc Graphics (Core Ultra / Meteor Lake)".to_string()
        }
        0x7D50 | 0x7D51 | 0x7D60 => "Intel Graphics (Core Ultra / Meteor Lake)".to_string(),

        // ---- Iris Xe / UHD on Tiger Lake (Gen12 LP). 0x9A40-0x9AFF.
        0x9A40 | 0x9A49 | 0x9A60 | 0x9A68 | 0x9A70 | 0x9A78 | 0x9AC0 | 0x9AC9 | 0x9AD9 | 0x9AF8 => {
            "Intel Iris Xe Graphics (Tiger Lake)".to_string()
        }

        // ---- Iris Xe on Alder Lake / Raptor Lake. 0x4680-0x46FF cluster.
        0x4680 | 0x4682 | 0x4688 | 0x468A | 0x468B | 0x4690 | 0x4692 | 0x4693 | 0x46A0 | 0x46A3
        | 0x46A6 | 0x46A8 | 0x46AA | 0x46B0 | 0x46B3 | 0x46C0 | 0x46C3 | 0x46D0 | 0x46D1
        | 0x46D2 | 0x46D3 | 0x46D4 => {
            "Intel UHD / Iris Xe Graphics (Alder/Raptor Lake)".to_string()
        }

        // ---- UHD Graphics on Rocket Lake. 0x4C8x range.
        0x4C8A | 0x4C8B | 0x4C8C | 0x4C90 | 0x4C9A => {
            "Intel UHD Graphics (Rocket Lake)".to_string()
        }

        // ---- Iris Plus / UHD on Ice Lake. 0x8A50 family.
        0x8A50 | 0x8A51 | 0x8A52 | 0x8A53 | 0x8A56 | 0x8A57 | 0x8A58 | 0x8A59 | 0x8A5A | 0x8A5B
        | 0x8A5C | 0x8A5D | 0x8A71 => "Intel Iris Plus / UHD Graphics (Ice Lake)".to_string(),

        // ---- Xe2 / Lunar Lake / Arrow Lake (Gen13/14 IDs in 0xA7* range).
        0xA780 | 0xA781 | 0xA782 | 0xA783 | 0xA788 | 0xA789 | 0xA78A | 0xA78B | 0xA7A0 | 0xA7A1
        | 0xA7A8 | 0xA7A9 | 0xA7AA | 0xA7AB | 0xA7AC | 0xA7AD => {
            "Intel Graphics (Arrow/Lunar Lake)".to_string()
        }

        _ => String::new(),
    }
}

/// Compose a final marketing string, falling back to the generic
/// "Intel Graphics (device 0xXXXX)" form when the table has no entry.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn resolve_intel_gpu_name(device_id: u32) -> String {
    let curated = intel_gpu_marketing_name(device_id);
    if curated.is_empty() {
        format!("Intel Graphics (device {:#06x})", device_id & 0xFFFF)
    } else {
        curated
    }
}

// ---------------------------------------------------------------------
// Architecture classification (consumed by both intel_gpu_linux and
// intel_gpu_windows readers, and re-exported for downstream consumers).
// ---------------------------------------------------------------------

/// Intel client GPU architecture family, derived from the marketing name.
///
/// Used by downstream consumers (e.g. an accelerator-selection layer that
/// chooses between SYCL/oneAPI and CPU inference backends) to avoid
/// re-implementing the same name-pattern table. The classification
/// mirrors the `INTEL_GPU_PATTERNS` table in lablup/backend.ai-go's
/// `src-tauri/src/engine/gpu.rs` so the two projects stay in agreement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntelArchitecture {
    /// Arc A-series discrete (A310/A380/A580/A750/A770) — Alchemist (Xe-HPG).
    Alchemist,
    /// Arc B-series discrete (e.g. B580) — Battlemage (Xe2).
    Battlemage,
    /// Xe-LPG integrated (Meteor Lake / Core Ultra Series 1).
    XeLpg,
    /// Xe-LPG+ integrated (Lunar Lake / Core Ultra Series 2 / Arc 140V/130V).
    XeLpgPlus,
    /// Xe (Iris Xe on Tiger / Alder / Raptor Lake integrated graphics).
    IrisXe,
    /// Older integrated graphics — HD Graphics / UHD Graphics on pre-Xe
    /// parts. Not SYCL-capable.
    OlderIntegrated,
    /// Could not be classified from the name.
    Unknown,
}

impl IntelArchitecture {
    /// Returns `true` when this architecture is expected to support SYCL /
    /// oneAPI compute. Mirrors lablup/backend.ai-go's
    /// `check_intel_sycl_support`.
    pub fn is_sycl_capable(self) -> bool {
        matches!(
            self,
            Self::Alchemist | Self::Battlemage | Self::XeLpg | Self::XeLpgPlus | Self::IrisXe,
        )
    }

    /// Short human-readable label suitable for a `detail` map entry.
    pub fn label(self) -> &'static str {
        match self {
            Self::Alchemist => "Alchemist (Xe-HPG, A-series)",
            Self::Battlemage => "Battlemage (Xe2, B-series)",
            Self::XeLpg => "Xe-LPG (Meteor Lake)",
            Self::XeLpgPlus => "Xe-LPG+ (Lunar Lake)",
            Self::IrisXe => "Iris Xe (Tiger/Alder/Raptor Lake)",
            Self::OlderIntegrated => "Pre-Xe (HD/UHD Graphics)",
            Self::Unknown => "Unknown",
        }
    }

    /// Render the SYCL-capability decision for the `detail["SYCL Capable"]`
    /// map entry. Unlike a bare `is_sycl_capable()` boolean, this returns
    /// `"Unknown"` for the [`Unknown`](Self::Unknown) variant so consumers
    /// can distinguish "we know this GPU is not SYCL-capable" from "we
    /// couldn't classify this GPU at all".
    pub fn sycl_capable_label(self) -> &'static str {
        match self {
            Self::Unknown => "Unknown",
            _ if self.is_sycl_capable() => "Yes",
            _ => "No",
        }
    }
}

/// Classify the architecture of an Intel client GPU from its marketing
/// name.
///
/// The matcher is pure-Rust string analysis — no regex, no allocations
/// beyond the single lowercase copy of the input. Pattern order is
/// load-bearing:
///
/// 1. **Older integrated first** so a `HD Graphics 520` style name never
///    accidentally matches a later Xe-LPG / Iris Xe rule.
/// 2. **Battlemage before Alchemist** because `Intel Arc B580` contains
///    the substring `arc` but is not Alchemist.
/// 3. **Alchemist before Lunar Lake** for the same reason — Alchemist
///    names contain a specific `a3`/`a5`/`a7` token, Lunar Lake's Arc
///    140V/130V names do not.
/// 4. **Lunar Lake before generic Xe-LPG** because Lunar Lake is a
///    distinct architecture and we want it labelled `XeLpgPlus`, not the
///    Meteor Lake `XeLpg`.
/// 5. **Generic Xe-LPG before Iris Xe** so Core Ultra (Meteor Lake) iGPU
///    names — sold as `Intel Arc Graphics` with no model number — land in
///    `XeLpg`, not in `IrisXe` or `Unknown`.
///
/// The trickiest disambiguation is the trio
/// `Intel Arc Graphics` (Meteor Lake iGPU, → `XeLpg`),
/// `Intel Arc A770 Graphics` (discrete Alchemist, → `Alchemist`), and
/// `Intel Arc 140V Graphics` (Lunar Lake iGPU, → `XeLpgPlus`). The
/// substring `a3`/`a5`/`a7` is the single token that distinguishes
/// Alchemist from the two integrated iGPU variants — `140v` contains an
/// `a` but no `a3`/`a5`/`a7`, so it falls through to the Lunar Lake rule.
pub fn classify_intel_architecture(name: &str) -> IntelArchitecture {
    let n = name.to_lowercase();

    // 1. Older integrated FIRST. These names contain `hd graphics` or
    //    `uhd graphics` and NO modern architecture token. The guards
    //    against `arc`/`iris`/`xe` are belt-and-braces — current Intel
    //    naming conventions never mix the two, but if a future SKU were
    //    named "HD Graphics Xe Edition" we want the modern token to win.
    if (n.contains("hd graphics") || n.contains("uhd graphics"))
        && !n.contains("iris")
        && !n.contains("arc")
        && !n.contains("xe")
    {
        return IntelArchitecture::OlderIntegrated;
    }

    // 2. Battlemage — explicit family name, or Arc + a known B-series SKU.
    if n.contains("battlemage")
        || (n.contains("arc") && (n.contains("b580") || n.contains("b570") || n.contains("b380")))
    {
        return IntelArchitecture::Battlemage;
    }

    // 3. Alchemist (Arc A-series discrete). Arc + one of the A-series
    //    family tokens. A3/A5/A7 are the three product tiers (Pro / Mid /
    //    High); A1/A2/A4/A6 are not real SKUs.
    if n.contains("arc") && (n.contains("a3") || n.contains("a5") || n.contains("a7")) {
        return IntelArchitecture::Alchemist;
    }

    // 4. Lunar Lake (Xe-LPG+). Either the explicit family name, or the
    //    Arc 140V / 130V iGPU on Core Ultra Series 2.
    if n.contains("lunarlake")
        || n.contains("lunar lake")
        || (n.contains("arc") && (n.contains("140v") || n.contains("130v")))
    {
        return IntelArchitecture::XeLpgPlus;
    }

    // 5. Xe-LPG (Meteor Lake). Either the explicit `xe-lpg`/`xe lpg`
    //    family name, or any other Arc iGPU — by this point Alchemist and
    //    Lunar Lake have been ruled out, so a residual `arc` + `graphics`
    //    name (notably `Intel Arc Graphics`) is the Meteor Lake iGPU.
    if (n.contains("xe") && n.contains("lpg")) || (n.contains("arc") && n.contains("graphics")) {
        return IntelArchitecture::XeLpg;
    }

    // 6. Iris Xe (Tiger / Alder / Raptor Lake integrated).
    if n.contains("iris") && n.contains("xe") {
        return IntelArchitecture::IrisXe;
    }

    IntelArchitecture::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_families_resolve() {
        assert!(intel_gpu_marketing_name(0x56A0).contains("Arc A770"));
        assert!(intel_gpu_marketing_name(0x56A2).contains("Arc A750"));
        assert!(intel_gpu_marketing_name(0xE20B).contains("Battlemage"));
        assert!(intel_gpu_marketing_name(0x7D40).contains("Meteor Lake"));
        assert!(intel_gpu_marketing_name(0x9A49).contains("Tiger Lake"));
        assert!(intel_gpu_marketing_name(0x46A6).contains("Alder/Raptor Lake"));
        assert!(intel_gpu_marketing_name(0x4C8A).contains("Rocket Lake"));
        assert!(intel_gpu_marketing_name(0x8A50).contains("Ice Lake"));
        assert!(intel_gpu_marketing_name(0xA780).contains("Arrow/Lunar Lake"));
    }

    #[test]
    fn unknown_falls_back_to_generic() {
        let n = resolve_intel_gpu_name(0x1234);
        assert!(n.starts_with("Intel Graphics (device"));
        assert!(n.contains("0x1234"));
    }

    #[test]
    fn high_bits_ignored() {
        // Some lspci output reports IDs with the upper 16 bits set;
        // we mask to the device portion before matching.
        assert!(resolve_intel_gpu_name(0x0000_56A0).contains("Arc A770"));
        assert!(resolve_intel_gpu_name(0xFFFF_56A0).contains("Arc A770"));
    }

    // ---------- Architecture classification tests ----------
    //
    // The fixtures below mirror lablup/backend.ai-go's `INTEL_GPU_PATTERNS`
    // and `check_intel_sycl_support` tests so the two projects stay in
    // agreement about what each marketing name means.

    #[test]
    fn classifies_arc_a_series_as_alchemist() {
        for name in &[
            "Intel Arc A770 Graphics",
            "Intel Arc A750",
            "Intel Arc A580",
            "Intel Arc A380",
            "Intel Arc A310",
            "Intel(R) Arc(TM) A770 Graphics",
        ] {
            assert_eq!(
                classify_intel_architecture(name),
                IntelArchitecture::Alchemist,
                "mis-classified: {name}"
            );
            assert!(IntelArchitecture::Alchemist.is_sycl_capable());
        }
    }

    #[test]
    fn classifies_battlemage_b_series() {
        for name in &[
            "Intel Battlemage Graphics",
            "Intel(R) Battlemage(TM) Graphics",
            "Intel Arc B580",
            "Intel(R) Arc(TM) B580 Graphics",
        ] {
            assert_eq!(
                classify_intel_architecture(name),
                IntelArchitecture::Battlemage,
                "mis-classified: {name}"
            );
            assert!(IntelArchitecture::Battlemage.is_sycl_capable());
        }
    }

    #[test]
    fn classifies_core_ultra_integrated_arc_as_xe_lpg() {
        // Arc integrated graphics on Core Ultra (Meteor Lake, no A-series
        // model number) is Xe-LPG, not Alchemist.
        assert_eq!(
            classify_intel_architecture("Intel Arc Graphics"),
            IntelArchitecture::XeLpg,
        );
        assert_eq!(
            classify_intel_architecture("Intel(R) Arc(TM) Graphics"),
            IntelArchitecture::XeLpg,
        );
        assert!(IntelArchitecture::XeLpg.is_sycl_capable());
    }

    #[test]
    fn classifies_lunar_lake_arc_140v() {
        // Arc 140V / 130V on Lunar Lake — should map to XeLpgPlus, not
        // Alchemist. "140V" contains "a" in "140V Graphics" but no A3/A5/A7
        // token, so the Alchemist matcher must not fire.
        let result = classify_intel_architecture("Intel Arc 140V Graphics");
        assert!(
            matches!(
                result,
                IntelArchitecture::XeLpgPlus | IntelArchitecture::XeLpg
            ),
            "Arc 140V should classify as a Lunar Lake / Xe-LPG-family part, got {result:?}",
        );
        assert!(result.is_sycl_capable());

        // Lunar Lake's other iGPU SKU.
        let result_130v = classify_intel_architecture("Intel Arc 130V Graphics");
        assert!(
            matches!(
                result_130v,
                IntelArchitecture::XeLpgPlus | IntelArchitecture::XeLpg
            ),
            "Arc 130V should classify as a Lunar Lake / Xe-LPG-family part, got {result_130v:?}",
        );
    }

    #[test]
    fn classifies_iris_xe_as_iris_xe() {
        for name in &["Intel Iris Xe Graphics", "Intel(R) Iris(R) Xe Graphics"] {
            assert_eq!(
                classify_intel_architecture(name),
                IntelArchitecture::IrisXe,
                "mis-classified: {name}"
            );
            assert!(IntelArchitecture::IrisXe.is_sycl_capable());
        }
    }

    #[test]
    fn classifies_xe_lpg_meteor_lake() {
        assert_eq!(
            classify_intel_architecture("Intel Xe-LPG Graphics"),
            IntelArchitecture::XeLpg,
        );
    }

    #[test]
    fn classifies_lunar_lake_explicit() {
        for name in &[
            "Intel LunarLake Graphics",
            "Intel(R) LunarLake(TM) Graphics",
            "Intel Lunar Lake Graphics",
        ] {
            assert_eq!(
                classify_intel_architecture(name),
                IntelArchitecture::XeLpgPlus,
                "mis-classified: {name}"
            );
            assert!(IntelArchitecture::XeLpgPlus.is_sycl_capable());
        }
    }

    #[test]
    fn older_integrated_is_not_sycl_capable() {
        for name in &[
            "Intel HD Graphics 630",
            "Intel UHD Graphics 770",
            "Intel HD Graphics 520",
            "Intel UHD Graphics 620",
        ] {
            let arch = classify_intel_architecture(name);
            assert_eq!(
                arch,
                IntelArchitecture::OlderIntegrated,
                "mis-classified: {name}"
            );
            assert!(!arch.is_sycl_capable(), "{name} should not be SYCL capable");
        }
    }

    #[test]
    fn unknown_names_classified_as_unknown() {
        let arch = classify_intel_architecture("Definitely Not An Intel GPU");
        assert_eq!(arch, IntelArchitecture::Unknown);
        assert!(!arch.is_sycl_capable());

        // An empty name is also unknown.
        assert_eq!(classify_intel_architecture(""), IntelArchitecture::Unknown);
    }

    #[test]
    fn architecture_labels_are_stable() {
        // Lock in the label strings so downstream consumers (which embed
        // them in `detail["Architecture"]`) can rely on them.
        assert_eq!(
            IntelArchitecture::Alchemist.label(),
            "Alchemist (Xe-HPG, A-series)"
        );
        assert_eq!(
            IntelArchitecture::Battlemage.label(),
            "Battlemage (Xe2, B-series)"
        );
        assert_eq!(IntelArchitecture::XeLpg.label(), "Xe-LPG (Meteor Lake)");
        assert_eq!(IntelArchitecture::XeLpgPlus.label(), "Xe-LPG+ (Lunar Lake)");
        assert_eq!(
            IntelArchitecture::IrisXe.label(),
            "Iris Xe (Tiger/Alder/Raptor Lake)"
        );
        assert_eq!(
            IntelArchitecture::OlderIntegrated.label(),
            "Pre-Xe (HD/UHD Graphics)"
        );
        assert_eq!(IntelArchitecture::Unknown.label(), "Unknown");
    }

    #[test]
    fn sycl_capability_matches_backend_ai_go() {
        // The five SYCL-capable architectures, mirrored from
        // lablup/backend.ai-go's check_intel_sycl_support.
        assert!(IntelArchitecture::Alchemist.is_sycl_capable());
        assert!(IntelArchitecture::Battlemage.is_sycl_capable());
        assert!(IntelArchitecture::XeLpg.is_sycl_capable());
        assert!(IntelArchitecture::XeLpgPlus.is_sycl_capable());
        assert!(IntelArchitecture::IrisXe.is_sycl_capable());
        assert!(!IntelArchitecture::OlderIntegrated.is_sycl_capable());
        assert!(!IntelArchitecture::Unknown.is_sycl_capable());
    }

    #[test]
    fn sycl_capable_label_distinguishes_unknown_from_no() {
        // The map-entry label must not collapse Unknown into "No" —
        // downstream consumers need to know whether the GPU is *known*
        // not to be SYCL-capable vs. unrecognised.
        assert_eq!(IntelArchitecture::Alchemist.sycl_capable_label(), "Yes");
        assert_eq!(IntelArchitecture::Battlemage.sycl_capable_label(), "Yes");
        assert_eq!(IntelArchitecture::XeLpg.sycl_capable_label(), "Yes");
        assert_eq!(IntelArchitecture::XeLpgPlus.sycl_capable_label(), "Yes");
        assert_eq!(IntelArchitecture::IrisXe.sycl_capable_label(), "Yes");
        assert_eq!(
            IntelArchitecture::OlderIntegrated.sycl_capable_label(),
            "No"
        );
        assert_eq!(IntelArchitecture::Unknown.sycl_capable_label(), "Unknown");
    }
}
