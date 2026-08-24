# Technical Report: PR #351 - Unify GPU Detail Keys with the Shared Reader Convention

**Date**: 2026-08-07  
**Status**: Completed  
**Related**: PR #351, follow-up to PR #348 and PR #349  
**Risk Level**: Low (no key in this set has appeared in a tagged release)

---

## Executive Summary

PR #351 aligns the Windows ADL reader's `detail` map keys with the convention every other reader already follows: the key names the quantity and the unit rides in the value. `Fan Speed (RPM)` with value `1450` becomes `Fan Speed` with value `1450 RPM`, and four sibling keys move the same way.

The two DXGI VRAM diagnostics added by #348 keep their qualifier, because `(this process)` is a scope rather than a unit. Only their values gained an explicit `bytes`.

---

## 1. Problem Statement

Four readers publish fan speed through the `detail` map, and three of them agreed by convention alone:

| Reader | Key | Value |
|--------|-----|-------|
| `amd.rs` (Linux) | `Fan Speed` | `1450 RPM` |
| `intel_gpu_linux/sources.rs` | `Fan Speed` | `1450 RPM` |
| `intel_gpu_level_zero/apply.rs` | `Fan Speed` | `1450 RPM` |
| `amd_adl.rs` (added by #349) | `Fan Speed (RPM)` | `1450` |

The ADL reader was the odd one out, and #348 had done the same thing with its two VRAM diagnostics. A consumer keying on `Fan Speed` therefore silently missed the Windows AMD path, and any code that tried to normalize the map had to know about two spellings for one quantity.

## 2. Change Summary

| Item | Value |
|------|-------|
| Files changed | 2 |
| Lines added | 91 |
| Lines deleted | 23 |
| Keys renamed | 5 |
| Consumers broken | 0 (see 3.3) |

### The renames

| Before | After | Value |
|--------|-------|-------|
| `Fan Speed (RPM)` | `Fan Speed` | `1450 RPM` |
| `Memory Clock (MHz)` | `Memory Clock` | `1250 MHz` |
| `Hotspot Temperature (C)` | `Hotspot Temperature` | `81 C` |
| `Memory Temperature (C)` | `Memory Temperature` | `70 C` |
| `Memory Controller Activity (%)` | `Memory Controller Activity` | `44%` |
| `VRAM Budget (this process)` | unchanged | `7000000000 bytes` |
| `VRAM Usage (this process)` | unchanged | `123456 bytes` |

## 3. Technical Decisions

### 3.1 The two VRAM keys keep their qualifier

`(this process)` is a **scope**, not a unit. Those DXGI figures are process-scoped rather than system-wide, and dropping the qualifier would invite exactly the misreading the label exists to prevent, which #348 chose the wording specifically to avoid. Only their values gained the `bytes` unit, matching `VRAM Total` in the existing readers.

### 3.2 `Temperature` is now emitted only below zero

The key exists to preserve a sub-zero die reading that the unsigned `GpuInfo.temperature` field floors at 0. On every normal poll it merely duplicated that field, adding a row to the detail map that carried no information the typed field did not already have.

### 3.3 The compatibility question, answered rather than assumed

None of these keys has appeared in a tagged release: #348 and #349 both merged after v0.25.0 and before v0.26.0. No consumer can depend on the old spellings, so the rename is free at exactly this moment and would not have been a month later.

### 3.4 A test guards against recurrence rather than a comment

`detail_keys_follow_the_shared_reader_convention` asserts both the new spellings and the **absence** of the old ones, and its comment records the two concrete costs of diverging. The next reader to publish these quantities fails a test rather than drifting quietly. This is the same reasoning that later produced `src/device/readers/detail_keys.rs` in #365.

## 4. Validation Results

| Gate | Result |
|------|--------|
| `cargo fmt --check` | pass |
| `cargo clippy --all-targets -- -D warnings` | exit 0 |
| `cargo test` | exit 0, 3176 passed, 0 failed across 23 binaries |
| `cargo xwin check --target x86_64-pc-windows-msvc` | exit 0, 0 warnings |

A note on the test count, recorded because it affects how the neighbouring reports read: the figures quoted in #348 and #349 were read through a shell pipe that intermittently truncated cargo's output, so they undercounted slightly. Counting from a captured file gives 3176 here. The authoritative signal in all three PRs was the exit code, which was 0 throughout.

## 5. Outcome and Follow-up

- PR #351 was squash-merged into `main` as `cafb054`.
- Two follow-ups were filed rather than folded in:
  - Promoting fan speed to a real `GpuInfo` field so it reaches the TUI and Prometheus, not just `snapshot` JSON. Four readers would benefit, and `Source: Fan` already exported as `source__fan` while the value did not. This became **#360**.
  - ADL `AdapterInfo` for multi-AMD-GPU attribution, which became **#361**.
- The convention this PR pinned by test was later given a shared home in `detail_keys.rs` by #365, after the `Metrics Source` key demonstrated the same class of drift in a different direction.

---

## Appendix: Key Terms

| Keyword | Description | Relevance |
|---------|-------------|-----------|
| `detail` map | Untyped string-to-string map a reader publishes alongside typed `GpuInfo` fields | Where all five renamed keys live |
| unit-in-value convention | The key names the quantity, the value carries the unit | The convention the ADL reader was violating |
| scope qualifier | A label narrowing what a figure covers, such as `(this process)` | Why the two VRAM keys were exempt from the rename |
