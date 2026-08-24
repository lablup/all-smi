# Technical Report: PR #381 - Report Level Zero Loader, Init, and Device Visibility in Doctor

**Date**: 2026-08-23  
**Status**: Completed; the init and device stages are exercised only through their verdict functions (see section 6)  
**Related**: PR #381, Issue #380, follow-up filed from PR #365  
**Risk Level**: Low (diagnostic checks only, no runtime behavior change)

---

## Executive Summary

`all-smi doctor` had no Intel check at all. PR #365 added a `level_zero:` line to the support bundle's `version.txt`, but that reports a compile-time cfg: it cannot say whether the loader was found, whether `zeInit` succeeded, or whether Sysman saw a device, and those three decide whether an operator gets GPU temperature, power, and frequency on Intel hardware. On Windows nothing else supplies those fields at all.

PR #381 adds four checks that report what the cfg cannot, by having the loader record which stage it reached rather than by re-deriving it.

---

## 1. Problem Statement

Four failures used to collapse into one silent fallback, each with a different remedy:

1. No candidate path loaded.
2. The loader exports no `zesInit` and `ZES_ENABLE_SYSMAN=1` was not set before `zeInit`.
3. `zeInit` failed.
4. `zesInit` failed.

A fifth state is worth reporting and was equally invisible: initialisation succeeded and Sysman enumerated nothing, which produces empty metrics with no other symptom.

## 2. Change Summary

| Item | Value |
|------|-------|
| Files changed | 7 |
| Lines added | 662 |
| Lines deleted | 4 |
| New doctor checks | 4 |
| Unit tests added | 10 (plus a redaction test) |

### The checks

| id | worst outcome | reports |
|----|---------------|---------|
| `level_zero.build` | Info | compiled in for this target |
| `level_zero.loader` | Warn | which `LIBZE_PATHS` entry loaded |
| `level_zero.init` | Warn | initialised, and by which Sysman route |
| `level_zero.devices` | Info | how many devices Sysman enumerated |

## 3. Technical Decisions

### 3.1 Recording, not re-deriving

`initialize_runtime` now writes which stage it reached into a `OnceCell` beside `LZ_RUNTIME`, and `probe()` reads it back. Doctor performs no `dlopen` and no `zeInit` of its own.

That is the point rather than an optimisation. `LZ_RUNTIME` is initialised once per process, so a diagnostic with its own loading path would be a **second implementation, free to drift from the one that actually decides whether metrics appear**. A diagnostic that succeeds where the real path fails is worse than no diagnostic.

The recording adds no library load, changes no return value, and promotes no log level.

### 3.2 None of the checks can FAIL

An absent runtime degrades to the sysfs or WMI baseline rather than breaking a run, so the worst honest verdict is a warning. A FAIL would tell an operator that something is broken when the program is behaving exactly as designed.

### 3.3 `level_zero.loader` deliberately does not skip without hardware

`level_zero.init` and `level_zero.devices` skip when no Intel GPU is present, since such a host is not broken.

`level_zero.loader` does not, for three reasons: it is the one stage a machine without the hardware can reach, whether the runtime is installed is worth knowing before the card arrives, and gating it would make the whole namespace unreachable in CI.

### 3.4 Device counts are always printed; BDFs ride behind `--verbose`

The count answers the question. The PCI bus/device/function values identify specific hardware, which is exactly the kind of detail that belongs behind an explicit opt-in in a diagnostic that operators paste into issues.

## 4. Validation Results

- **10 unit tests on pure verdict functions covering every branch**: loader present without hardware, hardware without loader, neither, all three init failures kept textually distinct with the numeric `ze_result_t` in the message, zero devices after a successful init, and the verbose split.
- **A test pins that a PCI BDF survives `doctor::redact`.** `0000:03:00.0` is colon-separated hex, the shape both the IPv6 and the MAC matcher hunt for, and a mangled BDF makes the device check useless in the exact situation it exists for.
- **A CI step on the Linux job** runs `doctor --only level_zero --json` and asserts build PASS, loader PASS naming the library, and exit 0. The runner installs `libze1` and has no Intel GPU, which is precisely the loader-without-hardware case.
- **3448 tests on macOS** across 23 binaries and 1641 lib tests through the scratch probe with the backend compiled in, 0 failures. `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings` clean on both.

**Not verified**: no Intel GPU was available, so the init and device stages are exercised only through their verdict functions, never against a live Sysman. Reaching those without hardware is #379, delivered by PR #382 the same day.

## 5. Outcome and Follow-up

- PR #381 was squash-merged into `main` as `ba82d92`.
- Issue #380 closed automatically through the PR's `Closes #380` link.
- README gained the four check ids.
- PR #382 landed alongside it, driving the backend against a stub loader so the paths past `try_load_library` get executed in CI. Together the two close most of the gap #365 left, without hardware.

---

## Appendix: Key Terms

| Keyword | Description | Relevance |
|---------|-------------|-----------|
| Sysman | The Level Zero system-management API supplying temperature, power, frequency | The layer whose availability the checks report |
| `ZES_ENABLE_SYSMAN` | Environment variable enabling Sysman on loaders without `zesInit` | One of the four previously indistinguishable failure modes |
| `ze_result_t` | Numeric Level Zero status code | Kept in the message so three init failures stay distinguishable |
| redaction | Stripping identifying values from diagnostic output | Why a BDF surviving `doctor::redact` needed its own test |
