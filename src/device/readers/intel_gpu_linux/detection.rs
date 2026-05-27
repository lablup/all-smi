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

//! Detection helpers for the Intel client GPU reader.
//!
//! Pulled out of the parent module so the reader file stays within the
//! 500-line budget after the addition of per-process fdinfo accounting
//! (issue #247). The public entry point
//! [`crate::device::readers::intel_gpu_linux::has_intel_client_gpu`]
//! delegates here.

use super::{is_card_node, is_intel_vendor, resolve_driver};
use crate::device::common::execute_command_default;
use std::path::Path;

/// Walk `drm_root` and `lspci -n` to decide whether at least one Intel
/// client GPU is present. See
/// [`crate::device::readers::intel_gpu_linux::has_intel_client_gpu`] for
/// the public contract.
pub(super) fn has_intel_client_gpu_from_root(drm_root: &Path) -> bool {
    if let Ok(entries) = std::fs::read_dir(drm_root) {
        for entry in entries.flatten() {
            let path = entry.path();
            let name = match path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n.to_string(),
                None => continue,
            };
            if !is_card_node(&name) {
                continue;
            }
            let device_dir = path.join("device");
            if !is_intel_vendor(&device_dir) {
                continue;
            }
            let driver = resolve_driver(&device_dir);
            if driver == "i915" || driver == "xe" {
                return true;
            }
        }
    }

    // Fallback: `lspci -n` for hosts without `/sys` access (some
    // unprivileged containers). Class codes 0300/0301/0302/0380 cover
    // VGA / XGA / 3D / Display controllers respectively.
    if let Ok(output) = execute_command_default("lspci", &["-n"])
        && output.status == 0
    {
        for line in output.stdout.lines() {
            if line_matches_intel_gpu(line) {
                return true;
            }
        }
    }
    false
}

/// Match a single `lspci -n` line against the "Intel GPU" criteria
/// (graphics-class code + Intel vendor ID).
///
/// `lspci -n` lines look like `03:00.0 0300: 8086:56a0 (rev 08)`. We
/// pull out the class (`0300`) and the vendor (`8086`) tokens. Vendor
/// 8086 alone is not enough — an Intel NIC would falsely match without
/// the class check.
pub(super) fn line_matches_intel_gpu(line: &str) -> bool {
    let mut tokens = line.split_whitespace();
    let _bdf = tokens.next();
    let class = tokens.next().unwrap_or("").trim_end_matches(':');
    let vendor_device = tokens.next().unwrap_or("");

    let class_match = matches!(class, "0300" | "0301" | "0302" | "0380");
    if !class_match {
        return false;
    }
    vendor_device.split(':').next() == Some("8086")
}
