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

//! Unit tests for the Intel client GPU reader on Windows. Pulled out of
//! `intel_gpu_windows.rs` to keep that file under the 500-line budget,
//! mirroring the `intel_gpu_linux/tests.rs` split.

use super::*;

#[test]
fn intel_arc_a770_recognised() {
    assert!(is_intel_gpu_name("Intel(R) Arc(TM) A770 Graphics"));
}

#[test]
fn intel_arc_b580_recognised() {
    assert!(is_intel_gpu_name("Intel(R) Arc(TM) B580 Graphics"));
}

#[test]
fn intel_iris_xe_recognised() {
    assert!(is_intel_gpu_name("Intel(R) Iris(R) Xe Graphics"));
}

#[test]
fn intel_uhd_770_recognised() {
    assert!(is_intel_gpu_name("Intel(R) UHD Graphics 770"));
}

#[test]
fn intel_hd_graphics_recognised() {
    assert!(is_intel_gpu_name("Intel(R) HD Graphics 530"));
}

#[test]
fn meteor_lake_arc_igpu_recognised() {
    // Meteor Lake / Core Ultra iGPU ships as "Intel(R) Arc(TM)
    // Graphics" with no number.
    assert!(is_intel_gpu_name("Intel(R) Arc(TM) Graphics"));
}

#[test]
fn intel_display_audio_excluded() {
    // Audio device — must NOT match even though "Intel" is in the name.
    assert!(!is_intel_gpu_name("Intel(R) Display Audio"));
}

#[test]
fn intel_management_engine_excluded() {
    assert!(!is_intel_gpu_name(
        "Intel(R) Management Engine Interface #1"
    ));
}

#[test]
fn intel_smart_sound_excluded() {
    assert!(!is_intel_gpu_name(
        "Intel(R) Smart Sound Technology (Intel(R) SST)"
    ));
}

#[test]
fn non_intel_excluded() {
    assert!(!is_intel_gpu_name("NVIDIA GeForce RTX 4090"));
    assert!(!is_intel_gpu_name("AMD Radeon RX 7900 XTX"));
}

#[test]
fn classify_arc_discrete() {
    assert_eq!(
        classify_intel_variant("Intel(R) Arc(TM) A770 Graphics"),
        "Discrete"
    );
    assert_eq!(
        classify_intel_variant("Intel(R) Arc(TM) B580 Graphics"),
        "Discrete"
    );
}

#[test]
fn classify_iris_integrated() {
    assert_eq!(
        classify_intel_variant("Intel(R) Iris(R) Xe Graphics"),
        "Integrated"
    );
}

#[test]
fn classify_uhd_integrated() {
    assert_eq!(
        classify_intel_variant("Intel(R) UHD Graphics 770"),
        "Integrated"
    );
}

#[test]
fn classify_meteor_lake_arc_igpu_as_integrated() {
    // "Intel Arc Graphics" without a model number on Core Ultra is
    // the iGPU and must NOT be classified as Discrete.
    assert_eq!(
        classify_intel_variant("Intel(R) Arc(TM) Graphics"),
        "Integrated"
    );
}

#[test]
fn arc_model_token_recognises_known_skus() {
    assert!(is_arc_model_token("a770"));
    assert!(is_arc_model_token("a750"));
    assert!(is_arc_model_token("a580"));
    assert!(is_arc_model_token("a380"));
    assert!(is_arc_model_token("b580"));
    assert!(is_arc_model_token("b570"));
}

#[test]
fn arc_model_token_rejects_non_models() {
    assert!(!is_arc_model_token("arc"));
    assert!(!is_arc_model_token("tm"));
    assert!(!is_arc_model_token("graphics"));
    assert!(!is_arc_model_token("a"));
    // Single letter followed by <3 digits doesn't count as a model.
    assert!(!is_arc_model_token("a77"));
}

// ---------- Marketing-name fixture parity with backend.ai-go ----------
//
// Every name the architecture classifier handles must also pass the
// WMI name filter, otherwise the reader would drop the GPU before it
// ever gets classified. These tests pin that contract.

#[test]
fn arc_a_series_fixtures_all_pass_filter() {
    for name in &[
        "Intel Arc A770 Graphics",
        "Intel Arc A750",
        "Intel Arc A580",
        "Intel Arc A380",
        "Intel Arc A310",
        "Intel(R) Arc(TM) A770 Graphics",
    ] {
        assert!(is_intel_gpu_name(name), "filter dropped: {name}");
    }
}

#[test]
fn arc_b_series_fixtures_all_pass_filter() {
    for name in &[
        "Intel Arc B580",
        "Intel(R) Arc(TM) B580 Graphics",
        "Intel Arc B570",
    ] {
        assert!(is_intel_gpu_name(name), "filter dropped: {name}");
    }
}

#[test]
fn arc_lunar_lake_fixtures_pass_filter() {
    for name in &[
        "Intel Arc 140V Graphics",
        "Intel Arc 130V Graphics",
        "Intel LunarLake Graphics",
        "Intel(R) LunarLake(TM) Graphics",
        "Intel Lunar Lake Graphics",
    ] {
        assert!(is_intel_gpu_name(name), "filter dropped: {name}");
    }
}

#[test]
fn battlemage_explicit_fixtures_pass_filter() {
    for name in &[
        "Intel Battlemage Graphics",
        "Intel(R) Battlemage(TM) Graphics",
    ] {
        assert!(is_intel_gpu_name(name), "filter dropped: {name}");
    }
}

#[test]
fn xe_lpg_explicit_fixtures_pass_filter() {
    assert!(is_intel_gpu_name("Intel Xe-LPG Graphics"));
}

#[test]
fn iris_xe_fixtures_pass_filter() {
    assert!(is_intel_gpu_name("Intel Iris Xe Graphics"));
    assert!(is_intel_gpu_name("Intel(R) Iris(R) Xe Graphics"));
}

#[test]
fn older_integrated_fixtures_pass_filter() {
    // These are still legitimate GPUs (just not SYCL-capable) so the
    // filter MUST keep them — the classifier downstream labels them
    // OlderIntegrated and marks SYCL Capable = "No".
    for name in &[
        "Intel HD Graphics 630",
        "Intel UHD Graphics 770",
        "Intel HD Graphics 520",
        "Intel UHD Graphics 620",
    ] {
        assert!(is_intel_gpu_name(name), "filter dropped: {name}");
    }
}

// Regression cases — these "Intel"-named devices that can appear in
// `Win32_VideoController` enumeration on some systems MUST NOT be
// matched as GPUs. Includes both the `(R)`/`(TM)`-decorated and
// plain-prose variants since both forms appear in WMI output.

#[test]
fn intel_display_audio_excluded_plain() {
    assert!(!is_intel_gpu_name("Intel Display Audio"));
}

#[test]
fn intel_smart_sound_excluded_plain() {
    assert!(!is_intel_gpu_name("Intel Smart Sound Technology"));
}

#[test]
fn intel_thunderbolt_excluded() {
    assert!(!is_intel_gpu_name("Intel(R) Thunderbolt(TM) Controller"));
}

#[test]
fn intel_wifi_excluded() {
    assert!(!is_intel_gpu_name("Intel(R) Wi-Fi 6E AX211 160MHz"));
}

// ---------- Architecture classifier wiring ----------

#[test]
fn sycl_capable_label_renders_known_yes_no_unknown() {
    use crate::device::readers::intel_gpu_names::IntelArchitecture;

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
