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

//! Energy accounting (issue #191).
//!
//! Integrates instantaneous power samples over time into cumulative Joule
//! counters per device / per chassis. The integrator runs in-memory during
//! a live session and optionally persists its running totals to a WAL so
//! Prometheus counters survive process restarts (see [`super::energy_wal`]).
//!
//! # Integration rule
//!
//! For each device we keep the last `(t_prev, p_prev)` sample. On a new
//! sample `(t_now, p_now)` the Joule increment is the trapezoidal rule:
//!
//! ```text
//! dJ = 0.5 * (p_prev + p_now) * (t_now - t_prev)
//! ```
//!
//! This is the exact linear-interpolation integral when power varies
//! linearly between samples — which is the right first-order assumption
//! for a 1-10 second polling cadence.
//!
//! # Gap handling
//!
//! - `dt <= 0`: ignored (clock stalled, duplicate sample).
//! - `dt <= gap_interpolate_seconds` (default 10s): trapezoidal rule
//!   above. The two observed endpoints ARE the linear interpolation
//!   across the gap, so no special case is needed.
//! - `dt > gap_interpolate_seconds`: hold last reading across the gap.
//!   A dropped sample burst is statistically more likely than an instant
//!   doubling of draw, so we prefer the conservative estimate
//!   `p_prev * dt` over an interpolation that could silently double-count
//!   a transient spike.
//! - Either power reading is `NaN`, infinite, or negative: that
//!   endpoint is replaced with `0.0` before the trapezoidal rule is
//!   applied. The window therefore contributes `0.5 * p_other * dt`
//!   (a linear glide toward zero from the adjacent valid reading)
//!   rather than a full zero. The timestamp still advances so the
//!   next sample gets a sensible `dt`. A sample above the
//!   [`MAX_POWER_WATTS`] ceiling is clamped to that ceiling so a
//!   bogus `f64::MAX` cannot overflow the accumulator to `+inf`.
//!
//! # Non-goals
//!
//! - No PSU efficiency model; the Joule counter is the integral of
//!   reported device power.
//! - No carbon-intensity mapping; that's downstream tooling's job.
//! - No historical store; Prometheus + the WAL are the memory.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::time::{Duration, Instant};

/// Conversion factor `J → kWh`. One kilowatt-hour is 3 600 000 Joules.
pub const JOULES_PER_KWH: f64 = 3_600_000.0;

/// Default gap-interpolation threshold in seconds. Matches the issue
/// spec: ≤ 10 s trapezoidal, > 10 s hold-last.
pub const DEFAULT_GAP_INTERPOLATE_SECONDS: u64 = 10;

/// Per-device upper bound for an instantaneous power reading, in watts.
///
/// A malicious or buggy driver can report a value like `f64::MAX` or
/// any unrealistically large number. Without a ceiling, a single such
/// sample multiplied by the elapsed `dt` would produce `+inf` (via
/// IEEE-754 overflow) and permanently poison the running `lifetime_joules`
/// counter. 100 kW is a comfortable order of magnitude above any real
/// single-chassis draw we can plausibly observe — it is roughly the
/// limit of a high-density AI rack — so anything above is treated as
/// a sensor-bug ceiling and clamped.
pub const MAX_POWER_WATTS: f64 = 100_000.0;

/// Scope tag for a single energy accumulator.
///
/// Used as part of the [`EnergyKey`] so the Prometheus exporter can emit
/// different label sets per scope (`gpu_index`+`gpu_uuid`, `scope="cpu"`,
/// `scope="chassis"`) and so the TUI top-consumer panel can rank scopes
/// independently.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EnergyScope {
    /// Per-GPU or per-NPU accumulator keyed on host + device UUID.
    Gpu,
    /// Per-CPU accumulator keyed on host (one CPU package per host).
    Cpu,
    /// Whole-chassis accumulator keyed on host (from
    /// `ChassisInfo::total_power_watts`).
    Chassis,
}

impl EnergyScope {
    /// Stable textual name used by the Prometheus exporter and the WAL
    /// hashing scheme.
    pub fn as_str(self) -> &'static str {
        match self {
            EnergyScope::Gpu => "gpu",
            EnergyScope::Cpu => "cpu",
            EnergyScope::Chassis => "chassis",
        }
    }
}

/// Fully-qualified key for an energy counter.
///
/// Chassis and CPU scopes leave `device` empty (the host name alone
/// identifies the counter).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct EnergyKey {
    pub scope: EnergyScope,
    pub host: String,
    /// Device identifier. For [`EnergyScope::Gpu`] this is the GPU UUID;
    /// for [`EnergyScope::Cpu`] / [`EnergyScope::Chassis`] it is an
    /// empty string.
    pub device: String,
}

impl EnergyKey {
    pub fn gpu(host: impl Into<String>, uuid: impl Into<String>) -> Self {
        Self {
            scope: EnergyScope::Gpu,
            host: host.into(),
            device: uuid.into(),
        }
    }

    pub fn cpu(host: impl Into<String>) -> Self {
        Self {
            scope: EnergyScope::Cpu,
            host: host.into(),
            device: String::new(),
        }
    }

    pub fn chassis(host: impl Into<String>) -> Self {
        Self {
            scope: EnergyScope::Chassis,
            host: host.into(),
            device: String::new(),
        }
    }

    /// Stable 64-bit hash of the host label for WAL records.
    ///
    /// Uses the standard library's default hasher. The WAL only needs
    /// *stability within a single binary build*: we replay records into
    /// a map keyed on the same hash at startup and match them against
    /// the live counter keys computed the same way. That is enough for
    /// Prometheus counter continuity across restarts.
    pub fn host_hash(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.host.hash(&mut hasher);
        hasher.finish()
    }

    /// Stable 64-bit hash of the `(scope, device)` pair for WAL records.
    ///
    /// The scope is included so `chassis` and `cpu` records for the
    /// same host do not collide with each other or with any hypothetical
    /// GPU whose UUID happens to hash to zero.
    pub fn device_hash(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.scope.as_str().hash(&mut hasher);
        self.device.hash(&mut hasher);
        hasher.finish()
    }
}

/// Per-device integrator state.
///
/// Holds the last observed sample so the next `record_sample` call can
/// compute the trapezoidal increment, plus the running Joule totals.
///
/// Two counters are tracked:
///
/// - `session_joules` — reset by the `R` hotkey; drives the TUI "Energy
///   session" row.
/// - `lifetime_joules` — never reset; drives the Prometheus counter so
///   `rate()` / `increase()` remain monotonic across `R` presses.
#[derive(Clone, Copy, Debug)]
struct DeviceState {
    last_sample: Option<Sample>,
    session_joules: f64,
    lifetime_joules: f64,
    /// Joule delta accumulated since the last WAL flush. Zeroed out each
    /// time [`EnergyAccountant::drain_wal_deltas`] is called.
    wal_pending_joules: f64,
}

#[derive(Clone, Copy, Debug)]
struct Sample {
    t: Instant,
    p: f64,
}

impl Default for DeviceState {
    fn default() -> Self {
        Self {
            last_sample: None,
            session_joules: 0.0,
            lifetime_joules: 0.0,
            wal_pending_joules: 0.0,
        }
    }
}

/// Per-integrator upper bound on the number of distinct
/// `(scope, host, device)` entries that will be tracked.
///
/// A hostname or UUID churn attack (e.g. a misconfigured collector that
/// keeps reporting fresh device IDs) can otherwise grow the internal
/// `HashMap` without bound, which in turn inflates the Prometheus
/// exporter and eats RSS until OOM. 10 000 entries covers a realistic
/// cluster ceiling (1 000 hosts × ~10 devices each) with room to spare,
/// and additional unique keys beyond that are silently dropped rather
/// than allowed to accumulate.
pub const MAX_DEVICES: usize = 10_000;

/// Trapezoidal energy integrator.
///
/// Each device is driven by a stream of `(timestamp, watts)` samples.
/// See the module docs for the exact gap-handling rules.
#[derive(Clone, Debug)]
pub struct PowerIntegrator {
    devices: HashMap<EnergyKey, DeviceState>,
    /// Gap threshold above which we switch from trapezoidal to
    /// hold-last integration.
    gap_interpolate: Duration,
}

impl Default for PowerIntegrator {
    fn default() -> Self {
        Self::new(Duration::from_secs(DEFAULT_GAP_INTERPOLATE_SECONDS))
    }
}

impl PowerIntegrator {
    pub fn new(gap_interpolate: Duration) -> Self {
        Self {
            devices: HashMap::new(),
            gap_interpolate,
        }
    }

    /// Feed a single `(t, watts)` sample for a device.
    ///
    /// Returns the Joule increment contributed by this sample (0.0 for
    /// the first observation of the device, NaN-guarded samples, or
    /// non-positive `dt`). The returned delta is ALSO added into the
    /// device's `wal_pending_joules` accumulator so a later WAL flush
    /// can persist it.
    ///
    /// If the integrator is already tracking [`MAX_DEVICES`] distinct
    /// keys and the incoming sample is for a new key, the sample is
    /// silently dropped (returns 0.0) to protect against unbounded
    /// memory growth from hostname/UUID churn.
    pub fn record_sample(&mut self, key: EnergyKey, t: Instant, watts: f64) -> f64 {
        let gap = self.gap_interpolate;
        if self.devices.len() >= MAX_DEVICES && !self.devices.contains_key(&key) {
            // Cardinality cap: refuse to open a new bucket. Existing
            // buckets keep accumulating normally; a later bounded-rate
            // key eviction policy can be layered in via the WAL replay
            // compaction, but for now a hard ceiling is the only
            // protection we need against UUID-churn attacks.
            return 0.0;
        }
        let state = self.devices.entry(key).or_default();

        // Filter invalid readings up front — a NaN / negative sample
        // still advances the clock (so the *next* sample gets a sane
        // `dt`) but contributes zero Joules for this window.
        let sanitized_now = sanitize_power(watts);

        let Some(prev) = state.last_sample else {
            // First observation: nothing to integrate yet.
            state.last_sample = Some(Sample {
                t,
                p: sanitized_now,
            });
            return 0.0;
        };

        let dt = t.saturating_duration_since(prev.t);
        if dt.is_zero() {
            // Duplicate sample or monotonic-clock quirk. Refresh the
            // stored power so gradually-changing draws are not lost if
            // consecutive samples carry identical timestamps.
            state.last_sample = Some(Sample {
                t: prev.t,
                p: sanitized_now,
            });
            return 0.0;
        }

        let dt_secs = dt.as_secs_f64();
        let prev_power = sanitize_power(prev.p);
        let delta = if sanitized_now == 0.0 && prev_power == 0.0 {
            // Both endpoints are zero / sanitized. No energy.
            0.0
        } else if dt > gap {
            // Gap exceeds the interpolation window: hold the last
            // reading across the whole gap.
            prev_power * dt_secs
        } else {
            // Trapezoidal rule for valid pairs (the linear-interpolation
            // integral).
            0.5 * (prev_power + sanitized_now) * dt_secs
        };

        state.session_joules += delta;
        state.lifetime_joules += delta;
        state.wal_pending_joules += delta;
        state.last_sample = Some(Sample {
            t,
            p: sanitized_now,
        });
        delta
    }

    /// Accumulated Joules for `key` in the current session (zeroed by
    /// [`PowerIntegrator::reset_session`]).
    #[allow(dead_code)] // Consumed by the chassis / top-consumer panels (issue #191).
    pub fn session_joules(&self, key: &EnergyKey) -> f64 {
        self.devices
            .get(key)
            .map(|s| s.session_joules)
            .unwrap_or(0.0)
    }

    /// Accumulated Joules for `key` across the lifetime of the process
    /// (including any seed from the WAL replay). Used for Prometheus.
    #[allow(dead_code)] // Consumed by the Prometheus exporter (issue #191).
    pub fn lifetime_joules(&self, key: &EnergyKey) -> f64 {
        self.devices
            .get(key)
            .map(|s| s.lifetime_joules)
            .unwrap_or(0.0)
    }

    /// `true` iff a sample has ever been recorded for `key` — used by
    /// the exporter / TUI to distinguish "device currently reports zero
    /// power" from "device does not report power at all".
    pub fn has_samples(&self, key: &EnergyKey) -> bool {
        self.devices
            .get(key)
            .map(|s| s.last_sample.is_some())
            .unwrap_or(false)
    }

    /// Reset the per-session counters for every device. The lifetime
    /// counter (which backs the Prometheus metric) is deliberately
    /// preserved so `rate()` / `increase()` stay monotonic.
    pub fn reset_session(&mut self) {
        for state in self.devices.values_mut() {
            state.session_joules = 0.0;
        }
    }

    /// Seed the lifetime counter for `key` with `joules`. Used during
    /// WAL replay before any live samples arrive.
    ///
    /// Does NOT touch the session counter — a fresh session starts at
    /// zero regardless of how much energy was recorded on disk.
    ///
    /// Respects the [`MAX_DEVICES`] cardinality cap: once the map
    /// already contains that many keys, additional seeds for *new*
    /// keys are silently dropped.
    pub fn seed_lifetime(&mut self, key: EnergyKey, joules: f64) {
        if !joules.is_finite() || joules <= 0.0 {
            return;
        }
        if self.devices.len() >= MAX_DEVICES && !self.devices.contains_key(&key) {
            return;
        }
        let state = self.devices.entry(key).or_default();
        state.lifetime_joules += joules;
    }

    /// Drain every device's `wal_pending_joules` into a vector of
    /// `(key, delta)` pairs, zeroing the per-device accumulator.
    ///
    /// Entries with zero pending delta are suppressed to keep the WAL
    /// small on idle hosts.
    pub fn drain_wal_deltas(&mut self) -> Vec<(EnergyKey, f64)> {
        let mut out = Vec::new();
        for (key, state) in self.devices.iter_mut() {
            if state.wal_pending_joules > 0.0 {
                out.push((key.clone(), state.wal_pending_joules));
                state.wal_pending_joules = 0.0;
            }
        }
        out
    }

    /// Iterate over every `(key, session_joules, lifetime_joules)`
    /// tuple with at least one recorded sample. Used by the TUI and
    /// Prometheus exporter.
    pub fn iter_stats(&self) -> impl Iterator<Item = EnergyStats<'_>> {
        self.devices.iter().filter_map(|(key, state)| {
            if state.last_sample.is_some() || state.lifetime_joules > 0.0 {
                Some(EnergyStats {
                    key,
                    session_joules: state.session_joules,
                    lifetime_joules: state.lifetime_joules,
                })
            } else {
                None
            }
        })
    }
}

/// Point-in-time view of one device's energy counters.
#[derive(Clone, Copy, Debug)]
pub struct EnergyStats<'a> {
    pub key: &'a EnergyKey,
    pub session_joules: f64,
    pub lifetime_joules: f64,
}

/// Top-level wrapper that owns the integrator and the
/// session-reset timestamp surfaced in the TUI.
#[derive(Clone, Debug)]
pub struct EnergyAccountant {
    integrator: PowerIntegrator,
    session_started_at: Instant,
}

impl Default for EnergyAccountant {
    fn default() -> Self {
        Self::new(Duration::from_secs(DEFAULT_GAP_INTERPOLATE_SECONDS))
    }
}

impl EnergyAccountant {
    pub fn new(gap_interpolate: Duration) -> Self {
        Self {
            integrator: PowerIntegrator::new(gap_interpolate),
            session_started_at: Instant::now(),
        }
    }

    /// Borrow the underlying integrator mutably. Collectors use this
    /// to feed samples via
    /// [`PowerIntegrator::record_sample`].
    pub fn integrator_mut(&mut self) -> &mut PowerIntegrator {
        &mut self.integrator
    }

    /// Borrow the underlying integrator for read-only queries.
    pub fn integrator(&self) -> &PowerIntegrator {
        &self.integrator
    }

    #[allow(dead_code)] // Consumed by the top-consumer panel (issue #191).
    pub fn session_started_at(&self) -> Instant {
        self.session_started_at
    }

    #[allow(dead_code)] // Consumed by the top-consumer panel (issue #191).
    pub fn session_elapsed(&self) -> Duration {
        self.session_started_at.elapsed()
    }

    /// Handle the `R` hotkey: zero the per-session counters and reset
    /// the session-started timestamp. The lifetime counter is NOT
    /// touched — Prometheus needs to stay monotonic.
    pub fn reset_session(&mut self) {
        self.integrator.reset_session();
        self.session_started_at = Instant::now();
    }

    /// Seed the lifetime counter for `key` directly, used by the WAL
    /// replay path when a live sample arrives whose hash matches a
    /// previously-recorded entry.
    #[allow(dead_code)] // Called via EnergyAccountant::integrator_mut() in tests.
    pub fn seed_lifetime(&mut self, key: EnergyKey, joules: f64) {
        self.integrator.seed_lifetime(key, joules);
    }
}

/// Guard NaN / infinite / negative / implausibly-large power readings.
///
/// - NaN, `±inf`, and negative values are treated as zero for the
///   affected window (the timestamp still advances so the next sample
///   gets a sensible `dt`).
/// - Values above [`MAX_POWER_WATTS`] are clamped to that ceiling. An
///   unclamped `f64::MAX` would overflow to `+inf` once multiplied by
///   any non-zero `dt`, permanently poisoning the running
///   `lifetime_joules` counter — a single bad sample could then show
///   up forever in `all_smi_energy_consumed_joules_total`.
#[inline]
fn sanitize_power(watts: f64) -> f64 {
    if !watts.is_finite() || watts < 0.0 {
        0.0
    } else if watts > MAX_POWER_WATTS {
        MAX_POWER_WATTS
    } else {
        watts
    }
}

/// Convert Joules to kilowatt-hours.
#[inline]
pub fn joules_to_kwh(joules: f64) -> f64 {
    joules / JOULES_PER_KWH
}

/// Compute the approximate monetary cost of `joules` at the given
/// `price_per_kwh`.  A non-positive or non-finite price yields `0.0`.
#[inline]
pub fn joules_to_cost(joules: f64, price_per_kwh: f64) -> f64 {
    if !price_per_kwh.is_finite() || price_per_kwh <= 0.0 {
        return 0.0;
    }
    joules_to_kwh(joules) * price_per_kwh
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    /// Analytic energy integral of `P(t) = P_mid + A * sin(omega * t)`
    /// from `t = 0` to `t = T`.
    fn analytic_sine_joules(p_mid: f64, amp: f64, omega: f64, t: f64) -> f64 {
        // ∫₀ᵀ P_mid + A * sin(ω t) dt
        //   = P_mid * T + A * (1 - cos(ω T)) / ω
        p_mid * t + amp * (1.0 - (omega * t).cos()) / omega
    }

    #[test]
    fn sine_wave_trapezoidal_matches_analytic_within_0_1_percent() {
        // Construct a 1 000-sample sine-wave power stream and integrate
        // with the trapezoidal rule; compare with the analytic integral.
        //
        // P(t) = 200 W + 100 * sin(2π t / 60)   (60 s period)
        let p_mid = 200.0;
        let amp = 100.0;
        let period = 60.0_f64;
        let omega = 2.0 * PI / period;

        let samples = 1000;
        let dt = 0.1_f64; // 100 ms between samples → total 100 s
        let total_time = samples as f64 * dt;

        let mut integ = PowerIntegrator::new(Duration::from_secs(10));
        let key = EnergyKey::gpu("host", "uuid");
        let origin = Instant::now();

        for i in 0..=samples {
            let t = i as f64 * dt;
            let watts = p_mid + amp * (omega * t).sin();
            integ.record_sample(key.clone(), origin + Duration::from_secs_f64(t), watts);
        }

        let integrated = integ.lifetime_joules(&key);
        let analytic = analytic_sine_joules(p_mid, amp, omega, total_time);

        let rel_error = ((integrated - analytic).abs() / analytic).abs();
        assert!(
            rel_error < 0.001,
            "trapezoidal sine integral off: analytic {analytic:.6}, integrated {integrated:.6}, rel_error {rel_error:.6}"
        );
    }

    #[test]
    fn constant_power_matches_rectangle_integral() {
        // 300 W held for 10 minutes should yield 300 * 600 = 180 000 J
        // which rounds to ~0.05 kWh (acceptance criterion in the issue).
        let mut integ = PowerIntegrator::default();
        let key = EnergyKey::gpu("dgx-01", "uuid-0");
        let origin = Instant::now();

        integ.record_sample(key.clone(), origin, 300.0);
        integ.record_sample(key.clone(), origin + Duration::from_secs(600), 300.0);

        let joules = integ.lifetime_joules(&key);
        assert!(
            (joules - 180_000.0).abs() < 1e-6,
            "expected 180 000 J, got {joules}"
        );
        assert!((joules_to_kwh(joules) - 0.05).abs() < 1e-9);
    }

    #[test]
    fn short_gap_interpolates_linearly() {
        // 5-second gap between 100 W and 200 W samples should yield the
        // trapezoidal average: 0.5 * (100 + 200) * 5 = 750 J.
        let mut integ = PowerIntegrator::new(Duration::from_secs(10));
        let key = EnergyKey::gpu("host", "uuid");
        let origin = Instant::now();
        integ.record_sample(key.clone(), origin, 100.0);
        integ.record_sample(key.clone(), origin + Duration::from_secs(5), 200.0);
        assert!((integ.lifetime_joules(&key) - 750.0).abs() < 1e-9);
    }

    #[test]
    fn long_gap_holds_last_reading() {
        // 30-second gap (> 10 s threshold) with 100 W → 200 W should
        // hold the previous 100 W across the whole gap:
        //   100 * 30 = 3 000 J (NOT 4 500 J that trapezoid would give).
        let mut integ = PowerIntegrator::new(Duration::from_secs(10));
        let key = EnergyKey::gpu("host", "uuid");
        let origin = Instant::now();
        integ.record_sample(key.clone(), origin, 100.0);
        integ.record_sample(key.clone(), origin + Duration::from_secs(30), 200.0);
        assert!(
            (integ.lifetime_joules(&key) - 3_000.0).abs() < 1e-9,
            "got {}",
            integ.lifetime_joules(&key)
        );
    }

    #[test]
    fn nan_and_negative_samples_linear_glide_to_zero() {
        // A NaN or negative sample in the middle of a stream is
        // replaced with 0.0 at that endpoint (see `sanitize_power`).
        // The trapezoidal rule then produces a linear glide toward
        // zero from the adjacent valid reading rather than a full-zero
        // window. The clock still advances so the next sample gets a
        // sensible `dt`.
        let mut integ = PowerIntegrator::default();
        let key = EnergyKey::gpu("host", "uuid");
        let origin = Instant::now();
        integ.record_sample(key.clone(), origin, 100.0);
        integ.record_sample(key.clone(), origin + Duration::from_secs(1), f64::NAN);
        integ.record_sample(key.clone(), origin + Duration::from_secs(2), -50.0);
        integ.record_sample(key.clone(), origin + Duration::from_secs(3), 100.0);

        let joules = integ.lifetime_joules(&key);
        // Windows (sanitize_power replaces NaN / negative with 0.0):
        //  t=0 → 1s: 0.5 * (100 + 0) * 1 = 50   (glide down from 100)
        //  t=1 → 2s: 0.5 * (0 + 0)   * 1 = 0    (both endpoints zero)
        //  t=2 → 3s: 0.5 * (0 + 100) * 1 = 50   (glide up to 100)
        assert!((joules - 100.0).abs() < 1e-9, "got {joules}");
    }

    #[test]
    fn reset_session_preserves_lifetime() {
        let mut acct = EnergyAccountant::default();
        let key = EnergyKey::gpu("host", "uuid");
        let origin = Instant::now();
        acct.integrator_mut()
            .record_sample(key.clone(), origin, 100.0);
        acct.integrator_mut()
            .record_sample(key.clone(), origin + Duration::from_secs(10), 100.0);

        let lifetime_before = acct.integrator().lifetime_joules(&key);
        assert!(lifetime_before > 0.0);
        assert!(acct.integrator().session_joules(&key) > 0.0);

        acct.reset_session();

        assert_eq!(acct.integrator().session_joules(&key), 0.0);
        assert!(
            (acct.integrator().lifetime_joules(&key) - lifetime_before).abs() < 1e-9,
            "lifetime must survive reset"
        );
    }

    #[test]
    fn seed_lifetime_adds_to_counter_without_touching_session() {
        let mut integ = PowerIntegrator::default();
        let key = EnergyKey::gpu("host", "uuid");
        integ.seed_lifetime(key.clone(), 1_000.0);
        assert_eq!(integ.lifetime_joules(&key), 1_000.0);
        assert_eq!(integ.session_joules(&key), 0.0);

        // A NaN / negative seed is ignored (robust against a corrupted
        // WAL record that slips past the torn-final-record check).
        integ.seed_lifetime(key.clone(), f64::NAN);
        integ.seed_lifetime(key.clone(), -5.0);
        assert_eq!(integ.lifetime_joules(&key), 1_000.0);
    }

    #[test]
    fn drain_wal_deltas_zeros_pending() {
        let mut integ = PowerIntegrator::default();
        let key = EnergyKey::gpu("host", "uuid");
        let origin = Instant::now();
        integ.record_sample(key.clone(), origin, 100.0);
        integ.record_sample(key.clone(), origin + Duration::from_secs(10), 100.0);

        let drained = integ.drain_wal_deltas();
        assert_eq!(drained.len(), 1);
        assert!((drained[0].1 - 1_000.0).abs() < 1e-9);

        // Second drain with no new samples: empty.
        let drained2 = integ.drain_wal_deltas();
        assert!(drained2.is_empty());
    }

    #[test]
    fn first_sample_does_not_panic_and_returns_zero() {
        let mut integ = PowerIntegrator::default();
        let key = EnergyKey::gpu("host", "uuid");
        let delta = integ.record_sample(key.clone(), Instant::now(), 250.0);
        assert_eq!(delta, 0.0);
        assert_eq!(integ.lifetime_joules(&key), 0.0);
    }

    #[test]
    fn duplicate_timestamp_refreshes_power_without_accumulating() {
        let mut integ = PowerIntegrator::default();
        let key = EnergyKey::gpu("host", "uuid");
        let origin = Instant::now();
        integ.record_sample(key.clone(), origin, 100.0);
        let delta = integ.record_sample(key.clone(), origin, 500.0);
        assert_eq!(delta, 0.0);
        // After a 1-second gap, the second (refreshed) power wins.
        integ.record_sample(key.clone(), origin + Duration::from_secs(1), 500.0);
        let joules = integ.lifetime_joules(&key);
        // 0.5 * (500 + 500) * 1.0 = 500
        assert!((joules - 500.0).abs() < 1e-9, "got {joules}");
    }

    #[test]
    fn joules_to_cost_respects_non_positive_prices() {
        assert_eq!(joules_to_cost(3_600_000.0, 0.12), 0.12);
        assert_eq!(joules_to_cost(3_600_000.0, 0.0), 0.0);
        assert_eq!(joules_to_cost(3_600_000.0, -0.5), 0.0);
        assert_eq!(joules_to_cost(3_600_000.0, f64::NAN), 0.0);
    }

    #[test]
    fn energy_key_hashes_are_stable_within_process() {
        let k1 = EnergyKey::gpu("host-a", "uuid-0");
        let k2 = EnergyKey::gpu("host-a", "uuid-0");
        assert_eq!(k1.host_hash(), k2.host_hash());
        assert_eq!(k1.device_hash(), k2.device_hash());

        // Chassis and GPU scopes must not collide on the device hash
        // even though both leave `device` empty / non-empty.
        let chassis = EnergyKey::chassis("host-a");
        let cpu = EnergyKey::cpu("host-a");
        assert_ne!(chassis.device_hash(), cpu.device_hash());
    }

    #[test]
    fn pathological_power_samples_do_not_overflow_lifetime() {
        // An attacker or buggy driver can feed f64::MAX / infinity; the
        // integrator must clamp at MAX_POWER_WATTS so the counter stays
        // finite even across a long `dt`.
        let mut integ = PowerIntegrator::default();
        let key = EnergyKey::gpu("host", "uuid");
        let origin = Instant::now();
        // 1 hour of f64::MAX readings, one per second.
        for i in 0..3600 {
            integ.record_sample(
                key.clone(),
                origin + Duration::from_secs(i),
                if i == 0 { 100.0 } else { f64::MAX },
            );
        }
        // Final sample: infinity.
        integ.record_sample(
            key.clone(),
            origin + Duration::from_secs(3601),
            f64::INFINITY,
        );
        let joules = integ.lifetime_joules(&key);
        assert!(
            joules.is_finite(),
            "lifetime counter must remain finite under pathological input (got {joules})"
        );
        // Upper bound: MAX_POWER_WATTS * total_dt with a bit of slack.
        let upper_bound = MAX_POWER_WATTS * 3601.0 * 1.01;
        assert!(
            joules <= upper_bound,
            "lifetime counter should stay under the MAX_POWER_WATTS envelope: got {joules}, bound {upper_bound}"
        );
    }

    #[test]
    fn max_power_watts_clamps_single_sample() {
        // One pathological sample followed by a normal one: the clamped
        // sample should be treated as exactly MAX_POWER_WATTS for the
        // trapezoidal integral.
        let mut integ = PowerIntegrator::default();
        let key = EnergyKey::gpu("host", "uuid");
        let origin = Instant::now();
        integ.record_sample(key.clone(), origin, f64::MAX);
        integ.record_sample(key.clone(), origin + Duration::from_secs(1), 0.0);
        let joules = integ.lifetime_joules(&key);
        // Trapezoidal with p_prev=MAX_POWER_WATTS, p_now=0 over 1 s:
        //   0.5 * (MAX_POWER_WATTS + 0) * 1 = MAX_POWER_WATTS / 2
        assert!(
            (joules - MAX_POWER_WATTS / 2.0).abs() < 1e-3,
            "expected ~MAX_POWER_WATTS/2 J, got {joules}"
        );
    }

    #[test]
    fn record_sample_enforces_device_cardinality_cap() {
        // Once the device cap is reached, further record_sample calls
        // for NEW keys must silently drop rather than grow the map.
        let mut integ = PowerIntegrator::default();
        let origin = Instant::now();
        for i in 0..MAX_DEVICES {
            integ.record_sample(EnergyKey::gpu("host", format!("uuid-{i}")), origin, 100.0);
        }
        assert_eq!(integ.devices.len(), MAX_DEVICES);

        // One more new key should be refused.
        let overflow_key = EnergyKey::gpu("host", "uuid-overflow");
        let delta = integ.record_sample(overflow_key.clone(), origin, 100.0);
        assert_eq!(delta, 0.0);
        assert_eq!(integ.devices.len(), MAX_DEVICES);
        assert!(!integ.has_samples(&overflow_key));

        // An existing key keeps working.
        let existing = EnergyKey::gpu("host", "uuid-0");
        integ.record_sample(existing.clone(), origin + Duration::from_secs(1), 100.0);
        assert!(integ.has_samples(&existing));
    }
}
