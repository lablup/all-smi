# 기술 보고서: PR #348 - DXGI와 PDH 기반 벤더 중립 Windows GPU 메트릭

**일자**: 2026-08-07  
**상태**: 코드 경로 기준 완료, 하드웨어 수용 기준은 미해결 (6절 참조)  
**관련 항목**: PR #348, 이슈 #346  
**위험 수준**: 중간 (기본 CI가 컴파일하지 않는 타겟의 신규 FFI 표면)

---

## 요약

PR #348은 벤더 SDK가 필요 없는 두 OS 기능 위에 만든 공유 Windows GPU 메트릭 계층 `src/device/readers/windows_gpu_perf.rs`를 추가하고, AMD와 Intel Windows 리더 양쪽을 여기에 연결했습니다. DXGI는 진짜 64비트 전용 메모리, 어댑터 LUID, PCI 벤더/디바이스 id를 제공하고 PDH는 엔진 사용률과 어댑터 메모리 사용량을 제공합니다.

그 전까지 두 리더는 WMI 전용 기준선이었습니다. `utilization`, `used_memory`, `frequency`, `power_consumption`이 `0`으로 하드코딩돼 있었고, `get_process_info()`는 빈 `Vec`을 반환했으며, WMI 스키마에서 `Win32_VideoController.AdapterRAM`이 `uint32`이기 때문에 4 GB를 넘는 카드의 `total_memory`가 틀렸습니다.

---

## 1. 문제 정의

Windows 모니터링 노드는 GPU를 감지했다고 보고하면서 의미 있는 수치는 전부 0으로 내보냈고, 정작 모니터링 대상이 될 가능성이 높은 카드에서 메모리 용량이 조용히 랩어라운드했습니다. WMI 스키마로는 둘 다 고칠 수 없습니다. 엔진 사용률 표면이 아예 없고, 메모리 필드는 정의상 32비트입니다.

두 벤더 리더가 같은 구멍을 갖고 있었으므로, 수정은 벤더별 패치가 아니라 공유 계층이어야 했습니다. 그러지 않으면 같은 코드가 두 벌 쓰이고 갈라집니다.

| 필드 | 이전 | 추가된 소스 |
|------|------|-------------|
| `total_memory` | `AdapterRAM`, 4 GB 초과 시 랩어라운드 | DXGI `DedicatedVideoMemory` (64비트) |
| `utilization` | `0` 하드코딩 | PDH `\GPU Engine(*)\Utilization Percentage` |
| `used_memory` | `0` 하드코딩 | PDH `\GPU Adapter Memory(*)\Dedicated Usage` |
| 프로세스별 행 | 빈 `Vec` | pid로 키를 잡은 PDH 엔진 인스턴스 |

## 2. 변경 요약

| 항목 | 값 |
|------|----|
| 변경 파일 | 10개 |
| 추가 줄 | 2301줄 |
| 삭제 줄 | 33줄 |
| 테스트 추가 | 28개 |
| 신규 직접 의존성 | `windows` 0.62 |

### 파일

| 파일 | 목적 |
|------|------|
| `src/device/readers/windows_gpu_perf.rs` | 공유 계층: 스냅샷 조립, 어댑터 매칭, 필드 적용. |
| `src/device/readers/windows_gpu_perf/dxgi.rs` | `DXGI_ADAPTER_DESC1` 열거: 용량, LUID, PCI id. |
| `src/device/readers/windows_gpu_perf/ids.rs` | 카운터 인스턴스 파싱과 PNPDeviceID 파싱(`parse_pnp_device_id` 포함). |
| `src/device/readers/windows_gpu_perf/pdh.rs` | 지속 PDH 쿼리, 카운터 수집, 엔진별/프로세스별 집계. |
| `src/device/readers/amd_windows.rs`, `intel_gpu_windows.rs` | 0을 게시하는 대신 공유 계층을 소비. |
| `src/doctor/checks/windows.rs` | 실패 모드를 구분하는 `windows.gpu.perf_counters` 검사. |
| `Cargo.toml`, `Cargo.lock` | `windows` 0.62를 전이 의존에서 직접 의존으로 승격. |

## 3. 기술적 선택과 그 이유

### 3.1 PDH 쿼리는 폴링마다가 아니라 지속적으로 유지

`Utilization Percentage`는 rate 카운터입니다. `PdhCollectQueryData` 한 번은 기준선을 세울 뿐 쓸 수 있는 값을 내지 않으므로, 순진하게 구현하면 두 번째 샘플을 만들려고 리더 안에서 sleep해야 합니다.

대신 쿼리를 한 번 열어 두고 매 폴링이 수집 한 번씩을 기여하게 했습니다. 기동 후 첫 폴링은 사용률을 보고하지 않고, 그 이후 모든 폴링은 실제 폴링 간격에 대한 비율을 보고합니다. `get_process_info()`는 다시 수집하지 않고 `latest()`로 같은 샘플을 재사용합니다. 다시 수집하면 비율 계산 구간이 절반이 되어 보고 부하가 대략 두 배가 됩니다.

### 3.2 사용률은 엔진 내에서는 합산, 엔진 사이에서는 최댓값

PDH 샘플 하나는 한 프로세스가 한 엔진에서 차지한 몫입니다. 따라서 프로세스에 대해 합산하면 그 엔진의 사용 비율이 나오고 이는 옳습니다. 반면 카드가 가진 여러 3D/Compute 엔진에 대해 합산하면 100%를 크게 넘는 값이 나와 모든 게이지가 끝까지 붙습니다.

엔진 사이 최댓값은 작업 관리자의 대표 GPU 퍼센트가 보고하는 값이므로 방어 가능하면서 익숙합니다. 이는 "LUID별로 3D와 Compute 엔진 타입을 합산"하라던 이슈 본문에서 의도적으로 벗어난 것이며, 조용히 채택하지 않고 명시합니다.

### 3.3 비디오 엔진은 사용률에서 제외

비디오를 디코딩하는 컴포지터는 셰이더 코어가 놀고 있는 동안에도 `VideoDecode`를 바쁘게 만듭니다. 이를 포함하면 유휴 데스크톱이 큰 0이 아닌 부하를 보고하게 되며, 이는 아무것도 보고하지 않는 것보다 나쁩니다.

### 3.4 DXGI `QueryVideoMemoryInfo`는 프로세스 범위이므로 장치 사용량으로 쓰면 안 됨

MSDN은 `CurrentUsage`와 `Budget`을 시스템이 아니라 이 프로세스의 관점으로 정의합니다. 둘 중 무엇이든 장치의 사용 메모리로 읽으면, 바쁜 GPU를 다른 모든 프로세스가 잡고 있는 양만큼 과소 보고하게 됩니다.

그래도 드러낼 가치가 있으므로 명확히 이름 붙인 진단 detail 필드(`VRAM Usage (this process)`)로 노출하고, `used_memory`는 PDH 어댑터 카운터에서 가져옵니다.

### 3.5 카운터는 `PdhAddEnglishCounterW`로 추가

카운터 경로 구성 요소는 비영어 Windows에서 지역화됩니다. 리터럴 영어 경로 `\GPU Engine(*)\Utilization Percentage`는 영어 전용 진입점을 통해서만 해석되므로, 그러지 않으면 독일어나 한국어 Windows 설치에서는 카운터를 하나도 찾지 못합니다.

## 4. 구현 상세

`snapshot()`은 DXGI 어댑터를 먼저 열거하고, PDH 샘플을 수집한 다음, 둘을 매칭합니다. 매칭은 LUID를 먼저 시도합니다. DXGI와 PDH 인스턴스 이름이 모두 LUID를 담고 있기 때문입니다. LUID 경로가 해석되지 않으면 WMI `PNPDeviceID`에 대해 `parse_pnp_device_id`로 PCI 벤더/디바이스 쌍 대조로 폴백합니다.

필드 적용은 각 벤더 리더가 아니라 이 계층 자신의 단계로 두었고, 이것이 AMD와 Intel 경로가 갈라지지 않게 막는 장치입니다.

## 5. 검증 결과

이 저장소에는 기본적으로 all-smi를 Windows용으로 컴파일하는 CI 잡이 없습니다. 유일한 Windows 잡은 설정되지 않은 저장소 변수 뒤에 게이트돼 있고, 그 잡의 주석 자체가 한 번도 실행된 적 없다고 적고 있습니다. 따라서 조치가 없으면 Windows 전용 코드는 자동 커버리지 0으로 나가며, 두 가지 조치를 취했습니다.

**1. 모듈을 `cfg(any(target_os = "windows", test))`로 게이트했습니다.** 기존 `intel_gpu_sysfs` 패턴과 같습니다. 이로써 카운터 인스턴스 파싱, 사용률 집계, PNPDeviceID 매칭, 필드 적용이 모두 Linux 테스트 러너에서 실행됩니다. 신규 테스트 28개.

**2. FFI 자체는 macOS에서 실제 릴리스 타겟으로 크로스 컴파일해 확인했습니다.**

| 게이트 | 결과 |
|--------|------|
| `cargo fmt --check` | 통과 |
| `cargo clippy --all-targets -- -D warnings` | 통과 |
| `cargo test` | 3118 통과, 0 실패 |
| `cargo xwin check --target x86_64-pc-windows-msvc` | 통과 |
| `cargo xwin clippy --target x86_64-pc-windows-msvc -- -D warnings` | 이 변경분에 대해 통과 |

Windows 크로스 체크에는 `cargo-xwin`과 `llvm-lib`가 필요했습니다(rustup의 `llvm-tools`가 제공하는 `llvm-ar`가 그 이름으로 `llvm-lib` 역할을 합니다). 그러지 않으면 `zstd-sys`가 macOS에서의 모든 Windows 크로스 컴파일을 막습니다.

### 기존 지적, 여기서 고치지 않음

Windows clippy는 이 PR이 건드리지 않는 파일에서 collapsible-`if` 린트 세 건을 보고합니다(`src/device/cpu_windows.rs` 두 곳, `src/device/windows_temp/amd_ryzen.rs` 한 곳). 그 타겟을 린트한 적이 없었기 때문에 이제야 드러난 것입니다. diff 범위를 유지하기 위해 그대로 두었습니다.

## 6. 결과 및 후속

- PR #348은 `55c6a1a`로 `main`에 squash merge되었습니다.
- 이슈 #346은 PR의 `Closes #346` 링크로 자동 종료되었습니다.
- **하드웨어 미검증**: 바쁜 AMD/Intel Windows 머신에서의 0이 아닌 사용률, 4 GB 초과 카드의 정확한 VRAM, 채워진 프로세스별 행. `windows.gpu.perf_counters` 진단 검사가 운영자 머신에서 바로 그 증거를 만들어 내기 위해 존재합니다. 이 검사는 "DXGI 어댑터 없음", "DXGI는 되지만 PDH가 인스턴스를 게시하지 않음"(VM과 RDP에서 정상), 전체 경로 성공을 구분합니다.
- Windows 타겟을 실제로 컴파일하는 CI 잡과 함께 처리할 가치가 있는 후속이며, 이 공개 저장소에서는 `windows-latest` 러너가 무료로 해 줍니다. 이것이 #368이 되었습니다.
- PR #349가 이 계층 위에 AMD ADL 계층을 올렸고, 이후 #365가 이 PR이 도입한 메모리 및 detail 키 처리의 결함 두 건을 교정했습니다.

---

## 부록: 핵심 키워드

| 키워드 | 설명 | 관련성 |
|-------|------|--------|
| PDH | Performance Data Helper, Windows 카운터 조회 API | 사용률과 어댑터 메모리의 출처 |
| DXGI 어댑터 LUID | 그래픽 어댑터의 로컬 고유 식별자 | DXGI 어댑터와 PDH 인스턴스를 잇는 기본 키 |
| rate 카운터 | 값이 구간에 대한 델타여서 두 번 수집이 필요한 카운터 | PDH 쿼리를 폴링마다가 아니라 지속 유지하는 이유 |
| `PdhAddEnglishCounterW` | 지역화되지 않은 영어 경로로 카운터를 추가 | 비영어 Windows에서 경로가 해석되기 위한 필수 조건 |
