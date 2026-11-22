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
use crate::device::platform_detection::{any_pci_device, pci_is_intel_gpu};
use std::path::Path;

/// Walk `drm_root`, then the shared PCI inventory, to decide whether at least one Intel
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

    // Fallback: the shared PCI inventory (sysfs, else one `lspci -nn` per
    // process) for hosts without `/sys/class/drm` (some unprivileged
    // containers). Class codes 0300/0301/0302/0380 cover VGA / XGA / 3D /
    // Display controllers; vendor 8086 alone is not enough, since an Intel
    // NIC would match without the class check (issue #504).
    any_pci_device(pci_is_intel_gpu)
}
