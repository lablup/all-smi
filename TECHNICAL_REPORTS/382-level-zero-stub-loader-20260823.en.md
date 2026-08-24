# Technical Report: PR #382 - Level Zero Stub Loader Coverage

**Date**: 2026-08-23  
**Status**: Completed  
**Related**: PR #382, Issue #379  
**Risk Level**: Low (test and CI changes only)

---

## Executive Summary

PR #382 adds deterministic, hardware-independent integration coverage for the Intel Level Zero backend. CI now compiles a test-only C loader against Intel's vendor headers, places it ahead of the system loader for one isolated test step, and drives the Rust backend through its public API. This validates ABI layout, enumeration, BDF mapping, count-then-fill behavior, metric conversion, delta arithmetic, and per-family failure isolation without requiring an Intel GPU.

The final review found two gaps in the original fixture: mutable global counters could race when Rust integration tests ran concurrently, and the negative-path device returned an empty successful result instead of an actual Sysman error. The fixture now uses relaxed C atomics, and Device B returns `ZE_RESULT_ERROR_UNSUPPORTED_FEATURE` for power-domain enumeration. CI confirmed all five armed tests and the exact clippy gate pass. The PR was squash-merged as `89cd907`; Issue #379 closed automatically.

---

## 1. Problem Statement

The earlier Level Zero loader check stopped after `dlopen` and symbol resolution. On a runner without an Intel GPU, a later `zeInit` failure is expected, but that also meant the following production paths had no executable CI coverage:

- Device enumeration and BDF mapping.
- The count-then-buffer convention, including the handle-count clamp and post-fill truncation.
- C/Rust FFI field ordering against the actual vendor ABI.
- Delta-derived engine utilization and power calculations.
- Point-in-time temperature, memory, frequency, and fan refreshes.
- Isolation when one Sysman metric family fails.

Self-referential Rust layout assertions can detect size mismatches, but they cannot reliably detect two same-typed fields being transposed if both the struct and its assertion repeat the same transcription error. The coverage therefore needed an independent C side compiled from Intel's own headers.

## 2. Change Summary

| Item | Value |
|------|-------|
| Files changed | 3 |
| Lines added | 868 |
| Lines deleted | 0 |
| Integration tests | 5 |
| Production code changed | No |

### Files

| File | Purpose |
|------|---------|
| `.github/workflows/ci.yml` | Installs vendor headers, compiles and verifies the fixture, scopes loader interposition, runs armed tests, and checks assertion markers. |
| `tests/fixtures/level_zero/stub_ze_loader.c` | Implements the 23 symbols resolved by the backend and supplies deterministic multi-device Sysman behavior. |
| `tests/level_zero_stub.rs` | Exercises enumeration, metrics, count edges, failure isolation, and unknown-BDF behavior through the public API. |

## 3. Technical Decisions

### 3.1 Compile against vendor headers

The C fixture includes Intel's Level Zero headers rather than copying Rust-side declarations into a second hand-written ABI. Values written through vendor-defined structures are then read through the project's `#[repr(C)]` Rust structures. This makes the test capable of detecting same-size field-order and offset defects that size-only assertions miss.

A deliberate proof commit swapped two `zes_freq_state_t` fields. CI run `32623903057` failed with `actual` receiving `1200` instead of `2100`, demonstrating that the test reaches and detects this defect class. The swap was reverted before merge.

### 3.2 Keep the test loader out of production configuration

The fixture is built as `libze_loader.so.1`, while the existing loader already opens that bare SONAME. `LD_LIBRARY_PATH` is changed only for the dedicated integration-test step, so normal CI steps and released binaries retain the standard loader search behavior.

An application-level environment variable that accepted an arbitrary loader path was intentionally avoided. No production branch, runtime override, binary payload, or release artifact was added for the fixture.

### 3.3 Use a separate integration-test process

The Level Zero runtime is stored in a process-wide `OnceCell`. Running these checks inside a unit-test binary could let whichever test initializes first permanently select either the real or stub loader. A dedicated integration-test target provides process isolation and deterministic loader selection.

### 3.4 Make mutable fixture state atomic

Rust runs tests concurrently by default. Plain C global counters would therefore permit unsynchronized reads and writes, which is a data race and undefined behavior. The final fixture uses `_Atomic uint64_t` and relaxed `atomic_fetch_add_explicit` operations. Relaxed ordering is sufficient because the test needs indivisible increments, not cross-variable synchronization.

### 3.5 Model an actual per-family failure

Device B now returns `ZE_RESULT_ERROR_UNSUPPORTED_FEATURE` from power-domain enumeration. The test refreshes the device twice and proves that render utilization remains fresh at exactly 10% while power alone is unavailable. This verifies that one family error does not suppress unrelated metrics.

## 4. Implementation Details

The fixture exports every symbol the backend resolves: core initialization and device functions, `zesInit`, and engine, power, temperature, memory, frequency, and fan families. Its values are deliberately distinct so that an offset error produces an obviously wrong value instead of a plausible one.

Counters advance by fixed increments with no clock or randomness. The second sample therefore yields exact expected values: 25% compute utilization, 10% render utilization, and 45 W. Another device reports 4,096 handles during the count query but fills only one, covering both the maximum-handle clamp and truncation to the actual filled count.

The five integration tests cover:

1. Sorted BDF enumeration end to end.
2. Exact engine-utilization and power deltas.
3. Point-in-time metric families populating `GpuInfo`.
4. Handle-count edges plus isolation of a failing family.
5. An unknown BDF producing no binding.

The tests are explicitly armed in CI. A success marker is printed only after assertions pass, and the workflow requires five markers, preventing an unsupported platform or an accidentally skipped test body from appearing green.

## 5. Review and Hardening

The final review examined correctness, security, performance, ABI boundaries, concurrent test execution, and CI reachability. It added commit `2443b4e` (`test: make the Level Zero stub concurrency-safe`) with the atomic-counter and true-error-path corrections described above.

No CRITICAL or HIGH security findings remained. The loader interposition is restricted to a single CI step, and the fixture cannot be reached through a shipped application setting. There is no production runtime performance impact. Relaxed atomics add negligible cost only to the test fixture.

## 6. Validation Results

### Local

- `cargo fmt --check`: passed.
- `cargo test --test level_zero_stub`: 5 passed in unarmed mode; the local machine did not have the Linux vendor-header fixture.
- `cargo test`: passed with 1,859 tests passed and 2 ignored; documentation tests also passed (23 passed, 13 ignored).
- `cargo clippy --all-targets -- -D warnings -A clippy::result_large_err`: passed.
- The exact local clippy command reported two pre-existing Rust 1.98 `result_large_err` warnings in untouched SSH strategy code. The repository's CI toolchain did not reproduce them.

### GitHub CI

Run `32653173247` passed all relevant jobs, including Test Suite, Build Check, packaging sync, systemd smoke, launchd smoke, and CLA. In the Test Suite:

- The C fixture compiled against vendor headers with `-Wall -Wextra -Werror`.
- Export verification found all required symbols.
- The armed integration target ran all 5 tests successfully.
- Five `all-smi: level-zero-stub-assertions-ran` markers were observed.
- `count_edges_and_a_failing_family_are_isolated` passed.
- The exact `cargo clippy --all-targets -- -D warnings` gate passed.

The optional self-hosted Windows Service Smoke Test remained skipped because its repository-level enable flag was not set. That job is unrelated to this Linux-only Level Zero fixture and was not a merge blocker.

## 7. Outcome and Follow-up

- PR #382 was squash-merged into `main` as `89cd907bbb5ee2be0426c3a2f733283a6a1760af`.
- Issue #379 was automatically closed by the PR's `Closes #379` link.
- Both the PR and issue carry `status:done`.
- Real Intel driver behavior and Windows Level Zero behavior remain outside this test's scope. Hardware-backed validation can supplement this deterministic ABI test but is not required for its stated acceptance criteria.

---

## Appendix: Commits Reviewed

| Commit | Purpose |
|--------|---------|
| `901e964` | Add the CI fixture and integration coverage. |
| `f375735` | Deliberately swap same-typed fields to prove detection. |
| `bae222a` | Revert the deliberate defect after recording the red run. |
| `2443b4e` | Remove fixture data races and exercise a real family error. |
