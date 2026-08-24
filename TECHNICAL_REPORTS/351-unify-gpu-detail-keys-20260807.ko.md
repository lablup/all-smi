# 기술 보고서: PR #351 - GPU detail 키를 공유 리더 규약으로 통일

**일자**: 2026-08-07  
**상태**: 완료  
**관련 항목**: PR #351, PR #348 및 PR #349의 후속  
**위험 수준**: 낮음 (이 키들 중 태그된 릴리스에 나간 것이 없음)

---

## 요약

PR #351은 Windows ADL 리더의 `detail` 맵 키를 다른 모든 리더가 이미 따르는 규약에 맞췄습니다. 키는 양을 이름 짓고 단위는 값에 싣습니다. 값 `1450`을 가진 `Fan Speed (RPM)`은 값 `1450 RPM`을 가진 `Fan Speed`가 되고, 형제 키 네 개도 같은 방식으로 옮겼습니다.

#348이 추가한 DXGI VRAM 진단 키 두 개는 한정어를 유지합니다. `(this process)`는 단위가 아니라 범위이기 때문입니다. 값에만 명시적인 `bytes`를 붙였습니다.

---

## 1. 문제 정의

네 리더가 `detail` 맵으로 팬 속도를 게시하고 있었고, 그중 셋은 규약만으로 합의돼 있었습니다.

| 리더 | 키 | 값 |
|------|----|-----|
| `amd.rs` (Linux) | `Fan Speed` | `1450 RPM` |
| `intel_gpu_linux/sources.rs` | `Fan Speed` | `1450 RPM` |
| `intel_gpu_level_zero/apply.rs` | `Fan Speed` | `1450 RPM` |
| `amd_adl.rs` (#349에서 추가) | `Fan Speed (RPM)` | `1450` |

ADL 리더만 예외였고, #348도 자신의 VRAM 진단 키 두 개에서 같은 일을 했습니다. 따라서 `Fan Speed`를 키로 잡은 소비자는 Windows AMD 경로를 조용히 놓쳤고, 맵을 정규화하려는 코드는 한 양에 대해 두 철자를 알아야 했습니다.

## 2. 변경 요약

| 항목 | 값 |
|------|----|
| 변경 파일 | 2개 |
| 추가 줄 | 91줄 |
| 삭제 줄 | 23줄 |
| 이름 변경 키 | 5개 |
| 깨진 소비자 | 0개 (3.3 참조) |

### 이름 변경 내역

| 이전 | 이후 | 값 |
|------|------|-----|
| `Fan Speed (RPM)` | `Fan Speed` | `1450 RPM` |
| `Memory Clock (MHz)` | `Memory Clock` | `1250 MHz` |
| `Hotspot Temperature (C)` | `Hotspot Temperature` | `81 C` |
| `Memory Temperature (C)` | `Memory Temperature` | `70 C` |
| `Memory Controller Activity (%)` | `Memory Controller Activity` | `44%` |
| `VRAM Budget (this process)` | 변경 없음 | `7000000000 bytes` |
| `VRAM Usage (this process)` | 변경 없음 | `123456 bytes` |

## 3. 기술적 선택과 그 이유

### 3.1 VRAM 키 두 개는 한정어를 유지

`(this process)`는 단위가 아니라 **범위**입니다. 이 DXGI 수치는 시스템 전체가 아니라 프로세스 범위이며, 한정어를 떼면 그 레이블이 막으려고 존재하는 바로 그 오독을 부릅니다. #348이 문구를 그렇게 고른 이유가 그것입니다. 값에만 기존 리더의 `VRAM Total`과 맞춰 `bytes` 단위를 붙였습니다.

### 3.2 `Temperature`는 이제 영하일 때만 발행

이 키는 부호 없는 `GpuInfo.temperature` 필드가 0으로 깎아 버리는 영하 다이 판독값을 보존하려고 존재합니다. 평범한 폴링에서는 그 필드를 복제할 뿐이어서, 타입 필드가 이미 담고 있지 않은 정보를 하나도 싣지 않은 행을 detail 맵에 더하고 있었습니다.

### 3.3 호환성 질문을 가정하지 않고 답했다

이 키들 중 태그된 릴리스에 나간 것은 없습니다. #348과 #349 모두 v0.25.0 이후, v0.26.0 이전에 머지됐습니다. 어떤 소비자도 옛 철자에 의존할 수 없으므로, 이 이름 변경은 정확히 이 시점에서 무료이며 한 달 뒤였다면 아니었을 것입니다.

### 3.4 주석이 아니라 테스트가 재발을 막는다

`detail_keys_follow_the_shared_reader_convention`은 새 철자와 옛 철자의 **부재**를 함께 단언하며, 주석에 규약이 갈라졌을 때의 구체적 비용 두 가지를 기록합니다. 다음에 이 양들을 게시하는 리더는 조용히 어긋나는 대신 테스트에서 실패합니다. 이는 이후 #365에서 `src/device/readers/detail_keys.rs`를 만들게 한 것과 같은 논리입니다.

## 4. 검증 결과

| 게이트 | 결과 |
|--------|------|
| `cargo fmt --check` | 통과 |
| `cargo clippy --all-targets -- -D warnings` | exit 0 |
| `cargo test` | exit 0, 23개 바이너리에서 3176 통과, 0 실패 |
| `cargo xwin check --target x86_64-pc-windows-msvc` | exit 0, 경고 0 |

테스트 수에 대한 기록입니다. 인접 보고서를 읽는 방식에 영향을 주므로 남깁니다. #348과 #349에 인용한 수치는 cargo 출력을 간헐적으로 잘라 먹는 셸 파이프를 통해 읽은 값이라 약간 적게 세어졌습니다. 파일로 캡처해 세면 여기서는 3176입니다. 세 PR 모두에서 권위 있는 신호는 exit code였고 그것은 내내 0이었습니다.

## 5. 결과 및 후속

- PR #351은 `cafb054`로 `main`에 squash merge되었습니다.
- 두 건은 이 PR에 접어 넣지 않고 별도로 등록했습니다.
  - 팬 속도를 실제 `GpuInfo` 필드로 승격해 `snapshot` JSON만이 아니라 TUI와 Prometheus에도 닿게 하는 것. 네 리더가 혜택을 보며, `Source: Fan`은 이미 `source__fan`으로 익스포트되는데 값은 그렇지 않았습니다. 이것이 **#360**이 되었습니다.
  - 다중 AMD GPU 귀속을 위한 ADL `AdapterInfo`. 이것이 **#361**이 되었습니다.
- 이 PR이 테스트로 고정한 규약은 이후 #365에서 `detail_keys.rs`라는 공유 거처를 얻었습니다. `Metrics Source` 키가 같은 종류의 어긋남을 다른 방향에서 보여 준 뒤였습니다.

---

## 부록: 핵심 키워드

| 키워드 | 설명 | 관련성 |
|-------|------|--------|
| `detail` 맵 | 리더가 타입 `GpuInfo` 필드와 함께 게시하는 무타입 문자열 맵 | 이름 변경된 다섯 키가 사는 곳 |
| 값-단위 규약 | 키는 양을 이름 짓고 값이 단위를 싣는다 | ADL 리더가 위반하던 규약 |
| 범위 한정어 | `(this process)`처럼 수치가 덮는 범위를 좁히는 레이블 | VRAM 키 두 개가 이름 변경에서 제외된 이유 |
