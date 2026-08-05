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

//! Intel Mac CPU hardware discovery
//!
//! Collects the immutable description of an Intel Mac's CPU: model, socket,
//! core and thread counts, clocks, and cache size.
//!
//! sysctl is the primary source because it costs a few milliseconds.
//! `system_profiler SPHardwareDataType` costs roughly half a second and is
//! kept only as a gap filler, since it is the sole source for the L3 cache
//! size and the marketing processor name on models where the corresponding
//! sysctl keys are missing.

use crate::device::common::command_executor::{CommandOptions, execute_command};
use crate::device::common::execute_command_default;
use std::collections::HashMap;
use std::time::Duration;

/// Absolute paths to the macOS system binaries this module shells out to.
///
/// Resolving these through `PATH` would let a writable directory earlier in
/// `PATH` than `/usr/sbin` substitute a different binary. `sudo` on macOS does
/// not set `secure_path`, so the invoking user's `PATH` survives into an
/// elevated process. Both binaries live at fixed, SIP-protected locations, so
/// naming them outright costs no portability.
const SYSCTL_BIN: &str = "/usr/sbin/sysctl";
const SYSTEM_PROFILER_BIN: &str = "/usr/sbin/system_profiler";

/// `system_profiler SPHardwareDataType` legitimately takes roughly half a
/// second, which does not comfortably fit the shared fast-fail budget (2s on
/// bare metal, 500ms containerized). It runs at most once per process and only
/// on the gap-filling path, so it gets its own more generous ceiling.
const SYSTEM_PROFILER_TIMEOUT: Duration = Duration::from_secs(5);

/// Hardware facts about an Intel Mac CPU. These never change while the process
/// runs, so the caller collects them once and caches them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IntelCpuHardware {
    /// Brand or marketing string, e.g. "Intel(R) Core(TM) i9-9880H CPU @ 2.30GHz".
    pub model: String,
    /// Physical CPU packages (sockets). Never zero after collection.
    pub socket_count: u32,
    /// Physical cores across all sockets.
    pub physical_cores: u32,
    /// Logical cores (threads) across all sockets. Equals `physical_cores` on
    /// models without hyperthreading.
    pub logical_cores: u32,
    /// Nominal base frequency in MHz, 0 when unknown.
    pub base_frequency_mhz: u32,
    /// Maximum turbo frequency in MHz, 0 when unknown.
    pub max_frequency_mhz: u32,
    /// L3 cache size in MB, 0 when unknown.
    pub l3_cache_mb: u32,
}

impl IntelCpuHardware {
    /// Collect the CPU description, consulting `system_profiler` only when
    /// sysctl left a gap.
    ///
    /// `logical_cpu_fallback` is the kernel's logical CPU count as seen by
    /// sysinfo, used only if `hw.logicalcpu` could not be read. Passing a real
    /// count is how the thread total stays honest without reintroducing the
    /// old `cores * 2` hyperthreading assumption.
    pub fn collect(logical_cpu_fallback: u32) -> Self {
        let mut hardware = Self::from_sysctl();

        if hardware.has_gaps()
            && let Some(fallback) = Self::from_system_profiler()
        {
            hardware.fill_gaps_from(&fallback);
        }

        if hardware.logical_cores == 0 {
            hardware.logical_cores = logical_cpu_fallback;
        }

        hardware.apply_defaults();
        hardware
    }

    /// Query the CPU description from sysctl in a single call.
    fn from_sysctl() -> Self {
        // `hw.cpufrequency` and `hw.cpufrequency_max` were removed on some
        // later Intel Macs. sysctl reports unknown keys on stderr and keeps
        // going, so the exit status is deliberately ignored and only the keys
        // that did come back are parsed.
        // Routed through `execute_command_default` rather than a bare
        // `Command::output()` so the call inherits the shared timeout, the
        // kill-child-on-timeout behavior, and the 16 MiB output cap. A bare
        // `output()` waits forever and reads to EOF, and this runs on a
        // `spawn_blocking` worker that Tokio cannot cancel, so a wedged child
        // would strand that worker for the life of the process.
        let stdout = execute_command_default(
            SYSCTL_BIN,
            &[
                "machdep.cpu.brand_string",
                "hw.packages",
                "hw.physicalcpu",
                "hw.logicalcpu",
                "hw.cpufrequency",
                "hw.cpufrequency_max",
                "hw.l3cachesize",
            ],
        )
        .map(|output| output.stdout)
        .unwrap_or_default();

        let values = parse_sysctl_pairs(&stdout);
        let read_u64 = |key: &str| values.get(key).and_then(|v| v.parse::<u64>().ok());

        let model = values
            .get("machdep.cpu.brand_string")
            .map(|v| (*v).to_string())
            .unwrap_or_default();

        // `hw.cpufrequency` is the nominal base frequency in Hz. When the key
        // is gone, the brand string still carries the nominal clock.
        let base_frequency_mhz = read_u64("hw.cpufrequency")
            .map(hz_to_mhz)
            .filter(|mhz| *mhz > 0)
            .or_else(|| parse_frequency_from_brand_string(&model))
            .unwrap_or(0);

        Self {
            model,
            socket_count: read_u64("hw.packages").unwrap_or(0) as u32,
            physical_cores: read_u64("hw.physicalcpu").unwrap_or(0) as u32,
            logical_cores: read_u64("hw.logicalcpu").unwrap_or(0) as u32,
            base_frequency_mhz,
            max_frequency_mhz: read_u64("hw.cpufrequency_max").map(hz_to_mhz).unwrap_or(0),
            l3_cache_mb: read_u64("hw.l3cachesize")
                .map(|bytes| (bytes / 1024 / 1024) as u32)
                .unwrap_or(0),
        }
    }

    /// Fall back to `system_profiler SPHardwareDataType` for the fields sysctl
    /// cannot supply on every model.
    fn from_system_profiler() -> Option<Self> {
        // Bounded for the same reason as the sysctl call above. This one is the
        // likelier of the two to wedge: `system_profiler` queries IOKit, which
        // can block on unresponsive hardware.
        let output = execute_command(
            SYSTEM_PROFILER_BIN,
            &["SPHardwareDataType"],
            &CommandOptions {
                timeout: Some(SYSTEM_PROFILER_TIMEOUT),
                check_status: false,
            },
        )
        .ok()?;

        Some(parse_system_profiler_hardware(&output.stdout))
    }

    /// True when sysctl left a field that `system_profiler` can still supply.
    fn has_gaps(&self) -> bool {
        self.model.is_empty()
            || self.physical_cores == 0
            || self.base_frequency_mhz == 0
            || self.l3_cache_mb == 0
    }

    /// Copy any field this record is missing from `other`.
    fn fill_gaps_from(&mut self, other: &Self) {
        if self.model.is_empty() {
            self.model = other.model.clone();
        }
        if self.socket_count == 0 {
            self.socket_count = other.socket_count;
        }
        if self.physical_cores == 0 {
            self.physical_cores = other.physical_cores;
        }
        if self.logical_cores == 0 {
            self.logical_cores = other.logical_cores;
        }
        if self.base_frequency_mhz == 0 {
            self.base_frequency_mhz = other.base_frequency_mhz;
        }
        if self.max_frequency_mhz == 0 {
            self.max_frequency_mhz = other.max_frequency_mhz;
        }
        if self.l3_cache_mb == 0 {
            self.l3_cache_mb = other.l3_cache_mb;
        }
    }

    /// Apply the last-resort defaults that keep downstream arithmetic safe.
    ///
    /// `socket_count` in particular is used as a divisor when building
    /// per-socket records, so it must never be zero.
    fn apply_defaults(&mut self) {
        if self.model.is_empty() {
            self.model = "Unknown Intel CPU".to_string();
        }
        if self.socket_count == 0 {
            self.socket_count = 1;
        }
        if self.physical_cores == 0 {
            self.physical_cores = self.logical_cores.max(1);
        }
        if self.logical_cores == 0 {
            // No hyperthreading assumption: without a reading, one thread per
            // physical core is the only defensible answer.
            self.logical_cores = self.physical_cores;
        }
        if self.max_frequency_mhz == 0 {
            self.max_frequency_mhz = self.base_frequency_mhz;
        }
    }
}

/// Parse the `key: value` block that `sysctl` prints for multiple keys.
///
/// Keys sysctl could not resolve are reported on stderr and simply do not
/// appear here, which is how optional keys such as `hw.cpufrequency` are
/// distinguished from present ones. Values may themselves contain colons, so
/// only the first separator is significant.
fn parse_sysctl_pairs(output: &str) -> HashMap<&str, &str> {
    output
        .lines()
        .filter_map(|line| line.split_once(": "))
        .map(|(key, value)| (key.trim(), value.trim()))
        .collect()
}

/// Convert a frequency in Hz to MHz, rounding to nearest.
fn hz_to_mhz(hz: u64) -> u32 {
    ((hz + 500_000) / 1_000_000) as u32
}

/// Extract the nominal clock from an Intel brand string.
///
/// Brand strings end with the nominal clock, for example
/// "Intel(R) Core(TM) i9-9880H CPU @ 2.30GHz". This is the fallback for the
/// Intel Macs where `hw.cpufrequency` no longer exists.
fn parse_frequency_from_brand_string(brand: &str) -> Option<u32> {
    let tail = brand.rsplit_once('@')?.1.trim();

    let (number, unit) = match tail.strip_suffix("GHz") {
        Some(rest) => (rest, 1000.0),
        None => (tail.strip_suffix("MHz")?, 1.0),
    };

    let value = number.trim().parse::<f64>().ok()?;
    if !value.is_finite() || value <= 0.0 {
        return None;
    }

    Some((value * unit).round() as u32)
}

/// Parse `system_profiler SPHardwareDataType` output into hardware facts.
///
/// Notably this no longer derives the thread count from the core count:
/// `system_profiler` does not report logical CPUs, and the old `cores * 2`
/// hyperthreading assumption was wrong on every model without hyperthreading.
fn parse_system_profiler_hardware(hardware_info: &str) -> IntelCpuHardware {
    let mut hardware = IntelCpuHardware::default();

    for line in hardware_info.lines() {
        let line = line.trim();
        if line.starts_with("Processor Name:") {
            hardware.model = line.split(':').nth(1).unwrap_or("").trim().to_string();
        } else if line.starts_with("Processor Speed:") {
            if let Some(ghz) = crate::parse_colon_value!(line, f64) {
                hardware.base_frequency_mhz = (ghz * 1000.0) as u32;
            }
        } else if line.starts_with("Number of Processors:") {
            if let Some(procs) = crate::parse_colon_value!(line, u32) {
                hardware.socket_count = procs;
            }
        } else if line.starts_with("Total Number of Cores:") {
            if let Some(cores) = crate::parse_colon_value!(line, u32) {
                hardware.physical_cores = cores;
            }
        } else if line.starts_with("L3 Cache:")
            && let Some(size) = crate::parse_colon_value!(line, u32)
        {
            hardware.l3_cache_mb = size;
        }
    }

    hardware
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sysctl_key_value_block() {
        let output = "machdep.cpu.brand_string: Intel(R) Core(TM) i9-9880H CPU @ 2.30GHz\n\
                      hw.packages: 1\n\
                      hw.physicalcpu: 8\n\
                      hw.logicalcpu: 16\n";

        let values = parse_sysctl_pairs(output);
        assert_eq!(
            values.get("machdep.cpu.brand_string").copied(),
            Some("Intel(R) Core(TM) i9-9880H CPU @ 2.30GHz")
        );
        assert_eq!(values.get("hw.physicalcpu").copied(), Some("8"));
        assert_eq!(values.get("hw.logicalcpu").copied(), Some("16"));
        // Keys sysctl could not resolve are absent rather than empty.
        assert!(!values.contains_key("hw.cpufrequency"));
    }

    #[test]
    fn parses_frequency_from_intel_brand_strings() {
        assert_eq!(
            parse_frequency_from_brand_string("Intel(R) Core(TM) i9-9880H CPU @ 2.30GHz"),
            Some(2300)
        );
        assert_eq!(
            parse_frequency_from_brand_string("Intel(R) Core(TM) i5-8210Y CPU @ 1.60GHz"),
            Some(1600)
        );
        assert_eq!(
            parse_frequency_from_brand_string("Some CPU @ 800MHz"),
            Some(800)
        );
        // Apple Silicon brand strings carry no clock at all.
        assert_eq!(parse_frequency_from_brand_string("Apple M2 Pro"), None);
        assert_eq!(parse_frequency_from_brand_string(""), None);
        assert_eq!(
            parse_frequency_from_brand_string("Weird CPU @ fastGHz"),
            None
        );
    }

    #[test]
    fn converts_hz_to_mhz_by_rounding() {
        assert_eq!(hz_to_mhz(2_300_000_000), 2300);
        assert_eq!(hz_to_mhz(2_299_800_000), 2300);
        assert_eq!(hz_to_mhz(0), 0);
    }

    /// The system_profiler fallback must not invent threads. The old parser
    /// set `threads = cores * 2`, which reported 16 threads on an 8-core
    /// non-hyperthreaded part.
    #[test]
    fn system_profiler_fallback_does_not_assume_hyperthreading() {
        let output = "Hardware Overview:\n\
                      \n\
                      Model Name: MacBook Pro\n\
                      Processor Name: 8-Core Intel Core i9\n\
                      Processor Speed: 2.3 GHz\n\
                      Number of Processors: 1\n\
                      Total Number of Cores: 8\n\
                      L2 Cache (per Core): 256 KB\n\
                      L3 Cache: 16 MB\n";

        let hardware = parse_system_profiler_hardware(output);
        assert_eq!(hardware.model, "8-Core Intel Core i9");
        assert_eq!(hardware.socket_count, 1);
        assert_eq!(hardware.physical_cores, 8);
        assert_eq!(hardware.base_frequency_mhz, 2300);
        assert_eq!(hardware.l3_cache_mb, 16);
        assert_eq!(
            hardware.logical_cores, 0,
            "system_profiler cannot report logical CPUs, so it must leave the field unset"
        );
    }

    /// sysctl is authoritative; system_profiler only fills what is missing.
    #[test]
    fn sysctl_values_win_over_the_system_profiler_fallback() {
        let mut sysctl = IntelCpuHardware {
            model: "Intel(R) Core(TM) i9-9880H CPU @ 2.30GHz".to_string(),
            socket_count: 1,
            physical_cores: 8,
            logical_cores: 16,
            base_frequency_mhz: 2300,
            max_frequency_mhz: 4800,
            l3_cache_mb: 0,
        };
        let fallback = IntelCpuHardware {
            model: "8-Core Intel Core i9".to_string(),
            socket_count: 1,
            physical_cores: 8,
            logical_cores: 0,
            base_frequency_mhz: 2300,
            max_frequency_mhz: 0,
            l3_cache_mb: 16,
        };

        sysctl.fill_gaps_from(&fallback);

        assert_eq!(sysctl.model, "Intel(R) Core(TM) i9-9880H CPU @ 2.30GHz");
        assert_eq!(sysctl.logical_cores, 16);
        assert_eq!(sysctl.max_frequency_mhz, 4800);
        assert_eq!(sysctl.l3_cache_mb, 16, "the only gap should be filled");
    }

    /// Without a logical-CPU reading, threads must equal physical cores rather
    /// than doubling, and socket_count must never be a zero divisor.
    #[test]
    fn defaults_never_assume_hyperthreading_or_zero_sockets() {
        let mut hardware = IntelCpuHardware {
            physical_cores: 6,
            ..Default::default()
        };
        hardware.apply_defaults();

        assert_eq!(hardware.logical_cores, 6);
        assert_eq!(hardware.socket_count, 1);
        assert_eq!(hardware.model, "Unknown Intel CPU");
    }

    #[test]
    fn missing_max_frequency_falls_back_to_base() {
        let mut hardware = IntelCpuHardware {
            physical_cores: 4,
            logical_cores: 8,
            base_frequency_mhz: 2600,
            max_frequency_mhz: 0,
            ..Default::default()
        };
        hardware.apply_defaults();

        assert_eq!(hardware.max_frequency_mhz, 2600);
    }

    #[test]
    fn gap_detection_ignores_optional_fields() {
        let complete = IntelCpuHardware {
            model: "Intel(R) Core(TM) i7".to_string(),
            socket_count: 1,
            physical_cores: 4,
            logical_cores: 8,
            base_frequency_mhz: 2600,
            max_frequency_mhz: 0,
            l3_cache_mb: 8,
        };
        assert!(
            !complete.has_gaps(),
            "a missing turbo clock alone must not trigger the slow system_profiler path"
        );

        let missing_cache = IntelCpuHardware {
            l3_cache_mb: 0,
            ..complete
        };
        assert!(missing_cache.has_gaps());
    }

    /// `apply_defaults` alone must always yield a usable record even when
    /// every probe failed, because the caller divides by `socket_count`.
    ///
    /// This exercises the worst case directly on a fully empty record rather
    /// than going through `collect()`, which shells out to `sysctl` and,
    /// when there are gaps, `system_profiler`. `collect()`'s own logic (gap
    /// detection, gap filling, the sysctl/brand-string frequency fallback) is
    /// already covered by the other pure-logic tests in this module, so
    /// spawning real subprocesses here would only add wall-clock cost to the
    /// suite without covering anything new.
    #[test]
    fn apply_defaults_produces_safe_values_from_a_fully_empty_record() {
        let mut hardware = IntelCpuHardware::default();
        hardware.apply_defaults();

        assert!(!hardware.model.is_empty());
        assert!(hardware.socket_count >= 1);
        assert!(hardware.physical_cores >= 1);
        assert!(hardware.logical_cores >= 1);
        assert!(hardware.max_frequency_mhz >= hardware.base_frequency_mhz);
    }
}
