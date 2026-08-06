# Technical Report: PR #339 - ci: build the Docker image on pull requests

**Date**: 2026-08-06
**Status**: Completed
**Languages**: YAML (GitHub Actions), Rust (test)
**Risk Level**: Low (CI trigger and caching-policy change plus two new tests; no application source touched)

---

## Table of Contents

1. [Executive Summary](#executive-summary)
2. [Problem Statement](#1-problem-statement)
3. [Technical Review](#2-technical-review)
4. [Technical Decisions](#3-technical-decisions)
5. [Implementation Details](#4-implementation-details)
6. [Learning Points](#5-learning-points)
7. [Further Learning](#6-further-learning)
8. [Change Summary](#7-change-summary)
9. [Follow-up Actions](#8-follow-up-actions)
10. [Appendix](#appendix)

---

## Executive Summary

`docker-check` was gated on `github.event_name == 'push' && github.ref == 'refs/heads/main'`, so an image break could only ever surface as a red default branch, never as a red pull request. Issue #328 was filed after exactly that happened: PR #319 (#309) added the crate's first `include_str!`-embedded asset, the Dockerfile's builder stage did not copy the directory it lived in, every PR check passed, and `main` went red on the merge commit `74f75d2`. The issue asked for the decision to be made on measured cost, not an estimate, and the decision rests on two numbers pulled from `gh api .../actions/runs/<id>/jobs` across ten runs on `main`: `docker-check` and `build-check` are both `needs: test`, so they start together, and `docker-check` finishes a mean of 2 seconds after `build-check`, which already runs on every pull request. Since the repository is public and standard runner minutes are not billed, the measured marginal cost of running the full image build on every PR is about 2 seconds of wall clock, confirmed on this PR's own CI run (31106052626): the Docker build finished 109 seconds before the gating `build-check` job.

The rejected alternative closes the loop on why the issue existed at all: a path filter on `Dockerfile`/`.dockerignore`/`Cargo.toml`/`Cargo.lock`/`build.rs` would have skipped the build on 19 of the last 40 commits on `main` (48%), and that skip set includes `74f75d2` itself, the commit that actually broke `main`, since it added a source-level `include_str!` and touched none of the filter paths. A filter that misses the one empirical failure on record, to save a cost that already rounds to zero, was rejected as a bad trade. `cache-to` is made push-only (pull requests read `main`'s warm cache and write nothing back) to avoid PR-triggered `mode=max` exports churning a shared, LRU-evicted ~13.3 GB cache that `main` depends on. `tests/docker_build_context_test.rs` gains the reverse-direction check the existing embedded-asset test did not have: every builder-stage `COPY` source must actually exist in the build context, catching a `COPY` left pointing at a path an unrelated change renamed or deleted, verified against a real `docker buildx build` failing on the identical input in 2 seconds. Total: 2 files, +169/-11, one commit, closing #328. An incidental finding, recorded in an issue comment rather than fixed here: `docker build --platform linux/arm64` is broken independently of this issue, because `Cargo.toml`'s vendored-OpenSSL path for `aarch64`+`gnu` needs perl's `FindBin`, which `rust:1.96-slim` does not carry.

---

## 1. Problem Statement

### 1.1 Background

`.github/workflows/ci.yml`'s `docker-check` job builds the project's Docker image via `docker/build-push-action`, with GHA layer caching (`cache-from: type=gha`, `cache-to: type=gha,mode=max`) already wired up before this PR. Before this PR it ran only on a push to `main`, so it could never gate a pull request; the only signal an image break produced was a red default branch, discovered after the fact.

### 1.2 Existing Issues

- **Issue 1 (the gate that should have caught the break never ran on the PR that introduced it)**: PR #319 (#309) added `packaging/systemd/all-smi.service`, embedded via `include_str!`. The Dockerfile's builder stage did not copy the `packaging/` directory into the build context, so `cargo build` inside the image failed with `couldn't read src/service_cmd/../../packaging/systemd/all-smi.service`. Because `docker-check` is gated to pushes on `main`, PR #319 itself showed fully green, and the failure appeared only on the merge commit, fixed separately by PR #322.
- **Issue 2 (the existing embedded-asset test covers only one direction)**: `tests/docker_build_context_test.rs`'s `embedded_assets_are_inside_the_docker_build_context` (added by PR #322) checks that every `include_str!`/`include_bytes!` target is reachable from some builder-stage `COPY`, which is the class of break PR #319 hit, but says nothing about the reverse: a `COPY` naming a path that does not exist in the context at all, which an unrelated rename or deletion could introduce.
- **Issue 3 (the cost of running the full build on PRs was unmeasured)**: the issue explicitly required a measured, not estimated, cost before deciding whether to run the build on every PR or adopt a cheaper variant.

### 1.3 Risk Assessment

| Risk | Impact | Likelihood |
|---|---|---|
| An image-breaking change merges to `main` with every PR check green, discovered only after the fact | High per occurrence (a red default branch, as already happened with PR #319/#322) | Demonstrated once already; the gate this PR closes is the direct fix |
| A path-filtered trigger, chosen without measurement, turns out to skip the exact class of change that breaks builds | High if chosen: verified directly against the last 40 commits, a path filter would have skipped 19 of them (48%), including the one commit known to have broken `main` | Avoided by rejecting the path-filter option on measured evidence rather than intuition (section 3.1) |
| `cache-to: type=gha,mode=max` from every PR evicts cache layers `main`'s own builds depend on | Medium: a shared, LRU-evicted cache (~13.3 GB across 41 entries at the time of this PR) can be churned by concurrent PR-scoped writes that are themselves unreadable from other PRs anyway | Avoided by making `cache-to` push-only (section 3.2) |
| `linux/arm64` image builds are silently broken | Low today (CI only ever builds `linux/amd64`), but would surface immediately if multi-arch images were ever requested | Identified as an incidental finding, deliberately not fixed in this PR (section 8) |

---

## 2. Technical Review

### 2.1 Correctness

The decision to remove the trigger gate rests on a specific, falsifiable measurement rather than an assumption that "Docker builds are always slow." Ten runs on `main` show `docker-check` costs 313–354 s (mean 326 s) when the build context changed, and only 12–20 s (mean 15 s) when it did not, a genuinely bimodal distribution with nothing in between. The PR's own comment block explains why: `COPY src/` invalidates its own layer on any source change, and there is no dependency-prebuild stage (no cargo-chef, no dummy-main trick), so `cargo build --release` reruns in full on any source edit; only the base image and apt layers are ever reused. This matters for the decision because it rules out "the cache will make this fast enough" as a reason to skip measuring the marginal cost against `build-check` directly.

The marginal-cost number, not the absolute cost, is what the decision turns on: `docker-check` and `build-check` are both `needs: test` and therefore start at the same moment, and both do substantially the same work (a release compile of the crate), so `docker-check` finishing a mean of 2 seconds after `build-check` (0–15 s range across the ten sampled runs) means the wall-clock cost PR authors actually experience, time until the required checks are all green, does not change. This PR's own CI run (31106052626) is a positive control for exactly this claim: `Docker Build Check` finished at 13:35:33 after 277 s, `Build Check` finished at 13:37:22 after 386 s, so the image build finished 109 seconds *before* the job that already gates every PR, with the Docker build itself coming in faster than `main`'s own 313–354 s band because PRs read `main`'s cache without writing back to it.

### 2.2 Performance

Covered above; the change is itself a performance/cost decision rather than something with a separate performance profile to review. One additional cost dimension is addressed directly: `cache-to` becomes conditional (`${{ github.event_name == 'push' && 'type=gha,mode=max' || '' }}`), so PRs stop contributing to GHA cache churn while still benefiting from `main`'s warm cache via `cache-from: type=gha`.

### 2.3 Compatibility and Dependencies

- **Breaking changes**: none to the application; this PR changes only CI trigger conditions, caching policy, and adds test coverage.
- **New dependencies**: none.
- **Compatibility**: `docker-check` now also runs on pull requests, in addition to pushes to `main`, which is a strict widening of coverage with no narrowing anywhere.

### 2.4 Code Quality

The new test, `builder_stage_copy_sources_exist_in_the_context`, is verified with a matched pair of negative controls rather than trusted on its own logic: adding `COPY nonexistent-assets/` to the builder stage fails the new test with an actionable message ("drop the COPY line, or repoint it at wherever this path moved to"), and a real `docker buildx build --target builder` on the identical Dockerfile independently fails in 2 seconds with BuildKit's own `"/nonexistent-assets": not found`, confirming the new Rust-level test agrees with what Docker itself would report, just roughly two orders of magnitude faster and with a friendlier message. The existing embedded-asset test's control (removing `COPY packaging/`, originally PR #322's own verification) was re-run and re-verified rather than assumed still valid, and now names both embedded assets in its failure output rather than one. A companion unit test, `glob_copy_sources_are_recognised`, pins the `is_glob` helper's behavior directly (`packaging/*.service`, `src/**`, `file?.txt`, `[abc].txt` recognized as patterns; `src/`, `Cargo.toml`, a literal packaging path, and `.` recognized as not patterns), which matters because the new context-existence check deliberately skips glob sources rather than attempting to resolve BuildKit's own glob semantics, a scope limitation documented as safe specifically because this repository's Dockerfile uses only literal `COPY` sources today.

---

## 3. Technical Decisions

### 3.1 Run the full image build on pull requests, rejecting a path filter on measured evidence

**Context**: the issue asked whether a full `docker build` on every PR was too expensive for the runner budget, and if so, to evaluate three cheaper variants before dismissing the idea: a path-filtered trigger, a builder-stage-only build, or a context-assembly check with no compile.

| Option | Pros | Cons |
|---|---|---|
| Path filter on `Dockerfile`/`.dockerignore`/`Cargo.toml`/`Cargo.lock`/`build.rs` | Cheap to evaluate; skips the build entirely on most commits | Verified against the last 40 commits on `main`: would have skipped 19 of 40 (48%), including `74f75d2`, the one commit on record that actually broke the build, because it added a source-level `include_str!` and touched none of the filter paths. A filter that misses the only empirical failure available, to save a cost already measured at ~2 s, is a bad trade |
| `--target builder` only | Skips the runtime stage (apt, `useradd`, `COPY --from=builder`), a few seconds against a ~5.5 min compile | Stops verifying that the built binary actually lands where the runtime stage expects to find it, a real class of break the full build catches and this variant would not |
| Context-assembly check with no compile | Cheapest possible option; a direct generalization of what `tests/docker_build_context_test.rs` already did for embedded assets | Cannot see a missing system dependency in the builder stage or anything that only appears once `cargo` actually runs inside the image; adopted as a complement (this PR extends exactly this check), not as a replacement |
| **Chosen: run the full build on every PR, no gate** | At a measured ~2 s marginal cost (section 2.1), every cheaper variant trades away real coverage for a saving that rounds to nothing; catches the actual failure class that motivated the issue | None identified at the measured cost; the PR frames every alternative as strictly worse once the marginal-cost number is known |

**Rationale**: the issue's own acceptance criteria required the cost to be measured, not estimated, before a decision; once measured, the "cheaper" variants no longer trade off against a real cost, only against real coverage, which settles the decision. The path-filter option is the sharpest rejection because it is verified against exactly the failure this issue exists to prevent, not against a hypothetical one.

### 3.2 Make `cache-to` push-only rather than exporting from every PR

**Context**: `cache-to: type=gha,mode=max` was already unconditional before this PR, exporting the full layer cache on every run that reached the step, including, after this PR removes the trigger gate, every pull request.

**Decision**: `cache-to: ${{ github.event_name == 'push' && 'type=gha,mode=max' || '' }}`; PRs read `main`'s cache via `cache-from` but write nothing back.

**Rationale**: the GHA cache is a shared, LRU-evicted budget (~13.3 GB across 41 entries at the time of this PR), and `mode=max` exports from every concurrent PR would churn that budget and could evict the warm layers `main`'s own builds depend on, for no benefit, since a PR-scoped cache write is unreadable from any other PR anyway. This is a direct consequence of removing the trigger gate: the caching policy had to be revisited in the same change, not left as an unconsidered side effect of running the job more often.

### 3.3 Extend `tests/docker_build_context_test.rs` with the reverse-direction check, keep both it and the full image build rather than treating either as redundant

**Context**: with the full image build now running on every PR, one might argue the fast Rust-level context tests become unnecessary, since the slow build would catch the same classes of failure anyway.

**Decision**: extend the test suite with `builder_stage_copy_sources_exist_in_the_context` (every builder-stage `COPY` source must exist in the context) and keep both the tests and the image build, rather than treating the tests as superseded.

**Rationale**: the two mechanisms cover genuinely different classes, stated precisely in the PR: the tests cover embedded-asset reachability and `COPY`-source existence, both of which are structural properties of the repository checkable without ever invoking `cargo` or `docker`; the image build covers what neither test can, a missing system dependency in the builder stage, or anything that only surfaces once `cargo` actually compiles inside the image environment. The tests are not made redundant by the image build running on PRs: they run in the `test` job, `docker-check` is `needs: test`, and they finish in milliseconds, so for the classes they do cover, the 5–6 minute image build never even starts, and the failure names the offending path and the fix directly rather than surfacing as a raw BuildKit error partway through a multi-minute compile.

---

## 4. Implementation Details

### 4.1 Architecture Changes

```
[Before]
docker-check:
  needs: test
  if: github.event_name == 'push' && github.ref == 'refs/heads/main'
  cache-to: type=gha,mode=max   (unconditional)
  -> only ever runs after a merge; a break surfaces as a red default branch

tests/docker_build_context_test.rs:
  embedded_assets_are_inside_the_docker_build_context   (one direction only)

[After]
docker-check:
  needs: test
  (no `if:` gate; runs on pull_request and push alike)
  cache-to: ${{ push && 'type=gha,mode=max' || '' }}   (push-only)
  -> runs alongside build-check on every PR, finishing ~2s after it (measured)

tests/docker_build_context_test.rs:
  embedded_assets_are_inside_the_docker_build_context        (unchanged direction)
  builder_stage_copy_sources_exist_in_the_context   (new: the reverse direction)
  glob_copy_sources_are_recognised                  (new: pins is_glob's scope)
```

### 4.2 Key Code Changes

**File: `.github/workflows/ci.yml` (the gate removed, the decision recorded)**
```yaml
  # Decision: run the full build on pull requests rather than one of
  # the cheaper variants, because the full build turns out to be
  # nearly free in wall-clock terms. Measured over 10 runs on main:
  #   - docker-check takes 313-354s (mean 326s) when the build context
  #     changed, and 12-20s when it did not...
  #   - docker-check therefore finishes 0-15s after build-check
  #     (mean 2s). It is not on the critical path...
  docker-check:
    name: Docker Build Check
    runs-on: ubuntu-latest
    needs: test
    steps:
      ...
      - uses: docker/build-push-action@...
        with:
          ...
          cache-from: type=gha
          cache-to: ${{ github.event_name == 'push' && 'type=gha,mode=max' || '' }}
```
**Reason for change**: the `if:` gate that prevented this job from ever running on a pull request is removed; `cache-to` becomes conditional in the same change so removing the gate does not also silently turn every PR into a cache-writer.

**File: `tests/docker_build_context_test.rs` (the reverse-direction check)**
```rust
#[test]
fn builder_stage_copy_sources_exist_in_the_context() {
    ...
    for source in &copies {
        if is_glob(source) { continue; }
        ...
        if !root.join(relative).exists() {
            failures.push(format!(
                "  COPY {source}\n    No such path in the build context.\n    \
                 To fix: drop the COPY line, or repoint it at wherever this path moved to."
            ));
            continue;
        }
        if let Some(pattern) = dockerignore_hit(&ignore_patterns, relative) {
            failures.push(format!(
                "  COPY {source}\n    The path exists, but .dockerignore pattern `{pattern}` \
                 strips it back out of the context.\n    \
                 To fix: narrow that pattern or add a `!` negation for this path."
            ));
        }
    }
    ...
}
```
**Reason for change**: this is the direction `embedded_assets_are_inside_the_docker_build_context` did not cover, catching a `COPY` left pointing at a path an unrelated change renamed or deleted, before it reaches a 5–6 minute image build or, worse, a merge to `main`.

**File: `tests/docker_build_context_test.rs` (module doc, corrected to match the new trigger)**
```rust
//! Since #328 that gate is gone and `docker-check` builds the image on
//! pull requests too, so a real `docker build` now backs every PR. These
//! tests are still the first line of defence rather than a leftover:
//! they run in the `test` job, `docker-check` is `needs: test`, and they
//! finish in milliseconds.
```
**Reason for change**: the previous module doc explicitly stated `docker-check` "only runs on pushes to main," which this PR makes false; leaving stale documentation describing a gate that no longer exists would misinform the next reader about why the tests still matter.

### 4.3 Data Model Changes

Not applicable. No source code, wire format, or metric definition changed; this PR is CI trigger/caching policy plus test coverage.

---

## 5. Learning Points

### 5.1 A cost-based CI decision should be settled by the marginal number, not the absolute one

**Concept**: when a check already runs in parallel with another check that gates the same event (here, both `docker-check` and `build-check` start together under `needs: test`), the number that determines whether adding the check changes anyone's experienced wait time is how much later the new check finishes relative to the existing one, not the new check's standalone duration.

**Application in this PR**: `docker-check`'s absolute cost (313–354 s cold) looks expensive in isolation; its marginal cost (0–15 s after `build-check`, which was already required) is what actually determined the decision, and this PR measured that number directly rather than reasoning from the absolute one.

### 5.2 A path filter is only as good as its ability to include the specific failure it exists to prevent

**Concept**: a path-based trigger filter is an intuitively appealing optimization, but its correctness has to be checked against the actual historical failure it is meant to guard against, not against an assumption about which files "usually" matter for a given build.

**Application in this PR**: the rejected path filter (`Dockerfile`/`.dockerignore`/`Cargo.*`/`build.rs`) is a reasonable-looking first guess at "files that affect the Docker build," but the one recorded failure, `74f75d2`, broke the build by adding a source file reachable via `include_str!`, a change no plausible Docker-specific path filter would have included. Verifying the filter against the actual incident, rather than trusting the intuition that produced the filter, is what surfaced this.

### 5.3 Removing a gate can have a caching-policy consequence that has to be revisited explicitly, not left as an accidental side effect

**Concept**: a caching configuration written under one trigger condition (only `main` pushes) can carry an implicit assumption (only one writer at a time, low contention) that a later change to the trigger condition (also PRs) silently invalidates.

**Application in this PR**: `cache-to: type=gha,mode=max` was safe when only `main` pushes could reach it; once PRs can reach the same step, the same configuration would let every concurrent PR export a full cache write against a shared, size-limited budget. This PR treats that as a decision to make deliberately (push-only `cache-to`), not as an incidental consequence to discover later.

---

## 6. Further Learning

### Key Terms

| Keyword | Description | Relevance |
|---|---|---|
| `needs: test` | GitHub Actions job dependency, making two jobs start at the same point once their shared prerequisite finishes | Why `docker-check` and `build-check` start together, making their *relative* finish time the meaningful cost metric |
| `cache-from` / `cache-to` (GHA cache backend) | Docker Buildx's GitHub Actions cache import/export directives | The mechanism this PR makes asymmetric (read always, write only on push) to avoid PR-triggered cache churn |
| `include_str!` | Rust macro embedding a file's contents into the binary at compile time | The mechanism behind the original `74f75d2` failure: an embedded asset the Docker build context did not contain |
| BuildKit context resolution | How `docker build` resolves `COPY` sources against the assembled build context, including `.dockerignore` | What the new `builder_stage_copy_sources_exist_in_the_context` test reproduces at the Rust-test level, without invoking Docker |
| Bimodal build cost | A cost distribution with two clusters and no values between them (here, 12–20 s vs. 313–354 s) | The empirical signature that GHA layer caching does not help the expensive path (`COPY src/` invalidates its own layer on any source change) |

### Related Technologies and Frameworks

- Docker BuildKit and its `--cache-from`/`--cache-to` GitHub Actions cache backend, including `mode=max`'s full-layer export behavior.
- GitHub Actions job dependency graphs (`needs:`) and their effect on which jobs start simultaneously versus sequentially.
- Rust's vendored-OpenSSL build path and its dependency on perl's `FindBin` module, the mechanism behind the incidental `linux/arm64` finding (section 8).

### Related PRs and Issues

- Issue #328: the issue this PR closes.
- PR #319 (issue #309): the PR whose merge commit (`74f75d2`) actually broke `main`'s Docker image build, the incident that motivated this issue.
- PR #322: fixed the immediate `74f75d2` break and added `tests/docker_build_context_test.rs`'s original embedded-asset check, which this PR extends with the reverse-direction check.
- PR #337: lands after this PR in the same merge sequence; unrelated in content, but its `ci.yml` edits are on the launchd job, not `docker-check`, so the two PRs' diffs do not collide.

---

## 7. Change Summary

### Statistics

| Item | Value |
|---|---|
| Files changed | 2 |
| Lines added | +169 |
| Lines removed | -11 |
| Commits | 1 |

### Changes by Category

| Category | Summary |
|---|---|
| CI coverage | `docker-check`'s `if:` gate removed; the job now runs on pull requests as well as pushes to `main` |
| CI cost policy | `cache-to` made conditional (push-only), so pull requests read `main`'s cache without exporting their own |
| Tests | New `builder_stage_copy_sources_exist_in_the_context` (the reverse-direction context check) and `glob_copy_sources_are_recognised` (pins the glob-detection helper's scope) |
| Documentation | `.github/workflows/ci.yml` comment records the measured costs and every rejected alternative; `tests/docker_build_context_test.rs` module doc and failure messages corrected to describe the new trigger |

### Related Commits

| SHA | Type | Message |
|---|---|---|
| `d84a213d` | ci | build the Docker image on pull requests |

Merged to `main` as `89c621f7`. Closes #328.

---

## 8. Follow-up Actions

### Required

None identified as blocking.

### Monitoring Required

- What stays uncovered even after this PR: a moved or retagged base image breaks on its own schedule rather than per-PR, so it would surface on whichever build happens to run next, not necessarily on an open PR. The PR notes a scheduled build would be the right tool if this ever becomes a problem, but does not implement one.

### Future Improvements

- **`linux/arm64` build is broken independently of this issue**, recorded in an issue comment on #328 rather than fixed here: `Cargo.toml` enables vendored OpenSSL for `cfg(all(target_arch = "aarch64", target_env = "gnu"))`, and building OpenSSL from source needs perl's `FindBin`, which `rust:1.96-slim` does not carry (it ships `perl-base`, not full `perl`). CI only ever builds `linux/amd64` today, where this dependency path is inactive, so nothing is broken in practice; the issue comment states this is "worth its own issue if multi-arch images are ever wanted." This report could not find this finding stated anywhere in the PR body or diff itself; it is recorded only in the linked issue's comment thread, which this report cross-checked directly.

---

## Appendix

### A. Test Results

Negative controls, each paired with a real `docker build` on the same input:

- **New check**: adding `COPY nonexistent-assets/` to the builder stage fails `builder_stage_copy_sources_exist_in_the_context` with an actionable message. A real `docker buildx build --target builder` on the identical Dockerfile independently fails in 2 s with `failed to compute cache key: ... "/nonexistent-assets": not found`.
- **Existing check, re-verified**: removing `COPY packaging/` fails `embedded_assets_are_inside_the_docker_build_context`, now naming both embedded assets. A probe build over the same `COPY` set confirmed the asset is genuinely absent from the assembled context, while the unmodified set has it.
- `cargo test --test docker_build_context_test`: 7 passed.
- `cargo check --lib --tests`: clean.
- `cargo clippy --lib --tests -- -D warnings` and `cargo clippy --bin all-smi -- -D warnings`: both clean.
- `cargo fmt --check`: clean.
- `actionlint .github/workflows/ci.yml`: no findings beyond the 5 already present on `main` (3x SC2015, 1x SC2251, 1x unknown self-hosted runner label), none inside `docker-check`.
- This PR's own CI run (31106052626) is the positive control for the trigger change itself: `docker-check` appeared on the pull request and built the image; `Docker Build Check` finished at 13:35:33 after 277 s, `Build Check` finished at 13:37:22 after 386 s, a 109-second gap in `docker-check`'s favor.

### B. Performance Benchmarks

The core quantitative result of this PR, from `gh api repos/lablup/all-smi/actions/runs/<id>/jobs` across 10 runs on `main`:

| | duration |
|---|---|
| `docker-check`, build context changed | 313–354 s (mean 326 s), n=7 |
| `docker-check`, context unchanged (docs/workflow-only commits) | 12–20 s (mean 15 s), n=3 |
| `build-check`, already runs on every PR | 319–378 s (mean 339 s) |
| `docker-check` finishing after `build-check` | 0–15 s, mean 2 s |

### C. References

- Issue #328: root cause narrative (the `74f75d2` incident), scope, and acceptance criteria this report draws from, cross-checked against the diff.
- Issue #328's comment thread: the full measured-cost table, the per-alternative rejection reasoning, and the incidental `linux/arm64`/`FindBin` finding, none of which appear in the PR body or diff itself.
- PR #322: the prior fix for `74f75d2` and the original `tests/docker_build_context_test.rs` this PR extends.
