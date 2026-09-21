# Technical Report: PR #432 - macOS Process Metrics Through proc_pidinfo

**Date**: 2026-09-21
**Status**: Needs Follow-up
**Languages**: Rust, Markdown
**Risk Level**: Medium

---

## Executive Summary

PR #432 stops calling sysinfo's process refresh on four of every five ticks of the macOS local collector and reads per-process CPU percent, memory, state and run time through `proc_pidinfo` instead. sysinfo still runs on the fifth, full tick to discover processes and supply their static metadata, after which the new sampler reads every PID, so every row's CPU percent is its own task-time delta over its own elapsed time. On an M1 Ultra the selective process refresh falls from 16.28 to 4.24 ms, the whole steady-state tick from 28.87 to 20.90 ms, and `all-smi local --interval 1` from 3.70 to 2.53 percent of one core; the full tick rises from 31.67 to 35.18 ms because it now pays for both readers. The PR closes #427 and #414: it implements #414's last open item, which PR #426 measured and deliberately did not implement. It also fixes a second sysinfo defect found along the way: a process refreshed only every fifth tick read about five times its real CPU percent on that tick, and that inflated value decided which 500 processes the table showed.

---

## 1. Problem Statement

### 1.1 Background

PR #426 took the IOReport and SMC reads off the every-tick path and left the process refresh as the largest per-tick cost in `local`. Its standalone bench (500 tracked PIDs of 967, 20 rounds) measured the selective refresh at 27.2 ms, of which 20.8 ms was a pair of `KERN_PROCARGS2` sysctls, while the three `proc_pidinfo` calls that carry the values the collector keeps took 1.9 ms. PR #426 still rejected a `proc_pidinfo` bypass: it could not reproduce sysinfo's `cpu_usage` (task time over sysinfo's private interval and per-process baselines) or the source of sysinfo's `status`, so displayed values would have changed on every selective tick. It named an upstream sysinfo guard plus a version bump as the remaining path, and #414 stayed open for that one item. Issue #427 reopened the question with the sysinfo 0.39.6 source in hand.

The collector refreshes the tracked top `MAX_DISPLAY_PROCESSES` (500) PIDs every tick and every PID every `FULL_REFRESH_INTERVAL` (5) ticks. This PR keeps that schedule and changes which code does the reading on each kind of tick.

### 1.2 Existing Issues

- **An argument copy that no refresh kind turns off**: in sysinfo 0.39.6, `update_process` (`src/unix/apple/macos/process.rs:750`) calls `get_process_infos` for every refreshed process, and `get_process_infos` (`:539-648`) issues two `KERN_PROCARGS2` sysctls that copy the whole argument and environment area before it checks whether `exe`, `cmd` or `environ` need updating (`:630-641`). No `ProcessRefreshKind` disables it, and the collector discarded every byte of the copy. #426's bench put it at about 20.8 ms per selective tick. Linux and Windows read the command line only when the refresh kind asks for it.
- **One global interval for every process**: `get_time_interval` (`src/unix/apple/macos/system.rs:113-152`) derives `time_interval` from the CPU ticks elapsed since the previous refresh call, and `compute_cpu_usage` (`process.rs:285-306`) divides each process's task-time delta by it. A process outside the tracked set is refreshed only on the full tick, so on that tick its five-tick delta is divided by a one-tick interval. The ignored test `sysinfo_inflates_untracked_processes_on_a_full_refresh` reproduced it with a `yes` child: 100.21 percent, then 499.53 on the full tick, then 100.07. That value is the sort key for the displayed top 500 in `local_collector.rs`, so unrelated processes could jump onto the screen every fifth tick, and the inflated ranking then chose the next tick's tracked set. An upstream guard against the `KERN_PROCARGS2` copy, the path #426 proposed, would not have fixed this.
- **A CPU percent that does not return to zero**: `compute_cpu_usage` assigns `cpu_usage` only when the task-time delta is positive (`process.rs:297-302`, unchanged in upstream master), so a process that used CPU once and then went idle kept its last non-zero value. A child that burned 1.44 percent for one second read 1.44 for the next five idle seconds; `photolibraryd` read 33.78 percent for a second in which its counter did not move.
- **A reused PID kept the previous process's identity**: `update_process_cache` matched rows by PID alone and refreshed only the dynamic fields, so a PID taken over by a new process kept the old process's name, user and command.

### 1.3 Risk Assessment

| Risk | Impact | Likelihood |
|------|--------|------------|
| The native reader changes displayed values nobody meant to change, which is why #426 rejected it | Medium | Medium |
| Without an explicit `ESRCH` check, an `EPERM` read (349 of 985 processes for a normal user) is taken for an exit and live rows vanish | High | High |
| Unless the sampler reports exits, a dead tracked PID stays on screen, since sysinfo no longer prunes on selective ticks | Medium | High |
| New `unsafe` FFI reads an uninitialized or partly written buffer | High | Low |
| The shared collector code regresses the Linux or Windows process path | Medium | Low |

---

## 2. Technical Decisions

### 2.1 Why the Bypass Ships When PR #426 Said It Could Not

#426's objection was that a bypass would change displayed values on every selective tick. #432 answered it with measurements on the same M1 Ultra (macOS 27.0 26A428, 985 processes, load 2.1 to 3.3), from a probe and from the ignored test `sampler_matches_sysinfo_across_the_process_table`:

- **`status` is reproduced exactly.** sysinfo's macOS status is `pbi_status` as read when the process was first seen, except that `SRUN` is replaced by the state of thread id 0 from `PROC_PIDTHREADINFO` (`R` when that call fails, which it did for 371 of 636 inspectable processes), and `?` for a process whose BSD info was unreadable when first seen (`new_empty`). The sampler applies the same rule to the same fields, and 637 of 637 inspectable PIDs matched. The 349 `EPERM` processes keep sysinfo's `?` because their rows keep sysinfo's values.
- **Memory, virtual memory and run time come from the same kernel fields.** rss matched for 636 of 637 (the exception changed between two reads milliseconds apart), vms for 637 of 637, and run time within one second for 637 of 637.
- **CPU percent matches for the processes on screen.** sysinfo's `time_interval` and the sampler's elapsed mach time both measure wall time. Over the same one-second window: `yes` 100.30 against 99.98, `sharingd` 4.14 against 4.14, `rapportd` 2.03 against 2.02, `ghostty` 1.40 against 1.40, `WindowManager` 0.59 against 0.58. The mean absolute difference was 0.02 to 0.09 points over the 55 to 100 PIDs whose counters moved, and the differences above 0.5 points belonged to processes whose load changed between the two windows, such as the test binary itself (1.57 against 3.67).

Three displayed behaviors change on purpose. Each one is a fix:

1. **Untracked processes are no longer inflated on the full tick.** Through the real `collect_steady_state` path the new build reads the `yes` child at 99.94 percent on the full tick and 99.94 one second later. Matching sysinfo here would mean keeping the defect.
2. **CPU percent no longer sticks when a counter does not move.** Such a row now reads 0. In one probe run 28, 29 and 46 of 639 inspectable processes carried a stale sysinfo value in three consecutive one-second windows.
3. **A reused PID rebuilds its row.** The full tick compares the start time and rebuilds the row from sysinfo's metadata; a selective tick drops the row until the next full tick rediscovers it.

None of the three invents a value. A PID's first sighting still has no CPU reading, as with sysinfo.

Two notes on the figures. The PR body gives the stale-value count as "28 to 49 of 636", which commit `88163d6` identified as one probe run's range combined with the other run's denominator; the single-run figure above is the corrected one now in the `sampler_macos.rs` module docs. Those module docs also quote a different `yes` run of the same shape (98.87, 501.87, 100.11); the sequence in section 1.2 is the PR body's run from the ignored test.

### 2.2 Two Kinds of Tick, Two Paths

On a **selective tick** sysinfo is not called for processes at all. The sampler reads the tracked PIDs, and the cache is walked directly: a tracked row takes its sample's values, leaves when the sampler reports the process gone or its start time changed, and keeps its values when the kernel will not answer. Untracked rows keep the values of the last full tick, as they did before. sysinfo's process map is deliberately not walked, because it still holds every tracked PID that has died since the last full tick, and walking it would put those rows back.

On a **full tick** sysinfo refreshes everything, because it is still what discovers new processes and supplies name, user, parent, start time and command. The sampler then reads every PID sysinfo holds, and each row's dynamic fields come from its sample. That second pass is the cost of removing the inflation: the full tick rises by the 3.90 ms sampler pass while the four selective ticks each save about 12 ms. Replacing sysinfo's discovery with a native `proc_listallpids` pass could remove most of the remaining 31 ms, but #427 put it out of scope as a possible later step.

### 2.3 CPU Percent Over the Sampler's Own Elapsed Time

The sampler keeps, per PID, `pti_total_user + pti_total_system` and the `mach_absolute_time` at which it was read, and reports the task-time delta over that PID's own elapsed time. Both counters are in mach absolute time units, so the ratio needs no timebase conversion. A PID with no baseline has no reading (`None`), as sysinfo has none for a process it has just discovered; a counter that did not move reads `Some(0.0)`; no elapsed time reads `None` (`checked_sub`), and a counter that went backwards reads 0 (`saturating_sub`). Because the interval is per PID, it no longer matters whether a PID was sampled one tick ago or five.

### 2.4 `ESRCH` Means Gone, Everything Else Means Unreadable

For a normal user the kernel answers `proc_pidinfo` for about two thirds of the process table and returns 0 bytes with `EPERM` for the rest. The issue's implementation notes had already flagged that 0 bytes is not an exit. `pidinfo_macos::read` clears `errno` before the call and reads it back immediately after, and returns `PidInfoError::Gone` only for `ESRCH`. Every other failure (`EPERM`, a short write, a PID outside `c_int`) is `Unreadable`, which the refresh treats as "keep what you had". A `Gone` sample removes the row on a selective tick. On a full tick, a PID that sysinfo listed but the sampler then finds gone is dropped at once instead of one tick later.

### 2.5 One Entry Point, Used by Everything That Measures

`process_list::refresh_processes` is the only per-tick process pass on macOS. The collector's steady state, its first iteration, `tests/perf_tick_stages.rs` and `local_collector/tests.rs` all call it, so the measurements and tests run the shipped path instead of an inline replica. #427 proposed a macOS-only extra parameter on `update_process_cache`; the implementation instead added `refresh_macos.rs` with separate full and selective cache walks that take the samples map directly, which, like #427's proposal, avoids a post-pass over the cache. On Linux and Windows the old inline code moved unchanged into a `cfg(not(target_os = "macos"))` `process_pass`, and `update_process_cache` is compiled only there.

---

## 3. Implementation Details

### 3.1 `pidinfo_macos`: One Read, Shared

`read::<T>(pid)` zero-fills a `MaybeUninit<T>`, passes its exact size to `proc_pidinfo`, and calls `assume_init` only when the kernel reports writing exactly that many bytes. `T` must implement a sealed `PidInfo` trait implemented for `proc_taskinfo`, `proc_bsdinfo` and `proc_threadinfo` only, which keeps the zero-then-trust pattern limited to plain-data structs, and each struct carries its flavor constant. `PROC_PIDTHREADINFO` is always called with thread id 0, as sysinfo does, because the point is to reproduce sysinfo's status column. `priority_macos::base_priority` now calls the same function, so the two `unsafe` blocks it used to hold for its own `proc_pidinfo` read moved here.

### 3.2 `sampler_macos`: Baselines and the State Rule

`ProcessSampler::sample` takes a PID iterator and returns a `Sampled` per PID: `Live(ProcessSample)`, `Unreadable` or `Gone`. `Readings::read` makes the task-info call first and skips the BSD and thread calls when it fails, which is why an `EPERM` process costs one failed call and an inspectable one three. `Readings` is kept apart from `fold`, so the unit tests can drive the arithmetic without a kernel. The baseline holds the start time, the `pbi_status` from the first sighting (fixed for the life of the PID, as sysinfo's `process_status` is), the task time, and the sample instant. A different `pbi_start_tvsec` under the same PID discards the baseline, and `Gone` removes it. `state_code` maps an unreadable first sighting to `?`, `SRUN` to the thread-0 state (`R`, `S` or `T`, `?` for any other thread state, and `R` when the thread call failed), and `SIDL`, `SSLEEP`, `SSTOP`, `SZOMB` to `I`, `S`, `T`, `Z`, the same letters `convert_process_state` produces. Run time is epoch seconds minus `pbi_start_tvsec`, the clock sysinfo's `run_time()` uses.

### 3.3 `refresh_macos`: Folding Samples Into the Cache

`refresh_processes` runs a full tick when asked or when the tracked set is empty, and a selective tick otherwise, and returns `ProcessRefreshTimings` (sysinfo, sampler, cache) for `perf_tick_stages`. `update_cache_full` walks sysinfo's map, rebuilds a row whose cached start time differs from sysinfo's, and applies the sample where there is one; an unreadable row takes sysinfo's values as before (memory 0, state `?`). `update_cache_selective` walks the cache with `retain`. `apply_sample` leaves CPU percent alone when the sample has no reading, so a first sighting keeps sysinfo's value on a new row and the previous reading on an existing one. After either walk, `sampler.retain(|pid| cache.contains_key(&pid))` drops baselines for PIDs that left the cache, so they do not pile up.

### 3.4 Collector and Measurement Wiring

`LocalCollector` gained a macOS-only `process_sampler: Arc<std::sync::Mutex<ProcessSampler>>`. `process_pass` takes the global sysinfo lock, then the cache write lock, then the sampler lock, and recovers both from poison, as the cache lock already did. `perf_tick_stages` reports the macOS refresh row as sysinfo plus sampler, keeps all twelve existing row names so older runs stay comparable, and adds "process sampler (full ticks)" and "process sampler (selective ticks)". `docs/ARCHITECTURE.md` describes the new split.

### 3.5 Unsafe Surface

The new files hold six `unsafe` sites. In `pidinfo_macos.rs`: clearing `errno` through `__error()`, the `proc_pidinfo` call, `assume_init`, and reading `errno` back. In `sampler_macos.rs`: the `unsafe extern "C"` declaration of `mach_absolute_time` (the `libc` binding is deprecated; the IOReport reader declares it the same way) and its one call. The `proc_pidinfo` call and `assume_init` replace the pair deleted from `priority_macos.rs`, and the `errno` clear-and-read pattern is the one `priority_macos::nice` already used for `getpriority`.

---

## 4. Measurements on an M1 Ultra

Mac13,2, macOS 27.0 26A428, about 990 processes, no compiler running. Four `perf_tick_stages` passes alternating the baseline binary (`31ab474`, the merged #426 tree) and this branch (built from `b9c9f32`; `88163d6` changes comments only), `PERF_TICKS=30`, 21:18 to 21:25, load averages 1.90 to 3.56 at the pass starts. Each column is the mean of its two passes; `top -l 4 -s 8 -pid` samples 2 to 4 of each pass, with `local` in a 160x50 pty.

| per steady-state tick | baseline | this branch |
|---|---|---|
| process refresh, selective | 16.28 ms | 4.24 ms |
| process refresh, full (every 5th) | 31.67 ms | 35.18 ms (sysinfo's full refresh plus the sampler over every PID) |
| of which the sampler | n/a | 4.21 ms selective, 3.90 ms full |
| `update_process_cache` row | 1.12 ms selective, 0.86 ms full | 0.97 ms selective, 0.92 ms full |
| merge + sort + truncate | 0.81 ms | 1.07 ms |
| whole tick | 28.87 ms | 20.90 ms |
| process CPU during ticks | 2.86 % of one core at 1 s | 2.04 % |
| `all-smi local --interval 1`, whole process | 3.70 % of one core | 2.53 % |
| `all-smi api --interval 1`, whole process | 0.77 % of one core | 0.72 % |

The selective refresh drops 74 percent and the whole tick 28 percent. The full tick rises 3.5 ms: subtracting the 3.90 ms sampler pass leaves 31.28 ms for sysinfo, against 31.67 ms before, so the rise is the extra pass and nothing else. Averaged over a five-tick cycle the process refresh falls from 19.4 ms to 10.4 ms ((4 x 16.28 + 31.67) / 5 against (4 x 4.24 + 35.18) / 5). `api` is the control: it collects no processes on Apple Silicon and is unchanged within noise. Merge, sort and truncate rose 0.26 ms, which the PR does not attribute; it is small against the 8 ms the tick saved.

The selective sampler pass costs about as much as the full one because the tracked top 500 by CPU are almost exactly the inspectable processes, each costing three calls, while the 349 `EPERM` processes cost one failed call each. The `sample()` doc first promised about 1.3 ms for 500 PIDs, extrapolated from an isolated 2.6 us per PID; measured inside a tick the pass is 4.1 ms, and `b9c9f32` changed the doc to say so.

These numbers and #426's bench are different instruments. The bench's 27.2 ms selective refresh and 20.8 ms `KERN_PROCARGS2` pair came from a dedicated loop over 500 tracked PIDs; inside `perf_tick_stages` the same selective refresh read 13.59 and 15.93 ms in #426's two columns and 16.28 ms here. Compare #432 against its own alternating baseline, not against the bench.

A single development run on the same host (label `dev1`, 986 processes, loads 3.68 to 3.09 for the baseline and 2.52 to 2.34 for the build) agreed: selective refresh 16.264 to 4.138 ms, full 31.790 to 33.738 ms, whole tick 28.877 to 19.889 ms, and on the first tick a full process refresh of 13.458 against 12.710 ms and a whole first tick of 313.931 against 302.844 ms. The alternating run above is the number of record.

---

## 5. Learning Points

### 5.1 Equivalence Is a Measurement, and a Dependency's Number Can Be Wrong

#426 rejected the bypass on the premise that sysinfo's values could not be reproduced. Reading sysinfo's source turned that into two separate questions with different answers. `status` could be reproduced exactly, because it is a deterministic rule over fields `proc_pidinfo` exposes, and a table-wide A/B confirmed it for 637 of 637 inspectable processes. `cpu_usage` should not be reproduced where it differs, because there it is defective. Before deciding that a replacement "changes values", measure which values change and why; before promising to match a dependency, check that its number is right.

### 5.2 A Delta and Its Divisor Must Cover the Same Span

sysinfo's defect is a general one: a per-item delta divided by a global interval is right only for items refreshed on every call. Any schedule that refreshes some items less often (a tracked set, a full refresh every N ticks) silently multiplies their rate by N. Keeping the divisor next to the delta, per item, makes the refresh schedule irrelevant to the result, and the Linux path has the same class of defect in a worse form (#428).

### 5.3 A Failed Read Is Not an Exit

`proc_pidinfo` returns 0 bytes both for a process that is gone and for one the caller may not inspect, and for a normal user the second case is a third of the table. Only `errno` tells them apart, and only if it is cleared before the call and read back before anything else runs. Treating the two alike would either drop hundreds of live rows or keep dead ones.

### 5.4 An Isolated Per-Call Cost Underestimates the In-Tick Cost

2.6 us per PID measured alone predicted 1.3 ms for 500 PIDs; the pass costs 4.1 ms inside a real tick. The doc comment was corrected to the measured number rather than left at the extrapolation.

### 5.5 Tests That Run on a Loaded Runner Must Compare Stable Things

The `macOS Unit Tests` CI job runs `cargo test --lib device::process_list` on a hosted `macos-14` runner, so `b9c9f32` hardened two kinds of test. Thread 0 of a running test binary flips between running and waiting between two reads, so the exact state comparison now applies only to a sleeping child, whose thread 0 is stable. The inflation tests had bounded the full-tick reading only as a ratio of a one-second reference, which a starved reference second can fail; they now also require the reading to stay under 200 percent, where the defect reads about 500, and allow 3x on the ratio.

---

## 6. Change Summary

### Statistics

| Item | Value |
|------|-------|
| Files changed | 11 |
| Lines added | +1764 |
| Lines deleted | -160 |
| Tests added | 22 (sampler 15, refresh 6 of which 2 are ignored hardware tests, collector 1) |
| New modules | 3 (`process_list/pidinfo_macos.rs`, `process_list/sampler_macos.rs`, `process_list/refresh_macos.rs`) |

### Changes by Category

| Category | Count | Summary |
|----------|-------|---------|
| Performance | 1 | Selective ticks read `proc_pidinfo` instead of sysinfo's refresh |
| Correctness | 4 | No full-tick inflation, no sticky CPU percent, PID reuse rebuilds the row, `ESRCH` split from `EPERM` |
| FFI | 1 | Shared `pidinfo_macos::read::<T>` over a sealed trait, also used by the priority lookup |
| Measurement | 1 | `perf_tick_stages` runs `refresh_processes` and adds two sampler rows |
| Tests | 3 files | Sampler arithmetic and state rule, refresh paths, collector full-tick inflation |
| Documentation | 2 | `docs/ARCHITECTURE.md`, module docs with sysinfo file and line references |

### Related Commits

| Hash | Type | Message |
|------|------|---------|
| `ffee283` | update | Read macOS process metrics natively instead of per-tick sysinfo |
| `b9c9f32` | test | Harden the macOS sampler tests against a loaded CI runner |
| `88163d6` | docs | Correct two probe counts in the macOS sampler comments |
| `d5e4be3` | squash merge | Merge PR #432 into `main` |

---

## 7. Validation and Follow-up

### Completed Validation

- `cargo test --lib device::process_list` on the M1 Ultra: 28 passed and 2 ignored, three consecutive runs.
- `cargo test --bin all-smi view::data_collection::local_collector`: 7 passed, including `full_tick_does_not_inflate_untracked_processes` through the real `collect_steady_state` path.
- The two ignored hardware tests run with `--ignored --nocapture`: the sysinfo inflation reproduction and the table-wide A/B in section 2.1.
- `cargo clippy --lib --tests -- -D warnings`, `cargo fmt --check`, and `cargo test --test user_facing_text_test`.
- All seven PR checks passed, including `macOS Unit Tests` on `macos-14` (which runs the non-ignored `device::process_list` tests) and the Linux `Test Suite`, which compiles the non-macOS path.
- Four alternating `perf_tick_stages` passes with `top` for `api` and `local`, in section 4.

### Review

`pr-security-checker` was not dispatched for this PR. The FFI soundness review was folded into the correctness reviewer instead, covering buffer discipline, `errno` handling, short writes, overflow, and lifetimes. It found nothing above LOW, and the maintainer chose to merge on that basis. The PR has no review or comment on GitHub, so this report is where that decision is recorded. The FFI part of that review covered the six `unsafe` sites in section 3.5.

### Not Verified

- Linux and Windows runtime behavior. No host was available. The non-macOS path is the previous code moved into `process_pass`; the Linux build is covered by the `Test Suite` job, but nothing was run or measured on either platform.
- Windows compilation of this change. `windows-selfhosted.yml` runs on pushes to `main`, not on pull requests. When this report was written, its run for `d5e4be3` was still pending, and the four preceding completed runs on `main` (`c840c36`, `133b978`, `61710d8`, `31ab474`) had failed at the `cargo test --lib` step. The `main` CI run for `d5e4be3` was also still in progress.
- Apple chips other than this M1 Ultra, and Intel Macs, which take the same `target_os = "macos"` path.
- The manual comparison of `local` rows against `top` listed in #427's verification section is not recorded in the PR.
- `full_tick_does_not_inflate_untracked_processes` lives in the binary crate, which the macOS CI job does not run (it runs three `cargo test --lib` filters), so it has run only on the maintainer's host.

### Required Follow-up

- Issue #428, still OPEN at `status:ready`: the Linux counterpart of the CPU-percent defect. There, sysinfo recomputes every listed process on every refresh against one global interval, so a process outside the tracked set is inflated on every tick, not only the full one. It needs a Linux host for the reproduction, the fix, and a `perf_tick_stages` run.
- Check the `Windows Self-Hosted` result for `d5e4be3` once it runs, keeping in mind that the job was already failing at `cargo test --lib` before this PR.

### Remaining Constraints

- The full tick still pays sysinfo's full refresh, including the `KERN_PROCARGS2` copy for every process, about 31 ms every fifth tick. Native discovery through `proc_listallpids` could remove most of it but was out of scope in #427, and no issue tracks it yet.
- Rows the kernel will not let a normal user inspect (349 of 985 in the probe) keep sysinfo's values, as before: memory 0 and state `?`.
- On Linux and Windows `update_process_cache` still matches rows by PID alone, so a reused PID there keeps the previous process's name, user and command.
- For library users on macOS, `all_smi::device::process_list::update_process_cache` is no longer compiled; `refresh_processes` and `ProcessSampler` replace it there. Within this repository the collector and `perf_tick_stages` were its only callers, and both were updated.
- The macOS CI job does not run clippy, so `-D warnings` on the macOS-only modules was checked locally only (#429 tracks adding it).

### References

- [PR #432](https://github.com/lablup/all-smi/pull/432)
- [Issue #427](https://github.com/lablup/all-smi/issues/427): the problem statement, sysinfo source references, and acceptance criteria
- [Issue #414](https://github.com/lablup/all-smi/issues/414) and [PR #426](https://github.com/lablup/all-smi/pull/426): the measurement and the reason this item was deferred
- [Issue #428](https://github.com/lablup/all-smi/issues/428): the same class of CPU-percent defect on Linux
- Issue #429: clippy on the macOS CI job
- sysinfo 0.39.6: `src/unix/apple/macos/process.rs` (`update_process`, `get_process_infos`, `compute_cpu_usage`) and `src/unix/apple/macos/system.rs` (`get_time_interval`)
