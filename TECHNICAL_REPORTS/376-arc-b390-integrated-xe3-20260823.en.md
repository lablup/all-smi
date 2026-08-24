# Technical Report: PR #376 - Report the Arc B390 as an Integrated Xe3 Part

**Date**: 2026-08-23  
**Status**: Completed for the classification and capacity defects; the utilization symptom is deferred (see section 6)  
**Related**: PR #376, Issue #364  
**Risk Level**: Medium (changes reported device class and memory capacity; also moves every Radeon APU on Windows)

---

## Executive Summary

An Intel Arc B390 is an integrated Xe3 (Panther Lake) GPU. all-smi found the device but attached three wrong values to it: `Discrete`, `Xe-LPG (Meteor Lake)`, and a 128 MiB total memory. Each comes from a separate premise that was true when it was written and that current hardware has invalidated. All three are confirmed by reading the source, and all three are fixed here.

The memory defect lives in `windows_gpu_perf.rs`, which was new since v0.25.0 and had never shipped, so this is the difference between releasing it correct and releasing it wrong for the first time. It is also shared with `amd_windows.rs` through `augment_gpus`, so the fix moves every Radeon APU on Windows as well.

---

## 1. Problem Statement

### Defect 1: discrete versus integrated

The rule read "an Arc name with a model number" as "discrete card". That held until Panther Lake, the first Intel iGPU generation sold under a model number. It cannot be repaired as a pattern, because the integrated B390 and the discrete B380 differ by one digit.

### Defect 2: architecture

`IntelArchitecture` had no Xe3 variant, so the B390 fell through to a residual rule whose comment claimed any remaining `arc` plus `graphics` name must be the Meteor Lake iGPU.

### Defect 3: memory

The branch assumed integrated graphics report no dedicated pool at all, falling back to the shared aperture only when the dedicated pool was exactly zero. Modern Intel and AMD integrated parts publish a small stolen-memory carve-out through DXGI, and 128 MiB is the classic value, so the branch took the carve-out as the whole capacity.

## 2. Change Summary

| Item | Value |
|------|-------|
| Files changed | 6 |
| Lines added | 474 |
| Lines deleted | 125 |
| Tests added | 16 |
| Total tests passing | 3406 |

### Files

| File | Change |
|------|--------|
| `src/device/readers/intel_gpu_names.rs` | Both name rules moved here; explicit discrete and integrated SKU tables; `IntelArchitecture::Xe3`. |
| `src/device/readers/intel_gpu_windows.rs` | The discrete/integrated rule removed from the Windows-gated module. |
| `src/device/readers/windows_gpu_perf.rs` | The 1 GiB carve-out floor and the shared-aperture fallback. |
| `src/device/readers/mod.rs` | `test` arm added to the `intel_gpu_names` gate. |
| `src/device/macos_native/smc.rs` | One unrelated clippy waiver (see 3.4). |

## 3. Technical Decisions

### 3.1 Explicit tables, and no answer where the tables are silent

The pattern rule is replaced by an explicit table of the SKUs this project already claimed as discrete, plus a table of the numbered integrated parts: Lunar Lake's 140V and 130V, which previously escaped the old test only by accident, and the B390.

A name carrying a number that **neither table knows returns no answer at all** rather than guessing, and the DXGI memory layout fills it in instead. Owning a dedicated pool versus addressing a shared aperture is the definition of the distinction, so DXGI answers it directly rather than by inference from a marketing string.

### 3.2 The residual architecture rule is narrowed rather than extended

Xe3 becomes a variant, the residual rule is narrowed to unnumbered names only, and a numbered part the table does not recognize reports `Unknown` instead of being assigned to Meteor Lake by elimination. `Unknown` is a worse-looking answer that is more often correct, which is the trade this project makes elsewhere too.

### 3.3 The 1 GiB carve-out floor, and why the populations separate cleanly

A dedicated pool below 1 GiB is now treated as a carve-out and the shared aperture is reported instead. No discrete card has ever shipped with less than that floor, and the stock carve-outs are 64 to 512 MiB, so the two populations separate with room on both sides.

A firmware-configured carve-out at or above the floor is still reported at face value, since that is the amount the operator set aside and reporting the aperture there would contradict a deliberate configuration.

### 3.4 Making the rules reachable by a test runner is the substance of the fix

Both name rules move into `intel_gpu_names`, which is pure string matching, and that module's gate gains a `test` arm. This is not tidying.

The discrete/integrated rule lived in `intel_gpu_windows`, which is `cfg(target_os = "windows")`. No runner this project has ever compiled it, so **the wrong rule was unreachable by every test job and could only be found by someone holding the hardware.** `intel_gpu_sysfs`, `windows_gpu_perf`, and `amd_adl` already carry the same `test` arm, and `mod.rs` states the reason next to each. This applies it to the module that just demonstrated why it matters.

### 3.5 One unrelated fix carried here, deliberately

`cargo clippy -- -D warnings`, the command CI runs, failed on `main` on a macOS host: `TempKeyScan::scanned_keys` and `total_keys` are diagnostic counters read only by tests, so they are dead behind the binary's module root exactly as `used_sorted_range` already was. It arrived with #375 and CI did not catch it, because `smc.rs` is macOS-only, the job that runs clippy runs on `ubuntu-latest`, and the one macOS job runs `cargo build --bin all-smi` alone with no clippy and no tests. That is the same blind spot #368 describes for Windows, one platform over.

It is fixed here rather than left for a separate PR because it is a one-line waiver on code this branch already had to lint past, and leaving `main` red under its own CI command is worse than the small scope bleed. Both items now carry the waiver that the neighbouring items in each file already use.

## 4. Validation Results

No B390, no Windows host, and the self-hosted runners were down for maintenance. Everything below was actually executed; nothing is inferred.

- **3406 tests pass, 0 failures.** 16 are new: the B390 itself, the one-digit B380/B390 collision, the deferral for unknown numbered parts, the AMD APU carve-out range (64, 128, 256, 512 MiB), the discrete floor boundary, and regression guards pinning every SKU and architecture that was already classified correctly.
- `cargo clippy --lib --tests --all-features` produces no new warnings, and `cargo fmt --check` is clean.
- **The Windows-only reader was type-checked, linted, and its tests run**, using the probe technique this repository documents in `src/service_cmd/mod.rs`: a scratch worktree with a stubbed `wmi` crate and the module's gate widened, so the real `intel_gpu_windows.rs` compiles against the real `GpuInfo`, the real `windows_gpu_perf`, and the real classifier. `cargo check --target x86_64-pc-windows-msvc` still dies in `zstd-sys` for the reason #357 records, so this is the only route available.
- **Probe reachability was proven rather than assumed**: a deliberate type error injected at the changed line produced a compile error through the probe, and reverting it returned a clean check. All 22 of that module's own tests pass through it.

**What is not verified**: the B390's actual reported values. No one has run this against the device. The classification is verified against the name string the issue records, not against WMI output from the machine.

## 5. Not Addressed, Deliberately

- **The missing-utilization symptom.** The issue frames it as a hypothesis needing the device, and it cannot be settled from the source. `UTILIZATION_ENGINE_TYPES = ["3D", "Compute"]` in `windows_gpu_perf/ids.rs` is the candidate, recorded in the follow-up rather than guessed at here.
- **Temperature.** An explicit non-goal: the part exposes no Sysman thermal sensor, so a criterion for it could never pass.

## 6. Outcome and Follow-up

- PR #376 was squash-merged into `main` as `7472d23`.
- Issue #364 closed automatically through the PR's `Closes #364` link.
- **Issue #377 stays open**: settling the Arc B390 utilization symptom and the `level_zero` `apply.rs` sites left over from #364.
- PR #365 landed the remaining half of #364 a day later: the packaging decision that makes the Intel backend present at all on Windows, plus four defects that decision exposed. #365 was rewritten against this PR, dropping everything that overlapped, including the name-pattern work, `IntelArchitecture::Xe3`, the discrete and integrated tables, and the DXGI carve-out floor.

---

## Appendix: Key Terms

| Keyword | Description | Relevance |
|---------|-------------|-----------|
| stolen memory carve-out | A small dedicated pool an integrated GPU publishes out of system RAM | Mistaken for the whole capacity before the 1 GiB floor |
| shared aperture | The system memory an integrated GPU can address | The correct capacity for an integrated part |
| Xe3 / Panther Lake | Intel's integrated GPU generation sold under model numbers | The generation that invalidated the "numbered name means discrete" rule |
| scratch probe | A throwaway worktree with a stubbed dependency, used to compile platform-gated code | How the Windows-only reader was type-checked without a Windows host |
