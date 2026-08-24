# Technical Report: PR #360 - Promote GPU Fan Speed to a First-Class `GpuInfo` Field

**Date**: 2026-08-07  
**Status**: Completed  
**Related**: PR #360, Issue #352, follow-up filed from PR #351  
**Risk Level**: Medium (touches 49 files, adds a new exported metric family)

---

## Executive Summary

Fan speed was published only through the untyped `detail` map, so it reached `all-smi snapshot` output but never the TUI or the Prometheus exporter, and consumers had to parse `"1450 RPM"` back out of a string that four readers agreed on by convention alone. PR #360 promotes it to `GpuInfo::fan_speed_rpm` and wires it through the exporter, the remote-metrics parser, and the GPU view.

The typed field is `Option<u32>` with `#[serde(default)]`, so snapshots and remote payloads produced before it existed still deserialize. `None` means the device reports no tachometer, and every consumer renders nothing rather than substituting `0`, which would be indistinguishable from a seized fan.

---

## 1. Problem Statement

`Source: Fan` already exported as `source__fan` while the value it described did not export at all. Four readers had the data and no typed channel to put it in:

| Reader | Source |
|--------|--------|
| `amd.rs` | Linux amdgpu sensors |
| `intel_gpu_linux.rs` | hwmon `fan1_input` |
| `intel_gpu_level_zero/apply.rs` | Sysman fan family |
| `amd_adl.rs` | Windows PMLog |

Every consumer that wanted the number had to re-parse a string, and the string's shape differed between readers (`"1450 RPM"` against the Level Zero `"1600 RPM (40%)"`), so each consumer would have had to know all of them.

## 2. Change Summary

| Item | Value |
|------|-------|
| Files changed | 49 |
| Lines added | 922 |
| Lines deleted | 43 |
| New metric family | `all_smi_gpu_fan_speed_rpm` |
| Construction sites updated | 48 (all taking `None`) |

### Files of substance

| File | Change |
|------|--------|
| `src/device/types.rs` | `fan_speed_rpm: Option<u32>` with `#[serde(default)]`, documenting that `None` means "no tachometer" and must never render as `0`. |
| Four readers | Set the typed field from the same source value, in the same branch, as the existing `Fan Speed` detail write. |
| `src/api/metrics/gpu.rs` | Exports the gauge, gated on presence, following the `all_smi_gpu_performance_state` block. |
| `src/network/metrics_parser.rs` | Reconstructs the field from the exported series, with a new `MAX_GPU_FAN_RPM` ceiling of 100000. |
| `src/ui/renderers/gpu_renderer.rs` | Renders `Fan:1450rpm` on the existing thermal and P-state secondary row. |
| `API.md`, `docs/LIB_mode.md` | Documented the metric and the field. |

## 3. Technical Decisions

### 3.1 `None` renders as absent, never as `0`

A substituted `0` is indistinguishable from a seized fan, which is the one reading an operator most needs to be able to trust. Passively cooled datacenter cards and drivers that expose only a duty cycle both legitimately have no tachometer, so absence is a normal state rather than an error, and it has to stay distinguishable from a real zero.

### 3.2 The detail writes are deliberately kept

Snapshots depend on them, and `intel_gpu_level_zero::apply_fan` uses `detail.contains_key("Fan Speed")` as the cross-reader overwrite guard that lets a Linux hwmon reading outrank a later Level Zero sample. Removing the string would have removed that guard.

Both writes sit behind the same early returns, so the field and the string always describe the same sample. That invariant is what makes the exporter's fallback in 3.4 safe.

### 3.3 A duty-cycle-only readout leaves the typed field unset

A Level Zero readout carrying only a percentage (`rpm == None`, `percent == Some(40)`) does not populate `fan_speed_rpm`. A percentage stored in a field named `_rpm` would be exported as a wildly wrong RPM. The percentage still reaches snapshots through the detail string, so no information is lost, only mislabelled information is prevented.

### 3.4 The exporter falls back to parsing the legacy string

`src/api/metrics/gpu.rs` prefers the typed field and falls back to parsing the detail string, so mock servers and remote nodes running older builds keep exporting the series through a mixed-version fleet. The parser handles both `"1450 RPM"` and the Level Zero `"1600 RPM (40%)"` shape and rejects a bare `"40%"`.

The chosen metric name matches what `src/mock/templates/amd_gpu.rs` already emits, so mock fleets populate the field end to end with no mock change.

### 3.5 The renderer's row predicate is shared with the layout line count

`gpu_renderer.rs` renders fan speed on the secondary row that already carries thermal thresholds and P-state. The "does this row exist" predicate is now a shared helper used by both the renderer and the layout line count, so the reserved line count and what is actually drawn cannot drift apart.

That matters specifically here: AMD and Intel cards report none of the NVML thresholds, which makes fan speed the only thing that opens the row for them. A drifted predicate would have shown as a blank reserved line on exactly those cards.

## 4. Validation Results

| Gate | Result |
|------|--------|
| `cargo check --lib --tests` | pass |
| `cargo clippy --lib --tests -- -D warnings` | pass, also with `--features level_zero` |
| `cargo fmt --check` | pass |

| Test target | Passed | Covers |
|-------------|--------|--------|
| `api::metrics::gpu` | 15 | present when `Some`, absent when `None`, detail fallback, structured field wins over a conflicting detail string, duty-cycle-only emits nothing, table-driven parser over every reader value shape |
| `network::metrics_parser` | 54 | genuine exporter-to-parser round trip through `GpuMetricExporter`, absence stays `None`, out-of-range and fractional values rejected |
| `device::readers::amd_adl` | 24 | typed field asserted alongside the existing detail assertions, empty readout leaves both unset |
| `device::readers::intel_gpu_linux` | 31 | one hwmon `fan1_input` read reaches both field and string; a card without the file leaves both unset |
| `device::readers::intel_gpu_level_zero` | 47 | hwmon priority covers the typed field, duty-cycle-only leaves it unset, L0 fills a gap the hwmon baseline left |
| `ui::renderers::gpu_renderer` | 41 | rendered when present, omitted when absent, no double leading space when fan is the only field, correct separation from neighbours, fan independently bumps the layout line count |
| `snapshot_test` | 13 | serialization round trip |
| `thermal_pstate_integration_test` | 4 | the shared secondary row |

**Not verified**: `cargo check --target x86_64-pc-windows-gnu --lib` fails on the development host in `zstd-sys` because the mingw cross-compiler is not installed, which is unrelated to this change. The ADL code path that changed (`apply_to_gpu_info`) is not `cfg`-gated and its tests do run on Linux. The Windows-gated `GpuInfo` literals in `amd_windows.rs`, `intel_gpu_windows.rs`, and `windows_gpu_perf.rs` only gained `fan_speed_rpm: None` and were reviewed by hand.

## 5. Outcome and Follow-up

- PR #360 was squash-merged into `main` as `ceaea01`.
- Issue #352 closed automatically through the PR's `Closes #352` link.
- `all_smi_gpu_fan_speed_rpm` moved out of the AMD-only table in `API.md` into the shared GPU metrics table with the real label set, since Intel emits it too.
- Backward compatibility holds in both directions: `#[serde(default)]` accepts old payloads, and the exporter's detail fallback keeps old remote nodes exporting the series.

---

## Appendix: Key Terms

| Keyword | Description | Relevance |
|---------|-------------|-----------|
| tachometer | The fan sensor reporting actual revolutions | What `None` means the device lacks |
| duty cycle | A fan control percentage, not a measured speed | Why a percentage must not be stored in an `_rpm` field |
| `#[serde(default)]` | Deserializes a missing field to its default instead of failing | What keeps pre-field snapshots readable |
| cross-reader overwrite guard | `detail.contains_key` check letting one reader outrank another | Why the detail string writes were kept |
