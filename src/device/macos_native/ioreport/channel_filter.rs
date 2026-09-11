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

//! Which channels the IOReport subscription asks for.
//!
//! `IOReportCopyChannelsInGroup` describes every channel in a group, and a
//! subscription samples whatever it was opened with. On an M5 Max that is 364
//! Energy Model channels, of which five feed a rail, next to 18 CPU and one
//! GPU performance-state channel. Each group's description is cut down to the
//! channels something reads before the groups are merged, using the same
//! predicates the parsers use, so nothing that was read before is dropped:
//!
//! | group | kept | predicate |
//! |---|---|---|
//! | `Energy Model` | 5 of 364 | [`classify_energy_channel`] |
//! | `CPU Stats` / `CPU Core Performance States` | 18 of 18 | [`classify_cpu_channel`] |
//! | `GPU Stats` / `GPU Performance States` | 1 of 1 | `GPUPH` |
//!
//! Measured on an M5 Max in a release build at a 1 s poll, the merged
//! subscription drops from 383 to 24 channels, `IOReportCreateSamples` from
//! 10.2 ms to 8.7 ms, reading the energy channels out of a raw sample from
//! 226 us to 26 us, and the residency delta from 772 us to 238 us. The
//! sample call stays the floor: the providers do their work per sample, not
//! per channel.

use super::{
    IOReportChannelGetChannelName, cfstr_to_string, classify_cpu_channel, classify_energy_channel,
};
use core_foundation::array::{
    CFArrayAppendValue, CFArrayCreateMutable, CFArrayGetCount, CFArrayGetTypeID,
    CFArrayGetValueAtIndex, CFArrayRef, kCFTypeArrayCallBacks,
};
use core_foundation::base::{CFGetTypeID, CFRelease, CFTypeRef, TCFType, kCFAllocatorDefault};
use core_foundation::dictionary::{
    CFDictionaryCreateMutableCopy, CFDictionaryGetTypeID, CFDictionaryGetValue, CFDictionaryRef,
    CFDictionarySetValue,
};
use core_foundation::string::CFString;
use std::ffi::c_void;

/// Decides, by channel name, whether a channel of one group is subscribed to.
pub(super) type KeepChannel = fn(&str) -> bool;

/// Energy Model channels that feed a rail. Everything the energy tracker sums
/// passes, including `GPU<n>`, which it needs to know whether to fall back.
pub(super) fn keep_energy_channel(name: &str) -> bool {
    classify_energy_channel(name).is_some()
}

/// CPU performance-state channels that name a cluster, which is every channel
/// `process_cpu_channel` does not drop.
pub(super) fn keep_cpu_channel(name: &str) -> bool {
    classify_cpu_channel(name).is_some()
}

/// The one GPU performance-state channel the metrics read.
pub(super) fn keep_gpu_channel(name: &str) -> bool {
    name == "GPUPH"
}

/// Cut a channel-group description down to the channels `keep` accepts.
///
/// Takes ownership of `description`, an owned (+1) dictionary from
/// `IOReportCopyChannelsInGroup`, and returns an owned (+1) dictionary: a
/// copy whose `IOReportChannels` array holds only the accepted channels, with
/// every other key (`QueryOpts`) as it was. The array is mutable, which
/// `IOReportMergeChannels` needs of the dictionary it merges into.
///
/// `description` comes back untouched when it does not have the expected
/// shape, when a copy cannot be made, or when `keep` accepts nothing. The
/// last case matters on hardware whose channel names nobody has recorded yet:
/// subscribing to the whole group there is what happened before the filter,
/// while an empty group could leave nothing to subscribe to.
pub(super) fn retain_channels(description: CFDictionaryRef, keep: KeepChannel) -> CFDictionaryRef {
    if description.is_null() {
        return description;
    }

    let key = CFString::new("IOReportChannels");
    let key_ref = key.as_concrete_TypeRef() as *const c_void;

    // SAFETY: `description` is a live dictionary owned by this function.
    // `CFDictionaryGetValue` follows the get rule, and the value is null- and
    // type-checked as a CFArray before it is used. Every element is null- and
    // type-checked as a CFDictionary before IOReport reads its name. `kept`
    // and `filtered` are created here (+1); `kept` is released once it has
    // been stored in `filtered`, which retains it, and `description` is
    // released only after `filtered` has replaced it.
    unsafe {
        let channels = CFDictionaryGetValue(description, key_ref);
        if channels.is_null() || CFGetTypeID(channels) != CFArrayGetTypeID() {
            return description;
        }
        let channels = channels as CFArrayRef;

        let kept = CFArrayCreateMutable(kCFAllocatorDefault, 0, &kCFTypeArrayCallBacks);
        if kept.is_null() {
            return description;
        }
        for index in 0..CFArrayGetCount(channels) {
            let channel = CFArrayGetValueAtIndex(channels, index);
            if channel.is_null() || CFGetTypeID(channel) != CFDictionaryGetTypeID() {
                continue;
            }
            let accepted =
                cfstr_to_string(IOReportChannelGetChannelName(channel as CFDictionaryRef))
                    .is_some_and(|name| keep(&name));
            if accepted {
                CFArrayAppendValue(kept, channel);
            }
        }

        if CFArrayGetCount(kept as CFArrayRef) == 0 {
            CFRelease(kept as CFTypeRef);
            return description;
        }

        let filtered = CFDictionaryCreateMutableCopy(kCFAllocatorDefault, 0, description);
        if filtered.is_null() {
            CFRelease(kept as CFTypeRef);
            return description;
        }
        CFDictionarySetValue(filtered, key_ref, kept as *const c_void);
        CFRelease(kept as CFTypeRef);
        CFRelease(description as CFTypeRef);
        filtered as CFDictionaryRef
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every Energy Model channel of the recorded M5 Max inventory that the
    /// tracker sums is subscribed to, and nothing else is.
    #[test]
    fn energy_filter_keeps_exactly_the_rails_of_the_m5_max_inventory() {
        let inventory = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/ioreport/m5_max_energy_model.tsv"
        ));
        let names: Vec<&str> = inventory
            .lines()
            .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
            .filter_map(|line| line.split('\t').next())
            .collect();
        assert!(names.len() > 300, "fixture has {} channels", names.len());

        let mut kept: Vec<&str> = names
            .iter()
            .copied()
            .filter(|name| keep_energy_channel(name))
            .collect();
        kept.sort_unstable();
        assert_eq!(
            kept,
            ["ANE0", "CPU Energy", "DRAM0", "GPU Energy", "GPU0"],
            "the filter must keep every rail channel and only those"
        );
    }

    /// The CPU filter is the cluster classifier: what it drops is exactly what
    /// `process_cpu_channel` would have ignored.
    #[test]
    fn cpu_filter_follows_the_cluster_classifier() {
        for name in [
            "MCPU00",
            "MCPU15",
            "PCPU3",
            "ECPU1",
            "DIE_1_PCPU1_CPU3",
            "PCPU",
        ] {
            assert!(keep_cpu_channel(name), "{name}");
            assert!(classify_cpu_channel(name).is_some(), "{name}");
        }
        for name in ["CPU Complex", "DIE_0_ACC", ""] {
            assert_eq!(keep_cpu_channel(name), classify_cpu_channel(name).is_some());
        }
    }

    #[test]
    fn gpu_filter_keeps_only_gpuph() {
        assert!(keep_gpu_channel("GPUPH"));
        assert!(!keep_gpu_channel("GPU0"));
        assert!(!keep_gpu_channel("GPUPH0"));
    }
}
