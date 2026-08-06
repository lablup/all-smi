# Technical Report: PR #317 - fix: derive Apple Silicon CPU cluster frequency from the pmgr table

**Date**: 2026-08-05
**Status**: Completed
**Languages**: Rust
**Risk Level**: Low (single file, additive table lookup, 13 new regression tests pinned against real hardware samples)

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

`all_smi_cpu_frequency_mhz`, `all_smi_cpu_p_cluster_frequency_mhz`, and `all_smi_cpu_e_cluster_frequency_mhz` read 0 on every Apple Silicon machine. The cause sits in one function, `IOReportMetrics::process_cpu_channel` in `src/device/macos_native/ioreport.rs`: it recovered a cluster's clock by parsing the IOReport performance-state *name* as a megahertz integer. Apple Silicon never names CPU states that way; an M1 Ultra reports `IDLE`, `V0P14`, `V1P13` ... `V14P0` for the performance cluster and a shorter `V0P4` ... `V4P0` run for the efficiency cluster. Every `parse::<i64>()` on those names failed, so the residency-weighted frequency sum stayed 0 and the reported clock collapsed to 0, while residency itself, computed on a separate accumulator, stayed correct and utilization was never wrong. The GPU path escaped the same bug because it already joined its residency histogram against an IOKit `AppleARMIODevice` pmgr `voltage-states9*` table; the CPU side had no equivalent lookup.

PR #317 gives the CPU path the same table join. `voltage-states*` properties are loaded once into a shared table list that both the GPU and CPU code now select from, the efficiency cluster reading `voltage-states1-sram` and the performance cluster `voltage-states5-sram`, with a length-match fallback that resolves clusters with no documented key (the M5 "Super" cluster). Verified on an Apple M1 Ultra: `/metrics` moves from 0/0/0 to 2646/3228/2064 MHz for the overall, performance-cluster, and efficiency-cluster metrics, and the TUI's CPU row moves from `Freq: 0+0MHz` to `Freq: 2.58+1.20GHz`. Two hardening changes came out of the same investigation: stripping the `DIE_<n>_` prefix multi-die packages (M1/M2 Ultra) put on every channel before classification, and correcting for 32-bit wraparound in the voltage-states payload, since clocks above 4.295 GHz do not fit the field and later Apple Silicon P-cores exceed it. Total: 1 file, +626/-119, one commit, closing #314.

---

## 1. Problem Statement

### 1.1 Background

Apple Silicon CPU and GPU frequency in all-smi comes from IOReport's `CPU Stats` / `CPU Core Performance States` and `GPU Stats` channel groups, which report residency (time spent in each performance state) but never a clock value directly. IOReport identifies a performance state only by a symbolic name, and turning that name into a megahertz figure requires a separate lookup table. The GPU path already had one: `load_gpu_frequencies` (as it was named before this PR) read the IOKit `AppleARMIODevice` pmgr node's `voltage-states9*` property and built a `Vec<u32>` of ascending clock values, one per active `GPUPH` state. The CPU path had no counterpart. `calc_freq_from_residencies` tried to recover a clock straight from the state name by parsing it as an integer, which is a strategy that only works if the platform names its states numerically.

### 1.2 Existing Issues

- **Issue 1 (every CPU frequency metric reads 0)**: Apple Silicon does not name CPU performance states numerically. Captured on an M1 Ultra: `DIE_0_PCPU_CPU0` reports `IDLE, V0P14, V1P13, V2P12, ... V14P0` (15 active states), `DIE_0_ECPU_CPU0` reports `IDLE, V0P4, V1P3, V2P2, V3P1, V4P0` (5 active states). Every `parse::<i64>()` against a name like `V0P14` fails, so the residency-weighted sum used to compute the average never accumulates anything and the average collapses to 0.
- **Issue 2 (utilization stayed correct, masking the bug)**: residency is accumulated on a separate counter from the frequency sum, so CPU utilization was always right even while frequency read 0 in the same scrape. This is why the defect could ship and persist without an obviously broken metric to catch it: `all_smi_cpu_frequency_mhz{...} 0` next to a correct, moving utilization number does not look like a crash, it looks like an idle machine.
- **Issue 3 (the GPU path's own table lookup did not generalize)**: `load_gpu_frequencies` was written specifically for the GPU, reading exactly the `voltage-states9*` key and returning a single `Vec<u32>`. Reusing the same pattern for two more clusters (P and E) needed a keyed, multi-table structure, not a copy of the GPU function per cluster.
- **Issue 4 (channel classification had two latent gaps)**: cluster classification used ad hoc `starts_with`/`contains` checks against the raw channel name. Multi-die packages (M1/M2 Ultra) prefix every channel with `DIE_<n>_`, so a name like `DIE_0_ECPU_CPU0` only matched the `contains("ECPU")` branch by substring accident; a hypothetical `DIE_0_MCPU0` would not have matched the M5 `starts_with("MCPU0")` rule at all, since the check ran against the unstripped name.
- **Issue 5 (32-bit hertz field cannot represent modern P-core clocks)**: the `voltage-states*` payload stores frequency as a 4-byte little-endian value in hertz. 4.295 GHz is the largest value that fits; later Apple Silicon P-cores exceed it, and a wrapped value reads as a small, wrong frequency rather than failing loudly.

This is pre-existing behavior, not a regression introduced by the concurrent Intel Mac work (#312): the released v0.25.0 Homebrew binary was run side by side with the #312 build and reproduced the same zeros, which places the regression window before both.

### 1.3 Risk Assessment

| Risk | Impact | Likelihood |
|---|---|---|
| CPU frequency silently reads 0 on every Apple Silicon deployment | Medium (a dashboard panel reading "0 MHz" next to correct utilization is confusing but not a crash; still, every P/E-cluster-aware view is unusable) | Certain prior to this fix, on every affected machine |
| A future chip's pmgr key numbering does not match the documented `voltage-states1/5-sram` keys | Low (the length-match fallback and the "documented key regardless of length" last resort both exist for this case) | Low; already exercised for the M5 Super cluster, which has no documented key at all |
| Wraparound correction misfires on a legitimate non-frequency payload | Low (the guard only activates after an already-accepted entry near the 32-bit ceiling, and rejected entries do not advance that tracker) | Low; covered by a dedicated regression test |

---

## 2. Technical Review

### 2.1 Correctness

The fix is purely additive at the data-flow level: a new `PMGR_VOLTAGE_STATES` table replaces the GPU-only `GPU_FREQUENCIES` cache as the single source loaded from IOKit, and `GPU_FREQUENCIES` becomes a thin selection over it (`select_gpu_frequency_table`), so the GPU path's existing behavior does not change shape, only its data source. `calc_gpu_freq_with_table` was renamed `calc_freq_with_table` and is now shared by the CPU cluster path, with no logic change to the residency-weighting itself. The plausibility range used to validate a parsed frequency (100 MHz–6 GHz, up from the GPU-only 100 MHz–4 GHz) is a superset that still rejects the values that used to trip the old GPU-only bound, since GPU clocks are always comfortably inside both ranges.

A defect class the review specifically checked for: whether extending the CPU path could regress the already-shipping GPU path. It cannot, structurally: `GPU_TABLE_KEYS` still resolves to `voltage-states9-sram`/`voltage-states9` first, and the CPU cluster keys (`voltage-states1*`, `voltage-states5*`) are disjoint property names, so table selection for one cluster can never accidentally return another's table by key match. The length-match fallback (matching a table by its entry count against the channel's active-state count) is the one path that could in principle cross clusters if two different clusters happened to have the same active-state count and the documented key lookups both missed; this is judged acceptable because the fallback only activates when the preferred key is entirely absent, and a wrong-but-plausible frequency from the wrong table is still bounded by the same 100 MHz–6 GHz sanity range.

### 2.2 Performance

`PMGR_VOLTAGE_STATES` and `GPU_FREQUENCIES` are both loaded exactly once, through `OnceLock`, at first use. The CPU path add one extra per-channel computation compared to before: counting non-idle residency entries (`active_states`) to pick the right table by length. This is O(number of performance states per channel), typically under 20, and runs once per collection cycle per channel, not per sample.

### 2.3 Compatibility and Dependencies

- **Breaking changes**: none. The wire format (`all_smi_cpu_frequency_mhz` and friends) is unchanged in name, type, and labels; only the values change, from always-0 to correct.
- **New dependencies**: none. The fix reads more properties off an IOKit node it already queried.
- **Compatibility**: GPU frequency reporting is verified unchanged (`all_smi_gpu_frequency_mhz` moved from 657 to 639 in the specific verification run, which is normal moment-to-moment variation under load, not a regression; the metric family and its computation path are untouched). The fix is scoped entirely to `src/device/macos_native/ioreport.rs` and only affects Apple Silicon; no other platform reader is touched.

### 2.4 Code Quality

13 new unit tests, all built from residency histograms and pmgr tables captured verbatim from the M1 Ultra used for verification, so the fixtures are real hardware data rather than synthetic ones. `test_cpu_performance_states_are_not_numeric` pins the original bug directly: feeding the real P-cluster histogram through the old strategy (`calc_freq_from_residencies` alone) yields 0 MHz with correct residency, which is the regression test for the defect itself, not just for the fix. `test_p_cluster_frequency_from_real_m1_ultra_sample` and its E-cluster counterpart assert the corrected values (3226 MHz at 72.10% residency, 1846 MHz at 73.14%) from that same input. The remaining tests cover table parsing (frequency acceptance, period-table rejection, 32-bit wraparound, a truncated payload), `DIE_` prefix stripping, cluster classification across single-die, multi-die, and M5 naming, and every branch of table selection.

---

## 3. Technical Decisions

### 3.1 One shared voltage-states table, keyed by property name, instead of a second GPU-shaped cache

**Context**: the GPU path already had a working, narrow solution: load exactly `voltage-states9-sram`/`voltage-states9` into a flat `Vec<u32>`. The CPU path needed two more tables (P and E clusters), plus a third with no documented key (M5 Super).

| Option | Pros | Cons |
|---|---|---|
| Copy the GPU loader per cluster, three near-identical functions | Minimal change to any one code path | Triples the IOKit property-scan logic; each copy has to be kept in sync by hand |
| **Chosen: one shared `PMGR_VOLTAGE_STATES` table of `(property name, frequencies)`, with per-cluster selection functions** | One scan of the pmgr node's properties; GPU and CPU both become thin selections over the same cache | Selection logic has to handle "documented key present", "length match fallback", and "no usable table" as three distinct outcomes |
| Parse only the specific keys each caller asks for, on demand, with no shared cache | Avoids loading tables nothing uses | Re-scans IOKit properties on every miss; loses the one-time-load property the `OnceLock` pattern was built around |

**Rationale**: the pmgr node publishes every `voltage-states*` property in one property dictionary scan regardless of which keys the caller ultimately wants, so scanning once and caching everything costs no more IOKit round trips than scanning for one key, and it is the only shape that lets a chip with an undocumented key numbering (the M5 Super cluster) still resolve through the length-match fallback, since that fallback needs to see every table, not just the ones with recognized names.

### 3.2 Strip the `DIE_<n>_` prefix before classification, rather than special-casing multi-die names in the match rules

**Context**: M1/M2 Ultra fuse two dies into one package and IOReport names every channel with a `DIE_0_`/`DIE_1_` prefix (`DIE_0_ECPU_CPU0`); single-die parts report the bare name (`ECPU0`). The old classification rules (`starts_with('E')`, `contains("ECPU")`, etc.) happened to still match the multi-die names, but only because `contains` does not care where in the string a substring appears.

**Finding**: this was fragile rather than correct. The M5 Super-cluster rule used `starts_with("MCPU0")`, which requires the match at position 0; a hypothetical `DIE_0_MCPU0` would fail that check even though `contains` rules elsewhere happened to still work. Nothing enforced that every classification rule tolerated the prefix; it was incidental.

**Chosen fix**: `strip_die_prefix` removes a leading `DIE_<digits>_` before any classification rule runs, so every rule, `starts_with` included, operates on the same normalized name regardless of package topology. This is verified directly with tests across single-die, multi-die, and M5 naming.

### 3.3 Correct 32-bit wraparound only after an entry already near the ceiling, never proactively

**Context**: the `voltage-states*` payload's frequency field is a 4-byte little-endian value; the field cannot represent a clock above `2^32 - 1` Hz (about 4.295 GHz). Later Apple Silicon P-cores are documented to exceed that.

| Option | Pros | Cons |
|---|---|---|
| Always add `2^32` to any value below the previous entry | Simple | Would misfire on a genuinely descending table, corrupting it |
| **Chosen: only apply the `+2^32` correction once the previous accepted entry is already at or above a 4 GHz guard, and only when the raw value dropped relative to it** | The wraparound signature (an ascending table that suddenly drops right after a near-ceiling entry) is exactly what this detects; a table that is not near the ceiling is never touched | Requires tracking the previous *accepted* value, not the previous raw value, so a rejected non-frequency entry cannot arm the guard |
| Widen the field to interpret payloads as 5+ bytes | Would require reverse-engineering a payload layout Apple has not published to support | Speculative; no evidence the on-disk layout changes at all, only that the value can wrap |

**Rationale**: `voltage-states*` tables are ascending by construction (each entry is a higher performance state than the last), so a value that drops immediately after an entry already near the 32-bit ceiling is diagnostic of a wraparound, not a real step down. Guarding on the *previous accepted* value, rather than the previous raw byte read, is what keeps a table of unrelated (non-frequency) payloads from spuriously triggering the correction, since rejected entries by construction never update that tracker.

---

## 4. Implementation Details

### 4.1 Architecture Changes

```
[Before]
GPU channel  --> load_gpu_frequencies() --> Vec<u32> (voltage-states9* only)
                                               |
                                               v
                                     calc_gpu_freq_with_table()

CPU channel  --> calc_freq_from_residencies()   # parses state NAME as MHz integer
                                                   Apple Silicon names are symbolic
                                                   (V0P14, ...) -> parse fails -> 0

[After]
pmgr node  --> load_pmgr_voltage_states() --> PMGR_VOLTAGE_STATES: [(key, Vec<u32>)]
                                                        |
                              +-------------------------+-------------------------+
                              v                                                   v
                 select_gpu_frequency_table()                     select_cpu_frequency_table(cluster, active_states)
                              |                                                   |
                              v                                                   v
                    calc_freq_with_table()  <-------- shared -------->  calc_freq_with_table()
                     (was calc_gpu_freq_with_table)

CPU channel classification: strip_die_prefix() -> classify_cpu_channel() -> CpuCluster
```

### 4.2 Key Code Changes

**File: `src/device/macos_native/ioreport.rs` (frequency-table selection for a CPU channel)**
```rust
let Some(cluster) = classify_cpu_channel(&item.channel) else {
    return;
};

// IOReport names CPU performance states symbolically (`IDLE`, `V0P14`,
// ... `V14P0`), never in megahertz, so the clock has to come from the
// pmgr voltage-states table for this cluster. Without that join every
// CPU frequency reads 0.
let active_states = residencies
    .iter()
    .filter(|(name, _)| !is_idle_state(name))
    .count();
let table = select_cpu_frequency_table(get_pmgr_voltage_states(), cluster, active_states);

let (freq, residency) = match table {
    Some(table) if !table.is_empty() => Self::calc_freq_with_table(&residencies, table),
    _ => Self::calc_freq_from_residencies(&residencies),
};
```
**Reason for change**: this is the join the CPU path never had. `select_cpu_frequency_table` prefers the documented key for the cluster, filtered to match the channel's own active-state count, then falls back to any table whose length matches, then as a last resort returns the documented key even if its length disagrees, on the reasoning that a partially mapped clock still beats reporting 0 MHz.

**File: `src/device/macos_native/ioreport.rs` (32-bit wraparound correction)**
```rust
let freq_hz = if prev_hz >= WRAP_GUARD_HZ && raw_hz < prev_hz {
    raw_hz + U32_SPAN_HZ
} else {
    raw_hz
};

if !(MIN_FREQ_HZ..=MAX_FREQ_HZ).contains(&freq_hz) {
    continue;
}

prev_hz = freq_hz;
frequencies.push((freq_hz / 1_000_000) as u32);
```
**Reason for change**: `prev_hz` only advances on an *accepted* entry, so a payload of non-frequency data (which never enters the accepted range) can never arm the wraparound guard; this is what keeps the plain `voltage-states1`/`voltage-states5` keys, which hold clock periods rather than hertz on this hardware, from being corrected into a plausible-looking wrong frequency instead of being cleanly rejected.

### 4.3 Data Model Changes

Not a wire-format change. Internally, the GPU-only `GPU_FREQUENCIES: OnceLock<Vec<u32>>` cache is now backed by a new, more general `PMGR_VOLTAGE_STATES: OnceLock<Vec<(String, Vec<u32>)>>`, and `calc_gpu_freq_with_table` is renamed `calc_freq_with_table` to reflect that it is no longer GPU-specific. No Prometheus metric name, type, or label changed.

---

## 5. Learning Points

### 5.1 A residency histogram and a frequency table are two independent pieces of hardware knowledge

**Concept**: IOReport tells you *how long* a block spent in each performance state (residency), never *what clock* that state runs at. On Apple Silicon the state names are symbolic version tags (`V0P14`), not clock values, so recovering a megahertz figure always requires a second data source that maps state index to clock, which on this platform is the IOKit pmgr node's `voltage-states*` properties.

**Application in this PR**: the GPU path already understood this and joined against `voltage-states9*`. The CPU path's `calc_freq_from_residencies` silently assumed the state name itself would parse as a number, which happens to be true on some other Apple product lines' IOReport channels but never on Apple Silicon CPU clusters.

**Example**:
```rust
fn select_cpu_frequency_table<'a>(
    tables: &'a [(String, Vec<u32>)],
    cluster: CpuCluster,
    active_states: usize,
) -> Option<&'a [u32]> {
    // 1. documented key, length-checked
    // 2. any table whose length matches, preferring an `-sram` suffix
    // 3. documented key regardless of length, as a last resort
}
```

### 5.2 A bug that leaves a correct, moving neighbor metric is harder to notice than a crash

**Concept**: residency (and therefore utilization) and frequency were computed from the same IOReport sample but through separate accumulators. When the frequency accumulator broke, the utilization accumulator kept working, so the scrape output looked like "correct utilization, frequency stuck at 0" rather than an obviously broken subsystem.

**Application in this PR**: this is exactly why the defect could ship and persist: `all_smi_cpu_frequency_mhz 0` next to `all_smi_cpu_utilization_percent 34.2` in the same scrape reads as plausible if you are not specifically looking at the frequency panel. The fix does not change how residency and frequency are computed (they remain separate accumulators), it only fixes the frequency accumulator's data source.

### 5.3 32-bit field wraparound in a device-reported binary payload is only safe to correct directionally

**Concept**: when a fixed-width field can wrap, correcting for it requires a directional assumption (the underlying sequence is monotonic) plus a guard that only activates near the wrap boundary, or the correction itself becomes a source of corruption on legitimate data that happens to decrease.

**Application in this PR**: `voltage-states*` tables are ascending by construction, so a drop after an entry already near 4.295 GHz is diagnostic of wraparound specifically, not of a real frequency drop. The guard's precision, tracking only the previous *accepted* value, is what keeps a non-frequency payload (the plain `voltage-states1`/`voltage-states5` keys, which hold clock periods on this hardware) from ever entering the correction path in the first place, since those entries are rejected before `prev_hz` advances.

---

## 6. Further Learning

### Key Terms

| Keyword | Description | Relevance |
|---|---|---|
| IOReport | Apple's low-level API for power and performance telemetry, exposing residency histograms per channel | Source of both the CPU and GPU performance-state data this PR joins against a frequency table |
| pmgr / clpc | The IOKit `AppleARMIODevice` node publishing `voltage-states*` properties | Where the actual clock-to-state mapping lives; this PR's core fix is joining against it from the CPU path |
| `voltage-states*` | Per-cluster property holding ascending `(frequency, voltage)` pairs, 8 bytes each | The table this PR now reads for CPU clusters, not only the GPU |
| Residency | Fraction of the sampling window a performance state was active | Computed on a separate accumulator from frequency; stayed correct throughout the whole bug |
| 32-bit wraparound | A fixed-width field overflowing and restarting from 0 | Corrected in this PR for clocks above 4.295 GHz |
| DIE_\<n\>\_ prefix | IOReport channel-name prefix on multi-die packages (M1/M2 Ultra) | Stripped before classification so multi-die and single-die channel names share one rule set |

### Related Technologies and Frameworks

- IOReport and IOKit: Apple's telemetry and driver-matching frameworks, both used without any elevated privilege on this platform.
- Apple Silicon performance-state naming (`V<n>P<m>`): an internal, undocumented naming scheme that this PR treats as opaque and never attempts to parse for a value.

### Related PRs and Issues

- Issue #314: the issue this PR closes.
- PR #312 (Intel Mac support, issue #306): ruled out as the source of this regression by side-by-side comparison against the released v0.25.0 binary.
- Section 2.3 of this report notes the GPU metric family is verified unaffected.

---

## 7. Change Summary

### Statistics

| Item | Value |
|---|---|
| Files changed | 1 (`src/device/macos_native/ioreport.rs`) |
| Lines added | +626 |
| Lines removed | -119 |
| Commits | 1 |
| New unit tests | 13 |

### Changes by Category

| Category | Summary |
|---|---|
| Correctness | CPU cluster frequency now resolves through a pmgr voltage-states table join, matching the pre-existing GPU strategy |
| Hardening | `DIE_<n>_` prefix stripping before classification; 32-bit wraparound correction for clocks above 4.295 GHz |
| Refactor | `calc_gpu_freq_with_table` renamed `calc_freq_with_table` and shared by GPU and CPU paths; `GPU_FREQUENCIES` becomes a selection over the new shared `PMGR_VOLTAGE_STATES` cache |
| Tests | 13 new unit tests built from residency histograms and pmgr tables captured verbatim from an M1 Ultra |

### Related Commits

| SHA | Type | Message |
|---|---|---|
| `456bf7d4` | fix | derive Apple Silicon CPU cluster frequency from the pmgr table |

Merged to `main` as `02b2e6d2`. Closes #314.

---

## 8. Follow-up Actions

### Required

None identified. The fix is verified against real M1 Ultra hardware for both the API metrics output and the TUI renderer, and the underlying investigation (residency-versus-frequency separation, table selection, wraparound) is covered by regression tests built from real captured data rather than synthetic fixtures.

### Monitoring Required

- Whether a future Apple Silicon generation introduces a CPU cluster whose pmgr key numbering does not match `voltage-states1*`/`voltage-states5*` and whose active-state count collides with another cluster's, which is the one scenario the length-match fallback cannot disambiguate on its own. The M5 Super cluster already exercises the no-documented-key case safely; a genuine collision has not been observed.

### Future Improvements

- None proposed in the PR. The fix is scoped tightly to the frequency computation defect identified in the issue.

---

## Appendix

### A. Test Results

- `cargo test --lib device::macos_native`: 49 passed.
- `cargo test --lib device::cpu_macos`: 9 passed.
- `cargo test --lib ui::renderers::cpu_renderer`: 8 passed.
- `cargo clippy --lib --tests -- -D warnings`: clean.
- `cargo fmt --check`: clean.
- `all-smi api` on an Apple M1 Ultra, `/metrics` before and after:

```
# before
all_smi_gpu_frequency_mhz{gpu="Apple M1 Ultra GPU",...} 657
all_smi_cpu_frequency_mhz{cpu_model="Apple M1 Ultra",...} 0
all_smi_cpu_p_cluster_frequency_mhz{cpu_model="Apple M1 Ultra",...} 0
all_smi_cpu_e_cluster_frequency_mhz{cpu_model="Apple M1 Ultra",...} 0

# after
all_smi_gpu_frequency_mhz{gpu="Apple M1 Ultra GPU",...} 639
all_smi_cpu_frequency_mhz{cpu_model="Apple M1 Ultra",...} 2646
all_smi_cpu_p_cluster_frequency_mhz{cpu_model="Apple M1 Ultra",...} 3228
all_smi_cpu_e_cluster_frequency_mhz{cpu_model="Apple M1 Ultra",...} 2064
```

Sampled repeatedly over a minute: values track load and stay inside hardware limits (P-cluster 3017–3228 MHz against a 3228 MHz table maximum; E-cluster 1106–2064 MHz against a 2064 MHz maximum).

- TUI verification: since a pty has no window size and `ui/chrome.rs` panics on a zero-width terminal, the renderer was driven directly with a live `CpuInfo` taken off real hardware, through the same `print_cpu_info` function the TUI calls:

```
# before
p_cluster_frequency_mhz: Some(0), e_cluster_frequency_mhz: Some(0)
CPU  Apple M1 Ultra @ cube.loca  Arch:arm64  Sockets: 1  Cores:16P+ 4E  Freq:       0+0MHz  Temp: 56C

# after
p_cluster_frequency_mhz: Some(2583), e_cluster_frequency_mhz: Some(1195)
CPU  Apple M1 Ultra @ cube.loca  Arch:arm64  Sockets: 1  Cores:16P+ 4E  Freq: 2.58+1.20GHz  Temp: 53C
```

The probe used for this was removed before committing; no test file other than `ioreport.rs` is touched.

### B. Performance Benchmarks

Not separately benchmarked. The added per-channel cost is counting non-idle residency entries (typically under 20) once per collection cycle per channel; the pmgr table itself is loaded exactly once per process via `OnceLock`.

### C. References

- Apple: IOReport (undocumented, reverse-engineered by community tooling such as `mactop`, whose approach the original GPU loader was explicitly based on)
- Apple: IOKit `AppleARMIODevice`, pmgr/clpc nodes, `voltage-states*` properties
- Issue #314: root cause narrative and verification data this report draws from, cross-checked against the diff
