# Technical Report: PR #426 - macOS Per-Tick Collection Cost

**Date**: 2026-09-20
**Status**: Needs Follow-up
**Languages**: Rust, YAML, Markdown
**Risk Level**: Medium

---

## Executive Summary

PR #426 takes the two expensive Apple Silicon native reads off the every-tick path, overlaps the first tick's two warm-up waits, bounds the per-tick storage capacity refresh, and runs the macOS-only unit tests in CI. On an M1 Ultra the whole steady-state tick falls from 31.85 to 27.67 ms, `all-smi api --interval 1` from 1.2 to 0.63 percent of one core, and time to first data from 554 to 385 ms. Five of the six items in issue #414 landed; item 3, a cheaper process refresh, was measured and deliberately not implemented, because 77 percent of the selective refresh is a `KERN_PROCARGS2` pair sysinfo issues unconditionally and no local bypass reproduces its `cpu_usage` and `status` values. Issue #414 therefore stays open at `status:ready` for that one item.

---

## 1. Problem Statement

### 1.1 Background

#411 (PR #413) cut steady-state CPU at `--interval 1` on an M5 Max from 3.3 to 3.8 percent of one core down to 1.0 to 1.4 percent for `all-smi api`, and from 5.5 to 6.6 percent down to 2.6 to 3.5 percent for `all-smi local`. What remained was work that does not need to run every second. Of the 26.5 ms the issue measured per steady-state tick, `collect_once` was 14.7 ms, of which `IOReportCreateSamples` was 9.07 ms and the SMC read 5.04 ms. The first tick took 270 ms because the CPU warm-up sleep and the IOReport baseline window ran back to back. The per-tick storage capacity refresh ran on the caller's thread with no time bound, and the only unit test job in CI ran on Linux, where none of the macOS-only modules even compile.

### 1.2 Existing Issues

- **A fixed provider cost paid every tick**: `IOReportCreateSamples` does its work per sample, not per channel, so filtering the subscription from 383 channels to 24 moved it only from 10.2 to 8.7 ms. Nothing it feeds needs a fresh window every second: residency is a cumulative counter, and since #410 the energy tracker times each channel by the driver's own publication timestamps.
- **An IOKit round trip per sensor, every tick**: `SmcSampler::collect` read the full temperature set on every call while system power and fans already ran on a 5 s interval.
- **An unmeasured cost inside sysinfo**: on macOS, sysinfo issues `KERN_PROCARGS2` for every refreshed process before it checks whether `exe`, `cmd`, or `environ` need updating, and no `ProcessRefreshKind` disables it. Its share of the selective refresh had never been measured.
- **Two serial warm-up waits at startup**: `MacOsCpuReader` took no sample until the first tick, so its 100 ms wait could not overlap the manager's blocking 100 ms IOReport window.
- **An unbounded refresh on the tick thread**: only `NETWORK_FILE_SYSTEMS` types were skipped, so any other volume that stopped answering stalled the tick in `local`, `api`, and `LocalStorageReader` alike. On Linux a volume unmounted between 30 s list refreshes reported its parent filesystem's capacity for up to 30 s, a case the macOS `f_mntonname` check already rejected.
- **No macOS coverage in CI**: the `macos_native` manager, the macOS `DiskCache` path, and `process_list::priority_macos` never compiled or ran on any runner.

### 1.3 Risk Assessment

| Risk | Impact | Likelihood |
|------|--------|------------|
| A slower sampling cadence silently changes what a displayed value means, without saying so anywhere | Medium | High |
| Reusing the previous window with no bound lets a dead subscription keep reporting its last live residency and power as healthy | High | Low |
| A volume that stops answering stalls every panel because the refresh has no time bound | High | Medium |
| A macOS-only optimization regresses the Linux and Windows collection path it shares | Medium | Medium |
| Bypassing sysinfo to make the process refresh cheaper changes displayed values to buy speed | Medium | High |

---

## 2. Technical Decisions

### 2.1 The IOReport Gate Is 1.75 s and Independent of the Poll Interval

The threshold sits below 2 s by a margin so that the jitter of 1 s ticks cannot stretch the cadence to every third tick, and above 1 s by a margin so that a 1 s poll never samples twice in a row. At `--interval 1` every second tick samples, and at 2 s and above every tick samples as before. The gate is a property of the provider's per-sample cost, not of the UI setting, so it is expressed in wall time rather than in ticks.

### 2.2 Power Resolution at `--interval 1` Halves, Against the Issue's Premise

The issue assumed power would be unaffected, because the energy tracker holds each channel's reading between driver publications. That held on the M5 Max, where the mJ counters publish in batches about 2.1 s apart, slower than a 1 s tick. It does not hold on the M1 Ultra: #415 measured `GPU Energy` publishing every 109 to 139 ms, and the CPU, ANE, and DRAM channels publishing twice per roughly 2.1 s at uneven spans of 0.42 to 1.69 s. At `--interval 1` the tracker had a newer reading on most ticks, and now it is read on every second tick, so power at 1 s is a 2 s value like frequency and residency.

The values stay correct. The tracker divides each counter delta by the driver's own publication span, never by the poll window, so the cadence changes how often a reading is fetched and not what it means. The 20 power samples in the measurement comment show exactly this: each value appears twice, one second apart, and the CPU rail reads 10.54 to 10.90 W against 10.768 W computed from the two `DIE_<n>_CPU Energy` channels over the same window.

### 2.3 The SMC Temperature Interval Is 2.5 s, and the 5 s Full Read Resets It

`SmcSampler` gained `TEMPERATURE_READ_INTERVAL` (2.5 s) next to the existing `SLOW_READ_INTERVAL` (5 s) and repeats the last temperatures in between, as it already did for system power and fans. Because the 5 s full read also reads temperatures and restarts the interval, the resulting pattern at `--interval 1` is ticks 0, 3, 5, 8, 10 and so on, two reads per 5 s, not the every-third-tick pattern the interval alone would give. The first commit's wording said every third tick; the measurement showed 12 reads in 30 ticks, and `f21bcc4` corrected the code comments and `docs/ARCHITECTURE.md` to match. When it does read, it reads the unchanged full sensor set, so the displayed average does not move and a future hottest-sensor reading stays possible.

### 2.4 The Storage Budget Lives Inside `DiskCache`, and Is Paid Once

A timeout at each call site was rejected: a hung call holds the `disk_cache` mutex, so every later tick would park another blocking-pool thread on it. Instead each `DiskCache` owns one worker thread. A tick lends it the listing's `Disks` and the per-volume requests, waits at most `CAPACITY_REFRESH_BUDGET` (50 ms), and reports the previous values with a debug-level trace when the answer does not arrive. One refresh runs at a time, and a result belonging to a replaced listing is discarded by generation.

The budget is also paid once rather than per tick. `overran` is sticky until a refresh completes within the budget, so after an overrun later ticks only check for a result. Without the sticky flag a volume whose `statfs` takes just over the budget alternated a full wait and a free tick, about 25 ms per tick sustained, which is worse than the cost the bound was added to remove.

### 2.5 The Process Refresh Was Measured and Deliberately Left Alone

The issue asked for the `KERN_PROCARGS2` share to be measured first. On this host, with 500 tracked PIDs of 967 processes over 20 rounds:

| measurement | mean |
|---|---|
| sysinfo selective refresh (tracked) | 27.226 ms |
| sysinfo full refresh (all) | 40.675 ms |
| `KERN_PROCARGS2` pair, tracked (343 ok, 142737 bytes copied) | 20.841 ms |
| `KERN_PROCARGS2` pair, all pids | 17.870 ms |
| the three `proc_pidinfo` calls, tracked | 1.924 ms |
| `proc_listallpids` | 0.089 ms |

The `KERN_PROCARGS2` pair is about 77 percent of the selective refresh, which makes it the largest remaining per-tick cost in `local`, and the three `proc_pidinfo` calls that would replace the whole selective refresh cost 1.924 ms, a factor of 14 less. The bypass was still rejected: `proc_pidinfo` cannot reproduce sysinfo's `cpu_usage`, which is task time over sysinfo's own private interval and per-process baselines, nor its `status` source, so displayed values would change on every selective tick, which the issue forbids. The remaining path is an upstream sysinfo guard that skips the call when `name`, `exe`, `cmd`, and `environ` are already known, followed by a version bump here. Issue #414 stays open at `status:ready` for that item alone.

---

## 3. Implementation Details

### 3.1 Manager Cadence, Reuse Bound, and Timings

`collect_once` samples only when `IOREPORT_SAMPLE_INTERVAL` (1750 ms) has passed since the previous full-interval sample and reuses the previous `IOReportMetrics` in between. The gate counts from the first full-interval sample, not from the warm-up window, so the first uncached collection after startup still takes a fresh delta against the baseline that window retained.

A failed sample repeats the previous window only while it is younger than `IOREPORT_REUSE_LIMIT` (10 s, the order of the energy tracker's staleness limit); past that the error propagates and the readers fall back to their documented absence encoding, so a subscription that dies mid-session cannot keep publishing its last live values as if the GPU were still being read. `CollectionTimings` distinguishes the three cases: a reused tick makes no call and reports a zero sample cost, while a failed or too-short sample reports the `IOReportCreateSamples` time it actually paid with `ioreport_sampled = false`, so that cost does not land in `ioreport_parse` and `perf_tick_stages` can average sampled ticks separately. `collect_once` takes `collection_lock` with poison recovery, because the lock guards a unit value and a panic on the detached warm-up thread would otherwise fail every later collection in the process.

### 3.2 First Tick

`ensure_manager` runs the warm-up collection and its blocking 100 ms window on a background thread that the first reader waits for through `collection_lock`, and `initialize_native_metrics_manager` documents that it returns before that work finishes. `MacOsCpuReader::new` takes the first `refresh_cpu_usage` sample and records the instant, so the first `ensure_cpu_refreshed` sleeps only what is left of `CPU_WARM_UP` (100 ms), which is nothing at all when other readers were built in between. Both collectors and `perf_tick_stages` now build the CPU readers before the GPU readers, which is what turns the remainder to zero.

### 3.3 Storage Worker and Linux Mount Identity

The capacity refresh moved into two new modules, `src/storage/disk_cache/capacity.rs` and `src/storage/disk_cache/volume.rs`. The worker ends when its cache is dropped, except that one blocked inside a hung `statfs` outlives it until that call returns, which the module docs state because the API's per-request snapshot collector creates and drops caches. On Linux each volume records its mount point's `st_dev` at list time and keeps its previous values when the device no longer matches, giving the same outcome as the macOS `f_mntonname` check. Linux and Windows per-disk values still come from sysinfo's own refresh, so what they report is unchanged.

### 3.4 macOS CI and Two Runner-Dependent Tests

`macos-unit-tests` runs on `macos-14` with no `needs`, in parallel with the Linux `test` job, reusing the checkout, `setup-protoc` (`osx-aarch_64`), and cargo cache steps of `launchd-service` under its own cache key. It runs `cargo test --lib device::macos_native`, `cargo test --lib storage`, and `cargo test --lib device::process_list`; tests that need real IOReport or SMC hardware detect the VM and skip.

The first run failed two assumptions that only a hosted runner exposes. `reads_the_nice_value_of_a_reniced_child` expected an absolute nice of 7 from `nice -n 7`, but nice is relative to the caller and a hosted macOS runner starts jobs at nice -10, so the child read -3; the test now derives its expectation from this process's own nice plus 7, capped at 20, which is still 7 on a workstation. `a_hung_capacity_refresh_returns_the_previous_values_within_the_budget` failed because the runner handed a 50 ms `recv_timeout` back after 160 ms; wall-clock time on a loaded VM cannot tell a zero wait from a preempted one, so `CapacityWorker` records how long the most recent tick was prepared to wait and the test asserts that instead, keeping the wall-clock bound only as a hang guard.

---

## 4. Measurements on an M1 Ultra

Mac13,2, macOS 27.0 26A428, 960 processes, no compiler running. Four `perf_tick_stages` passes alternating baseline (`6902cbd`) and branch (`1f50e97`), `PERF_TICKS=30`, 18:30 to 18:36, load averages 2.25 to 2.83 throughout. Each column is the mean of its two passes; `top -l 4 -s 8 -pid` samples 2 to 4 of each pass, with `local` in a 160x50 pty.

| per steady-state tick | baseline | this branch |
|---|---|---|
| `collect_once` total | 13.27 ms | 6.76 ms |
| `IOReportCreateSamples` | 10.07 ms every tick | 5.05 ms per tick (10.11 ms on the 15 of 30 sampled ticks) |
| IOReport energy, delta, parse | 1.06 ms | 0.57 ms |
| SMC | 2.12 ms every tick | 1.10 ms per tick (2.75 ms on the 12 of 30 read ticks) |
| storage (`DiskCache::storage_info`) | 0.052 ms | 0.131 ms (worker hand-off) |
| process refresh, selective / full | 13.59 / 27.53 ms | 15.93 / 28.02 ms |
| whole tick | 31.85 ms | 27.67 ms |
| process CPU during ticks | 3.04 % of one core at 1 s | 2.72 % |
| reader construction + first tick | 214.6 + 339.7 = 554.4 ms | 87.1 + 297.8 = 384.9 ms |
| `all-smi api --interval 1`, whole process | 1.2 % of one core | 0.63 % |
| `all-smi local --interval 1`, whole process | 4.2 % of one core | 3.6 % |
| `api`: launch to the first CPU power sample on `/metrics` | 0.609 s mean of 3 | 0.433 s mean of 3 |

The process refresh row is unchanged code, and the spread on this host is 12.8 to 16.4 ms selective, so the difference is host noise rather than a regression. The 15.2 ms `IOReportCreateSamples` quoted in the `manager.rs` module docs comes from an earlier baseline pass at 15:45 under a busier host (load 4.02 to 4.74, 19.67 ms `collect_once`, 46.53 ms whole tick), which is why only same-window alternating passes are compared above.

Against the acceptance criteria: the IOReport per-second cost halves (10.07 to 5.05 ms, sampled on 15 of 30 ticks); the SMC temperature cost falls 2.12 to 1.10 ms, which is 48 percent and therefore just short of the issue's "at least half", because a spaced-out read costs 2.75 ms against the 2.12 ms of an every-tick read; the first tick no longer pays the two warm-up waits serially (31 percent less time to first data); the storage bound costs 0.08 ms per tick.

The #415 power check was re-run at `--interval 1` after the cadence change (release binary, four `yes` loads, 20 samples, 18:38, load 2.86 to 3.93). CPU power read 10.54 to 10.90 W with no 0 W sample, against a tracker mean of 10.75 W over 269 ticks with no zero tick and 10.768 W from the two `DIE_<n>_CPU Energy` channels over the 29.337 s common window. The 20 samples form 10 pairs of identical values, which is the 2 s power resolution of section 2.2 shown directly.

---

## 5. Learning Points

### 5.1 A Cadence Gate Belongs Below the Interval, and a Sibling Timer Reshapes It

A 2 s gate against 1 s ticks is a coin flip: jitter decides whether the second tick is due, and a late tick pushes the cadence to every third one. Setting the gate at 1.75 s makes the pattern deterministic without making the sampling meaningfully more frequent. The second half of the lesson cost a correction: `SmcSampler` already had a 5 s full read that also reads temperatures, so adding a 2.5 s temperature interval did not give every third tick, it gave ticks 0, 3, 5, 8, 10. A new timer next to an existing one produces the interleaving of the two, and only counting the reads in a real run showed which it was.

### 5.2 Halving the Call Count Does Not Halve the Cost

Both cadences were expected to save half. IOReport did, because `IOReportCreateSamples` costs the same whenever it runs. The SMC did not: a read 2.5 s after the last one costs 2.75 ms where an every-tick read costs 2.12 ms, so the per-second cost fell 48 percent rather than 50. The criterion is recorded as just missed rather than met, because the honest number is the one a future reader needs when they decide whether to space the reads further.

### 5.3 A Per-Tick Budget Must Be Paid Once, Not Per Tick

The first version of the storage bound waited 50 ms on every tick while a volume stayed hung, which converts an occasional stall into a permanent 50 ms tax. Making `overran` sticky until a refresh completes within the budget fixes both the hung case and the more awkward one, a volume whose `statfs` takes just over the budget and would otherwise alternate a full wait with a free tick at about 25 ms per tick sustained. A timeout bounds one call; a bound on repeated calls also has to remember that it already paid.

### 5.4 Reuse Must Be Bounded, or Failure Looks Like Health

Repeating the previous window on a failed sample is right for a transient error and wrong for a dead subscription, and the two are indistinguishable at the call site. Without `IOREPORT_REUSE_LIMIT` a subscription that died mid-session would have kept publishing its last live residency, frequency, and power, which is exactly what the readers' absence encoding exists to prevent. Any hold-last-value path needs an expiry that hands control back to the absence encoding.

---

## 6. Change Summary

### Statistics

| Item | Value |
|------|-------|
| Files changed | 15 |
| Lines added | +1572 |
| Lines deleted | -216 |
| Tests added | 18 (manager 5, SMC 4, disk cache 9) |
| New modules | 2 (`storage/disk_cache/capacity.rs`, `storage/disk_cache/volume.rs`) |

### Changes by Category

| Category | Count | Summary |
|----------|-------|---------|
| Performance | 4 | IOReport cadence, SMC temperature cadence, first-tick overlap, bounded storage refresh |
| Correctness | 3 | Bounded window reuse, poison-safe collection lock, Linux mount identity |
| Measurement | 2 | Process refresh bench, `perf_tick_stages` sampled-tick and time-to-first-data rows |
| CI | 1 | `macos-unit-tests` job on `macos-14` |
| Tests | 2 | Storage worker budget tests, two runner-independent test fixes |
| Documentation | 3 | `manager.rs` cadence section, `capacity.rs` module docs, `docs/ARCHITECTURE.md` and `DEVELOPERS.md` |

### Related Commits

| Hash | Type | Message |
|------|------|---------|
| `000e48e` | update | Sample IOReport and SMC temperatures on their own cadence |
| `c61ad36` | update | Bound the per-tick storage capacity refresh |
| `84e20c0` | chore | Run the macOS-only unit tests in CI |
| `4664647` | fix | Make the reniced-child test and the startup order test hold in CI |
| `f21bcc4` | fix | Bound IOReport window reuse and stop paying the storage budget twice |
| `2c330cc` | test | Assert the storage worker's wait length instead of wall-clock time |
| `1f50e97` | fix | Recover a poisoned collection lock and keep the storage budget paid once |
| `133b978` | squash merge | Merge PR #426 into `main` |

---

## 7. Validation and Follow-up

### Completed Validation

- `cargo test --lib device::macos_native`, `cargo test --lib storage`, and `cargo test --lib device::process_list` on the M1 Ultra, plus the full `cargo test --lib` and `--bin all-smi` suites.
- `cargo clippy --lib --tests -- -D warnings`, `cargo fmt --check`, and `cargo test --test user_facing_text_test`.
- The `macos-unit-tests` job green on this PR, after the two runner-dependent tests were fixed.
- Four alternating `perf_tick_stages` passes with `top` for `api` and `local`, and the process refresh bench, all in section 4.
- The #415 power check re-run at `--interval 1` after the cadence change, cross-checked against the energy diagnostic and the raw `DIE_<n>_CPU Energy` channels.

### Not Verified

- Linux and Windows performance. This host cannot run them, so the issue's "no Linux regression" criterion is unmeasured rather than met. The storage worker is shared with both, though their per-disk values still come from sysinfo's own refresh, and the Linux mount-identity unit test runs in the Linux CI `test` job.
- The first-tick overlap in `local`. It was measured in `perf_tick_stages` and in the `api` binary; in `local` it is verified by construction only, because its first frame is a TUI paint rather than a scrapeable value.
- Chips other than this M1 Ultra. The M5 Max figures in the issue were not re-measured, and the publication cadences that make the power resolution chip-dependent are known for two chips only.

### Required Follow-up

- Issue #414 stays open at `status:ready` for acceptance criterion 3 alone. The work is an upstream sysinfo guard that skips `KERN_PROCARGS2` when `name`, `exe`, `cmd`, and `environ` are already known, then a sysinfo version bump here. It is the largest remaining per-tick cost in `local`, about 20.8 ms of a 27.2 ms selective refresh.
- Linux and Windows measurement of the shared storage path, whenever a host is available.

### Remaining Constraints

- At `--interval 1` on Apple Silicon, frequency, residency, and power are 2 s values shown twice, and SMC temperatures are read twice per 5 s. This is documented in `manager.rs`, `smc.rs`, and `docs/ARCHITECTURE.md`, and it is a resolution change, not an accuracy change.
- A capacity worker blocked inside a hung `statfs` outlives the `DiskCache` that spawned it until that call returns.
- A local `cargo check --target x86_64-unknown-linux-gnu` fails in `aws-lc-sys` for lack of a Linux C cross-compiler, which is unrelated to this change but is why the Linux path could not be checked locally.

### References

- [PR #426](https://github.com/lablup/all-smi/pull/426)
- [Issue #414](https://github.com/lablup/all-smi/issues/414)
- [Measurement evidence in the PR comments](https://github.com/lablup/all-smi/pull/426#issuecomment-5749013571)
- Issue #410 and PR #412: per-channel publication timing, which is what makes a 1.75 s sampling gate safe for power
- Issue #415 and PR #424: the M1 Ultra channel inventory and the publication cadences quoted here
- Issue #411 and PR #413: the previous round, whose remaining floor this PR addresses
