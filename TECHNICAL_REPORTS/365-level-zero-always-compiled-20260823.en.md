# Technical Report: PR #365 - Compile the Intel Level Zero Backend into Every Linux and Windows Build

**Date**: 2026-08-23  
**Status**: Completed for the packaging decision and the four defects; Sysman metric calls remain unexercised on hardware (see section 7)  
**Related**: PR #365, refs Issue #364 and Issue #372, builds on PR #376  
**Risk Level**: High (changes what every Linux and Windows artifact contains, and corrects four reporting defects)

---

## Executive Summary

PR #365 makes `build.rs` emit an `all_smi_level_zero` cfg, and every consumer gates on that instead of `feature = "level_zero"`. It is on for **every Linux and Windows target**, off for everything else including macOS, and the `level_zero` cargo feature survives only as an accepted no-op so `--features level_zero` and any downstream manifest listing it keep building.

The branch was rewritten against current `main` (`64a8651`) after #376 landed the classification and capacity half of #364 while it was open. Everything that overlapped is gone; what remains is the part #376 did not reach: the packaging decision that makes the Intel backend present at all on Windows, and four defects that decision exposes. The original commit is preserved at `744dab9`.

---

## 1. The Decision

| Target | `--features level_zero` | Backend compiled in |
|--------|-------------------------|---------------------|
| Linux | either | **yes** |
| Windows | either | **yes** |
| macOS | either | no |

Four reasons it is unconditional rather than opt-in:

- **It adds no dependency.** The loader is `dlopen`ed through `libloading`, already an unconditional dependency on both targets, so compiling the backend in adds no `NEEDED` entry and no import-table entry. `tpu_pjrt` already dlopens on Linux with no musl guard, so the musl artifacts gain no new class of behaviour either.
- **It costs nothing without the hardware.** `reader_factory` builds the Intel reader only when an Intel GPU is actually present, which on Linux means `/sys/class/drm` shows one. The gate is GPU presence, not CPU vendor, so an AMD host never opens the loader. With a GPU but no runtime, the failed load is cached process-wide behind the existing `OnceCell` and the sysfs or WMI baseline stands.
- **We ship one artifact per target.** An opt-in backend would mean publishing an Intel and a non-Intel package for the same platform. An Intel Arc owner would otherwise have to build from source to get anything the vendor backend adds.
- **On Windows nothing else can supply the fields.** GPU temperature, power, and frequency have no WMI, DXGI, or PDH source.

The one asymmetry worth stating plainly rather than glossing: **Linux does have a sysfs baseline for those fields**, so there the backend is an upgrade (the XMX `COMPUTE_SINGLE` engine class that sysfs cannot reach, energy-counter power, dedicated memory state) rather than the difference between data and empty columns. The first three points apply identically on both.

Cargo cannot express a per-target feature default, hence a cfg rather than a manifest entry. The practical consequence is that `features:` in a support bundle no longer says anything about this backend, so `all-smi doctor` writes a separate `level_zero: compiled-in | absent` line into `version.txt`, derived from the cfg.

This also settles the Part B question in #372: there is nothing to enable in the release workflow, because the target decides.

## 2. Change Summary

| Item | Value |
|------|-------|
| Files changed | 20 |
| Lines added | 1204 |
| Lines deleted | 188 |
| Tests added | 27 |
| `intel_gpu_level_zero::` tests reached by a plain `cargo test` | 0 to 56 |

### Files of substance

| File | Purpose |
|------|---------|
| `build.rs` | Emits the `all_smi_level_zero` cfg for Linux and Windows targets. |
| `Cargo.toml` | `level_zero` retained as an accepted no-op, documented. |
| `src/device/readers/detail_keys.rs` | New, unconditionally compiled shared helper (see 3.2). |
| `src/device/readers/intel_gpu_level_zero/apply.rs` | The Sysman-over-DXGI capacity fix and the `Note` key rework. |
| `src/device/readers/windows_gpu_perf.rs`, `pdh.rs` | Shared-usage memory pools and the DXGI-to-PDH handoff. |
| `src/device/readers/intel_gpu_names.rs` | Panther Lake PCI device-ID range. |
| `src/doctor/bundle.rs` | The `level_zero:` line in `version.txt`. |
| `.github/workflows/ci.yml` | Installs the real loader, arms the loader assertion, removes the now-redundant feature steps. |

## 3. The Four Defects This Decision Exposes

### 3.1 Sysman overwrote the capacity DXGI had just resolved

#376 taught the DXGI layer to report an integrated part's shared aperture instead of its 128 MiB stolen carve-out. `intel_gpu_level_zero::apply` then assigned `total_memory` from the Sysman dedicated pool **unconditionally**, putting the carve-out straight back. On a B390 that is 17.88 GiB reported as 128 MiB, which is the original symptom of #364 arriving through a second door.

The Sysman figure is now kept as `VRAM Dedicated (L0)` and the aperture stands. A discrete card still takes its Sysman total, and Linux is unaffected.

### 3.2 `Metrics Source` was assigned, not appended

Each layer knows only about itself, so the last one to run erased the record of the others. A Windows host with the full stack reported `WMI + Level Zero Sysman`, losing DXGI and PDH; on Linux the `(engine counters)` qualifier disappeared the moment Sysman produced a reading.

The helper moved into a new always-compiled `src/device/readers/detail_keys.rs`, **because the layers that write these keys sit behind disjoint `cfg` gates and could not call a helper held inside any one of them.** That is the same reason the bug existed, and it is why the module is unconditional rather than Windows-gated.

### 3.3 Every Intel and AMD iGPU on Windows reported 0 bytes in use

PDH sampled only `Dedicated Usage`, whose instances are a flat zero on an integrated part since nothing is allocated out of the carve-out. The `Shared Usage` families are now read for adapters whose capacity is a shared aperture, per adapter and per process.

They are added to the query **only once DXGI has reported such an adapter**: `snapshot()` enumerates DXGI first and passes the answer to `pdh::sample`, so a machine with only discrete cards never adds the counters and never pays for them on any poll.

There is deliberately **no cross-pool fallback**, because `Source: Memory Used` is labelled from the same flag and a substituted figure would sit under a label that does not describe it.

### 3.4 The `Note` key was a blanket string

Every poll published "Detailed metrics require Level Zero / xpu-smi", now wrong in both directions: the backend cannot be the thing to go install, and the note fired next to the very fields it claimed were unavailable.

It now names the fields no layer could supply, and says nothing when there are none. An integrated part legitimately exposes no Sysman thermal sensor, so "nothing missing" and "temperature missing" are both normal, and saying which one this machine is in is the useful part.

### Also: the Panther Lake device-ID range

Adds `0xB080-0xB08F` to the marketing-name table. The Linux reader has no marketing string to read: it resolves the name from sysfs, so without a table entry the reporting host's `8086:b080` became `Intel Graphics (device 0xb080)` and then classified as `Unknown`, since every architecture rule keys off the name.

## 4. Loading the Real Loader in CI

The Level Zero loader ships as its own Ubuntu package, separate from the Intel GPU driver, so a runner with no GPU can still load it. The Linux job now installs it and sets `ALL_SMI_EXPECT_LEVEL_ZERO_LOADER=1`, which turns a new test from a skip into an assertion: every path in `LIBZE_PATHS` must resolve, and every mandatory symbol in `LzApi` must be spelled the way the real library exports it. Both are invisible to the compiler, and either one being wrong turns the whole backend into a silent no-op on real hardware.

It deliberately stops short of `zeInit`. A runner with no Intel GPU has no driver for the loader to hand back, so initialisation failing there is correct behaviour rather than a defect, and asserting on it would make the test fail for the wrong reason.

Note what this does **not** buy: a GitHub-hosted runner having an Intel CPU is irrelevant here. There is no GPU, so `has_intel_client_gpu()` is false, the reader is never constructed, and no Sysman call is ever made. Exercising the metric paths without hardware would need a stub `libze_loader.so.1`, which became #379 and then PR #382.

### CI steps that became redundant

`cargo test` and `cargo clippy` with no flags now reach the backend on Linux, so the two `--features level_zero` steps added in #373 were testing the same code twice. The `--all-targets` half of that pair was the real addition and moves onto the default clippy step, which means every test target in the crate is now linted rather than just lib and bin. What stays feature-specific is the no-op contract itself: one debug build asserting the flag is still accepted, and the existing release build asserting it does not move `Cargo.lock`.

## 5. Validation Results

CI on this branch, run 32621525738, everything green except the Windows job, which was `skipping` on the stopped self-hosted runner.

**The Linux always-on change does what it claims.** A plain `cargo test` with no flags executed **56 distinct `intel_gpu_level_zero::` tests**. Before #373 that number was 0 in every configuration, and after #373 it took an explicit `--features level_zero` to reach them. Totals: lib 1672, bin 1848.

**The loader really loads.** `libze1` installs `libze_loader.so.1` to `/lib/x86_64-linux-gnu`, and the dedicated step printed the marker the test emits only after asserting:

```
running 1 test
all-smi: level-zero-loader-assertion-ran
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 1672 filtered out
```

So every path in `LIBZE_PATHS` resolves against a real loader, and every mandatory symbol in `LzApi` is spelled the way the library exports it. The three other branches of that logic were exercised locally through the scratch probe: without the env key the test skips and passes; with the key set on a host with no loader it fails with the intended message; and the marker is absent in that failing case, so the grep guard cannot pass vacuously.

**Local, on macOS**: 3446 tests across 23 binaries, 0 failures. `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings` clean. Through the scratch probe with the backend compiled in and the Windows reader's gate widened: 1630 lib tests, and reachability proven by injecting a deliberate type error into `annotate_missing_metrics` and watching it surface.

27 new tests, all reachable by the Linux runner: the `detail_keys` contract, the DXGI-to-Sysman memory handoff in both directions, the usage-pool selection and the per-process split, the device-ID range and its boundaries, the `level_zero:` line in `version.txt`, and the loader symbol resolution.

## 6. What Is Not Verified

No B390, no Windows host, and the self-hosted Windows runner was down, so the Sysman metric calls and the PDH counter paths are unexercised on real hardware. The loader step above closes the nearest reachable part of that gap; what remains needs either a device or a stub loader. Everything else here is a decision made around those calls, which is where all four defects lived.

The loader test's package name could not be confirmed from a macOS host with no Level Zero loader available. The CI run on this branch is what confirms it, and the step fails loudly with a pointed message rather than silently skipping if the name is wrong.

## 7. Outcome and Follow-up

- PR #365 was squash-merged into `main` as `dd64e21`.
- Two follow-ups were filed rather than widening the PR:
  - A stub `libze_loader.so.1` in CI to reach the Sysman paths without hardware. This became **#379**, delivered by PR #382.
  - A doctor check for the runtime, since `level_zero: compiled-in` tells an operator the backend is in the binary but nothing tells them whether the loader actually came up. This became **#380**, delivered by PR #381.
- **Issue #377 stays open**: the Arc B390 utilization symptom and the remaining `apply.rs` sites from #364.
- **Issue #378 stays open**: the Windows Intel and AMD readers still publish 0 for utilization and power that no layer sourced, instead of the `GPU_METRIC_UNAVAILABLE` sentinel.
- This shipped in v0.26.0. For consumers, the practical change is that `features:` in a support bundle no longer answers the Level Zero question; read `doctor`'s `level_zero:` line instead.

---

## Appendix: Key Terms

| Keyword | Description | Relevance |
|---------|-------------|-----------|
| build cfg | A `cfg` emitted by `build.rs` rather than declared in the manifest | How a per-target default is expressed when cargo cannot express one |
| accepted no-op feature | A feature retained so existing manifests keep building, gating nothing | Why `--features level_zero` still works after the switch |
| `OnceCell` load cache | A process-wide record of whether the loader came up | Why a missing runtime costs one failed load, not one per poll |
| shared aperture versus dedicated pool | The two memory layouts DXGI distinguishes | The distinction Sysman was overwriting |
