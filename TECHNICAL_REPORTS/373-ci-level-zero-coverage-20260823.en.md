# Technical Report: PR #373 - Compile, Lint, and Test the level_zero Backend in CI

**Date**: 2026-08-23  
**Status**: Completed  
**Related**: PR #373, refs Issue #372 (Part A), Issue #364  
**Risk Level**: Low (CI configuration only, no source change)

---

## Executive Summary

The `level_zero` cargo feature was default-off and no CI job enabled it, so everything under `src/device/readers/intel_gpu_level_zero/` was never compiled, never linted, and its tests were filtered out of every run. Before this change, `grep -rn "level_zero" .github/workflows/` returned nothing. The module had 49 passing tests and none of them ran.

PR #373 is Part A of #372: CI coverage only. Part B, shipping the feature in release artifacts, was deliberately not in this PR and stayed gated on #364. It was later superseded entirely by #365, which made the backend unconditional on Linux and Windows.

---

## 1. Problem Statement

The hole had already cost something concrete: #364's analysis attributes two of its five root causes to `intel_gpu_level_zero/apply.rs`, which no job compiled. Defects there cannot be caught by review alone when nothing else is looking.

A module with 49 tests that never run is worse than a module with none, because the test count reads as coverage in every summary that quotes it.

## 2. Change Summary

| Item | Value |
|------|-------|
| Files changed | 1 (`.github/workflows/ci.yml`) |
| Lines added | 42 |
| Lines deleted | 1 |
| Source changed | No |
| `Cargo.toml` changed | No |

### Steps added

| Job | Step |
|-----|------|
| Linux `test` | `cargo test --verbose --features level_zero` |
| Linux `test` | `cargo clippy --features level_zero --all-targets -- -D warnings` |
| `build-check` | release-profile build with the feature on, `--locked` retained |
| Windows | `--features level_zero` added to the existing invocation |

## 3. Technical Decisions

### 3.1 The new steps are additional, not replacements

The default-feature build is what `cargo install all-smi` and every downstream crate without `default-features = false` resolve to, so it has to stay covered on its own. Replacing the existing steps would have traded one blind spot for another.

### 3.2 `--all-targets` is deliberate where the existing clippy step has no such flag

The module's tests sit behind the same feature gate, so linting only lib and bin would leave them unchecked for the second time. `cargo fmt --check` is untouched, being feature-independent.

### 3.3 `--locked` is kept on the release build on purpose

`level_zero = []` activates no dependency and therefore must not move `Cargo.lock`. If that ever stops being true, it fails here rather than in the vendored Debian build, which uses the stricter `--frozen` and is a much worse place to discover it.

### 3.4 The Windows step is coverage when available, not a guarantee

`src/device/readers/intel_gpu_windows.rs` gates its Level Zero augmentation on both `cfg(target_os = "windows")` and the feature, so without the flag that code compiles nowhere in CI at all. The job is opt-in via `ENABLE_WINDOWS_SERVICE_SMOKE`, so adding the flag improves what runs when the runner is up without promising anything when it is not. It stayed `skipping` throughout, because the self-hosted runner was down.

## 4. Validation Results

### Local, on Windows 11 Pro 26200, native `x86_64-pc-windows-msvc`, rustc 1.97.1

```
cargo test --features level_zero --lib intel_gpu_level_zero
test result: ok. 49 passed; 0 failed; 0 ignored; 0 measured; 1467 filtered out

cargo clippy --features level_zero --all-targets        (exit 0)
```

No findings in `intel_gpu_level_zero`. Six warnings are emitted, and since the new step adds `-D warnings`, each was checked against whether it can reach the Linux runner. All six are Windows-only:

| Site | Why it does not warn on Linux |
|------|-------------------------------|
| `src/api/shutdown.rs:106` | carries `#[cfg_attr(not(windows), allow(dead_code))]` |
| `src/device/readers/windows_gpu_perf.rs:119` | Windows-only file, not compiled on Linux |
| `src/device/readers/windows_gpu_perf/ids.rs:52` | same |
| `src/main.rs:39` (`SocketSetting`) | consumed inside the `#[cfg(unix)]` block at `src/main.rs:311` |
| `src/main.rs:322` (`interval`) | consumed inside the `#[cfg(target_os = "linux")]` block at `src/main.rs:338` |
| `src/utils/command_timeout.rs:190` | backs two `#[cfg(unix)]` tests, so the import is live on Linux |

These are the pre-existing native-Windows findings tracked in #367, present with or without this change.

Lockfile stability was confirmed directly, since the build-check leg keeps `--locked`:

```
cargo build --release --target x86_64-pc-windows-msvc --locked --features level_zero   -> exit 0, 2m41s
Cargo.lock sha256 before: 6cd3928b31b8fbd079e9917c3817b16b94f15326dbaddb8eda6013683e286b32
Cargo.lock sha256 after:  6cd3928b31b8fbd079e9917c3817b16b94f15326dbaddb8eda6013683e286b32
```

### CI, run 32616550644, after rebasing onto `7472d23`

All three new steps succeeded, and the test step's numbers show the exact size of the hole this PR closes:

| Step | lib / bin passed | `intel_gpu_level_zero::` tests executed |
|------|------------------|------------------------------------------|
| `Run tests` (existing) | 1596 / 1772 | **0** |
| `Run tests (level_zero)` (new) | 1645 / 1821 | **49** |

Exactly +49 in each target, matching the 49 that passed locally on Windows. `Run clippy (level_zero)` finished clean in 41.18s with `--all-targets`, which is the first time any job linted the integration-test targets, including `tests/library_api_test.rs` added by #375. `Build with the level_zero feature` passed with `--release --locked`, so the feature still does not move `Cargo.lock`.

`Windows Service Smoke Test` remained `skipping`: the self-hosted Windows runner was down, so the `--features level_zero` change to that job stays unverified in CI.

## 5. Outcome and Follow-up

- PR #373 was squash-merged into `main` as `64a8651`.
- What this PR deliberately did not do: ship the feature. Release artifacts, `debian/rules`, and the docs were Parts B and C of #372 and stayed gated on #364. It also did not change `Cargo.toml`, so the feature remained default-off for `cargo build` and for library consumers.
- It also did not exercise the runtime-absent degradation path inside the Level Zero code, which needs an Intel GPU host without the oneAPI runtime.
- **Superseded within the same cycle.** #365 made the backend compile into every Linux and Windows build through a build cfg, which made the two `--features level_zero` steps added here redundant on Linux: a plain `cargo test` now reaches the module. The `--all-targets` half of the clippy pair was the durable addition and moved onto the default clippy step, so every test target in the crate is now linted rather than just lib and bin.

---

## Appendix: Key Terms

| Keyword | Description | Relevance |
|---------|-------------|-----------|
| `--all-targets` | Lints tests, benches, and examples in addition to lib and bin | The reason feature-gated tests were being linted for the first time |
| `--locked` / `--frozen` | Fail if `Cargo.lock` would change / also forbid network access | Why lockfile drift fails in CI rather than in the Debian build |
| default-off feature | A cargo feature not in the `default` set | The condition that made 49 tests invisible |
