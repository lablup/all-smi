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

        // ---- Xe3 / Panther Lake (Core Ultra Series 3). 0xB08x range.
        // The reporting host for issue #364 is `8086:b080`. The range is
        // deliberate rather than a single ID: Intel ships an iGPU family
        // across a contiguous block of device IDs per generation, and the
        // alternative to a range is reporting `Intel Graphics (device
        // 0xb081)` for the next SKU off the same silicon. The name is
        // generic for the same reason, since one ID does not distinguish
        // the B390 from its siblings; `classify_intel_architecture` reads
        // the marketing string for that.
        0xB080..=0xB08F => "Intel Arc Graphics (Core Ultra / Panther Lake)".to_string(),

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
    /// Xe3 integrated (Panther Lake / Core Ultra Series 3 / Arc B390).
    ///
    /// The first Intel iGPU generation sold under a discrete-looking model
    /// number, which is what broke the naming assumptions this module used
    /// to rely on (issue #364).
    Xe3,
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
            Self::Alchemist
                | Self::Battlemage
                | Self::XeLpg
                | Self::XeLpgPlus
                | Self::Xe3
                | Self::IrisXe,
        )
    }

    /// Short human-readable label suitable for a `detail` map entry.
    pub fn label(self) -> &'static str {
        match self {
            Self::Alchemist => "Alchemist (Xe-HPG, A-series)",
            Self::Battlemage => "Battlemage (Xe2, B-series)",
            Self::XeLpg => "Xe-LPG (Meteor Lake)",
            Self::XeLpgPlus => "Xe-LPG+ (Lunar Lake)",
            Self::Xe3 => "Xe3 (Panther Lake)",
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

    // 5. Xe3 (Panther Lake). Either an explicit family token, or Arc plus
    //    a known Xe3 iGPU model number. Panther Lake is the first Intel
    //    iGPU generation sold under a model number, so it has to be named
    //    here: it is neither ruled in by the Battlemage SKU list above nor
    //    ruled out by the residual-iGPU rule below, and without this arm
    //    it silently reported as Meteor Lake (issue #364).
    if n.contains("panther lake")
        || n.contains("pantherlake")
        || n.contains("xe3")
        || (n.contains("arc") && XE3_INTEGRATED_MODELS.iter().any(|m| n.contains(m)))
    {
        return IntelArchitecture::Xe3;
    }

    // 6. Xe-LPG (Meteor Lake). Either the explicit `xe-lpg`/`xe lpg`
    //    family name, or a residual unnumbered Arc iGPU. The unnumbered
    //    part is load-bearing: `Intel Arc Graphics` with no model number
    //    is the Meteor Lake iGPU, while every numbered Arc name has been
    //    resolved by one of the arms above or is a part this table does
    //    not know yet. Claiming Meteor Lake for an unknown numbered SKU is
    //    what produced the B390 misreport, so numbered names fall through
    //    to `Unknown` instead.
    if n.contains("xe") && n.contains("lpg") {
        return IntelArchitecture::XeLpg;
    }
    if n.contains("arc") && n.contains("graphics") && !contains_arc_model_number(&n) {
        return IntelArchitecture::XeLpg;
    }

    // 7. Iris Xe (Tiger / Alder / Raptor Lake integrated).
    if n.contains("iris") && n.contains("xe") {
        return IntelArchitecture::IrisXe;
    }

    IntelArchitecture::Unknown
}

// ---------------------------------------------------------------------
// Discrete / integrated classification.
//
// Lives here rather than in `intel_gpu_windows` for two reasons. It is
// pure string matching, like everything else in this module. And
// `intel_gpu_windows` is `cfg(target_os = "windows")`, which means no
// runner this project has ever compiles it, so the rule that produced the
// B390 misreport was unreachable by every test job (issue #364, #368).
// ---------------------------------------------------------------------

/// Arc model numbers that belong to discrete cards.
///
/// An explicit table, not a pattern. Until Panther Lake, "an Arc name with
/// a model number" was a sound proxy for "discrete", and the code relied on
/// it. That proxy is dead: the integrated B390 and the discrete B380 differ
/// by one digit, so no general rule over the name can separate them.
///
/// The entries are the SKUs this project already claimed as discrete before
/// issue #364, kept exactly as they were so nothing regresses.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
const DISCRETE_ARC_MODELS: &[&str] = &[
    // Alchemist (Xe-HPG) desktop
    "a310", "a380", "a580", "a750", "a770", // Battlemage (Xe2) desktop
    "b380", "b570", "b580",
];

/// Arc model numbers that belong to integrated parts.
///
/// The counterpart to [`DISCRETE_ARC_MODELS`]: numbered names that are
/// iGPUs. Lunar Lake's `V` suffix used to escape the old heuristic by
/// accident (`140v` is digits-then-letter, which failed its digits-only
/// test); it is named explicitly here so the escape is deliberate.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
const INTEGRATED_ARC_MODELS: &[&str] = &["130v", "140v", "b390"];

/// Panther Lake iGPU model numbers.
///
/// A subset of [`INTEGRATED_ARC_MODELS`]; kept separate because it also
/// drives the architecture arm, where Lunar Lake's parts must not land.
const XE3_INTEGRATED_MODELS: &[&str] = &["b390"];

/// `true` when the lowercased name carries an Arc-style model number,
/// whether or not this module knows which part it is.
///
/// A model number is a single letter in `a`..=`d` followed by three or more
/// digits (`a770`, `b580`, `b390`), or three digits followed by `v`
/// (`140v`). Used to tell "unnumbered, therefore the Meteor Lake iGPU" from
/// "numbered, therefore a part that must be named explicitly".
fn contains_arc_model_number(lower: &str) -> bool {
    lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(is_arc_model_token)
}

/// `true` for one token that looks like an Arc model number.
fn is_arc_model_token(token: &str) -> bool {
    let bytes = token.as_bytes();
    if bytes.len() < 4 {
        return false;
    }
    // `a770`, `b580`, `b390`: letter then digits.
    let letter_first =
        matches!(bytes[0], b'a' | b'b' | b'c' | b'd') && bytes[1..].iter().all(u8::is_ascii_digit);
    // `140v`, `130v`: digits then the Lunar Lake suffix.
    let suffix_last =
        bytes[bytes.len() - 1] == b'v' && bytes[..bytes.len() - 1].iter().all(u8::is_ascii_digit);
    letter_first || suffix_last
}

/// Whether an Intel GPU marketing name denotes a discrete card.
///
/// `None` means "this name carries a model number this table does not
/// know". That is a deliberate third answer rather than a guess: the caller
/// leaves the field to the authoritative source (the DXGI memory layout)
/// instead of asserting a variant it cannot support. Guessing here is
/// exactly what reported the integrated B390 as `Discrete`.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub fn classify_intel_variant(name: &str) -> Option<&'static str> {
    let lower = name.to_lowercase();
    if !lower.contains("arc") {
        // Iris / UHD / HD / Xe Graphics are always integrated.
        return Some("Integrated");
    }
    if DISCRETE_ARC_MODELS.iter().any(|m| lower.contains(m)) {
        return Some("Discrete");
    }
    if INTEGRATED_ARC_MODELS.iter().any(|m| lower.contains(m)) {
        return Some("Integrated");
    }
    if contains_arc_model_number(&lower) {
        // A numbered Arc part that predates this table's knowledge. Since
        // Panther Lake, a number no longer implies discrete.
        return None;
    }
    // Unnumbered Arc, e.g. `Intel(R) Arc(TM) Graphics` on Meteor Lake.
    Some("Integrated")
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------- issue #364: Intel Arc B390 (Xe3 / Panther Lake) ----------

    /// The reported defect: an integrated Xe3 part with a model number was
    /// called `Discrete` because "Arc name with a model number" was taken
    /// as a proxy for discrete.
    #[test]
    fn b390_is_integrated_not_discrete() {
        assert_eq!(
            classify_intel_variant("Intel(R) Arc(TM) B390 Graphics"),
            Some("Integrated")
        );
    }

    /// The second half of the same defect: with no Xe3 variant the B390
    /// fell through to the residual-iGPU rule and reported as Meteor Lake.
    #[test]
    fn b390_classifies_as_xe3_not_meteor_lake() {
        let arch = classify_intel_architecture("Intel(R) Arc(TM) B390 Graphics");
        assert_eq!(arch, IntelArchitecture::Xe3);
        assert_eq!(arch.label(), "Xe3 (Panther Lake)");
        assert!(arch.is_sycl_capable());
    }

    /// B390 (integrated Xe3) and B380 (discrete Battlemage) differ by one
    /// digit. This is the case that makes an explicit table necessary and
    /// any general name pattern wrong.
    #[test]
    fn b380_and_b390_do_not_collide() {
        assert_eq!(
            classify_intel_variant("Intel(R) Arc(TM) B380 Graphics"),
            Some("Discrete")
        );
        assert_eq!(
            classify_intel_architecture("Intel(R) Arc(TM) B380 Graphics"),
            IntelArchitecture::Battlemage
        );
        assert_eq!(
            classify_intel_variant("Intel(R) Arc(TM) B390 Graphics"),
            Some("Integrated")
        );
        assert_eq!(
            classify_intel_architecture("Intel(R) Arc(TM) B390 Graphics"),
            IntelArchitecture::Xe3
        );
    }

    /// An Arc name carrying a model number this table does not know gets
    /// no variant at all, so the caller can defer to DXGI. Guessing
    /// `Discrete` here is the original bug.
    #[test]
    fn unknown_numbered_arc_defers_instead_of_guessing() {
        assert_eq!(
            classify_intel_variant("Intel(R) Arc(TM) C990 Graphics"),
            None
        );
        assert_eq!(
            classify_intel_architecture("Intel(R) Arc(TM) C990 Graphics"),
            IntelArchitecture::Unknown
        );
    }

    // ---------- regression guards for the pre-#364 behaviour ----------

    #[test]
    fn known_discrete_arc_skus_stay_discrete() {
        for name in [
            "Intel(R) Arc(TM) A770 Graphics",
            "Intel(R) Arc(TM) A750 Graphics",
            "Intel(R) Arc(TM) A580 Graphics",
            "Intel(R) Arc(TM) A380 Graphics",
            "Intel(R) Arc(TM) A310 Graphics",
            "Intel(R) Arc(TM) B580 Graphics",
            "Intel(R) Arc(TM) B570 Graphics",
        ] {
            assert_eq!(
                classify_intel_variant(name),
                Some("Discrete"),
                "{name} must stay discrete"
            );
        }
    }

    #[test]
    fn integrated_families_stay_integrated() {
        for name in [
            "Intel(R) Iris(R) Xe Graphics",
            "Intel(R) UHD Graphics 770",
            "Intel(R) HD Graphics 620",
            // Unnumbered Arc on Meteor Lake.
            "Intel(R) Arc(TM) Graphics",
            // Lunar Lake's numbered iGPUs.
            "Intel(R) Arc(TM) 140V GPU",
            "Intel(R) Arc(TM) 130V GPU",
        ] {
            assert_eq!(
                classify_intel_variant(name),
                Some("Integrated"),
                "{name} must stay integrated"
            );
        }
    }

    /// The Meteor Lake residual rule still has to fire for the unnumbered
    /// name it was written for, and Lunar Lake must not be swallowed by
    /// the new Xe3 arm.
    #[test]
    fn neighbouring_architectures_do_not_regress() {
        assert_eq!(
            classify_intel_architecture("Intel(R) Arc(TM) Graphics"),
            IntelArchitecture::XeLpg
        );
        assert_eq!(
            classify_intel_architecture("Intel(R) Arc(TM) 140V GPU"),
            IntelArchitecture::XeLpgPlus
        );
        assert_eq!(
            classify_intel_architecture("Intel(R) Arc(TM) A770 Graphics"),
            IntelArchitecture::Alchemist
        );
        assert_eq!(
            classify_intel_architecture("Intel(R) Arc(TM) B580 Graphics"),
            IntelArchitecture::Battlemage
        );
    }

    #[test]
    fn arc_model_number_detection() {
        assert!(contains_arc_model_number("intel(r) arc(tm) a770 graphics"));
        assert!(contains_arc_model_number("intel(r) arc(tm) b390 graphics"));
        assert!(contains_arc_model_number("intel(r) arc(tm) 140v gpu"));
        assert!(!contains_arc_model_number("intel(r) arc(tm) graphics"));
        assert!(!contains_arc_model_number("intel(r) iris(r) xe graphics"));
        // A single letter followed by fewer than three digits is not a model.
        assert!(!contains_arc_model_number("a77"));
    }

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

    // ---------- device-ID resolution for Panther Lake ----------

    /// The Linux reader has no marketing string to read: it resolves the
    /// name from the PCI device ID in sysfs. Without a table entry, the
    /// reporting host's `8086:b080` produced `Intel Graphics (device
    /// 0xb080)`, which then classified as `Unknown` because every
    /// architecture rule keys off the name.
    #[test]
    fn panther_lake_device_ids_resolve_to_a_named_part() {
        for id in [0xB080u32, 0xB084, 0xB08F] {
            assert_eq!(
                intel_gpu_marketing_name(id),
                "Intel Arc Graphics (Core Ultra / Panther Lake)",
                "device {id:#06x}"
            );
        }
    }

    /// The vendor half of a full PCI ID must not disturb the lookup.
    #[test]
    fn the_vendor_half_of_a_pci_id_is_ignored() {
        assert_eq!(
            resolve_intel_gpu_name(0x8086_B080),
            "Intel Arc Graphics (Core Ultra / Panther Lake)"
        );
    }

    /// The range must stay inside its block. `0xB07F` and `0xB090` belong
    /// to whatever Intel ships next, and claiming them would repeat the
    /// mistake that put the B390 on Meteor Lake.
    #[test]
    fn the_panther_lake_range_does_not_bleed() {
        assert!(intel_gpu_marketing_name(0xB07F).is_empty());
        assert!(intel_gpu_marketing_name(0xB090).is_empty());
        assert_eq!(
            resolve_intel_gpu_name(0xB090),
            "Intel Graphics (device 0xb090)"
        );
    }

    /// A resolved Panther Lake name must survive the round trip into the
    /// architecture classifier, which is what the readers actually publish.
    #[test]
    fn a_resolved_panther_lake_name_classifies_as_xe3() {
        let name = resolve_intel_gpu_name(0xB080);
        assert_eq!(classify_intel_architecture(&name), IntelArchitecture::Xe3);
        assert_eq!(classify_intel_variant(&name), Some("Integrated"));
    }
}
