# Technical Report: PR #336 - ci: run both systemd scopes and tear down unconditionally

**Date**: 2026-08-06
**Status**: Completed; first real execution of the system-scope path in the project's history
**Languages**: YAML (GitHub Actions), bash
**Risk Level**: Low (CI-only change), but it exercised a code path (root-privileged systemd install) that had never run before and found a real defect on its first attempt

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

The `systemd-service` job's system-scope step, added by PR #319 (issue #309) to cover crash recovery, the privileged tarball install/uninstall lifecycle, and `/etc/all-smi/config.toml` moving the listener, was gated on `steps.probe.outputs.user_scope != 'true'`. On `ubuntu-latest` a per-user systemd manager is always present, so that condition never held, and the step had never executed once since #309 merged. Everything inside it was unverified code sitting in the workflow, and that gate is the entire reason three #309 acceptance criteria stayed unchecked for as long as they did. PR #336 removes the gate so both scopes run on every job execution, orders user scope before system scope (the system-scope step writes `/etc/all-smi/config.toml`, a discovery candidate for every all-smi process on the host, so running it first would make the user-scope assertions silently test the wrong listener), and adds an `if: always()` teardown step so a step that dies mid-lifecycle cannot strand an enabled unit, a running daemon, or a config file for the next run to trip over.

Running the step for the first time did not go smoothly, which is the most informative part of this PR. The first CI run failed for real with `status=203/EXEC`: the unit's `ProtectHome=true` made `/home/runner/work/all-smi/all-smi/target/debug/all-smi` unexecutable, because `ExecStart` pointed straight at the workspace checkout under `$HOME`. This is a genuine property of the hardened unit that nobody had hit before, precisely because the step had never run. The fix stages the binary to `/usr/local/bin` with `install -m 0755` rather than relaxing `ProtectHome`, which the PR argues is a more faithful test of #309's "from a plain tarball install" criterion, not a workaround. The second run passed and closed three #309 criteria with concrete CI evidence: `kill -9 4332` led to a new MainPID `4435` within `RestartSec=5`; a tarball install through `uninstall` left no unit file behind; and `[api] port = 19191` served `/-/ready` and `all_smi_memory_total_bytes` while the compiled default port 9090 was explicitly confirmed not listening, proving the config file moved the listener rather than merely also being answered on. The PR deliberately ticks nothing on the separately tracked issue #332, whose own scope excludes these three items, and instead leaves an evidence comment there. Total: 1 file, +156/-12, two commits, closing #330.

---

## 1. Problem Statement

### 1.1 Background

PR #319 (issue #309) added the `systemd-service` smoke test with two lifecycle paths: a user-scope step (no root required) and a system-scope step (root, covering what user scope structurally cannot: crash recovery via `kill -9` and `RestartSec`, the privileged tarball install/uninstall flow, and `/etc/all-smi/config.toml` discovery). A probe step decided which one ran, based on whether the runner has a per-user systemd manager. `ubuntu-latest` always has one, so the system-scope step's `if: steps.probe.outputs.user_scope != 'true'` was, in effect, `if: false` on every run since it merged.

### 1.2 Existing Issues

- **Issue 1 (the system-scope step never executed)**: its gate condition is never true on the runner this job actually uses, so every check inside it, including three #309 acceptance criteria, was unverified code rather than a passing test.
- **Issue 2 (the two scopes have a hidden ordering dependency)**: the system-scope step writes `/etc/all-smi/config.toml`, and `LINUX_SYSTEM_CONFIG_PATH` (`src/common/paths.rs`) makes that path a discovery candidate for *every* all-smi process on the host, not only system-scope ones. Running system scope before user scope would make a user-scope daemon silently inherit port 19191 from that file, and the user-scope assertions would then be testing something other than what they claim.
- **Issue 3 (no unconditional cleanup)**: every existing cleanup line ran only on the success path. A step that died mid-lifecycle left an enabled unit, a config file, or a running daemon behind for a later run to trip over, and running both scopes unconditionally increases the amount of state that can be stranded this way.
- **Issue 4 (the config-file criterion was only ever implied, not proved)**: a daemon serving on port 19191 does not by itself prove `/etc/all-smi/config.toml` moved the listener; a daemon that ignored the file and happened to also bind 19191, or bound both ports, would pass the same check.

### 1.3 Risk Assessment

| Risk | Impact | Likelihood |
|---|---|---|
| Removing the gate exercises a hardened unit's `ProtectHome=true` against a binary path under `/home` for the first time | High if unaddressed: the step fails with an opaque `203/EXEC` and no application log output at all | Materialized on this PR's own first CI run; fixed within the same PR (section 3.2) rather than shipped broken |
| Running both scopes without deliberate ordering | Medium: a user-scope daemon could silently pick up the system-scope config file's port, making the user-scope assertions test the wrong listener without failing loudly | Avoided by running user scope first, documented in the job header as deliberate rather than incidental |
| A step failing mid-lifecycle with no unconditional cleanup, now with two scopes running | Medium on a hosted runner (merely untidy, since it is ephemeral), higher on a future self-hosted runner (poisons the next run in a way that looks unrelated to its actual cause) | Addressed by the new `if: always()` teardown step, itself proven on the failed first run |

---

## 2. Technical Review

### 2.1 Correctness

The gate change itself is a one-line removal (`if: steps.probe.outputs.user_scope != 'true'` deleted from the system-scope step), but its correctness depends on everything downstream of it actually being safe to run unconditionally, which is exactly what the first failed CI run tested and found wanting. The probe survives, narrowed to its one honest remaining use: skipping the user-scope step on a runner that genuinely has no user manager. The system-scope step no longer consults it at all, which the PR states explicitly rather than leaving as an implicit consequence of deleting the `if:` line, since "no longer a fallback, covers strictly more" is a claim worth stating for a future reader deciding whether the step is still needed.

The config-file criterion is proved rather than implied by adding an explicit negative check: after confirming `/-/ready` and `all_smi_memory_total_bytes` on port 19191, the step also asserts port 9090 is *not* listening. This closes the gap where a daemon binding both ports, or ignoring the config file and happening to bind 19191 anyway, would have passed the weaker check.

### 2.2 Performance

The PR measures rather than assumes the cost of running both scopes: the new system-scope step took 11 seconds and the new teardown step took 1 second in the successful run, for about 12 seconds of added step time. Job totals: 173 seconds before (run 31101864563, system-scope skipped) versus 151 seconds after (run 31103744522); the job is dominated by the cargo build and cache restore, whose run-to-run variance exceeds the cost of the added scope, so the net wall-clock change is not attributable to this PR in either direction with confidence.

### 2.3 Compatibility and Dependencies

- **Breaking changes**: none; this PR touches only `.github/workflows/ci.yml`.
- **New dependencies**: none.
- **Compatibility**: no Rust source changed, so `cargo` checks are not relevant to this diff.

### 2.4 Code Quality

The teardown step is deliberately best-effort throughout (`set +e`, `|| true` on every line), on the reasoning that a teardown failure turning an otherwise-green run red would mask the actual failure on a red run and add noise on a green one; it reports residue as `::warning::` annotations and a positive "clean" line rather than enforcing cleanliness with an exit code. It removes the system unit, the user unit, `/etc/all-smi/config.toml`, `/etc/default/all-smi`, the staged `/usr/local/bin/all-smi` binary, `/var/cache/all-smi`, and an `all-smi` system user if one exists, then runs `daemon-reload` and `reset-failed`. The `userdel` line is explicitly documented as belt-and-braces rather than addressing a real leak: `all-smi service install` never runs `useradd` (`--service-user` only writes `User=` into the unit and expects the account to already exist), so the criterion is satisfied by construction; the teardown line exists so that property does not have to keep holding for the criterion to stay true.

---

## 3. Technical Decisions

### 3.1 Remove the gate rather than invert it or add a second unconditional job

**Context**: the system-scope step needed to run on every execution, not only when the probe reports no user manager, but the probe itself still has a legitimate use for the user-scope step.

| Option | Pros | Cons |
|---|---|---|
| Invert the probe condition so system scope becomes the default and user scope the fallback | Symmetric with the old shape | Backwards from what #309 actually needs verified continuously (the union of both), and still an either/or rather than both |
| Add a second, separate job that always runs the system-scope lifecycle | Keeps the existing job's shape untouched | Duplicates setup (checkout, toolchain, build) for no benefit, since both scopes need the same built binary |
| **Chosen: remove the `if:` gate from the system-scope step; keep the probe for the user-scope step only** | Both scopes run in the same job, sharing one build; the probe's remaining use (skip user scope on a genuinely manager-less runner) stays intact | The two scopes now share global state (unit paths under `/etc/systemd/system` versus the user's own unit directory, `/etc/all-smi/config.toml`), which has to be managed deliberately (section 3.2) rather than being a non-issue |

**Rationale**: system scope "covers strictly more" than user scope, per the PR's own framing, rather than substituting for it, so running both unconditionally is the only shape that actually verifies everything #309 asked for on every run, on the one runner (`ubuntu-latest`) this job actually executes on.

### 3.2 Run user scope before system scope, and state why in the job header rather than leaving it to be discovered

**Context**: the system-scope step writes `/etc/all-smi/config.toml`. `src/common/paths.rs`'s `LINUX_SYSTEM_CONFIG_PATH` makes that file a discovery candidate for *any* all-smi process on the host, system-scope or not.

**Decision**: user scope runs first.

**Rationale**: if system scope ran first and left `/etc/all-smi/config.toml` with `api.port = 19191` in place (even after its own cleanup, in a scenario where the file survives a partial failure), a subsequently started user-scope daemon could silently discover and honor that file, moving its own listener off port 9090 without the user-scope assertions (which check 9090) ever failing loudly, they would simply be checking a port nothing is bound to for reasons unrelated to what they intended to test. Ordering user scope first removes the window entirely. The PR records this reasoning directly in the job's header comment so a future edit that reorders the steps "for readability" has to consciously override a documented decision rather than silently reintroduce the hazard.

### 3.3 Fix the `203/EXEC` failure by staging the binary outside `$HOME`, not by relaxing `ProtectHome`

**Context**: the first real execution of the system-scope step failed with `status=203/EXEC` because `ExecStart` pointed at `/home/runner/work/all-smi/all-smi/target/debug/all-smi`, and the unit's `ProtectHome=true` hides `/home` from the process, so systemd could not execute the binary at all.

| Option | Pros | Cons |
|---|---|---|
| Relax `ProtectHome` in the unit template so the CI binary location works | Fixes the immediate failure with a small change | Weakens every real deployment's hardening to accommodate a binary location no real deployment uses; the unit under test would no longer be the unit that ships |
| **Chosen: stage `target/debug/all-smi` to `/usr/local/bin` with `install -m 0755` before installing the service from there** | #309's own criterion is specifically "from a plain tarball install," and `/usr/local/bin` is exactly where a tarball install puts the binary, so this makes the step a more faithful test of the criterion rather than a workaround for a CI-specific path problem | Adds a staging step; teardown must also remove the staged copy |
| Leave `ExecStart` pointing into the workspace and accept the step cannot run under a hardened unit | Simplest | Fails the actual goal of this PR, which is to exercise the system-scope step at all |

**Rationale**: the failure is a genuine property of the hardened unit, not a CI artifact, and the PR is explicit that "nobody hit this before because the step was gated behind a condition `ubuntu-latest` never meets." Fixing it by relaxing the unit would have hidden a real hardening/deployment-location interaction rather than testing it correctly; staging to `/usr/local/bin` both fixes the CI failure and makes the test closer to a genuine tarball deployment than it was before this PR.

### 3.4 Tick nothing on issue #332; leave evidence there instead

**Context**: the brief driving this PR's originating work noted that issue #332's "group A" might appear to overlap with the three #309 criteria this PR closes.

**Decision**: tick nothing on #332. Its own Scope section explicitly excludes these three items ("Three of the 19 items are already tracked by #330 ... Those three are excluded from the checklist below so the two issues do not overlap"), and #332's group A covers three different, still-unverified items: a real `.deb` install, a non-systemd host, and a dpkg-managed binary refusing without `--force`. None of those is exercised by this job.

**Rationale**: ticking anything on #332 would have been false. Instead, PR #336 leaves an evidence comment there recording that #332's documented exclusion is now discharged (the three items it deliberately left out are done, via #330), plus the `ProtectHome` finding since it bears on #332's own deb-install item. #332 stays open with zero of its sixteen items verified.

---

## 4. Implementation Details

### 4.1 Architecture Changes

```
[Before]
probe (user manager present?) --true--> User-scope lifecycle
                               --false-> System-scope lifecycle   (never true on ubuntu-latest)

[After]
probe (user manager present?) --true--> User-scope lifecycle
                               --false-> (skip user-scope step only)

System-scope lifecycle: unconditional, runs after user-scope, every execution
  - stage target/debug/all-smi -> /usr/local/bin/all-smi (outside $HOME, ProtectHome-safe)
  - install --now, kill -9 recovery, restart, tarball uninstall, config.toml port move + 9090-not-listening check

Clean up all systemd state: if: always(), runs regardless of what failed above
  - best-effort removal of both scopes' units, config files, staged binary, service user
  - reports residue as warnings, never fails the job itself
```

### 4.2 Key Code Changes

**File: `.github/workflows/ci.yml` (the gate removed, ordering documented)**
```yaml
# Ordering is deliberate, not incidental. User scope runs first
# because the system-scope step writes /etc/all-smi/config.toml, and
# that path is a discovery candidate for *every* all-smi process on
# the host (`LINUX_SYSTEM_CONFIG_PATH` in src/common/paths.rs), so a
# user-scope daemon started after it would silently inherit
# port 19191. The two scopes are otherwise disjoint: different unit
# paths, different managers, different ports (9090 vs 19191).
...
      - name: System-scope lifecycle
        run: |
          set -eEux
          UNIT=/etc/systemd/system/all-smi.service
```
**Reason for change**: the `if:` condition that previously guarded this step is gone; the comment records why the step's position relative to the user-scope step matters, so a future reordering is a conscious decision rather than an accidental regression.

**File: `.github/workflows/ci.yml` (staging outside `$HOME`, discovered by the first real CI run)**
```bash
# Stage the binary outside $HOME before installing it, the way
# a tarball install actually would. This is load bearing, not
# tidiness: the system unit sets ProtectHome=true, so a service
# whose ExecStart points into /home/runner/work/... cannot be
# executed at all. systemd fails it with 203/EXEC before the
# process starts...
BIN=/usr/local/bin/all-smi
sudo install -m 0755 "$PWD/target/debug/all-smi" "$BIN"
```
**Reason for change**: this is the direct fix for the `203/EXEC` failure on the step's first real execution; it also happens to make the test more faithful to #309's tarball-install criterion.

**File: `.github/workflows/ci.yml` (proving the config-file criterion, not just observing a coincidence)**
```bash
# #309 asks that api.port in /etc/all-smi/config.toml "moves"
# the listener, so answering on 19191 is only half the proof:
# a daemon that ignored the file and also happened to bind
# 19191, or one that bound both, would pass that alone. Assert
# the compiled default is NOT listening.
if curl -sf --max-time 5 http://127.0.0.1:9090/metrics >/dev/null 2>&1; then
  echo "::error::the daemon is still serving on the default port 9090; /etc/all-smi/config.toml did not move the listener"
  ...
  exit 1
fi
```
**Reason for change**: closes the gap between "the config-driven port answers" and "the config file actually moved the listener," which are not the same claim.

**File: `.github/workflows/ci.yml` (unconditional teardown)**
```yaml
      - name: Clean up all systemd state
        if: always()
        run: |
          set +e
          ...
          sudo rm -f /usr/local/bin/all-smi || true
          sudo rm -rf /var/cache/all-smi || true
          ...
          if [ "$LEFTOVER" = "0" ]; then echo "clean: no unit, no config, no service user"; fi
          exit 0
```
**Reason for change**: guarantees a failed step cannot poison a later run's environment; `exit 0` at the end means the teardown itself never turns a run red, only reports what it found.

### 4.3 Data Model Changes

Not applicable. This PR is entirely CI workflow logic; no source code, wire format, or metric definition changed.

---

## 5. Learning Points

### 5.1 A gate that never opens is not a fallback, it is dead code with a green checkmark

**Concept**: a CI step guarded by a condition that never evaluates true on the runner it actually executes on will always show as "skipped," which reads as intentional and benign, not as "never verified." The distinction matters because the step can accumulate real defects (like a hardened unit's incompatibility with the CI binary's path) with no signal at all until the gate is finally removed.

**Application in this PR**: the system-scope step had been present since PR #319 and had never run once. Its first real execution immediately found a defect (`203/EXEC`) that had nothing to do with this PR's own changes, it was latent in the unit template the whole time, waiting for the step to actually execute.

### 5.2 Fixing a CI failure by weakening the thing under test is a different action from fixing the test

**Concept**: when a hardened production artifact (here, a systemd unit with `ProtectHome=true`) fails under a specific CI condition, there are two categories of fix: change the artifact to tolerate the CI condition, or change the CI condition to be a more faithful test of the artifact as it ships. Only the second one is actually testing anything.

**Application in this PR**: relaxing `ProtectHome` would have made the test pass by making the unit under test different from the unit that ships. Staging the binary to `/usr/local/bin` instead made the CI environment resemble the deployment scenario #309's criterion actually describes, so the fix improved the test's fidelity rather than only removing a red X.

### 5.3 A criterion phrased as "X happens" needs a matching negative check for "not-X does not also happen"

**Concept**: "the daemon serves on the configured port" is a weaker claim than "the configuration moved the listener," because the former is also true if the daemon ignores configuration entirely and happens to bind the same port, or binds multiple ports. Proving the stronger claim requires checking that the alternative explanation is absent.

**Application in this PR**: the explicit assertion that port 9090 is *not* listening is what upgrades "port 19191 answers" into "the config file moved the listener," closing a gap the original #309 criterion's phrasing left open.

---

## 6. Further Learning

### Key Terms

| Keyword | Description | Relevance |
|---|---|---|
| `ProtectHome=true` | systemd hardening directive hiding `/home` (and similar) from the unit's process | Made the CI-checked-out binary under `/home/runner/work/...` unexecutable, producing `203/EXEC` |
| `203/EXEC` | systemd's exit status for a unit whose `ExecStart` binary could not be executed at all | The concrete failure this PR's first real run produced and diagnosed |
| `LINUX_SYSTEM_CONFIG_PATH` (`src/common/paths.rs`) | The system-wide config candidate path (`/etc/all-smi/config.toml`) added by PR #319 | Why user scope has to run before system scope in this job |
| `if: always()` | GitHub Actions step condition that runs regardless of prior step outcomes | Used for the new teardown step so a mid-lifecycle failure cannot strand state |
| Belt-and-braces cleanup | A cleanup action addressing a leak that current code should already prevent by construction | The `userdel` line in the teardown step, documented as such rather than as evidence of a real leak |

### Related Technologies and Frameworks

- `systemd.exec(5)` hardening directives, specifically `ProtectHome=` and its interaction with a unit's `ExecStart` path.
- GitHub Actions job/step conditionals (`if:`), including `always()` for unconditional cleanup steps.

### Related PRs and Issues

- Issue #330: the issue this PR closes.
- PR #319 (issue #309): added the `systemd-service` job, its probe-gated either/or structure, and the three acceptance criteria this PR is the first to actually verify in CI.
- PR #335: gates this same job's readiness checks on `/-/ready`; its systemd-side changes are what this PR's newly-unconditional system-scope step actually exercises for the first time.
- Issue #332: a separately tracked issue whose scope explicitly excludes the three criteria this PR closes; receives an evidence comment rather than any ticked item.

---

## 7. Change Summary

### Statistics

| Item | Value |
|---|---|
| Files changed | 1 (`.github/workflows/ci.yml`) |
| Lines added | +156 |
| Lines removed | -12 |
| Commits | 2 |

### Changes by Category

| Category | Summary |
|---|---|
| CI coverage | System-scope systemd lifecycle step runs unconditionally on every execution; the `if:` gate is removed and the probe is narrowed to gating the user-scope step only |
| CI reliability | Deliberate user-scope-then-system-scope ordering, documented in the job header, to prevent a config-file collision between the two scopes |
| CI reliability | New `if: always()` teardown step removing both scopes' units, config files, the staged binary, and a leftover service user, reporting residue as warnings |
| Bug fix (CI environment) | `203/EXEC` failure fixed by staging the binary to `/usr/local/bin` rather than relaxing the unit's `ProtectHome=true` hardening |
| Verification | Config-file criterion strengthened with an explicit "default port 9090 is not listening" check |

### Related Commits

| SHA | Type | Message |
|---|---|---|
| `ee892866` | ci | run both systemd scopes and tear down unconditionally |
| `4b92a2aa` | ci | install the system-scope binary outside /home before starting it |

Merged to `main` as `4646da6c`. Closes #330.

---

## 8. Follow-up Actions

### Required

None identified as blocking. All three #309 criteria this PR targets are ticked with CI evidence from run 31103744522 (see Appendix A).

### Monitoring Required

- Issue #332 remains open with 0 of 16 items verified; the evidence comment left by this PR only discharges the three items #332's own scope had already excluded as covered by #330.

### Future Improvements

- None proposed in the PR beyond what issue #332 already tracks independently.

---

## Appendix

### A. Test Results

- `yaml.safe_load` parses the workflow; step gating verified programmatically: only `User-scope lifecycle` carries a probe condition, `System-scope lifecycle` is unconditional, `Clean up all systemd state` is `if: always()`.
- `bash -n` across every `run:` block in the systemd and launchd jobs: 0 syntax errors.
- CI run 31102859355 (first real execution of the system-scope step): failed with `status=203/EXEC`; the `/-/ready` gate from PR #335 timed out correctly after 120 s with diagnostics pointing straight at the cause (empty `ss -lntp`, connection refused on 19191, the 203/EXEC status); the `if: always()` teardown ran and succeeded despite the lifecycle step dying mid-flight.
- CI run 31103744522 (after the staging fix): `User-scope lifecycle` success, `System-scope lifecycle` success, `Refuse to clobber a foreign unit` success, `Clean up all systemd state` success.
- Per-criterion evidence from run 31103744522: `sudo kill -9 4332` then MainPID `4435` within `RestartSec=5`, followed by a readiness wait and a metric assertion; staged to `/usr/local/bin/all-smi`, installed, `"running": true`, uninstalled, `test ! -f` on the unit passed; `[api] port = 19191` written, served `/-/ready` and `all_smi_memory_total_bytes`, and the default port 9090 confirmed not listening.

### B. Performance Benchmarks

`System-scope lifecycle` step: 11 s. `Clean up all systemd state` step: 1 s. Roughly 12 s of added step time. Job totals: 173 s before (run 31101864563, system-scope skipped) versus 151 s after (run 31103744522); the job is dominated by the cargo build and cache restore, whose run-to-run variance exceeds this delta.

### C. References

- Issue #330: acceptance criteria and per-criterion CI evidence this report draws from, cross-checked against the diff.
- Issue #309: the three long-unverified criteria this PR closes.
- Issue #332: the separately tracked issue whose scope excludes these three items; received an evidence comment from this PR.
- `src/common/paths.rs`: `LINUX_SYSTEM_CONFIG_PATH`, the discovery candidate that makes the user-scope-before-system-scope ordering necessary.
