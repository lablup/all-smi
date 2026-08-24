# Technical Report: PR #349 - AMD ADL Sensor Augmentation on Windows via PMLog

**Date**: 2026-08-07  
**Status**: Completed for the code path; sensor-index mapping and hardware criteria remain open (see section 6)  
**Related**: PR #349, Issue #347, builds on PR #348 / Issue #346  
**Risk Level**: Medium (hand-transcribed vendor ABI on a target no CI job compiles by default)

---

## Executive Summary

PR #349 adds `src/device/readers/amd_adl.rs`, which reads AMD temperature, board power, fan speed, and clocks on Windows from `ADL2_New_QueryPMLogData_Get`. The DXGI and PDH layer from #348 covers everything WDDM publishes; it cannot cover these four, because Windows does not expose them at all.

`atiadlxx.dll` is loaded at runtime through `libloading` from the absolute path `C:\Windows\System32\atiadlxx.dll`, never by bare name and never through an import library, so the executable carries no `atiadlxx` entry in its import table and a machine without AMD's driver starts normally and simply reports no ADL data.

---

## 1. Problem Statement

After #348, a Windows AMD host had utilization and memory but no temperature, no board power, no fan speed, and no clocks. Those four have no WMI, DXGI, or PDH source on Windows, so the only way to get them is AMD's own library.

That library is a hand-transcribed FFI surface against a target nothing in CI compiles, which makes the failure mode worth stating up front: a wrong sensor index does not crash, it reports a plausible but wrong number, and a plausible wrong number is worse than an absent one.

## 2. Change Summary

| Item | Value |
|------|-------|
| Files changed | 9 |
| Lines added | 1644 |
| Lines deleted | 4 |
| Tests added | 34 |
| New dependency | None (`libloading` was already present) |

### Files

| File | Purpose |
|------|---------|
| `src/device/readers/amd_adl.rs` | Reader entry point, augmentation, and the `can_attribute` gate. |
| `src/device/readers/amd_adl/ffi.rs` | ADL struct and function-pointer declarations, with compile-time layout assertions. |
| `src/device/readers/amd_adl/loader.rs` | Absolute-path DLL load, ADL2 context creation, PMLog capability scan. |
| `src/device/readers/amd_adl/sensors.rs` | `ADLSensorType` index mapping and the range guards. |
| `src/device/readers/windows_gpu_perf/dxgi.rs` | DXGI factory caching (see 3.4). |
| `src/doctor/checks/amd.rs` | `amd.adl.library` and `amd.adl.sensors` checks. |

## 3. Technical Decisions

### 3.1 Precedence is WMI < DXGI/PDH < ADL

ADL reads the hardware's own telemetry rather than the OS's accounting of it, so it overwrites PDH utilization and is the sole source for temperature, power, fan, and clocks.

### 3.2 Legacy Overdrive paths are deliberately not implemented

Sensors come only from `ADL2_New_QueryPMLogData_Get`, gated on `ADL2_Overdrive_Caps`. OD5, OD6, and OD7 would be three more ABI surfaces to get right with no way to test them, for hardware predating the cards all-smi targets. A pre-Vega card keeps the DXGI and PDH baseline.

### 3.3 `AdapterInfo` is not declared, so augmentation requires exactly one AMD GPU

This is the load-bearing limitation of the PR. `AdapterInfo` is the struct carrying the PCI bus, device, and function plus the PNP string that would let an ADL adapter index be tied to a specific card. ADL sizes its write by *its own* `sizeof`, so a layout mistake overflows the caller's buffer rather than failing cleanly. Worse, a single card exposes several adapter indices, one per display output, all reporting identical telemetry, which cannot be deduplicated without that struct either.

Rather than guess, `can_attribute()` requires a single AMD GPU. A multi-AMD-GPU host gets the honest DXGI and PDH baseline instead of one card's temperature reported against another. This is the same conclusion the #346 review reached about adapter matching: declining to attribute beats attributing wrongly. PR #361 later declared the struct and lifted this restriction.

### 3.4 The sensor indices are the weakest point and are treated as such

`ADLSensorType` indices are transcribed from AMD's public `adl_structures.h`. Nothing in CI compiles all-smi for Windows and no test can call the real library, so if AMD renumbered an entry this would read the wrong sensor. Two mitigations:

1. **Range guards.** Every value is checked against a physically sensible band (temperature 0 to 150 C, power 0 to 1000 W, activity 0 to 100%, and so on). A misindexed read almost always lands outside its target band, so the guard turns a silent wrong number into an absent one. A test feeds a clock value into the temperature slot and asserts the whole readout comes back empty.
2. **The `amd.adl.sensors` doctor check dumps the raw `index=value` table**, unfiltered and unnamed. That makes the mapping confirmable from real hardware without shipping a code change first. If every sensor fails the range guard, the check says explicitly that this is what a shifted enum looks like and asks for the dump.

`amd.adl.library` separately distinguishes four states: DLL absent, DLL present but context creation failed, loaded but no PMLog-capable adapter, and healthy with the selected adapter index.

### 3.5 DXGI factory caching, carried in this PR

Prompted by a question about polling overhead. `CreateDXGIFactory1` is COM object creation that can pull in graphics driver DLLs, and #348 ran it on every poll, as often as once per second. It is now created once and rebuilt only when `IDXGIFactory1::IsCurrent` reports the adapter set changed, which also keeps hot-plug correct.

For the record on cost: **nothing in either layer submits GPU work.** DXGI reads adapter descriptors, PDH reads counters the OS already maintains, ADL reads a telemetry block the driver already collects. The load is CPU-side and small. The library load, ADL context creation, and the PMLog capability scan each happen once per process, so a steady-state poll is a single ADL call.

One caveat is now documented in the module: very aggressive sensor polling, the 100 ms rates desktop monitoring tools use, can hold an AMD GPU out of its deepest idle state. all-smi's intervals start at one second, well clear of that.

## 4. Validation Results

Same constraint as #348: no CI job compiles all-smi for Windows. The module is gated `cfg(any(target_os = "windows", test))` so the sensor mapping, the range guards, and the field application all run on the Linux test runner. 34 new tests.

| Gate | Result |
|------|--------|
| `cargo fmt --check` | pass |
| `cargo clippy --all-targets -- -D warnings` | pass |
| `cargo test` | 3162 pass, 0 fail (3128 on `main` before) |
| `cargo xwin check --target x86_64-pc-windows-msvc` | pass, 0 warnings |
| `cargo xwin clippy --target x86_64-pc-windows-msvc -- -D warnings` | pass for this change |

Layout is pinned by compile-time assertions on `ADLSingleSensorData` (8 bytes, 4-aligned) and `ADLPMLogDataOutput` (2052 bytes), plus a test asserting the sensor array actually strides by one 8-byte record from offset 4. Those assertions are the only automated check this ABI can have, which is why they are there rather than being decorative.

## 5. Security Notes

The DLL is loaded from an absolute `System32` path, never by bare name, matching the DLL-hijacking stance already documented in `windows_temp/amd_ryzen.rs`. No import library is referenced, so the executable carries no `atiadlxx` entry in its import table. That is deliberately the same shape #345 asks for on Linux, where a missing `libdrm` is a loader error before `main` rather than a degradable condition.

## 6. Outcome and Follow-up

- PR #349 was squash-merged into `main` as `7201d6c`.
- Issue #347 closed automatically through the PR's `Closes #347` link.
- **Not verified, needs real hardware**: temperature, power, fan, and clocks populated on a PMLog-capable card; the sensor index mapping itself (`amd.adl.sensors` exists to produce this evidence); and `dumpbin /imports` showing no `atiadlxx` entry. The design guarantees the last one (nothing is linked, `libloading` only), but it has not been observed on a built exe.
- The single-GPU restriction from 3.3 was lifted by PR #361, which declared `AdapterInfo` with runtime layout verification.
- PR #351 followed immediately to align this reader's `Fan Speed (RPM)` detail key with the `Fan Speed` convention the other readers use.
- The three pre-existing Windows clippy lints noted in #348 remain unfixed in files neither PR touches.

---

## Appendix: Key Terms

| Keyword | Description | Relevance |
|---------|-------------|-----------|
| PMLog | AMD's driver-side telemetry log block, read through `ADL2_New_QueryPMLogData_Get` | The only source for temperature, power, fan, and clocks on Windows |
| `ADL2_Overdrive_Caps` | Capability query gating PMLog availability | Decides whether a card takes the ADL path or the baseline |
| range guard | Bounds check turning an implausible reading into an absent one | The mitigation for a possibly-wrong transcribed sensor index |
| DLL hijacking | Loading a DLL by bare name so a planted file on the search path wins | Why the load is by absolute `System32` path |
