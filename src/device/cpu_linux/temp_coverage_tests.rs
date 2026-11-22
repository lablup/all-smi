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

//! Widened board label, sensor type and ARM thermal zone coverage (issue #497).

use super::tests::{hwmon, write, zone};
use super::*;
use tempfile::TempDir;

fn assert_pick(res: &CpuTempResolution, rule: TempRule, rel: &str, label: Option<&str>) {
    assert_eq!(res.rule, rule, "{res:?}");
    let path = res.path.as_deref().expect("a path");
    assert!(path.ends_with(rel), "{path:?} does not end with {rel}");
    assert_eq!(res.label.as_deref(), label, "{res:?}");
}

#[test]
fn each_new_board_label_resolves_on_its_driver() {
    let cases = [
        ("nct6799", "PECI/TSI Agent 0 Calibration"),
        ("nct6799", "TSI0_TEMP"),
        ("nct6775", "AMD SB-TSI"),
        ("nct6687", "AMD TSI Addr 98h"),
        ("nct6686", "AMD TSI Addr 98h"),
        ("hp_wmi_sensors", "CPU0 Temperature"),
        ("hp_wmi_sensors", "CPU Temperature"),
        ("dell_smm", "CPU"),
        ("lenovo_ec", "CPU1"),
        ("apm_xgene", "SoC Temperature"),
    ];
    for (driver, label) in cases {
        let dir = TempDir::new().unwrap();
        let r = dir.path();
        hwmon(
            r,
            3,
            driver,
            &[(1, Some("SYSTIN"), 30000), (4, Some(label), 47000)],
        );
        let res = resolve_cpu_temp(r, None);
        assert_pick(
            &res,
            TempRule::BoardLabel,
            "hwmon3/temp4_input",
            Some(label),
        );
        assert_eq!(res.driver.as_deref(), Some(driver), "{driver} {label}");
    }
}

#[test]
fn peci_agent_beats_tsi_on_the_same_chip() {
    let dir = TempDir::new().unwrap();
    let r = dir.path();
    hwmon(
        r,
        2,
        "nct6799",
        &[
            (1, Some("TSI0_TEMP"), 50000),
            (7, Some("PECI Agent 0"), 48000),
        ],
    );
    let res = resolve_cpu_temp(r, None);
    assert_pick(
        &res,
        TempRule::BoardLabel,
        "hwmon2/temp7_input",
        Some("PECI Agent 0"),
    );
}

#[test]
fn hp_wmi_multi_socket_takes_cpu0() {
    let dir = TempDir::new().unwrap();
    let r = dir.path();
    hwmon(
        r,
        5,
        "hp_wmi_sensors",
        &[
            (1, Some("CPU1 Temperature"), 52000),
            (2, Some("CPU0 Temperature"), 49000),
        ],
    );
    let res = resolve_cpu_temp(r, None);
    assert_pick(
        &res,
        TempRule::BoardLabel,
        "hwmon5/temp2_input",
        Some("CPU0 Temperature"),
    );
}

#[test]
fn pch_and_smbus_labels_never_resolve() {
    for label in ["PCH_CPU_TEMP", "SMBUSMASTER 0"] {
        let dir = TempDir::new().unwrap();
        let r = dir.path();
        hwmon(r, 2, "nct6798", &[(3, Some(label), 45000)]);
        let res = resolve_cpu_temp(r, None);
        assert_eq!(res.rule, TempRule::None, "{label}: {res:?}");
    }
}

#[test]
fn asusec_wins_over_asus_wmi_sensors() {
    let dir = TempDir::new().unwrap();
    let r = dir.path();
    // asus_wmi_sensors has the better label and the lower index, and still loses.
    hwmon(
        r,
        1,
        "asus_wmi_sensors",
        &[(1, Some("CPU Temperature"), 47000)],
    );
    hwmon(r, 4, "asusec", &[(2, Some("CPU"), 45000)]);
    let res = resolve_cpu_temp(r, None);
    assert_pick(
        &res,
        TempRule::BoardLabel,
        "hwmon4/temp2_input",
        Some("CPU"),
    );
    assert_eq!(res.driver.as_deref(), Some("asusec"));

    // An unreadable asusec input leaves asus_wmi_sensors as the candidate.
    write(r, "class/hwmon/hwmon4/temp2_input", "x");
    let res = resolve_cpu_temp(r, None);
    assert_pick(
        &res,
        TempRule::BoardLabel,
        "hwmon1/temp1_input",
        Some("CPU Temperature"),
    );
    assert_eq!(res.driver.as_deref(), Some(ASUS_WMI_SENSORS_DRIVER));
}

#[test]
fn sensor_type_wins_over_asus_wmi_sensors() {
    let dir = TempDir::new().unwrap();
    let r = dir.path();
    hwmon(
        r,
        1,
        "asus_wmi_sensors",
        &[(1, Some("CPU Temperature"), 47000)],
    );
    hwmon(r, 3, "it8689", &[(3, None, 46000)]);
    write(r, "class/hwmon/hwmon3/temp3_type", "6");
    let res = resolve_cpu_temp(r, None);
    assert_pick(&res, TempRule::SensorType, "hwmon3/temp3_input", None);
    assert_eq!(res.driver.as_deref(), Some("it8689"));

    // With no PECI or TSI input elsewhere, asus_wmi_sensors still resolves.
    write(r, "class/hwmon/hwmon3/temp3_type", "4");
    let res = resolve_cpu_temp(r, None);
    assert_pick(
        &res,
        TempRule::BoardLabel,
        "hwmon1/temp1_input",
        Some("CPU Temperature"),
    );
}

/// Unlabelled it8689 with three inputs and the given `temp*_type` values.
fn it8689(types: [&str; 3]) -> TempDir {
    let dir = TempDir::new().unwrap();
    let r = dir.path();
    zone(r, 0, "acpitz", 27000);
    hwmon(
        r,
        3,
        "it8689",
        &[(1, None, 30000), (2, None, 33000), (3, None, 46000)],
    );
    for (i, ty) in types.iter().enumerate() {
        write(r, &format!("class/hwmon/hwmon3/temp{}_type", i + 1), ty);
    }
    dir
}

#[test]
fn unlabelled_peci_input_resolves_by_sensor_type() {
    let dir = it8689(["4", "4", "6"]);
    let res = resolve_cpu_temp(dir.path(), None);
    assert_pick(&res, TempRule::SensorType, "hwmon3/temp3_input", None);
    assert_eq!(res.driver.as_deref(), Some("it8689"));
    assert_eq!(res.rule.as_str(), "sensor-type");

    let dir = it8689(["4", "5\n", "4"]);
    let res = resolve_cpu_temp(dir.path(), None);
    assert_pick(&res, TempRule::SensorType, "hwmon3/temp2_input", None);
}

#[test]
fn thermistor_only_tree_falls_to_legacy() {
    let dir = it8689(["4", "4", "4"]);
    let res = resolve_cpu_temp(dir.path(), None);
    assert_pick(&res, TempRule::Legacy, "thermal_zone0/temp", None);
}

#[test]
fn sensor_type_ties_go_to_lower_index_then_lower_input() {
    let dir = TempDir::new().unwrap();
    let r = dir.path();
    hwmon(r, 7, "it8792", &[(1, None, 40000)]);
    write(r, "class/hwmon/hwmon7/temp1_type", "6");
    hwmon(r, 2, "it8689", &[(2, None, 41000), (5, None, 42000)]);
    write(r, "class/hwmon/hwmon2/temp5_type", "6");
    write(r, "class/hwmon/hwmon2/temp2_type", "5");
    let res = resolve_cpu_temp(r, None);
    assert_pick(&res, TempRule::SensorType, "hwmon2/temp2_input", None);

    // A dead candidate falls through to the next one.
    write(r, "class/hwmon/hwmon2/temp2_input", "x");
    let res = resolve_cpu_temp(r, None);
    assert_pick(&res, TempRule::SensorType, "hwmon2/temp5_input", None);
}

#[test]
fn board_label_beats_sensor_type() {
    let dir = TempDir::new().unwrap();
    let r = dir.path();
    hwmon(r, 1, "it8689", &[(3, None, 46000)]);
    write(r, "class/hwmon/hwmon1/temp3_type", "6");
    hwmon(r, 2, "nct6798", &[(2, Some("CPUTIN"), 41000)]);
    let res = resolve_cpu_temp(r, None);
    assert_pick(
        &res,
        TempRule::BoardLabel,
        "hwmon2/temp2_input",
        Some("CPUTIN"),
    );
}

#[test]
fn each_new_thermal_zone_type_resolves() {
    for ty in [
        "package-thermal",
        "cpu0-thermal",
        "soc-thermal",
        "soc_thermal",
        "BCPU-therm",
        "bigcore0-thermal",
    ] {
        let dir = TempDir::new().unwrap();
        let r = dir.path();
        zone(r, 0, "gpu-thermal", 30000);
        zone(r, 3, ty, 44000);
        let res = resolve_cpu_temp(r, None);
        assert_pick(&res, TempRule::ThermalZone, "thermal_zone3/temp", None);
        assert_eq!(res.driver.as_deref(), Some(ty));
    }
}

#[test]
fn rk3588_prefers_package_over_cluster_zones() {
    let dir = TempDir::new().unwrap();
    let r = dir.path();
    zone(r, 0, "package-thermal", 45000);
    zone(r, 1, "bigcore0-thermal", 47000);
    zone(r, 2, "bigcore2-thermal", 46000);
    zone(r, 3, "littlecore-thermal", 44000);
    let res = resolve_cpu_temp(r, None);
    assert_pick(&res, TempRule::ThermalZone, "thermal_zone0/temp", None);

    // The cluster zone is a fallback even at a lower zone index.
    let dir = TempDir::new().unwrap();
    let r = dir.path();
    zone(r, 0, "bigcore0-thermal", 47000);
    zone(r, 4, "package-thermal", 45000);
    let res = resolve_cpu_temp(r, None);
    assert_pick(&res, TempRule::ThermalZone, "thermal_zone4/temp", None);
    assert_eq!(res.driver.as_deref(), Some("package-thermal"));
}

#[test]
fn existing_zone_types_keep_priority_over_new_arm_types() {
    // A host that resolved to `cpu-thermal` under #496 must not switch to a
    // newly listed type just because that zone has a lower index.
    let dir = TempDir::new().unwrap();
    let r = dir.path();
    zone(r, 0, "soc-thermal", 52000);
    zone(r, 1, "cpu-thermal", 48000);
    let res = resolve_cpu_temp(r, None);
    assert_pick(&res, TempRule::ThermalZone, "thermal_zone1/temp", None);
    assert_eq!(res.driver.as_deref(), Some("cpu-thermal"));

    // Secondary types still beat the per-cluster fallback at any index.
    let dir = TempDir::new().unwrap();
    let r = dir.path();
    zone(r, 0, "bigcore0-thermal", 47000);
    zone(r, 3, "soc_thermal", 45000);
    let res = resolve_cpu_temp(r, None);
    assert_pick(&res, TempRule::ThermalZone, "thermal_zone3/temp", None);
}

#[test]
fn other_cluster_zones_are_not_cpu_zones() {
    let dir = TempDir::new().unwrap();
    let r = dir.path();
    zone(r, 2, "bigcore2-thermal", 46000);
    zone(r, 3, "littlecore-thermal", 44000);
    assert_eq!(resolve_cpu_temp(r, None).rule, TempRule::None);
}
