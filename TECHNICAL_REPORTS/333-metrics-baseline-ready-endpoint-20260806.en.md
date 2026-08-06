# Technical Report: PR #333 - fix(api): always emit a metrics baseline and add /-/ready

**Date**: 2026-08-06
**Status**: Completed
**Languages**: Rust
**Risk Level**: Low (additive exposition change plus one new route; no existing metric name, type, or label removed)

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

`/metrics` answered `200 OK` with a byte-empty body from the moment axum bound the listener until the first collection cycle landed, because the exposition renders straight from `AppState` and every exporter self-filters. A Prometheus scrape landing in that window is recorded as a *successful* scrape with zero samples, a silent gap in the series rather than a failed target, so it never alerts. Issue #324 laid out three non-exclusive fixes: delay the `200` behind the first cycle, expose a separate readiness signal, or always emit at least one line. PR #333 combines the second and third and explicitly rejects the first: a new `all_smi_up`/`all_smi_build_info` baseline now precedes every device family in the exposition, unconditionally, and a new `GET /-/ready` route answers `503` with `Retry-After: 1` until the first cycle completes and `200` afterward. `/metrics` itself keeps answering `200` for its entire lifetime, so no existing scraper's status-code handling changes.

The more consequential decision is what the PR does *not* change: `mark_serving()`, the call that lets the Windows SCM report `SERVICE_RUNNING` and the launchd/systemd latches open, stays at bind rather than moving behind the first collection cycle. The reasoning, now recorded in `src/api/shutdown.rs`, rests on three points verified against the code rather than asserted: the SCM has no readiness concept, only a liveness one (`SERVICE_START_PENDING`/`SERVICE_RUNNING`); the inconsistency issue #324 was worried about is fixed at the other end, since `/metrics` now carries a defined, non-empty body the instant the latch opens; and `src/service_cmd/scm_host.rs` reports `StartPending` exactly once with a 10-second wait hint (`TRANSITION_WAIT_HINT_SECS`, `src/service_cmd/scm.rs:70`) and a checkpoint that never increments, so moving the latch behind a slow first collection (cold WMI plus NVML enumeration on a many-GPU or wedged-driver host) risks the SCM failing the start and restarting into the same slow path, a boot loop on exactly the hosts where the telemetry matters most. A drive-by fix rides along: the man page's `API ENDPOINTS` section documented a `/health` endpoint that has never existed in the router; it now lists the real surface, including `/-/ready`. Total: 15 files, +932/-23, two commits, closing #324.

---

## 1. Problem Statement

### 1.1 Background

`all-smi api` exposes Prometheus-format metrics from `AppState`, populated by a background collection loop (`src/api/collection_loop.rs`) that runs its first pass some time after the HTTP listener binds. Every exporter in `src/api/metrics/render.rs` self-filters: a device family with no data contributes nothing to the response body. Before this PR that included every family in the exposition, so an all-empty `AppState` rendered a byte-empty string, and `render_prometheus_exposition`'s own module doc and the test `empty_inputs_render_empty_string` asserted exactly that as intended behavior.

### 1.2 Existing Issues

- **Issue 1 (a successful scrape can carry zero samples)**: `/metrics` answers `200 OK` with zero bytes from listener bind until the first collection cycle writes into `AppState`. Prometheus records a scrape in that window as successful with no samples, a silent gap in the series that does not alert, unlike a failed target which does.
- **Issue 2 (no yes/no signal for orchestrators)**: a Kubernetes `readinessProbe` or a load balancer health check pointed at `/metrics` passes the instant the listener binds, before there is anything to serve, because the endpoint's only observable signal was its status code.
- **Issue 3 (the Windows SCM latch opens onto an undefined response)**: `src/api/latch.rs` documents the serving latch as opening "once a listener is bound," and `mark_serving()` is called immediately after a successful bind (`src/api/shutdown.rs`), so `SERVICE_RUNNING` could be reported while `/metrics` was still serving zero bytes.
- **Issue 4 (no unconditional line in the exposition)**: no metric family in the chain was emitted regardless of data, so a consumer had no in-band way to distinguish "up but not yet collected" from "up and reporting nothing because there is nothing to report."
- **Issue 5 (documentation drift)**: the man page's `API ENDPOINTS (API MODE)` section documented `/health` as returning `"OK"`. `src/api/server.rs` never mounted such a route; only `/metrics`, `/events`, and `/snapshot` existed before this PR.

### 1.3 Risk Assessment

| Risk | Impact | Likelihood |
|---|---|---|
| A scrape or a readiness probe landing in the pre-collection window is treated as healthy | Medium (a silent gap in a time series, or a Kubernetes pod marked ready before it has real data) | Certain prior to this fix, on every process start |
| Moving `mark_serving()` behind the first collection cycle to "fix" the window at its source | High if done: `scm_host.rs`'s single `StartPending` report with a 10 s wait hint and a checkpoint that never increments means a slow first cycle (cold WMI/NVML enumeration on a many-GPU host) would make the SCM fail the start and restart into the same slow path | Avoided by this PR's decision to leave `mark_serving()` at bind; documented as a considered-and-rejected option rather than an oversight |
| `all_smi_up` and `/-/ready` computed independently and allowed to disagree | Medium (a consumer gating on one and alerting on the other would see a contradiction with no way to resolve it) | Avoided by construction: both read the single predicate `api::handlers::ready::is_ready`, and a test asserts they never disagree across the transition |

---

## 2. Technical Review

### 2.1 Correctness

The exposition change is additive at the rendering level. `ExporterStatusMetricExporter` (`src/api/metrics/exporter_status.rs`, new) is the one exporter in the chain that never self-filters; it is prepended to `render_prometheus_exposition`'s output ahead of every device family, so a consumer reading only the head of the response, or scraping before the first cycle lands, still sees `all_smi_up` and `all_smi_build_info`. Every device exporter is untouched and keeps self-filtering, verified by `empty_inputs_render_only_the_baseline_families`, which asserts that an all-empty input set renders nothing beyond the two baseline families.

The readiness predicate is a single function, `api::handlers::ready::is_ready(state: &AppState) -> bool`, defined as `!state.loading`. Both `/-/ready`'s status code and the `all_smi_up` gauge's value are computed by calling this function against the same `AppState` read, which is what makes disagreement structurally impossible rather than merely unlikely: `metrics_handler` passes `ready: is_ready(&state)` into `MetricsRenderInputs` under the same lock acquisition that reads the device data, and `ready_handler` calls the identical function. `tests/api_readiness_test.rs`'s `ready_endpoint_and_up_gauge_never_disagree` asserts this against the live router on both sides of the transition, not just at the renderer level, which specifically catches the class of bug where a handler forgets to pass `ready` or a route is not mounted.

The snapshot serializer (`src/snapshot/serializers/prometheus.rs`) hard-codes `ready: true`, since `snapshot --format prometheus` only reaches that code after a synchronous collection has already returned. Byte-identical parity between a live scrape and a one-shot snapshot, asserted by `prometheus_output_is_byte_identical_to_api_exporter_for_same_data`, is preserved by matching that same `ready: true` in the parity test's own inputs.

### 2.2 Performance

`ExporterStatusMetricExporter::export_metrics` calls `get_hostname()`, which reads a process-wide `Lazy<String>` rather than performing a syscall per scrape, so the added cost per request is two more `MetricBuilder` entries (six labels total between the two families) ahead of the existing device exporters. No new background work, locking, or polling is introduced; `/-/ready` reuses the same `AppState` read lock the `/metrics` handler already takes.

### 2.3 Compatibility and Dependencies

- **Breaking changes**: none in the wire sense. Every existing metric family, its name, type, and labels are unchanged. The only behavioral change to an existing consumer is that `/metrics`'s body is no longer byte-empty during the startup window; a scraper that specifically depended on an empty body (none is known to) would see different content, not a different status code.
- **New dependencies**: none.
- **Compatibility**: `/-/ready` is a new, additively mounted route (`src/api/server.rs`); nothing that previously resolved now 404s. The man page correction removes a documented-but-nonexistent `/health` endpoint rather than removing a working feature.

### 2.4 Code Quality

`tests/api_readiness_test.rs` is new and drives the real axum router over a loopback TCP connection with raw HTTP/1.1 requests (not a test-only mock), specifically because a renderer-level test cannot catch a handler that forgot to wire `ready` through or a router that never mounted `/-/ready`. Its seven cases cover: `/metrics` is `200` and non-empty before the first collection; `all_smi_up` transitions `0` then `1` across the transition; an `^all_smi_`-prefixed line exists from the very first request (the exact pattern PR #323's CI workaround polled for, now satisfied unconditionally); `/-/ready` is `503` with `Retry-After: 1` and `Cache-Control: no-store` before, `200` after; the endpoint and the gauge never disagree across the transition; and an unmounted neighbor path (`/-/healthy`) still `404`s, guarding against someone "fixing" the unusual `/-/` prefix with a wildcard route. `render.rs`'s and `prometheus.rs`'s test suites were updated in a second commit after the first one broke `empty_snapshot_renders_empty_string` in CI (run 31100471191), which the scoped `--lib api::` and `--lib snapshot::serializers` filters used during development did not reach; the fix superseded that assertion with `empty_snapshot_still_renders_the_baseline`, pinning the corrected contract rather than only patching the failure.

---

## 3. Technical Decisions

### 3.1 Combine a separate readiness endpoint with an in-band baseline, and reject delaying the 200

**Context**: issue #324 offered three non-mutually-exclusive options: (1) delay the `200` until the first collection cycle has populated `AppState`, which would also move the Windows SCM `SERVICE_RUNNING` transition and any `readinessProbe` behind real readiness; (2) expose a separate readiness signal such as `/ready`, leaving `/metrics` semantics untouched; (3) always emit at least one line so a scrape in the window is distinguishable from a scrape of a host with no devices.

| Option | Pros | Cons |
|---|---|---|
| Option 1: delay the 200 | Simplest mental model: not ready means not answering | Couples `/metrics`'s status code to internal readiness, which breaks any existing scraper or health check that treats a slow-to-start `/metrics` as a failed target; also moves the SCM/latch transition behind the same slow path (see 3.2) |
| Option 2 alone: `/-/ready` only | Purpose-built yes/no signal for orchestrators | A plain `/metrics` scrape in the window is still a silent zero-sample gap for anyone not polling the new endpoint |
| Option 3 alone: baseline metric only | Makes every scrape self-describing | No dedicated yes/no gate for a `readinessProbe` or load balancer, which would still have to infer readiness from `all_smi_up`'s value rather than a status code |
| **Chosen: Option 2 + Option 3, Option 1 rejected** | `/metrics` never changes its status-code contract, so no existing scraper is affected; a scrape-only consumer gets `all_smi_up`; an orchestrator gets a dedicated gate at `/-/ready` | Two surfaces to keep in sync, resolved by having both read one predicate (`is_ready`) rather than computing readiness twice |

**Rationale**: the two chosen options answer different questions for different consumers, and combining them costs nothing beyond keeping them backed by the same predicate. Rejecting option 1 specifically preserves the existing `/metrics` contract (`200` from bind onward) that PR #323's CI workaround, and any external scraper, already depend on, and avoids re-coupling the SCM's liveness transition to hardware-enumeration latency (section 3.2).

**Trade-offs**: consumers who want a true black-or-white gate must know to look at `/-/ready` rather than `/metrics`; the in-band `all_smi_up` gauge is a weaker signal for automation (a PromQL query rather than a status code) but is the only signal visible to a plain scrape.

### 3.2 `mark_serving()` stays at bind rather than moving behind the first collection cycle

**Context**: issue #324's evidence section specifically called out that the Windows SCM readiness latch (added by PR #320, issue #311) opens "once a listener is bound" and asked whether that should change now that a real readiness signal exists.

**Decision**: it does not move. Three reasons, each recorded in the `mark_serving` doc comment in `src/api/shutdown.rs`:

1. **They are different questions.** The SCM's state machine offers `SERVICE_START_PENDING` and `SERVICE_RUNNING`; `SERVICE_RUNNING` is a liveness verdict, not a readiness one, and the natural liveness boundary for a network exporter is "the listener answers." This is the same liveness/readiness split Kubernetes and the Prometheus ecosystem already use, and readiness is now separately queryable by anyone who needs it.
2. **The inconsistency is fixed at the other end.** Before this PR the latch opened onto an endpoint serving zero bytes, so `SERVICE_RUNNING` promised nothing. Now the instant the latch opens, `/metrics` carries a defined, non-empty response (`all_smi_up 0` plus build info). Giving `/metrics` a floor resolves the mismatch; delaying the latch is not needed to resolve it a second time.
3. **Moving it has a worse failure mode than the one it prevents.** `src/service_cmd/scm_host.rs` reports `StartPending` exactly once, with `wait_hint = TRANSITION_WAIT_HINT_SECS` (10 s, `src/service_cmd/scm.rs:70`) and `checkpoint: 0`. Because the checkpoint never increments, that single report is the entire start budget the SCM grants. A first collection cycle on Windows means cold COM/WMI initialization plus NVML enumeration, which on a many-GPU host or behind a wedged driver can exceed 10 s. The SCM would fail the start and apply the configured recovery actions, restarting into the same slow path: a boot loop on exactly the hosts where the telemetry matters most.

**Trade-off accepted**: a Windows service can report `SERVICE_RUNNING` while `all_smi_up` is still `0`. This is judged correct rather than merely tolerated, because it is now an honestly-reported state (`all_smi_up 0` is visible) rather than a silently empty one.

### 3.3 Encode the readiness predicate once and pass it through the render inputs, rather than reading `AppState` a second time inside the renderer

**Context**: `render_prometheus_exposition` and its `MetricsRenderInputs` struct are also used by the snapshot serializer, which has no live `AppState` to lock at render time.

| Option | Pros | Cons |
|---|---|---|
| Have the renderer accept an `&AppState` and compute `is_ready` internally | One less field on `MetricsRenderInputs` | Forces every caller, including the snapshot path with no live state, to fabricate or lock an `AppState`; couples the pure rendering function to the live-server type |
| **Chosen: add `ready: bool` to `MetricsRenderInputs`, computed by each caller** | The live handler and the snapshot serializer each supply the value appropriate to their own execution model; the renderer stays a pure function of its inputs | Callers must remember to pass the right value; mitigated by `tests/api_readiness_test.rs` exercising the live handler specifically |

**Rationale**: keeping `render_prometheus_exposition` a pure function of `MetricsRenderInputs` is what lets `prometheus_output_is_byte_identical_to_api_exporter_for_same_data` assert byte-for-byte parity between the API path and the snapshot path in the first place; threading a live `AppState` reference through it would have broken that symmetry for no benefit, since the snapshot path never has one.

---

## 4. Implementation Details

### 4.1 Architecture Changes

```
[Before]
AppState (empty until first cycle)
    │
    ▼
render_prometheus_exposition(inputs)  -- every exporter self-filters
    │
    ▼
"" (byte-empty body, 200 OK)

[After]
AppState.loading  ──────────────┐
    │                            │  is_ready(&state)
    ▼                            ▼
metrics_handler            ready_handler
    │                            │
    ▼                            ▼
MetricsRenderInputs{ready}   /-/ready: 503 (Retry-After: 1) | 200
    │
    ▼
render_prometheus_exposition(inputs)
    │
    ├─ ExporterStatusMetricExporter (unconditional: all_smi_up, all_smi_build_info)
    └─ every device exporter (still self-filters)
    │
    ▼
non-empty body, 200 OK, always
```

### 4.2 Key Code Changes

**File: `src/api/handlers/ready.rs` (new; the single readiness predicate)**
```rust
/// [`AppState::loading`] starts `true` and is cleared by
/// [`crate::api::collection_loop::run_collection_loop`] at the end of its
/// first iteration, which is exactly the transition being described. It is
/// never set back to `true` on the API path.
pub fn is_ready(state: &AppState) -> bool {
    !state.loading
}

pub async fn ready_handler(State(state): State<SharedState>) -> Response {
    let ready = is_ready(&*state.read().await);
    readiness_response(ready)
}
```
**Reason for change**: one function backs both halves of the contract (the `/-/ready` status code and the `all_smi_up` gauge), so the two cannot compute readiness independently and drift apart.

**File: `src/api/shutdown.rs` (the decision to leave `mark_serving()` at bind)**
```rust
/// Third, moving it has a concrete failure mode that is worse than the
/// one it would prevent. `crate::service_cmd::scm_host` reports
/// `StartPending` exactly once, with `wait_hint =
/// TRANSITION_WAIT_HINT_SECS` (10 s) and `checkpoint: 0`. Because the
/// checkpoint never increments, that single report is the entire start
/// budget the SCM grants. A first collection cycle on Windows means cold
/// COM/WMI initialization plus NVML enumeration, which on a many-GPU
/// host or a wedged driver can exceed 10 s. The SCM would then fail the
/// start and apply the configured recovery actions, restarting the
/// process into the same slow path...
pub(crate) fn mark_serving() {
    serving_latch().trigger();
}
```
**Reason for change**: this comment is the artifact of the decision in section 3.2; it exists so a future contributor revisiting the "SCM reports running before there is data" question does not have to rediscover the `scm_host.rs` timing constraint from scratch.

**File: `src/api/metrics/render.rs` (the baseline is prepended, not appended)**
```rust
// Baseline first (issue #324), so a consumer that reads only the head
// of the response, or scrapes before the first collection cycle has
// landed, still learns whether this exporter is up and which build it
// is. Everything below this line self-filters; this block does not.
let status_exporter = ExporterStatusMetricExporter::new(inputs.ready);
all_metrics.push_str(&status_exporter.export_metrics());
```
**Reason for change**: ordering is deliberate; a consumer truncating the response body (or a human skimming it) sees the baseline before any device family.

### 4.3 Data Model Changes

Not a schema change. `MetricsRenderInputs` gains one field, `pub ready: bool`. Internally, `AppState::loading` (pre-existing) is now read by a named predicate (`is_ready`) instead of being consulted ad hoc, and that predicate is the single source both `/-/ready` and the `all_smi_up` gauge use.

---

## 5. Learning Points

### 5.1 Liveness and readiness are different questions, and a service-management API that only has one of them should not be forced to answer both

**Concept**: liveness answers "is the process alive and not stuck," readiness answers "does the process have real work to serve." Kubernetes models these as two separate probes for exactly this reason; a service manager that only exposes a liveness-shaped state machine (like the Windows SCM's `SERVICE_RUNNING`) should not have a readiness condition smuggled into it, because doing so couples an orchestrator's restart policy to how long the readiness condition takes to become true.

**Application in this PR**: `mark_serving()`/`SERVICE_RUNNING` stayed a liveness signal; `/-/ready` and `all_smi_up` became the readiness signal, kept entirely separate. The alternative, folding readiness into the SCM transition, would have made a slow hardware enumeration into a service-manager-triggered restart loop.

### 5.2 A pure rendering function with all its inputs threaded through an explicit struct is what makes cross-path parity testable

**Concept**: when the same output has to be produced by two different code paths with different execution models (a live async handler versus a synchronous one-shot collector), keeping the shared logic a pure function of an explicit input struct, rather than a function that reaches into ambient state, is what lets a test assert the two paths produce identical output for identical inputs.

**Application in this PR**: `render_prometheus_exposition(&MetricsRenderInputs)` stayed pure by adding `ready: bool` as an explicit field rather than accepting `&AppState`. `prometheus_output_is_byte_identical_to_api_exporter_for_same_data` is the test this choice keeps possible.

### 5.3 A single predicate function is a stronger correctness guarantee than "these two things should agree"

**Concept**: two independently computed values that are supposed to represent the same fact can drift the moment one code path changes and the other does not. Routing both through one function eliminates the class of bug rather than merely testing against it.

**Application in this PR**: `is_ready(state: &AppState) -> bool` is called by both `metrics_handler` (to set `all_smi_up`) and `ready_handler` (to set the `/-/ready` status code), under the same lock acquisition in the metrics path. `ready_endpoint_and_up_gauge_never_disagree` is a test of this property, not the mechanism that guarantees it; the mechanism is that there is only one function to call.

---

## 6. Further Learning

### Key Terms

| Keyword | Description | Relevance |
|---|---|---|
| Liveness vs. readiness | Two distinct health questions: is the process alive, versus does it have real data | The split this PR draws between `mark_serving()`/SCM liveness and `/-/ready`/`all_smi_up` readiness |
| `all_smi_up` | New unconditional gauge, 0 before the first collection cycle, 1 after | The in-band half of the readiness contract, visible to any plain scrape |
| `all_smi_build_info` | New unconditional gauge, always 1, content in its labels (`version`, `os`, `arch`) | Follows the `node_exporter`/Prometheus build-info idiom for joining version onto other series |
| `GET /-/ready` | New out-of-band readiness route | The dedicated yes/no gate for orchestrators, following the Prometheus-ecosystem `/-/` convention |
| `mark_serving()` / serving latch | Existing primitive (PR #320/#321) that lets the Windows SCM and launchd/systemd report the process as running | Deliberately left at bind rather than moved behind readiness (section 3.2) |
| `TRANSITION_WAIT_HINT_SECS` | The SCM's 10-second wait hint for a pending start, reported exactly once | The concrete constraint that ruled out moving `mark_serving()` |

### Related Technologies and Frameworks

- Prometheus exposition conventions: the `/-/ready`, `/-/healthy` path prefix used by Prometheus, Alertmanager, and the Pushgateway; the `*_build_info` constant-`1` idiom used by `node_exporter` and Prometheus itself.
- Windows Service Control Manager (SCM) state machine: `SERVICE_START_PENDING`, `SERVICE_RUNNING`, wait hints, and checkpoints.
- Kubernetes liveness/readiness probe semantics, referenced as the model this PR's split follows.

### Related PRs and Issues

- Issue #324: the issue this PR closes.
- PR #323: the CI workaround (`grep -q '^all_smi_'`) that this PR's unconditional baseline makes obsolete; PR #335 migrates that CI job onto `/-/ready`.
- PR #320 (Windows SCM) and PR #321 (launchd): added the serving latch and `mark_serving()` this PR deliberately leaves unchanged.
- PR #335: migrates both the systemd and launchd smoke tests from content-based gating onto `/-/ready`, and is the PR that actually depends on this one's readiness contract.

---

## 7. Change Summary

### Statistics

| Item | Value |
|---|---|
| Files changed | 15 |
| Lines added | +932 |
| Lines removed | -23 |
| Commits | 2 |
| New files | `src/api/handlers/ready.rs`, `src/api/metrics/exporter_status.rs`, `tests/api_readiness_test.rs` |

### Changes by Category

| Category | Summary |
|---|---|
| API | New `GET /-/ready` route; new unconditional `all_smi_up`/`all_smi_build_info` baseline prepended to every `/metrics` response |
| Documentation | `mark_serving()` decision recorded in `src/api/shutdown.rs` and `src/api/latch.rs`; README.md and API.md gain a "Readiness and the Startup Window" section; man page's nonexistent `/health` entry replaced with the real endpoint list |
| Tests | New `tests/api_readiness_test.rs` (7 integration tests against the live router); `src/api/metrics/render.rs` and `src/snapshot/serializers/prometheus.rs` test suites updated to assert the baseline rather than an empty string |
| Compatibility | No existing metric name, type, or label changed; `/metrics`'s status-code contract (200 from bind) is unchanged |

### Related Commits

| SHA | Type | Message |
|---|---|---|
| `612a7a01` | fix(api) | always emit a metrics baseline and add /-/ready |
| `4c3a8fa4` | test | supersede the snapshot serializer's empty-output assertion |

Merged to `main` as `5f2fa816`. Closes #324.

---

## 8. Follow-up Actions

### Required

None identified in the PR. The contract is verified against the live binary (section on live verification below) and pinned by integration tests running against the real router rather than only the renderer in isolation.

### Monitoring Required

- Behavior under the Windows SCM is derived from reading `scm_host.rs` (the single `StartPending` report, the 10 s wait hint, the checkpoint that never increments) rather than from a live SCM run in this PR; the PR itself notes this and that no code on that path changed, so it is unchanged behavior with newly documented rationale rather than a new, unverified claim.
- The launchd and systemd smoke jobs still gated on metric content as of this PR; PR #335 migrates them onto `/-/ready`.

### Future Improvements

- None proposed in the PR beyond the migration already planned for PR #335.

---

## Appendix

### A. Test Results

- `cargo test --test api_readiness_test`: 7 passed.
- `cargo test --lib api::`: 113 passed.
- `cargo test --test snapshot_test`: 13 passed, including `prometheus_output_is_byte_identical_to_api_exporter_for_same_data`.
- `cargo clippy --lib --tests -- -D warnings` and `cargo clippy --bin all-smi -- -D warnings`: both clean, run separately because the crate compiles its module tree twice and PRs #319/#320/#321 were each bitten by a `pub` item live in the library target and dead in the binary target.
- `cargo fmt --check`: clean.
- Live binary, pre-first-collection window raced deliberately: `/metrics` returned `HTTP 200` with 399 bytes instead of zero, containing `all_smi_up{...} 0` and `all_smi_build_info{...,version="0.25.0",os="macos",arch="aarch64"} 1`. `/-/ready` in the same window returned `503 Service Unavailable` with `retry-after: 1` and `cache-control: no-store`. After the cycle landed, `/-/ready` returned `200 OK` with `all-smi is ready.` and `/metrics` reported `all_smi_up ... 1`.

### B. Performance Benchmarks

Not separately benchmarked. The added per-request cost is two more label sets rendered ahead of the existing exporters; no new locking or background work is introduced.

### C. References

- Issue #324: root cause narrative, the three-option scope, and acceptance criteria this report draws from, cross-checked against the diff.
- `src/service_cmd/scm_host.rs`, `src/service_cmd/scm.rs`: the SCM timing constraint (`TRANSITION_WAIT_HINT_SECS`, `checkpoint: 0`) that decided section 3.2.
- Prometheus, Alertmanager, and Pushgateway `/-/ready` conventions.
