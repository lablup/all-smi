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

//! Motherboard stages of CPU temperature selection (issues #495, #497): the
//! board sensor label stage and the hwmon sensor type stage. Both read only
//! cheap text files (`name`, `temp*_label`, `temp*_type`) to rank candidates and
//! read a sensor input only for the candidate under consideration.

use std::path::{Path, PathBuf};

use super::{hwmon_inputs, hwmon_label, numbered_entries, read_millicelsius, read_trimmed};

/// Motherboard sensor labels (Super I/O, board EC/WMI) that name the CPU input,
/// in priority order. Exact, case-sensitive match. PECI, TSI and package
/// readings are die temperatures; `CPUTIN` is a socket thermistor and ranks
/// last. `PCH_CPU_TEMP` (relayed by the PCH) and `SMBUSMASTER 0` (board
/// dependent) are deliberately absent.
const BOARD_CPU_LABELS: &[&str] = &[
    "CPU Package",                  // asusec
    "PECI Agent 0",                 // nct6775 family
    "PECI Agent 0 Calibration",     // nct6792..nct6798
    "PECI/TSI Agent 0 Calibration", // nct6799
    "TSI0_TEMP",                    // nct6775 family TSI registers ("TSI%d_TEMP")
    "AMD SB-TSI",                   // nct6775
    "PECI 0.0",                     // nct6683, Intel boards
    "AMD TSI Addr 98h",             // nct6683/6686/6687, AMD boards
    "CPU Temperature",              // asus_wmi_sensors, hp_wmi_sensors
    "CPU0 Temperature",             // hp_wmi_sensors, multi-socket
    "CPU",                          // asusec, dell_smm, out-of-tree nct6687
    "CPU1",                         // lenovo_ec
    "SoC Temperature",              // apm_xgene
    "CPUTIN",                       // nct6775 family, socket thermistor
];

/// hwmon `name` of the ASUS WMI driver. Its BIOS interface can stop or pin the
/// fans when polled often, so its board labels are tried only after every other
/// board label and the sensor type stage find nothing (see
/// docs.kernel.org/hwmon/asus_wmi_sensors.html).
#[cfg_attr(not(feature = "cli"), allow(dead_code))]
pub(crate) const ASUS_WMI_SENSORS_DRIVER: &str = "asus_wmi_sensors";

/// hwmon ABI `temp*_type` values that mean the CPU's own digital sensor:
/// 5 = AMD AMDSI (SB-TSI), 6 = Intel PECI. Both rank equally.
const CPU_SENSOR_TYPES: &[&str] = &["5", "6"];

/// Board sensor stage: the best-priority label present on any hwmon instance
/// that reads wins; ties go to the lower hwmon index, then the lower input N.
///
/// `asus_wmi` selects which instances are considered: `false` for every driver
/// except `asus_wmi_sensors`, `true` for `asus_wmi_sensors` alone. The resolver
/// runs the `true` pass after the sensor type stage, so a PECI or TSI input on
/// another chip wins over the hazardous WMI source.
pub(super) fn pick_from_board_labels(root: &Path, asus_wmi: bool) -> Option<PathBuf> {
    let mut candidates: Vec<(usize, PathBuf)> = Vec::new();
    for dir in numbered_entries(&root.join("class/hwmon"), "hwmon") {
        let inputs = hwmon_inputs(&dir);
        if inputs.is_empty() {
            continue;
        }
        let is_asus_wmi =
            read_trimmed(&dir.join("name")).as_deref() == Some(ASUS_WMI_SENSORS_DRIVER);
        if is_asus_wmi != asus_wmi {
            continue;
        }
        for (n, path) in inputs {
            let priority = hwmon_label(&dir, n)
                .and_then(|label| BOARD_CPU_LABELS.iter().position(|l| *l == label));
            if let Some(priority) = priority {
                candidates.push((priority, path));
            }
        }
    }
    candidates.sort_by_key(|(priority, _)| *priority); // stable: keeps index order
    candidates
        .into_iter()
        .map(|(_, p)| p)
        .find(|p| read_millicelsius(p).is_some())
}

/// Sensor type stage for chips without CPU labels (it87 and others): the first
/// input, by hwmon index then N, whose `temp<N>_type` is PECI or AMD TSI and
/// that reads. The input's label, if any, is not consulted.
pub(super) fn pick_from_sensor_types(root: &Path) -> Option<PathBuf> {
    numbered_entries(&root.join("class/hwmon"), "hwmon")
        .into_iter()
        .flat_map(|dir| {
            hwmon_inputs(&dir)
                .into_iter()
                .filter(|(n, _)| {
                    read_trimmed(&dir.join(format!("temp{n}_type")))
                        .is_some_and(|t| CPU_SENSOR_TYPES.contains(&t.as_str()))
                })
                .map(|(_, p)| p)
                .collect::<Vec<_>>()
        })
        .find(|p| read_millicelsius(p).is_some())
}
