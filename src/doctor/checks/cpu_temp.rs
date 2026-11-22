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

//! `platform.cpu_temp`: which file the Linux CPU temperature is read from and
//! why (issue #495). Registered through [`super::platform::checks`].

#[cfg(target_os = "linux")]
use crate::device::cpu_linux::{
    ASUS_WMI_SENSORS_DRIVER, CpuTempResolution, TempRule, cpu_temp_status,
};
use crate::doctor::types::{Check, CheckCtx, CheckResult, Severity};
#[cfg(target_os = "linux")]
use crate::parsing::common::strip_control_chars;

pub(super) static CPU_TEMP: Check = Check {
    id: "platform.cpu_temp",
    title: "CPU temperature sensor",
    severity_on_fail: Severity::Info,
    run: check_cpu_temp,
};

#[cfg(target_os = "linux")]
const OVERRIDE_HINT: &str = "set ALL_SMI_CPU_TEMP_SENSOR to <driver>:temp<N>, <driver>:<label>, or a /sys path; find candidates with `sensors` or /sys/class/hwmon/*/name and temp*_label";

#[cfg(target_os = "linux")]
const ASUS_WMI_HINT: &str = "pin another sensor with ALL_SMI_CPU_TEMP_SENSOR=<driver>:temp<N> if the board has one; see the asus_wmi_sensors note in docs/troubleshooting.md";

#[cfg(target_os = "linux")]
fn check_cpu_temp(_ctx: &CheckCtx) -> CheckResult {
    // Same process-wide source as the CPU readers: no second sysfs scan.
    let (resolution, reading) = cpu_temp_status();
    describe(resolution, reading)
}

#[cfg(not(target_os = "linux"))]
fn check_cpu_temp(_ctx: &CheckCtx) -> CheckResult {
    CheckResult::Skip("CPU temperature sensor selection is Linux-only".to_string())
}

/// Render a resolution and the current reading as a check outcome.
#[cfg(target_os = "linux")]
fn describe(res: &CpuTempResolution, reading: Option<u32>) -> CheckResult {
    // Driver and label come from sysfs, and the rejection embeds the override
    // value, so strip control characters before they reach the terminal.
    let rejection_text = res.override_rejection.as_deref().map(strip_control_chars);
    let Some(path) = &res.path else {
        let msg = "no CPU temperature sensor found".to_string();
        return match &rejection_text {
            Some(rejection) => {
                CheckResult::Warn(format!("{msg}; {rejection}"), Some(OVERRIDE_HINT.into()))
            }
            None => CheckResult::Skip(format!("{msg} ({OVERRIDE_HINT})")),
        };
    };
    let mut msg = res.rule.as_str().to_string();
    if let Some(driver) = &res.driver {
        msg.push_str(&format!(" {}", strip_control_chars(driver)));
    }
    if let Some(label) = &res.label {
        msg.push_str(&format!(" \"{}\"", strip_control_chars(label)));
    }
    msg.push_str(&format!(
        " at {}",
        strip_control_chars(&path.display().to_string())
    ));
    match reading {
        Some(celsius) => msg.push_str(&format!(", {celsius}°C")),
        None => msg.push_str(", currently unreadable"),
    }
    if let Some(rejection) = &rejection_text {
        return CheckResult::Warn(format!("{msg}; {rejection}"), Some(OVERRIDE_HINT.into()));
    }
    if reading.is_none() {
        return CheckResult::Warn(
            msg,
            Some("the sensor chosen at startup no longer reads; restart all-smi or set ALL_SMI_CPU_TEMP_SENSOR".into()),
        );
    }
    let asus_wmi = res.driver.as_deref() == Some(ASUS_WMI_SENSORS_DRIVER);
    if asus_wmi && res.rule == TempRule::Override {
        // The user chose it; note the hazard without warning.
        msg.push_str(" (asus_wmi_sensors: frequent polling can upset some ASUS BIOSes)");
    } else if asus_wmi {
        return CheckResult::Warn(
            format!(
                "{msg}; asus_wmi_sensors is the only CPU source, and polling its BIOS WMI interface often can stop or pin the fans on some ASUS boards"
            ),
            Some(ASUS_WMI_HINT.into()),
        );
    }
    if res.rule == TempRule::Legacy {
        // Legacy paths are a guess, but on boards like GB10 they are the CPU.
        msg.push_str(" (legacy fallback; set ALL_SMI_CPU_TEMP_SENSOR if this is not the CPU)");
    }
    CheckResult::Pass(msg)
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn res(rule: TempRule, path: Option<&str>, rejection: Option<&str>) -> CpuTempResolution {
        CpuTempResolution {
            rule,
            driver: path.map(|_| "nct6798".to_string()),
            label: path.map(|_| "PECI Agent 0".to_string()),
            path: path.map(PathBuf::from),
            override_rejection: rejection.map(str::to_string),
        }
    }

    const PATH: &str = "/sys/class/hwmon/hwmon2/temp7_input";

    #[test]
    fn shows_rule_driver_label_path_and_reading() {
        let r = describe(&res(TempRule::BoardLabel, Some(PATH), None), Some(48));
        let CheckResult::Pass(msg) = r else {
            panic!("expected a pass, got {r:?}");
        };
        assert_eq!(
            msg,
            format!("board-label nct6798 \"PECI Agent 0\" at {PATH}, 48°C")
        );
    }

    #[test]
    fn override_rejection_is_a_warning() {
        let rejection =
            "ALL_SMI_CPU_TEMP_SENSOR=it87:temp2 ignored: no hwmon device is named `it87`";
        let r = describe(
            &res(TempRule::BoardLabel, Some(PATH), Some(rejection)),
            Some(48),
        );
        let CheckResult::Warn(msg, Some(_)) = r else {
            panic!("expected a warning with a hint, got {r:?}");
        };
        assert!(msg.starts_with("board-label nct6798"), "{msg}");
        assert!(msg.ends_with(rejection), "{msg}");
    }

    #[test]
    fn accepted_override_passes() {
        let r = describe(&res(TempRule::Override, Some(PATH), None), Some(50));
        assert!(matches!(r, CheckResult::Pass(ref m) if m.starts_with("override nct6798")));
    }

    #[test]
    fn no_sensor_skips_unless_an_override_was_rejected() {
        let r = describe(&res(TempRule::None, None, None), None);
        assert!(matches!(r, CheckResult::Skip(_)), "{r:?}");
        let r = describe(&res(TempRule::None, None, Some("bad")), None);
        assert!(
            matches!(r, CheckResult::Warn(ref m, _) if m.ends_with("; bad")),
            "{r:?}"
        );
    }

    #[test]
    fn unreadable_sensor_is_a_warning() {
        let r = describe(&res(TempRule::Driver, Some(PATH), None), None);
        assert!(matches!(r, CheckResult::Warn(ref m, _) if m.ends_with("currently unreadable")));
    }

    #[test]
    fn control_characters_are_stripped_from_the_message() {
        let mut r = res(TempRule::Override, Some(PATH), Some("bad \x1b[2Jvalue\n"));
        r.driver = Some("nct\x1b[31m6798".to_string());
        r.label = Some("PECI\x07 Agent\x1b]0;x\x07".to_string());
        let CheckResult::Warn(msg, _) = describe(&r, Some(48)) else {
            panic!("expected a warning");
        };
        assert!(!msg.chars().any(char::is_control), "{msg:?}");
        assert!(msg.contains("nct[31m6798"), "{msg}");
        let r = res(
            TempRule::None,
            None,
            Some("ALL_SMI_CPU_TEMP_SENSOR=\x1b[2J ignored"),
        );
        let CheckResult::Warn(msg, _) = describe(&r, None) else {
            panic!("expected a warning");
        };
        assert!(!msg.chars().any(char::is_control), "{msg:?}");
    }

    fn asus_wmi(rule: TempRule) -> CpuTempResolution {
        CpuTempResolution {
            rule,
            driver: Some("asus_wmi_sensors".to_string()),
            label: Some("CPU Temperature".to_string()),
            path: Some(PathBuf::from("/sys/class/hwmon/hwmon4/temp3_input")),
            override_rejection: None,
        }
    }

    #[test]
    fn automatic_asus_wmi_pick_is_a_warning() {
        let r = describe(&asus_wmi(TempRule::BoardLabel), Some(47));
        let CheckResult::Warn(msg, Some(hint)) = r else {
            panic!("expected a warning with a hint, got {r:?}");
        };
        assert!(msg.starts_with("board-label asus_wmi_sensors"), "{msg}");
        assert!(msg.contains("polling"), "{msg}");
        assert!(hint.contains("ALL_SMI_CPU_TEMP_SENSOR"), "{hint}");
        assert!(hint.contains("docs/troubleshooting.md"), "{hint}");
    }

    #[test]
    fn overridden_asus_wmi_pick_passes_with_a_note() {
        let r = describe(&asus_wmi(TempRule::Override), Some(47));
        let CheckResult::Pass(msg) = r else {
            panic!("expected a pass, got {r:?}");
        };
        assert!(
            msg.ends_with("frequent polling can upset some ASUS BIOSes)"),
            "{msg}"
        );
    }

    #[test]
    fn sensor_type_pick_shows_driver_and_input() {
        let r = CpuTempResolution {
            rule: TempRule::SensorType,
            driver: Some("it8689".to_string()),
            label: None,
            path: Some(PathBuf::from("/sys/class/hwmon/hwmon3/temp3_input")),
            override_rejection: None,
        };
        let CheckResult::Pass(msg) = describe(&r, Some(46)) else {
            panic!("expected a pass");
        };
        assert_eq!(
            msg,
            "sensor-type it8689 at /sys/class/hwmon/hwmon3/temp3_input, 46°C"
        );
    }

    #[test]
    fn real_check_resolves_through_the_process_wide_source() {
        // Whatever this host has, the check runs and never fails.
        let (first, _) = cpu_temp_status();
        let (second, _) = cpu_temp_status();
        assert!(std::ptr::eq(first, second));
    }
}
