# 기술 보고서: PR #317 - fix: derive Apple Silicon CPU cluster frequency from the pmgr table

**작성일**: 2026-08-05
**상태**: 완료
**언어**: Rust
**위험도**: Low (파일 1개, 추가 성격의 테이블 조회, 실제 하드웨어 샘플로 고정한 회귀 테스트 13개)

---

## 목차

1. [요약](#요약)
2. [문제 정의](#1-문제-정의)
3. [기술적 검토 사항](#2-기술적-검토-사항)
4. [기술적 선택과 그 이유](#3-기술적-선택과-그-이유)
5. [구현 상세](#4-구현-상세)
6. [학습 포인트](#5-학습-포인트)
7. [추가 학습](#6-추가-학습)
8. [변경 요약](#7-변경-요약)
9. [후속 조치](#8-후속-조치)
10. [부록](#부록)

---

## 요약

`all_smi_cpu_frequency_mhz`, `all_smi_cpu_p_cluster_frequency_mhz`, `all_smi_cpu_e_cluster_frequency_mhz`는 애플 실리콘 머신 전부에서 0을 찍었다. 원인은 함수 하나, `src/device/macos_native/ioreport.rs`의 `IOReportMetrics::process_cpu_channel`에 있었다. 이 함수는 IOReport 성능 상태의 *이름*을 메가헤르츠 정수로 파싱해서 클러스터 클록을 얻으려 했는데, 애플 실리콘은 CPU 상태 이름을 그런 식으로 짓지 않는다. M1 Ultra에서 성능 클러스터는 `IDLE, V0P14, V1P13, ... V14P0`을, 효율 클러스터는 더 짧은 `V0P4 ... V4P0` 구간을 보고한다. 이런 이름에 대고 돌린 `parse::<i64>()`는 전부 실패했고, 그 결과 잔류율 가중 주파수 합은 0에 머물렀으며 평균 클록도 0으로 무너졌다. 반면 잔류율 자체는 별도 누적기에서 계산되기 때문에 이 버그 내내 계속 맞았고, CPU 사용률도 틀린 적이 없었다. GPU 경로가 같은 함정을 피한 이유는 이미 IOKit `AppleARMIODevice`의 pmgr `voltage-states9*` 테이블에 잔류율 히스토그램을 조인하고 있었기 때문이다. CPU 쪽에는 그에 해당하는 조회가 아예 없었다.

PR #317은 CPU 경로에 같은 테이블 조인을 붙였다. `voltage-states*` 속성들을 한 번만 읽어 GPU와 CPU 양쪽이 함께 쓰는 공유 테이블 목록으로 만들고, 효율 클러스터는 `voltage-states1-sram`을, 성능 클러스터는 `voltage-states5-sram`을 읽게 했다. 문서화된 키가 없는 클러스터(M5의 "Super" 클러스터)는 길이 매칭 폴백으로 풀린다. Apple M1 Ultra에서 검증한 결과 `/metrics`의 전체/성능/효율 클러스터 주파수가 0/0/0에서 2646/3228/2064 MHz로 바뀌었고, TUI의 CPU 행도 `Freq: 0+0MHz`에서 `Freq: 2.58+1.20GHz`로 바뀌었다. 같은 조사 과정에서 부수적으로 두 가지 방어 로직이 붙었다. 멀티 다이 패키지(M1/M2 Ultra)가 모든 채널에 붙이는 `DIE_<n>_` 접두사를 분류 전에 떼어내는 것과, voltage-states 페이로드의 32비트 오버플로를 보정하는 것이다. 4.295GHz를 넘는 클록은 이 필드에 들어가지 않는데, 이후 세대 애플 실리콘 P코어가 이를 넘어선다. 전체 규모는 파일 1개, +626/-119, 커밋 1개이고 #314를 닫는다.

---

## 1. 문제 정의

### 1.1 배경

all-smi에서 애플 실리콘 CPU와 GPU 주파수는 IOReport의 `CPU Stats` / `CPU Core Performance States`, `GPU Stats` 채널 그룹에서 나온다. 이 채널들은 잔류율(각 성능 상태에 머문 시간)만 보고할 뿐 클록 값을 직접 주지는 않는다. IOReport는 성능 상태를 오직 상징적인 이름으로만 구분하므로, 그 이름을 메가헤르츠 숫자로 바꾸려면 별도 조회 테이블이 필요하다. GPU 경로에는 이미 그런 테이블이 있었다. 이 PR 이전 이름으로 `load_gpu_frequencies`가 IOKit `AppleARMIODevice`의 pmgr 노드에서 `voltage-states9*` 속성을 읽어 오름차순 `Vec<u32>`를 만들었고, `GPUPH` 활성 상태 하나마다 값 하나를 대응시켰다. CPU 경로에는 이에 대응하는 것이 없었다. `calc_freq_from_residencies`는 상태 이름 자체를 정수로 파싱해서 클록을 바로 얻으려 했는데, 이는 플랫폼이 상태 이름을 숫자로 짓는 경우에만 통하는 전략이다.

### 1.2 기존 문제점

- **문제 1 (모든 CPU 주파수 지표가 0)**: 애플 실리콘은 CPU 성능 상태 이름을 숫자로 짓지 않는다. M1 Ultra에서 `DIE_0_PCPU_CPU0`은 `IDLE, V0P14, V1P13, V2P12, ... V14P0`(활성 15개)을, `DIE_0_ECPU_CPU0`은 `IDLE, V0P4, V1P3, V2P2, V3P1, V4P0`(활성 5개)을 보고한다. `V0P14` 같은 이름에 대한 `parse::<i64>()`는 전부 실패하고, 평균 계산에 쓰이는 잔류율 가중 합은 아무것도 쌓지 못한 채 평균이 0으로 무너진다.
- **문제 2 (사용률은 계속 맞아서 버그가 가려짐)**: 잔류율은 주파수 합과 별개의 누적기에서 계산되므로, 같은 스크레이프 안에서 주파수가 0을 찍는 동안에도 CPU 사용률은 항상 맞았다. 이것이 이 결함이 눈에 띄지 않고 배포되어 지속될 수 있었던 이유이기도 하다. `all_smi_cpu_frequency_mhz{...} 0` 옆에 정상적으로 움직이는 사용률 숫자가 있으면 고장으로 보이지 않고, 그냥 유휴 상태인 머신처럼 보인다.
- **문제 3 (GPU 경로의 테이블 조회가 일반화되어 있지 않았음)**: `load_gpu_frequencies`는 GPU 전용으로 작성되어 정확히 `voltage-states9*` 키만 읽고 단일 `Vec<u32>`만 반환했다. 같은 패턴을 클러스터 두 개(P, E)에 더 쓰려면 클러스터별로 함수를 복제하는 대신 키가 있는 다중 테이블 구조가 필요했다.
- **문제 4 (채널 분류에 잠재된 결함 두 가지)**: 클러스터 분류는 원본 채널 이름에 대고 임시로 짠 `starts_with`/`contains` 검사로 이뤄졌다. M1/M2 Ultra 같은 멀티 다이 패키지는 모든 채널에 `DIE_0_`/`DIE_1_` 접두사를 붙이는데, `DIE_0_ECPU_CPU0` 같은 이름이 매칭된 것은 `contains("ECPU")` 규칙이 우연히 맞았기 때문일 뿐이다. M5 Super 클러스터 규칙 `starts_with("MCPU0")`은 문자열 맨 앞에서만 매칭되므로, 가상의 `DIE_0_MCPU0`는 다른 곳의 `contains` 규칙이 우연히 버텨준 것과 달리 전혀 매칭되지 않았을 것이다.
- **문제 5 (32비트 헤르츠 필드가 최신 P코어 클록을 담지 못함)**: `voltage-states*` 페이로드의 주파수 필드는 4바이트 리틀 엔디안 값이다. 이 필드가 담을 수 있는 최댓값은 약 4.295GHz이고, 이후 세대 애플 실리콘 P코어는 이를 넘어선다고 문서화되어 있다. 오버플로된 값은 크게 실패하는 대신 작고 틀린 주파수로 읽힌다.

이는 기존부터 있던 동작이지 동시에 진행되던 인텔 맥 작업(#312)이 만든 회귀가 아니다. 배포된 v0.25.0 Homebrew 바이너리를 #312 빌드와 나란히 돌려본 결과 같은 0이 재현되었고, 이는 회귀 구간이 두 작업 모두보다 앞선다는 뜻이다.

### 1.3 위험성

| 위험 | 영향 | 발생 가능성 |
|---|---|---|
| 애플 실리콘 배포 전체에서 CPU 주파수가 조용히 0을 찍음 | Medium (사용률은 맞는데 "0 MHz"만 찍는 대시보드 패널은 혼란스럽지만 장애는 아니다. 다만 P/E 클러스터를 다루는 화면은 전부 쓸모없어짐) | 이 수정 전에는 영향받는 머신 전부에서 확실 |
| 미래 칩의 pmgr 키 번호 체계가 문서화된 `voltage-states1/5-sram` 키와 어긋남 | Low (길이 매칭 폴백과 "길이가 안 맞아도 문서화된 키" 최후 수단이 모두 이 경우를 위해 존재) | Low. M5 Super 클러스터라는 문서화된 키 없는 사례에서 이미 검증됨 |
| 오버플로 보정이 정당한 비주파수 페이로드에 잘못 발동 | Low (보호 장치는 이미 32비트 상한 근처에서 받아들여진 값 다음에만 활성화되고, 거부된 항목은 그 값을 갱신하지 않음) | Low. 전용 회귀 테스트로 검증됨 |

---

## 2. 기술적 검토 사항

### 2.1 정확성

이 수정은 데이터 흐름 수준에서 순수하게 추가적이다. 새 `PMGR_VOLTAGE_STATES` 테이블이 GPU 전용이던 `GPU_FREQUENCIES` 캐시를 대신해 IOKit에서 읽는 단일 소스가 되고, `GPU_FREQUENCIES`는 그 위의 얇은 선택 함수(`select_gpu_frequency_table`)가 됐다. 그래서 GPU 경로의 기존 동작은 형태가 바뀌지 않고 데이터 출처만 바뀐다. `calc_gpu_freq_with_table`은 `calc_freq_with_table`로 이름이 바뀌어 CPU 클러스터 경로와 공유되며, 잔류율 가중 계산 자체의 로직은 그대로다. 파싱된 주파수를 검증하는 타당성 범위는 100MHz~6GHz로 (GPU 전용이던 100MHz~4GHz에서) 넓어졌지만, 옛 GPU 전용 상한에 걸리던 값들은 여전히 걸러낸다. GPU 클록은 두 범위 어느 쪽에도 넉넉히 들어가기 때문이다.

리뷰에서 구체적으로 확인한 결함 유형 하나: CPU 경로 확장이 이미 배포 중인 GPU 경로를 되돌릴 수 있는가. 구조적으로 불가능하다. `GPU_TABLE_KEYS`는 여전히 `voltage-states9-sram`/`voltage-states9`를 우선 찾고, CPU 클러스터 키(`voltage-states1*`, `voltage-states5*`)는 이름이 겹치지 않는 별개 속성이라 어느 한쪽 테이블 선택이 다른 클러스터 테이블을 키 매칭으로 잘못 반환할 수 없다. 원칙적으로 클러스터를 넘나들 여지가 있는 유일한 경로는 길이 매칭 폴백(채널의 활성 상태 수와 항목 수가 같은 테이블을 매칭)인데, 서로 다른 두 클러스터의 활성 상태 수가 우연히 같고 문서화된 키 조회가 둘 다 실패하는 경우에나 문제가 된다. 이 폴백은 우선 키가 아예 없을 때만 작동하고, 잘못된 테이블에서 나온 값도 같은 100MHz~6GHz 타당성 범위에 걸린다는 점에서 받아들일 만하다고 판단했다.

### 2.2 성능 관점

`PMGR_VOLTAGE_STATES`와 `GPU_FREQUENCIES` 모두 `OnceLock`을 통해 최초 사용 시 딱 한 번만 로드된다. CPU 경로가 이전보다 추가로 하는 계산은 채널당 하나, 즉 비유휴 잔류율 항목 수(`active_states`)를 세어 길이로 맞는 테이블을 고르는 것이다. 채널당 성능 상태 수는 보통 20개 미만이라 O(채널당 성능 상태 수)이고, 샘플마다가 아니라 수집 주기마다 채널당 한 번 실행된다.

### 2.3 호환성 및 의존성

- **Breaking Changes**: 없다. `all_smi_cpu_frequency_mhz`를 비롯한 지표들은 이름, 타입, 레이블이 그대로다. 값만 항상 0이던 것에서 올바른 값으로 바뀐다.
- **새로운 의존성**: 없다. 이미 조회하던 IOKit 노드에서 속성을 더 읽을 뿐이다.
- **호환성**: GPU 주파수 보고는 변화 없음을 확인했다(검증 실행에서 `all_smi_gpu_frequency_mhz`가 657에서 639로 움직였는데, 이는 부하에 따른 통상적인 순간 변동이지 회귀가 아니다. 지표군과 계산 경로 자체는 손대지 않았다). 수정 범위는 `src/device/macos_native/ioreport.rs`에 한정되며 애플 실리콘에만 영향을 준다. 다른 플랫폼 리더는 건드리지 않는다.

### 2.4 코드 품질

새 단위 테스트 13개는 모두 검증에 쓴 M1 Ultra에서 그대로 캡처한 잔류율 히스토그램과 pmgr 테이블로 만들어졌다. 그래서 픽스처가 합성 데이터가 아니라 실제 하드웨어 데이터다. `test_cpu_performance_states_are_not_numeric`는 원래 버그 자체를 고정한다. 실제 P클러스터 히스토그램을 옛 전략(`calc_freq_from_residencies` 단독)에 통과시키면 잔류율은 맞으면서 0MHz가 나온다는 것을 확인하는데, 이는 수정에 대한 테스트가 아니라 결함 자체에 대한 회귀 테스트다. `test_p_cluster_frequency_from_real_m1_ultra_sample`과 그 E클러스터 짝은 같은 입력에서 보정된 값(잔류율 72.10%에서 3226MHz, 73.14%에서 1846MHz)을 확인한다. 나머지 테스트는 테이블 파싱(주파수 수용, 주기 테이블 거부, 32비트 오버플로, 잘린 페이로드), `DIE_` 접두사 제거, 단일 다이·멀티 다이·M5 명명 전반의 클러스터 분류, 테이블 선택의 모든 분기를 다룬다.

---

## 3. 기술적 선택과 그 이유

### 3.1 GPU 모양 캐시를 하나 더 두는 대신, 속성 이름으로 키를 매긴 공유 voltage-states 테이블 하나

**컨텍스트**: GPU 경로에는 이미 좁은 범위에서 동작하는 해법이 있었다. `voltage-states9-sram`/`voltage-states9`만 읽어 평평한 `Vec<u32>`로 로드하는 것. CPU 경로는 테이블이 두 개(P, E) 더 필요했고, 문서화된 키가 아예 없는 것(M5 Super)도 하나 있었다.

| 선택지 | 장점 | 단점 |
|---|---|---|
| GPU 로더를 클러스터마다 복제, 거의 동일한 함수 세 개 | 어느 한 코드 경로 변경은 최소화됨 | IOKit 속성 스캔 로직이 세 배로 늘고, 각 복제본을 손으로 맞춰야 함 |
| **채택: `(속성 이름, 주파수 목록)` 쌍으로 이뤄진 공유 `PMGR_VOLTAGE_STATES` 테이블 하나, 클러스터별 선택 함수** | pmgr 노드 속성 스캔이 한 번뿐이고, GPU와 CPU 모두 같은 캐시 위의 얇은 선택이 됨 | 선택 로직이 "문서화된 키 존재", "길이 매칭 폴백", "쓸 만한 테이블 없음"이라는 세 결과를 각각 다뤄야 함 |
| 호출자가 요청하는 특정 키만 그때그때 파싱, 공유 캐시 없음 | 아무도 쓰지 않는 테이블을 로드하지 않음 | 조회가 어긋날 때마다 IOKit 속성을 다시 스캔함. `OnceLock` 패턴이 노리던 "한 번만 로드" 특성을 잃음 |

**선택 이유**: pmgr 노드는 호출자가 결국 어떤 키를 원하든 상관없이 `voltage-states*` 속성 전부를 한 번의 속성 딕셔너리 스캔으로 내놓는다. 그러니 한 번 스캔해서 전부 캐시하는 쪽이 한 키만 스캔하는 것과 IOKit 왕복 비용이 같고, 문서화되지 않은 키 번호 체계를 가진 칩(M5 Super 클러스터)이 길이 매칭 폴백으로 풀리게 하는 유일한 형태이기도 하다. 그 폴백은 이름을 알아본 테이블뿐 아니라 모든 테이블을 봐야 하기 때문이다.

### 3.2 멀티 다이 이름을 매칭 규칙에서 개별 대응하는 대신, 분류 전에 `DIE_<n>_` 접두사를 제거

**컨텍스트**: M1/M2 Ultra는 두 다이를 하나의 패키지로 합치고, IOReport는 모든 채널에 `DIE_0_`/`DIE_1_` 접두사를 붙인다(`DIE_0_ECPU_CPU0`). 단일 다이 제품은 맨 이름(`ECPU0`)을 보고한다. 옛 분류 규칙(`starts_with('E')`, `contains("ECPU")` 등)은 멀티 다이 이름에도 여전히 매칭됐는데, 그건 오직 `contains`가 문자열 어디에 있든 상관하지 않기 때문이었다.

**발견한 사실**: 이것은 올바른 게 아니라 취약한 것이었다. M5 Super 클러스터 규칙 `starts_with("MCPU0")`는 위치 0에서의 매칭을 요구한다. 가상의 `DIE_0_MCPU0`는 이 검사에 실패했을 것이다. 다른 곳의 `contains` 규칙이 우연히 계속 버텨준 것과 대조적이다. 모든 분류 규칙이 접두사를 견딘다는 보장은 어디에도 없었고, 그건 우연이었다.

**채택한 수정**: `strip_die_prefix`가 어떤 분류 규칙이 돌기 전에 앞의 `DIE_<숫자>_`를 제거한다. 그래서 `starts_with`를 포함한 모든 규칙이 패키지 구조와 무관하게 같은 정규화된 이름 위에서 동작한다. 단일 다이, 멀티 다이, M5 명명 전반에 걸쳐 직접 테스트로 검증했다.

### 3.3 32비트 오버플로는 이미 상한 근처에 있는 항목 다음에만 보정하고, 미리 앞질러 보정하지 않음

**컨텍스트**: `voltage-states*` 페이로드의 주파수 필드는 4바이트 리틀 엔디안 값이다. 이 필드는 약 4.295GHz(`2^32 - 1` Hz)를 넘는 클록을 표현할 수 없고, 이후 세대 애플 실리콘 P코어는 이를 넘어선다고 문서화되어 있다.

| 선택지 | 장점 | 단점 |
|---|---|---|
| 이전 항목보다 작은 값이면 무조건 `2^32`를 더함 | 단순함 | 실제로 하강하는 테이블을 잘못 건드려 망가뜨릴 수 있음 |
| **채택: 이전에 받아들인 항목이 이미 4GHz 보호 기준 이상이고, 원시값이 그보다 작을 때만 `+2^32` 보정을 적용** | 오버플로의 특징(근-상한 항목 바로 다음에 갑자기 떨어지는 오름차순 테이블)을 정확히 이 조건이 잡아낸다. 상한 근처가 아닌 테이블은 손대지 않음 | 이전 원시값이 아니라 이전에 "받아들여진" 값을 추적해야 하므로, 거부된 비주파수 항목이 보호 장치를 발동시키지 않게 신경 써야 함 |
| 페이로드를 5바이트 이상으로 해석하도록 필드 폭을 넓힘 | - | 애플이 공개하지 않은 페이로드 레이아웃을 역공학해야 한다는 추측에 기댐. 온디스크 레이아웃 자체가 바뀐다는 증거는 없고, 값이 오버플로될 수 있다는 것만 확인됨 |

**선택 이유**: `voltage-states*` 테이블은 구조상 오름차순이다(각 항목이 이전보다 높은 성능 상태). 그러니 이미 32비트 상한 근처인 항목 바로 다음에 값이 떨어지는 것은 진짜 하강이 아니라 오버플로임을 시사한다. 이전 원시 바이트 읽기가 아니라 이전에 "받아들여진" 값을 기준으로 보호하는 것이, 관련 없는(비주파수) 페이로드가 이 보정을 잘못 발동시키지 않게 만드는 핵심이다. 거부된 항목은 구조상 그 추적값을 절대 갱신하지 않기 때문이다.

---

## 4. 구현 상세

### 4.1 아키텍처 변경

```
[변경 전]
GPU 채널  --> load_gpu_frequencies() --> Vec<u32> (voltage-states9*만)
                                            |
                                            v
                                  calc_gpu_freq_with_table()

CPU 채널  --> calc_freq_from_residencies()   # 상태 "이름"을 MHz 정수로 파싱
                                                애플 실리콘 이름은 상징적(V0P14, ...)
                                                -> 파싱 실패 -> 0

[변경 후]
pmgr 노드  --> load_pmgr_voltage_states() --> PMGR_VOLTAGE_STATES: [(키, Vec<u32>)]
                                                    |
                            +-----------------------+-----------------------+
                            v                                               v
               select_gpu_frequency_table()                 select_cpu_frequency_table(cluster, active_states)
                            |                                               |
                            v                                               v
                  calc_freq_with_table()  <-------- 공유 -------->  calc_freq_with_table()
                   (구 calc_gpu_freq_with_table)

CPU 채널 분류: strip_die_prefix() -> classify_cpu_channel() -> CpuCluster
```

### 4.2 주요 코드 변경

**파일: `src/device/macos_native/ioreport.rs` (CPU 채널의 주파수 테이블 선택)**
```rust
let Some(cluster) = classify_cpu_channel(&item.channel) else {
    return;
};

// IOReport는 CPU 성능 상태 이름을 상징적으로 짓는다(`IDLE`, `V0P14`,
// ... `V14P0`). 절대 메가헤르츠가 아니므로, 클록은 이 클러스터의
// pmgr voltage-states 테이블에서 와야 한다. 그 조인이 없으면
// 모든 CPU 주파수는 0을 찍는다.
let active_states = residencies
    .iter()
    .filter(|(name, _)| !is_idle_state(name))
    .count();
let table = select_cpu_frequency_table(get_pmgr_voltage_states(), cluster, active_states);

let (freq, residency) = match table {
    Some(table) if !table.is_empty() => Self::calc_freq_with_table(&residencies, table),
    _ => Self::calc_freq_from_residencies(&residencies),
};
```
**변경 이유**: CPU 경로에 없었던 바로 그 조인이다. `select_cpu_frequency_table`은 채널 자신의 활성 상태 수와 길이가 맞는지 확인한 뒤 클러스터별 문서화된 키를 우선하고, 그다음 길이가 맞는 아무 테이블로 물러나며, 최후에는 길이가 안 맞아도 문서화된 키를 반환한다. 부분적으로라도 매핑된 클록이 0MHz를 보고하는 것보다 낫다는 판단이다.

**파일: `src/device/macos_native/ioreport.rs` (32비트 오버플로 보정)**
```rust
let freq_hz = if prev_hz >= WRAP_GUARD_HZ && raw_hz < prev_hz {
    raw_hz + U32_SPAN_HZ
} else {
    raw_hz
};

if !(MIN_FREQ_HZ..=MAX_FREQ_HZ).contains(&freq_hz) {
    continue;
}

prev_hz = freq_hz;
frequencies.push((freq_hz / 1_000_000) as u32);
```
**변경 이유**: `prev_hz`는 "받아들여진" 항목에서만 갱신된다. 그래서 비주파수 데이터로 이뤄진 페이로드(받아들여지는 범위에 절대 들어가지 않는다)는 오버플로 보호 장치를 절대 발동시킬 수 없다. 이것이 이 하드웨어에서 헤르츠가 아니라 클록 주기를 담고 있는 평범한 `voltage-states1`/`voltage-states5` 키를, 그럴듯해 보이는 틀린 주파수로 보정하는 대신 깔끔히 거부하게 만드는 장치다.

### 4.3 데이터 모델 변경

와이어 포맷 변경은 아니다. 내부적으로는 GPU 전용이던 `GPU_FREQUENCIES: OnceLock<Vec<u32>>` 캐시가 이제 더 일반적인 `PMGR_VOLTAGE_STATES: OnceLock<Vec<(String, Vec<u32>)>>`를 근거로 삼고, `calc_gpu_freq_with_table`은 더 이상 GPU 전용이 아님을 반영해 `calc_freq_with_table`로 이름이 바뀌었다. Prometheus 지표 이름, 타입, 레이블은 전혀 바뀌지 않았다.

---

## 5. 학습 포인트

### 5.1 잔류율 히스토그램과 주파수 테이블은 서로 독립된 별개의 하드웨어 지식이다

**개념**: IOReport는 어떤 블록이 각 성능 상태에 *얼마나 오래* 머물렀는지(잔류율)만 알려줄 뿐, 그 상태가 *어떤 클록*으로 도는지는 절대 알려주지 않는다. 애플 실리콘에서 상태 이름은 상징적인 버전 태그(`V0P14`)일 뿐 클록 값이 아니므로, 메가헤르츠 숫자를 얻으려면 항상 상태 인덱스를 클록에 대응시키는 두 번째 데이터 소스가 필요하다. 이 플랫폼에서 그것이 바로 IOKit pmgr 노드의 `voltage-states*` 속성이다.

**이 PR에서의 적용**: GPU 경로는 이를 이미 이해하고 `voltage-states9*`에 조인하고 있었다. CPU 경로의 `calc_freq_from_residencies`는 상태 이름 자체가 숫자로 파싱될 거라고 조용히 가정했는데, 이는 다른 애플 제품군 IOReport 채널 일부에서는 맞지만 애플 실리콘 CPU 클러스터에서는 절대 맞지 않는다.

**예시 코드**:
```rust
fn select_cpu_frequency_table<'a>(
    tables: &'a [(String, Vec<u32>)],
    cluster: CpuCluster,
    active_states: usize,
) -> Option<&'a [u32]> {
    // 1. 문서화된 키, 길이 확인
    // 2. 길이가 맞는 아무 테이블. `-sram` 접미사를 우선함
    // 3. 최후 수단으로 길이가 안 맞아도 문서화된 키
}
```

### 5.2 이웃한 지표가 계속 맞으면서 움직이면, 버그는 크래시보다 눈에 띄기 어렵다

**개념**: 잔류율(따라서 사용률)과 주파수는 같은 IOReport 샘플에서 나오지만 서로 다른 누적기로 계산된다. 주파수 누적기가 고장 나도 사용률 누적기는 계속 동작했으므로, 스크레이프 출력은 "명백히 고장 난 서브시스템"이 아니라 "사용률은 맞고 주파수만 0에 박힘"처럼 보였다.

**이 PR에서의 적용**: 이것이 이 결함이 배포되어 지속될 수 있었던 바로 그 이유다. 같은 스크레이프에서 `all_smi_cpu_frequency_mhz 0`이 `all_smi_cpu_utilization_percent 34.2` 옆에 있으면, 특별히 주파수 패널을 들여다보고 있지 않는 한 그럴듯해 보인다. 이 수정은 잔류율과 주파수를 계산하는 방식(여전히 별도 누적기다) 자체를 바꾸지 않는다. 주파수 누적기의 데이터 출처만 고친다.

### 5.3 장치가 보고하는 이진 페이로드의 32비트 필드 오버플로는 방향성 있게만 안전하게 보정할 수 있다

**개념**: 고정 폭 필드가 오버플로될 수 있을 때, 이를 보정하려면 기저 수열이 단조롭다는 방향성 가정과 오버플로 경계 근처에서만 발동하는 보호 장치가 함께 필요하다. 그러지 않으면 보정 자체가 우연히 감소하는 정당한 데이터를 손상시키는 원인이 된다.

**이 PR에서의 적용**: `voltage-states*` 테이블은 구조상 오름차순이므로, 이미 4.295GHz 근처인 항목 바로 다음의 하강은 특정적으로 오버플로를 가리키지 진짜 주파수 하강을 가리키지 않는다. 이 보호 장치의 정밀함, 즉 이전 원시값이 아니라 이전에 "받아들여진" 값만 추적한다는 점이 비주파수 페이로드(이 하드웨어에서 클록 주기를 담고 있는 평범한 `voltage-states1`/`voltage-states5` 키)가 애초에 보정 경로에 들어오지 못하게 막는다. 그런 항목은 `prev_hz`가 갱신되기 전에 거부되기 때문이다.

---

## 6. 추가 학습

### 핵심 용어

| 용어 | 설명 | 관련성 |
|---|---|---|
| IOReport | 채널별 잔류율 히스토그램을 노출하는 애플의 저수준 전력·성능 텔레메트리 API | 이 PR이 주파수 테이블에 조인하는 CPU·GPU 성능 상태 데이터의 출처 |
| pmgr / clpc | `voltage-states*` 속성을 발행하는 IOKit `AppleARMIODevice` 노드 | 실제 클록-상태 대응이 사는 곳. 이 PR의 핵심 수정은 CPU 경로에서 여기 조인하는 것 |
| `voltage-states*` | 오름차순 `(주파수, 전압)` 쌍을 담은, 클러스터별 8바이트 항목 속성 | GPU뿐 아니라 CPU 클러스터에도 이제 이 PR이 읽는 테이블 |
| 잔류율(Residency) | 샘플링 구간 중 성능 상태가 활성이었던 비율 | 주파수와 별개 누적기로 계산됨. 이 버그 내내 계속 맞았음 |
| 32비트 오버플로 | 고정 폭 필드가 넘쳐서 0부터 다시 시작하는 것 | 4.295GHz를 넘는 클록에 대해 이 PR에서 보정됨 |
| DIE_\<n\>\_ 접두사 | 멀티 다이 패키지(M1/M2 Ultra)의 IOReport 채널 이름 접두사 | 분류 전에 제거되어 멀티 다이·단일 다이 채널 이름이 규칙 하나를 공유함 |

### 관련 기술/프레임워크

- IOReport와 IOKit: 애플의 텔레메트리·드라이버 매칭 프레임워크. 이 플랫폼에서는 별도 권한 상승 없이 쓰인다.
- 애플 실리콘 성능 상태 명명(`V<n>P<m>`): 내부적이고 문서화되지 않은 명명 체계. 이 PR은 이를 값으로 파싱하려 하지 않고 그대로 불투명하게 취급한다.

### 관련 PR/이슈

- 이슈 #314: 이 PR이 닫는 이슈.
- PR #312 (인텔 맥 지원, 이슈 #306): 배포된 v0.25.0 바이너리와의 나란히 비교로 이 회귀의 원인이 아님을 확인함.
- 이 보고서 2.3절에서 GPU 지표군이 영향받지 않음을 확인함.

---

## 7. 변경 요약

### 통계

| 항목 | 값 |
|---|---|
| 변경 파일 | 1 (`src/device/macos_native/ioreport.rs`) |
| 추가 줄 | +626 |
| 삭제 줄 | -119 |
| 커밋 | 1 |
| 새 단위 테스트 | 13 |

### 카테고리별 변경

| 분류 | 내용 |
|---|---|
| 정확성 | CPU 클러스터 주파수가 이제 pmgr voltage-states 테이블 조인으로 해석됨. 기존 GPU 전략과 동일한 방식 |
| 방어 로직 | 분류 전 `DIE_<n>_` 접두사 제거. 4.295GHz를 넘는 클록에 대한 32비트 오버플로 보정 |
| 리팩터링 | `calc_gpu_freq_with_table`을 `calc_freq_with_table`로 이름 변경해 GPU·CPU 경로가 공유. `GPU_FREQUENCIES`는 새 공유 `PMGR_VOLTAGE_STATES` 캐시 위의 선택으로 바뀜 |
| 테스트 | M1 Ultra에서 그대로 캡처한 잔류율 히스토그램·pmgr 테이블로 만든 새 단위 테스트 13개 |

### 관련 커밋

| SHA | 유형 | 메시지 |
|---|---|---|
| `456bf7d4` | fix | derive Apple Silicon CPU cluster frequency from the pmgr table |

`main`에 `02b2e6d2`로 병합됨. #314를 닫는다.

---

## 8. 후속 조치

### 필수

없음. 이 수정은 실제 M1 Ultra 하드웨어에서 API 지표 출력과 TUI 렌더러 양쪽으로 검증되었고, 근본 조사(잔류율과 주파수의 분리, 테이블 선택, 오버플로)는 합성 픽스처가 아니라 실제로 캡처한 데이터로 만든 회귀 테스트가 뒷받침한다.

### 모니터링 필요

- 미래의 애플 실리콘 세대가 `voltage-states1*`/`voltage-states5*`와 맞지 않는 pmgr 키 번호 체계를 가진 CPU 클러스터를 도입하고, 그 활성 상태 수가 다른 클러스터와 우연히 겹치는 경우. 이것이 길이 매칭 폴백 혼자서는 구분할 수 없는 유일한 시나리오다. M5 Super 클러스터라는 문서화된 키 없는 사례는 이미 안전하게 처리되고 있고, 진짜 충돌은 아직 관측된 바 없다.

### 향후 개선 사항

- PR에서 별도로 제안된 것은 없다. 수정 범위는 이슈에서 지목한 주파수 계산 결함에 좁게 한정되어 있다.

---

## 부록

### A. 테스트 결과

- `cargo test --lib device::macos_native`: 49개 통과.
- `cargo test --lib device::cpu_macos`: 9개 통과.
- `cargo test --lib ui::renderers::cpu_renderer`: 8개 통과.
- `cargo clippy --lib --tests -- -D warnings`: 클린.
- `cargo fmt --check`: 클린.
- Apple M1 Ultra에서 `all-smi api`, `/metrics` 전후:

```
# 이전
all_smi_gpu_frequency_mhz{gpu="Apple M1 Ultra GPU",...} 657
all_smi_cpu_frequency_mhz{cpu_model="Apple M1 Ultra",...} 0
all_smi_cpu_p_cluster_frequency_mhz{cpu_model="Apple M1 Ultra",...} 0
all_smi_cpu_e_cluster_frequency_mhz{cpu_model="Apple M1 Ultra",...} 0

# 이후
all_smi_gpu_frequency_mhz{gpu="Apple M1 Ultra GPU",...} 639
all_smi_cpu_frequency_mhz{cpu_model="Apple M1 Ultra",...} 2646
all_smi_cpu_p_cluster_frequency_mhz{cpu_model="Apple M1 Ultra",...} 3228
all_smi_cpu_e_cluster_frequency_mhz{cpu_model="Apple M1 Ultra",...} 2064
```

1분간 반복 샘플링한 결과 값은 부하를 따라 움직이며 하드웨어 한계 안에 머문다(P클러스터 3017~3228MHz, 테이블 최댓값 3228MHz 대비. E클러스터 1106~2064MHz, 최댓값 2064MHz 대비).

- TUI 검증: pty에는 창 크기가 없고 `ui/chrome.rs`는 폭 0인 터미널에서 패닉을 일으키므로, 실제 하드웨어에서 얻은 살아있는 `CpuInfo`를 TUI가 호출하는 것과 같은 `print_cpu_info` 함수에 직접 넣어 렌더러를 구동했다.

```
# 이전
p_cluster_frequency_mhz: Some(0), e_cluster_frequency_mhz: Some(0)
CPU  Apple M1 Ultra @ cube.loca  Arch:arm64  Sockets: 1  Cores:16P+ 4E  Freq:       0+0MHz  Temp: 56C

# 이후
p_cluster_frequency_mhz: Some(2583), e_cluster_frequency_mhz: Some(1195)
CPU  Apple M1 Ultra @ cube.loca  Arch:arm64  Sockets: 1  Cores:16P+ 4E  Freq: 2.58+1.20GHz  Temp: 53C
```

이때 쓴 프로브는 커밋 전에 제거했고, `ioreport.rs` 외의 테스트 파일은 건드리지 않았다.

### B. 성능 벤치마크

별도로 벤치마크하지 않았다. 채널당 추가된 비용은 비유휴 잔류율 항목(보통 20개 미만)을 세는 것 하나뿐이고 수집 주기마다 채널당 한 번 실행된다. pmgr 테이블 자체는 프로세스당 `OnceLock`을 통해 딱 한 번만 로드된다.

### C. 참고 자료

- Apple: IOReport(문서화되지 않음. `mactop` 등 커뮤니티 도구가 역공학한 방식을 원래 GPU 로더가 명시적으로 참고함)
- Apple: IOKit `AppleARMIODevice`, pmgr/clpc 노드, `voltage-states*` 속성
- 이슈 #314: 이 보고서가 근거로 삼은 근본 원인 서술과 검증 데이터. diff와 교차 확인함
