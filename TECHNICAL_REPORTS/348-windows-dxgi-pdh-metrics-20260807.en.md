# Technical Report: PR #348 - Vendor-Neutral Windows GPU Metrics via DXGI and PDH

**Date**: 2026-08-07  
**Status**: Completed for the code path; hardware acceptance criteria remain open (see section 6)  
**Related**: PR #348, Issue #346  
**Risk Level**: Medium (new FFI surface on a target no CI job compiles by default)

---

## Executive Summary

PR #348 adds `src/device/readers/windows_gpu_perf.rs`, a shared Windows GPU metrics layer built on two OS facilities that need no vendor SDK, and wires both the AMD and the Intel Windows readers to it. DXGI supplies true 64-bit dedicated memory, the adapter LUID, and PCI vendor and device ids; PDH supplies engine utilization and adapter memory usage.

Before this, both readers were WMI-only baselines: `utilization`, `used_memory`, `frequency`, and `power_consumption` were hardcoded to `0`, `get_process_info()` returned an empty `Vec`, and `total_memory` was wrong for any card above 4 GB because `Win32_VideoController.AdapterRAM` is a `uint32` in the WMI schema.

---

## 1. Problem Statement

A Windows monitoring node reported a detected GPU with every interesting number set to zero, and a memory capacity that silently wrapped for the cards most likely to be monitored. The WMI schema cannot fix either problem: it has no engine-utilization surface at all, and its memory field is 32 bits wide by definition.

The two vendor readers had the same hole, so the fix had to be a shared layer rather than a per-vendor patch, or the same code would be written twice and drift.

| Field | Before | Source added |
|-------|--------|--------------|
| `total_memory` | `AdapterRAM`, wraps above 4 GB | DXGI `DedicatedVideoMemory` (64-bit) |
| `utilization` | hardcoded `0` | PDH `\GPU Engine(*)\Utilization Percentage` |
| `used_memory` | hardcoded `0` | PDH `\GPU Adapter Memory(*)\Dedicated Usage` |
| per-process rows | empty `Vec` | PDH engine instances, keyed by pid |

## 2. Change Summary

| Item | Value |
|------|-------|
| Files changed | 10 |
| Lines added | 2301 |
| Lines deleted | 33 |
| Tests added | 28 |
| New direct dependency | `windows` 0.62 |

### Files

| File | Purpose |
|------|---------|
| `src/device/readers/windows_gpu_perf.rs` | The shared layer: snapshot assembly, adapter matching, field application. |
| `src/device/readers/windows_gpu_perf/dxgi.rs` | `DXGI_ADAPTER_DESC1` enumeration: capacity, LUID, PCI ids. |
| `src/device/readers/windows_gpu_perf/ids.rs` | Counter-instance parsing and PNPDeviceID parsing, including `parse_pnp_device_id`. |
| `src/device/readers/windows_gpu_perf/pdh.rs` | Persistent PDH query, counter collection, per-engine and per-process aggregation. |
| `src/device/readers/amd_windows.rs`, `intel_gpu_windows.rs` | Consume the shared layer instead of publishing zeros. |
| `src/doctor/checks/windows.rs` | `windows.gpu.perf_counters` check, which distinguishes the failure modes. |
| `Cargo.toml`, `Cargo.lock` | `windows` 0.62 promoted from transitive to direct. |

## 3. Technical Decisions

### 3.1 The PDH query is persistent, not per-poll

`Utilization Percentage` is a rate counter. A single `PdhCollectQueryData` establishes a baseline and yields nothing usable, so a naive implementation would have to sleep inside the reader to manufacture a second sample.

Instead the query is opened once and each poll contributes one collection. The first poll after start-up reports no utilization; every poll after that reports the rate over the real interval between polls. `get_process_info()` reuses that sample through `latest()` rather than collecting again, which would halve the interval the rate is computed over and roughly double the reported load.

### 3.2 Utilization sums within an engine but takes the maximum across engines

Each PDH sample is one process's share of one engine. Summing across processes therefore gives that engine's busy fraction, which is correct. Summing across a card's several 3D and Compute engines yields figures well above 100% and would peg every gauge.

The maximum across engines is what Task Manager's headline GPU percentage reports, so it is both defensible and familiar. This is a deliberate deviation from the issue text, which asked for "summing the 3D and Compute engine types per LUID", and is flagged rather than quietly adopted.

### 3.3 Video engines are excluded from utilization

A compositor decoding video keeps `VideoDecode` busy while the shader cores idle. Folding it in would make an idle desktop report a large non-zero load, which is worse than reporting nothing.

### 3.4 DXGI `QueryVideoMemoryInfo` is process-scoped and must not be used as device usage

MSDN defines `CurrentUsage` and `Budget` as this process's view, not the system's. Reading either as the device's used memory would understate a busy GPU by whatever every other process holds.

Both are still worth surfacing, so they are exposed as clearly labelled diagnostic detail fields (`VRAM Usage (this process)`), and `used_memory` comes from the PDH adapter counter instead.

### 3.5 Counters are added with `PdhAddEnglishCounterW`

Counter path components are localized on non-English Windows. The literal English path `\GPU Engine(*)\Utilization Percentage` only resolves through the English-specific entry point, so a German or Korean Windows install would otherwise find no counters at all.

## 4. Implementation Details

`snapshot()` enumerates DXGI adapters first, then collects the PDH sample, then matches the two. Matching runs LUID first, since DXGI and the PDH instance names both carry it, and falls back to the PCI vendor and device pair through `parse_pnp_device_id` against the WMI `PNPDeviceID` when the LUID route does not resolve.

Field application is the layer's own step rather than each vendor reader's, which is what keeps the AMD and Intel paths from drifting.

## 5. Validation Results

No CI job in this repository compiles all-smi for Windows by default. The only Windows job is gated behind an unset repository variable, and its own comment states it has never executed. Windows-only code therefore ships with zero automated coverage unless something is done about it, and two things were:

**1. The module is gated `cfg(any(target_os = "windows", test))`**, matching the existing `intel_gpu_sysfs` pattern. Counter-instance parsing, utilization aggregation, PNPDeviceID matching, and field application are therefore all exercised by the Linux test runner. 28 new tests.

**2. The FFI itself was cross-compiled to the real release target from macOS.**

| Gate | Result |
|------|--------|
| `cargo fmt --check` | pass |
| `cargo clippy --all-targets -- -D warnings` | pass |
| `cargo test` | 3118 pass, 0 fail |
| `cargo xwin check --target x86_64-pc-windows-msvc` | pass |
| `cargo xwin clippy --target x86_64-pc-windows-msvc -- -D warnings` | pass for this change |

The Windows cross-check needed `cargo-xwin` plus `llvm-lib` (rustup's `llvm-tools` provides `llvm-ar`, which acts as `llvm-lib` under that name); `zstd-sys` otherwise blocks any Windows cross-compile from macOS.

### Pre-existing findings, not fixed here

Windows clippy reports three collapsible-`if` lints in files this PR does not touch (`src/device/cpu_windows.rs` at two sites, `src/device/windows_temp/amd_ryzen.rs` at one). They surfaced only because nothing had ever linted that target. Left alone to keep the diff scoped.

## 6. Outcome and Follow-up

- PR #348 was squash-merged into `main` as `55c6a1a`.
- Issue #346 closed automatically through the PR's `Closes #346` link.
- **Not verified on hardware**: non-zero utilization on a busy AMD or Intel Windows machine, correct VRAM on a card above 4 GB, and populated per-process rows. The `windows.gpu.perf_counters` doctor check exists to produce exactly that evidence from an operator's machine. It distinguishes "no DXGI adapters", "DXGI works but PDH publishes no instances" (normal on VMs and RDP), and the full path.
- Follow-up worth taking together with a CI job that actually compiles the Windows target, which `windows-latest` runners would do for free on this public repository. That became #368.
- PR #349 built the AMD ADL layer on top of this one, and #365 later corrected two defects in the memory and detail-key handling this PR introduced.

---

## Appendix: Key Terms

| Keyword | Description | Relevance |
|---------|-------------|-----------|
| PDH | Performance Data Helper, the Windows counter query API | Source of utilization and adapter memory |
| DXGI adapter LUID | Locally unique identifier for a graphics adapter | The primary key matching DXGI adapters to PDH instances |
| rate counter | A counter whose value is a delta over an interval, needing two collections | Why the PDH query is persistent rather than per-poll |
| `PdhAddEnglishCounterW` | Adds a counter by its non-localized English path | Required for the path to resolve on non-English Windows |
