# Technical Report: PR #123 - fix: preserve per-device rows for multi-GPU PID attribution

**Date**: 2026-03-04
**Author**: AI Code Reviewer
**Status**: Completed
**Languages**: Rust
**Risk Level**: Medium

---

## Executive Summary

This PR fixes two bugs that caused incorrect per-process GPU reporting in multi-GPU systems. A single process spanning multiple GPUs was collapsed into one row, losing per-device memory and utilization attribution. The fix re-keys process merging by `(PID, device_uuid)` and eliminates a double-merge issue in the local data collector.

---

## 1. Problem Statement

### 1.1 Background
The application monitors GPU processes across multiple devices. When a single process (e.g., llama-server) uses multiple GPUs, the system needs to show separate rows for each GPU with correct memory attribution.

### 1.2 Existing Issues

- **Issue 1 (NVML collection)**: A PID appearing as both a compute and graphics process was mishandled. The `!gpu_pids.contains()` guard on graphics processes silently dropped entries when the PID was already seen on any device as a compute process. Additionally, using a flat `Vec` allowed the same `(PID, device)` pair to be double-counted when reported by both compute and graphics lists.
- **Issue 2 (Merge logic)**: `merge_gpu_processes()` keyed only by PID, collapsing a process spanning multiple GPUs into a single row and losing per-device memory and utilization attribution.
- **Issue 3 (Double merge)**: The local collector called `get_process_info()` (which internally performs system-wide enumeration + merge), then did its own enumeration + merge again, causing duplicate rows.

### 1.3 Risk Assessment

| Risk | Impact | Likelihood |
|------|--------|------------|
| Incorrect VRAM attribution in multi-GPU setups | High | High (affects all multi-GPU NVIDIA systems) |
| Misleading process monitoring data for operators | Medium | High |

---

## 2. Technical Review

### 2.1 Security
No security implications. Changes are limited to internal data structure transformations with no external input handling modifications.

### 2.2 Performance

**Checklist:**
- [x] Algorithm complexity: O(n) HashMap operations replace O(n) Vec operations -- equivalent
- [x] Memory usage: Minor increase from HashMap overhead per GPU process entry (negligible at typical scale of tens of processes)
- [x] No new allocations in hot paths beyond what existed before

**Performance Impact:**
| Area | Before | After | Note |
|------|--------|-------|------|
| NVML process collection | Vec + contains check | HashMap<(pid, uuid)> | Same O(n) complexity |
| Process merge | HashMap<pid> | HashMap<(pid, uuid)> | Same O(n) complexity |
| Local collector | Double system enumeration | Single enumeration | Reduced CPU overhead |

The elimination of the double `get_process_info()` call in the local collector is a meaningful performance improvement, as system-wide process enumeration is expensive.

### 2.3 Compatibility & Dependencies

- **Breaking Changes**: Yes -- `merge_gpu_processes` signature changed from `(&mut Vec, Vec)` to `(Vec, Vec) -> Vec`. All internal callers have been updated.
- **New Dependencies**: None
- **Compatibility**: All platforms (NVIDIA, Jetson, Tenstorrent, and others via default trait implementation)

### 2.4 Code Quality

- **Test Coverage**: 6 new unit tests added covering multi-GPU preservation, deduplication, non-GPU preservation, and orphan GPU processes
- **Code Complexity**: Slightly increased in `merge_gpu_processes` due to richer key structure, but well-documented
- **Technical Debt**: Decreased -- removed the incorrect PID-only keying and the double-merge anti-pattern

---

## 3. Technical Decisions

### 3.1 Keying by (PID, device_uuid) instead of PID alone

**Context:**
A single process can legitimately use multiple GPUs, each with independent memory allocations and utilization.

**Alternatives Considered:**

| Option | Pros | Cons |
|--------|------|------|
| Option A: Key by PID, sum memory across devices | Simple, single row per process | Loses per-device attribution; cannot determine which GPU has which allocation |
| **Option B: Key by (PID, device_uuid)** | Preserves per-device attribution; accurate monitoring | Multiple rows per process in UI; slightly more complex merge logic |
| Option C: Nested structure (PID -> Map<UUID, metrics>) | Most type-safe representation | Requires significant refactoring of ProcessInfo and all consumers |

**Rationale:**
Option B was chosen because it preserves accurate per-device attribution with minimal structural changes. The existing `ProcessInfo` struct already has `device_uuid` and `device_id` fields, so one row per (process, device) is a natural fit.

### 3.2 Adding `get_gpu_processes()` trait method with default implementation

**Context:**
The local collector needed raw GPU process entries without the full system enumeration that `get_process_info()` performs, to avoid a double-merge.

**Alternatives Considered:**

| Option | Pros | Cons |
|--------|------|------|
| Option A: Refactor `get_process_info()` into two steps for all readers | Clean separation | Requires changes to all 10 GpuReader implementations |
| **Option B: Add `get_gpu_processes()` with default impl** | Only 3 readers need override; backward compatible | Default impl calls `get_process_info()` which is slightly wasteful for non-overriding readers |

**Rationale:**
Option B minimizes the blast radius. The default implementation calls `get_process_info()` and filters, which is correct (if slightly wasteful) for readers that don't call `merge_gpu_processes` internally. Only NVIDIA, Jetson, and Tenstorrent needed explicit overrides.

---

## 4. Implementation Details

### 4.1 Architecture Changes

```
[Before]
LocalCollector:
  reader.get_process_info()  -- internally does: enumerate_system + merge_gpu_processes
  enumerate_system again
  merge_gpu_processes again  -- DOUBLE MERGE BUG

[After]
LocalCollector:
  reader.get_gpu_processes()  -- returns only raw GPU entries (no system enumeration)
  enumerate_system once
  merge_gpu_processes once    -- SINGLE MERGE, CORRECT
```

### 4.2 Key Code Changes

**File: `src/device/process_list.rs`**
```rust
// Before: keyed by PID only
let gpu_map: HashMap<u32, ProcessInfo> =
    gpu_processes.into_iter().map(|p| (p.pid, p)).collect();

// After: keyed by (PID, device_uuid)
let mut gpu_map: HashMap<(u32, String), ProcessInfo> = HashMap::new();
for gpu_process in gpu_processes {
    let key = (gpu_process.pid, gpu_process.device_uuid.clone());
    gpu_map.entry(key)
        .and_modify(|existing| {
            existing.used_memory = existing.used_memory.max(gpu_process.used_memory);
            existing.gpu_utilization = existing.gpu_utilization.max(gpu_process.gpu_utilization);
        })
        .or_insert(gpu_process);
}
```

**Reason for change:** PID-only keying collapsed multi-GPU processes into a single row. The compound key preserves per-device attribution while using `max()` to safely coalesce overlapping compute/graphics reports for the same (PID, device).

**File: `src/device/readers/nvidia.rs`**
```rust
// Before: graphics processes dropped if PID already seen
if proc.pid > 0 && !gpu_pids.contains(&proc.pid) {

// After: all processes collected, deduplication handled by HashMap
if proc.pid > 0 {
```

**Reason for change:** The `contains` guard was based on the PID set which is global across devices. A PID could legitimately appear as a compute process on GPU-A and a graphics process on GPU-B -- the old guard would drop the GPU-B entry.

---

## 5. Learning Points

### 5.1 Multi-GPU Process Attribution in NVML

**Concept:**
NVML reports processes through two separate APIs: `running_compute_processes()` and `running_graphics_processes()`. A single PID can appear in both lists for the same device (if it uses both compute and graphics contexts) or across different devices (if it spans multiple GPUs).

**Application in this PR:**
The fix handles both cases: same-device overlap is coalesced via `max()`, while cross-device entries are preserved as separate rows using the `(PID, device_uuid)` compound key.

**Common Use Cases:**
- LLM inference servers (llama.cpp, vLLM) that shard models across multiple GPUs
- Training workloads using data parallelism or model parallelism
- Applications using CUDA Multi-Process Service (MPS)

---

## 6. Further Learning

### Key Terms
| Keyword | Description | Relevance |
|---------|-------------|-----------|
| `NVML` | NVIDIA Management Library | Primary API for querying GPU process information |
| `device_uuid` | Unique GPU identifier | Used as part of compound key to preserve per-device attribution |
| `compute_processes` / `graphics_processes` | Two separate NVML process lists | Source of overlapping entries that caused double-counting |

### Related PRs/Issues
- PR #122: Dependency upgrades (merged just before this PR's base)

---

## 7. Change Summary

### Statistics
| Item | Value |
|------|-------|
| Files changed | 6 |
| Lines added | +275 |
| Lines deleted | -49 |
| Tests added | 6 |

### Changes by Category

| Category | Count | Summary |
|----------|-------|---------|
| Bug Fix | 2 | NVML collection dedup + merge re-keying |
| Performance | 1 | Eliminated double system process enumeration |
| Code Quality | 1 | Added trait method with default implementation |
| Tests | 6 | Unit tests for NVML merge and process_list merge |

### Related Commits
| Hash | Type | Message |
|------|------|---------|
| `c4b090f` | fix | preserve per-device rows for multi-GPU PID attribution |
| `ad39544` | fix | add get_gpu_processes trait method to prevent double-merge in local collector |

---

## 8. Follow-up Actions

### Monitoring Required
- Verify correct multi-GPU process attribution on systems with 2+ NVIDIA GPUs
- Monitor memory usage of the new HashMap-based process tracking (expected negligible)

### Future Improvements
- Consider adding `get_gpu_processes()` overrides for other readers (furiosa, rebellions, amd) if their `get_process_info()` becomes expensive
- The `nvidia-smi` fallback path (`get_gpu_processes_nvidia_smi`) still uses a flat `Vec` -- it does not have the compute/graphics overlap issue since it queries `--query-compute-apps` only, but could be aligned for consistency

---

## Appendix

### A. Test Results
All 334 tests pass (144 + 147 unit tests, 1 integration test, 19 doc tests, remainder in sub-crates). `cargo clippy -- -D warnings` and `cargo fmt --check` both pass cleanly.
