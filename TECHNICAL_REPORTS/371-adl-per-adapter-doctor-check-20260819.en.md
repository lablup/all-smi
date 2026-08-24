# Technical Report: PR #371 - Add an `amd.adl.per_adapter` Doctor Check for Per-Index PMLog Sampling

**Date**: 2026-08-19  
**Status**: Completed; validated on single-card hardware, per-card differentiation still unobserved  
**Related**: PR #371, Issue #369, refs #353, #361, #370  
**Risk Level**: Low (diagnostic check only)

---

## Executive Summary

Three pieces of the multi-GPU AMD attribution path added by #353 and #361 are both `cfg(target_os = "windows")` and unreachable on a single-GPU host, because `plan_attribution` takes its `SoleGpu` arm before any of them execute: `loader::adapter_inventory`, `loader::sample_adapter`, and the `PerCard` arm of `augment`. No unit test reaches them and no CI job compiles all-smi for Windows, so they were shipping on inspection alone. The evidence that they work at all came from a temporary probe written by hand and then reverted.

PR #371 makes that probe a permanent doctor check, the same field-verification role `amd.adl.sensors` plays for the sensor index mapping and `amd.adl.adapters` plays for the `AdapterInfo` layout. Whoever gets a host with two or more AMD GPUs can now settle #370 with one command instead of reading the source and writing their own probe.

---

## 1. Problem Statement

The per-card attribution work has a verification asymmetry: the code that matters most on a multi-GPU host is exactly the code a single-GPU host cannot execute. `plan_attribution` returns `SoleGpu` and short-circuits, so the inventory call, the per-index sampling, and the `PerCard` augmentation arm never run.

That leaves three ways to learn whether they work: read the source, write a throwaway probe, or ship and wait. The first two had already been done once and thrown away, which is the waste this PR stops repeating.

## 2. Change Summary

| Item | Value |
|------|-------|
| Files changed | 2 |
| Lines added | 266 |
| Lines deleted | 14 |
| New doctor check | `amd.adl.per_adapter` |
| Tests added | 5 (on `per_card_verdict`) |

### Files

| File | Change |
|------|--------|
| `src/doctor/checks/amd.rs` | The check itself, `per_card_verdict` as a testable free function, and `describe_readout` extracted for sharing. |
| `README.md` | Added to the `amd.*` check id table. |

## 3. Technical Decisions

### 3.1 The summary states a verdict rather than leaving it to be read off the dump

The check prints, per index, the ADL index, whether PMLog answered, the interpreted readout, and the raw `index=value` pairs, in the unfiltered style `amd.adl.sensors` already uses. Output is grouped by physical card with its bus, device, function, and instance path, so a two-card host shows at a glance whether the cards differ.

But a dump that requires the reader to draw the conclusion is a dump that gets misread. The summary therefore says outright which of four states holds: **DISTINCT**, **IDENTICAL**, **undetermined** when too few cards answered, or **unobservable** on a single-card host.

That last state is the one worth having explicitly. A single card agreeing with itself across five display-output indices is not evidence about two cards, and a summary that reported IDENTICAL there would be technically true and practically misleading, since it is the exact string a reader is hoping to see refuted.

### 3.2 `per_card_verdict` is split off the Windows gate so it can be tested

The verdict logic is a free function left off the `cfg(target_os = "windows")` gate, so the Linux runner tests it. Five cases cover each arm, including a three-card mix where only one card differs, which is the case a naive all-equal comparison would get wrong.

This is the same split #361 applied to grouping, matching, and the count bound, and for the same reason: the logic that decides what an operator concludes should not be the part that ships untested.

### 3.3 Readout formatting moved into a shared `describe_readout`

`amd.adl.per_adapter` and `amd.adl.sensors` both render a sensor readout. The two dumps are only comparable if they render the same struct identically, and two independent call sites cannot guarantee that over time. One function can.

### 3.4 Every failure path skips or warns rather than guessing

This matches the decline-rather-than-guess posture of the rest of the module. A missing inventory points the reader at `amd.adl.adapters`, which is the check that separates no-library from no-entry-point from failed layout verification. A diagnostic that guesses is worse than one that hands off.

## 4. Validation Results

**Validated on real hardware.** AMD Radeon(TM) 8060S Graphics, Strix Halo APU, driver 32.0.31035.1003, Windows 11 Pro 26200, native `x86_64-pc-windows-msvc`:

- All 5 adapter indices answered, each with a full 16-sensor table.
- Telemetry was identical across the indices of the one card, which is the expected shape for a single physical GPU exposing one index per display output.
- The verdict correctly reported per-card differentiation as **unobservable** on this host rather than claiming IDENTICAL.

That last point is the one this run actually tested: the check declined to overclaim on the hardware available.

Linux-side gates: the five `per_card_verdict` cases pass on the standard runner.

## 5. Outcome and Follow-up

- PR #371 was squash-merged into `main` as `81321cf`.
- Issue #369 closed automatically through the PR's `Closes #369` link.
- **Issue #370 stays open** and is now answerable with a single command on suitable hardware. That is the whole point of the check: it converts "read the source and write a probe" into "run `all-smi doctor --only amd.adl.per_adapter`".
- The `PerCard` arm of `augment` and the multi-card DISTINCT verdict remain unexercised. A single-card host cannot reach them by construction.

---

## Appendix: Key Terms

| Keyword | Description | Relevance |
|---------|-------------|-----------|
| `plan_attribution` | Returns `SoleGpu`, `PerCard`, or `Decline` | Its `SoleGpu` short circuit is why the paths are unreachable on one card |
| field verification | Using a diagnostic to confirm a transcribed ABI on an operator's machine | The role this check shares with `amd.adl.sensors` and `amd.adl.adapters` |
| unobservable verdict | Explicitly reporting that the host cannot answer the question | Prevents a single card's self-agreement from reading as multi-card evidence |
