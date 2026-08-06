# Technical Report: PR #338 - fix(test): measure publish cost, not sleep overshoot, in the SSE test

**Date**: 2026-08-06
**Status**: Completed
**Languages**: Rust
**Risk Level**: Low (test-only change; no `src/` file touched)

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

`fifty_concurrent_clients_do_not_stall_the_publisher` claimed to prove that `FrameBus::publish` is non-blocking, but its measurement loop reset a timestamp *after* `publish` returned and took the next delta *before* the following call, so the measured interval spanned only `tokio::time::sleep`'s overshoot plus scheduler noise. The duration of `bus.publish()` itself never entered any measurement. The proof this PR leads with is not a description of the flaw but a demonstration of it: with a 5 ms stall injected directly inside `FrameBus::publish`, roughly 5000x its real cost, the old assertion **still passed**, which is direct, empirical evidence the test was vacuous rather than merely fragile. Issue #327 was filed after the test failed once for real (run 30994446017, an observed 108.5 ms against a 90 ms budget under two overlapping CI jobs) and then passed 5/5 locally under load, a pattern consistent with a flaky timer, not with a broken publish path, which is exactly what the stall-injection proof confirms directly rather than by inference.

The fix splits one conflated assertion into two properties, each independently budgeted and independently named in its failure message. Property A (cadence) times the whole 20-tick loop against twice its nominal duration, deliberately budgeting the loop rather than each tick so a single descheduled tick (the CI flake's actual shape) cannot fail the run while a genuine cadence collapse still does. Property B (publish cost) times `publish` itself directly, over a burst of 200 calls with all 50 subscribers attached and reading nothing so the 16-slot broadcast ring is overwritten many times over, exactly the state a backpressuring implementation would have to wait in. Two details are load-bearing and documented as such: the 1 ms spacing between burst calls sits outside the timed window specifically so the SSE tasks have time to return to `recv()` and register a waker between publishes, since back-to-back calls measure a ~1 µs path with nothing to wake and would make the subscriber count, the whole point of the test, nearly invisible to the measurement; and the budget is asserted on the mean of 200 samples rather than the max, because a wall-clock max cannot distinguish a blocked publish from the OS descheduling the measuring thread mid-call, the same noise that produced the original flake. The strongest evidence for that second choice is a real one: one instrumented run absorbed a single 14 ms deschedule inside a timed window and still reported a 218 µs mean, 4.6x inside the 1 ms budget, a run any max-based budget under 14 ms would have failed. Total: 1 file (`tests/sse_events_test.rs`), +141/-24, one commit, closing #327.

---

## 1. Problem Statement

### 1.1 Background

`fifty_concurrent_clients_do_not_stall_the_publisher` (`tests/sse_events_test.rs`) exists to protect a specific property from issue #193: `FrameBus::publish` must not block on a slow or absent SSE subscriber. The test attaches 50 concurrent SSE clients, publishes 20 snapshots at a fixed interval, and was supposed to fail if publishing stalled.

### 1.2 Existing Issues

- **Issue 1 (the measurement window excluded the thing being measured)**: the loop set `last = Instant::now()` *after* `bus.publish(snapshot).await` returned, then computed the next iteration's `delta` as `t0 - last`, where `t0` was taken *before* that iteration's `publish` call. `delta` therefore spanned exactly the gap from the end of one publish to the start of the next, which is `sleep(publish_interval)` plus scheduling overhead, never the duration of `publish` itself.
- **Issue 2 (the failure message misattributed the measurement)**: the assertion read `"tick jitter exceeded {tick_budget:?} ... publish is supposed to be non-blocking"`, naming `publish` as the property under test while the value being compared had structurally never included any part of a `publish` call's duration.
- **Issue 3 (the flaw is not merely theoretical)**: with a 5 ms stall injected directly inside `FrameBus::publish` in a controlled reproduction, roughly 5000x the function's real cost, the old assertion still passed, which is direct proof the test could not have caught the property it claimed to protect.
- **Issue 4 (the budget, even for what it did measure, was tight on a shared runner)**: `publish_interval` was 50 ms and `tick_budget` was `publish_interval + 40ms`, leaving 40 ms of slack for `tokio::time::sleep` overshoot and OS scheduling on a two-core hosted runner simultaneously holding 50 concurrent SSE clients; `sleep` guarantees only "at least" the requested duration, not a bound on overshoot, so this budget was measuring runner contention more than anything about the application.

### 1.3 Risk Assessment

| Risk | Impact | Likelihood |
|---|---|---|
| A genuinely blocking `publish` implementation ships undetected, because the test that should catch it structurally cannot | High if it occurred: the property (non-blocking SSE publish under load) is exactly what issue #193 required and this test's name claims to guard | Latent for as long as this test existed in its original form; demonstrated directly by the stall-injection proof rather than left as a theoretical gap |
| The new mean-based publish-cost budget masks a real intermittent stall by averaging it away | Low: a publish that "genuinely waits pays its cost on every single call" per the PR's own reasoning, so a real blocking implementation fails the mean budget by roughly the same margin regardless of averaging, while an isolated descheduling artifact (the 14 ms case) does not | Addressed directly; the mean-vs-max trade-off is the PR's own central design decision (section 3.2) |
| The cadence assertion's widened per-loop budget (2x nominal) hides a real, smaller cadence regression | Low-medium: a regression smaller than the budget's slack would not be caught by this specific test | Accepted trade-off; the PR frames cadence as a coarse "not catastrophically broken" check, with publish cost as the precise measurement for the property that actually matters here |

---

## 2. Technical Review

### 2.1 Correctness

The fix's correctness rests on two facts about what `FrameBus::publish` actually does, stated and then verified rather than assumed: it bumps an `AtomicU64`, wraps the snapshot in one `Arc`, takes the `latest` write lock (uncontended in this test, since no `/snapshot` request runs concurrently), and calls `broadcast::Sender::send`. Tokio's broadcast channel applies no backpressure: once the 16-slot ring is full, `send` overwrites the oldest slot and a lagging receiver learns about the gap through `RecvError::Lagged` on its next `recv()`, rather than the sender blocking. So `publish` is O(1) work plus waking whichever receivers are currently parked in `recv()`, and structurally never waits on a client. Measured at 25–35 µs per call with 50 subscribers attached, this places the chosen 1 ms budget roughly 35x above the measurement and, per the PR's reasoning, "at least an order of magnitude below the failure mode" a blocking implementation would produce (a scheduler round trip per subscriber per call, milliseconds at best, and no return at all for these specific clients, which read nothing).

The most direct correctness check the PR performs on itself is a mutation test: re-inject the same 5 ms stall used to demonstrate the old test's vacuity, and confirm the *new* assertion fires. It does, with the message `"publish cost: FrameBus::publish averaged 6.809984ms per call over 200 calls with 50 subscribers attached and reading nothing (1.361996845s spent inside publish in total), over the 1ms per-call budget"`, both proving the new test can fail on the exact defect the old one could not, and demonstrating the failure message names the measured quantity precisely, addressing issue #327's acceptance criterion that a message must not attribute a timer overshoot to `publish` blocking.

### 2.2 Performance

Not applicable to the application; this is a test-only change. The test's own runtime cost is unchanged in shape: 20 cadence ticks at ~50 ms nominal (about 1 s) plus a 200-call burst spaced 1 ms apart (about 200 ms), so total wall time is comparable to before, with the burst's actual measured work occupying only a few milliseconds of that ~200 ms.

### 2.3 Compatibility and Dependencies

- **Breaking changes**: none; `tests/sse_events_test.rs` is the only file touched, and no `src/` behavior changes.
- **New dependencies**: none.
- **Compatibility**: not applicable beyond the test suite itself.

### 2.4 Code Quality

The burst measurement is wrapped in a `timeout(burst_hang_guard, ...)` (5 seconds, "25x the nominal burst, far too loose to fire on scheduling noise"), so an implementation that awaited a receiver would fail the test with a diagnosable message rather than hanging the CI job until the runner's own timeout kills it, a failure mode the old test had no defense against either. The subscriber-count assertion (`subscriber_count() >= 50`) is preserved and reworded slightly for a clearer message, rather than dropped as redundant, since it is a precondition for property B to be measuring what it claims to measure (fifty parked wakers per publish). The comment block above the test is rewritten in full to describe exactly the two assertions present, replacing the old comment's now-inaccurate description; issue #327 explicitly required this ("the comment block ... matches the assertions actually present").

---

## 3. Technical Decisions

### 3.1 Split one conflated assertion into two independently budgeted properties

**Context**: the original test had one assertion doing two jobs implicitly: catching a catastrophic cadence failure and (supposedly) catching a blocking `publish`. Neither job was done correctly, and conflating them into one number made the failure message misleading about which property actually broke.

| Option | Pros | Cons |
|---|---|---|
| Fix the single assertion's timing bug in place (measure `publish` inside the same loop, keep one budget) | Smallest diff | Still conflates two different phenomena (scheduler slop across a whole loop, versus the cost of one function call) under one number and one message; a future reader still cannot tell which property a failure indicates |
| **Chosen: two separate assertions, Property A (cadence, loop-scoped) and Property B (publish cost, per-call, mean-based)** | Each has its own budget derived from its own reasoning and its own failure message naming what it measured; a cadence regression and a publish-blocking regression fail distinguishably | More code in the test (a second loop, a burst-specific timeout, more constants), but each piece is independently simpler to justify than the one conflated assertion was |
| Drop the cadence check entirely, keep only publish-cost | Simplest | Loses the coarse "the loop as a whole did not collapse" signal, which is a different and still useful property from "no single publish call blocked" |

**Rationale**: the issue's own acceptance criteria required both a direct measurement of `publish`'s duration *and* that any remaining loop-period check be explicitly labeled as measuring scheduling slop rather than `publish` behavior. Splitting the assertion is the only way to satisfy both without one property's budget compromising the other's precision.

### 3.2 Budget the publish-cost assertion on the mean of 200 samples, not the max

**Context**: a wall-clock maximum is the more intuitive choice for "did any single publish block," but the original CI flake (run 30994446017) was itself a single-sample timing artifact under runner contention, exactly the kind of noise a max-based budget is most sensitive to.

**Decision**: assert `mean_publish <= mean_publish_budget` (1 ms), and report `worst_publish` in the failure message purely as a diagnostic, never as a trigger.

**Rationale, stated directly in the PR and confirmed by a real run rather than only argued**: a wall-clock max cannot distinguish a blocked `publish` call from a measuring thread the OS simply descheduled mid-call; both produce an outlier sample. Averaging over 200 samples amortizes an isolated descheduling event while a publish that genuinely blocks pays its cost on every single call in the burst, so the mean converges to the real cost either way, isolated noise or systematic blocking, in opposite directions. The empirical confirmation: one of eight instrumented local runs absorbed a single 14 ms deschedule inside a timed window and still reported a 218 µs mean, 4.6x inside the 1 ms budget; any max-based budget tighter than 14 ms would have failed that specific, otherwise-healthy run.

### 3.3 Keep the 1 ms inter-call spacing outside the timed window, and treat it as load-bearing rather than incidental

**Context**: publishing 200 frames back-to-back with no spacing would be a simpler burst loop and would still exercise `publish` under 50 attached subscribers.

**Decision**: insert `sleep(BURST_SPACING)` (1 ms) between calls, outside the `Instant::now()`/`.elapsed()` window that measures each `publish`.

**Rationale**: the SSE tasks need to actually return to `.recv()` and re-register a waker between publishes for a call to pay the cost of waking fifty parked receivers, which is the specific cost this property exists to measure. Publishing back-to-back instead measures a structurally cheaper path, roughly 1 µs versus the ~28 µs measured with spacing, because no receiver has re-parked and there is nothing to wake; the PR states plainly that this "would make the subscriber count, the whole point of the test, nearly invisible to the measurement." The spacing itself sits outside the timed window specifically so it inflates the burst's total wall time (about 200 ms) without inflating any individual measured sample.

---

## 4. Implementation Details

### 4.1 Architecture Changes

```
[Before]
for _ in 0..20 {
    let t0 = Instant::now();
    bus.publish(snapshot).await;
    let delta = t0 - last;        // spans previous publish's END to this publish's START
    last = Instant::now();        // set AFTER publish returns
    sleep(publish_interval).await;
}
assert!(max_delta <= tick_budget, "... publish is supposed to be non-blocking");
    -- measures sleep overshoot + scheduler noise; never measures publish() itself

[After]
// Property A: cadence (loop-scoped, coarse, explicitly labeled as scheduling slop)
let cadence_start = Instant::now();
for _ in 0..TICKS {
    bus.publish(load_snapshot()).await;
    sleep(publish_interval).await;
}
assert!(cadence_start.elapsed() <= cadence_budget, "... This measures sleep overshoot and runtime scheduling slop across the loop, not the cost of a single publish...");

// Property B: publish cost (per-call, mean-based, precise)
for _ in 0..BURST {
    sleep(BURST_SPACING).await;      // outside the timed window; lets wakers re-register
    let t0 = Instant::now();
    bus.publish(snapshot).await;
    costs.push(t0.elapsed());        // measures publish() itself, and only publish()
}
assert!(mean(costs) <= mean_publish_budget, "publish cost: FrameBus::publish averaged {mean:?} per call ...");
```

### 4.2 Key Code Changes

**File: `tests/sse_events_test.rs` (Property B, the corrected measurement)**
```rust
let burst = timeout(burst_hang_guard, async {
    let mut costs: Vec<Duration> = Vec::with_capacity(BURST as usize);
    for _ in 0..BURST {
        let snapshot = load_snapshot();
        sleep(BURST_SPACING).await;
        let t0 = Instant::now();
        bus.publish(snapshot).await;
        costs.push(t0.elapsed());
    }
    costs
})
.await;
```
**Reason for change**: `t0` is now taken immediately before the call being measured and `.elapsed()` immediately after, so the timed window is exactly `publish`'s own duration; the previous version's window covered the interval between two different calls instead.

**File: `tests/sse_events_test.rs` (the assertion and its failure message)**
```rust
assert!(
    mean_publish <= mean_publish_budget,
    "publish cost: FrameBus::publish averaged {mean_publish:?} per call over {BURST} calls with {subscribers} subscribers attached and reading nothing ({total_publish:?} spent inside publish in total), over the {mean_publish_budget:?} per-call budget. Publish is a non-blocking broadcast send and should cost tens of microseconds no matter how far behind the subscribers are. Slowest single call was {worst_publish:?}, reported for diagnosis only: an isolated slow sample is usually the OS descheduling the measuring thread, which is why the budget is on the mean."
);
```
**Reason for change**: the message states what was measured (`publish` itself, not a loop period), the sample size, the subscriber count, and reports the max only as a diagnostic with an explicit note on why it is not the trigger, directly satisfying issue #327's "no message attributes a timer overshoot to publish blocking" criterion.

### 4.3 Data Model Changes

Not applicable; no source code, wire format, or metric definition changed.

---

## 5. Learning Points

### 5.1 A test that has never failed is not evidence it measures the right thing

**Concept**: a passing test suite proves the tested property held under the conditions exercised, not that the assertion is capable of detecting a violation of that property. The strongest way to check the latter is a mutation test: deliberately break the property under test and confirm the assertion fails.

**Application in this PR**: the PR's central proof is exactly this: inject a 5 ms stall into `publish`, 5000x its real cost, and observe the old assertion pass anyway. This is stronger evidence than code review alone that the original test was vacuous, not merely imprecise.

### 5.2 The timing window of a measurement has to align exactly with the operation being measured, not merely occur nearby it in the same loop iteration

**Concept**: `Instant::now()` calls placed near an operation, but not immediately bracketing it, can produce a duration that is dominated by unrelated work (a sleep, a scheduler yield) rather than the operation itself, while still looking like a direct measurement to a reader who does not trace the exact bracketing.

**Application in this PR**: the original bug is precisely this: `last` was updated after `publish` returned, and the next `delta` used a `t0` from before the *next* call, so the "measured" interval was the gap between two calls, not either call's own duration.

### 5.3 Choosing mean versus max for a latency budget is itself a decision about what kind of noise the budget should tolerate

**Concept**: a max-based budget is sensitive to any single outlier, whatever its cause (a real block, or the OS descheduling the measuring thread); a mean-based budget over enough samples is robust to isolated non-representative outliers while still failing reliably against a systematic cost paid on every call.

**Application in this PR**: the PR chooses mean specifically because the original flake's own root cause (scheduler noise producing a single bad sample) is the exact failure mode a max-based budget cannot distinguish from a real defect, and confirms the choice with a real run that absorbed a 14 ms deschedule and still passed comfortably on the mean.

---

## 6. Further Learning

### Key Terms

| Keyword | Description | Relevance |
|---|---|---|
| `tokio::sync::broadcast` | Tokio's multi-producer, multi-consumer broadcast channel, backpressure-free (overwrites the oldest slot when full) | Why `FrameBus::publish` is structurally non-blocking regardless of subscriber count, the property this test protects |
| Mutation testing | Deliberately introducing a defect to confirm a test can detect it | The technique behind this PR's central proof (the 5 ms stall injection) |
| Mean vs. max latency budget | Two different statistics for the same latency distribution, tolerating different kinds of noise | The PR's explicit, empirically-justified choice of mean over max (section 3.2) |
| Timed window bracketing | Placing `Instant::now()`/`.elapsed()` calls exactly around the operation under measurement, with no unrelated work inside the bracket | The precise defect this PR fixes; the old code's bracket spanned the wrong interval |
| `RecvError::Lagged` | The signal a `broadcast` receiver gets when it fell behind and the sender overwrote unread slots | What happens instead of the sender blocking, which is why "publish never blocks" is a real, provable property here |

### Related Technologies and Frameworks

- Tokio's `broadcast` channel semantics, specifically its no-backpressure, overwrite-oldest-slot behavior under a full ring buffer.
- Server-Sent Events (SSE) as the consumer of `FrameBus`, and the concurrency model (50 simultaneous subscriber tasks) this test exercises.

### Related PRs and Issues

- Issue #327: the issue this PR closes.
- Issue #193: the original spec ("collection tick jitter stays within ±20 ms" under 50+ concurrent SSE clients) this test's comment block referenced before this PR, and which this PR's rewritten comment reconciles with the two assertions actually present.
- The PR body separately notes `snapshot_error_response_carries_no_cache_headers` as a test named for a path it does not exercise, left unfixed deliberately to keep this diff narrow; not itself the subject of an issue at merge time.

---

## 7. Change Summary

### Statistics

| Item | Value |
|---|---|
| Files changed | 1 (`tests/sse_events_test.rs`) |
| Lines added | +141 |
| Lines removed | -24 |
| Commits | 1 |

### Changes by Category

| Category | Summary |
|---|---|
| Test correctness | Replaced a vacuous non-blocking-publish assertion (proven vacuous by mutation testing) with two independently budgeted properties: loop cadence and per-call publish cost |
| Test reliability | Publish-cost budget uses the mean of 200 samples rather than a wall-clock max, confirmed by a real run to tolerate a 14 ms scheduling outlier while still failing on injected blocking |
| Test safety | Added a `timeout` guard around the burst so a genuinely blocking implementation fails with a message instead of hanging the CI job |
| Documentation | Comment block above the test rewritten to describe the two assertions actually present, reconciling it with issue #193's original spec |

### Related Commits

| SHA | Type | Message |
|---|---|---|
| `1ea4ba6b` | fix(test) | measure publish cost, not sleep overshoot, in the SSE test |

Merged to `main` as `a56707a6`. Closes #327.

---

## 8. Follow-up Actions

### Required

None identified as blocking.

### Monitoring Required

- None specific to this PR; it is a test-only change with no production code path affected.

### Future Improvements

- `snapshot_error_response_carries_no_cache_headers`, noted in the PR body as named for an error path it does not actually exercise (its own comment is candid about this, unlike the defect this PR fixes), is left alone deliberately to keep this diff narrow; a future PR could address it the same way.

---

## Appendix

### A. Test Results

- `cargo test --test sse_events_test` (11/11), run three times.
- `cargo test --test sse_events_test fifty_concurrent_clients_do_not_stall_the_publisher`, 8 consecutive instrumented runs, all passing: mean publish cost 24.6, 25.4, 27.1, 31.0, 31.8, 32.7, 35.0, and 218.1 µs (the last absorbing a 14 ms deschedule inside a timed window and still 4.6x inside the 1 ms budget). Cadence measured 1.035–1.041 s against the 2 s budget across all runs.
- Mutation check: re-injecting the 5 ms stall in `FrameBus::publish` makes the new assertion fail with `"publish cost: FrameBus::publish averaged 6.809984ms per call over 200 calls ..."`, while the old assertion (verified separately) passed under the identical injected stall. The injection was removed before committing; no `src/` file is touched by this PR.
- `cargo fmt --check`: clean.
- `cargo clippy --lib --tests -- -D warnings`: clean.
- `cargo clippy --bin all-smi -- -D warnings`: clean.

### B. Performance Benchmarks

The core quantitative result of this PR: `FrameBus::publish` measured at 25–35 µs per call with 50 subscribers attached and reading nothing, against a 1 ms mean-based budget (roughly 35x headroom). This is a latency measurement of the application's own publish path, produced specifically to derive and validate the new test's budget, not a general-purpose benchmark suite result.

### C. References

- Issue #327: root cause narrative (the exact line numbers of the original bracketing bug), scope, and acceptance criteria this report draws from, cross-checked against the diff.
- CI run 30994446017: the original flake (108.5 ms observed against a 90 ms budget under two overlapping jobs) that prompted the issue.
- Issue #193: the original SSE tick-jitter specification this test's comment block is reconciled against.
