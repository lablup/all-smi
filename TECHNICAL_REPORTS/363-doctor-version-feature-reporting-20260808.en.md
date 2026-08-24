# Technical Report: PR #363 - Report the Pinned libamdgpu_top Version and level_zero in Doctor

**Date**: 2026-08-08  
**Status**: Completed  
**Related**: PR #363, Issue #362, surfaced during review of PR #358  
**Risk Level**: Low (diagnostic reporting only, no runtime behavior change)

---

## Executive Summary

PR #363 fixes two reporting defects in the `doctor` module. Neither breaks execution; both make `doctor` state something false or incomplete, which matters here because the module exists to be trusted when something is already wrong.

`amd.libamdgpu_top.abi` formatted `env!("CARGO_PKG_VERSION")` into its message, so it reported all-smi's own version rather than the dependency's: a default build of 0.25.0 reported `linked libamdgpu_top 0.25.0` while the pin was `=0.11.5`. Separately, `enabled_features()` in the support-bundle packer had no arm for `level_zero`, so a build with that feature understated itself in every bundle it produced.

---

## 1. Problem Statement

Both defects were found while reviewing #358 and deliberately left out of it to keep that PR scoped.

The second one has a structural cause worth naming: the arms in `enabled_features()` are `#[cfg]`-gated, so a runtime test observes only the features the test binary was itself built with. **A missing arm for a feature that is off is invisible to such a test by definition.** That is precisely how `level_zero` stayed missing from the day the feature landed, and how `amd` went missing until #358 added it.

## 2. Change Summary

| Item | Value |
|------|-------|
| Files changed | 2 |
| Lines added | 219 |
| Lines deleted | 3 |
| Tests added | 4 |
| Runtime behavior changed | No |

### Files

| File | Change |
|------|--------|
| `src/doctor/checks/amd.rs` | `LIBAMDGPU_TOP_PINNED_VERSION` constant replacing `env!("CARGO_PKG_VERSION")` in the ABI message, plus `pinned_version_matches_cargo_toml` and `libamdgpu_top_is_pinned_exactly`. |
| `src/doctor/bundle.rs` | The missing `#[cfg(feature = "level_zero")]` arm, plus `bundle_covers_every_declared_feature` and `enabled_features_matches_build_configuration`. |

## 3. Technical Decisions

### 3.1 Option A (constant plus a manifest-parsing guard test), not Option B (emit from `build.rs`)

Option B is attractive because it derives the value rather than duplicating it, which makes drift impossible by construction. It was rejected on two grounds.

**First, the acceptance criterion asks that a pin bump without a matching update fail a test.** Under Option B there is nothing left to fail, because the reported value silently follows the pin. That sounds strictly better until the parser is what breaks: a `Cargo.toml` reshuffle that stops matching the build script yields either a build failure at best or a silently wrong compile-time value at worst, and no test exists to say so. Option A keeps a real assertion that can be watched failing, which is the difference between a guard and an assumption.

**Second, Option B moves the parse into `build.rs`**, where it runs on every build of every consumer, including ones that will never enable the `amd` feature, and where a failure surfaces as a build-script error rather than a test name. Option A confines the same parse to the test harness. The cost is one transcribed string, and the test is what makes that cost safe.

### 3.2 The feature-coverage test parses source text, and that is the only thing that works

`bundle_covers_every_declared_feature` parses the `[features]` table of `Cargo.toml` and compares it against the feature gates found in the **source text** of `enabled_features`, both embedded with `include_str!` so no path guessing is involved.

This is unusual, and it is unusual for the reason given in section 1: a runtime test cannot observe a missing arm for a feature it was not compiled with. Reading the source is the only way to see arms that do not exist.

Brittleness was bounded rather than accepted:

- The scan is scoped to the span between `fn enabled_features()` and the next top-level `fn`, so gates elsewhere in the file, and the feature names appearing in the test module itself, cannot satisfy the assertion.
- Both the manifest parse and the function lookup fail loudly with an explanatory message instead of degrading to a vacuous pass.
- A sanity assertion rejects an empty or malformed feature list before the loop runs.
- `enabled_features_matches_build_configuration` covers the runtime half in both directions: a compiled-in feature must appear, a compiled-out one must not.

### 3.3 The constant's cfg keeps it from becoming dead code

`LIBAMDGPU_TOP_PINNED_VERSION` is gated on the reporting arm's own cfg plus `test`, so it is absent from musl, non-Linux, and `amd`-off builds where nothing reads it, and never becomes dead code, while the guard test still runs in every configuration.

The three-state reporting introduced by #358 is untouched: linked, compiled out by the musl gate, compiled out by the `amd` feature. The other six legitimate `env!("CARGO_PKG_VERSION")` sites are untouched.

## 4. Validation Results

Both guards were observed failing before being observed passing, which is the only way to know a guard is a guard.

**Defect 1 guard.** Pin temporarily moved to `=0.11.4`, then `cargo test --lib doctor::checks::amd`:

```
pinned_version_matches_cargo_toml ... FAILED
libamdgpu_top is pinned to 0.11.4 in Cargo.toml but amd.libamdgpu_top.abi reports 0.11.5
```

Pin reverted, same command: `2 passed; 0 failed`. `git diff Cargo.toml` empty before committing.

**Defect 2 guard.** `level_zero` arm removed, then `cargo test --lib doctor::bundle`:

```
bundle_covers_every_declared_feature ... FAILED
arms found: ["cli", "amd", "mock", "furiosa"]
```

That is exactly the pre-fix state. Arm restored, same command: `7 passed; 0 failed`.

| Gate | Result |
|------|--------|
| `cargo test --lib doctor` | 28 passed, 0 failed |
| `cargo run --bin all-smi -- doctor` (aarch64 glibc, default features) | `PASS amd.libamdgpu_top.abi  linked libamdgpu_top 0.11.5`, down from the 0.25.0 it reported before; `amd.build.target_env` still passes with its unchanged message |
| `cargo check --no-default-features --features cli` | pass |
| `cargo clippy --lib --tests -- -D warnings` | clean for default features and for `--no-default-features --features cli` |
| `cargo build --features level_zero` | compiles; a bundle from that build records `features: cli,amd,level_zero` in `version.txt` |
| `cargo fmt --check` | clean |

**Not verified locally**: the musl and non-Linux arms, since only `aarch64-unknown-linux-gnu` is installed on the development host. Those arms are unchanged by this PR, and the new constant is compiled out of both by its cfg while the guard test still runs there under `test`.

## 5. Outcome and Follow-up

- PR #363 was squash-merged into `main` as `b92ea0a`.
- Issue #362 closed automatically through the PR's `Closes #362` link.
- The `level_zero` arm this PR added became partly moot two weeks later: #365 made the backend compile-time-unconditional on Linux and Windows and moved its reporting to a separate `level_zero:` line in `version.txt`, since a cargo feature can no longer answer the question. The coverage test remains valuable for the features that are still features.

---

## Appendix: Key Terms

| Keyword | Description | Relevance |
|---------|-------------|-----------|
| `env!("CARGO_PKG_VERSION")` | Compile-time macro yielding the *containing* crate's version | The wrong value substituted for a dependency's pin |
| `include_str!` | Embeds a file's text at compile time | How the test reads both `Cargo.toml` and its own source without path guessing |
| vacuous pass | A test that passes because it observed nothing, not because the property holds | What the loud-failure paths and the sanity assertion prevent |
| exact pin (`=x.y.z`) | Cargo requirement admitting one version only | Why a transcribed constant can be checked against the manifest at all |
