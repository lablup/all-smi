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

//! PMLog sensor indices and the pure extraction of a readout.
//!
//! `ADLPMLogDataOutput` is addressed by `ADLSensorType`, so reading a
//! value is just indexing the table. The indices below are transcribed
//! from AMD's public `adl_structures.h`.
//!
//! ## These indices are the least verifiable thing in this module
//!
//! Nothing in CI compiles all-smi for Windows, no test can call the real
//! library, and the enum has grown across driver generations. If AMD
//! renumbered an entry, this file would read the wrong sensor and report
//! a plausible but wrong number, which is worse than reporting nothing.
//!
//! There are two mitigations, and it is worth being precise about what
//! each one actually buys, because the weaker of the two is easy to
//! overrate:
//!
//! 1. **The `amd.adl.sensors` doctor check dumps every supported index
//!    and its raw value.** This is the real mitigation. It makes the
//!    mapping confirmable against actual hardware without shipping a
//!    code change first, and it is the only thing here that can catch a
//!    wrong index in the general case.
//! 2. **[`extract`] range-checks every value.** This catches *unit* and
//!    *scale* errors well (a millivolt reading read as watts, a clock
//!    read as a temperature) because those land orders of magnitude
//!    outside the band.
//!
//!    It does **not** reliably catch a small index shift, which is the
//!    likeliest way this file goes wrong, because the enum is grouped by
//!    physical quantity: clocks occupy 1-7, temperatures 8-13, fan 14-15,
//!    activity 19-20. Reading `FAN_PERCENTAGE` (15) as `FAN_RPM` (14)
//!    yields 0-100, comfortably inside `0..=20_000`. Reading
//!    `TEMPERATURE_MEM` as `TEMPERATURE_EDGE` yields a plausible
//!    temperature. The guard is a backstop against gross errors, not a
//!    correctness proof, and mitigation 1 is what the design actually
//!    leans on.

use super::ffi::AdlPmLogDataOutput;

// `ADLSensorType`, from adl_structures.h. Only the entries this reader
// consumes are named; the enum has many more.
pub const PMLOG_CLK_GFXCLK: usize = 1;
pub const PMLOG_CLK_MEMCLK: usize = 2;
pub const PMLOG_TEMPERATURE_EDGE: usize = 8;
pub const PMLOG_TEMPERATURE_MEM: usize = 9;
pub const PMLOG_FAN_RPM: usize = 14;
pub const PMLOG_INFO_ACTIVITY_GFX: usize = 19;
pub const PMLOG_INFO_ACTIVITY_MEM: usize = 20;
pub const PMLOG_ASIC_POWER: usize = 23;
pub const PMLOG_TEMPERATURE_HOTSPOT: usize = 27;
pub const PMLOG_TEMPERATURE_GFX: usize = 28;
pub const PMLOG_GFX_POWER: usize = 30;
/// Whole-board power, including memory and VRM losses.
///
/// On RDNA3 and later this, not [`PMLOG_ASIC_POWER`], is the board
/// figure. `ASIC_POWER` still reports as supported on those parts but
/// measures a narrower domain, so preferring it there would produce a
/// number that is wrong in a way no range check can see (both are watts,
/// both plausible). LibreHardwareMonitor makes the same switch, keyed on
/// the GCN family; this reader instead prefers whichever the card
/// actually publishes, which needs no family lookup and degrades to the
/// old behaviour on parts that only expose `ASIC_POWER`.
pub const PMLOG_BOARD_POWER: usize = 73;

/// Physically sensible bands, used to reject a misindexed or
/// wrongly-scaled reading.
///
/// The bands are deliberately generous: the goal is to catch a value
/// that is obviously not the quantity we asked for, not to second-guess
/// an unusual but real reading.
/// Lower bound is well below freezing on purpose: a machine cold-started
/// in an unheated room reports a genuinely negative die temperature, and
/// rejecting it would drop a real reading.
const TEMPERATURE_C: std::ops::RangeInclusive<i32> = -40..=150;
const POWER_W: std::ops::RangeInclusive<i32> = 0..=1000;
const FAN_RPM: std::ops::RangeInclusive<i32> = 0..=20_000;
const CLOCK_MHZ: std::ops::RangeInclusive<i32> = 0..=10_000;
const ACTIVITY_PCT: std::ops::RangeInclusive<i32> = 0..=100;

/// One PMLog readout, reduced to the quantities all-smi displays.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct AdlReadout {
    /// Edge (junction-adjacent) temperature in Celsius.
    ///
    /// Signed: a cold-started machine genuinely reports below zero.
    pub temperature_edge_c: Option<i32>,
    /// Graphics-die temperature in Celsius. The RDNA-era stand-in for
    /// edge on parts that do not publish `TEMPERATURE_EDGE`.
    pub temperature_gfx_c: Option<i32>,
    /// Hotspot / junction temperature in Celsius. Higher than edge on
    /// every modern part, and the number AMD's own tooling leads with.
    pub temperature_hotspot_c: Option<i32>,
    /// Memory (HBM / GDDR) temperature in Celsius.
    pub temperature_mem_c: Option<i32>,
    /// Board power in watts.
    pub power_w: Option<f64>,
    pub fan_rpm: Option<u32>,
    pub clock_gfx_mhz: Option<u32>,
    pub clock_mem_mhz: Option<u32>,
    /// Graphics engine activity, 0..=100.
    pub activity_gfx_pct: Option<f64>,
    /// Memory controller activity, 0..=100.
    pub activity_mem_pct: Option<f64>,
}

impl AdlReadout {
    /// Whether anything at all was extracted.
    ///
    /// A readout where every field failed its range check is treated as
    /// no readout, so the caller keeps the baseline rather than
    /// advertising an ADL source that produced nothing.
    pub fn is_empty(&self) -> bool {
        self.temperature_edge_c.is_none()
            && self.temperature_gfx_c.is_none()
            && self.temperature_hotspot_c.is_none()
            && self.temperature_mem_c.is_none()
            && self.power_w.is_none()
            && self.fan_rpm.is_none()
            && self.clock_gfx_mhz.is_none()
            && self.clock_mem_mhz.is_none()
            && self.activity_gfx_pct.is_none()
            && self.activity_mem_pct.is_none()
    }

    /// The temperature to report as *the* GPU temperature, and which
    /// sensor it came from.
    ///
    /// Edge is preferred because it is what `nvidia-smi` and the Linux
    /// `amdgpu` reader report, so the number stays comparable across the
    /// platforms all-smi aggregates in one view. `TEMPERATURE_GFX` is
    /// the next choice: it measures the same die and sits in the same
    /// range, and RDNA-era parts that omit edge publish it instead.
    ///
    /// Hotspot is the last resort and is deliberately *named* in the
    /// return value, because it runs 15-30 C above edge on modern parts.
    /// Falling back to it silently would mix two different quantities in
    /// an aggregated multi-host view with nothing to distinguish them;
    /// the caller uses the label to say which sensor it published.
    pub fn primary_temperature_c(&self) -> Option<(i32, &'static str)> {
        if let Some(edge) = self.temperature_edge_c {
            return Some((edge, "ADL (edge)"));
        }
        if let Some(gfx) = self.temperature_gfx_c {
            return Some((gfx, "ADL (gfx)"));
        }
        self.temperature_hotspot_c
            .map(|hotspot| (hotspot, "ADL (hotspot)"))
    }
}

/// Read a sensor entry, returning `None` unless the card marks it
/// supported and the value falls inside `range`.
fn sensor(
    output: &AdlPmLogDataOutput,
    index: usize,
    range: std::ops::RangeInclusive<i32>,
) -> Option<i32> {
    let entry = output.sensors.get(index)?;
    if entry.supported == 0 {
        return None;
    }
    range.contains(&entry.value).then_some(entry.value)
}

/// Reduce a raw PMLog table to the quantities all-smi displays.
pub fn extract(output: &AdlPmLogDataOutput) -> AdlReadout {
    AdlReadout {
        temperature_edge_c: sensor(output, PMLOG_TEMPERATURE_EDGE, TEMPERATURE_C),
        temperature_gfx_c: sensor(output, PMLOG_TEMPERATURE_GFX, TEMPERATURE_C),
        temperature_hotspot_c: sensor(output, PMLOG_TEMPERATURE_HOTSPOT, TEMPERATURE_C),
        temperature_mem_c: sensor(output, PMLOG_TEMPERATURE_MEM, TEMPERATURE_C),
        // Widest domain first. BOARD_POWER is the whole-card figure and
        // is what RDNA3 and later publish; ASIC_POWER is a narrower
        // domain on those parts even though it still reports supported,
        // and is the correct answer on everything older; GFX_POWER
        // covers the graphics block alone and is the last resort.
        //
        // Preferring whatever the card actually publishes avoids needing
        // the GCN family lookup that a family-keyed rule would require,
        // and no range check could distinguish these three: all are
        // watts and all are plausible.
        power_w: sensor(output, PMLOG_BOARD_POWER, POWER_W)
            .or_else(|| sensor(output, PMLOG_ASIC_POWER, POWER_W))
            .or_else(|| sensor(output, PMLOG_GFX_POWER, POWER_W))
            .map(|v| v as f64),
        fan_rpm: sensor(output, PMLOG_FAN_RPM, FAN_RPM).map(|v| v as u32),
        clock_gfx_mhz: sensor(output, PMLOG_CLK_GFXCLK, CLOCK_MHZ).map(|v| v as u32),
        clock_mem_mhz: sensor(output, PMLOG_CLK_MEMCLK, CLOCK_MHZ).map(|v| v as u32),
        activity_gfx_pct: sensor(output, PMLOG_INFO_ACTIVITY_GFX, ACTIVITY_PCT).map(|v| v as f64),
        activity_mem_pct: sensor(output, PMLOG_INFO_ACTIVITY_MEM, ACTIVITY_PCT).map(|v| v as f64),
    }
}

/// Every supported sensor as `(index, raw value)`, for the doctor dump.
///
/// Deliberately unfiltered and unnamed: the point is to show what the
/// card actually publishes so a field report can confirm or correct the
/// index mapping above without a code change.
pub fn supported_raw(output: &AdlPmLogDataOutput) -> Vec<(usize, i32)> {
    output
        .sensors
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.supported != 0)
        .map(|(index, entry)| (index, entry.value))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::super::ffi::AdlPmLogDataOutput;
    use super::*;

    fn table(entries: &[(usize, i32)]) -> AdlPmLogDataOutput {
        let mut output = AdlPmLogDataOutput::default();
        for (index, value) in entries {
            output.sensors[*index].supported = 1;
            output.sensors[*index].value = *value;
        }
        output
    }

    #[test]
    fn extracts_a_realistic_rdna_readout() {
        let output = table(&[
            (PMLOG_CLK_GFXCLK, 2400),
            (PMLOG_CLK_MEMCLK, 1250),
            (PMLOG_TEMPERATURE_EDGE, 62),
            (PMLOG_TEMPERATURE_MEM, 70),
            (PMLOG_TEMPERATURE_HOTSPOT, 81),
            (PMLOG_FAN_RPM, 1450),
            (PMLOG_ASIC_POWER, 310),
            (PMLOG_INFO_ACTIVITY_GFX, 97),
            (PMLOG_INFO_ACTIVITY_MEM, 44),
        ]);
        let readout = extract(&output);

        assert_eq!(readout.temperature_edge_c, Some(62));
        assert_eq!(readout.temperature_hotspot_c, Some(81));
        assert_eq!(readout.temperature_mem_c, Some(70));
        assert_eq!(readout.power_w, Some(310.0));
        assert_eq!(readout.fan_rpm, Some(1450));
        assert_eq!(readout.clock_gfx_mhz, Some(2400));
        assert_eq!(readout.clock_mem_mhz, Some(1250));
        assert_eq!(readout.activity_gfx_pct, Some(97.0));
        assert_eq!(readout.activity_mem_pct, Some(44.0));
        assert!(!readout.is_empty());
        // Edge, not hotspot, so the figure stays comparable with the
        // Linux amdgpu reader and with nvidia-smi, and the label says
        // which sensor it came from.
        assert_eq!(readout.primary_temperature_c(), Some((62, "ADL (edge)")));
    }

    #[test]
    fn unsupported_sensors_are_absent_not_zero() {
        // A card that publishes nothing must yield no readout at all,
        // rather than a table of confident zeroes.
        let readout = extract(&AdlPmLogDataOutput::default());
        assert!(readout.is_empty());
        assert_eq!(readout.primary_temperature_c(), None);
        assert_eq!(readout.power_w, None);
    }

    #[test]
    fn a_supported_flag_with_an_absurd_value_is_rejected() {
        // This is the misindexing guard. If the enum shifted and we read
        // a clock where a temperature should be, the value lands far
        // outside the band and must be dropped rather than reported as a
        // 2400 degree GPU.
        let output = table(&[
            (PMLOG_TEMPERATURE_EDGE, 2400),
            (PMLOG_ASIC_POWER, 99_999),
            (PMLOG_FAN_RPM, 250_000),
            (PMLOG_INFO_ACTIVITY_GFX, 5_000),
            (PMLOG_CLK_GFXCLK, 1_000_000),
        ]);
        let readout = extract(&output);
        assert!(readout.is_empty(), "got {readout:?}");
    }

    #[test]
    fn negative_power_and_activity_are_rejected_but_cold_dies_are_not() {
        // Power and activity have no meaningful negative value, so a
        // negative one is a bad read.
        let nonsense = table(&[(PMLOG_ASIC_POWER, -1), (PMLOG_INFO_ACTIVITY_GFX, -3)]);
        assert!(extract(&nonsense).is_empty());

        // A sub-zero die is real on a machine cold-started in an
        // unheated room, and must survive the guard.
        let cold = table(&[(PMLOG_TEMPERATURE_EDGE, -12)]);
        assert_eq!(extract(&cold).temperature_edge_c, Some(-12));

        // Far enough below ambient to be a bad read rather than a cold
        // room.
        let absurd = table(&[(PMLOG_TEMPERATURE_EDGE, -273)]);
        assert!(extract(&absurd).is_empty());
    }

    #[test]
    fn board_power_outranks_asic_power() {
        // On RDNA3 and later both report supported, but only
        // BOARD_POWER is the whole-card figure. No range check can tell
        // them apart, so the preference order is the only defence.
        let both = table(&[
            (PMLOG_BOARD_POWER, 355),
            (PMLOG_ASIC_POWER, 290),
            (PMLOG_GFX_POWER, 180),
        ]);
        assert_eq!(extract(&both).power_w, Some(355.0));

        // Older parts publish no BOARD_POWER and must still work.
        let older = table(&[(PMLOG_ASIC_POWER, 290), (PMLOG_GFX_POWER, 180)]);
        assert_eq!(extract(&older).power_w, Some(290.0));
    }

    #[test]
    fn asic_power_wins_over_gfx_power_but_gfx_is_the_fallback() {
        let both = table(&[(PMLOG_ASIC_POWER, 300), (PMLOG_GFX_POWER, 180)]);
        assert_eq!(extract(&both).power_w, Some(300.0));

        let gfx_only = table(&[(PMLOG_GFX_POWER, 180)]);
        assert_eq!(extract(&gfx_only).power_w, Some(180.0));

        // An out-of-band ASIC reading must fall through to GFX rather
        // than poisoning the field.
        let asic_bogus = table(&[(PMLOG_ASIC_POWER, 50_000), (PMLOG_GFX_POWER, 180)]);
        assert_eq!(extract(&asic_bogus).power_w, Some(180.0));
    }

    #[test]
    fn gfx_then_hotspot_stand_in_when_edge_is_unavailable() {
        // GFX measures the same die as edge and is preferred.
        let gfx = table(&[(PMLOG_TEMPERATURE_GFX, 64), (PMLOG_TEMPERATURE_HOTSPOT, 88)]);
        assert_eq!(
            extract(&gfx).primary_temperature_c(),
            Some((64, "ADL (gfx)"))
        );

        // Hotspot is the last resort and must be labelled as such:
        // it runs well above edge, so an aggregated view needs to be
        // able to tell the two apart.
        let hotspot = table(&[(PMLOG_TEMPERATURE_HOTSPOT, 88)]);
        assert_eq!(
            extract(&hotspot).primary_temperature_c(),
            Some((88, "ADL (hotspot)"))
        );
    }

    #[test]
    fn the_raw_dump_reports_index_and_value_unfiltered() {
        // The doctor dump must not apply the range guard: an
        // out-of-band value is exactly the evidence needed to spot a
        // wrong index mapping from a field report.
        let output = table(&[(3, 1234), (27, 81), (200, -5)]);
        let dumped = supported_raw(&output);
        assert_eq!(dumped, vec![(3, 1234), (27, 81), (200, -5)]);
    }

    #[test]
    fn boundary_values_are_inclusive() {
        let output = table(&[
            (PMLOG_TEMPERATURE_EDGE, 150),
            (PMLOG_INFO_ACTIVITY_GFX, 100),
            (PMLOG_FAN_RPM, 0),
        ]);
        let readout = extract(&output);
        assert_eq!(readout.temperature_edge_c, Some(150));
        assert_eq!(readout.activity_gfx_pct, Some(100.0));
        // Zero RPM is a real reading on a passively-idling card, not an
        // absent one.
        assert_eq!(readout.fan_rpm, Some(0));
    }
}
