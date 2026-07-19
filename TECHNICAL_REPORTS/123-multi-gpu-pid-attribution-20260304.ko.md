# 기술 보고서: PR #123 - fix: preserve per-device rows for multi-GPU PID attribution

**작성일**: 2026-03-04
**작성자**: AI Code Reviewer
**상태**: 완료
**언어**: Rust
**위험도**: Medium

---

## 요약

이 PR은 멀티 GPU 시스템에서 프로세스별 GPU 보고가 부정확했던 두 가지 버그를 수정한다. 여러 GPU에 걸쳐 실행되는 단일 프로세스가 하나의 행으로 병합되어 디바이스별 메모리 및 사용률 귀속 정보가 손실되던 문제를 해결했다. `(PID, device_uuid)` 복합 키로 프로세스 병합을 재설계하고, 로컬 데이터 수집기의 이중 병합 문제를 제거했다.

---

## 1. 문제 정의

### 1.1 배경
애플리케이션은 여러 디바이스에 걸쳐 GPU 프로세스를 모니터링한다. 단일 프로세스(예: llama-server)가 여러 GPU를 사용할 때, 각 GPU에 대해 정확한 메모리 귀속 정보가 포함된 별도의 행을 표시해야 한다.

### 1.2 기존 문제점

- **문제 1 (NVML 수집)**: compute 프로세스와 graphics 프로세스 모두에 나타나는 PID가 잘못 처리되었다. graphics 프로세스의 `!gpu_pids.contains()` 가드가 해당 PID가 이미 어떤 디바이스에서든 compute 프로세스로 확인된 경우 엔트리를 무시했다. 또한 flat `Vec` 사용으로 compute와 graphics 목록 모두에서 보고된 동일 `(PID, device)` 쌍이 이중 카운트될 수 있었다.
- **문제 2 (병합 로직)**: `merge_gpu_processes()`가 PID만을 키로 사용하여, 여러 GPU에 걸친 프로세스를 단일 행으로 병합해 디바이스별 메모리 및 사용률 귀속 정보가 손실되었다.
- **문제 3 (이중 병합)**: 로컬 수집기가 `get_process_info()`를 호출(내부적으로 시스템 전체 열거 + merge 수행)한 후, 자체적으로 다시 열거 + merge를 수행하여 중복 행이 발생했다.

### 1.3 위험성

| 위험 | 영향도 | 발생 가능성 |
|-----|-------|-----------|
| 멀티 GPU 설정에서 부정확한 VRAM 귀속 | High | High (모든 멀티 GPU NVIDIA 시스템에 영향) |
| 운영자에게 잘못된 프로세스 모니터링 데이터 제공 | Medium | High |

---

## 2. 기술적 검토 사항

### 2.1 보안 관점
보안 관련 영향 없음. 변경 사항은 외부 입력 처리 수정 없이 내부 데이터 구조 변환에 국한된다.

### 2.2 성능 관점

**검토 항목:**
- [x] 알고리즘 복잡도: O(n) HashMap 연산이 O(n) Vec 연산을 대체 -- 동등
- [x] 메모리 사용: GPU 프로세스 엔트리당 HashMap 오버헤드 소폭 증가 (일반적 규모인 수십 개 프로세스에서 무시 가능)
- [x] 기존 대비 hot path에 새로운 할당 없음

**성능 영향:**
| 영역 | Before | After | 비고 |
|-----|--------|-------|-----|
| NVML 프로세스 수집 | Vec + contains 확인 | HashMap<(pid, uuid)> | 동일 O(n) 복잡도 |
| 프로세스 병합 | HashMap<pid> | HashMap<(pid, uuid)> | 동일 O(n) 복잡도 |
| 로컬 수집기 | 시스템 열거 이중 수행 | 단일 열거 | CPU 오버헤드 감소 |

로컬 수집기에서 이중 `get_process_info()` 호출을 제거한 것은 의미 있는 성능 개선이다. 시스템 전체 프로세스 열거는 비용이 높은 연산이기 때문이다.

### 2.3 호환성/의존성 관점

- **Breaking Changes**: 있음 -- `merge_gpu_processes` 시그니처가 `(&mut Vec, Vec)`에서 `(Vec, Vec) -> Vec`로 변경. 모든 내부 호출 위치 업데이트 완료.
- **새로운 의존성**: 없음
- **호환성**: 모든 플랫폼 (NVIDIA, Jetson, Tenstorrent 및 기본 trait 구현을 통한 기타 플랫폼)

### 2.4 코드 품질 관점

- **테스트 커버리지**: 멀티 GPU 보존, 중복 제거, 비GPU 보존, 고아 GPU 프로세스를 다루는 6개의 새로운 단위 테스트 추가
- **코드 복잡도**: `merge_gpu_processes`에서 더 풍부한 키 구조로 인해 소폭 증가했으나 잘 문서화됨
- **기술 부채**: 감소 -- 잘못된 PID 전용 키와 이중 병합 안티패턴 제거

---

## 3. 기술적 선택과 그 이유

### 3.1 PID 단독 대신 (PID, device_uuid) 복합 키 사용

**컨텍스트:**
단일 프로세스가 합법적으로 여러 GPU를 사용할 수 있으며, 각각 독립적인 메모리 할당과 사용률을 갖는다.

**고려한 대안:**

| 옵션 | 장점 | 단점 |
|-----|-----|-----|
| Option A: PID로 키, 디바이스 간 메모리 합산 | 단순, 프로세스당 단일 행 | 디바이스별 귀속 손실; 어느 GPU에 어떤 할당이 있는지 판별 불가 |
| **Option B: (PID, device_uuid) 키** | 디바이스별 귀속 보존; 정확한 모니터링 | UI에서 프로세스당 복수 행; 약간 더 복잡한 병합 로직 |
| Option C: 중첩 구조 (PID -> Map<UUID, metrics>) | 가장 타입 안전한 표현 | ProcessInfo와 모든 소비자의 대대적 리팩토링 필요 |

**선택 이유:**
Option B는 최소한의 구조적 변경으로 정확한 디바이스별 귀속을 보존한다. 기존 `ProcessInfo` 구조체에 이미 `device_uuid`와 `device_id` 필드가 있으므로, (프로세스, 디바이스)당 하나의 행은 자연스러운 설계이다.

### 3.2 기본 구현이 포함된 `get_gpu_processes()` trait 메서드 추가

**컨텍스트:**
로컬 수집기가 이중 병합을 방지하기 위해, `get_process_info()`가 수행하는 전체 시스템 열거 없이 원시 GPU 프로세스 엔트리가 필요했다.

**고려한 대안:**

| 옵션 | 장점 | 단점 |
|-----|-----|-----|
| Option A: 모든 리더에 대해 `get_process_info()`를 두 단계로 리팩토링 | 깔끔한 분리 | 10개의 모든 GpuReader 구현체 변경 필요 |
| **Option B: 기본 구현이 포함된 `get_gpu_processes()` 추가** | 3개 리더만 override 필요; 하위 호환 | 기본 구현이 `get_process_info()`를 호출하여 override하지 않는 리더에서 약간 비효율적 |

**선택 이유:**
Option B는 영향 범위를 최소화한다. 기본 구현은 `get_process_info()`를 호출하고 필터링하며, 내부적으로 `merge_gpu_processes`를 호출하지 않는 리더에서는 정확하다(다소 비효율적이더라도). NVIDIA, Jetson, Tenstorrent만 명시적 override가 필요했다.

---

## 4. 구현 상세

### 4.1 아키텍처 변경

```
[변경 전]
LocalCollector:
  reader.get_process_info()  -- 내부적으로: 시스템 열거 + merge_gpu_processes
  시스템 다시 열거
  merge_gpu_processes 다시 수행  -- 이중 병합 버그

[변경 후]
LocalCollector:
  reader.get_gpu_processes()  -- 원시 GPU 엔트리만 반환 (시스템 열거 없음)
  시스템 한 번 열거
  merge_gpu_processes 한 번 수행  -- 단일 병합, 정확
```

### 4.2 주요 코드 변경

**파일: `src/device/process_list.rs`**
```rust
// 변경 전: PID만으로 키
let gpu_map: HashMap<u32, ProcessInfo> =
    gpu_processes.into_iter().map(|p| (p.pid, p)).collect();

// 변경 후: (PID, device_uuid) 복합 키
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

**변경 이유:** PID 전용 키가 멀티 GPU 프로세스를 단일 행으로 병합했다. 복합 키는 디바이스별 귀속을 보존하면서 `max()`를 사용해 동일 (PID, device)의 중복 compute/graphics 보고를 안전하게 병합한다.

**파일: `src/device/readers/nvidia.rs`**
```rust
// 변경 전: PID가 이미 확인된 경우 graphics 프로세스 삭제
if proc.pid > 0 && !gpu_pids.contains(&proc.pid) {

// 변경 후: 모든 프로세스 수집, 중복 제거는 HashMap에서 처리
if proc.pid > 0 {
```

**변경 이유:** `contains` 가드가 디바이스 전체에 걸쳐 전역인 PID 집합에 기반했다. PID가 합법적으로 GPU-A에서 compute 프로세스로, GPU-B에서 graphics 프로세스로 나타날 수 있었는데, 기존 가드는 GPU-B 엔트리를 삭제했다.

---

## 5. 학습 포인트

### 5.1 NVML의 멀티 GPU 프로세스 귀속

**개념:**
NVML은 `running_compute_processes()`와 `running_graphics_processes()` 두 개의 별도 API를 통해 프로세스를 보고한다. 단일 PID가 동일 디바이스에서 두 목록 모두에 나타나거나(compute와 graphics 컨텍스트 모두 사용 시), 다른 디바이스에 걸쳐 나타날 수 있다(여러 GPU에 걸쳐 실행 시).

**이 PR에서의 적용:**
수정은 두 경우 모두 처리한다: 동일 디바이스 중복은 `max()`로 병합하고, 크로스 디바이스 엔트리는 `(PID, device_uuid)` 복합 키를 사용해 별도 행으로 보존한다.

**일반적인 사용 사례:**
- 여러 GPU에 모델을 분할하는 LLM 추론 서버 (llama.cpp, vLLM)
- 데이터 병렬 또는 모델 병렬을 사용하는 학습 워크로드
- CUDA Multi-Process Service (MPS)를 사용하는 애플리케이션

---

## 6. 추가 학습 리소스

### 핵심 키워드
| 키워드 | 설명 | 관련성 |
|-------|-----|-------|
| `NVML` | NVIDIA Management Library | GPU 프로세스 정보 조회를 위한 주요 API |
| `device_uuid` | 고유 GPU 식별자 | 디바이스별 귀속 보존을 위한 복합 키의 일부 |
| `compute_processes` / `graphics_processes` | 두 개의 별도 NVML 프로세스 목록 | 이중 카운트를 유발한 중복 엔트리의 원천 |

### 관련 PR/이슈
- PR #122: 의존성 업그레이드 (이 PR의 base 직전에 병합)

---

## 7. 변경 요약

### 통계
| 항목 | 값 |
|-----|---|
| 변경된 파일 수 | 6 |
| 추가된 라인 | +275 |
| 삭제된 라인 | -49 |
| 테스트 추가 | 6 |

### 카테고리별 변경

| 카테고리 | 변경 수 | 주요 내용 |
|---------|--------|----------|
| Bug Fix | 2 | NVML 수집 중복 제거 + 병합 재설계 |
| Performance | 1 | 이중 시스템 프로세스 열거 제거 |
| Code Quality | 1 | 기본 구현이 포함된 trait 메서드 추가 |
| Tests | 6 | NVML 병합 및 process_list 병합 단위 테스트 |

### 관련 커밋
| Hash | Type | Message |
|------|------|---------|
| `c4b090f` | fix | preserve per-device rows for multi-GPU PID attribution |
| `ad39544` | fix | add get_gpu_processes trait method to prevent double-merge in local collector |

---

## 8. 후속 조치

### 모니터링 필요
- 2+ NVIDIA GPU가 있는 시스템에서 정확한 멀티 GPU 프로세스 귀속 확인
- 새로운 HashMap 기반 프로세스 추적의 메모리 사용량 모니터링 (무시 가능한 수준으로 예상)

### 향후 개선 사항
- 다른 리더(furiosa, rebellions, amd)의 `get_process_info()`가 비용이 증가하면 `get_gpu_processes()` override 고려
- `nvidia-smi` 폴백 경로(`get_gpu_processes_nvidia_smi`)는 여전히 flat `Vec` 사용 -- `--query-compute-apps`만 조회하므로 compute/graphics 중복 문제는 없지만, 일관성을 위해 정렬 가능

---

## 부록

### A. 테스트 결과
전체 334개 테스트 통과 (144 + 147 단위 테스트, 1 통합 테스트, 19 문서 테스트, 나머지 하위 크레이트). `cargo clippy -- -D warnings`와 `cargo fmt --check` 모두 깨끗하게 통과.
