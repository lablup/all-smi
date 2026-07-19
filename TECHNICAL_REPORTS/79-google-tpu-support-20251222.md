# Technical Report: PR #79 - feat: add Google TPU support via libtpu

**Date**: 2025-12-22
**Author**: AI Code Reviewer
**Status**: Completed
**Languages**: Rust, Python
**Risk Level**: Medium

---

## Executive Summary

This PR adds comprehensive support for monitoring Google Cloud TPU (Tensor Processing Unit) accelerators. The implementation introduces multiple detection and metrics collection strategies including sysfs scanning, libtpu/PJRT FFI bindings, and the tpu-info CLI tool. The code follows existing patterns from other NPU implementations (Gaudi, Furiosa, Rebellions) and includes appropriate safety measures for the experimental PJRT integration.

---

## 1. Problem Statement

### 1.1 Background
Google TPUs are custom ASICs used extensively for machine learning workloads in Google Cloud. Users running workloads on TPU VMs need visibility into device utilization, memory usage, and health metrics - similar to what nvidia-smi provides for NVIDIA GPUs.

### 1.2 Prior Limitations
- all-smi had no support for TPU devices
- Users on TPU VMs could not monitor their accelerator resources
- No unified interface for TPU metrics alongside other accelerators

### 1.3 Risk Assessment

| Risk | Impact | Likelihood |
|------|--------|------------|
| FFI crashes from ABI mismatch | High | Medium |
| Orphaned tpu-info processes | Medium | Low |
| Background thread resource leak | Medium | Low |

---

## 2. Technical Review

### 2.1 Security Perspective

**Reviewed Items:**
- [x] Input validation
- [x] Command execution safety
- [x] FFI safety
- [x] Privilege requirements

**Findings:**

| Issue | Severity | Status |
|-------|----------|--------|
| pre_exec prctl return unchecked | HIGH | Open |
| Missing SAFETY comments on unsafe | HIGH | Open |
| Panic on mutex lock in bg thread | MEDIUM | Open |
| Debug logging in production | LOW | Open |

**Command Execution Analysis:**
The tpu_info_runner.rs spawns tpu-info as an external process. Security review:
- Command name: Hardcoded "tpu-info" - no injection risk
- Arguments: Static strings ["--streaming", "--rate", "2"] - safe
- Environment: Sets TERM=dumb, NO_COLOR=1, PYTHONUNBUFFERED=1 - appropriate
- stderr: Piped to null to prevent deadlocks - good practice

**FFI Safety Analysis:**
The code loads libtpuinfo.so and libtpu.so dynamically:
- Library paths are hardcoded constants and standard Python paths
- No user-controlled paths in library search
- Symbol resolution uses proper libloading patterns
- PJRT API struct layout assumptions are explicitly documented as risky

### 2.2 Performance Perspective

**Reviewed Items:**
- [x] Process spawning frequency
- [x] Memory allocation patterns
- [x] Caching strategy
- [x] Thread management

**Performance Optimizations Found:**

| Optimization | Description | Commit |
|--------------|-------------|--------|
| TPU type caching | Accelerator type cached via OnceLock | 53f2959 |
| Worker thread limit | Tokio workers limited to 4 | e96481b |
| Streaming mode | tpu-info uses streaming to reduce process spawns | a330d91 |
| Conditional status update | Runner status only updates on successful parse | b18a8d1 |

**Resource Usage:**
- Background thread: 1 persistent thread for tpu-info streaming
- Memory: HashMap per device for metrics, OnceLock for static info
- Process: Single long-running tpu-info process instead of repeated spawns

### 2.3 Compatibility Perspective

**Breaking Changes:** None - additive feature only

**New Dependencies:**
- libloading (existing in project, used for FFI)
- libc (for prctl in pre_exec)

**Platform Support:**
- Linux only (TPUs are only available on Linux/GCE)
- macOS: Gracefully returns empty results
- Windows: Not applicable

### 2.4 Code Quality Perspective

**Test Coverage:**
- Unit tests: Generation parsing, GPU info creation
- Integration: Manual Python test (test_tpu_jax.py)
- Missing: FFI error condition tests, background thread tests

**Code Organization:**
- Follows existing patterns (readers/google_tpu.rs matches gaudi.rs, furiosa.rs)
- Modular design: Separate files for sysfs, PJRT, libtpuinfo, tpu-info runner
- Clear documentation headers with TPU generation specs

---

## 3. Technical Decisions

### 3.1 Multiple Detection Strategies

**Context:**
TPU detection is complex because different TPU versions expose devices differently:
- TPU v2-v5: Use /sys/class/accel/* with kernel driver
- TPU v6e: Use /dev/vfio/* with VFIO passthrough
- TPU VMs: Have environment variables (TPU_NAME, TPU_ACCELERATOR_TYPE)

**Options Considered:**

| Option | Pros | Cons |
|--------|------|------|
| A: tpu-info CLI only | Simple, unified | Requires external tool |
| B: Sysfs + PJRT native | No dependencies | Complex, ABI issues |
| C: Layered approach | Robust fallbacks | More code |

**Decision:** Option C - Layer multiple strategies:
1. Sysfs (fastest, most reliable for presence)
2. libtpuinfo (real-time metrics if available)
3. tpu-info CLI (comprehensive but requires install)
4. Environment variables (fallback for TPU VMs)

**Tradeoff:** More code complexity in exchange for broader compatibility.

### 3.2 Disabling PJRT Client Creation

**Context:**
Initial implementation attempted to create a PJRT client for direct TPU metrics access. This caused segfaults due to unstable ABI between libtpu versions.

**Decision:** Disable PJRT client creation, rely on tpu-info and sysfs instead.

**Rationale (from code comment):**
```rust
// SAFETY: We temporarily disable actual client creation because PJRT ABI 
// varies wildly between versions, causing Segfaults when calling function pointers
// at wrong offsets.
```

### 3.3 Background Thread for tpu-info Streaming

**Context:**
tpu-info supports --streaming mode for continuous metrics output.

**Options Considered:**

| Option | Pros | Cons |
|--------|------|------|
| A: Spawn per read | Simple | Process overhead |
| B: Persistent stream | Efficient | Thread management |
| C: gRPC client | Native | Requires PJRT |

**Decision:** Option B - Run tpu-info in streaming mode in background thread.

---

## 4. Implementation Details

### 4.1 Architecture

```
[Before]
LocalCollector -> GpuReaders -> [NVIDIA, Apple, Jetson, Gaudi, Furiosa, ...]

[After]
LocalCollector -> GpuReaders -> [NVIDIA, Apple, Jetson, Gaudi, Furiosa, GoogleTpu, ...]
                                                                      |
                              +-------------------------------------------+
                              | GoogleTpuReader                           |
                              |   +- tpu_sysfs.rs (Sysfs scanning)        |
                              |   +- tpu_info_runner.rs (CLI streaming)   |
                              |   +- libtpuinfo.rs (FFI to libtpuinfo.so) |
                              |   +- tpu_pjrt.rs (FFI - disabled)         |
                              +-------------------------------------------+
```

### 4.2 Key Code Patterns

**Singleton Background Runner:**
```rust
static RUNNER: OnceLock<TpuInfoRunner> = OnceLock::new();

pub fn get_runner() -> &'static TpuInfoRunner {
    RUNNER.get_or_init(TpuInfoRunner::new)
}
```

**Process Termination on Parent Death:**
```rust
unsafe {
    Command::new("tpu-info")
        .pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
            Ok(())
        })
        .spawn()
}
```

---

## 5. Learning Points

### 5.1 PR_SET_PDEATHSIG for Child Process Management

**Concept:**
Linux provides prctl(PR_SET_PDEATHSIG, signal) to request that a signal be sent to a process when its parent dies. This prevents orphaned child processes.

**Application in this PR:**
Used in tpu_info_runner.rs to ensure the tpu-info streaming process is killed when all-smi terminates unexpectedly.

### 5.2 OnceLock vs Lazy Static

**Concept:**
Rust 1.70+ provides std::sync::OnceLock as a standard library alternative to the once_cell crate's Lazy<T>.

**Pattern Used:**
```rust
static RUNNER: OnceLock<TpuInfoRunner> = OnceLock::new();
static ACCELERATOR_TYPE_CACHE: OnceLock<Option<String>> = OnceLock::new();
```

### 5.3 FFI with libloading

**Concept:**
The libloading crate provides safe wrappers for dynamic library loading.

**Important:** Store the Library alongside function pointers to prevent use-after-free.

---

## 6. Further Learning Resources

### Keywords
| Keyword | Description | Relevance |
|---------|-------------|-----------|
| PJRT | Platform-Independent Runtime | TPU's accelerator abstraction layer |
| libtpu | TPU runtime library | Core TPU functionality |
| prctl | Process control | Linux process management |
| VFIO | Virtual Function I/O | Used by TPU v6e for device passthrough |
| sysfs | Linux pseudo-filesystem | Hardware device information |

### Related Technologies
- **JAX**: Google's ML framework, primary TPU consumer
- **XLA**: Accelerated Linear Algebra compiler
- **Cloud TPU**: Google Cloud's TPU offering

---

## 7. Change Summary

### Statistics
| Item | Value |
|------|-------|
| Changed files | 22 |
| Lines added | +3012 |
| Lines deleted | -34 |
| New tests | 8 unit tests |

### Changes by Category

| Category | Count | Summary |
|----------|-------|---------|
| Feature | 6 | TPU reader, exporter, detection, runner |
| Performance | 2 | Worker thread limit, accelerator caching |
| Bug Fix | 5 | Process orphan, test compilation, status updates |
| Documentation | 2 | README, API.md |

---

## 8. Follow-up Actions

### Required
- [ ] Add SAFETY comments to all unsafe blocks
- [ ] Check prctl return value in pre_exec
- [ ] Replace .unwrap() with proper error handling in bg thread

### Recommended
- [ ] Add graceful shutdown mechanism for background thread
- [ ] Gate debug logging with cfg(debug_assertions)
- [ ] Add integration tests for sysfs parsing

### Monitoring
- Process count on TPU VMs (ensure no orphaned tpu-info processes)
- Memory usage over time (verify no leaks from HashMap growth)

---

## Appendix

### A. Test Results
```
running 8 tests
test device::readers::google_tpu::tests::test_create_gpu_info_from_mock_device ... ok
test device::readers::google_tpu::tests::test_create_gpu_info_with_empty_uuid ... ok
test device::readers::google_tpu::tests::test_format_memory_size ... ok
test device::readers::google_tpu::tests::test_reader_creation ... ok
test device::readers::google_tpu::tests::test_tpu_generation_display_name ... ok
test device::readers::google_tpu::tests::test_tpu_generation_from_chip_version ... ok
test device::readers::google_tpu::tests::test_tpu_generation_hbm_size ... ok
test device::readers::google_tpu::tests::test_tpu_generation_memory_type ... ok

test result: ok. 8 passed; 0 failed; 0 ignored
```

### B. Supported TPU Generations
| Generation | Codename | HBM | PCI Device ID |
|------------|----------|-----|---------------|
| TPU v2 | - | 8 GB | 0x0027 |
| TPU v3 | - | 16 GB | 0x0027 |
| TPU v4 | - | 32 GB | 0x005e |
| TPU v5e | - | 16 GB | 0x0063 |
| TPU v5p | - | 95 GB | 0x0062 |
| TPU v6e | - | 16 GB | 0x006f |
| TPU v6 | Trillium | 32 GB | - |
| TPU v7 | Ironwood | 192 GB | 0x0076 |

### C. Files Changed
```
src/device/readers/google_tpu.rs      (new, 1100 lines)
src/device/readers/tpu_info_runner.rs (new, 299 lines)
src/device/readers/libtpuinfo.rs      (new, 257 lines)
src/device/readers/tpu_pjrt.rs        (new, 494 lines)
src/device/readers/tpu_sysfs.rs       (new, 209 lines)
src/api/metrics/npu/google_tpu.rs     (new, 200 lines)
src/device/common/constants.rs        (+175 lines)
src/device/platform_detection.rs      (+70 lines)
+ 14 other modified files
```
