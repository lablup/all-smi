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
use crate::device::cpu_linux::{CpuTempResolution, TempRule, cpu_temp_status};
use crate::doctor::types::{Check, CheckCtx, CheckResult, Severity};

pub(super) static CPU_TEMP: Check = Check {
    id: "platform.cpu_temp",
    title: "CPU temperature sensor",
    severity_on_fail: Severity::Info,
    run: check_cpu_temp,
};

#[cfg(target_os = "linux")]
const OVERRIDE_HINT: &str = "set ALL_SMI_CPU_TEMP_SENSOR to <driver>:temp<N>, <driver>:<label>, or a /sys path; find candidates with `sensors` or /sys/class/hwmon/*/name and temp*_label";

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
    let Some(path) = &res.path else {
        let msg = "no CPU temperature sensor found".to_string();
        return match &res.override_rejection {
            Some(rejection) => {
                CheckResult::Warn(format!("{msg}; {rejection}"), Some(OVERRIDE_HINT.into()))
            }
            None => CheckResult::Skip(format!("{msg} ({OVERRIDE_HINT})")),
        };
    };
    let mut msg = res.rule.as_str().to_string();
    if let Some(driver) = &res.driver {
        msg.push_str(&format!(" {driver}"));
    }
    if let Some(label) = &res.label {
        msg.push_str(&format!(" \"{label}\""));
    }
    msg.push_str(&format!(" at {}", path.display()));
    match reading {
        Some(celsius) => msg.push_str(&format!(", {celsius}°C")),
        None => msg.push_str(", currently unreadable"),
    }
    if let Some(rejection) = &res.override_rejection {
        return CheckResult::Warn(format!("{msg}; {rejection}"), Some(OVERRIDE_HINT.into()));
    }
    if reading.is_none() {
        return CheckResult::Warn(
            msg,
            Some("the sensor chosen at startup no longer reads; restart all-smi or set ALL_SMI_CPU_TEMP_SENSOR".into()),
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
    fn real_check_resolves_through_the_process_wide_source() {
        // Whatever this host has, the check runs and never fails.
        let (first, _) = cpu_temp_status();
        let (second, _) = cpu_temp_status();
        assert!(std::ptr::eq(first, second));
    }
}
