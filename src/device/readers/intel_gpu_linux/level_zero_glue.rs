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

//! Level Zero augmentation glue for the Linux Intel reader. Lives in a
//! submodule so the per-OS reader file stays under the 500-line budget.

use super::{IntelGpuCard, read_pci_bus_id};
use crate::device::readers::intel_gpu_level_zero as l0;
use crate::device::types::GpuInfo;
use std::path::Path;

/// Run one Level Zero refresh against the just-pushed `GpuInfo` for
/// `card`. Noop when the card has no PCI bus path or when the L0
/// runtime cannot bind it. Called from `IntelGpuReader::get_gpu_info`
/// once per card, *after* the sysfs baseline `GpuInfo` is pushed onto
/// `out`.
pub(super) fn augment(card: &IntelGpuCard, out: &mut [GpuInfo], device_dir: &Path) {
    let Some(last) = out.last_mut() else { return };
    let Some(bus) = read_pci_bus_id(device_dir) else {
        return;
    };
    let normalised = l0::normalise_pci_bdf(&bus);
    if let Ok(mut state) = card.level_zero_state.lock()
        && let Some(readout) = l0::refresh(&mut state, &normalised)
    {
        l0::apply_to_gpu_info(last, &readout, l0::ApplyPlatform::Linux);
    }
}
