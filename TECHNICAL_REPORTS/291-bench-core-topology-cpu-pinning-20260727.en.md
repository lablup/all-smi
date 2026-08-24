# Technical Report: PR #291 - test: record core topology and support CPU pinning in the benchmark

**Date**: 2026-07-27
**Status**: Completed
**Languages**: Bash, Markdown
**Risk Level**: Low (developer tooling only; no change to what all-smi collects or reports)

---

## Executive Summary

`scripts/bench-local-interval.sh`, landed one PR earlier in #289, reports CPU as "percent of one core" and exists so that measurements taken on different machines can be compared. On a heterogeneous CPU that premise does not hold: identical work costs a different amount of CPU time on a performance core than on an efficiency core, and nothing in the output recorded which kind actually ran. Measured on an NVIDIA GB10, pinning the same run to the Cortex-X925 cluster versus the Cortex-A725 cluster moved the result by about 1.5x, which is larger than the interval effect the script was built to measure.

This PR makes the ambiguity visible and controllable rather than pretending it away. The environment block gained a `topology` line and an `affinity` line, a `-c` flag pins the run to a CPU list, and a `-r` flag averages several windows. Review then found that the first implementation could report an affinity it was not actually running under, over-split real hardware into phantom core tiers, and attribute the measurement to the wrong process; all were reproduced on hardware and fixed across three follow-up commits.

---

## 1. Problem Statement

### 1.1 Background

PR #286 changed local mode's default collection interval from 3s to 1s on Apple Silicon and to 2s elsewhere, pairing it with a roughly 20x reduction in IOReport collection cost that is gated to macOS. Issue #288 asked for measurements on the platforms that took the polling increase without the offsetting reduction, and PR #289 landed the benchmark script so those measurements would be produced the same way everywhere.

The first Linux results from that script, collected on an NVIDIA GB10 (DGX Spark), showed run-to-run variance large enough to sit uncomfortably close to the effect being measured. Investigating the variance rather than averaging it away led to issue #290.

### 1.2 Existing Issues

- **Issue 1 (the headline metric is ambiguous)**: GB10 pairs ten Cortex-X925 cores at 3.90 GHz with ten Cortex-A725 cores at 2.81 GHz. Pinning an otherwise identical run to one cluster or the other produced 1.12% versus 1.70% at `-i 2s`, a ratio of 1.52x. The 3s to 2s interval step that the script was measuring is +19.4% on the same host, so the placement artifact was larger than the signal. Unpinned runs land between the two depending on scheduler decisions, which is the source of the variance.
- **Issue 2 (no way to control or record placement)**: The script offered no affinity control and printed no topology, so a reader could not tell a heterogeneous host from a uniform one, nor tell which cores produced a given number.
- **Issue 3 (the property is not GB10-specific)**: Intel P/E hybrids and Apple Silicon mix core types the same way, so the Apple M5 Max reference numbers in #286, #288, and #289 carry the same ambiguity.

### 1.3 Risk Assessment

| Risk | Impact | Likelihood |
|------|--------|------------|
| Absolute percentages compared across machines without stating placement | Medium (wrong conclusions about collection cost) | High before this change |
| Measurement attributed to a different all-smi process | High (a confidently labelled wrong number) | Medium, and raised by `-c` itself |
| Interrupted run leaves an orphan TUI biasing later runs | Medium | Medium, growing with `-r` |
| Regression in the existing single-window output format | Low | Low (format string verified byte-identical) |

---

## 2. Technical Review

### 2.1 Security

The script builds a command string that tmux hands to a shell, so unvalidated flag values were an injection route. `-i '$(touch /tmp/x)1'` created the file, and a bare `-i '*'` glob-expanded so filenames in the working directory became intervals. This is not a privilege boundary, since the operator supplies the flags and could run the command directly, but the script's own reporting workflow encourages copying invocations out of issue threads. Intervals and durations are whole seconds by definition, so a numeric check removes the class, and the interval walk runs with globbing disabled.

`-c` was verified not to be an injection route: validation is handed to `taskset`, whose parser accepts only digits, commas, hyphens, and colons, and it runs before the launch prefix is built. Probes with `0; id`, `0,$(id)`, backticks, pipes, spaces, and embedded newlines were all rejected.

The PID change also closed a cross-user exposure: `pgrep` is not user-filtered, so the previous detection could attach to another user's all-smi on a shared host and read its `/proc` and `ps` data into the report.

### 2.2 Performance

No measurable change to the script's own cost. `-r` multiplies runtime by the repeat count and defaults to 1, so existing invocations are unaffected. The clock change to bash 5's `EPOCHREALTIME` removes up to 1.7% of quantisation error on a 60s window, error that previously landed directly on top of the spread `-r` exists to expose.

### 2.3 Compatibility & Dependencies

No new dependencies. `taskset` (util-linux) is required only when `-c` is used and its absence is reported. `EPOCHREALTIME` falls back to `date +%s` on bash 4 and earlier. `lscpu` is used only as an aarch64 fallback for the CPU model name. macOS support is preserved: topology comes from `hw.nperflevels`, and `-c` is rejected with an explanation rather than silently ignored.

### 2.4 Code Quality

`shellcheck` 0.11.0 is clean at default severity. The `-r 1` success-line format string was compared byte-for-byte against the pre-PR version so previously reported numbers remain comparable. `measure` was split into `measure_once` plus an aggregating wrapper, with failure reasons carried out through exit codes.

---

## 3. Technical Decisions

### 3.1 Group cores by maximum frequency, with a tolerance bucket

Exact-key grouping is the obvious implementation and it is wrong on real hardware. This machine's `cpu_capacity` reads 718, 731, 997, 1017, and 1024 across a two-cluster part, so exact equality reported five tiers, one of them a single CPU, and the printed CPU lists stopped being pasteable into `-c`. Intel parts with per-core turbo binning have the same shape on the cpufreq path, where favoured P-cores carry a higher `cpuinfo_max_freq` than their siblings.

Keys are bucketed within 5% of the group's fastest member. The comparison is against that fixed representative rather than the previous row, so a long run of small steps cannot drift one bucket across a genuine cluster boundary. Verified that this merges binned siblings (5.3 GHz with 5.2 GHz) without merging real tiers (5.0/4.0/3.0 GHz stays three groups).

### 3.2 Report the effective CPU mask, not the requested one

`taskset` fails only when *no* CPU in the list exists. It rejects `99-200` but silently accepts `0,99` and narrows it to CPU 0. Trusting the requested list meant the `affinity` line could attest to a placement that never happened, which is worse than printing nothing given that the line exists to certify a number as comparable. The mask actually in force is read back after launch and reported, with the requested list shown alongside when they differ.

### 3.3 Take the measured PID from tmux rather than inferring it

The original detection diffed `pgrep -x all-smi` snapshots and took the first new PID, which is a race with an arbitrary tie-break. `-c` turned this from theoretical into likely, because the natural way to use the flag is one pinned run per cluster, and running both concurrently to halve wall time made the two runs report byte-identical numbers while each `affinity` line claimed a different placement. Asking tmux which process it started (`list-panes -F '#{pane_pid}'`) removes the race entirely. This relies on tmux exec'ing the command in place, verified for both the bare and `taskset`-prefixed forms, with a one-level child lookup as a fallback.

### 3.4 Default `-r` to 1

A single 60s window on a low-cost host puts the measured effect close to the run-to-run spread, which argues for repeating by default. Against that, three repeats over four configurations is roughly 16 minutes, and silently tripling an existing tool's runtime is a poor default. The flag defaults to 1 and preserves the previous output format exactly; the header and DEVELOPERS.md recommend raising it when the effect is close to the spread.

### 3.5 Reject `-c` on macOS instead of ignoring it

macOS exposes no userspace CPU affinity control: `thread_policy_set` affinity is a cache-locality hint that Apple Silicon reports as unsupported, and only E-core confinement is reachable at all, via `taskpolicy -b`, which also changes scheduling priority. Silently ignoring the flag would produce an unpinned run labelled as pinned, which is the exact failure this PR set out to eliminate.

---

## 4. Implementation Details

### 4.1 Topology detection

Logical CPUs are grouped by `cpufreq/cpuinfo_max_freq`, falling back to the device-tree `cpu_capacity` on ARM systems without cpufreq, and to `hw.nperflevels` plus per-perflevel core counts on macOS. When none is readable the topology is reported as `unknown` rather than assumed uniform, because assuming uniform is the specific error the line exists to prevent. A uniform host prints count and speed only; a heterogeneous host additionally prints each cluster's CPU list, range-collapsed so it can be pasted straight into `-c`.

### 4.2 Affinity and the launch prefix

`-c` prepends `taskset -c LIST ` to the command tmux launches. On a heterogeneous host an unpinned run states so explicitly. The hint pointing at `-c` is Linux-only, because every Apple Silicon Mac reports two perflevels and so takes the heterogeneous branch, where advising a flag macOS rejects would be advice that can never work.

### 4.3 Repeats and aggregation

`measure_once` emits one window as `cpu_seconds wall_seconds rss_kb`; `measure` loops and aggregates. Percentages are averaged per window rather than dividing summed CPU time by summed wall time, so each window is weighted equally even if one ran long. Sample standard deviation uses the n-1 denominator with a clamp against the cancellation in `sumsq - n*mean^2`. A partial run reports how many windows succeeded and why the others failed.

### 4.4 Lifecycle

A trap kills the sessions this invocation created. The INT and TERM handlers exit rather than only cleaning up, because a bash trap returns to the point of interruption: cleanup alone killed the window in flight and then cheerfully opened the next one.

---

## 5. Learning Points

### 5.1 "Percent of one core" is not a portable unit

The unit silently assumes homogeneous cores. Every hybrid part breaks it, and the breakage is invisible in the output. The durable quantity is the ratio between two configurations measured on one host; absolute percentages are comparable across machines only when both state their core placement. Notably the ratio held up well here (+17.9% pinned to performance cores, +21.4% pinned to efficiency cores, +19.4% unpinned) even though the absolute values did not.

### 5.2 `taskset` validates less than it appears to

It fails only when the list matches no CPU at all. Any partially valid list is accepted and narrowed. Anything reporting an affinity should read the effective mask back rather than echo what was asked for.

### 5.3 A bash trap returns to where it interrupted

`trap cleanup INT` cleans up and then continues execution. For a loop that creates resources, that means tearing down the current one and immediately building the next. Handlers that are meant to stop the script must call `exit` themselves.

### 5.4 Background jobs cannot be tested for SIGINT handling

Bash sets SIGINT to ignored for asynchronous commands, and a signal ignored at shell entry cannot be trapped or reset. Testing interrupt handling by launching the script with `&` and sending SIGINT therefore exercises nothing, and looks exactly like a broken trap. The faithful test is a real terminal: running the script in a tmux pane and sending `C-c`.

### 5.5 Per-core frequency and capacity values are not uniform within a cluster

Real silicon reports a spread within one core type, from turbo binning on x86 and from device-tree capacity values on ARM. Any code that groups cores by these values needs a tolerance, not equality.

---

## 6. Further Learning

### Key Terms

- **big.LITTLE / hybrid CPU**: a part combining core types with different performance and efficiency characteristics.
- **`cpu_capacity`**: an ARM device-tree value expressing relative core throughput, normalised so the fastest core is 1024.
- **`sched_getaffinity` / `taskset`**: the Linux CPU affinity mask and the util-linux tool that sets it.
- **CPU-time delta**: process utime plus stime measured across a window, as opposed to `ps -o %cpu`, which means different things on Linux and macOS.

### Related Technologies/Frameworks

- `tmux` for a fixed-size detached terminal, so render cost is constant across machines.
- `/proc/PID/stat` on Linux and BSD `ps -o cputime=` on macOS as the two CPU-time sources.
- bash 5 `EPOCHREALTIME` for sub-second wall timing without spawning `date`.

### Related PRs/Issues

- Issue #290: the tracking issue this PR closes.
- PR #289: added the benchmark script; this PR fixes its core-topology blind spot.
- Issue #288: collects local-mode measurements on non-Apple-Silicon hardware; the GB10 results that exposed #290 are posted there.
- PR #286: the interval and IOReport sampling change being measured.

---

## 7. Change Summary

### Statistics

- Files changed: 2
- Lines: +391 / -42
- Commits: 4
- No files under `src/` touched, satisfying issue #290's fourth acceptance criterion.

### Changes by Category

| Category | Detail |
|----------|--------|
| Feature | `topology` and `affinity` lines in the environment block; `-c` CPU pinning; `-r` repeats with mean and standard deviation |
| Correctness | Effective mask read-back; tolerance bucketing of core speeds; PID taken from tmux |
| Robustness | Cleanup trap on INT/TERM/EXIT; numeric validation of `-i` and `-d`; tmux stderr surfaced on launch failure; higher-resolution wall clock |
| Documentation | Comparability limit, all flags, container caveat, and `libdrm-dev` prerequisite in the script header and `DEVELOPERS.md` |

### Related Commits

- `1b3423f` test: record core topology and support CPU pinning in the benchmark
- `8e50f95` fix: report the effective CPU mask and bucket near-equal core speeds
- `55cc462` fix: take the measured PID from tmux and clean up on interrupt
- `6fbfb26` docs: document -h and -b for the local-mode benchmark script

---

## 8. Follow-up Actions

### Required

- None. Issue #290's acceptance criteria are met.

### Monitoring Required

- Issue #288 remains open for a Windows host result and for a multi-GPU NVIDIA node. The GB10 host has a single GPU, so the "multi-GPU node" case in #288 is still unmeasured, and since `gpu_info` dominates the collection pipeline an 8-GPU node could shift the per-collection cost materially.

### Future Improvements

- `topology` enumerates sysfs directly while `core_count` uses `nproc`, which honours `sched_getaffinity`. Under a restricted mask the two disagree, and the block can list CPU IDs the run cannot touch. Intersecting the list with the current affinity mask would keep the block honest.
- On cloud Linux VMs that expose neither cpufreq nor `cpu_capacity`, topology reads `unknown`, which is honest but does not distinguish heterogeneous from uniform. A different signal would be needed there.

---

## Appendix

### A. Test Results

- `shellcheck` 0.11.0: clean at default severity. `bash -n`: clean.
- Topology detection exercised against the shipped awk for uniform x86, Intel P/E (8P+16E), Intel favoured-core binning, the real ARM capacity values on this host, single core, non-contiguous clusters, three genuine tiers, and CPU indices past 9.
- Flag validation: `-r 0`, `-r abc`, `-d abc`, `-d 0`, `-i '$(touch ...)'`, `-i '1;id'`, `-i '*'`, and `-c 99-200` each rejected with a specific message.
- `-c 0,99` reports `cpus 0 (taskset, requested 0,99)`.
- Two concurrent pinned runs report distinct numbers (1.30% perf versus 1.80% eff) where the pre-fix script reported byte-identical lines.
- Genuine terminal Ctrl-C during a window: bench tmux session and child both removed; the operator's unrelated sessions survive.
- `-r 1` success-line format string byte-identical to the pre-PR version.

### B. Performance Benchmarks

Measured on NVIDIA GB10 (DGX Spark), Ubuntu 24.04.4, kernel 6.17.0-1026-nvidia, aarch64, 20 cores, all-smi 0.24.2 release build, 60s windows after 8s warmup, percent of one core.

Cluster placement, same binary and interval:

| config | perf (X925) | eff (A725) | ratio |
|--------|-------------|------------|-------|
| default | 1.13% | 1.67% | 1.48x |
| `-i 2s` | 1.12% | 1.70% | 1.52x |
| `-i 3s` | 0.95% | 1.40% | 1.47x |

Pinning also tightens the spread: pinned to the efficiency cluster, `default` and `-i 2s` agreed exactly at 1.58%, where they must be identical because local mode resolves to 2s on Linux, with a deviation near 0.04 against the 0.08 to 0.13 measured unpinned.

### C. References

- `src/common/config.rs:293-317`, `EnvConfig::adaptive_interval` and `local_interval`.
- Linux kernel documentation for `sysfs` cpufreq and `cpu_capacity`.
- bash reference manual, Signals and the `trap` builtin.
