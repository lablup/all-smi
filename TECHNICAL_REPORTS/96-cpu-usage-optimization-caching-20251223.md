# Technical Report: PR #96 - Optimize CPU usage by caching expensive system calls

**Date**: 2025-12-23
**Author**: AI Code Reviewer
**Status**: Completed
**Languages**: Rust
**Risk Level**: Low

---

## Executive Summary

This PR implements caching optimizations to reduce CPU usage by avoiding repeated expensive system calls during metrics collection. The changes target three key areas: `system_profiler` calls in CPU monitoring, `System::new()` allocations for memory info, and hostname lookups. The implementation correctly addresses issue #88.

---

## 1. 문제 정의 (Problem Statement)

### 1.1 배경
The all-smi application collects hardware metrics on every update cycle (typically every 1-3 seconds). On macOS, this required calling expensive system commands and creating new objects repeatedly, causing unnecessary CPU overhead.

### 1.2 기존 문제점

- **문제 1**: `system_profiler SPHardwareDataType` was being called on every CPU info request, even though the data (CPU model, core counts) never changes during runtime. This command takes ~100-500ms to execute.
- **문제 2**: Two new `System::new()` instances were created on every GPU info update just to query memory, despite `sysinfo::System` being expensive to construct.
- **문제 3**: Hostname was fetched via system call on every metrics collection, despite being static during application runtime.

### 1.3 위험성

| 위험 | 영향도 | 발생 가능성 |
|-----|-------|-----------|
| Excessive CPU usage reducing system responsiveness | Medium | High |
| Battery drain on laptops due to repeated expensive calls | Low | High |
| Reduced metrics update frequency to compensate | Low | Medium |

---

## 2. 기술적 검토 사항 (Technical Review)

### 2.1 보안 관점

**검토 항목:**
- [x] 입력 검증 - N/A (no user input in cached values)
- [x] 인증/인가 - N/A (local system monitoring only)
- [x] 데이터 암호화 - N/A (no sensitive data cached)
- [x] 로깅 (민감정보 제외) - Cached data contains only hardware metadata

**발견된 이슈:**

| 이슈 | 심각도 | 상태 |
|-----|-------|-----|
| None | - | - |

The PR introduces no security concerns. All cached data consists of hardware metadata (CPU model, core counts, total memory, hostname) that is not sensitive.

### 2.2 성능 관점

**검토 항목:**
- [x] 쿼리 최적화 - System calls cached appropriately
- [x] 캐싱 전략 - Static values cached in Lazy statics
- [x] 알고리즘 복잡도 - O(1) cache lookups
- [x] 메모리 사용 - Minimal additional memory for cache

**성능 영향:**

| 영역 | Before | After | 개선율 |
|-----|--------|-------|-------|
| system_profiler calls/cycle | 1-2 | 0* | 100% |
| System::new() calls/cycle | 2 | 0 | 100% |
| hostname syscalls/cycle | 3+ | 0 | 100% |

*After initial call

### 2.3 호환성/의존성 관점

- **Breaking Changes**: None
- **새로운 의존성**: None (uses existing `once_cell` and `sysinfo`)
- **호환성**: Maintains compatibility with all existing platforms

### 2.4 코드 품질 관점

- **테스트 커버리지**: Existing tests pass, no new tests required (caching is an implementation detail)
- **코드 복잡도**: Slightly increased due to cache-first checks, but well-structured
- **기술 부채**: Minor - Mutex poisoning handling could be improved

---

## 3. 기술적 선택과 그 이유 (Technical Decisions)

### 3.1 Static Lazy Initialization for Immutable Data

**컨텍스트:**
Total memory and hostname never change during application runtime, making them ideal candidates for static caching.

**고려한 대안:**

| 옵션 | 장점 | 단점 |
|-----|-----|-----|
| Option A: Per-instance caching | Easier to test, more flexible | Memory overhead per reader instance |
| **Option B: Static Lazy** | Zero per-instance overhead, thread-safe | Slightly harder to test in isolation |
| Option C: Global mutable state | Simple | Not thread-safe, requires synchronization |

**선택 이유:**
Static `Lazy<T>` from `once_cell` provides zero-cost initialization and thread-safe access without requiring explicit synchronization at access time.

**트레이드오프:**
Testing requires integration tests rather than unit tests with mocked values, but this is acceptable for hardware metadata caching.

### 3.2 Cache Check Before Expensive Operation

**컨텍스트:**
The original code checked the cache AFTER calling `system_profiler`, which defeated the purpose of caching.

**고려한 대안:**

| 옵션 | 장점 | 단점 |
|-----|-----|-----|
| **Option A: Cache check first** | Avoids expensive call when cache hit | Requires restructuring control flow |
| Option B: Separate cached/uncached paths | Clear separation | Code duplication |

**선택 이유:**
Moving the cache check to the beginning of the function provides the maximum benefit with minimal code changes.

---

## 4. 구현 상세 (Implementation Details)

### 4.1 아키텍처 변경

```
[변경 전: cpu_macos.rs]
get_apple_silicon_cpu_info()
  ├── system_profiler call (expensive, ~100-500ms)
  ├── parse_apple_silicon_hardware_info()
  │     └── check cache (too late!)
  └── return cached/parsed values

[변경 후: cpu_macos.rs]
get_apple_silicon_cpu_info()
  ├── check cache FIRST
  │     ├── hit: use cached values (fast path)
  │     └── miss: system_profiler call → parse → cache
  └── return values
```

### 4.2 주요 코드 변경

**파일: `src/device/cpu_macos.rs`**
```rust
// 변경 전
let output = Command::new("system_profiler")
    .arg("SPHardwareDataType")
    .output()?;
let hardware_info = String::from_utf8_lossy(&output.stdout);
let (cpu_model, p_core_count, e_core_count, gpu_core_count) =
    self.parse_apple_silicon_hardware_info(&hardware_info)?;

// 변경 후
let (cpu_model, p_core_count, e_core_count, gpu_core_count) = if let (
    Some(cpu_model),
    Some(p_core_count),
    Some(e_core_count),
    Some(gpu_core_count),
) = (
    self.cached_cpu_model.lock().unwrap().clone(),
    *self.cached_p_core_count.lock().unwrap(),
    *self.cached_e_core_count.lock().unwrap(),
    *self.cached_gpu_core_count.lock().unwrap(),
) {
    // Use cached values - avoids expensive system_profiler call
    (cpu_model, p_core_count, e_core_count, gpu_core_count)
} else {
    // First time only
    let output = Command::new("system_profiler")
        .arg("SPHardwareDataType")
        .output()?;
    let hardware_info = String::from_utf8_lossy(&output.stdout);
    self.parse_apple_silicon_hardware_info(&hardware_info)?
};
```

**변경 이유:** Cache check must happen before the expensive operation to be effective.

**파일: `src/device/readers/apple_silicon_native.rs`**
```rust
// 변경 전
fn get_total_memory() -> u64 {
    let mut system = System::new();
    system.refresh_memory();
    system.total_memory()
}

fn get_used_memory() -> u64 {
    let mut system = System::new();
    system.refresh_memory();
    system.used_memory()
}

// 변경 후
static CACHED_TOTAL_MEMORY: Lazy<u64> = Lazy::new(|| {
    let mut system = System::new();
    system.refresh_memory();
    system.total_memory()
});

fn get_total_memory() -> u64 {
    *CACHED_TOTAL_MEMORY
}

fn get_used_memory() -> u64 {
    crate::utils::with_global_system(|system| {
        system.refresh_memory();
        system.used_memory()
    })
}
```

**변경 이유:** Total memory is immutable, so cache once. Used memory changes, so reuse global System instance instead of creating new ones.

**파일: `src/utils/system.rs`**
```rust
// 변경 전
pub fn get_hostname() -> String {
    System::host_name().unwrap_or_else(|| "unknown".to_string())
}

// 변경 후
static CACHED_HOSTNAME: Lazy<String> =
    Lazy::new(|| System::host_name().unwrap_or_else(|| "unknown".to_string()));

pub fn get_hostname() -> String {
    CACHED_HOSTNAME.clone()
}
```

**변경 이유:** Hostname never changes during runtime, eliminating repeated system calls.

---

## 5. 학습 포인트 (Learning Points)

### 5.1 once_cell::sync::Lazy for Static Initialization

**개념:**
`Lazy<T>` provides a thread-safe, lazily-initialized static value. The initialization function runs exactly once, on first access, and subsequent accesses return the cached value.

**이 PR에서의 적용:**
- `CACHED_TOTAL_MEMORY` stores total system memory (immutable)
- `CACHED_HOSTNAME` stores the system hostname (immutable)

**일반적인 사용 사례:**
- Configuration values read from environment
- Compiled regex patterns
- Database connection pools
- Any expensive-to-compute but immutable data

**예시 코드:**
```rust
use once_cell::sync::Lazy;

static CONFIG: Lazy<Config> = Lazy::new(|| {
    Config::load_from_file("config.toml")
        .expect("Failed to load configuration")
});

fn get_config() -> &'static Config {
    &*CONFIG
}
```

### 5.2 Cache Check Ordering Pattern

**개념:**
When implementing caching for expensive operations, the cache check must precede the expensive operation. A common bug is to compute the value first, then check if it was already cached.

**Anti-pattern:**
```rust
// WRONG: Expensive operation happens regardless of cache
let result = expensive_operation();
if let Some(cached) = cache.get() {
    return cached;  // Too late, already did the work!
}
cache.set(result);
return result;
```

**Correct pattern:**
```rust
// RIGHT: Check cache first
if let Some(cached) = cache.get() {
    return cached;  // Fast path
}
let result = expensive_operation();  // Only if cache miss
cache.set(result);
return result;
```

---

## 6. 추가 학습 리소스 (Further Learning)

### 핵심 키워드
| 키워드 | 설명 | 관련성 |
|-------|-----|-------|
| `Lazy<T>` | Thread-safe lazy initialization | Core pattern used for caching |
| `Mutex poisoning` | Rust's mutex behavior on panic | Potential edge case in current implementation |
| `sysinfo crate` | Cross-platform system info library | Used for memory and process information |

### 관련 기술/프레임워크
- **once_cell**: Rust crate for single-assignment cells
  - 공식 문서: https://docs.rs/once_cell
- **sysinfo**: Cross-platform system information
  - 공식 문서: https://docs.rs/sysinfo

### 추천 학습 주제
| 주제 | 왜 공부하면 좋은지 | 난이도 |
|-----|-----------------|-------|
| Rust memory ordering (Acquire/Release) | Understanding AtomicBool usage in this code | 중급 |
| Mutex poisoning and recovery | Handling edge cases in concurrent code | 중급 |
| Interior mutability patterns | Understanding when to use RefCell vs Mutex | 중급 |

### 관련 PR/이슈
- Issue #88: CPU usage optimization request - This PR closes the issue

---

## 7. 변경 요약 (Change Summary)

### 통계
| 항목 | 값 |
|-----|---|
| 변경된 파일 수 | 3 |
| 추가된 라인 | +52 |
| 삭제된 라인 | -19 |
| 테스트 추가 | 0 |

### 카테고리별 변경

| 카테고리 | 변경 수 | 주요 내용 |
|---------|--------|----------|
| Performance | 3 | Caching for system calls |
| Code Quality | 0 | - |
| Documentation | 0 | - |

### 관련 커밋
| Hash | Type | Message |
|------|------|---------|
| (PR) | perf | Optimize CPU usage by caching expensive system calls |

---

## 8. 후속 조치 (Follow-up Actions)

### 완료 필요
- [ ] Consider returning `&'static str` from `get_hostname()` instead of cloning (minor optimization)
- [ ] Consider graceful handling of mutex poisoning (edge case)

### 모니터링 필요
- CPU usage during view mode operation
- Memory usage (should remain stable with caching)

### 향후 개선 사항
- Potential use of `parking_lot::Mutex` which doesn't poison on panic
- Consider `Arc<str>` for hostname if reference semantics are needed
