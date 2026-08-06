# Technical Report: PR #323 - fix(ci): gate the launchd smoke test on metric content, not HTTP 200

**Date**: 2026-08-05
**Status**: Completed
**Languages**: YAML (GitHub Actions, bash)
**Risk Level**: Low (CI-only change; no application source touched)

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

The `launchd Service Smoke Test` job PR #321 added went red on `main` at `464bfdd` ([run 31005178260](https://github.com/lablup/all-smi/actions/runs/31005178260)). The failure was not a flake in the ordinary sense: the job's readiness gate and its downstream assertion were checking two different conditions, and the gate happened to be coarse enough, by luck, to usually paper over the gap. `ready()` waited for `curl -sf` to return HTTP 200, which happens the instant axum binds the listener. The assertion immediately afterward required the response body to contain a line starting with `all_smi_`. Those are not the same moment: `/metrics` renders directly from `AppState`, the background collection loop in `src/api/collection_loop.rs` has not written anything into it yet at bind time, and an empty `AppState` renders a byte-empty body by design, a behavior the existing unit test `empty_inputs_render_empty_string` already asserts. So the endpoint legitimately answers 200 with zero bytes for a real, measured window after the listener binds, not as a bug but as documented behavior nobody had connected to this CI job's timing assumptions. In the failing run, the gate opened at `12:25:28.626` and the assertion fired 243 milliseconds later, still inside that window, against an otherwise entirely healthy service (pid 4094, `state: running`, both requests logging `status=200`).

Local measurement on an M1 Ultra, polling `/metrics` every 20 milliseconds from process start, put the empty-body window at about 0.30 seconds at normal scheduling priority and between 2.1 and 3.5 seconds under background QoS, which is what the plist's `ProcessType=Background` actually selects for a launchd-managed instance. Driving the real gate functions against a background-QoS process confirmed the mechanism precisely: the old HTTP-200 gate failed 5 out of 5 times at 20ms polling granularity and passed 5 out of 5 times at the shipped 2-second granularity, which means the old gate's correctness depended entirely on its poll interval happening to be coarse enough to step over the empty-body window by accident, not on checking the right condition. The fix polls for the same content the downstream assertions require (`grep -q '^all_smi_'`), asserts a specific metric family (`all_smi_memory_total_bytes`, chosen because the memory reader is pure `sysinfo` with no failure path, making it the one line guaranteed present regardless of IOReport availability), and replaces two other fixed-sleep waits (the energy-WAL shutdown check, the post-uninstall `launchctl print` check) with polls for the same reason: each was a gate-versus-assertion mismatch in a different costume. The IOReport warning visible in the failing run's log was investigated and ruled out as a red herring; the window exists on hardware with IOReport fully available, and a VM runner without it only widens the same pre-existing window. No source code changed. Total: 1 file (`.github/workflows/ci.yml`), +49/-11, one commit, no linked issue.

---

## 1. Problem Statement

### 1.1 Background

PR #321 added a `launchd-service` CI job exercising the full `all-smi service` lifecycle (install, start, restart, stop, uninstall) against a real launchd LaunchAgent on a `macos-14` runner. The job's `ready()` helper exists specifically because `service status` reports the process running the moment launchd has spawned it, well before the API server has actually begun serving useful data, so every readiness wait in the job was written to poll the metrics endpoint rather than the process state. That reasoning was correct as far as it went; it did not go far enough, because it treated "the endpoint answers" and "the endpoint has data" as the same condition.

### 1.2 Existing Issues

- **Issue 1 (the readiness gate and the assertion check different things)**: `ready()` polled for `curl -sf ... >/dev/null`, i.e., a successful HTTP response of any content, while the very next lines in the job asserted `grep -q '^all_smi_'` against that same endpoint's body. Axum begins answering `/metrics` as soon as its listener is bound, independent of whether the background collection loop (`src/api/collection_loop.rs`) has completed even one cycle.
- **Issue 2 (an empty response is not a bug, but nothing in the job accounted for it)**: `/metrics` renders straight from `AppState`, and an empty `AppState` produces a byte-empty body by design; `empty_inputs_render_empty_string` in `src/api/metrics/render.rs` already asserts exactly this. The job's gate simply never checked for this documented, intentional behavior.
- **Issue 3 (two other fixed-duration waits had the identical shape)**: the energy-WAL shutdown check slept a fixed 3 seconds before grepping the log for the flush line, and the post-uninstall check queried `launchctl print` exactly once immediately after `uninstall`. Both are the same class of mistake: a fixed wait (or no wait at all) standing in for a poll on the actual condition the following assertion depends on.
- **Issue 4 (the log's IOReport warning was a plausible but incorrect suspect)**: the failing run's log contained a `Failed to create IOReport subscription` warning, which could easily be mistaken for the root cause on a VM runner with no real IOReport-capable hardware behind it.

### 1.3 Risk Assessment

| Risk | Impact | Likelihood |
|---|---|---|
| The old gate's correctness depended on its poll interval (2 seconds) happening to exceed the empty-body window, not on checking the right condition | High: any change that shortens the window's typical closing time relative to the poll interval, or any environment with a longer window, reintroduces intermittent failures | Materialized directly: the failing run lost the race by 243 milliseconds |
| Weakening the shutdown-log wait or the post-uninstall check in isolation, without recognizing they share the same underlying mistake | Medium: a future edit fixing only the readiness gate would leave two other latent races in the same job | Closed in this PR by fixing all three in one pass, once the shared pattern was recognized |
| Misdiagnosing the IOReport warning as the cause and "fixing" the wrong thing | Medium: would have left the actual race condition in place while adding unrelated noise | Ruled out by tracing the actual reader behavior (section 2.2) rather than acting on the warning's presence alone |
| The same gate-versus-assertion mismatch exists in the `systemd-service` job (PR #319) | Low today, since that job's assertion is only `curl -sf ... | head -5` with no content check, but would become exploitable the moment someone strengthens that line | Explicitly noted, deliberately left unfixed in this PR (section 8) |

---

## 2. Technical Review

### 2.1 Root cause, measured rather than assumed

**The two conditions being conflated.** `ready()`'s condition was "the TCP listener accepts a connection and axum answers with 2xx." The condition the subsequent assertions actually require is "the background collection loop has completed at least one cycle and written into `AppState`." Between listener bind and the first completed collection cycle, `/metrics` answers 200 with a body of exactly zero bytes, which is `empty_inputs_render_empty_string`'s asserted behavior working exactly as designed, just not the behavior this CI job's assumptions accounted for.

**Local measurement, polling every 20 milliseconds from process start on an M1 Ultra with IOReport available:**

| Condition | First HTTP 200 | First `all_smi_` line | Window serving 200 with an empty body |
|---|---|---|---|
| normal priority | 0.51 s | 0.81 s | 0.30 s |
| normal priority | 0.52 s | 0.83 s | 0.31 s |
| background QoS (`taskpolicy -b`) | 3.74 s | 5.86 s | 2.12 s |
| background QoS | 4.67 s | 6.94 s | 2.26 s |
| background QoS | 6.03 s | 9.55 s | 3.52 s |

Background QoS is the relevant row for this job specifically, since the launchd plist sets `ProcessType=Background`, which is exactly the scheduling class a real launchd-managed instance runs under.

**Driving the actual gate functions, not a reconstruction of them, against a background-QoS instance:**

| Gate | Poll granularity | Result |
|---|---|---|
| old (HTTP 200) | 20 ms | 5/5 FAIL |
| old (HTTP 200) | 2 s, as shipped | 5/5 pass |
| new (`^all_smi_`) | 2 s | 3/3 pass |
| new (`^all_smi_`) | 20 ms, equal 120 s budget | 5/5 pass |

The second row is the finding that explains why this had not failed constantly: the old gate's correctness depended entirely on its 2-second poll interval happening to be coarse enough to usually step over the 2.1–3.5 second empty-body window by accident. This is not a runner quirk and not a flake in the conventional sense; it is a gate that was, by construction, only sometimes checking the condition it needed to.

**The IOReport warning, investigated and ruled out.** Tracing through the readers confirms the native metrics manager genuinely stays absent for the whole process on a runner without IOReport: `NativeMetricsManager::new()` propagates the `IOReport::new()` error before assigning the singleton, so `get_native_metrics_manager()` returns `None` forever after for that process's lifetime. But the effect of that absence differs sharply by reader:

| Reader | Behavior without the native manager | Deciding line |
|---|---|---|
| Memory | Works, no failure path at all | `src/device/memory_macos.rs:74` |
| CPU | Works, degraded (no temperature/power, placeholder frequencies) | `src/device/cpu_macos.rs:160` |
| GPU | Emits an entry with utilization, power, and temperature hard-zeroed | `src/device/readers/apple_silicon_native.rs:164` |
| Chassis | Returns empty | `src/device/readers/chassis/apple_silicon_native.rs:52` |

None of these degrade to a permanently metric-less body. The emptiness observed in the failing run is entirely a not-yet-collected condition, unrelated to IOReport availability; a content-based poll with a sane time bound is the complete fix, with no source change required.

### 2.2 What the runner actually exports without IOReport, and why the new assertion targets memory specifically

There is no unconditional metric line anywhere in the exposition: no `all_smi_up`, no build-info line, no scrape timestamp. Every exporter function in `src/api/metrics/render.rs` (lines 75–156) is gated on non-empty input, so a fresh `AppState` really does render nothing at all rather than a minimal "I am alive" line. Given that, and given that the runner is a VM lacking IOReport, the new assertion specifically checks `all_smi_memory_total_bytes` rather than any GPU- or chassis-family metric, because the memory reader is the one family with no failure path on macOS regardless of IOReport, virtualization, or GPU support, making it the correct choice for "prove the service produced real data" on exactly the kind of runner this job executes on.

### 2.3 Compatibility and Dependencies

- **Breaking changes**: none; this PR touches only `.github/workflows/ci.yml`.
- **New dependencies**: none.
- **Compatibility**: no Rust source changed, so no `cargo` checks are relevant to this diff; the PR's own test plan states this explicitly rather than running irrelevant checks for form's sake.

### 2.4 Code Quality

Every new loop shape was checked for `set -e` safety specifically, since a subtly wrong pattern here would reintroduce a silent failure mode rather than a loud one: `grep -q ... && break || sleep 1` is safe under `set -e` (the `||` branch absorbs the non-zero exit from a failed `grep`), whereas `grep -q ... && break` alone is not (a failed `grep` under `set -e` would abort the whole script rather than looping). `bash -n` passes on the whole job.

---

## 3. Technical Decisions

### 3.1 Gate on the exact condition the downstream assertion needs, not a weaker proxy for it

**Context**: the job had three independent instances of the same underlying mistake (readiness, shutdown-log wait, post-uninstall check), each using a condition weaker than, or entirely absent relative to, what the following assertion actually required.

**Chosen approach, applied uniformly across all three**:

| Wait | Old condition | New condition |
|---|---|---|
| Readiness (`ready()`) | HTTP 200 from `/metrics` | `curl ... | grep -q '^all_smi_'` |
| Energy-WAL shutdown | Fixed 3-second sleep | Poll (up to 30 s) for `energy WAL: shutdown requested` in the log |
| Post-uninstall `launchctl print` | Single immediate check | Poll (up to 30 s) for `launchctl print` to stop finding the target |

**Rationale**: in each case the previous wait checked a necessary but insufficient condition (or no condition at all, in the fixed-sleep case), and the fix is to check the actual condition the next assertion depends on, bounded by a generous but finite timeout so a genuine failure still fails the job rather than hanging it. The `launchctl print` case is explicitly reasoned by analogy to `BOOTSTRAP_ATTEMPTS` already existing in `src/service_cmd/launchctl.rs` to absorb the same class of race (`launchctl bootout` returning before launchd has actually finished tearing the job down): sampling `launchctl print` exactly once after `uninstall` is the identical gate-versus-assertion mistake in different clothing, recognized by pattern rather than independently rediscovered.

### 3.2 Poll rather than sleep-and-check-once for the shutdown-log line, framed as a latency-regression detector, not merely a flake fix

**Context**: the original 3-second fixed sleep before checking for the shutdown log line was itself a form of the same mistake, just with a wait long enough in practice that it had not yet been observed to fail.

**Chosen approach**: poll for up to 30 seconds rather than sleeping a fixed duration.

**Rationale**: the energy-WAL flush runs on a `spawn_blocking` task and ends in an `fsync`, and this specific CI job is the only place in the project that would notice that operation getting slower over time, since nothing else in the test suite exercises the real shutdown path end to end under an actual service manager. Framing the change as "poll with a generous bound" rather than "increase the fixed sleep" is what keeps the job useful as an early-warning signal for a regression in flush latency, rather than merely restoring today's specific timing by another fixed number likely to go stale the same way the original 3 seconds eventually did.

### 3.3 Leave the `systemd-service` job's equivalent gate shape unfixed, on record rather than silently

**Context**: the `systemd-service` job added by PR #319 has the identical structural shape, waiting on `systemctl is-active` and then curling `/metrics`, but its own assertion is only `curl -sf ... | head -5` with no content check, so today it cannot fail the way this PR's target job did.

**Chosen approach**: leave it alone in this PR, and say so explicitly in the PR description rather than silently ignoring the parallel.

**Rationale**: it is currently green and is not the defect being reported; more importantly, verifying a fix for it is not practical from this environment, since exercising it requires rehearsing a Linux systemd job, which the reporting environment cannot do locally with any confidence. Recording the latent risk (it becomes exploitable the moment someone strengthens that assertion line) is judged more useful than either fixing it blind or leaving it completely unmentioned.

---

## 4. Implementation Details

### 4.1 Architecture Changes

```
[Before, in .github/workflows/ci.yml's launchd-service job]

ready() { for _ in $(seq 1 60); do curl -sf .../metrics >/dev/null && return 0; sleep 2; done; ... }
  -> gates on: listener bound + HTTP 2xx
  -> next assertion requires: response body contains ^all_smi_
  -> gap: 0.3s (normal) to 3.5s (background QoS) where the gate passes but the assertion would fail

sleep 3; grep -q "energy WAL: shutdown requested" "$LOG"
  -> fixed wait, no relation to actual flush completion time

launchctl print "$TARGET" >/dev/null 2>&1 && rc=0 || rc=$?; test "$rc" -ne 0
  -> single sample immediately after `uninstall`, no allowance for launchd's own teardown latency

[After]

ready() { for _ in $(seq 1 60); do curl -sf .../metrics | grep -q '^all_smi_' && return 0; sleep 2; done; ... }
  -> gates on exactly what the downstream assertions require

for _ in $(seq 1 30); do grep -q "energy WAL: shutdown requested" "$LOG" && break || sleep 1; done
  -> polls for the actual condition, bounded, and doubles as a latency-regression detector

for _ in $(seq 1 30); do launchctl print "$TARGET" >/dev/null 2>&1 || break; sleep 1; done
  -> polls for launchd to actually finish tearing the job down before asserting its absence
```

### 4.2 Key Code Changes

**File: `.github/workflows/ci.yml` (the readiness gate)**
```bash
ready() {
  for _ in $(seq 1 60); do
    if curl -sf --max-time 5 localhost:9090/metrics | grep -q '^all_smi_'; then
      return 0
    fi
    sleep 2
  done
  echo "::error::the exporter never rendered an all_smi_ metric line"
  echo "--- last /metrics response ---"
  curl -sv --max-time 5 localhost:9090/metrics 2>&1 | head -40
  return 1
}
```
**Reason for change**: gates on the same content condition the job's own assertions require, and, on exhaustion, dumps the actual last response so a future failure is diagnosable from the CI log directly rather than requiring a rerun to reproduce.

**File: `.github/workflows/ci.yml` (the memory-specific assertion)**
```bash
# Assert a specific family rather than only "some line". The runner is a
# VM with no IOReport, so the native metrics manager fails to initialize
# and the GPU and chassis readers degrade to zeros and to nothing
# respectively. The memory reader is pure sysinfo with no failure path at
# all (src/device/memory_macos.rs), which makes this the one line
# guaranteed to be present on any macOS host.
curl -sf --max-time 10 localhost:9090/metrics | grep -q '^all_smi_memory_total_bytes'
```
**Reason for change**: strengthens "some metric line exists" to "the one metric family guaranteed present on this exact class of runner exists," which is a meaningfully stronger and more specific assertion than the pre-existing `grep -q '^all_smi_'` check it sits beside.

### 4.3 Data Model Changes

Not applicable. No source code, wire format, or metric definition changed; this PR is entirely CI workflow logic.

---

## 5. Learning Points

### 5.1 A readiness gate is only as strong as the weakest condition it shares with the assertion that follows it

**Concept**: "the service responds" and "the service has useful data" are frequently treated as interchangeable in CI health checks, but they are only the same condition when there is no meaningful gap between binding a listener and producing real output. Any service whose response can legitimately be well-formed-but-empty (here, by explicit design) needs its readiness gate to check for content, not merely for a successful response.

**Application in this PR**: `/metrics` answering 200 with zero bytes is not a bug anywhere in this codebase; `empty_inputs_render_empty_string` proves it is intentional. The CI job's mistake was never connecting that intentional behavior to its own timing assumptions.

### 5.2 A gate whose correctness depends on its own poll interval being coarser than an unmeasured window is not actually correct, it is lucky

**Concept**: when a poll loop's granularity happens to exceed the duration of a race window by chance rather than by design, the loop can pass reliably for a long time and still be checking the wrong thing. The measured evidence for this is specific and reproducible: the same gate function, run against the same condition, fails 5/5 at fine granularity and passes 5/5 at the coarse granularity actually shipped.

**Application in this PR**: this is precisely what the two-granularity comparison in section 2.1 demonstrates, and it is why the fix targets the *condition* being polled rather than simply widening the poll interval or its timeout, which would have papered over the same gap again at a different threshold.

### 5.3 Investigate a plausible-looking log warning before treating it as the cause

**Concept**: a warning present in a failing run's log is not evidence that it caused the failure; it is only evidence that it occurred in the same run. Distinguishing correlation from causation here required tracing the actual code paths of each affected reader rather than pattern-matching on the warning text.

**Application in this PR**: the `Failed to create IOReport subscription` warning was a natural suspect on a VM runner, but tracing `memory_macos.rs`, `cpu_macos.rs`, and the two Apple Silicon native readers showed that only two of the four degrade in a way that could plausibly explain an empty body, and even those degrade to zeroed or missing *fields*, not to a metric-less body overall; the actual cause was unrelated to IOReport entirely.

---

## 6. Further Learning

### Key Terms

| Keyword | Description | Relevance |
|---|---|---|
| Readiness gate vs. assertion condition | Two checks in a CI job that should, but do not always, verify the same underlying fact | The central mismatch this PR's fix closes |
| `ProcessType=Background` (launchd) | A plist key selecting background QoS scheduling for the whole process | Why the empty-body window is 2.1–3.5s under launchd specifically, versus 0.3s in the foreground (measured in PR #321's own report, section 5.3, and precisely quantified here) |
| `empty_inputs_render_empty_string` | The existing unit test asserting `/metrics` renders a byte-empty body from an empty `AppState` | The documented, intentional behavior this CI job's timing assumptions had not accounted for |
| Gate-versus-assertion mismatch | A wait condition weaker than what the code immediately following it actually requires | The single underlying pattern behind all three fixes in this PR (readiness, shutdown-log, post-uninstall) |
| `BOOTSTRAP_ATTEMPTS` (`src/service_cmd/launchctl.rs`) | Existing retry logic absorbing the same class of `launchctl bootout`-returns-early race | The precedent this PR's post-uninstall poll is explicitly modeled on |

### Related Technologies and Frameworks

- launchd `ProcessType` and background QoS scheduling on macOS, and its effect on IOReport channel enumeration latency.
- Prometheus exposition format conventions around unconditional versus data-gated metric lines, and the absence of an `all_smi_up`-style line in this exporter today.
- Bash `set -e` interaction with compound conditionals (`&&`/`||`) in polling loops.

### Related PRs and Issues

- PR #321 (issue #310): added the `launchd-service` CI job this PR fixes, and its own report documents the `ProcessType Background` startup-cost finding this PR quantifies precisely as a CI-timing problem.
- PR #319 (issue #309): added the structurally identical `systemd-service` job, whose weaker assertion currently masks the same class of gate-versus-assertion gap, left unfixed here and recorded as a monitoring item.
- No linked GitHub issue; this PR was filed directly against the `main` CI failure at run 31005178260.

Two product-level findings were reported in this PR's description but deliberately not acted on here (no code changed): (A) `/metrics` answers 200 for 0.3–3.5 seconds before it has any data, which would read as a successful-but-empty Prometheus scrape, an immediately-passing Kubernetes readiness probe, or a premature `SERVICE_RUNNING`/`RunAtLoad` signal to a service manager; (B) the Apple Silicon GPU reader reports `utilization`/`power_consumption`/`temperature` as hard zeros rather than omitting them when the native manager is unavailable, indistinguishable from a genuinely idle GPU. Both are reported for a maintainer to file as separate issues; this report notes them because they were explicitly flagged as out of scope for this CI fix rather than overlooked.

---

## 7. Change Summary

### Statistics

| Item | Value |
|---|---|
| Files changed | 1 (`.github/workflows/ci.yml`) |
| Lines added | +49 |
| Lines removed | -11 |
| Commits | 1 |

### Changes by Category

| Category | Summary |
|---|---|
| CI reliability | Readiness gate now polls for `^all_smi_` content instead of HTTP 200; on exhaustion, dumps the last `/metrics` response for diagnosability |
| CI reliability | New assertion on `all_smi_memory_total_bytes` specifically, the one metric family guaranteed present on an IOReport-less runner |
| CI reliability | Energy-WAL shutdown check and post-uninstall `launchctl print` check both changed from fixed sleeps/single samples to bounded polls |
| Documentation | Two product-level findings (premature 200 response, GPU zero-vs-absent metrics) recorded in the PR description as reporting-only, not implemented |

### Related Commits

| SHA | Type | Message |
|---|---|---|
| `a5e214a8` | fix(ci) | gate the launchd smoke test on metric content, not HTTP 200 |

Merged to `main` as `37d5b56c`. No linked issue.

---

## 8. Follow-up Actions

### Required

None. The fix is verified against the real binary under both normal and background QoS scheduling, at both fine and coarse polling granularity, and the job's own CI run is the only available verification of runner behavior itself (see Appendix A).

### Monitoring Required

- The `systemd-service` job's identical gate shape (section 3.3): currently safe because its assertion has no content check, but it would become exploitable the instant that assertion is strengthened. No action taken in this PR; left as an explicit watch item for whoever next touches that job.

### Future Improvements

- **Finding A (reported, not filed as an issue here)**: emit an unconditional metric line (e.g., an `all_smi_up` or build-info line) so a scrape can distinguish "up but not yet collected" from "up and reporting," which would also have made this PR's own underlying CI failure self-describing from the scrape output alone. Alternatives noted: serve `503` on `/metrics` until the first cycle completes, or delay listener bind until after the first collection returns (most correct, most disruptive to startup latency and to the SCM/launchd readiness latches PR #320 and PR #321 already built).
- **Finding B (reported, not filed as an issue here)**: the Apple Silicon GPU reader should omit `utilization`/`power_consumption`/`temperature` rather than reporting them as zero when the native metrics manager is unavailable, so a dashboard cannot mistake "no data" for "idle GPU."

---

## Appendix

### A. Test Results

- `python3 race.py` against the real binary, 20ms polling, normal and background QoS: produced the measurement table in section 2.1.
- Old gate (HTTP 200): reproduced failing 5/5 at 20ms polling granularity; passing 5/5 at the shipped 2-second granularity.
- New gate (`^all_smi_`): passing 5/5 at 20ms polling granularity with an equal 120-second total budget; 3/3 at 2-second granularity.
- `set -e` safety of all three new loop shapes: verified under `set -eEux` with an `ERR` trap.
- `bash -n` on every `run:` block in the job: clean; YAML parses and the step list is unchanged in shape.
- `all_smi_memory_total_bytes` confirmed present in live `/metrics` output during verification.
- **Not verified in this environment**: the job's actual behavior on the real self-hosted/GitHub-hosted `macos-14` runner beyond this PR's own CI run, since the job only executes on macOS runners and the local measurement, while on real Apple Silicon hardware, is not the CI runner itself.
- No Rust source changed, so no `cargo` checks apply to this diff.

### B. Performance Benchmarks

The measurement table in section 2.1 (empty-body window duration under normal versus background QoS scheduling) is this PR's central quantitative result; it is a latency measurement of the exporter's own startup behavior, not of a benchmark suite, gathered specifically to explain and then fix the CI race.

### C. References

- Failing run: [github.com/lablup/all-smi/actions/runs/31005178260](https://github.com/lablup/all-smi/actions/runs/31005178260).
- `src/api/metrics/render.rs`: the exposition logic gating every metric family on non-empty input, and the `empty_inputs_render_empty_string` test asserting the empty-body behavior.
- `src/device/memory_macos.rs`, `src/device/cpu_macos.rs`, `src/device/readers/apple_silicon_native.rs`, `src/device/readers/chassis/apple_silicon_native.rs`: the four readers traced in section 2.1 to rule out IOReport absence as the cause.
- `src/service_cmd/launchctl.rs`: `BOOTSTRAP_ATTEMPTS`, the existing precedent for polling around a `launchctl` teardown race, which this PR's post-uninstall fix follows.
- PR #321's report: the `ProcessType Background` startup-cost finding this PR measures precisely and turns into a CI fix.
