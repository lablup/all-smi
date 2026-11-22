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

use super::*;

#[test]
fn lspci_nn_lines_parse_class_vendor_and_device() {
    let devices = parse_lspci_nn(
        "01:00.0 VGA compatible controller [0300]: NVIDIA Corporation GA102 [GeForce RTX 3080] [10de:2206] (rev a1)
00:1e.0 Processing accelerators [1200]: Amazon Device [1D0F:7164]
garbage line without ids
00:00.0 Host bridge [0600]: Device [zzzz:0001]
",
    );
    assert_eq!(
        devices,
        vec![
            PciDevice {
                vendor: 0x10de,
                device: 0x2206,
                class: 0x0300,
            },
            PciDevice {
                vendor: 0x1d0f,
                device: 0x7164,
                class: 0x1200,
            },
        ]
    );
}

#[test]
fn sysfs_devices_are_read_and_malformed_entries_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let device = |name: &str, vendor: &str, device: &str, class: &str| {
        // Windows rejects `:` in file names; the reader ignores the name.
        let name = if cfg!(windows) {
            name.replace(':', "_")
        } else {
            name.to_string()
        };
        let path = dir.path().join(name);
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("vendor"), vendor).unwrap();
        std::fs::write(path.join("device"), device).unwrap();
        std::fs::write(path.join("class"), class).unwrap();
    };
    device("0000:01:00.0", "0x10de\n", "0x2331\n", "0x030200\n");
    device("0000:02:00.0", "0x8086\n", "0x15fc\n", "0x020000\n");
    device("0000:03:00.0", "nonsense\n", "0x0001\n", "0x030000\n");

    let mut devices = read_sysfs_devices(dir.path()).expect("listable");
    devices.sort_by_key(|device| device.vendor);
    assert_eq!(
        devices,
        vec![
            PciDevice {
                vendor: 0x10de,
                device: 0x2331,
                class: 0x0302,
            },
            PciDevice {
                vendor: 0x8086,
                device: 0x15fc,
                class: 0x0200,
            },
        ]
    );

    // An unreadable directory is `None`, which is what sends the loader to
    // the `lspci -nn` fallback; an empty one is an empty inventory.
    assert_eq!(read_sysfs_devices(&dir.path().join("missing")), None);
    let empty = tempfile::tempdir().unwrap();
    assert_eq!(read_sysfs_devices(empty.path()), Some(Vec::new()));
}

#[test]
fn the_inventory_loads_once_across_detector_calls() {
    let inventory = PciInventory::new();
    let loads = std::cell::Cell::new(0);
    let load = || {
        loads.set(loads.get() + 1);
        Some(vec![PciDevice {
            vendor: 0x1eff,
            device: 0x1220,
            class: 0x1200,
        }])
    };
    for _ in 0..8 {
        let devices = inventory.get_or_load(load).expect("loaded");
        assert!(
            devices
                .iter()
                .any(|device| device.vendor == vendor::REBELLIONS)
        );
        assert!(!devices.iter().any(|device| device.vendor == vendor::NVIDIA));
    }
    assert_eq!(loads.get(), 1);

    // A failed load is cached too, so a host with neither sysfs nor lspci
    // does not retry on every detector.
    let failed = PciInventory::new();
    let attempts = std::cell::Cell::new(0);
    for _ in 0..3 {
        assert!(
            failed
                .get_or_load(|| {
                    attempts.set(attempts.get() + 1);
                    None
                })
                .is_none()
        );
    }
    assert_eq!(attempts.get(), 1);
}
