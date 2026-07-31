# Technical Report: PR #296 - fix: report the affinity mask actually in force, not just -c

**Date**: 2026-07-31
**Status**: Completed
**Languages**: Bash (with awk, sed)
**Risk Level**: Low (benchmark tooling only; no change to what all-smi collects or reports)

---

## Executive Summary

`scripts/bench-local-interval.sh` prints an environment block whose only job is to let a reader decide whether two benchmark results are comparable. Under a CPU affinity mask the script did not set itself, three consecutive lines of that block disagreed with each other and with reality: a core count of 3 sat above a topology listing 20 CPUs, the CPU model line named a performance cluster no participating core belonged to, and the affinity line read `unpinned` for a run confined to three efficiency cores.

The cause was that the signals disagree about whether they can see a mask. `nproc` honours `sched_getaffinity`; `lscpu`, `/proc/cpuinfo`, and the sysfs CPU walk report the whole machine regardless; and the affinity string was derived solely from the `-c` flag. PR #291 had fixed the requested-versus-effective problem for `-c`, but a mask arriving from a container cpuset, a systemd `AllowedCPUs=` setting, a batch scheduler, or a bare `taskset` invocation stayed invisible.

The fix reads the mask once at startup through `taskset -pc $$`, compares it against `/sys/devices/system/cpu/online`, and scopes every line of the block against that single answer. Three secondary defects surfaced during the branch's own review and were fixed in the same PR: the mask readback was locale-dependent and could produce a confidently wrong answer, the mask was read from a process that was not the one being measured because tmux forks panes from its server, and the core count inherited `nproc`'s OpenMP thread cap. Total: 1 file, +242/-25, across 4 commits. Closes #292.

---

## 1. Problem Statement

### 1.1 Background

Issue #290 established that on a heterogeneous CPU, "percent of one core" is not a single quantity: identical work costs different CPU time on a performance core than on an efficiency one. On an NVIDIA GB10 (DGX Spark), pinning the same run to the Cortex-X925 cluster or the Cortex-A725 cluster moves the result by roughly 1.5x, which is larger than the collection-interval effect the script exists to measure. An absolute percentage is therefore comparable across machines only when core placement travels with it. PR #291 added the `topology` and `affinity` lines for exactly that purpose, and fixed one class of misreport in them: `taskset -c 0,99` is accepted and silently narrowed to CPU 0, so the affinity line was changed to report the mask read back rather than the list requested.

### 1.2 Existing Issues

Reproduced on a GB10 with 20 cores, where cpus 0-4 and 10-14 are Cortex-A725 at 2.81GHz and cpus 5-9 and 15-19 are Cortex-X925 at 3.90GHz:

```
$ taskset -c 0-2 scripts/bench-local-interval.sh -d 1 -i ""
  cpu           Cortex-X925 + Cortex-A725 (3 cores)
  topology      heterogeneous: 10x 3.90GHz (cpus 5-9,15-19), 10x 2.81GHz (cpus 0-4,10-14)
  affinity      unpinned (mixed core types: threads may migrate, see -c)
```

- **Issue 1 (count contradicts topology)**: the core count came from `nproc`, which honours `sched_getaffinity` and returned 3, while `linux_topology` enumerated `/sys/devices/system/cpu/cpu[0-9]*` directly and reported 20 across two clusters. The two numbers sat on consecutive lines.
- **Issue 2 (wrong CPU model)**: the line read `Cortex-X925 + Cortex-A725`, joining every cluster `lscpu` reports, but cpus 0-2 are all A725. No X925 core participated in the run.
- **Issue 3 (affinity line inverted)**: `AFFINITY_DESC` was derived solely from the `-c` flag, so a mask inherited from the environment was invisible to it. The one line whose job is to certify which cores produced a number stated the opposite of what happened. This is the serious one: a run confined to efficiency cores and labelled `unpinned` yields a number roughly 1.5x higher than the same work on performance cores, with nothing in the output indicating why.

The scenario is not exotic. It is reached by running the benchmark inside a container with a cpuset, on a host with a systemd `AllowedCPUs=` setting, under a batch scheduler that pins jobs, or simply by invoking the script under `taskset`.

### 1.3 Risk Assessment

| Risk | Impact | Likelihood |
|---|---|---|
| A mislabelled result is posted to issue #288 and treated as comparable to an unpinned one | High (a ~1.5x placement effect is silently attributed to the interval change under measurement) | High wherever the benchmark runs in a container or under a scheduler |
| An inherited mask is now re-applied to the launch, so what is measured changes, not only where it runs | Medium (results produced before and after this PR are not comparable on a host with a pre-existing tmux server) | High on any developer machine with a long-lived tmux server |
| `/sys/devices/system/cpu/online` unreadable, so restriction detection silently degrades to `unpinned` | Medium (reinstates the original misreport) | Low (sysfs is present on every Linux the script supports) |
| `lscpu` older than util-linux 2.38 lacks the `MODELNAME` column, so the cluster list over-reports under a mask | Low (the `N of M cores` count printed beside it already discloses confinement) | Medium on older distributions |
| A future util-linux rewords the `taskset -pc` readback | Low (the shape check degrades to "mask unreadable" by design) | Low |

---

## 2. Technical Review

### 2.1 Security

`EFFECTIVE_CPUS` reaches a command string that tmux hands to a shell, via `LAUNCH_PREFIX="taskset -c $EFFECTIVE_CPUS "`. Before it can get there it passes `affinity_list`, which rejects any value containing a character outside `[0-9,-]`, so no shell metacharacter can survive into that string. The pre-existing `-c` path interpolates `$CPUSET` raw, but only after `taskset -c "$CPUSET" true` has succeeded, and taskset rejects anything that is not a parseable CPU list; that gate predates this PR and remains adequate.

No new attack surface. The script is manual developer tooling with no network input and no privileged operation.

### 2.2 Performance

The added work is a handful of `awk` and `sed` invocations plus two `taskset` readbacks, all at startup and all outside the measurement window. `linux_topology` gains one shell-builtin membership test per CPU when given a mask. The measurement path itself is unmodified.

There is one effect on the numbers rather than on the tooling: re-applying an inherited mask confines the measured process, which is the intent, and which the header block now documents alongside the pre-existing caveat that all-smi sizes its own CPU view from `sched_getaffinity`, so pinning changes the work being measured and not only where that work runs.

### 2.3 Compatibility and Dependencies

- `taskset` (util-linux) is optional. When absent, the mask cannot be read, but `nproc` falling below the online count still proves one is in force, and that case says so explicitly rather than falling back to `unpinned`.
- The `lscpu -e=CPU,MODELNAME` per-CPU column requires util-linux 2.38 or newer. Older versions fall back to the whole-machine cluster list, which over-reports under a mask but not silently.
- `/sys/devices/system/cpu/online` is required for restriction detection. Without it the block degrades to prior behaviour.
- macOS is unaffected by construction: `EFFECTIVE_CPUS` is only assigned inside the `OS = Linux` branch or the Linux arm of the `-c` handler, and `-c` on Darwin exits with an error. The Darwin environment block was verified byte-identical to `main`.
- All constructs are bash 3.2 compatible.

### 2.4 Code Quality

`shellcheck -x` exits 0 and `bash -n` is clean. Comments in the changed regions explain rationale rather than restating syntax, matching the file's existing register.

The repository has no automated tests for shell scripts, so correctness was established by a simulation harness: a fabricated sysfs tree plus stub `uname`, `nproc`, `taskset`, and `lscpu`, driving the environment block across mask, locale, util-linux-version, and OpenMP permutations. This is worth noting as a gap rather than a strength; see section 8.

---

## 3. Technical Decisions

### 3.1 Read the effective mask through `taskset -pc $$`

| Option | Pros | Cons |
|---|---|---|
| **Chosen: `taskset -pc $$`** | Reads `sched_getaffinity`, so it sees a mask from any source; already a dependency of the `-c` path | Requires util-linux; output is translated, which caused the locale defect in 3.4 |
| Parse `/proc/self/status` `Cpus_allowed_list` | No dependency; field name is not translated; would also cover the no-taskset case | Reports `cpus_allowed` rather than the active-mask intersection, so it can differ from `sched_getaffinity` during CPU hotplug |
| Trust `nproc` alone | Already present | Answers "how many", never "which"; and is capped by OpenMP variables (see 4.4) |

The `taskset` route was chosen for consistency with the existing `-c` readback. The `/proc/self/status` alternative is recorded in section 8 as a genuine follow-up: it would have prevented the locale defect outright and would collapse the "restricted, cpus unknown" path, at the cost of a hotplug-window discrepancy that should be measured before switching.

### 3.2 Compare expanded CPU sets, not readback strings

The kernel's `online` file and taskset's readback are both CPU-list strings, but nothing guarantees they format an identical set the same way. Comparing `0,1,2,...,10-19` against `0-19` as strings would report a restriction where none exists. Both sides are therefore expanded to explicit CPU lists before comparison. Verified: rewriting the online file to a mixed comma-and-range form leaves the block reading `unpinned`.

### 3.3 Re-apply an inherited mask to the launch

| Option | Pros | Cons |
|---|---|---|
| **Chosen: re-apply via `LAUNCH_PREFIX`** | Honours the operator's evident intent in `taskset -c 0-2 bench.sh`; makes the affinity line true by construction | Changes what is measured, not only where it runs, because all-smi sizes its CPU view from `sched_getaffinity` |
| Report that the measured run is actually unrestricted | Leaves the measurement untouched | Produces a block that contradicts the command the operator typed |

This is the one decision in the PR that changes a measured number rather than a printed label. It was taken because tmux forks the pane from the tmux server rather than from this script (see 5.4), so without it the block certified a placement the measured process did not have. The consequence is disclosed in the header block.

### 3.4 Shape-check the readback and degrade to "unreadable"

A parser that cannot recognise its input should say so rather than pass along whatever it got. `affinity_list` strips the readback prefix and then discards any value that is not a bare CPU list, so a reworded or reformatted readback that `LC_ALL=C` cannot help with yields "mask unreadable" instead of a confidently wrong mask. This is the guard from the opposite side to the locale fix, and it is what makes the locale fix robust against future rewording rather than against two specific locales.

### 3.5 Key the migration warning on the mask, not the machine

The `mixed core types: threads may migrate` warning previously fired whenever the *machine* was heterogeneous. It now fires when the cores the run can actually reach are of more than one type. A run already confined to a single cluster cannot migrate across types, so pointing it at `-c` was advice it had effectively already taken.

---

## 4. Implementation Details

### 4.1 Mask acquisition and two new helpers

`cpu_list_expand` expands `0-2,5,7-8` into `0 1 2 5 7 8`, so a mask can be membership-tested against enumerations that do not know about it, and so two lists can be compared as sets.

`affinity_list` strips the `... current affinity list: ` prefix from a `taskset -pc` readback and shape-checks the remainder, returning nothing when it is not a bare CPU list.

Acquisition happens once, near the top:

```bash
if [ "$OS" = Linux ]; then
  ONLINE_CPUS="$(cat /sys/devices/system/cpu/online 2>/dev/null || true)"
  if command -v taskset >/dev/null 2>&1; then
    EFFECTIVE_CPUS="$(LC_ALL=C taskset -pc $$ 2>/dev/null | affinity_list || true)"
  fi
fi
```

From these, `CPUS_RESTRICTED` (mask readable and narrower than online), `CPUS_RESTRICTED_OPAQUE` (mask unreadable but provably present), and `MASK_ARG` (empty unless restricted) are derived. `MASK_ARG` being empty on an unrestricted run is what keeps that path byte-identical to its prior behaviour.

### 4.2 Scoping the three reported lines

- **`cpu`**: `detect_cpu` takes the mask and resolves cluster names through `lscpu -e=CPU,MODELNAME`, naming only the clusters the mask covers. The count is stated as a subset, `3 of 20 cores`, and is derived from the effective mask rather than from `nproc`, because under `-c` `nproc` describes this process and not the process about to be measured.
- **`topology`**: unchanged, still the whole machine.
- **`cores in use`**: new, carrying the topology intersected with the mask. `linux_topology` gained an optional CPU-list argument and skips CPUs outside it.
- **`affinity`**: reports an inherited mask as inherited, an unreadable-but-present mask as `restricted, cpus unknown`, and otherwise behaves as before.

### 4.3 The launch prefix

Under `-c`, `LAUNCH_PREFIX` was already set, which is why that path never exhibited the tmux problem. The inherited case now sets it the same way. Where the pane would have inherited the mask anyway the prefix is a no-op.

### 4.4 `core_count` and the OpenMP cap

GNU `nproc` caps its answer at `OMP_NUM_THREADS` and `OMP_THREAD_LIMIT`, which most ML and HPC images export and which say nothing about `sched_getaffinity`. Left unguarded, a thread-count cap on an entirely unpinned host was reported as an affinity mask.

The guard was first applied to the restriction probe only, which left the decision defended and the printed figure undefended: on a taskset-free host with an 8-CPU mask and `OMP_NUM_THREADS=4`, the block certified `4 of 20 cores` for a run that had 8, on the very path whose affinity line reads `cpus unknown` and gives the reader nothing to check it against. The final commit moved the guard into `core_count` itself so both callers inherit it, and reused the probe's count for the display so the number that decides "restricted" and the number printed beside that word cannot diverge.

---

## 5. Learning Points

### 5.1 `nproc` is not a core count

It answers "how many CPUs may this process use, subject also to OpenMP environment variables". Two different narrowings are folded into one integer, and neither is "how many cores does this machine have". Any code that prints `nproc` next to a hardware description is at risk of printing a contradiction.

### 5.2 `sched_getaffinity` and sysfs `online` answer different questions

The mask is per-process; the online set is per-machine. A tool that mixes a per-process source with a per-machine source in adjacent output lines will eventually print two numbers that cannot both be true. Reading both explicitly and stating which is which is the fix; picking whichever one a given helper happens to use is the bug.

### 5.3 Locale is a correctness surface when parsing tool output

util-linux translates `taskset -pc`, and `lscpu` translates its field labels. Under de_DE or ko_KR, `sed 's/.*list: *//'` matched nothing, the strip became a no-op, the entire sentence survived as the "mask", and awk read the prose prefix as CPU 0. On the GB10 this described a 3.90GHz X925 run as a 2.81GHz A725 one, on the exact lines the block exists to make trustworthy. Two lessons: pin `LC_ALL=C` on any tool output you parse, and validate the shape afterwards so an unrecognised format degrades to "unknown" rather than to a plausible wrong answer.

### 5.4 tmux does not fork the pane from the caller

tmux forks a new pane from the tmux *server*, and on the shared default socket that server is usually one an earlier, unrestricted shell started. So `taskset -c 5-9 bench.sh` produced a pane whose mask was the server's `0-19`. Anything that relies on a child inheriting the caller's process state must either start its own tmux server or re-apply the state explicitly. The same applies to environment variables, resource limits, and cgroup membership.

### 5.5 One premise in the original issue did not hold

Issue #292 stated that under an inherited mask the listed CPU ranges "invite the reader to pass `-c 5-9,15-19`, which cannot work because those CPUs are outside the inherited mask". That is not correct for a `taskset`-inherited mask: `sched_setaffinity` is bounded by the cgroup cpuset, not by the parent process mask. Confirmed on the host, where `taskset -c 0-2 sh -c "taskset -c 5-9 ..."` succeeds and lands on 5-9. The `-c` hint therefore remains valid advice and was not suppressed. For a genuine cgroup cpuset or `AllowedCPUs=` the constraint is real, and the readback added by PR #291 already reports the narrowing. Worth recording because an issue's diagnosis can be right about the symptom and wrong about a mechanism.

---

## 6. Further Learning

### Key Terms

| Keyword | Description | Relevance |
|---|---|---|
| `sched_getaffinity` / `sched_setaffinity` | Syscalls reading and setting a process's CPU affinity mask | The mask the whole PR is about; `taskset -pc` is the shell-reachable read |
| `/sys/devices/system/cpu/online` | Kernel's list of online CPUs, per machine | The baseline the mask is compared against |
| `Cpus_allowed_list` | Affinity mask exposed as a field in `/proc/self/status` | Locale-proof, dependency-free alternative source; see section 8 |
| cgroup cpuset / `AllowedCPUs=` | Container and systemd mechanisms that narrow the mask before the process starts | Two of the four ways an inherited mask arrives |
| `OMP_NUM_THREADS` / `OMP_THREAD_LIMIT` | OpenMP thread-count caps that GNU `nproc` honours | Source of the count defect in 4.4 |
| `lscpu -e=CPU,MODELNAME` | Per-CPU model name column, util-linux 2.38+ | How cluster names are scoped to the mask |
| big.LITTLE / P-core and E-core | Heterogeneous CPU designs mixing core types | Why placement has to travel with the number at all |

### Related Technologies and Frameworks

- util-linux (`taskset`, `lscpu`, `nproc`): version and locale behaviour both matter to this script.
- tmux server and client model: determines what the measured process actually inherits.
- Linux cpufreq and device-tree `cpu_capacity`: the two topology sources `linux_topology` reads, in that order.

### Related PRs and Issues

- Issue #292: the defect this PR closes.
- PR #291: added the `topology` and `affinity` lines and fixed requested-versus-effective for `-c`.
- Issue #290: established why core placement has to travel with the numbers.
- Issue #288: where affected measurements are collected and posted.
- Issue #293 (open): topology reported as `unknown` where CPU part IDs would resolve it; touches the same `unknown (no cpufreq or cpu_capacity)` path.
- Issue #297 and PR #298: follow-up found while reviewing this PR; the new `cores in use` line duplicated the `topology` line verbatim on a host whose topology cannot be read.

---

## 7. Change Summary

### Statistics

| Metric | Value |
|---|---|
| Files changed | 1 |
| Lines added | 242 |
| Lines removed | 25 |
| Commits | 4 |
| Base | `main` |

### Changes by Category

| Category | Detail |
|---|---|
| New helpers | `cpu_list_expand`, `affinity_list` |
| Modified functions | `linux_topology` (optional mask argument), `detect_topology` (argument passthrough), `detect_cpu` (mask-scoped cluster names, `LC_ALL=C` on the fallback), `core_count` (OpenMP cap stripped) |
| New state | `ONLINE_CPUS`, `ONLINE_COUNT`, `EFFECTIVE_CPUS`, `CPUS_RESTRICTED`, `CPUS_RESTRICTED_OPAQUE`, `VISIBLE_CPUS`, `MASK_ARG`, `TOPOLOGY_IN_USE`, `MIXED_CORE_TYPES` |
| Output | `cores in use` line added; `cpu` and `affinity` lines rescoped |
| Behaviour | Inherited mask re-applied through `LAUNCH_PREFIX` |
| Documentation | Header block extended to cover inherited masks and the re-application caveat |

### Related Commits

| SHA | Subject |
|---|---|
| `cf30874` | fix: report the affinity mask actually in force, not just -c |
| `3713096` | fix: make the reported mask the one the measured run actually gets |
| `dc96588` | fix: name CPU clusters in any locale, document the inherited-mask pinning |
| `0098cce` | fix: count cores, not OpenMP threads, on the cpu line |

Squashed to `9495e0c` on `main`.

---

## 8. Follow-up Actions

### Required

None. The PR is self-contained and its follow-up defect (#297) is already merged as PR #298.

### Monitoring Required

- Measurements posted to issue #288 from before this PR should not be compared against measurements taken after it when the host has a long-lived tmux server, because the inherited mask is now re-applied to the launch. Results predating the change may have been taken on the full machine despite a narrower label.
- `lscpu` output format and translation: the whole-machine fallback matches the `Model name` field label under `LC_ALL=C`. A future relabelling would silently return `unknown`.

### Future Improvements

- **Replace the `taskset -pc` readback with `/proc/self/status` `Cpus_allowed_list`.** It needs no util-linux, is not translated, and would collapse the `restricted, cpus unknown` path into the normal one. The tradeoff is that it exposes `cpus_allowed` rather than its intersection with the active-CPU mask, so the two can differ during CPU hotplug. Worth measuring that window before switching.
- **Add a regression harness for this script.** Correctness here was established by an ad hoc simulation (fabricated sysfs plus stub `uname`, `nproc`, `taskset`, `lscpu`). Four defects in this one PR were caught only because that harness existed. Committing something similar would make the guarantee repeatable rather than dependent on whoever is reviewing.
- **Reconcile with issue #293**, which improves topology detection on the same `unknown (no cpufreq or cpu_capacity)` path that PR #298 now keys a print suppression on. Whichever lands second should re-check the other's expected output.

---

## Appendix

### A. Test Results

Verified on the NVIDIA GB10 described in issue #292 across eight scenarios:

| Scenario | Result |
|---|---|
| Unrestricted | Output byte-identical to `main` |
| Inherited `0-2` (single cluster) | `Cortex-A725`, `3 of 20`, `uniform: 3x 2.81GHz`, affinity reported as inherited |
| Inherited `0-1,5-6` (mixed) | Both models, `4 of 20`, `heterogeneous: 2x 3.90GHz (cpus 5-6), 2x 2.81GHz (cpus 0-1)`, migration warning retained |
| Explicit `-c 5-9,15-19` | `Cortex-X925`, `10 of 20`, `uniform: 10x 3.90GHz` |
| Inherited `0-4` plus `-c 0-2` | `-c` wins: `3 of 20`, `uniform: 3x 2.81GHz` |
| `-c 0,99` (PR #291 clipping case) | `1 of 20`, `uniform: 1x 2.81GHz`, `cpus 0 (taskset, requested 0,99)` preserved |
| No `taskset`, unrestricted | Unchanged `unpinned` |
| No `taskset`, inherited mask | `N of 20 cores`, `restricted, cpus unknown (...)` |

Additionally verified: C, de_DE, and ko_KR locales; a simulated util-linux older than 2.38; a taskset-free PATH; `OMP_NUM_THREADS` with and without a pre-existing tmux server; and, for the final commit, a 14-combination sweep confirming byte-identical output wherever no OpenMP variable is set.

`shellcheck -x` exits 0. `bash -n` passes. The macOS environment block is byte-identical to `main`.

### B. Performance Benchmarks

Not applicable. The change is confined to the environment block printed before measurement begins; the measurement path is unmodified. The one effect on measured numbers is intentional and described in section 3.3.

### C. References

- `taskset(1)`, `lscpu(1)`, `nproc(1)`, util-linux
- `sched_getaffinity(2)`, `sched_setaffinity(2)`, `cpuset(7)`, `proc(5)`
- `tmux(1)`, server and client model
- Linux kernel documentation: `Documentation/ABI/testing/sysfs-devices-system-cpu`
