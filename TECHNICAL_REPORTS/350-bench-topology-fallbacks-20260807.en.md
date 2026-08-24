# Technical Report: PR #350 - Resolve Benchmark Topology via Intel Hybrid sysfs and ARM CPU Part IDs

**Date**: 2026-08-07  
**Status**: Completed  
**Related**: PR #350, Issue #293  
**Risk Level**: Low (benchmark script only, no product code)

---

## Executive Summary

PR #350 adds two signals to the CPU topology fallback chain in `scripts/bench-local-interval.sh`: Intel hybrid sysfs CPU lists and ARM `CPU part` grouping from `/proc/cpuinfo`. Before this, `linux_topology` resolved core types through exactly two signals, `cpufreq/cpuinfo_max_freq` then `cpu_capacity`, and printed `unknown` when neither was readable.

Cloud VMs commonly expose neither, since frequency scaling is the hypervisor's business there and they are not ARM device-tree systems either. On those hosts the benchmark's topology line degraded to `unknown` even where the topology was in fact discoverable.

---

## 1. Problem Statement

The benchmark's environment block exists so a measurement can be compared against another machine's. Topology matters more than most of it: on the measured GB10, pinning the same run to one cluster or the other moves the result about 1.5x, which is larger than the interval effect the script exists to measure.

A topology line reading `unknown` therefore does not just lose a detail, it removes the reader's ability to tell whether two numbers are comparable at all. Two signals were not enough to cover the hosts the benchmark actually runs on.

## 2. Change Summary

| Item | Value |
|------|-------|
| Files changed | 1 |
| Lines added | 173 |
| Lines deleted | 24 |
| Product code changed | No |
| Signals in the fallback chain | 2 to 4 |

### The chain, in order

| Order | Signal | Source |
|-------|--------|--------|
| 1 | maximum frequency | `cpufreq/cpuinfo_max_freq` |
| 2 | capacity | `cpu_capacity` (ARM device tree) |
| 3 | **Intel hybrid CPU lists** | `/sys/devices/cpu_core/cpus`, `/sys/devices/cpu_atom/cpus` |
| 4 | **ARM CPU part** | `CPU part` field in `/proc/cpuinfo` |
| final | `unknown` | all four failed |

## 3. Technical Decisions

### 3.1 The new signals group by exact equality, not by tolerance bucket

The existing frequency and capacity paths bucket keys within 5% of the group's fastest member, so per-core turbo binning does not over-split a real two-cluster part into five tiers. That tolerance exists because those readings carry per-core measurement noise.

A part ID or a kernel-published hybrid CPU list has no such noise: it is a discrete value. The new paths therefore use a shared `group_exact()` helper with no tolerance bucket, because applying one would only risk merging two genuinely different parts.

### 3.2 The ARM part path names groups through `lscpu` when it can

`arm_part_topology()` groups CPUs by the `CPU part` field, independent of both cpufreq and `cpu_capacity`. It labels each group with the name `lscpu -e=CPU,MODELNAME` decodes the part ID to (for example `Cortex-X925`) when `lscpu` is present and new enough, and falls back to the raw part ID (`part 0xd85`) otherwise.

The fallback never collapses two real groups into one. An unnamed group is still a distinct group; only its label degrades.

### 3.3 The ARM path's group ordering carries no performance ranking, and says so

The cpufreq path sorts fastest first, which sets an expectation a registry part ID cannot meet. On the GB10 used for testing the two orderings happen to agree, so the discrepancy would otherwise surface only on a different part pairing, and silently. It is documented in the script rather than left to be discovered.

### 3.4 The CPU-range collapse is extracted rather than duplicated

Both the existing `linux_topology` and the new `group_exact` need the same insertion sort and run-collapse loop to print `0-3,8` instead of a bare list. It is extracted into a shared `AWK_RANGES` awk source.

Two copies would have let a future fix land in one grouper and not the other, and the two would then disagree only on the hosts that fall through to the second path, which are precisely the hosts with the least topology information to begin with.

### 3.5 Both new paths honour the affinity mask

The existing cpufreq and capacity paths already take the affinity mask argument. The new ones do the same, and `intel_hybrid_topology()` returns failure (falling through to the next signal) when neither list is readable, or when an affinity mask excludes every CPU named in both.

`unknown` remains the final fallback when all four signals fail, and is still never reported as `uniform`. Those are different claims, and conflating them would let a measurement on an unknown topology be read as a measurement on a homogeneous one.

## 4. Outcome and Follow-up

- PR #350 was squash-merged into `main` as `e659a6f`.
- Issue #293 closed automatically through the PR's `Closes #293` link.
- The benchmark now resolves topology on Intel hybrid hosts and on ARM hosts without device-tree capacity, which covers the cloud VM cases that motivated the issue.
- Issue #288, validating the v0.25.0 local-mode interval change on non-Apple-Silicon platforms, remains open and is the consumer of this benchmark.

---

## Appendix: Key Terms

| Keyword | Description | Relevance |
|---------|-------------|-----------|
| `cpu_core` / `cpu_atom` | Kernel-published CPU lists for Intel hybrid P-cores and E-cores | Signal 3, works where cpufreq is absent |
| `CPU part` | ARM implementer-defined part number in `/proc/cpuinfo` | Signal 4, works with neither cpufreq nor device tree |
| `cpu_capacity` | ARM device-tree relative capacity | Signal 2, absent on cloud VMs |
| tolerance bucket | Grouping keys within 5% of the group maximum | Correct for noisy frequency readings, wrong for discrete part IDs |
