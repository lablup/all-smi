# 기술 보고서: PR #360 - GPU 팬 속도를 일급 `GpuInfo` 필드로 승격

**일자**: 2026-08-07  
**상태**: 완료  
**관련 항목**: PR #360, 이슈 #352, PR #351에서 등록한 후속  
**위험 수준**: 중간 (49개 파일 변경, 신규 익스포트 메트릭 패밀리 추가)

---

## 요약

팬 속도는 무타입 `detail` 맵으로만 게시되고 있어서 `all-smi snapshot` 출력에는 닿았지만 TUI와 Prometheus 익스포터에는 한 번도 닿지 못했고, 소비자는 네 리더가 규약만으로 합의한 `"1450 RPM"` 문자열을 다시 파싱해야 했습니다. PR #360은 이를 `GpuInfo::fan_speed_rpm`으로 승격하고 익스포터, 원격 메트릭 파서, GPU 뷰까지 배선했습니다.

타입 필드는 `#[serde(default)]`가 붙은 `Option<u32>`이므로, 필드가 존재하기 전에 만들어진 스냅샷과 원격 페이로드도 여전히 역직렬화됩니다. `None`은 장치에 회전 센서가 없다는 뜻이며, 모든 소비자는 `0`을 대입하는 대신 아무것도 렌더링하지 않습니다. `0`은 멈춰 버린 팬과 구분되지 않기 때문입니다.

---

## 1. 문제 정의

`Source: Fan`은 이미 `source__fan`으로 익스포트되고 있었는데, 정작 그것이 설명하는 값은 전혀 익스포트되지 않았습니다. 네 리더가 데이터를 갖고 있었지만 넣을 타입 채널이 없었습니다.

| 리더 | 소스 |
|------|------|
| `amd.rs` | Linux amdgpu 센서 |
| `intel_gpu_linux.rs` | hwmon `fan1_input` |
| `intel_gpu_level_zero/apply.rs` | Sysman fan 패밀리 |
| `amd_adl.rs` | Windows PMLog |

값을 원하는 소비자는 모두 문자열을 다시 파싱해야 했고, 문자열 형태가 리더마다 달랐기 때문에(`"1450 RPM"` 대 Level Zero의 `"1600 RPM (40%)"`) 각 소비자가 그 전부를 알아야 했을 것입니다.

## 2. 변경 요약

| 항목 | 값 |
|------|----|
| 변경 파일 | 49개 |
| 추가 줄 | 922줄 |
| 삭제 줄 | 43줄 |
| 신규 메트릭 패밀리 | `all_smi_gpu_fan_speed_rpm` |
| 갱신한 생성 지점 | 48곳 (전부 `None` 사용) |

### 핵심 파일

| 파일 | 변경 |
|------|------|
| `src/device/types.rs` | `#[serde(default)]`가 붙은 `fan_speed_rpm: Option<u32>` 추가. `None`이 "회전 센서 없음"을 뜻하며 결코 `0`으로 렌더링하면 안 된다고 문서화. |
| 리더 4종 | 기존 `Fan Speed` detail 기록과 같은 소스 값에서, 같은 분기에서 타입 필드를 설정. |
| `src/api/metrics/gpu.rs` | 존재 여부로 게이트해 게이지를 익스포트, `all_smi_gpu_performance_state` 블록을 따름. |
| `src/network/metrics_parser.rs` | 익스포트된 시리즈에서 필드를 복원, 신규 `MAX_GPU_FAN_RPM` 상한 100000 적용. |
| `src/ui/renderers/gpu_renderer.rs` | 기존 온도/P-state 보조 행에 `Fan:1450rpm` 렌더링. |
| `API.md`, `docs/LIB_mode.md` | 메트릭과 필드 문서화. |

## 3. 기술적 선택과 그 이유

### 3.1 `None`은 결코 `0`이 아니라 부재로 렌더링

대입된 `0`은 멈춰 버린 팬과 구분되지 않으며, 그것이야말로 운영자가 가장 신뢰할 수 있어야 하는 판독값입니다. 수동 냉각 데이터센터 카드와 듀티 사이클만 노출하는 드라이버는 둘 다 정당하게 회전 센서를 갖지 않으므로, 부재는 오류가 아니라 정상 상태이며 진짜 0과 구분 가능한 상태로 유지돼야 합니다.

### 3.2 detail 기록은 의도적으로 유지

스냅샷이 여기에 의존하고, `intel_gpu_level_zero::apply_fan`이 `detail.contains_key("Fan Speed")`를 리더 간 덮어쓰기 가드로 사용해 Linux hwmon 판독값이 이후의 Level Zero 샘플보다 우선하게 합니다. 문자열을 없앴다면 그 가드도 함께 사라졌을 것입니다.

두 기록은 동일한 early return 뒤에 놓여 있으므로 필드와 문자열이 항상 같은 샘플을 설명합니다. 이 불변식이 3.4의 익스포터 폴백을 안전하게 만드는 근거입니다.

### 3.3 듀티 사이클만 있는 판독은 타입 필드를 비워 둔다

퍼센트만 담은 Level Zero 판독(`rpm == None`, `percent == Some(40)`)은 `fan_speed_rpm`을 채우지 않습니다. `_rpm`이라는 이름의 필드에 담긴 퍼센트는 엄청나게 틀린 RPM으로 익스포트됩니다. 퍼센트 값 자체는 여전히 detail 문자열로 스냅샷에 닿으므로, 정보를 잃는 것이 아니라 잘못 이름 붙은 정보를 막는 것입니다.

### 3.4 익스포터는 레거시 문자열 파싱으로 폴백

`src/api/metrics/gpu.rs`는 타입 필드를 우선하고 detail 문자열 파싱으로 폴백하므로, mock 서버와 구버전 원격 노드가 섞인 플릿에서도 시리즈가 계속 익스포트됩니다. 파서는 `"1450 RPM"`과 Level Zero의 `"1600 RPM (40%)"` 형태를 모두 처리하고 맨 `"40%"`는 거부합니다.

선택한 메트릭 이름은 `src/mock/templates/amd_gpu.rs`가 이미 내보내던 것과 일치하므로, mock 플릿은 mock 변경 없이도 필드가 끝까지 채워집니다.

### 3.5 렌더러의 행 존재 술어를 레이아웃 줄 수와 공유

`gpu_renderer.rs`는 온도 임계치와 P-state를 이미 싣고 있는 보조 행에 팬 속도를 렌더링합니다. "이 행이 존재하는가" 술어를 이제 렌더러와 레이아웃 줄 수 계산이 공유하는 헬퍼로 두어, 예약된 줄 수와 실제로 그려지는 내용이 어긋날 수 없게 했습니다.

여기서 이것이 특히 중요합니다. AMD와 Intel 카드는 NVML 임계치를 하나도 보고하지 않으므로, 그 카드들에서는 팬 속도가 이 행을 여는 유일한 항목입니다. 술어가 어긋났다면 정확히 그 카드들에서 빈 예약 줄로 나타났을 것입니다.

## 4. 검증 결과

| 게이트 | 결과 |
|--------|------|
| `cargo check --lib --tests` | 통과 |
| `cargo clippy --lib --tests -- -D warnings` | 통과, `--features level_zero`에서도 통과 |
| `cargo fmt --check` | 통과 |

| 테스트 타겟 | 통과 | 커버 범위 |
|-------------|------|-----------|
| `api::metrics::gpu` | 15 | `Some`일 때 존재, `None`일 때 부재, detail 폴백, 충돌하는 detail 문자열보다 구조화 필드 우선, 듀티 사이클만 있으면 미발행, 모든 리더 값 형태에 대한 표 기반 파서 테스트 |
| `network::metrics_parser` | 54 | `GpuMetricExporter`를 통한 실제 익스포터-파서 왕복, 부재는 `None` 유지, 범위 밖 및 소수 값 거부 |
| `device::readers::amd_adl` | 24 | 기존 detail 단언과 나란히 타입 필드 단언, 빈 판독은 둘 다 미설정 |
| `device::readers::intel_gpu_linux` | 31 | hwmon `fan1_input` 한 번 읽기가 필드와 문자열 양쪽에 도달, 파일 없는 카드는 둘 다 미설정 |
| `device::readers::intel_gpu_level_zero` | 47 | hwmon 우선순위가 타입 필드에도 적용, 듀티 사이클만 있으면 미설정, L0가 hwmon 기준선이 남긴 공백을 채움 |
| `ui::renderers::gpu_renderer` | 41 | 존재 시 렌더링, 부재 시 생략, 팬이 유일 항목일 때 선행 공백 중복 없음, 이웃과의 구분, 팬이 독립적으로 레이아웃 줄 수를 올림 |
| `snapshot_test` | 13 | 직렬화 왕복 |
| `thermal_pstate_integration_test` | 4 | 공유 보조 행 |

**미검증**: 개발 호스트에서 `cargo check --target x86_64-pc-windows-gnu --lib`는 mingw 크로스 컴파일러가 설치돼 있지 않아 `zstd-sys`에서 실패하며, 이는 이 변경과 무관합니다. 변경된 ADL 코드 경로(`apply_to_gpu_info`)는 `cfg` 게이트가 없고 그 테스트는 Linux에서 실제로 실행됩니다. `amd_windows.rs`, `intel_gpu_windows.rs`, `windows_gpu_perf.rs`의 Windows 게이트 `GpuInfo` 리터럴은 `fan_speed_rpm: None`만 추가됐으며 수동으로 검토했습니다.

## 5. 결과 및 후속

- PR #360은 `ceaea01`로 `main`에 squash merge되었습니다.
- 이슈 #352는 PR의 `Closes #352` 링크로 자동 종료되었습니다.
- `all_smi_gpu_fan_speed_rpm`은 `API.md`의 AMD 전용 표에서 실제 레이블 집합과 함께 공유 GPU 메트릭 표로 옮겨졌습니다. Intel도 내보내기 때문입니다.
- 하위 호환은 양방향으로 유지됩니다. `#[serde(default)]`가 구버전 페이로드를 받아 주고, 익스포터의 detail 폴백이 구버전 원격 노드의 시리즈 발행을 유지합니다.

---

## 부록: 핵심 키워드

| 키워드 | 설명 | 관련성 |
|-------|------|--------|
| 회전 센서(tachometer) | 실제 회전수를 보고하는 팬 센서 | `None`이 장치에 없다고 말하는 대상 |
| 듀티 사이클 | 측정된 속도가 아닌 팬 제어 퍼센트 | 퍼센트를 `_rpm` 필드에 담으면 안 되는 이유 |
| `#[serde(default)]` | 누락 필드를 실패 대신 기본값으로 역직렬화 | 필드 도입 이전 스냅샷을 계속 읽게 해 주는 장치 |
| 리더 간 덮어쓰기 가드 | 한 리더가 다른 리더보다 우선하게 하는 `detail.contains_key` 검사 | detail 문자열 기록을 유지한 이유 |
