# Technical Report: PR #322 - fix(ci): include packaging assets in the Docker build context

**Date**: 2026-08-05
**Status**: Completed
**Languages**: Dockerfile, Rust (test harness)
**Risk Level**: Low (CI-only fix plus a new contract test; no application code path changes)

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

`main` broke immediately after PR #319 (issue #309) merged: the Docker image build failed to compile with `error: couldn't read src/service_cmd/../../packaging/systemd/all-smi.service`. PR #319 introduced the first file in this repository ever embedded with `include_str!`, and nothing enforced that the set of paths reachable from `include_str!`/`include_bytes!` under `src/` agreed with the set of paths the Dockerfile's builder stage actually copies into its build context. The Dockerfile copies `Cargo.toml`, `Cargo.lock`, `build.rs`, `proto/`, and `src/`, narrower than a full checkout, and had no `packaging/` entry, so the crate could not compile inside the image even though every other CI gate (`cargo test`, `cargo clippy`, `cargo build`) had passed on the same PR.

The reason this was invisible before merge, not merely unlucky, is structural: `docker-check` in `.github/workflows/ci.yml` is gated on `github.event_name == 'push' && github.ref == 'refs/heads/main'`, so it never runs on a pull request at all. Every other check builds from a full checkout and cannot see a narrowed-build-context problem by construction. The fix is two parts: add `COPY packaging/ ./packaging/` to the Dockerfile's builder stage, and add `tests/docker_build_context_test.rs`, a contract test that extracts every `include_str!`/`include_bytes!` literal under `src/`, resolves it against the file embedding it, and asserts the result is both covered by a builder-stage `COPY` and not stripped back out by `.dockerignore`. This moves the failure class onto the pull request, where a missing `COPY` line costs a few milliseconds to detect rather than a red default branch. The test's own correctness was checked with two independent negative controls: reverting the Dockerfile's `COPY` line fails the new test with a message naming the exact fix, and a from-scratch Docker build using this Dockerfile's exact `COPY` set plus a targeted `RUN test -f` assertion fails without the fix and succeeds with it. Total: 2 files, +340/-0, one commit, no linked issue.

---

## 1. Problem Statement

### 1.1 Background

The Dockerfile builds `all-smi` in two stages: a builder stage that copies a deliberately narrowed set of source paths (not a full checkout, to keep the build context and cache small) and compiles the release binary, and a runtime stage that copies only the built binary out of the builder via `COPY --from=builder`. This narrowing has always been implicit: nothing in the repository declared or checked which source paths the crate's compiled output actually depends on beyond what `cargo build` needs to see under `src/`, `proto/`, and the two manifest files.

PR #319 (issue #309) added `packaging/systemd/all-smi.service`, embedded via `include_str!("../../packaging/systemd/all-smi.service")` in `src/service_cmd/template.rs`, the first asset this repository has ever embedded into the compiled binary from outside `src/`. The Dockerfile's builder stage had no `COPY` line for `packaging/`, so the embedded path was unreachable inside the build context, and `rustc` failed at the `include_str!` macro expansion with a plain file-not-found error.

### 1.2 Existing Issues

- **Issue 1 (the actual break)**: `docker build` failed inside the builder stage with `error: couldn't read src/service_cmd/../../packaging/systemd/all-smi.service` immediately after PR #319 merged, confirmed at [run 30997457472](https://github.com/lablup/all-smi/actions/runs/30997457472).
- **Issue 2 (why nothing caught it before merge)**: `docker-check` in `ci.yml` is gated on `github.event_name == 'push' && github.ref == 'refs/heads/main'`, so it is structurally incapable of running on a pull request. PR #319 was fully green on every check that did run, since `cargo test`, `cargo clippy`, and `cargo build` all build from a full checkout and have no concept of a narrowed Docker build context.
- **Issue 3 (no enforced contract between two independently-maintained lists)**: the set of paths reachable from `include_str!`/`include_bytes!` in `src/` and the set of paths the Dockerfile's builder stage `COPY`s are two facts about the repository that have to agree, but nothing checked that they did, and nothing would have caught a future embedded asset falling into the same gap either.

### 1.3 Risk Assessment

| Risk | Impact | Likelihood |
|---|---|---|
| A future PR embeds a new asset under `src/` from outside the Dockerfile's `COPY` set, repeating this exact break | Medium: breaks `main` again, discovered only after merge, exactly as this incident was | Closed by the new contract test running in the normal `cargo test` suite on every PR |
| `.dockerignore` strips an asset back out even though a `COPY` line nominally covers it | Low (not observed in this repository today) | The new test's `dockerignore_hit` check covers this case explicitly, even though it found nothing wrong here |
| `docker-check` continuing to not run on pull requests, for defect classes this new test cannot see (e.g., a missing system dependency in the builder stage) | Medium: this fix narrows the blind spot to embedded-asset reachability specifically; other build-breaking changes could still only surface on `main` | Explicitly out of scope for this PR; recorded as a separate, filed-elsewhere CI cost decision |

---

## 2. Technical Review

### 2.1 Correctness of the fix itself

The Dockerfile change is a single added line, `COPY packaging/ ./packaging/`, placed in the builder stage alongside the existing `COPY src/ ./src/`. Because it copies the whole `packaging/` directory rather than the single file PR #319 introduced, it also covers the launchd plist PR #321 added under the same directory tree without requiring a second Dockerfile change for that PR.

### 2.2 The contract test's own correctness, established by negative controls rather than by inspection alone

A test that only ever passes proves nothing about whether it would catch the defect it claims to guard against. Two independent controls were run specifically to rule that out:

- **Reverting the `COPY packaging/` line** and rerunning `cargo test --test docker_build_context_test` fails `embedded_assets_are_inside_the_docker_build_context` with a message naming the exact missing asset, the file that embeds it, and the exact `COPY` line to add. This confirms the test would have caught the original incident, not merely that it passes now that the fix is in place.
- **A from-scratch Docker build** using this Dockerfile's exact builder-stage `COPY` set, plus a targeted `RUN test -f packaging/systemd/all-smi.service` assertion inside that same build, fails with `exit code: 1` on that `RUN` step without the fix, and succeeds with it, with both the unit template and the environment-file example confirmed present in the resulting image. This is the test's own claim (that the named `COPY` line makes the asset reachable) checked against the actual tool (`docker build`) rather than only against the Rust-level static-analysis logic the test performs.

### 2.3 Compatibility and Dependencies

- **Breaking changes**: none. The Dockerfile change only adds a `COPY` line to the builder stage; the runtime stage and the resulting image's contents (beyond making the build succeed at all) are unaffected.
- **New dependencies**: none. The test uses only `std::fs`, `std::path`, and `std::collections::BTreeSet` from the standard library.
- **Compatibility**: the new test lives under `tests/`, runs as part of the normal `cargo test` suite, and requires no Docker daemon, network access, or special CI configuration; it operates entirely on the checked-out source tree and the Dockerfile/`.dockerignore` text.

### 2.4 Code Quality

Five tests in `tests/docker_build_context_test.rs`: the primary contract check (`embedded_assets_are_inside_the_docker_build_context`), plus four supporting unit tests for the parsing helpers it depends on (`copy_coverage_matches_directories_and_exact_files`, `dockerignore_patterns_are_recognised`, `embedded_path_extraction_finds_literals`, `builder_stage_copies_are_isolated_from_the_runtime_stage`), the last of which specifically confirms that a `COPY --from=builder ...` line in a later stage is correctly excluded from the set of context-copied sources, since it pulls from a previous stage's output rather than from the build context.

`cargo fmt --check` and `cargo clippy --all-targets --all-features -- -D warnings` both reported clean on the new file.

---

## 3. Technical Decisions

### 3.1 A static-analysis contract test over `include_str!`/`include_bytes!` literals, rather than only fixing the Dockerfile

**Context**: the immediate fix (adding one `COPY` line) resolves the specific incident but does nothing to prevent the same class of break the next time an asset is embedded from a path outside the current `COPY` set.

| Option | Pros | Cons |
|---|---|---|
| Fix only the Dockerfile | Minimal change, resolves the reported break immediately | Leaves the underlying gap (two independently-maintained lists that must agree) completely unenforced; the exact same incident recurs on the next new embedded asset |
| **Chosen: fix the Dockerfile, and add a contract test that extracts every embedded-asset literal and checks it against the Dockerfile's actual `COPY` set** | Moves the failure class onto the pull request (a few milliseconds in `cargo test`) instead of onto `main` after merge; self-updating, since it reads the Dockerfile and source tree directly rather than encoding a fixed list of assets to check | The parser for `include_str!`/`include_bytes!` literals is deliberately simple (plain string literals only) and would not catch a `concat!`-built path, a documented, accepted limitation rather than a silent gap |
| Enable `docker-check` on pull requests instead | Would also have caught this specific incident, and catches other classes of Docker build breakage this test cannot (e.g. a missing system package) | A CI cost decision (build time, resource usage on every PR) distinct from this specific defect class; recorded as out of scope and filed separately rather than folded into this PR |

**Rationale**: a test that reads the actual Dockerfile and the actual source tree, rather than hardcoding today's one known asset, is what makes the guarantee last past this specific incident. Enabling full Docker builds on every pull request would be a strictly stronger guarantee but is a materially different, more expensive change that this PR deliberately does not make on the reporter's own initiative; it is called out explicitly as a follow-up decision for maintainers rather than assumed to be equivalent to this fix.

### 3.2 Resolve `include_str!` paths lexically, not via `canonicalize`

**Context**: the test needs to resolve a relative path literal (e.g., `"../../packaging/systemd/all-smi.service"`) against the source file that contains it, to get a repository-relative path to check against the Dockerfile's `COPY` set.

**Chosen approach**: a small lexical `normalize()` function that manually applies `..`/`.` path components against the accumulated path, rather than calling `Path::canonicalize`.

**Rationale**: `canonicalize` requires the target to exist on disk and would therefore be unusable in exactly the failure case this test is designed to report clearly, an asset that is referenced but not actually reachable; a lexical resolution works whether or not the target exists, which is what lets the test's own failure message name the specific missing or unreachable path rather than failing with an unrelated I/O error before it can even describe the problem.

---

## 4. Implementation Details

### 4.1 Architecture Changes

```
[Before]
Dockerfile builder stage COPY set: Cargo.toml, Cargo.lock, build.rs, proto/, src/
src/service_cmd/template.rs:  include_str!("../../packaging/systemd/all-smi.service")
                                                    |
                                                    v
                              unreachable inside the Docker build context
                              -> `docker build` fails at compile time on `main`

[After]
Dockerfile builder stage COPY set: Cargo.toml, Cargo.lock, build.rs, proto/, src/, packaging/
                                                    |
                                                    v
                              tests/docker_build_context_test.rs asserts, on every `cargo test`:
                                for each include_str!/include_bytes! literal under src/:
                                  resolved path exists on disk
                                  resolved path is covered by a builder-stage COPY
                                  resolved path is not stripped back out by .dockerignore
```

### 4.2 Key Code Changes

**File: `Dockerfile`**
```dockerfile
# Copy packaging assets embedded into the binary with include_str!
# (service unit templates). Keep this in sync with the paths asserted by
# tests/docker_build_context_test.rs.
COPY packaging/ ./packaging/
```
**Reason for change**: this is the fix for the reported break. The comment points at the new test specifically so a future editor of either file knows the other one exists and has to stay in agreement.

**File: `tests/docker_build_context_test.rs` (the contract check)**
```rust
if !copies.iter().any(|c| copy_covers(c, asset_path)) {
    failures.push(format!(
        "  {asset}\n    embedded by: {owner}\n    To fix: add `COPY {dir}/ ./{dir}/` to the builder stage of the Dockerfile."
    ));
}
```
**Reason for change**: the failure message names the exact fix (the specific `COPY` line to add), not just that a problem exists, which is what makes the test actionable from its own output rather than requiring the reader to re-derive the fix PR #322 itself had to work out from a build log.

### 4.3 Data Model Changes

Not applicable; this PR changes CI/build infrastructure only, not any wire format or Prometheus metric.

---

## 5. Learning Points

### 5.1 A build context narrower than a full checkout is an implicit, unchecked contract with the source tree

**Concept**: any Docker build that copies a subset of the repository into its build context (rather than the whole tree) is making an implicit claim: "the compiled output depends on nothing outside this subset." That claim is only as good as the last time someone verified it by hand, unless something checks it automatically as the source tree evolves.

**Application in this PR**: `include_str!`/`include_bytes!` are exactly the mechanism that can silently invalidate that claim, since they add a compile-time dependency on a file path that `cargo build`'s normal `src/`-plus-manifest view does not otherwise expect. The fix generalizes past the one known asset by deriving the check from the actual macro invocations in the source tree rather than from a maintained list.

### 5.2 A CI gate scoped to `push` on the default branch cannot catch a pull-request-introduced regression before merge, by construction

**Concept**: `if: github.event_name == 'push' && github.ref == 'refs/heads/main'` is a common pattern for expensive checks (here, a full Docker image build) that a team does not want running on every PR. The trade-off is structural, not incidental: any defect class only that gate can catch is, by the same condition, a defect class that can only ever be discovered after merge.

**Application in this PR**: this is precisely what happened. Every gate that did run on PR #319 (test suite, clippy, `cargo build`) had no way to see the Docker-build-context problem, because none of them build from a narrowed context. The fix's own contract test deliberately does not require Docker at all, specifically so it can run inside the normal `cargo test` suite that does execute on every pull request, moving this specific defect class out from behind the `push`-only gate without changing that gate's scope.

### 5.3 A test's own value depends on evidence that it can fail, not only on evidence that it currently passes

**Concept**: a newly-written check that only ever runs against an already-fixed codebase demonstrates that it accepts correct input, but says nothing about whether it would have rejected the broken input it was written in response to.

**Application in this PR**: both negative controls (reverting the Dockerfile `COPY` line; a from-scratch Docker build with and without that line) exist specifically to close that gap, confirming the test both fails informatively on the exact regression it targets and that its underlying claim (an added `COPY` line makes an asset reachable) holds against the real tool, not only against the test's own path-resolution logic.

---

## 6. Further Learning

### Key Terms

| Keyword | Description | Relevance |
|---|---|---|
| Docker build context | The set of files sent to the Docker daemon for a build, determined by what a `COPY`/`ADD` instruction can reach and what `.dockerignore` excludes | The thing this PR's test checks embedded-asset reachability against |
| `include_str!` / `include_bytes!` | Rust macros embedding a file's contents into the compiled binary at compile time, resolved relative to the source file containing the macro | The mechanism that introduced a Docker-build-context dependency PR #319 did not know it was creating |
| Multi-stage Docker build | A Dockerfile with more than one `FROM`, where later stages can selectively `COPY --from=` an earlier stage's output | Why the test explicitly excludes `COPY --from=` lines from the set of context-copied sources (section 2.4) |
| `.dockerignore` | A file listing patterns excluded from the Docker build context regardless of what a `COPY` instruction names | The second half of the test's coverage check, alongside `COPY` presence |
| Negative control (in a test-verification sense) | Deliberately reintroducing a known defect to confirm a test detects it, rather than only confirming the test passes on correct code | The methodology used to validate this PR's own new test (section 2.2) |

### Related Technologies and Frameworks

- Docker multi-stage builds and the `COPY`/`ADD`/`.dockerignore` build-context model.
- Rust's `include_str!`/`include_bytes!` compile-time file embedding.
- GitHub Actions `if:` conditions scoping a job to specific event types and refs.

### Related PRs and Issues

- PR #319 (issue #309): introduced `packaging/systemd/all-smi.service` and its `include_str!` embedding, the change that broke `main`'s Docker build.
- PR #321 (issue #310): added `packaging/launchd/com.lablup.all-smi.plist` under the same `packaging/` tree this PR's `COPY packaging/` line already covers, requiring no further Dockerfile change.
- No linked GitHub issue; this PR was filed directly against the `main` breakage.

---

## 7. Change Summary

### Statistics

| Item | Value |
|---|---|
| Files changed | 2 (`Dockerfile`, `tests/docker_build_context_test.rs`) |
| Lines added | +340 |
| Lines removed | 0 |
| Commits | 1 |
| New tests | 5 |

### Changes by Category

| Category | Summary |
|---|---|
| CI / Build fix | `COPY packaging/ ./packaging/` added to the Dockerfile's builder stage |
| Regression prevention | New `tests/docker_build_context_test.rs` contract test, running in the normal `cargo test` suite on every pull request |

### Related Commits

| SHA | Type | Message |
|---|---|---|
| `9989a860` | fix(ci) | include packaging assets in the Docker build context |

Merged to `main` as `acb3c946`. No linked issue.

---

## 8. Follow-up Actions

### Required

None. The immediate break is fixed and verified against both the Rust-level test and a real `docker build`.

### Monitoring Required

- Whether a future embedded asset (via `include_str!`/`include_bytes!`) is added under `src/` in a location the test's simple string-literal parser cannot see (for example, a path built with `concat!`); this is a documented, accepted limitation of the parser rather than a silent one.

### Future Improvements

- **Running `docker-check` on pull requests.** Explicitly out of scope for this PR and filed separately as a CI-cost decision; would catch a broader class of Docker build breakage (e.g., a missing system dependency in the builder stage) that this PR's static-analysis test cannot see by design.

---

## Appendix

### A. Test Results

- `cargo test --test docker_build_context_test`: 5 passed.
- Negative control 1 (Dockerfile): removing the `COPY packaging/` line makes `embedded_assets_are_inside_the_docker_build_context` fail with the exact fix message described in section 2.4.
- Negative control 2 (real Docker build): a Dockerfile built from this PR's exact builder-stage `COPY` set plus `RUN test -f packaging/systemd/all-smi.service` succeeds with the fix and fails with `exit code: 1` on that `RUN` line without it; the unit template and the environment-file example are both confirmed present in the resulting image.
- `cargo fmt --check`: clean.
- `cargo clippy --all-targets --all-features -- -D warnings`: clean.

### B. Performance Benchmarks

Not applicable; this PR is a build-configuration and static-analysis-test change with no runtime data path affected.

### C. References

- Docker documentation: multi-stage builds, `COPY --from=`, and `.dockerignore` semantics.
- Rust reference: `include_str!` and `include_bytes!` macro path resolution (relative to the invoking source file).
- Failing run: [github.com/lablup/all-smi/actions/runs/30997457472](https://github.com/lablup/all-smi/actions/runs/30997457472).
