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

//! Board-label stage and `ALL_SMI_CPU_TEMP_SENSOR` override (issue #495).

use super::tests::{hwmon, write, zone};
use super::*;
use tempfile::TempDir;

fn resolve(root: &Path, value: Option<&str>) -> CpuTempResolution {
    resolve_cpu_temp(root, value)
}

fn assert_pick(res: &CpuTempResolution, rule: TempRule, rel: &str, label: Option<&str>) {
    assert_eq!(res.rule, rule, "{res:?}");
    let path = res.path.as_deref().expect("a path");
    assert!(path.ends_with(rel), "{path:?} does not end with {rel}");
    assert_eq!(res.label.as_deref(), label, "{res:?}");
}

/// nct6798 with SYSTIN, CPUTIN and (optionally) PECI Agent 0, a junk
/// thermal_zone0, and no CPU driver.
fn nct6798(with_peci: bool) -> TempDir {
    let dir = TempDir::new().unwrap();
    let r = dir.path();
    zone(r, 0, "acpitz", 16000);
    let mut temps = vec![(1, Some("SYSTIN"), 30000), (2, Some("CPUTIN"), 41000)];
    if with_peci {
        temps.push((7, Some("PECI Agent 0"), 48000));
    }
    hwmon(r, 2, "nct6798", &temps);
    dir
}

#[test]
fn nct6798_prefers_peci_then_cputin() {
    let dir = nct6798(true);
    let res = resolve(dir.path(), None);
    assert_pick(
        &res,
        TempRule::BoardLabel,
        "hwmon2/temp7_input",
        Some("PECI Agent 0"),
    );
    assert_eq!(res.driver.as_deref(), Some("nct6798"));
    assert_eq!(res.override_rejection, None);

    let dir = nct6798(false);
    let res = resolve(dir.path(), None);
    assert_pick(
        &res,
        TempRule::BoardLabel,
        "hwmon2/temp2_input",
        Some("CPUTIN"),
    );
}

#[test]
fn dead_peci_falls_through_to_cputin() {
    let dir = nct6798(true);
    write(dir.path(), "class/hwmon/hwmon2/temp7_input", "x");
    let res = resolve(dir.path(), None);
    assert_pick(
        &res,
        TempRule::BoardLabel,
        "hwmon2/temp2_input",
        Some("CPUTIN"),
    );
}

#[test]
fn asus_ec_sensors_prefers_cpu_package_over_cpu() {
    let dir = TempDir::new().unwrap();
    let r = dir.path();
    hwmon(
        r,
        3,
        "asus_ec_sensors",
        &[(1, Some("CPU"), 45000), (2, Some("CPU Package"), 52000)],
    );
    let res = resolve(r, None);
    assert_pick(
        &res,
        TempRule::BoardLabel,
        "hwmon3/temp2_input",
        Some("CPU Package"),
    );
}

#[test]
fn asus_wmi_sensors_takes_cpu_temperature_not_socket() {
    let dir = TempDir::new().unwrap();
    let r = dir.path();
    hwmon(
        r,
        4,
        "asus_wmi_sensors",
        &[
            (1, Some("CPU Socket Temperature"), 33000),
            (2, Some("CPU VRM Temperature"), 40000),
            (3, Some("CPU Temperature"), 47000),
        ],
    );
    let res = resolve(r, None);
    assert_pick(
        &res,
        TempRule::BoardLabel,
        "hwmon4/temp3_input",
        Some("CPU Temperature"),
    );
}

#[test]
fn nct6683_peci_is_recognised() {
    let dir = TempDir::new().unwrap();
    let r = dir.path();
    hwmon(
        r,
        1,
        "nct6683",
        &[
            (1, Some("Thermistor 14"), 28000),
            (2, Some("PECI 0.0"), 51000),
        ],
    );
    let res = resolve(r, None);
    assert_pick(
        &res,
        TempRule::BoardLabel,
        "hwmon1/temp2_input",
        Some("PECI 0.0"),
    );
}

#[test]
fn board_label_priority_beats_hwmon_index() {
    let dir = TempDir::new().unwrap();
    let r = dir.path();
    hwmon(r, 1, "nct6798", &[(2, Some("CPUTIN"), 41000)]);
    hwmon(r, 9, "asus_ec_sensors", &[(1, Some("CPU Package"), 52000)]);
    let res = resolve(r, None);
    assert_pick(
        &res,
        TempRule::BoardLabel,
        "hwmon9/temp1_input",
        Some("CPU Package"),
    );
}

#[test]
fn board_label_match_is_case_sensitive() {
    let dir = TempDir::new().unwrap();
    let r = dir.path();
    zone(r, 0, "acpitz", 39000);
    hwmon(r, 2, "nct6798", &[(1, Some("cputin"), 41000)]);
    assert_eq!(resolve(r, None).rule, TempRule::Legacy);
}

#[test]
fn cpu_driver_and_thermal_zone_beat_board_labels() {
    let dir = nct6798(true);
    let r = dir.path();
    hwmon(r, 5, "k10temp", &[(1, Some("Tctl"), 60000)]);
    let res = resolve(r, None);
    assert_pick(&res, TempRule::Driver, "hwmon5/temp1_input", Some("Tctl"));
    assert_eq!(res.driver.as_deref(), Some("k10temp"));

    let dir = nct6798(true);
    let r = dir.path();
    hwmon(r, 6, "coretemp", &[(1, Some("Package id 0"), 58000)]);
    assert_eq!(resolve(r, None).rule, TempRule::Driver);

    let dir = nct6798(true);
    let r = dir.path();
    zone(r, 4, "x86_pkg_temp", 44000);
    let res = resolve(r, None);
    assert_pick(&res, TempRule::ThermalZone, "thermal_zone4/temp", None);
    assert_eq!(res.driver.as_deref(), Some("x86_pkg_temp"));
}

#[test]
fn override_driver_index_selects_unlabelled_it87_input() {
    let dir = TempDir::new().unwrap();
    let r = dir.path();
    zone(r, 0, "acpitz", 16000);
    hwmon(
        r,
        3,
        "it87",
        &[(1, None, 30000), (2, None, 49000), (3, None, 35000)],
    );
    let res = resolve(r, Some("it87:temp2"));
    assert_pick(&res, TempRule::Override, "hwmon3/temp2_input", None);
    assert_eq!(res.driver.as_deref(), Some("it87"));
    assert_eq!(res.override_rejection, None);
}

#[test]
fn override_driver_index_uses_lowest_readable_instance() {
    let dir = TempDir::new().unwrap();
    let r = dir.path();
    hwmon(r, 4, "it87", &[(2, None, 50000)]);
    hwmon(r, 11, "it87", &[(2, None, 60000)]);
    let res = resolve(r, Some("it87:temp2"));
    assert_pick(&res, TempRule::Override, "hwmon4/temp2_input", None);
    write(r, "class/hwmon/hwmon4/temp2_input", "x");
    let res = resolve(r, Some("it87:temp2"));
    assert_pick(&res, TempRule::Override, "hwmon11/temp2_input", None);
}

#[test]
fn override_label_selects_exact_label_over_automatic_choice() {
    let dir = nct6798(true);
    let res = resolve(dir.path(), Some("nct6798:CPUTIN"));
    assert_pick(
        &res,
        TempRule::Override,
        "hwmon2/temp2_input",
        Some("CPUTIN"),
    );
}

#[test]
fn override_absolute_path_maps_onto_root() {
    let dir = nct6798(true);
    let r = dir.path();
    let res = resolve(r, Some("/sys/class/hwmon/hwmon2/temp1_input"));
    assert_eq!(res.rule, TempRule::Override);
    assert_eq!(res.path, Some(r.join("class/hwmon/hwmon2/temp1_input")));
    assert_eq!(res.label.as_deref(), Some("SYSTIN"));

    let res = resolve(r, Some("/sys/class/thermal/thermal_zone0/temp"));
    assert_eq!(res.rule, TempRule::Override);
    assert_eq!(res.path, Some(r.join("class/thermal/thermal_zone0/temp")));
    assert_eq!(res.driver.as_deref(), Some("acpitz"));
}

#[test]
fn override_wins_over_named_cpu_driver() {
    let dir = nct6798(true);
    let r = dir.path();
    hwmon(r, 5, "k10temp", &[(1, Some("Tctl"), 60000)]);
    let res = resolve(r, Some("nct6798:temp7"));
    assert_pick(
        &res,
        TempRule::Override,
        "hwmon2/temp7_input",
        Some("PECI Agent 0"),
    );
}

#[test]
fn bad_override_falls_back_and_records_reason() {
    let dir = nct6798(true);
    let r = dir.path();
    write(r, "class/hwmon/hwmon2/temp9_input", "garbage");
    let cases: &[(&str, &str)] = &[
        ("it87", "expected `<driver>:temp<N>`"),
        (":temp2", "expected `<driver>:temp<N>`"),
        ("nct6798:", "expected `<driver>:temp<N>`"),
        ("it87:temp2", "no hwmon device is named `it87`"),
        ("nct6798:temp5", "no readable temp5_input"),
        ("nct6798:temp9", "no readable temp9_input"),
        ("nct6798:cputin", "no readable input labelled `cputin`"),
        ("/etc/passwd", "outside /sys"),
        ("/sysfoo/class/hwmon/hwmon2/temp1_input", "outside /sys"),
        ("/sys", "not a temp*_input"),
        ("/sys/../etc/passwd", "`.` or `..`"),
        ("/sys/class/hwmon/hwmon2/name", "not a temp*_input"),
        ("/sys/class/hwmon/hwmon2/temp9_input", "not readable"),
        ("/sys/class/hwmon/hwmon7/temp1_input", "not readable"),
        ("relative/temp1_input", "expected `<driver>:temp<N>`"),
    ];
    for (value, reason) in cases {
        let res = resolve(r, Some(value));
        assert_pick(
            &res,
            TempRule::BoardLabel,
            "hwmon2/temp7_input",
            Some("PECI Agent 0"),
        );
        let rejection = res.override_rejection.as_deref().unwrap_or_default();
        assert!(
            rejection.starts_with(&format!("ALL_SMI_CPU_TEMP_SENSOR={value} ignored: ")),
            "{value}: {rejection}"
        );
        assert!(rejection.contains(reason), "{value}: {rejection}");
    }
}

#[test]
fn empty_override_counts_as_unset() {
    let dir = nct6798(false);
    for value in ["", "   "] {
        let res = resolve(dir.path(), Some(value));
        assert_eq!(res.rule, TempRule::BoardLabel);
        assert_eq!(res.override_rejection, None);
    }
}

#[test]
fn rejected_override_with_no_sensor_records_reason() {
    let dir = TempDir::new().unwrap();
    let res = resolve(dir.path(), Some("it87:temp2"));
    assert_eq!(res.rule, TempRule::None);
    assert_eq!(res.path, None);
    assert!(res.override_rejection.is_some());
}

#[test]
fn scan_runs_once_with_override() {
    let dir = TempDir::new().unwrap();
    let r = dir.path();
    hwmon(r, 3, "it87", &[(1, None, 30000), (2, None, 49000)]);
    let source: &'static CpuTempSource = Box::leak(Box::new(CpuTempSource::with_override(
        r.to_path_buf(),
        Some("it87:temp2"),
    )));
    assert_eq!(source.scan_count(), 0);
    for _ in 0..3 {
        assert_eq!(source.read_celsius(), Some(49));
    }
    assert_eq!(source.resolution().rule, TempRule::Override);
    // The cached file is re-read, but the override is not re-evaluated.
    write(r, "class/hwmon/hwmon3/temp2_input", "51000");
    assert_eq!(source.read_celsius(), Some(51));
    assert_eq!(source.scan_count(), 1);
}

#[test]
fn production_sys_paths_map_onto_the_sys_root() {
    let sys = Path::new("/sys");
    for value in [
        "/sys/class/hwmon/hwmon2/temp1_input",
        "/sys/devices/platform/nct6775.656/hwmon/hwmon2/temp7_input",
        "/sys/class/thermal/thermal_zone0/temp",
        "/sys/devices/virtual/thermal/thermal_zone3/temp",
    ] {
        assert_eq!(map_override_path(sys, value), Ok(PathBuf::from(value)));
    }
    for value in [
        "/sysfoo/class/hwmon/hwmon2/temp1_input",
        "/sys/class/hwmon/hwmon2/../../../etc/temp1_input",
        "/proc/temp1_input",
    ] {
        assert!(map_override_path(sys, value).is_err(), "{value}");
    }
}

#[cfg(unix)]
#[test]
fn symlinked_hwmon_dirs_work_for_both_selection_and_override() {
    // Real sysfs: class/hwmon/hwmonN is a symlink into devices/.
    let dir = TempDir::new().unwrap();
    let r = dir.path();
    let dev = "devices/platform/nct6775.656/hwmon/hwmon2";
    write(r, &format!("{dev}/name"), "nct6798");
    write(r, &format!("{dev}/temp7_label"), "PECI Agent 0");
    write(r, &format!("{dev}/temp7_input"), "48000");
    fs::create_dir_all(r.join("class/hwmon")).unwrap();
    std::os::unix::fs::symlink(format!("../../{dev}"), r.join("class/hwmon/hwmon2")).unwrap();

    let res = resolve(r, None);
    assert_pick(
        &res,
        TempRule::BoardLabel,
        "hwmon2/temp7_input",
        Some("PECI Agent 0"),
    );

    for value in [
        "/sys/class/hwmon/hwmon2/temp7_input".to_string(),
        format!("/sys/{dev}/temp7_input"),
        "nct6798:temp7".to_string(),
    ] {
        let res = resolve(r, Some(&value));
        assert_eq!(res.rule, TempRule::Override, "{value}: {res:?}");
        assert_eq!(res.label.as_deref(), Some("PECI Agent 0"), "{value}");
    }
}
