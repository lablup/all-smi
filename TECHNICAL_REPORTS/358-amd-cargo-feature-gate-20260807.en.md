# Technical Report: PR #358 - Gate the AMD Backend Behind a Default-On `amd` Cargo Feature

**Date**: 2026-08-07  
**Status**: Completed  
**Related**: PR #358, Issue #345  
**Risk Level**: Medium (changes what a downstream `default-features = false` resolves to)

---

## Executive Summary

PR #358 moves the AMD GPU backend behind a new `amd` cargo feature, enabled by default. `libamdgpu_top` was a non-optional dependency, and it pulls in `libdrm_amdgpu_sys`, which links `libdrm.so.2` and `libdrm_amdgpu.so.1` unconditionally. Every Linux binary depending on all-smi therefore inherited both as hard `NEEDED` entries, so a host without AMD's userspace DRM libraries, which is the overwhelming majority of deployments, failed to start with a loader error before `main` ran. The program cannot catch that or degrade to "no AMD GPU detected".

A downstream crate that declares `default-features = false` now stops inheriting those two entries. This was verified by comparing `objdump -p ... | grep NEEDED` between the two build configurations, not by reading the manifest.

---

## 1. Problem Statement

The failure this fixes happens before any all-smi code runs. The dynamic loader resolves `NEEDED` entries at process start, so a missing `libdrm.so.2` is a hard start failure with no branch for the program to take. There is no graceful degradation available at that layer, which is what separates this from an ordinary "handle the absent device" problem.

`lablup/backend.ai-go` already declared `all-smi = { version = "0.25.0", default-features = false }`, so it was carrying the entries for a backend it had explicitly opted out of, and reported the startup failure.

## 2. Change Summary

| Item | Value |
|------|-------|
| Files changed | 13 |
| Lines added | 212 |
| Lines deleted | 34 |
| `cfg` sites widened | 12 |
| `Cargo.lock` movement | None |

### Manifest shape

```toml
[target.'cfg(all(target_os = "linux", not(target_env = "musl")))'.dependencies]
libamdgpu_top = { version = "=0.11.5", optional = true }

[features]
default = ["cli", "amd"]
amd = ["dep:libamdgpu_top"]
```

The existing pin comment (semver-violating patch releases, the 0.11.5 file-descriptor-leak fix) is preserved and extended. The `[features]` block documents `amd` in the same voice as `furiosa` and `level_zero`, including why it is default-on where those two are default-off.

### The twelve widened `cfg` sites

| File | Site |
|------|------|
| `src/device/readers/mod.rs` | `pub mod amd;` |
| `src/device/reader_factory.rs` | `has_amd` import, `amd` import, the `AmdGpuReader` push |
| `src/device/platform_detection.rs` | `has_amd`, `detect_amd`, and the `introspection::detect_amd` pair |
| `src/utils/system.rs` | both sudo-permission blocks |
| `src/doctor/checks/amd.rs` | `check_libamdgpu_top`, `check_build_gate` |
| `src/doctor/checks/platform.rs` | `check_runtime` |

## 3. Technical Decisions

### 3.1 Default-on, and why that settled the packaging question

Default-on means every existing distribution path is byte-for-byte unchanged and keeps AMD support: the glibc release binaries, `cargo install all-smi`, Homebrew, and a plain `cargo build`. `Cargo.lock` does not move. Release workflows, packaging, and the Homebrew formula need no changes, which is the main reason default-on was chosen over default-off.

The trade-off is stated plainly rather than hidden: `--no-default-features` also drops `cli`, so a consumer that wants the CLI but not AMD needs `default-features = false, features = ["cli"]`. That is documented in all four docs this PR touches.

### 3.2 Why not runtime `dlopen` of `libdrm`

Option 2 in the issue was evaluated and rejected for this PR. `libdrm` is linked by the third-party `libamdgpu_top` crate, and `src/device/readers/amd.rs` uses its types throughout, so runtime loading means replacing that crate with hand-rolled FFI. That is a much larger change with a much larger blast radius, and it does not block the fix the reported failure needs. It was filed separately as **#359**, which remains open.

### 3.3 This is not a new configuration

`libamdgpu_top` was already excluded from musl builds, and `release.yml` ships `all-smi-linux-x86_64-musl` and `all-smi-linux-aarch64-musl` on every release. The change makes an already-shipping shape reachable on glibc, rather than inventing an untested one.

### 3.4 The negated complement is marked so it cannot drift

The complement in `introspection` is `not(all(target_os = "linux", not(target_env = "musl"), feature = "amd"))`, an exact complement of the positive arm. A comment marks it as such, so a later edit cannot silently drop or duplicate `detect_amd`.

The codebase has no `cfg` alias precedent (no `cfg_aliases` build dependency), so the predicate is written out at each of the twelve sites rather than introducing a new mechanism for one PR.

### 3.5 Diagnosability: the third state the doctor could not express

A glibc build with the feature off is a state the doctor previously could not represent, and it would have reported a false musl explanation. `all-smi doctor` on a feature-off build now reports:

```
WARN amd.build.target_env   glibc build without the `amd` cargo feature: AMD support compiled out
SKIP amd.libamdgpu_top.abi  libamdgpu_top not linked: built without the `amd` cargo feature
WARN platform.runtime       target aarch64-unknown-linux-gnu (env=gnu), built without the
                            `amd` cargo feature so AMD GPU support is compiled out
```

On the default build all three pass instead, with their existing messages unchanged. `amd.build.target_env` keeps its check id; only the internal function was renamed from `check_musl_gate` to `check_build_gate`, since it now reports two independent gates. `doctor --bundle` records the feature too, so a support bundle answers the question directly: `features: cli,amd` versus `features: cli`.

Warning on a deliberately feature-off build, and the resulting exit code 1, matches the existing musl behaviour exactly, so this introduces no new convention.

## 4. Validation Results

Run on `aarch64-unknown-linux-gnu`.

### The decisive check: `NEEDED` comparison

```
$ cargo build --release && objdump -p target/release/all-smi | grep NEEDED
  NEEDED  libdrm.so.2
  NEEDED  libdrm_amdgpu.so.1
  NEEDED  libgcc_s.so.1
  NEEDED  libm.so.6
  NEEDED  libc.so.6
  NEEDED  ld-linux-aarch64.so.1

$ cargo build --release --no-default-features --features cli && objdump -p target/release/all-smi | grep NEEDED
  NEEDED  libgcc_s.so.1
  NEEDED  libm.so.6
  NEEDED  libc.so.6
  NEEDED  ld-linux-aarch64.so.1
```

Both `libdrm` entries are gone and nothing else changed. The measured configuration is `--no-default-features --features cli`, because `--no-default-features` alone drops `cli` and therefore the binary.

### Dependency resolution

| Command | Result |
|---------|--------|
| `cargo tree -e normal -i libamdgpu_top` | present, `all-smi` as sole dependent |
| same, `--no-default-features` | package not found (the pass condition) |
| same, `--no-default-features --features cli` | package not found |
| `cargo tree -e normal --target x86_64-pc-windows-msvc` | exit 0, 0 occurrences |
| same, `--target aarch64-apple-darwin` | exit 0, 0 occurrences |
| same, `--target x86_64-unknown-linux-musl` | exit 0, 0 occurrences |

The three cross-target results confirm that `amd` being on for targets where the dependency is not declared resolves cleanly, and the musl result confirms the pre-existing musl exclusion still holds.

### Build and lint

`cargo check` passes with default features, `--no-default-features`, and `--no-default-features --features cli`. `cargo clippy --lib --tests -- -D warnings` passes in both the default and the feature-off configuration. No dead-code or unused-import fallout appeared behind the disabled feature, and no blanket `#[allow]` was added.

### CI guard

The existing `build-check` job gains a regression guard that builds `--no-default-features --features cli` and fails if any `NEEDED` entry matches `libdrm`.

## 5. Outcome and Follow-up

- PR #358 was squash-merged into `main` as `7320e5c`.
- Issue #345 closed automatically through the PR's `Closes #345` link.
- Documentation updated in `README.md`, `DEVELOPERS.md`, `docs/ARCHITECTURE.md`, and `docs/LIB_mode.md`. `docs/ARCHITECTURE.md` had a stale `[features]` block claiming `default = []`; it now matches `Cargo.toml`.
- This shipped in v0.26.0 as a behavior change for downstream consumers: `default-features = false` now drops AMD support along with the CLI.
- **#359 remains open**: restoring AMD detection for opt-out consumers through runtime `dlopen`, which is the part this PR deliberately did not take on. It carries `priority:high`.
- PR #363 followed to fix two doctor reporting defects surfaced while reviewing this PR.

---

## Appendix: Key Terms

| Keyword | Description | Relevance |
|---------|-------------|-----------|
| `NEEDED` entry | ELF dynamic-section record naming a required shared library | The exact thing that made a missing `libdrm` fatal before `main` |
| `dep:` prefix | Cargo syntax activating an optional dependency without exposing an implicit feature | How `amd = ["dep:libamdgpu_top"]` avoids a second feature name |
| optional dependency | A manifest dependency compiled only when a feature activates it | The mechanism that lets the entries disappear |
| `default-features = false` | Downstream opt-out of a crate's default feature set | What a consumer sets to shed `libdrm` |
