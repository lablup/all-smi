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

//! Scheduling for the Windows CPU temperature fallback chain.
//!
//! [`ChainState::run_chain`] walks the sources in priority order and stops at
//! the first reading. A source that reports `NotFound` is dropped for the
//! process lifetime; one that keeps answering `Error` or `NoValidReading` is
//! backed off instead of being queried on every tick (issue #501). The query
//! itself is a closure and the clock is injected, so the policy is tested on
//! every runner; `windows_temp` supplies the WMI-backed queries.

use std::time::{Duration, Instant};

/// Consecutive transient failures before a source is first backed off.
pub const FAILURES_BEFORE_BACKOFF: u32 = 3;
/// First backoff window.
pub const INITIAL_BACKOFF: Duration = Duration::from_secs(30);
/// Longest backoff window; each further failure doubles the window up to it.
pub const MAX_BACKOFF: Duration = Duration::from_secs(300);

/// Detected CPU vendor, which selects the vendor-specific sources.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuVendor {
    Intel,
    Amd,
    Unknown,
}

/// One entry in the temperature fallback chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TempSource {
    Acpi,
    AmdRyzen,
    IntelWmi,
    LibreHardwareMonitor,
    ThermalZoneInfo,
}

impl TempSource {
    const COUNT: usize = 5;

    fn index(self) -> usize {
        match self {
            TempSource::Acpi => 0,
            TempSource::AmdRyzen => 1,
            TempSource::IntelWmi => 2,
            TempSource::LibreHardwareMonitor => 3,
            TempSource::ThermalZoneInfo => 4,
        }
    }
}

/// Result of attempting to read temperature from a source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemperatureResult {
    /// Successfully read temperature (in Celsius)
    Success(u32),
    /// Source permanently not found (e.g., WMI class doesn't exist)
    NotFound,
    /// Transient error (connection issue, etc.)
    Error,
    /// Source exists but returned no valid reading
    NoValidReading,
}

/// The sources to try, in priority order, for a CPU vendor.
///
/// The CPU package sensors (vendor SDKs, LibreHardwareMonitor) come before
/// the ThermalZoneInformation counters, which report ACPI zones and answer
/// on almost every machine; trying them earlier would hide the package
/// temperature on Intel and AMD hosts (issue #459).
pub fn source_order(vendor: CpuVendor) -> Vec<TempSource> {
    let mut order = vec![TempSource::Acpi];
    match vendor {
        CpuVendor::Amd => order.push(TempSource::AmdRyzen),
        CpuVendor::Intel => order.push(TempSource::IntelWmi),
        CpuVendor::Unknown => {}
    }
    order.push(TempSource::LibreHardwareMonitor);
    order.push(TempSource::ThermalZoneInfo);
    order
}

/// Per-source scheduling state.
#[derive(Debug, Clone, Copy, Default)]
struct SourceState {
    /// `NotFound` was reported: never query again.
    unavailable: bool,
    /// Transient failures since the last success or backoff.
    consecutive_failures: u32,
    /// Current backoff window, `None` until the source is first backed off.
    backoff: Option<Duration>,
    /// Do not query before this instant.
    skip_until: Option<Instant>,
}

impl SourceState {
    fn should_query(&self, now: Instant) -> bool {
        !self.unavailable && self.skip_until.is_none_or(|until| now >= until)
    }

    fn record_failure(&mut self, now: Instant) {
        self.consecutive_failures += 1;
        // Once backed off, one more failure re-arms a doubled window; a
        // source that has never been backed off gets three tries first.
        let window = match self.backoff {
            Some(window) => (window * 2).min(MAX_BACKOFF),
            None if self.consecutive_failures >= FAILURES_BEFORE_BACKOFF => INITIAL_BACKOFF,
            None => return,
        };
        self.backoff = Some(window);
        self.skip_until = Some(now + window);
        self.consecutive_failures = 0;
    }
}

/// Availability and backoff state for every source in the chain.
#[derive(Debug, Clone, Default)]
pub struct ChainState {
    sources: [SourceState; TempSource::COUNT],
}

impl ChainState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Query `order` once, highest priority first, and return the first
    /// reading. Each eligible source is queried at most once per call.
    pub fn run_chain(
        &mut self,
        order: &[TempSource],
        now: Instant,
        mut query: impl FnMut(TempSource) -> TemperatureResult,
    ) -> Option<u32> {
        for &source in order {
            let state = &mut self.sources[source.index()];
            if !state.should_query(now) {
                continue;
            }
            match query(source) {
                TemperatureResult::Success(temp) => {
                    *state = SourceState::default();
                    return Some(temp);
                }
                TemperatureResult::NotFound => state.unavailable = true,
                TemperatureResult::Error | TemperatureResult::NoValidReading => {
                    state.record_failure(now);
                }
            }
        }
        None
    }

    /// Whether `source` has been marked permanently unavailable.
    #[cfg(test)]
    pub fn is_unavailable(&self, source: TempSource) -> bool {
        self.sources[source.index()].unavailable
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use TempSource::*;
    use std::collections::HashMap;

    #[test]
    fn thermal_zone_info_is_tried_after_every_cpu_sensor() {
        assert_eq!(
            source_order(CpuVendor::Intel),
            [Acpi, IntelWmi, LibreHardwareMonitor, ThermalZoneInfo]
        );
        assert_eq!(
            source_order(CpuVendor::Amd),
            [Acpi, AmdRyzen, LibreHardwareMonitor, ThermalZoneInfo]
        );
        // RTX Spark N1X (Arm brand string): no vendor SDK, and the
        // unelevated thermal-zone counters are the source that answers.
        assert_eq!(
            source_order(CpuVendor::Unknown),
            [Acpi, LibreHardwareMonitor, ThermalZoneInfo]
        );
    }

    /// Runs the chain with a closure that counts queries per source.
    struct Harness {
        chain: ChainState,
        order: Vec<TempSource>,
        calls: HashMap<&'static str, u32>,
        t0: Instant,
    }

    fn name(source: TempSource) -> &'static str {
        match source {
            Acpi => "acpi",
            AmdRyzen => "amd",
            IntelWmi => "intel",
            LibreHardwareMonitor => "lhm",
            ThermalZoneInfo => "tzi",
        }
    }

    impl Harness {
        fn new(vendor: CpuVendor) -> Self {
            Self {
                chain: ChainState::new(),
                order: source_order(vendor),
                calls: HashMap::new(),
                t0: Instant::now(),
            }
        }

        fn run(
            &mut self,
            at_secs: u64,
            answer: impl Fn(TempSource) -> TemperatureResult,
        ) -> Option<u32> {
            let calls = &mut self.calls;
            self.chain
                .run_chain(&self.order, self.t0 + Duration::from_secs(at_secs), |s| {
                    *calls.entry(name(s)).or_default() += 1;
                    answer(s)
                })
        }

        fn calls(&self, source: TempSource) -> u32 {
            self.calls.get(name(source)).copied().unwrap_or(0)
        }
    }

    fn no_reading(_: TempSource) -> TemperatureResult {
        TemperatureResult::NoValidReading
    }

    #[test]
    fn each_source_is_queried_once_per_run() {
        let mut h = Harness::new(CpuVendor::Intel);
        assert_eq!(h.run(0, no_reading), None);
        for source in [Acpi, IntelWmi, LibreHardwareMonitor, ThermalZoneInfo] {
            assert_eq!(h.calls(source), 1, "{source:?}");
        }
        assert_eq!(h.calls(AmdRyzen), 0, "not in the Intel order");
    }

    #[test]
    fn repeated_failures_back_off_and_the_window_doubles_to_the_cap() {
        let mut h = Harness::new(CpuVendor::Unknown);
        for tick in 0..3 {
            h.run(tick, no_reading);
        }
        assert_eq!(h.calls(Acpi), 3);

        // Three failures at t=0..2: skipped until t=2+30.
        for tick in 3..32 {
            h.run(tick, no_reading);
        }
        assert_eq!(h.calls(Acpi), 3, "queried inside the first window");
        h.run(32, no_reading);
        assert_eq!(h.calls(Acpi), 4, "window elapsed");

        // One failure after a backoff doubles the window: 60, 120, 240, 300.
        let mut at = 32;
        for window in [60, 120, 240, 300, 300] {
            let before = h.calls(Acpi);
            h.run(at + window - 1, no_reading);
            assert_eq!(h.calls(Acpi), before, "inside the {window}s window");
            at += window;
            h.run(at, no_reading);
            assert_eq!(h.calls(Acpi), before + 1, "window {window}s");
        }
    }

    #[test]
    fn success_stops_the_chain_and_resets_backoff() {
        let mut h = Harness::new(CpuVendor::Unknown);
        let lhm_answers = |s: TempSource| match s {
            LibreHardwareMonitor => TemperatureResult::Success(55),
            _ => TemperatureResult::NoValidReading,
        };
        // Second source fails three times and is backed off.
        for tick in 0..3 {
            assert_eq!(h.run(tick, no_reading), None);
        }
        assert_eq!(h.run(10, lhm_answers), None, "LHM is backed off");
        assert_eq!(h.calls(ThermalZoneInfo), 3);

        assert_eq!(h.run(40, lhm_answers), Some(55));
        assert_eq!(h.calls(ThermalZoneInfo), 3, "chain stopped at LHM");
        // The success reset LHM: two more failures do not back it off.
        h.run(41, no_reading);
        h.run(42, no_reading);
        assert_eq!(h.run(43, lhm_answers), Some(55));
        assert_eq!(h.calls(LibreHardwareMonitor), 7);
    }

    #[test]
    fn not_found_is_permanent() {
        let mut h = Harness::new(CpuVendor::Unknown);
        let acpi_missing = |s: TempSource| match s {
            Acpi => TemperatureResult::NotFound,
            ThermalZoneInfo => TemperatureResult::Success(40),
            _ => TemperatureResult::Error,
        };
        for tick in 0..1000 {
            assert_eq!(h.run(tick, acpi_missing), Some(40));
        }
        assert_eq!(h.calls(Acpi), 1);
        assert!(h.chain.is_unavailable(Acpi));
        assert!(!h.chain.is_unavailable(LibreHardwareMonitor));
    }
}
