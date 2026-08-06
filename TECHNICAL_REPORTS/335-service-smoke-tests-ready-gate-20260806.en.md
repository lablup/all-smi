# Technical Report: PR #335 - ci: gate both service smoke tests on /-/ready

**Date**: 2026-08-06
**Status**: Completed (systemd system-scope path verified only for the user-scope lifecycle at merge time; see section 8)
**Languages**: YAML (GitHub Actions), bash
**Risk Level**: Low (CI-only change; no application source touched), but the finding it acts on was CI-critical

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

The `systemd-service` job waited on `systemctl is-active` and then asserted `curl -sf .../metrics | head -5`, an assertion `head` cannot fail because it swallows the pipeline's exit status. That is a weaker assertion masking the same gate-versus-assertion mismatch PR #323 fixed on the launchd side, not a correct gate; tightening the assertion alone would have reintroduced the exact race PR #323 fixed, where run 31005178260 lost by 243 ms. PR #335 replaces both jobs' readiness gates with polling `/-/ready`, the endpoint PR #333 added two commits earlier, on the reasoning that the gate must check *the same event* the downstream assertion depends on, not merely a condition that happens to become true around the same time: `/-/ready`'s 200 transition and the exposure of `all_smi_memory_total_bytes` are the same `AppState` write inside `collection_loop.rs`, not two correlated observables.

The sharper finding is what PR #333 did to PR #323's own gate two commits earlier in the same merge sequence. PR #323 hardened the launchd job by polling `grep -q '^all_smi_'` for content, reasoning that an empty `AppState` renders a byte-empty body. PR #333 made the `all_smi_up`/`all_smi_build_info` baseline unconditional, which means `^all_smi_` now matches from the very first request, before any real collection has happened, so PR #323's own comment block became false and its gate would open immediately and race exactly as the pre-#323 gate did. Migrating the launchd job onto `/-/ready` was therefore not optional scope-matching discipline against issue #329's plural "these CI gates should adopt it": leaving it alone would have shipped a broken gate, silently reintroduced by a PR that never touched the launchd job's own code. The systemd job's readiness gate, its post-crash-recovery check, and its post-restart check all move to the same `ready()` helper polling `http://127.0.0.1:19191/-/ready`; the launchd assertions stay content-based and gain explicit checks for the `all_smi_up`/`all_smi_build_info` baseline. Total: 1 file, +112/-26, one commit, closing #329.

---

## 1. Problem Statement

### 1.1 Background

PR #323 (report: `323-launchd-smoke-test-race-20260805`) diagnosed and fixed a gate-versus-assertion mismatch on the launchd smoke test: the readiness wait checked a weaker condition (HTTP 200) than the assertion that followed it (metric content present), and the gap between the two was wide enough under launchd's `ProcessType=Background` scheduling to lose a race in CI. Issue #329 observed that the `systemd-service` job's system-scope step has the identical shape, `systemctl is-active` as the gate and `curl -sf | head -5` as the assertion, and asked whether it should be fixed the same way, while noting that issue #324 (the readiness contract PR #333 implements) might change the answer.

### 1.2 Existing Issues

- **Issue 1 (the systemd gate checks a weaker condition than its assertion)**: `systemctl is-active` reports active as soon as the unit's binary has been executed under `Type=exec`, independent of whether the listener is bound or a collection cycle has run.
- **Issue 2 (the systemd assertion cannot fail on the mismatch it should catch)**: `curl -sf .../metrics | head -5` cannot fail on an empty body, because `head` swallows the pipeline's exit status; the job could not lose the race PR #323's job lost, but only because its assertion was weaker, not because its gate was correct.
- **Issue 3 (`curl -sf .../metrics` is now permanently unusable as any kind of readiness gate)**: PR #333 made `/metrics` answer 200 from the moment the listener binds, by design, so a plain-200 gate can never again distinguish "up" from "up and ready."
- **Issue 4 (PR #323's own content gate silently broke)**: PR #333's unconditional baseline means `grep -q '^all_smi_'` now matches from the very first request, before the collection loop has written anything, so the launchd job's gate would open immediately and reintroduce the exact 243 ms race PR #323 fixed, unless it is migrated to check the real condition.
- **Issue 5 (the systemd job's changed code did not execute at merge time)**: the system-scope step, which contains the readiness gate this PR rewrites, was still gated behind `steps.probe.outputs.user_scope != 'true'`, a condition that never holds on `ubuntu-latest`, so this PR's systemd changes were unexercised by its own CI run; only the launchd job's migrated gate actually ran.

### 1.3 Risk Assessment

| Risk | Impact | Likelihood |
|---|---|---|
| Leaving the launchd job's content gate unmigrated after PR #333 | High: the gate would open on the very first request, reintroducing PR #323's exact race with no code change to the launchd job itself as the visible trigger | Would have materialized on the next launchd job run after PR #333 merged; avoided because this PR merges immediately after it |
| Tightening the systemd assertion without fixing its gate | High: reintroduces the class of flake PR #323 fixed, this time on the systemd job | Avoided; this PR fixes the gate and the assertion together |
| Verifying the systemd gate change against a code path that does not execute in CI | Medium: the PR's own acceptance criterion for "the job passes on main" could be claimed on the strength of an unexercised path | Explicitly not claimed; the PR ticks the criterion only for the user-scope path and defers the system-scope verification to PR #330 |

---

## 2. Technical Review

### 2.1 Correctness

The core design choice is gating on the same *event* the assertion depends on, not a correlated proxy for it. `collection_loop.rs` clears `loading` inside the same write-lock block that assigns `guard.memory_info`, so `/-/ready` returning 200 and `all_smi_memory_total_bytes` becoming renderable are not two things that happen to align in time, they are the same write observed from two different endpoints. This is a stronger relationship than PR #323's content gate offered: that gate polled for a metric line's presence, which became true at the same moment the readiness condition did only because both are downstream of the same collection cycle, not because the gate checked readiness directly. Polling `/-/ready` on port 19191 rather than 9090 in the systemd job is also a correctness lever, not just a convenience: the target port and the config-driven port are supposed to be the same value once #309/#330's `/etc/all-smi/config.toml` discovery works, so a successful poll on 19191 is itself partial evidence the config file moved the listener.

### 2.2 Performance

No measurable change. The gate loops are structurally identical to PR #323's (`for _ in $(seq 1 60); do ...; sleep 2; done`), just pointed at a different endpoint; total wait time is bounded the same way, and the job's overall wall-clock cost is dominated by the cargo build, unaffected by this PR.

### 2.3 Compatibility and Dependencies

- **Breaking changes**: none; this PR touches only `.github/workflows/ci.yml`.
- **New dependencies**: none. It depends on the `/-/ready` route PR #333 added, which had already merged.
- **Compatibility**: no Rust source changed, so no `cargo` checks apply to this diff; the PR's own test plan reflects that.

### 2.4 Code Quality

The `ready()` helper is duplicated with small variations across the systemd and launchd jobs (different ports, different diagnostic dumps), following the shape PR #323 established rather than introducing a shared script or composite action; this keeps the diff small and each job's failure diagnostics self-contained (on timeout, both dump the last `/-/ready` response and the last `/metrics` response, and the systemd variant additionally dumps `ss -lntp`). The systemd job's crash-recovery and `restart` checks previously had no post-action readiness assertion at all; this PR adds `ready` calls after both, so `restart`'s own exit code is no longer the only signal that the replacement process actually came up.

---

## 3. Technical Decisions

### 3.1 Gate on `/-/ready`, not on a stronger content check, once a real readiness endpoint exists

**Context**: with issue #324/PR #333 landed, the systemd job's gate could have been fixed several ways: strengthen `curl -sf | head -5` into a content check (`grep -q '^all_smi_'`, mirroring PR #323's launchd fix before this PR), or adopt the new `/-/ready` endpoint.

| Option | Pros | Cons |
|---|---|---|
| Mirror PR #323: gate on `grep -q '^all_smi_'` | Consistent with the pattern already proven on launchd | Already broken by PR #333's unconditional baseline before this PR could even land it; would need a second fix immediately |
| **Chosen: gate on `/-/ready`** | Checks the exact event the assertion depends on (same `AppState` write); immune to any future exporter change that adds another unconditional family, unlike a content-pattern gate | Requires the endpoint to exist, which it now does via PR #333 |
| Keep `systemctl is-active` / `curl -sf` and accept the risk | No change needed | Leaves the exact mismatch PR #323 fixed, present on the systemd job; rejected outright |

**Rationale**: a gate keyed to a specific rendered metric name is one exporter change away from breaking again, as PR #333 demonstrated for PR #323's own gate. `/-/ready` is purpose-built to answer exactly the question the gate needs answered and is defined by the same predicate the exposition's `all_smi_up` gauge uses, so it cannot be invalidated by an unrelated metrics change the way a `grep` pattern can.

### 3.2 Migrate the launchd job's gate too, treating issue #329's scope as covering it explicitly rather than as an out-of-scope nicety

**Context**: the brief driving this PR framed migrating the launchd job as a judgment call between consistency and scope discipline, since issue #329's title and evidence section are about the systemd job specifically.

**Decision**: migrate it. Issue #329's own Scope section states, in the plural, "if #324 lands a real readiness contract, these CI gates should adopt it rather than keep polling for content" (emphasis on "gates," not "gate"), which pre-authorizes touching both jobs. Beyond the textual authorization, two facts make leaving it alone the worse choice, not the conservative one: PR #323's comment block justified content-gating specifically on the claim that an empty `AppState` renders a byte-empty body, a claim PR #333 made false; and PR #323's `grep -q '^all_smi_'` pattern now matches from the very first request because of the same PR #333 change, so the gate would open immediately and race again, unless migrated.

**Rationale**: this is not scope creep, it is closing a defect this same merge sequence introduced two commits earlier. A gate left in its PR #323 shape after PR #333 merged would have been silently broken by a PR that never touched the launchd job's file section.

### 3.3 Keep the launchd assertions content-based rather than also moving them to `/-/ready`

**Context**: once the gate uses `/-/ready`, the downstream assertions could in principle also just check `/-/ready`'s status again rather than asserting specific metric content.

**Decision**: keep asserting content (`^all_smi_up`, `^all_smi_build_info`, `^all_smi_memory_total_bytes` for launchd; `^all_smi_memory_total_bytes` for systemd), on top of the `/-/ready` gate rather than instead of it.

**Rationale**: `/-/ready` proves a collection cycle completed; it does not by itself prove which families the exposition actually rendered on this specific runner. The launchd job runs on a macOS VM with no IOReport, so its content assertions exist to prove the macOS-specific degradation path (native GPU/chassis metrics absent, memory metrics present) still produces real data, which is a different and more specific claim than "the service is ready." Collapsing the two into one check would lose that specificity.

---

## 4. Implementation Details

### 4.1 Architecture Changes

```
[Before]
systemd job:  systemctl is-active (weak gate) -> curl | head -5 (can't fail on empty body)
launchd job:  curl | grep -q '^all_smi_' (gate, from PR #323) -> same pattern (assertion)
                 ^ this gate silently broke the moment PR #333 made the baseline unconditional

[After]
systemd job:  ready() { poll http://127.0.0.1:19191/-/ready }
                 -> curl | grep -q '^all_smi_memory_total_bytes'
                 -> also called after kill -9 recovery and after `service restart`
launchd job:  ready() { poll localhost:9090/-/ready }
                 -> curl | grep -q '^all_smi_up'
                 -> curl | grep -q '^all_smi_build_info'
                 -> curl | grep -q '^all_smi_memory_total_bytes'
```

### 4.2 Key Code Changes

**File: `.github/workflows/ci.yml` (systemd job's new gate)**
```bash
ready() {
  for _ in $(seq 1 60); do
    if curl -sf --max-time 5 "http://127.0.0.1:19191/-/ready" >/dev/null 2>&1; then
      return 0
    fi
    sleep 2
  done
  echo "::error::the exporter never reported ready on 127.0.0.1:19191/-/ready"
  echo "--- last /-/ready response ---"
  curl -sv --max-time 5 "http://127.0.0.1:19191/-/ready" 2>&1 | head -40
  echo "--- last /metrics response ---"
  curl -sv --max-time 5 "http://127.0.0.1:19191/metrics" 2>&1 | head -40
  echo "--- listeners ---"
  sudo ss -lntp 2>&1 | head -20 || true
  return 1
}
```
**Reason for change**: replaces the `systemctl is-active` loop and the unconditional `curl -sf ... | head -5` assertion. On timeout it dumps both `/-/ready` and `/metrics` plus the listener table, so a future failure is diagnosable from the CI log without a rerun.

**File: `.github/workflows/ci.yml` (systemd assertion, tightened alongside the gate)**
```bash
sudo "$BIN" service status
sudo "$BIN" service status --json | grep -q '"running": true'
# Assert on rendered content, not on the port merely answering.
# `all_smi_memory_total_bytes` rather than a bare `^all_smi_` prefix:
# since #324 the baseline families are emitted unconditionally, so
# `^all_smi_` now matches even when every device reader has failed.
curl -sf --max-time 10 http://127.0.0.1:19191/metrics | grep -q '^all_smi_memory_total_bytes'
```
**Reason for change**: `head -5` becomes a real content check, and the check specifically targets a family that requires a real collection to have happened, not just the baseline that would now match unconditionally.

**File: `.github/workflows/ci.yml` (launchd job, migrated gate, unchanged assertion class)**
```bash
ready() {
  for _ in $(seq 1 60); do
    if curl -sf --max-time 5 localhost:9090/-/ready >/dev/null 2>&1; then
      return 0
    fi
    sleep 2
  done
  echo "::error::the exporter never reported ready on localhost:9090/-/ready"
  ...
}
...
curl -sf --max-time 10 localhost:9090/metrics | grep -q '^all_smi_up'
curl -sf --max-time 10 localhost:9090/metrics | grep -q '^all_smi_build_info'
curl -sf --max-time 10 localhost:9090/metrics | grep -q '^all_smi_memory_total_bytes'
```
**Reason for change**: the gate moves off the pattern PR #333 silently broke; the assertions gain explicit checks that the new baseline families are present on a real launchd-managed service, on top of the pre-existing memory check.

### 4.3 Data Model Changes

Not applicable. No source code, wire format, or metric definition changed; this PR is entirely CI workflow logic, confined to `.github/workflows/ci.yml`.

---

## 5. Learning Points

### 5.1 A gate built on rendered content is only as stable as the exposition's own "always present" set

**Concept**: a CI gate that polls for a specific line matching a pattern is implicitly betting that the pattern's meaning stays fixed. If a later change makes that pattern match unconditionally (as an intentionally added always-present baseline can), the gate silently stops gating on anything meaningful, without any change to the gate's own code.

**Application in this PR**: PR #323's `grep -q '^all_smi_'` gate was correct when written; PR #333, an unrelated PR to a different subsystem, invalidated it two commits later by design. This is exactly the failure mode `/-/ready` is immune to, since it is backed by a dedicated predicate rather than a pattern over the exposition's contents.

### 5.2 "It has not failed yet" is not evidence a gate is correct, especially right after an upstream change

**Concept**: a CI job that has been green does not prove its gates check the right condition; it may simply not have been exercised against the new failure mode yet. The systemd job's `curl -sf | head -5` had never failed, not because it was a correct gate, but because `head` structurally cannot observe the failure it should catch.

**Application in this PR**: this PR does not wait for the systemd job to actually flake before fixing it, the same posture PR #323 took after a real failure on the launchd side; here the fix is proactive, based on recognizing the identical shape of mismatch issue #329 named, before it has a chance to manifest.

### 5.3 When a fix depends on an upstream contract, re-verify that contract's assumptions after every commit that touches it

**Concept**: a CI gate that depends on another subsystem's behavior (here, which metric lines are always present) needs to be re-checked whenever that subsystem changes, not just written once and trusted.

**Application in this PR**: the PR explicitly traces PR #333's baseline change forward to its effect on PR #323's gate rather than treating the two PRs as unrelated, which is what surfaced the "sharpest finding" this report leads with.

---

## 6. Further Learning

### Key Terms

| Keyword | Description | Relevance |
|---|---|---|
| Gate-versus-assertion mismatch | A readiness wait checking a weaker or different condition than the assertion that follows it | The recurring defect class across PR #323, this PR, and PR #336 |
| `/-/ready` | The readiness endpoint added by PR #333 | The event this PR's gates now check directly, instead of a content proxy |
| `collection_loop.rs`'s write-lock block | Where `AppState.loading` is cleared and `guard.memory_info` is assigned together | Why `/-/ready`'s 200 and `all_smi_memory_total_bytes`'s presence are the same event, not two correlated ones |
| Unconditional baseline (`all_smi_up`, `all_smi_build_info`) | The always-present metric families PR #333 added | The specific change that silently broke PR #323's content gate |
| `head -5` swallowing a pipeline's exit status | A shell pattern where a failing `curl` cannot fail the overall command because of the pipe to `head` | The mechanism that let the systemd job's weak assertion pass without ever exercising the real condition |

### Related Technologies and Frameworks

- Bash pipeline exit-status semantics (`set -o pipefail` is not in effect for these `curl | grep`/`curl | head` lines, which is exactly why `head` can mask a failure).
- Prometheus/Kubernetes-style readiness probing, the model `/-/ready` follows.

### Related PRs and Issues

- Issue #329: the issue this PR closes.
- PR #323: the launchd job's original readiness-gate fix; its own comment block and gate pattern are what PR #333 silently invalidated, which this PR corrects.
- PR #333 (issue #324): added `/-/ready` and the unconditional baseline; the PR whose side effect on PR #323's gate is this report's central finding.
- PR #330 (issue #330), implemented by PR #336: removes the `if:` gate on the systemd job's system-scope step, which is what actually exercises the systemd readiness-gate changes this PR makes; at this PR's own merge time, that step still did not run in CI.

---

## 7. Change Summary

### Statistics

| Item | Value |
|---|---|
| Files changed | 1 (`.github/workflows/ci.yml`) |
| Lines added | +112 |
| Lines removed | -26 |
| Commits | 1 |

### Changes by Category

| Category | Summary |
|---|---|
| CI reliability | systemd job's readiness gate, post-crash-recovery check, and post-`restart` check all switch from `systemctl is-active`/no check to polling `/-/ready` on port 19191 |
| CI reliability | systemd assertion tightened from `head -5` to `grep -q '^all_smi_memory_total_bytes'` |
| CI reliability | launchd job's gate migrated from PR #323's `grep -q '^all_smi_'` (silently broken by PR #333) to polling `/-/ready` |
| CI reliability | launchd assertions gain explicit `^all_smi_up` / `^all_smi_build_info` checks alongside the existing memory check |
| Documentation | Both jobs' headers gain comments recording the gating rule and cross-referencing PR #323, PR #333/issue #324, and this PR/issue #329 |

### Related Commits

| SHA | Type | Message |
|---|---|---|
| `d24fbe4f` | ci | gate both service smoke tests on /-/ready |

Merged to `main` as `cdeccc83`. Closes #329.

---

## 8. Follow-up Actions

### Required

None identified as blocking. The PR explicitly does not claim its systemd changes are exercised end to end; see below.

### Monitoring Required

- **The systemd system-scope step, containing this PR's systemd gate changes, did not execute at merge time.** It remained behind `steps.probe.outputs.user_scope != 'true'`, a condition `ubuntu-latest` never meets, so this PR's own CI run only exercised the user-scope path (unaffected by this PR's changes) plus the migrated launchd gate, which did run and passed. The PR ticks issue #329's "the job passes on main" criterion only for the user-scope path and explicitly defers system-scope verification to PR #330 (implemented by PR #336), which removes that gate.

### Future Improvements

- None proposed beyond the dependency on PR #330/#336 already noted.

---

## Appendix

### A. Test Results

- `yaml.safe_load` parses the workflow; all jobs still resolve.
- `bash -n` on every `run:` block in both edited jobs: 11 steps, 0 syntax errors.
- Launchd job's migrated `/-/ready` gate executed on this PR's own CI run and passed.
- Systemd job's changes did not execute (see Follow-up Actions); the job passed only because the unaffected user-scope path ran.
- Every remaining `systemctl is-active` occurrence was audited to confirm each is restart detection (a new MainPID after `kill -9`), not a readiness gate.

### B. Performance Benchmarks

Not applicable; this is a CI-only change with no measured performance claim in the PR.

### C. References

- PR #323 (report: `323-launchd-smoke-test-race-20260805`): the original readiness-gate fix and race measurement this PR's launchd migration corrects for.
- PR #333 (report: `333-metrics-baseline-ready-endpoint-20260806`): the `/-/ready` contract this PR's gates now depend on, and the source of the baseline change that silently broke PR #323's gate.
- Issue #329: scope, evidence, and acceptance criteria this report draws from, cross-checked against the diff.
