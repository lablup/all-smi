# Technical Report: PR #344 - Remove the Docker Image and Its CI Job

**Date**: 2026-08-06  
**Status**: Completed  
**Related**: PR #344, Issue #342  
**Risk Level**: Medium (removes a documented deployment path; no runtime code changed)

---

## Executive Summary

PR #344 deletes the Docker image and everything that existed to build it: the `Dockerfile`, the compose example, the `docker-check` CI job, the `docker-build-container` Makefile target, and the build-context test added a day earlier. Container deployment is no longer a supported way to run all-smi.

The image had never worked on any architecture and nothing in the repository or its release process consumed it. The maintainer's decision was to remove rather than repair, so the PR does not attempt to fix the glibc mismatch or the missing runtime libraries recorded in #342. Container-awareness code inside `src/`, which lets all-smi detect that it is running inside a container, is a separate product feature and is untouched.

---

## 1. Problem Statement

Issue #342 recorded that the image failed to run on every architecture. The repair cost was real (a glibc mismatch plus missing runtime libraries), and the return on paying it was not: no workflow published the image, no release artifact referenced it, and no documented install path went through it.

The consumption sweep turned up three findings that all point the same way:

- **The compose example configured a knob that does not exist.** `examples/docker-compose.yml` set `HOST_PROC_PATH: /host/proc`, and the binary never reads that variable. It appears nowhere else in the tree; `src/device/container_utils.rs` locates the host procfs by probing a hardcoded list (`/host/proc`, `/hostproc`, `/proc_host`) instead. An example configuring a nonexistent setting is hard to square with anyone having run it.
- **No `.dockerignore` was ever tracked.** `.gitignore` carries a blanket `.*` rule, so any local copy was silently never committed, and CI checks out from git. Every `docker-check` run therefore built with no exclusions at all, sending `target/` and `.git/` into the build context.
- **The `docker-build-container` Makefile target was dead, not merely undocumented.** It was the only `docker build` of this repository anywhere in the tree, added in `f3745e2` (#31) and never referenced since: absent from both `.PHONY` and `make help`, invoked by no script, workflow, or document. `DEVELOPERS.md` documented the raw `docker build` command rather than the target, so even the documentation did not know it existed.

## 2. Change Summary

| Item | Value |
|------|-------|
| Files changed | 7 |
| Lines added | 6 |
| Lines deleted | 642 |
| Runtime code changed | No |
| CI jobs removed | 1 |

### Files

| File | Change |
|------|--------|
| `Dockerfile` | Deleted (61 lines). |
| `examples/docker-compose.yml` | Deleted (58 lines). |
| `tests/docker_build_context_test.rs` | Deleted (437 lines). |
| `.github/workflows/ci.yml` | `docker-check` job removed together with its 57-line cost-analysis comment header. |
| `Makefile` | `docker-build-container` target removed. |
| `DEVELOPERS.md` | Removed the `docker build` instruction, the `Docker Check` CI bullet, and the `Docker images` release bullet. |
| `README.md` | Added a short note at the end of the Installation section stating that containers are not a supported path. |

## 3. Technical Decisions

### 3.1 Deleting the build-context test was deliberate

`tests/docker_build_context_test.rs` is not collateral damage. PR #322 added it a day earlier specifically to guard the Docker build context after #319 broke `main`. With no `Dockerfile` it has no subject, and it would fail outright rather than pass vacuously: two of its seven tests call `.expect("Dockerfile must exist")`. It had to go in the same commit as the `Dockerfile`, and it is called out here so the loss of that guard is visible rather than quietly absorbed.

### 3.2 Two documentation defects of different kinds

Only one of them was caused by this PR, and the distinction is worth keeping:

- `DEVELOPERS.md` described a `Docker Check` job that this PR deletes. **Invalidated by this change.**
- `DEVELOPERS.md` listed `Docker images` among the things a release publishes. **This was already false before this change**, and had been for as long as the line existed: no workflow ever published an image. There is no `docker/login-action`, no `docker push`, and no `ghcr.io` reference anywhere under `.github/`. Removing it corrects a claim that was never true rather than one this PR made untrue.

Flagged but deliberately not fixed: the same `DEVELOPERS.md` CI section names three jobs when `ci.yml` actually has seven. That staleness predates this PR, and fixing it is scope creep, so it is left for a docs pass.

### 3.3 What the README note is for

The Installation section lists six install options and previously said nothing about containers, so a reader expecting a seventh was left to infer its absence. The note names the supported alternatives explicitly: the release binaries, the Homebrew tap, the Debian package and Ubuntu PPA, `cargo install all-smi`, and `all-smi service` for running API mode supervised under systemd, launchd, or the Windows SCM. All of those work today and are exercised by CI, which the image never was.

### 3.4 Explicitly untouched

- **Container-awareness code under `src/`**: `/.dockerenv` probing, cgroup parsing, `ContainerRuntime::Docker`, and Docker-aware disk filtering. all-smi detecting that it is running *inside* a container is a product feature with nothing to do with shipping an image.
- **The container test harness**: the shell scripts under `tests/` and the three `docker-dev` Makefile targets, which run all-smi inside stock `rust:1.88` containers to exercise that feature. None of them built this repository's image. `DEVELOPERS.md` keeps their documentation, with a note clarifying what they do.
- **History**: the `README.md` and `debian/changelog` changelog lines and the `TECHNICAL_REPORTS/` entries for #322 and #339 record what happened and remain true.

## 4. Validation Results

- `actionlint .github/workflows/ci.yml` reports the same 5 pre-existing findings as before the change, with no new ones. The baseline was captured before editing.
- A PyYAML parse of `ci.yml` succeeds. Jobs are now `test`, `packaging-sync`, `systemd-service`, `launchd-service`, `windows-service`, and `build-check`; `docker-check` is absent and every remaining `needs:` target resolves to an existing job.
- No job referenced `docker-check` even before removal, so the graph needed no repair. The string appeared only inside its own comment header and as the job key.
- `main` is not branch-protected, so no required status check names `Docker Build Check`, and removing the job cannot wedge merges.
- All 19 integration test targets pass, run individually. 1363 library unit tests pass.
- `cargo fmt --check`, `cargo clippy --lib --tests -- -D warnings`, and `cargo clippy --bin all-smi -- -D warnings` are clean.
- No dangling references to `Dockerfile`, `docker-check`, `docker-build-container`, `docker_build_context_test`, or `docker-compose` remain outside `TECHNICAL_REPORTS/`, `debian/changelog`, and the `README.md` changelog.

## 5. Outcome and Follow-up

- PR #344 was squash-merged into `main` as `dd17ebd`.
- Issue #342 closed automatically through the PR's `Closes #342` link.
- This shipped as a breaking change in v0.26.0. Anyone who was building the image locally has to move to one of the supported install paths.
- The `DEVELOPERS.md` CI job list remains stale (three named, seven actual) and is left for a dedicated docs pass.
- The build-context guard from #322 is gone with its subject. If a container build is ever reintroduced, that guard has to be rewritten rather than restored, since it asserted against a `Dockerfile` that no longer exists.

---

## Appendix: Related PRs and Issues

| Number | Relationship |
|--------|--------------|
| Issue #342 | Recorded the image failing on every architecture; closed by this PR |
| PR #322 | Added the build-context test this PR deletes, after #319 broke `main` |
| PR #339 | Removed the push-only gate so PRs built the image; superseded one day later |
| PR #343 | Removed the vendored OpenSSL dependency; adjacent cleanup, no file overlap |
