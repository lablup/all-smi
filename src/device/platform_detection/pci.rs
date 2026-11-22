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

//! One PCI device inventory shared by every hardware detector (issue #504).
//!
//! Each detector used to spawn its own `lspci` (up to eight per startup on a
//! host with no accelerators). Every check they make can be stated over a
//! device's `(vendor, device, class)` triple, so the inventory is read once
//! from `/sys/bus/pci/devices/*/{vendor,device,class}`, which spawns nothing.
//! `lspci -nn` runs only when that directory cannot be read (some
//! unprivileged containers), and then at most once per process.

// Several vendor IDs and predicates serve Linux-only detectors (Neuron,
// Tenstorrent, AMD on glibc, the Intel client GPU fallback).
#![cfg_attr(any(not(target_os = "linux"), target_env = "musl"), allow(dead_code))]

use std::path::Path;
use std::sync::OnceLock;

use crate::device::common::execute_command_default;

/// PCI vendor IDs the detectors look for.
pub(crate) mod vendor {
    pub const NVIDIA: u16 = 0x10de;
    pub const AMD: u16 = 0x1002;
    pub const INTEL: u16 = 0x8086;
    pub const AMAZON: u16 = 0x1d0f;
    pub const HABANA: u16 = 0x1da3;
    pub const TENSTORRENT: u16 = 0x1e52;
    pub const REBELLIONS: u16 = 0x1eff;
}

/// One PCI function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PciDevice {
    pub vendor: u16,
    pub device: u16,
    /// Base class and subclass (`0x0300` for a VGA controller), the four
    /// digits `lspci -nn` prints in brackets. sysfs `class` carries the
    /// programming interface as a third byte, which is dropped.
    pub class: u16,
}

impl PciDevice {
    /// VGA (`0300`), XGA (`0301`), 3D (`0302`) or other display (`0380`)
    /// controller.
    pub(crate) fn is_display(&self) -> bool {
        matches!(self.class, 0x0300 | 0x0301 | 0x0302 | 0x0380)
    }

    /// What `lspci` printed as "VGA compatible controller" or
    /// "3D controller", the two classes the NVIDIA check accepted.
    pub(crate) fn is_vga_or_3d(&self) -> bool {
        matches!(self.class, 0x0300 | 0x0302)
    }
}

/// A process-wide inventory, loaded on first use.
pub(crate) struct PciInventory {
    devices: OnceLock<Option<Vec<PciDevice>>>,
}

impl PciInventory {
    pub(crate) const fn new() -> Self {
        Self {
            devices: OnceLock::new(),
        }
    }

    /// The inventory, running `load` only on the first call. `None` means
    /// neither sysfs nor `lspci` could list devices.
    pub(crate) fn get_or_load(
        &self,
        load: impl FnOnce() -> Option<Vec<PciDevice>>,
    ) -> Option<&[PciDevice]> {
        self.devices.get_or_init(load).as_deref()
    }
}

static PCI_INVENTORY: PciInventory = PciInventory::new();

/// The host's PCI devices, read once per process (sysfs, else `lspci -nn`).
pub(crate) fn pci_devices() -> Option<&'static [PciDevice]> {
    PCI_INVENTORY.get_or_load(load_pci_devices)
}

/// Whether any PCI device satisfies `matches`. `false` when there is no
/// inventory at all.
pub(crate) fn any_pci_device(matches: impl Fn(&PciDevice) -> bool) -> bool {
    pci_devices().is_some_and(|devices| devices.iter().any(matches))
}

fn load_pci_devices() -> Option<Vec<PciDevice>> {
    read_sysfs_devices(Path::new("/sys/bus/pci/devices")).or_else(|| {
        let output = execute_command_default("lspci", &["-nn"]).ok()?;
        (output.status == 0).then(|| parse_lspci_nn(&output.stdout))
    })
}

/// Read every `<dir>/*/{vendor,device,class}`. `None` only when `dir` cannot
/// be listed; an entry with an unreadable or malformed file is skipped.
pub(crate) fn read_sysfs_devices(dir: &Path) -> Option<Vec<PciDevice>> {
    let entries = std::fs::read_dir(dir).ok()?;
    let read_hex = |path: &Path| -> Option<u32> {
        let text = std::fs::read_to_string(path).ok()?;
        let text = text.trim();
        u32::from_str_radix(text.strip_prefix("0x").unwrap_or(text), 16).ok()
    };
    Some(
        entries
            .flatten()
            .filter_map(|entry| {
                let path = entry.path();
                let vendor = u16::try_from(read_hex(&path.join("vendor"))?).ok()?;
                let device = u16::try_from(read_hex(&path.join("device"))?).ok()?;
                let class = u16::try_from(read_hex(&path.join("class"))? >> 8).ok()?;
                Some(PciDevice {
                    vendor,
                    device,
                    class,
                })
            })
            .collect(),
    )
}

/// Parse `lspci -nn` output, one device per line, such as
/// `03:00.0 Processing accelerators [1200]: Rebellions Inc. RBLN-CA22 (PF) [1eff:1220] (rev 03)`.
///
/// The class is the first bracketed four-digit hex code, and vendor:device
/// the last bracketed `vvvv:dddd` (device names can carry their own
/// brackets, as in `GA102 [GeForce RTX 3080] [10de:2206]`). A line without
/// both is skipped.
pub(crate) fn parse_lspci_nn(output: &str) -> Vec<PciDevice> {
    output.lines().filter_map(parse_lspci_nn_line).collect()
}

fn parse_lspci_nn_line(line: &str) -> Option<PciDevice> {
    let mut class = None;
    let mut ids = None;
    let mut rest = line;
    while let Some(open) = rest.find('[') {
        let after = &rest[open + 1..];
        let Some(close) = after.find(']') else {
            break;
        };
        let token = &after[..close];
        if class.is_none() && token.len() == 4 {
            class = u16::from_str_radix(token, 16).ok();
        } else if token.len() == 9
            && let Some((vendor, device)) = token.split_once(':')
            && let (Ok(vendor), Ok(device)) = (
                u16::from_str_radix(vendor, 16),
                u16::from_str_radix(device, 16),
            )
        {
            ids = Some((vendor, device));
        }
        rest = &after[close + 1..];
    }
    let (vendor, device) = ids?;
    Some(PciDevice {
        vendor,
        device,
        class: class?,
    })
}

#[cfg(test)]
#[path = "pci_tests.rs"]
mod tests;
