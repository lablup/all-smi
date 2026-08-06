# Technical Report: PR #337 - fix: omit Apple Silicon GPU metrics instead of reporting 0

**Date**: 2026-08-06
**Status**: Completed for the reader/exporter/TUI/aggregation chain; the degraded path itself is exercised by unit tests plus the real launchd CI runner, not by hardware with IOReport physically disabled (see section 8)
**Languages**: Rust, YAML (GitHub Actions)
**Risk Level**: Medium (touches 21 files across the reader, exposition, TUI, and aggregation layers; behavior change is a correctness fix with a real, if narrow, wire-format effect: five metric families become conditionally omitted)

---

## Table of Contents

1. [Executive Summary](#executive-summary)
2. [Problem Statement](#1-problem-statement)
3. [Technical Review](#2-technical-review)
4. [Technical Decisions](#3-technical-decisions)
5. [Implementation Details](#4-implementation-details)
6. [Learning Points](#5-learning-points)
7. [Further Learning](#6-further-learning)
8. [Change Summary](#7-change-summary)
9. [Follow-up Actions](#8-follow-up-actions)
10. [Appendix](#appendix)

---

## Executive Summary

When `NativeMetricsManager::new()` fails, which is the normal state on any macOS host without IOReport (a VM, a hardened sandbox, a hosted CI runner), the Apple Silicon GPU reader fell back to `GpuMetrics::default()` and unwrapped every field to a literal zero. A macOS host that could not measure its GPU at all published `all_smi_gpu_utilization 0`, `all_smi_gpu_power_consumption_watts 0`, and `all_smi_gpu_temperature_celsius 0`, indistinguishable on the wire from a genuinely idle GPU. Fixing the exporter alone would not have been enough: `GpuInfo` feeds the Prometheus exporter, the TUI, and `snapshot` from one struct, so `src/network/metrics_parser.rs` also initialized a remote `GpuInfo`'s live fields to `0.0` on the viewing side, meaning `all-smi view --hosts` against a degraded node would still have rendered `0.0%` in the local TUI even after the exporter started omitting the series correctly. The fix touches the reader, the exposition, the remote parser, and every aggregation point in between, aligning all four macOS readers (memory, CPU, chassis, GPU) on one documented policy: emit a row whenever something true can still be said about the device, mark the fields that could not be sourced as absent, and never substitute 0.

The absence is encoded in-band rather than as `Option<f64>`, a choice made deliberately mid-fix rather than assumed from the start: `GpuInfo`'s live fields have roughly sixty consumers (gauges, sparklines, the LED grid, sort comparators, energy accumulation, three CLI shims, the mock server, twelve readers), and converting the type would have been a large, unrelated-to-the-bug refactor riding along with four sibling PRs touching the same base. `GPU_METRIC_UNAVAILABLE` (`-1.0`, out of range for every consuming quantity) fills that role instead, read back through five named accessors, and the reason rides the already-emitted `all_smi_gpu_info` identity series as a new `native_metrics="available"|"unavailable"` label, at zero cost in new metric families. Two real aggregation bugs surfaced during the fix, not merely theorized: the energy integrator would have summed the `-1.0` sentinel directly into a running joule total, accumulating negative energy, and the TUI's history graphs would have plotted a dip in GPU utilization/temperature that never actually happened, because the old code always divided a partial (sentinel-polluted) sum by the full device count. The degraded path itself was confirmed on the actual reproduction environment, the `macos-14` CI runner, which has no IOReport, via the launchd smoke test's new assertion that the runner emits no fabricated `all_smi_gpu_utilization`, not only by a unit test constructing the condition synthetically. Total: 21 files, +1176/-374, one commit, closing #325.

---

## 1. Problem Statement

### 1.1 Background

`src/device/macos_native/manager.rs` holds `NativeMetricsManager` in a process-wide `Lazy<Mutex<Option<Arc<...>>>>` singleton. When `NativeMetricsManager::new()` fails, which happens whenever `IOReport::new()` fails, the normal state on a VM, a hardened sandbox, or a hosted macOS CI runner without real IOReport access, the singleton stays `None` for the entire process lifetime; there is no retry. Before this PR, the four macOS readers each handled that absence differently: memory (pure `sysinfo`, unaffected), CPU (already correct: `Option` fields go `None`, frequency falls back to `sysctl`), chassis (already correct: returns no row at all, since every field it reports comes from the manager), and GPU, which alone fabricated data by unwrapping every `Option` field from the manager to a literal `0`.

### 1.2 Existing Issues

- **Issue 1 (the GPU reader fabricated zeros)**: `src/device/readers/apple_silicon_native.rs` fell back to `GpuMetrics::default()` (all `None`) when the manager was absent or a collection failed, then built `GpuInfo` with `utilization: metrics.utilization.unwrap_or(0.0)`, `frequency: metrics.frequency.unwrap_or(0)`, `power_consumption: metrics.power_consumption.unwrap_or(0.0)`, and a temperature fallback chain ending in `unwrap_or(0)`. A dashboard showed "GPU 0% / 0 W / 0 degrees" for a device that was measuring nothing.
- **Issue 2 (zero is a legitimate reading, so it cannot double as "no data")**: an idle GPU or a parked ANE can genuinely read `0`, so a consumer had no way to distinguish that from the manager-unavailable case without inspecting IOReport availability out of band.
- **Issue 3 (fixing only the exporter would have been incomplete)**: `GpuInfo` is shared by three consumers (the Prometheus exporter, the TUI, and `snapshot`), so omitting the series only at the exposition layer would have left the TUI still rendering `0.0%`.
- **Issue 4 (the remote-viewing boundary re-introduces the same bug even after the exporter is fixed)**: `src/network/metrics_parser.rs` built a remote `GpuInfo` with `utilization: 0.0, ane_utilization: 0.0, temperature: 0, power_consumption: 0.0` as its starting point, only overwriting fields whose series actually appeared in the scraped exposition. Once the exporter correctly omits a series, the parser's zero-initialized default silently re-fabricates the exact value the exporter went out of its way to omit, so `all-smi view --hosts` against a degraded macOS node would still have shown `0.0%` in the local TUI.
- **Issue 5 (aggregations folded the sentinel or the omission into their computations)**: `src/metrics/aggregator.rs`, `src/view/data_collection/aggregator.rs`, `src/api/collection_loop.rs`, and `src/snapshot/collector.rs` all summed or averaged raw `GpuInfo` fields without skipping absent readings, which (once absence was encoded as a negative sentinel) would corrupt an energy total and (even under the pre-fix all-zero encoding) already silently skewed averages and history graphs whenever one GPU stopped reporting.
- **Issue 6 (the CI comment describing this behavior was itself wrong)**: `.github/workflows/ci.yml`'s launchd job comment said the GPU and chassis readers "degrade to zeros and to nothing respectively," documenting the bug as expected behavior rather than flagging it.

### 1.3 Risk Assessment

| Risk | Impact | Likelihood |
|---|---|---|
| A macOS host with no IOReport publishes fabricated zero GPU metrics, indistinguishable from an idle GPU | Medium in isolation (a confusing but not crash-causing reading), higher in aggregate (a fleet dashboard averaging in fabricated zeros silently understates real utilization) | Certain prior to this fix, on every macOS host without IOReport (VMs, sandboxes, some CI runners) |
| Converting `GpuInfo`'s live fields to `Option<f64>` mid-fix, given roughly sixty consumers | High if attempted here: a type-level refactor across gauges, sparklines, sort comparators, energy accumulation, CLI shims, the mock server, and twelve readers, landing alongside sibling PRs (#333 through #339) touching overlapping files | Avoided by choosing an in-band sentinel instead (section 3.1) |
| Fixing the reader/exporter but not the remote-scrape parser | High if missed: `all-smi view --hosts` against a degraded macOS node would still show fabricated zeros even after the exporter was correct | Explicitly identified and fixed (`src/network/metrics_parser.rs`, section 1.2 issue 4) |
| The two aggregation bugs (negative-joule energy integration, history-graph dip) going unnoticed because they are silent, not crashing | Medium: an energy total quietly going negative, or a history graph plotting a dip that never happened, are the kind of defect that erodes trust in a monitoring tool without an obvious symptom to report | Found and fixed as part of this PR rather than left for a future bug report (section 2.1) |

---

## 2. Technical Review

### 2.1 Correctness

The unifying rule, documented on `src/device/macos_native/manager.rs` and referenced from the `GpuReader` trait, is stated precisely: a reader emits a row whenever it can still say something true about the device, and marks only the individual fields it could not source as absent; it never substitutes `0`. The Apple Silicon GPU reader satisfies this because identity (`sysctl`) and unified memory (`sysinfo`) are independent of IOReport and remain valid on the degraded path; only the five IOReport/SMC-sourced fields (utilization, ANE power, frequency, power consumption, temperature) go absent.

Two aggregation bugs were found and fixed as a direct consequence of switching the encoding from "always a plausible-looking zero" to "a sentinel that must be explicitly skipped," which is worth stating precisely because both existed in the pre-fix code too, just less visibly:

- **Energy integration** (`src/api/collection_loop.rs`, `integrate_power_samples`): before this PR, `gpu.power_consumption` was pushed into the energy sample list unconditionally; with the in-band sentinel, doing that unchanged would sum `-1.0` directly into a running joule total, an obvious corruption. The fix filters through `gpu.power_consumption_reading()`, contributing no sample at all for a device with no reading, which is also the behaviorally correct fix for the pre-existing (less obviously wrong) zero-substitution case: a fabricated `0.0` silently understated a device's energy draw exactly as much as an omitted sample now correctly contributes nothing.
- **History graphs** (`src/view/data_collection/aggregator.rs`): `avg_utilization` and `avg_temperature` were computed as `sum / state.gpu_info.len()`, unconditionally pushed into `utilization_history`/`temperature_history` every cycle. A GPU reporting the old fabricated `0` (or, after this PR, the sentinel, if summed naively) would drag the average down every cycle it failed to report, so the history graph plots a dip that never actually happened on the reporting devices. The fix uses `metrics::gpu_readings::mean_utilization`/`mean_temperature`, which return `None` when nothing reported, and the history push is skipped entirely for that cycle rather than pushing a fabricated low value.

`src/metrics/gpu_readings.rs` (new) centralizes every one of these skip-aware aggregations (`total_power_watts`, `mean_utilization`, `mean_temperature`, `temperature_std_dev`, `first_ane_power_watts`) so the TUI dashboard, header, sparkline panel, snapshot writer, and cluster aggregator cannot each reimplement the skip logic slightly differently. `temperature_std_dev` additionally changes its return type to `Option<f64>`, returning `None` when fewer than two devices reported a reading, correcting a latent issue where the pre-existing code divided by `total_gpus - 1` and relied on a `total_gpus > 1` guard at call sites rather than on "how many devices actually reported."

### 2.2 Performance

No new per-cycle cost beyond what the aggregation functions already did: `metrics::gpu_readings`'s functions are single passes over the existing `&[GpuInfo]` slice, using `filter_map`/`find_map` rather than allocating an intermediate collection in most cases. The Prometheus exporter's per-field `if let Some(reading) = info.x_reading() { ... }` guards are O(1) checks per field, replacing an unconditional `builder.metric(...)` call with a conditionally-skipped one; the cost difference per scrape is negligible.

### 2.3 Compatibility and Dependencies

- **Breaking changes**: a real, narrow wire-format change. `all_smi_gpu_utilization`, `all_smi_gpu_power_consumption_watts`, `all_smi_gpu_temperature_celsius`, `all_smi_gpu_frequency_mhz`, `all_smi_ane_utilization`, and `all_smi_ane_power_watts` are now omitted (rather than emitted as `0`) for a GPU row with no reading for that specific field. This is a correctness fix, not a cosmetic one, but any PromQL query or alert rule that assumed these series are always present for every `gpu_index` (for example `gpu_temp > 80` without an `absent()` companion) needs the same review any Prometheus "omit on no data" convention requires. `all_smi_gpu_info` (the identity series) is unaffected and always present when a device is detected, now carrying `native_metrics="available"|"unavailable"` on Apple Silicon.
- **New dependencies**: none.
- **Compatibility**: two behavior changes extend beyond Apple Silicon, both bringing the exposition in line with what the TUI already rendered before this PR: `all_smi_gpu_frequency_mhz` is now omitted (rather than reported as a static `0`) for readers that already used `0` to mean "no clock probe" (Rebellions, Intel Gaudi, AMD via WMI), and `all_smi_gpu_temperature_celsius` is now omitted when no sensor answered on any platform. `all_smi_ane_utilization` is explicitly unchanged for non-Apple GPUs, which set a literal `0.0` meaning "not applicable" and keep publishing it; this is covered by a dedicated regression test (`exporter_keeps_emitting_zero_ane_for_non_apple_gpus`) specifically because it is the one place a naive "omit on -1 sentinel" change could have regressed a working platform.

### 2.4 Code Quality

The degraded path is exercised at three levels, not only unit-tested in isolation. First, `build_gpu_info(static_info, apple_info, sample: Option<&NativeSample>)`, extracted from `get_gpu_info`, is a seam that lets a test drive `sample: None`, the exact branch a macOS VM takes, with no `cfg(test)` switch and no forced-failure flag; `degraded_path_reports_absence_not_zero`, `degraded_path_keeps_identity_and_memory`, and `missing_smc_sensors_degrade_only_temperature` exercise it directly. Second, that degraded row is pushed through the real Prometheus exporter (`degraded_row_renders_no_gpu_value_series`) and its healthy counterpart (`healthy_row_renders_every_value_series`), so the reader-to-exporter seam is covered, not just the reader in isolation. Third, and the strongest form of verification in this PR, `.github/workflows/ci.yml`'s launchd smoke test job, which runs on a real `macos-14` runner with no IOReport, gained an explicit negative assertion: `! curl -sf --max-time 10 localhost:9090/metrics | grep -q '^all_smi_gpu_utilization'`, confirming on the actual reproduction environment, not a synthetic one, that the runner emits no fabricated utilization series. The same job's comment describing "degrade to zeros and to nothing respectively" is corrected to describe the fixed behavior.

`src/network/metrics_parser.rs` gains two tests mirroring the same pair on the reader side: `test_omitted_gpu_series_stay_absent_after_scrape` (parses an exposition body containing only identity and memory series, exactly what the degraded exporter renders, and asserts every `*_reading()` accessor returns `None`) and `test_zero_gpu_series_survives_scrape_as_a_reading` (a genuine `0` in the scraped body survives as `Some(0.0)` on the viewing side), so the two cases stay distinguishable end to end across a network hop, not only within one process.

---

## 3. Technical Decisions

### 3.1 Encode absence in-band with a sentinel, rejecting `Option<f64>` mid-fix on blast-radius grounds

**Context**: `Option<f64>` is the type-safe way to represent "a value or no value," and would let the compiler enforce every call site. `GpuInfo`'s `utilization`, `ane_utilization`, `power_consumption`, `temperature`, and `frequency` fields are plain non-optional numbers today, consumed by roughly sixty call sites: gauges, sparklines, the LED grid, sort comparators, energy accumulation, three CLI shims, the mock server, and twelve device readers.

| Option | Pros | Cons |
|---|---|---|
| **Rejected: `Option<f64>`/`Option<u32>` on `GpuInfo`'s live fields** | Type system enforces handling at every call site; no magic sentinel to remember | A type-level refactor across roughly sixty consumers, landing in a bug-fix PR running alongside four sibling PRs (#333, #334, #335/#336, #338, #339) touching overlapping files; large regression surface for no behavioral gain over the alternative |
| **Chosen: in-band sentinel, `GPU_METRIC_UNAVAILABLE = -1.0` for `f64` fields, `0` for `u32` fields, read back through five named accessors** | Zero blast radius on existing call sites that do not need to change; the codebase already had unenforced conventions matching this exact encoding (see below) | Requires discipline to always read through the accessors rather than the raw field, and the encoding must never leak onto the wire (verified by a dedicated test asserting no ` -1` appears in exported output) |
| Suppress the whole `GpuInfo` row when the manager is unavailable | Simplest change at the reader | Loses identity and unified-memory data that remains valid on the degraded path, and contradicts the documented policy ("emit a row whenever something true can still be said") |

**Rationale**: the encoding chosen is not a new invention. `src/ui/renderers/gpu_renderer.rs` and `src/ui/filter_dsl/eval.rs` were already reading `utilization < 0.0` and `power_consumption < 0.0` as N/A, and `temperature == 0` / `frequency == 0` as N/A, before this PR; those branches simply had no producer feeding them a negative or zero value on purpose. This PR gives them one. The sentinel value is out of the valid range for every quantity it represents (a percentage is `0..=100`, a power rail draws `>= 0` watts), so it cannot collide with a real reading, and the accessors (`utilization_reading()` and its four siblings) are the enforced boundary that keeps the internal encoding from leaking, checked directly by a test asserting the string `" -1"` never appears in rendered Prometheus output.

**Trade-off accepted**: the type system does not force a caller to handle absence; a future call site that reads `gpu.utilization` directly instead of `gpu.utilization_reading()` would silently treat the sentinel as a real (deeply negative, then likely clamped or ignored downstream) value rather than failing to compile. This risk is judged acceptable given the alternative's cost in this specific PR, not judged permanently acceptable; `Option<f64>` remains the type-correct target if a future, dedicated refactor takes it on.

### 3.2 Omission at the exposition layer, an explicit `native_metrics` label at the identity layer: both, because they answer different questions

**Context**: two candidate signals exist for representing "this GPU's value series are absent": omitting the series entirely (Prometheus' own no-data convention), or emitting an explicit sentinel/flag value that a consumer can query.

**Decision**: both, at different layers. The value series (`all_smi_gpu_utilization` and its four siblings) are omitted, matching the exporter's own pre-existing convention for `all_smi_gpu_performance_state` and the four thermal-threshold families, which have been omitted-when-absent since #132. The *reason* rides `all_smi_gpu_info`, the always-present identity series for a detected device, as a new `native_metrics="available"|"unavailable"` label.

**Rationale**: omission alone says nothing about *why* a series is missing, which matters here because "no IOReport on this host" is a permanent, actionable condition rather than a transient gap, and is now queryable directly (`all_smi_gpu_info{native_metrics="unavailable"}`) without inferring it from which other series happen to be missing. An explicit sentinel value on the wire instead of omission was rejected because it would have introduced a second absence convention alongside the exporter's existing one for performance state and thermal thresholds, for no benefit: the objection that a vanishing series resembles a dead target is weaker here than it looks, because the target is never silent under this PR, `all_smi_up`/`all_smi_build_info` (PR #333) are unconditional, memory/CPU/disk families still render, and `all_smi_gpu_info` for this very device still renders, so "device present, not reporting" and "device gone entirely" stay distinguishable without a special-cased metric.

### 3.3 One absence policy for all five affected metric families, not a per-field carve-out for temperature

**Context**: temperature is the one field worth arguing about differently, since a vanishing temperature series could in principle break a `max_over_time`-style thermal alert that depends on the series always existing.

**Decision**: no carve-out; temperature follows the same omit-on-absence rule as the other four fields.

**Rationale**: the alternative (keep publishing a fabricated temperature to avoid breaking an alert's *evaluation*) breaks the alert *correctness* worse than omission does: `gpu_temp > 80` never fires either way when the series is genuinely missing, while a fabricated `0` makes any `gpu_temp < N` alert fire spuriously and silently drags a cluster-wide temperature average down. More concretely, all five fields come from a single IOReport/SMC subscription and fail together as a unit, so a split policy (temperature behaves one way, the other four another) would force a dashboard author to learn two rules for what is, on this hardware, one failure mode. The codebase's own prior behavior corroborates this: the TUI, the alert engine, and the filter DSL already treated `temperature == 0` as unknown before this PR, so a differently-encoded temperature would have been the one inconsistent field rather than the other way around.

### 3.4 Build the healthy/degraded seam as `build_gpu_info(..., sample: Option<&NativeSample>)`, extracted specifically so a test can drive the manager-unavailable branch without real hardware

**Context**: the manager-unavailable path cannot be reproduced on the development machine (an M1 Ultra with working IOReport), so the fix needed a way to exercise the exact code path a macOS VM takes without mocking `NativeMetricsManager` itself.

**Decision**: extract the row-assembly logic that used to be inline in `get_gpu_info` into a free function, `build_gpu_info(static_info: &DeviceStaticInfo, apple_info: Option<&AppleSiliconInfo>, sample: Option<&NativeSample>) -> GpuInfo`, with `get_gpu_info` reduced to acquiring the sample (`self.native_manager.get().and_then(|m| m.collect_once().ok()).map(...)`) and calling it.

**Rationale**: `Option<&NativeSample>` is exactly the one bit of information everything downstream needs, whether the native source produced anything at all this cycle, and passing `None` to `build_gpu_info` directly drives byte-for-byte the same branch a macOS VM takes, with no `cfg(test)` conditional compilation and no forced-failure flag standing in for the real condition. This is what let `degraded_row_renders_no_gpu_value_series` push a reader-constructed row through the real `GpuMetricExporter` and assert on real rendered output, rather than constructing a `GpuInfo` by hand and hoping it matches what the reader would actually produce.

---

## 4. Implementation Details

### 4.1 Architecture Changes

```
[Before]
NativeMetricsManager::new() fails (no IOReport)
    │
    ▼
get_gpu_info(): metrics = GpuMetrics::default() (all None)
    │
    ▼
GpuInfo { utilization: metrics.utilization.unwrap_or(0.0), ... }   -- fabricated zeros
    │
    ├──▶ Prometheus exporter: always emits the series, value 0
    ├──▶ TUI: renders "0.0%"
    └──▶ metrics_parser (remote view): re-zeros on scrape parse anyway

[After]
NativeMetricsManager::new() fails (no IOReport)
    │
    ▼
get_gpu_info(): sample = native_manager.get().and_then(|m| m.collect_once().ok()).map(...) = None
    │
    ▼
build_gpu_info(static_info, apple_info, sample: None)
    │  identity + unified memory: still valid (sysctl / sysinfo)
    │  utilization/ane/power/frequency: GPU_METRIC_UNAVAILABLE or 0 (sentinel)
    │  detail["native_metrics"] = "unavailable"
    ▼
GpuInfo { utilization: -1.0, ... }   -- internal encoding, never on the wire
    │
    ├──▶ Prometheus exporter: `if let Some(v) = info.utilization_reading() { emit }` -- omitted
    ├──▶ TUI: renders "N/A", gauge draws empty
    ├──▶ metrics_parser (remote view): starts every live field absent, overwrites only present series
    └──▶ metrics::gpu_readings: excluded from means/sums (energy integrator, history graphs)
```

### 4.2 Key Code Changes

**File: `src/device/types.rs` (the sentinel and its accessors)**
```rust
pub const GPU_METRIC_UNAVAILABLE: f64 = -1.0;

impl GpuInfo {
    pub fn utilization_reading(&self) -> Option<f64> {
        (self.utilization >= 0.0).then_some(self.utilization)
    }
    pub fn temperature_reading(&self) -> Option<u32> {
        (self.temperature > 0).then_some(self.temperature)
    }
    // ane_utilization_reading, power_consumption_reading, frequency_reading follow the same shape
}
```
**Reason for change**: this is the single boundary that keeps the internal "no reading" encoding from being read as a real value anywhere it matters; every consumer that needs to distinguish absence from a genuine reading goes through one of these five functions rather than the raw field.

**File: `src/device/readers/apple_silicon_native.rs` (the healthy/degraded seam)**
```rust
fn build_gpu_info(
    static_info: &DeviceStaticInfo,
    apple_info: Option<&AppleSiliconInfo>,
    sample: Option<&NativeSample>,
) -> GpuInfo {
    ...
    detail.insert(
        "native_metrics".to_string(),
        if sample.is_some() { "available".to_string() } else { "unavailable".to_string() },
    );
    ...
    GpuInfo {
        utilization: sample.map_or(GPU_METRIC_UNAVAILABLE, |s| s.utilization),
        ane_utilization: sample.map_or(GPU_METRIC_UNAVAILABLE, |s| s.ane_power_mw),
        power_consumption: sample.map_or(GPU_METRIC_UNAVAILABLE, |s| s.power_watts),
        frequency: sample.map_or(0, |s| s.frequency),
        // identity and unified memory unaffected by `sample`
        ...
    }
}
```
**Reason for change**: this is the reader-level fix. `sample: None` is exactly the branch a macOS VM takes, testable directly without mocking IOReport itself.

**File: `src/api/metrics/gpu.rs` (the exposition-level fix, one of five identically-shaped guards)**
```rust
if let Some(utilization) = info.utilization_reading() {
    builder
        .help("all_smi_gpu_utilization", "GPU utilization percentage (omitted when the device reports no utilization)")
        .type_("all_smi_gpu_utilization", "gauge")
        .metric("all_smi_gpu_utilization", &base_labels, utilization);
}
```
**Reason for change**: an unconditional `builder.metric(...)` call becomes conditional on the reading actually being present, matching the exporter's own pre-existing convention for performance state and thermal thresholds.

**File: `src/network/metrics_parser.rs` (the remote-viewing boundary, the fix that keeps the reader/exporter fix from being local-only)**
```rust
// Start every live field at "no reading" rather than at zero. A scrape
// only overwrites the fields whose series it actually contains, so a
// zero default silently re-fabricated the exact value the exporter went
// out of its way to omit...
utilization: GPU_METRIC_UNAVAILABLE,
ane_utilization: GPU_METRIC_UNAVAILABLE,
...
power_consumption: GPU_METRIC_UNAVAILABLE,
```
**Reason for change**: without this, a correctly-omitting exporter and a correctly-N/A-rendering local TUI would still be defeated by `all-smi view --hosts`, which parses a scrape into a fresh `GpuInfo` and previously zero-initialized it before applying whatever series the scrape actually contained.

**File: `src/api/collection_loop.rs` (the energy-integration bug, fixed as a consequence of the encoding change)**
```rust
for gpu in &state.gpu_info {
    if let Some(watts) = gpu.power_consumption_reading() {
        samples.push((EnergyKey::gpu(gpu.hostname.clone(), gpu.uuid.clone()), watts));
    }
}
```
**Reason for change**: without the `if let Some(...)` guard, the sentinel `-1.0` would be summed directly into a running joule total, an unambiguous corruption the switch to an in-band sentinel made newly possible and this PR closes in the same change.

### 4.3 Data Model Changes

Not a schema change in the config or CLI sense; a metric-exposition contract change. Five Prometheus metric families become conditionally omitted rather than unconditionally emitted, for any `gpu_index` whose reader could not source that specific field. `all_smi_gpu_info` gains one new conditional label, `native_metrics`, on Apple Silicon rows. Internally, `GpuInfo`'s live fields keep their existing types (`f64`/`u32`, not `Option`), with the interpretation of specific out-of-range values redefined by this PR from "unused" to "the absence sentinel."

---

## 5. Learning Points

### 5.1 A shared data struct means a fix at one consumer is not a fix

**Concept**: when one struct (`GpuInfo`) feeds multiple independent consumers (an HTTP exporter, a TUI renderer, a snapshot writer, and, over a network hop, a remote-scrape parser reconstructing the same struct), a defect in how the struct represents "no data" has to be fixed at the struct's boundary with each consumer, not just at the one a bug report happened to be filed against.

**Application in this PR**: the issue as filed concerned the Prometheus exposition; fixing only `src/api/metrics/gpu.rs` would have left the TUI and, critically, the remote-viewing path in `src/network/metrics_parser.rs` still fabricating zeros, because that parser reconstructs a fresh `GpuInfo` from scratch on every scrape and needs its own absence-safe defaults independent of what the exporter now correctly omits.

### 5.2 Fixing "report zero" to "report absent" can convert a silent semantic bug into a silent arithmetic bug, if the encoding is not paired with skip-aware aggregation

**Concept**: switching an absence encoding from a plausible-looking value (`0`) to an explicit sentinel does not automatically make aggregations correct; it can make a previously merely-misleading average into an actively corrupting sum, if every summation site is not audited for the new encoding at the same time.

**Application in this PR**: the energy integrator's negative-joule bug is the sharp version of this: summing `0.0` into an energy total silently understated it (already wrong, but bounded), while summing `-1.0` would have made it decrease over time, an unambiguous corruption that only became *possible* because of this PR's own encoding choice, and was caught and fixed in the same change rather than shipped as a new defect.

### 5.3 A regression test for the *unaffected* case is as important as one for the fixed case

**Concept**: when a fix changes behavior conditionally (omit when absent, keep emitting when present), the case that must *not* change is exactly as much a contract as the case that must; a test asserting the old behavior persists where it should is what prevents a later "cleanup" from over-applying the new rule.

**Application in this PR**: `exporter_keeps_emitting_zero_ane_for_non_apple_gpus` exists specifically because non-Apple readers use a literal `0.0` for `ane_utilization` to mean "not applicable," which is structurally identical to the Apple Silicon "unavailable" case at the type level but must render differently (an explicit, meaningful zero, not an omission); without this test a future refactor unifying the "omit on sentinel" logic across all readers could silently break every existing NVIDIA/AMD scrape.

---

## 6. Further Learning

### Key Terms

| Keyword | Description | Relevance |
|---|---|---|
| `GPU_METRIC_UNAVAILABLE` | The `-1.0` sentinel written to `GpuInfo`'s `f64` fields when a reader has no reading | The core encoding decision of this PR, read back only through five named accessors |
| `native_metrics` label | New conditional label on `all_smi_gpu_info`, `"available"` / `"unavailable"` | The explicit, queryable reason for an omitted value series on Apple Silicon |
| `build_gpu_info(..., sample: Option<&NativeSample>)` | The extracted row-assembly seam in the Apple Silicon GPU reader | Lets a test drive the manager-unavailable branch without real hardware or mocking IOReport |
| `metrics::gpu_readings` | New module centralizing skip-aware aggregations (`total_power_watts`, `mean_utilization`, etc.) | The single place "skip absent readings" is implemented, used by the TUI, snapshot writer, and cluster aggregator alike |
| Prometheus omit-on-no-data convention | Absent series rather than a sentinel value on the wire | Already used by this exporter for performance state and thermal thresholds since #132; this PR extends it to five more families |

### Related Technologies and Frameworks

- Prometheus exposition conventions for representing missing data (`absent()`, the omit-rather-than-sentinel convention this PR follows).
- Rust's `Option<T>` versus an in-band sentinel value as competing representations of "no value," and the blast-radius argument for choosing the latter in an already-widely-consumed non-optional field.

### Related PRs and Issues

- Issue #325: the issue this PR closes.
- PR #323: the launchd CI job comment this PR corrects (it had documented the fabricated-zero behavior as expected).
- PR #333 (issue #324): added the unconditional `all_smi_up`/`all_smi_build_info` baseline, which is part of why this PR's exporter comment can say the target is "never silent" even when a device's value series are omitted (section 3.2).
- PR #334: left eight gauge-renderer sites with unchecked dimension arithmetic on the reasoning that they are unreachable below its 20-column floor; `src/ui/renderers/gpu_renderer.rs` is one of them, and this PR's diff there is confined to the five value readouts and the two gauges, not the dimension arithmetic PR #334 left alone.
- Issue #132: the prior work establishing the omit-when-absent convention for `all_smi_gpu_performance_state` and the thermal-threshold families, which this PR extends rather than reinvents.

---

## 7. Change Summary

### Statistics

| Item | Value |
|---|---|
| Files changed | 21 |
| Lines added | +1176 |
| Lines removed | -374 |
| Commits | 1 |
| New files | `src/metrics/gpu_readings.rs` |

### Changes by Category

| Category | Summary |
|---|---|
| Correctness | Apple Silicon GPU reader stops fabricating zeros; four macOS readers now follow one documented absence policy |
| Correctness | Remote-scrape parser (`metrics_parser.rs`) starts live GPU fields absent, so omission survives the network round trip |
| Bug fix (found during this PR) | Energy integrator could accumulate negative joules from the sentinel; fixed by skipping absent readings |
| Bug fix (found during this PR) | TUI history graphs plotted a dip that never happened when a GPU stopped reporting; fixed with skip-aware means |
| New module | `metrics::gpu_readings`: centralized skip-aware aggregations used across the TUI, snapshot, and cluster-metrics layers |
| Exposition | Five metric families (`all_smi_gpu_utilization`, `_power_consumption_watts`, `_temperature_celsius`, `_frequency_mhz`, `all_smi_ane_utilization`, `all_smi_ane_power_watts`) now conditionally omitted; `all_smi_gpu_info` gains `native_metrics` label on Apple Silicon |
| Cross-platform | `all_smi_gpu_frequency_mhz`/`_temperature_celsius` also now omitted (not `0`) for non-Apple readers that already used `0` to mean "no probe" |
| CI | Launchd job's `.github/workflows/ci.yml` comment corrected; new negative assertion confirms no fabricated `all_smi_gpu_utilization` on the real `macos-14` runner |

### Related Commits

| SHA | Type | Message |
|---|---|---|
| `e560b945` | fix | omit Apple Silicon GPU metrics instead of reporting 0 |

Merged to `main` as `11ccefa8`. Closes #325.

---

## 8. Follow-up Actions

### Required

None identified as blocking. The reader/exposition/TUI/remote-parser/aggregation chain is verified by unit and integration tests plus a real-runner CI assertion (Appendix A).

### Monitoring Required

- The PR itself notes the one thing not verified in its own development environment: "the actual degraded path on a machine without IOReport, which needs a macOS VM." The launchd smoke test, which runs on exactly such a host (`macos-14`), is the real check for this, and its new negative assertion (no fabricated `all_smi_gpu_utilization`) is the strongest available confirmation short of a dedicated macOS VM in the development loop itself.

### Future Improvements

- None proposed in the PR beyond the deliberately-not-taken `Option<f64>` refactor (section 3.1), which remains the type-correct target if a future PR is willing to absorb the roughly sixty-call-site blast radius on its own.

---

## Appendix

### A. Test Results

- `cargo fmt --check`: clean.
- `cargo clippy --lib --tests -j 9 -- -D warnings` and `cargo clippy --bin all-smi -j 9 -- -D warnings`: both clean, run separately since the crate compiles its module tree twice (the same class of check PR #319/#334 note).
- `cargo test --lib -j 9 device::readers::apple_silicon_native`: 8 passed.
- `cargo test --lib -j 9 api::metrics`: 71 passed; `metrics::gpu_readings`: 6 passed; `network::metrics_parser`: 51 passed; `ui::renderers::gpu_renderer`: 37 passed.
- `cargo test --lib -j 9` by module group: `ui::` 543, `network::` 127, `metrics::` 113, `device::` 169, `snapshot::` 47, `api::` 116, `app_state` 16, `parsing::` 19, all passing.
- `cargo test --test {device_tests,library_api_test,snapshot_test,thermal_pstate_integration_test,hardware_details_integration_test}`: 60 passed.
- Real hardware: `all-smi snapshot --format prometheus` on an M1 Ultra confirmed the healthy path is unaffected (all six families present, `native_metrics="available"`, and a genuine `all_smi_ane_power_watts ... 0` from a live subscription still publishes correctly).
- Not verified in the development environment specifically: the degraded path on a real IOReport-less machine, verified instead via the launchd CI job on `macos-14` (section 2.4 and the note in section 8).

### B. Performance Benchmarks

Not separately benchmarked. Per-field exposition checks are O(1); `metrics::gpu_readings`'s aggregation functions are single passes over the existing GPU slice with no new allocation in the common path.

### C. References

- Issue #325: root cause narrative, evidence (exact line numbers in the pre-fix reader), and acceptance criteria this report draws from, cross-checked against the diff.
- `src/device/macos_native/manager.rs`: the documented four-reader absence policy this PR establishes.
- Issue #132: prior art for the omit-when-absent Prometheus convention this PR extends.
- `.github/workflows/ci.yml`, launchd job: the real-runner assertion that is the strongest available confirmation of the degraded path.
