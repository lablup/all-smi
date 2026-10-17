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

//! Temperature conversion for Windows ACPI thermal zones.
//!
//! Two WMI classes expose the ACPI thermal zones:
//!
//! * `root\WMI:MSAcpi_ThermalZoneTemperature.CurrentTemperature`, tenths of
//!   Kelvin. Needs administrator rights on many machines and is empty on
//!   others (the RTX Spark N1X returns access denied to a normal user).
//! * `root\cimv2:Win32_PerfFormattedData_Counters_ThermalZoneInformation`,
//!   readable without elevation, with `HighPrecisionTemperature` in tenths
//!   of Kelvin (`3192` is 46.05 C) and `Temperature` in whole Kelvin.
//!
//! The conversion lives here, outside the Windows-only `windows_temp`
//! module, so it is unit-tested on every platform (issue #459).

/// Plausible CPU temperature range in Celsius. Readings outside it are a
/// zone reporting garbage (or 0 K), and are skipped.
const PLAUSIBLE_CELSIUS: std::ops::Range<f64> = 0.0..150.0;

/// Convert tenths of Kelvin to Celsius.
pub fn tenths_kelvin_to_celsius(tenths: u32) -> f64 {
    f64::from(tenths) / 10.0 - 273.15
}

/// Celsius for one `Win32_PerfFormattedData_Counters_ThermalZoneInformation`
/// row: `HighPrecisionTemperature` (tenths of Kelvin) when it is present
/// and plausible, else `Temperature` (Kelvin). `None` when neither is.
pub fn perf_thermal_zone_celsius(
    high_precision_tenths_kelvin: Option<u32>,
    kelvin: Option<u32>,
) -> Option<f64> {
    high_precision_tenths_kelvin
        .map(tenths_kelvin_to_celsius)
        .filter(|c| PLAUSIBLE_CELSIUS.contains(c))
        .or_else(|| {
            kelvin
                .map(|k| f64::from(k) - 273.15)
                .filter(|c| PLAUSIBLE_CELSIUS.contains(c))
        })
}

/// The hottest plausible zone, rounded to whole degrees.
///
/// The hottest zone is the one closest to the CPU on every machine probed:
/// the N1X reports `\_TZ.THRM` (the SoC) at 46 C next to `\_TZ.THZ0` at 43 C.
pub fn hottest_zone_celsius(zones: &[(Option<u32>, Option<u32>)]) -> Option<u32> {
    zones
        .iter()
        .filter_map(|(high_precision, kelvin)| perf_thermal_zone_celsius(*high_precision, *kelvin))
        .fold(None, |hottest: Option<f64>, c| {
            Some(hottest.map_or(c, |h| h.max(c)))
        })
        .map(|c| c.round() as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn high_precision_is_tenths_of_kelvin() {
        let c = perf_thermal_zone_celsius(Some(3192), Some(319)).unwrap();
        assert!((c - 46.05).abs() < 1e-9, "{c}");
    }

    #[test]
    fn falls_back_to_whole_kelvin() {
        let c = perf_thermal_zone_celsius(None, Some(316)).unwrap();
        assert!((c - 42.85).abs() < 1e-9, "{c}");
        // A zero high-precision reading (0 K) is implausible, so Kelvin wins.
        let c = perf_thermal_zone_celsius(Some(0), Some(316)).unwrap();
        assert!((c - 42.85).abs() < 1e-9, "{c}");
    }

    #[test]
    fn implausible_or_missing_readings_are_none() {
        assert_eq!(perf_thermal_zone_celsius(None, None), None);
        assert_eq!(perf_thermal_zone_celsius(Some(0), Some(0)), None);
        assert_eq!(perf_thermal_zone_celsius(Some(9999), None), None);
    }

    #[test]
    fn n1x_zones_report_the_hottest() {
        // \_TZ.THRM and \_TZ.THZ0 as probed on SPARK-WIN01.
        let zones = [(Some(3192), Some(319)), (Some(3162), Some(316))];
        assert_eq!(hottest_zone_celsius(&zones), Some(46));
        assert_eq!(hottest_zone_celsius(&[]), None);
        assert_eq!(hottest_zone_celsius(&[(None, None)]), None);
    }

    #[test]
    fn tenths_kelvin_matches_the_root_wmi_formula() {
        assert!((tenths_kelvin_to_celsius(3000) - 26.85).abs() < 1e-9);
    }
}
