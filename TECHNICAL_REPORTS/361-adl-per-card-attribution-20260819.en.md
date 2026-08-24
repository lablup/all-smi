# Technical Report: PR #361 - Attribute ADL Telemetry Per Card on Multi-GPU AMD Windows Hosts

**Date**: 2026-08-19  
**Status**: Completed for the code path; the layout remains unvalidated against real hardware (see section 6)  
**Related**: PR #361, Issue #353, follow-up filed from PR #349 / Issue #370  
**Risk Level**: High (hand-transcribed 1572-byte struct written by a closed-source driver, on a target no CI compiles)

---

## Executive Summary

PR #361 declares `AdapterInfo` in `ffi.rs` with the Windows layout derived from AMD's public `adl_structures.h`, groups ADL adapter rows per physical card by PCI bus, device, and function, and matches cards to the reader's GPUs by `strPNPString` against the WMI `PNPDeviceID`. This lifts the single-AMD-GPU restriction #349 imposed, so a Ryzen APU plus Radeon dGPU laptop, the most common multi-AMD configuration, now gets temperature, power, fan, and clocks instead of nothing.

The PR landed as four commits: the initial implementation, then three rounds of hardening that closed a defect in the verification itself, five driver-anomaly gaps, and the test-coverage hole behind `cfg(target_os = "windows")`.

---

## 1. Problem Statement

Without `AdapterInfo`, an ADL adapter index cannot be tied to a physical card, and one card exposes several indices, one per display output, all reporting identical telemetry. #349 therefore required exactly one AMD GPU before augmenting anything, which is the honest answer but leaves the multi-GPU case with only the DXGI and PDH baseline.

The struct is the hard part. ADL sizes its write by *its own* `sizeof`, so a layout mistake overflows the caller's buffer rather than failing cleanly, and nothing in CI compiles this code.

A note on the issue text: the issue body's 1568-byte figure is wrong; its formula drops `iAdapterIndex`. The correct layout is 9 ints plus 6 `char[256]` arrays, 1572 bytes with no padding.

## 2. Change Summary

| Item | Value |
|------|-------|
| Files changed | 6 |
| Lines added | 2184 |
| Lines deleted | 72 |
| Commits | 4 (implementation plus three hardening rounds) |
| `amd_adl` tests | 55 |

### Files

| File | Purpose |
|------|---------|
| `src/device/readers/amd_adl/ffi.rs` | `AdapterInfo` declaration, compile-time size and offset assertions, `looks_sane`, `is_blank`. |
| `src/device/readers/amd_adl/adapters.rs` | Platform-independent grouping, matching, `plan_attribution`, count bound, failure-shape formatter. |
| `src/device/readers/amd_adl/loader.rs` | `ADL2_Adapter_AdapterInfo_Get` call, padded-buffer pattern, capability scan. |
| `src/device/readers/amd_adl.rs` | `can_attribute` replaced by `adapters::plan_attribution`. |
| `src/doctor/checks/amd.rs` | `amd.adl.adapters` check dumping index, bus/device/function, `strAdapterName`, `strPNPString`. |
| `README.md` | Doctor check-ID table, which had listed no `amd.adl.*` checks at all. |

## 3. Technical Decisions

### 3.1 Runtime verification, because compile-time assertions cannot be enough here

Compile-time size and offset assertions pin the transcription against itself. They cannot catch a transcription that is internally consistent but wrong about the driver.

So the layout is additionally verified at runtime: every row's four NUL-terminated ASCII string fields must read as legible text, and `iSize` must agree with `size_of` when populated, **checked per row** so a wrong stride is caught on the second entry even when the first parses. A failed check, a failed match, a missing entry point, or a failed call all decline attribution and keep the DXGI and PDH baseline, which is the conclusion the #346 review reached for `match_adapter`.

`plan_attribution` returns `SoleGpu`, `PerCard`, or `Decline`. Single-GPU hosts never consult `AdapterInfo` and keep the pre-#353 behavior, pinned by a regression test.

### 3.2 The verification had a hole, and closing it was the second commit

`AdapterInfoArray::for_count` pre-fills its buffer with zeroed rows, and **every arm of `looks_sane` was individually satisfied by that zero row**: `iSize` of 0 is explicitly accepted, `iAdapterIndex` of 0 is in range, and each string field is a NUL at offset 0, which `adl_string` documents as the empty string rather than garbage. A buffer ADL never touched therefore passed layout verification as a table of valid empty adapters.

That hollowed out the only verification that will actually run on this transcription. If the real `sizeof(AdapterInfo)` is larger than the declared 1572, ADL derives its row count as `iInputSize / sizeof`, writes fewer rows than requested, and leaves the tail blank. Nothing garbles, so the per-row string checks passed and the documented guarantee that a wrong stride is caught on the second row did not hold in the one direction where nothing garbles. The `amd.adl.adapters` doctor check, which is how a real multi-GPU host is supposed to confirm or refute the layout, would have reported PASS over rows the driver never filled.

`is_blank` now compares a row against the zero it was pre-filled with, and `looks_sane` fails on it. Attribution itself was never at risk, since a blank row groups to an empty PNP string that matches no GPU, so this restores the verification rather than fixing a mis-attribution.

### 3.3 Five driver-anomaly gaps closed in the third commit

A security and performance review found no memory-safety defect in the new FFI itself: the advertised `iInputSize` is always strictly smaller than the allocation, the buffer is zero-filled before the call so no row is ever read uninitialized, every field type accepts any bit pattern the driver can write, no Rust reference is live across the call, and no path can panic on arbitrary driver bytes. The five fixes close lower-severity gaps around that core, all in the direction the module already commits to: decline rather than guess.

| Fix | What it prevents |
|-----|------------------|
| `group_by_card` drops a group whose rows carry two different non-empty PNP paths instead of merging | A driver leaving bus/device/function unfilled collapsed every card into one group, and `augment` samples the first index that answers PMLog, so one card's temperature, power, and fan could be reported for another GPU |
| `scan_for_capable_adapter` clamps the driver's adapter count to `MAX_ADAPTER_ROWS` | The scan makes one `ADL2_Overdrive_Caps` call and possibly one 8 KB-allocating PMLog read per index while holding the process-wide runtime lock, so a garbage count wedged the monitoring refresh loop |
| `read_pmlog` derives its pointer from the whole padded buffer and narrows by cast | `&raw mut buffer.output` was valid only for that field's 2052 bytes, putting the headroom out of bounds for exactly the oversized write the headroom exists to absorb |
| `AdapterInfoArray::input_size` is a checked conversion, not a truncating `as` | The one number that would be a buffer overflow inside a closed-source driver if it ever exceeded the allocation must not be reachable through a silent wrap |
| The doctor check names which direction a failed verification points | Blank rows mean a declared struct larger than the driver's; garbled strings are the opposite error. The two call for opposite corrections |

A blank sibling row is still backfilled, and a case difference is still not a conflict, since WMI and ADL do not agree on case.

### 3.4 Moving logic out of `cfg(target_os = "windows")` so it can be tested

The clamped scan loop, the `plausible_adapter_count` reject path, and the doctor check's blank-versus-garbled failure message had no automated coverage: every caller lived behind `cfg(target_os = "windows")`, and this repository has no Windows CI job.

The fourth commit moves `MAX_ADAPTER_ROWS`, the plausible-count check, the scan clamp, and the failure-shape formatter into the platform-independent `adapters.rs`, the same split the module already uses for grouping and matching, so the arithmetic and message logic run on the Linux test runner instead of shipping untested.

### 3.5 Two known verification gaps, recorded in the source rather than left implicit

Both are documented directly on `AdapterInfo::is_blank` and `looks_sane`:

- Blank-row rejection may be too strict for a driver that legitimately leaves some rows untouched. AMD's own SDK sample filters by `iPresent` rather than assuming every row is populated.
- `looks_sane` checks only 4 of the struct's 6 string fields, since the other two are transitively pinned by `strPNPString`.

Both are left as-is pending real hardware evidence, and the PR body documents the specific dump shape that would justify revisiting either one.

## 4. Validation Results

| Gate | Result |
|------|--------|
| `cargo check --lib --tests` | pass |
| `cargo clippy --lib --tests -- -D warnings` | pass |
| `cargo fmt -- --check` | pass |
| `cargo test --lib device::readers::amd_adl` | 55 passed (51 before the final commit) |
| `cargo test --lib doctor` | 25 passed |
| `cargo test --lib device::readers::windows_gpu_perf` | 31 passed |

A Windows-target compile check of `ffi.rs`, `adapters.rs`, and `loader.rs` was run in a throwaway crate via `cargo check --target x86_64-pc-windows-gnu`, with a **negative control**: flipping the `AdapterInfo` size assertion from 1572 to 1568 still fails the build, which proves the check reaches the assertion rather than passing vacuously. An earlier round did the same for a verbatim copy of the doctor check.

## 5. Security Notes

The review's core finding is worth restating because it is the load-bearing claim of the whole FFI: the advertised `iInputSize` is always strictly smaller than the allocation, the buffer is zero-filled before the call, every field type accepts any bit pattern, no Rust reference is live across the call, and no path panics on arbitrary driver bytes. Everything in 3.3 is defense around that, not a repair of it.

## 6. Outcome and Follow-up

- PR #361 was squash-merged into `main` as `f698b7e`.
- The layout remains **unvalidated against real hardware**. No CI compiles all-smi for Windows and no multi-AMD-GPU host was available, so the runtime verification and the decline fallback are the safety mechanism.
- **Issue #370 stays open**: confirming per-card ADL attribution on a Windows host with two or more AMD GPUs. It carries `priority:low` but is the acceptance criterion this PR cannot meet on its own.
- PR #371 followed immediately, turning the temporary hand-written probe used to prove these paths work into a permanent `amd.adl.per_adapter` doctor check.

---

## Appendix: Key Terms

| Keyword | Description | Relevance |
|---------|-------------|-----------|
| `AdapterInfo` | ADL struct carrying PCI location and PNP string per adapter index | The struct whose absence forced the single-GPU restriction |
| `strPNPString` | Plug-and-play instance path identifying the device to Windows | The key matching ADL adapters to WMI-discovered GPUs |
| padded-buffer pattern | Allocating more than the advertised `iInputSize` to absorb an oversized driver write | Why `read_pmlog`'s pointer had to cover the whole buffer |
| negative control | Deliberately breaking an assertion to prove a check reaches it | How the Windows compile check was shown not to pass vacuously |
