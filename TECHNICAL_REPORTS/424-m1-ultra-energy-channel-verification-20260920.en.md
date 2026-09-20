# Technical Report: PR #424 - M1 Ultra Energy Channel Verification

**Date**: 2026-09-20
**Status**: Completed
**Languages**: Rust, Markdown
**Risk Level**: Low

---

## Executive Summary

PR #424 verifies the Apple Silicon energy-model rules from #410 against real hardware for the first time on a multi-die chip, an M1 Ultra (Mac13,2, macOS 27.0 26A428), and pins the result with a 321-channel inventory fixture and 15 tests. The measurements answered the open questions: only CPU channels carry the `DIE_<n>_` prefix, so no new die-prefix rule was needed; `GPU0_0` turned out to be the whole GPU minus its SRAM rather than die 0, so the GPU fallback was extended to that name shape; and `sum_rails` gained a guard that lets only the least specific channel family present feed a rail, so a per-die sum can never be added to a package total. The diagnostic was moved into its own module and now prints the SMC temperature key inventory alongside the energy comparison.

---

## 1. Problem Statement

### 1.1 Background

#410 (PR #412) computes Apple Silicon power from each `Energy Model` channel's own driver publication timestamp rather than the poll window, and matches channels by exact name in `classify_energy_channel`. All of it was verified on a single chip, an M5 Max, whose 364-channel inventory is the only recorded evidence for rules that are written to cover every Apple Silicon part.

An Ultra is two dies, and nothing in the M5 Max inventory says how a two-die package names its channels. GitHub macOS runners are virtual machines without IOReport, so the question could only be answered on real hardware.

### 1.2 Existing Issues

- **Unknown channel names on multi-die packages**: whether the GPU, ANE, and DRAM blocks carry `DIE_<n>_` prefixes the way the CPU does was a guess, and a rail channel the classifier misses is not even subscribed, so it silently reads 0 W.
- **A latent double count**: a package `CPU Energy` sitting next to `DIE_<n>_CPU Energy` would match both rules and be summed twice, and `multi_die_rails_are_summed` assumed only the per-die channels exist.
- **Unverified cadence on a second chip**: the batched-publication behaviour, the short split tails, and the timestamp fallbacks were all M5 Max observations.
- **SMC aggregation differed by chip with no way to see it**: the temperature getters prefer a static key list and fall back to discovery only when no static key reads in range, so which path a rail takes is chip-dependent, and the diagnostic printed none of it.

### 1.3 Risk Assessment

| Risk | Impact | Likelihood |
|------|--------|------------|
| A multi-die chip's rail channel classifies as nothing, is never subscribed, and reports 0 W | High | Medium |
| A package channel and per-die channels are both summed, inflating a rail by roughly 2x | High | Low |
| A rule is generalized from one chip and shipped as if it were verified | Medium | High |
| Temperature aggregation is assumed uniform across chips when it is not | Low | High |

---

## 2. Technical Decisions

### 2.1 No New `DIE_<n>_` Rule for GPU, ANE, or DRAM

The M1 Ultra inventory settles it by evidence rather than by design taste. Of its 321 channels, only 34 carry the `DIE_<n>_` prefix and all of them are CPU channels: clusters, per-core channels, `_CPM`, and one `DIE_<n>_CPU Energy` per die. Every other block names its die with a suffix instead: `ANE0_0` and `ANE0_1`, `DRAM0_0` and `DRAM0_1`, and the same shape for `ISP`, `AVE`, `MSR`, `DCS`, and `AMCC`.

No recorded chip has a `DIE_<n>_` GPU, ANE, or DRAM channel, so adding a rule for one would mean writing a matching rule against no data. The decision is recorded in the `energy.rs` module docs together with the inventory that justifies it, which is what acceptance criterion 4 of #415 asked for.

### 2.2 Extend the GPU Fallback to `GPU<n>_<m>`, and Exclude a Bare `GPU`

The M1 Ultra has a single `GPU0_0` (plus `GPU SRAM0_0`) next to `GPU Energy`. The old rule matched `GPU<n>` only, so `GPU0_0` classified as nothing and was not even subscribed.

Whether `GPU0_0` means "die 0" or "the whole GPU" decides whether a chip without `GPU Energy` would read half its GPU power. The comparison run answers it:

| run | `GPU0_0` / `GPU Energy` | `(GPU0_0 + GPU SRAM0_0)` / `GPU Energy` |
|---|---|---|
| official idle, 29.292 s window | 0.963 | 0.996 |
| official loaded, 29.278 s window | 0.938 | 0.967 |
| post-change run, 30.066 s window | 0.973 | 1.004 |

A die-0-only channel would read near 0.5. `GPU0_0` is the whole GPU minus its SRAM, so the fallback is safe to extend to that name shape, and a chip that fell back to it would read roughly 3 % low. A bare `GPU` is explicitly excluded, because nothing has shown what it contains.

On the M1 Ultra itself the extension changes no reading: `GPU0_0` is subscribed and tracked, but `GPU Energy` is present, so the GPU rail is still `GPU Energy` alone.

### 2.3 `sum_rails` Counts a Package Channel or the Per-Die Ones, Never Both

Rather than trusting that no chip has both, the summation now enforces it on every rail. Channels are bucketed by how specific their name is, and only the least specific family present in a sample feeds the rail.

```rust
struct Families([Option<f64>; 3]);
// 0: package name (`CPU Energy`, `ANE`, `DRAM`)
// 1: numbered block (`GPU<n>`, `ANE<n>`, `DRAM<n>`; `DIE_<n>_CPU Energy` for the CPU)
// 2: per-die suffix (`GPU<n>_<m>`, `ANE<n>_<m>`, `DRAM<n>_<m>`)
```

The package `CPU Energy` overrides `DIE_<n>_CPU Energy`; without `GPU Energy` the fallback prefers `GPU<n>` over `GPU<n>_<m>`; ANE and DRAM take bare `ANE` / `DRAM`, then `ANE<n>` / `DRAM<n>`, then the per-die suffix.

Every channel still classifies, so the subscription filter and the tracker keep them all and the decision is made per sample, at summation time. No recorded chip exercises the guard: both the M5 Max and the M1 Ultra have exactly one family per rail, so both read exactly as before. The guard is for the multi-die chips nobody has recorded yet, such as the M2 Ultra and M3 Ultra.

### 2.4 The Diagnostic Compares Candidates Instead of Only Tracked Channels

The old diagnostic sampled the filtered subscription, so a rail channel the classifier missed was invisible to the very run meant to find it. It now opens an unfiltered `Energy Model` subscription for 32 s and compares every GPU, ANE, DRAM, and `CPU Energy` candidate over a common publication window, printing joules, watts, per-channel publication spans, stamp anomalies, and the ratios above. That is how `GPU0_0` was measured before any rule accepted it.

---

## 3. Implementation Details

### 3.1 Fixture and Tests

`tests/fixtures/ioreport/m1_ultra_energy_model.tsv` holds all 321 channels as `name<TAB>unit` in enumeration order, with a header recording chip, model identifier, macOS build, and capture date, matching the M5 Max fixture's format. It was enumerated with `IOReportCopyChannelsInGroup` rather than sampled from the filtered subscription, and the two official runs produced it byte for byte.

The classification tests pin that exactly 8 of the 321 channels feed a rail:

```
ANE0_0, ANE0_1, DIE_0_CPU Energy, DIE_1_CPU Energy,
DRAM0_0, DRAM0_1, GPU Energy, GPU0_0
```

The same list is asserted from the filter side in `channel_filter.rs`, which is what keeps a classifier change from silently changing what the subscription opens with. The remaining 313 channels are the same families per die, `GPU SRAM0_0`, and the `apciec<n> Energy` and `PCIe Port <n> Energy` channels whose trailing ` Energy` matches no rule.

One inventory property needed its own test: the 130 `DTL` names (`ECPUDTL*`, `PCPUDTL*`, `PCPU1DTL*`) each appear twice with no prefix to tell the two apart. `EnergyTracker` keys channels by name, so a duplicated name that classified would be ambiguous. None of them classifies, and a test pins that.

### 3.2 Cadence

The M1 Ultra publishes its mJ channels together, like the M5 Max, but twice per roughly 2.1 s at uneven intervals whose split drifts: 0.75 to 0.91 s alternating with 1.19 to 1.32 s in one 32 s run, and moving from 1.69 + 0.46 s to 0.97 + 1.14 s in another. Setting the short split tails aside, every publication came 2.03 to 2.13 s after the one two before it when idle and 2.02 to 2.20 s under load. `GPU Energy` publishes on a far shorter span, 110 to 246 ms.

Each publication's energy follows its own span, which is the property the whole design rests on: `DRAM0_0` read 1.75 to 1.83 W over every printed span of the idle run (815 to 1295 ms) and 2.00 to 2.20 W under load (416 to 1690 ms). `MIN_PUBLICATION_SPAN_NS` stays at 50 ms; the M1 Ultra's split tails are 12 to 29 ms, and its `GPU Energy` also moved its stamp 6 to 12 ms with no energy when a sample landed just before the next publication, both of which the existing threshold folds into the following span.

### 3.3 SMC Temperature Key Inventory

The diagnostic now prints which static keys exist and read in range, how many keys discovery finds, and which path each rail takes, using the same key lists and range the getters use. `smc.rs` was refactored so those are single-sourced constants (`CPU_STATIC_TEMP_KEYS`, `GPU_STATIC_TEMP_KEYS`, `PLAUSIBLE_TEMP_C`) instead of four inline literals; aggregation is unchanged.

The M1 Ultra result shows how far apart two chips can be:

| | M1 Ultra | M5 Max |
|---|---|---|
| static CPU keys present | 6 of 8 (`TC0P`, `TC0D` absent) | 0 of 8 |
| static GPU keys present | 1 of 4 (`Tg0j`) | 1 of 4 (`Tg0j`) |
| discovered CPU keys | 86 | 23 |
| discovered GPU keys | 16 | 84 |
| CPU temperature path | static, mean of 6 | discovery, mean of 23 |
| GPU temperature path | static, single sensor | static, single sensor |

On the idle run `get_cpu_temperature` returned 59.06 C and `get_gpu_temperature` 51.14 C; under load the CPU read 65.50 C. The `MAX_CPU_TEMP_KEYS` runaway guard comment was updated with the measured 86.

### 3.4 Module Layout

The diagnostic moved out of `ioreport.rs` into `ioreport/diagnostics.rs`, with the candidate comparison in `ioreport/energy_comparison.rs` and the SMC key report in `smc/temperature_report.rs`. `ioreport.rs` lost 112 lines and gained the M1 Ultra cadence to its module docs.

---

## 4. Learning Points

### 4.1 A Name Shape Is Not a Semantic

`GPU0_0` reads like "GPU block 0, die 0" and on a two-die package the obvious inference is that it covers half the GPU. The measurement says otherwise: it is the whole GPU minus its SRAM, which is why `(GPU0_0 + GPU SRAM0_0)` lands within 0.4 % of `GPU Energy` on the post-change run. Extending a fallback rule to a name shape is a claim about what that channel contains, and only a side-by-side comparison over a common window can support it.

### 4.2 A Filtered Diagnostic Cannot Find What the Filter Drops

The classifier was the subscription predicate, so the diagnostic sampled exactly the channels the classifier already accepted. Any rail channel it missed was invisible to the tool built to find missing rail channels. Verification tooling has to sit outside the filter it is verifying.

### 4.3 Guard the Combination Before a Chip Produces It

No recorded chip has both a package channel and per-die channels on the same rail, so the double-count guard changes no current reading and cannot be exercised on hardware available today. It was still worth adding, because the failure mode is a silent 2x on a power figure, and the alternative is discovering it on a chip nobody has yet. The honest form of that is to say so: the module docs state plainly that no recorded chip exercises the guard and that it exists for the M2 Ultra and M3 Ultra class of part.

---

## 5. Change Summary

### Statistics

| Item | Value |
|------|-------|
| Files changed | 12 |
| Lines added | +2025 |
| Lines deleted | -172 |
| Tests added | 15 |
| Fixtures added | 1 (`tests/fixtures/ioreport/m1_ultra_energy_model.tsv`, 321 channels) |

### Changes by Category

| Category | Count | Summary |
|----------|-------|---------|
| Verification | 2 | M1 Ultra inventory fixture, classification and cadence tests |
| Correctness | 2 | `GPU<n>_<m>` fallback, package-over-per-die guard in `sum_rails` |
| Tooling | 3 | Diagnostic extracted to its own module, unfiltered candidate comparison, SMC key report |
| Code Quality | 1 | SMC static key lists and plausible range single-sourced as constants |
| Documentation | 4 | `energy.rs` multi-die section, `ioreport.rs` cadence, `channel_filter.rs` table, `docs/ARCHITECTURE.md` |

### Related Commits

| Hash | Type | Message |
|------|------|---------|
| `f10978b` | test | Verify IOReport energy channels and SMC keys on an M1 Ultra |
| `1ec6945` | update | Count package or per-die energy channels, never both |
| `833cc55` | update | Count one ANE and DRAM family per sample, as for CPU and GPU |
| `6902cbd` | squash merge | Merge PR #424 into `main` |

---

## 6. Validation and Follow-up

### Completed Validation

- `cargo test --lib device::macos_native`: 107 passed, the hardware diagnostic ignored.
- `cargo test --test user_facing_text_test`, `cargo clippy --lib --tests -- -D warnings`, `cargo fmt --check`, and the pre-commit hook's `cargo clippy --all-targets --all-features -- -D warnings`.
- Two official `ioreport_energy_diagnostics` runs on the M1 Ultra, idle on a busy shared host and under four `yes` loads, both attached to the PR. Rail sums over the common window: idle cpu 5.940 W, dram 3.644 W, gpu 0.038 W, ane 0.000 W; loaded cpu 17.012 W, dram 3.972 W, gpu 0.081 W, ane 0.049 W. No stamp anomalies on any tracked channel in either run.
- Release load check at `833cc55`: `all-smi api --port 19090 --interval 1` under four `yes` loads, 3 s warm-up, 20 samples one second apart. CPU power 14.09 to 15.49 W, mean 14.76, no 0 W sample. The diagnostic ran over the same window and read a tracker CPU rail mean of 14.95 W over 272 ticks with no zero tick, against 14.84 W from the two `DIE_<n>_CPU Energy` channels over the 30.1 s common window. This is acceptance criterion 3 of #415.
- The M5 Max fixture and its tests pass unchanged, which is the constraint #415 set on any classifier change.

### Not Verified

- `GPU0_0` against `GPU Energy` with the GPU actually busy. Both official runs had the GPU at 38 to 81 mW. Only an unattached exploratory run had it active at about 7.5 W, reading 0.980 alone and 1.000 with `GPU SRAM0_0`.
- The 0.938 ratio in the loaded run is a 6.2 % gap, past a strict reading of "within a few percent". The review left this open as a MEDIUM item: the SRAM split rules out a die-0-only channel, and the GPU-active evidence is disclosed as exploratory.
- The package-over-per-die guard on real hardware. No recorded chip has both families on one rail.
- A fully frozen `GPU Energy` stamp occurred only in unattached exploratory runs sampling every 200 ms with the GPU near 8 W, not in the official runs. The existing hold tests cover it.
- Every other chip: M1, M1 Pro, M1 Max; M2, M2 Pro, M2 Max, M2 Ultra; M3, M3 Pro, M3 Max, M3 Ultra; M4, M4 Pro, M4 Max; M5 and M5 Pro. Intel Macs are out of scope.

### Remaining Constraints

- `energy.rs`: a package channel with no closed span, or a stale one, is summed as 0 W and masks live per-die readings. This is intended and pinned by a test, and is reachable only on a chip that has both families.
- `diagnostics.rs`: the unfiltered subscription and its dictionary are never released, following the existing `IOReport::new` pattern. Test-only code.
- `smc/temperature_report.rs` matches the literal string `"SMC key not found"`, so a changed error message would report absent keys as failed reads.
- Out of scope by #415 and unchanged here: how temperatures are aggregated once the key inventory is known, and the IOReport and SMC read cadence tracked in #414.

### References

- [PR #424](https://github.com/lablup/all-smi/pull/424)
- [Issue #415](https://github.com/lablup/all-smi/issues/415)
- [Measurement evidence in the PR comments](https://github.com/lablup/all-smi/pull/424#issuecomment-5744036010)
- Issue #410 and PR #412: the classification rules and publication timing under test
