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
}
