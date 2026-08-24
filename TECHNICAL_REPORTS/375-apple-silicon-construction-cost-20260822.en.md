# Technical Report: PR #375 - Cut Apple Silicon Client Construction from 1.4s to 0.28s

**Date**: 2026-08-22  
**Status**: Completed  
**Related**: PR #375, Issue #374  
**Risk Level**: Medium (changes process-global manager lifetime and SMC key discovery)

---

## Executive Summary

`AllSmi::with_config` blocked for over a second on Apple Silicon before returning, and all of it was in `get_gpu_readers` into `NativeMetricsManager::new`. Issue #374 identifies three independent causes; each is fixed independently here.

On an Apple M5 Max (Mac17,7, macOS 26.6.2 25G83) the first construction drops from **1.28-1.54s to 273-285ms**, and each subsequent one from **275-281ms to 131-136ms**. A fourth defect found during the work, where dropping one client disabled the others, is fixed in the same PR.

---

## 1. Problem Statement

Three costs stacked in one constructor:

- `discover_temperature_keys` scanned all 3,739 keys of an M5 Max SMC table at two IOKit round trips each, because its early exit required *both* the CPU and the GPU category to saturate a shared 64-key cap, and no real Mac saturates both. Measured: 23 CPU keys, 84 GPU keys, against a cap of 64.
- `IOReport::new` issued three `IOReportCopyChannelsInGroup` calls back to back and merged them, on every construction.
- The shared 64-key cap truncated the GPU sensor set, so the reported temperature averaged whichever 64 sensors the key table happened to list first.

## 2. Change Summary

| Item | Value |
|------|-------|
| Files changed | 7 |
| Lines added | 852 |
| Lines deleted | 88 |
| New integration test file | `tests/library_api_test.rs` |

## 3. Technical Decisions

### 3.1 SMC key discovery binary-searches a span instead of walking the table

The key table is ordered by FourCC, and every candidate name starts with `T`, so all candidates occupy one contiguous span. Discovery now binary-searches for the start of that span and walks only to its end, and skips the second round trip (`read_key_info`) for any name that cannot possibly classify.

That is **373 indices read instead of 3,739**, and **1.05s becomes 80ms**.

### 3.2 Ordering is checked, not assumed

Nothing short of reading the whole table proves it is sorted, so the fast path takes the two cheap checks available and backstops them:

- the indices the binary search already read must be non-decreasing,
- the key below the span must sort below it,
- a span that yields no sensors at all is treated as a failed assumption rather than as a machine with no sensors.

Any of those falling through runs the exhaustive scan, which remains the authority on what the table contains and is itself cheaper than before thanks to the name pre-filter. `the_span_search_finds_what_the_full_scan_finds` asserts on real hardware that the two paths return identical keys.

### 3.3 Channel enumeration runs concurrently and once per process

The three `IOReportCopyChannelsInGroup` calls are independent reads combined only afterwards, so they now run concurrently (137ms to 62ms measured). Because the channel set a machine exposes is fixed by its hardware, the merged dictionary is cached process-wide. Each subscription takes its own `CFDictionaryCreateMutableCopy`, so the cached description stays pristine.

The existing asymmetry is preserved deliberately: the Energy Model group stays fatal, CPU and GPU stats stay tolerable as absent. Two new distinctions were added rather than collapsed:

- A panicking enumeration thread is reported distinctly from a group that simply does not exist on the host.
- A thread the OS refuses under resource pressure falls back to running that query inline rather than panicking out of `AllSmi::with_config`, whose contract is to report failure through `Result`.

### 3.4 Dropping one client no longer disables the others

`AllSmi::drop` called `shutdown_native_metrics_manager`, which took the process-global manager out of its slot regardless of who else held it. A second client that had not yet touched its readers then found no manager and reported every live GPU field as unavailable. Reproduced before the fix:

```
a:                                    util=Some(0.61)  power=Some(0.0085)
b (first ever use, after a dropped):  util=Some(-1.0)  power=Some(-1.0)   <- GPU_METRIC_UNAVAILABLE
```

The manager now counts owning handles: `AllSmi` acquires one and releases it on drop, and teardown happens when the last one goes. `shutdown_native_metrics_manager` keeps its unconditional process-exit semantics for the binary, which does not participate in the count. As a side fix, the collector-thread join now runs outside the singleton lock rather than under it.

### 3.5 The GPU key cap becomes per-category and stops truncating

An M5 Max exposes 84 `Tg*` sensors against 23 `Tp*` and `Te*` ones, so the old shared cap of 64 truncated the GPU set. Both caps are now sized above real hardware and act as runaway guards, which makes the reported temperature a function of the sensors rather than of key ordering.

**This is the opposite direction from the smaller `Tg*` cap the issue suggests**, and the reasoning is worth recording. The issue's stated concern is that the value is "diluted across whatever happens to be enumerated"; an arbitrary first-N subset is what causes that. Removing the truncation addresses the concern directly, while lowering the cap would keep the value order-dependent. Measured cost of covering all 84 sensors instead of 64 is about 0.3ms per read.

## 4. Measurements

Release build, `default-features = false`, on an Apple M5 Max. `origin/main` and this branch were built into separate worktrees and run **interleaved in the same session**, three rounds, so thermal drift affects both sides equally.

| | before (`origin/main`) | after |
|---|---|---|
| first `AllSmi::with_config` | 1.280s / 1.518s / 1.536s | 273ms / 275ms / 285ms |
| subsequent `with_config` | 275-281ms | 131-136ms |
| `discover_temperature_keys` | 933ms / 953ms / 1.271s | 77ms / 80ms / 83ms |
| key-table indices read | 3,739 of 3,739 | 373 of 3,739 |
| `IOReportCopyChannelsInGroup` x3 | 137-169ms (sequential) | 62ms (concurrent), then cached |
| discovered `Tg*` sensors | 64 (truncated from 84) | 84 |
| **reported GPU temperature** | **53.73 / 53.56 / 53.38 C** | **53.63 / 53.45 / 53.34 C** |

The GPU temperature column is the acceptance criterion on the `Tg*` cap. The three readings fall monotonically on both sides because the machine was cooling across the run; the before-and-after gap at each step (0.10 / 0.11 / 0.04 C) is smaller than the drift between consecutive steps, so the cap change is not what moves it.

**The 9-11s baseline in the issue did not reproduce on this hardware.** Measured `IOReportCopyChannelsInGroup` here is about 45ms per call, not the roughly 2.5s the issue reports, so the same three calls cost 137ms rather than 7.5s. The components the issue identifies are real and are exactly what these numbers move, but the absolute starting point on this machine was 1.4s, not 9-11s. Worth confirming against the original reporting environment.

## 5. Tests

| Test | What it pins |
|------|--------------|
| `dropping_one_client_leaves_another_working` | The two-live-clients regression. The second client is deliberately left untouched until after the first is dropped, because a client whose readers already cached the manager kept working by accident, and warming it first would hide the bug |
| `a_client_built_after_the_last_drop_still_works` | The same lifetime from the other side |
| `discovery_stops_before_the_end_of_the_key_table` | The scanned-key count is below the table size |
| `the_span_search_finds_what_the_full_scan_finds` | Fast path and exhaustive fallback return identical keys on real hardware |
| `channel_enumeration_happens_once_per_process` | Pointer identity across two `merged_channels()` calls, which is direct evidence rather than a timing assertion |
| `each_subscription_copies_the_cached_channels`, `two_subscriptions_can_coexist` | The cache is never handed to a subscription itself |
| `name_prefilter_accepts_everything_the_classifier_can`, `every_candidate_name_sorts_inside_the_scanned_span` | The two invariants the fast path rests on |

Hardware-dependent tests skip cleanly where the SMC or IOReport is unreachable (Intel Mac, VM, sandboxed runner).

## 6. Outcome and Follow-up

- PR #375 was squash-merged into `main` as `e08b658`.
- Issue #374 closed automatically through the PR's `Closes #374` link.
- `docs/LIB_mode.md` now records the measured construction cost on this hardware and states that overlapping instances are safe. It previously told consumers to reuse an instance without saying what reuse saves, and said nothing about overlap.
- `tests/library_api_test.rs` was added here and became the first integration-test target any clippy job linted, once #373's `--all-targets` step landed.
- The issue's 9-11s baseline remains unconfirmed on the original reporting environment.

---

## Appendix: Key Terms

| Keyword | Description | Relevance |
|---------|-------------|-----------|
| FourCC | Four-character code, here the SMC key name | Its ordering is what makes a binary search valid |
| `IOReportCopyChannelsInGroup` | IOKit call enumerating a channel group | Three sequential calls per construction, now concurrent and cached |
| handle counting | Releasing a shared resource when the last owner drops | The fix for one client's drop disabling another |
| runaway guard | A bound sized above real hardware, meant never to bind | What the per-category key caps became |
