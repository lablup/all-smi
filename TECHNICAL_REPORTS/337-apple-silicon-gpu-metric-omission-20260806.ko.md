# 기술 보고서: PR #337 - fix: omit Apple Silicon GPU metrics instead of reporting 0

**작성일**: 2026-08-06
**상태**: 리더/익스포터/TUI/집계 체인은 완료. 저하 경로 자체는 단위 테스트와 실제 launchd CI 러너로 검증했지 IOReport를 물리적으로 끈 하드웨어로 검증한 건 아니다(8절 참고)
**언어**: Rust, YAML(GitHub Actions)
**위험도**: Medium(리더·노출·TUI·집계 레이어에 걸쳐 파일 21개를 건드림. 동작 변화는 실질적이지만 좁은 와이어 포맷 영향을 가진 정확성 수정이다. 지표군 다섯 개가 조건부 생략으로 바뀜)

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

`NativeMetricsManager::new()`가 실패하면(IOReport가 없는 macOS 호스트, 즉 VM이나 하드닝된 샌드박스, 호스트형 CI 러너에서는 이게 정상 상태다), 애플 실리콘 GPU 리더는 `GpuMetrics::default()`로 폴백해 모든 필드를 리터럴 0으로 언랩했다. GPU를 아예 측정하지 못하는 macOS 호스트가 `all_smi_gpu_utilization 0`, `all_smi_gpu_power_consumption_watts 0`, `all_smi_gpu_temperature_celsius 0`을 게시했고, 와이어상으로는 정말 유휴 상태인 GPU와 구분되지 않았다. 익스포터만 고쳤다면 부족했을 것이다. `GpuInfo` 구조체 하나가 Prometheus 익스포터, TUI, `snapshot`을 모두 먹여 살리므로, `src/network/metrics_parser.rs`도 보는 쪽에서 원격 `GpuInfo`의 실시간 필드를 `0.0`으로 초기화하고 있었다. 그러니 익스포터가 시리즈를 올바르게 생략하기 시작한 뒤에도 저하된 노드를 상대로 한 `all-smi view --hosts`는 여전히 로컬 TUI에 `0.0%`를 렌더링했을 것이다. 이 수정은 리더, 노출 로직, 원격 파서, 그 사이의 모든 집계 지점을 건드리며, 네 애플 실리콘 리더(메모리, CPU, 섀시, GPU)를 하나의 문서화된 정책으로 정렬한다. 디바이스에 대해 여전히 참인 뭔가를 말할 수 있다면 행을 내보내고, 소스를 구할 수 없었던 필드만 부재로 표시하며, 절대 0으로 대체하지 않는다는 정책이다.

부재는 `Option<f64>`가 아니라 인밴드로 인코딩됐다. 처음부터 그렇게 가정한 게 아니라 수정 도중 의도적으로 그렇게 선택했다. `GpuInfo`의 실시간 필드는 대략 예순 곳의 소비자(게이지, 스파크라인, LED 그리드, 정렬 비교자, 에너지 누적, CLI 셰임 세 개, 목 서버, 리더 열두 개)를 갖고 있어서, 타입을 바꾸는 건 이 버그와는 무관한, 같은 베이스를 건드리는 자매 PR 넷과 나란히 도는 대규모 리팩터가 됐을 것이다. 대신 `GPU_METRIC_UNAVAILABLE`(`-1.0`, 소비하는 모든 수량의 유효 범위 밖)이 그 역할을 맡고, 이름 붙은 접근자 다섯 개로 다시 읽힌다. 이유는 이미 내보내던 `all_smi_gpu_info` 신원 시리즈에 새 `native_metrics="available"|"unavailable"` 레이블로 실려서, 새 지표군 없이 무료로 전달된다. 이 수정 과정에서 이론상이 아니라 실제로 두 가지 집계 버그가 드러났다. 에너지 통합기는 `-1.0` 센티널을 실행 중인 줄 총량에 그대로 더했을 것이고(음의 에너지가 쌓임), TUI의 히스토리 그래프는 실제로 일어난 적 없는 GPU 사용률/온도 하강을 그렸을 것이다. 옛 코드는 부분(센티널로 오염된) 합을 전체 디바이스 수로 항상 나눴기 때문이다. 저하 경로 자체는 실제 재현 환경인, IOReport가 없는 `macos-14` CI 러너에서 확인했다. launchd 스모크 테스트가 새로 추가한 단언, 즉 러너가 조작된 `all_smi_gpu_utilization`을 전혀 내보내지 않는다는 걸 확인하는 검사를 통해서다. 합성으로 조건을 구성한 단위 테스트만이 아니다. 전체 규모는 파일 21개, +1176/-374, 커밋 1개, #325를 닫는다.

---

## 1. 문제 정의

### 1.1 배경

`src/device/macos_native/manager.rs`는 `NativeMetricsManager`를 프로세스 전역 `Lazy<Mutex<Option<Arc<...>>>>` 싱글턴에 보관한다. `NativeMetricsManager::new()`가 실패하면(`IOReport::new()`가 실패할 때마다 일어나며, IOReport에 실제로 접근할 수 없는 VM·하드닝된 샌드박스·호스트형 macOS CI 러너에서는 정상 상태다), 싱글턴은 프로세스 생애 전체 동안 `None`으로 남는다. 재시도는 없다. 이 PR 이전 네 macOS 리더는 각기 그 부재를 다르게 다뤘다. 메모리(순수 `sysinfo`, 영향 없음), CPU(이미 올바름: `Option` 필드는 `None`이 되고 주파수는 `sysctl`로 폴백), 섀시(이미 올바름: 아예 행을 반환하지 않음. 보고하는 모든 필드가 매니저에서 오기 때문), 그리고 GPU만 매니저에서 온 모든 `Option` 필드를 리터럴 `0`으로 언랩해서 데이터를 조작했다.

### 1.2 기존 문제점

- **문제 1 (GPU 리더가 조작된 0을 만들어냄)**: `src/device/readers/apple_silicon_native.rs`는 매니저가 없거나 수집이 실패하면 `GpuMetrics::default()`(전부 `None`)로 폴백한 뒤, `utilization: metrics.utilization.unwrap_or(0.0)`, `frequency: metrics.frequency.unwrap_or(0)`, `power_consumption: metrics.power_consumption.unwrap_or(0.0)`, 그리고 `unwrap_or(0)`으로 끝나는 온도 폴백 체인으로 `GpuInfo`를 만들었다. 아무것도 측정하지 못하는 디바이스에 대해 대시보드는 "GPU 0% / 0W / 0도"를 보여줬다.
- **문제 2 (0은 정당한 판독값이라 "데이터 없음"을 겸할 수 없음)**: 유휴 GPU나 멈춰선 ANE는 정말로 `0`을 읽을 수 있다. 그러니 소비자는 IOReport 가용성을 아웃오브밴드로 확인하지 않고서는 이걸 매니저-불가용 경우와 구분할 방법이 없었다.
- **문제 3 (익스포터만 고치면 불완전했을 것)**: `GpuInfo`는 소비자 셋(Prometheus 익스포터, TUI, `snapshot`)이 공유한다. 노출 레이어에서만 시리즈를 생략했다면 TUI는 여전히 `0.0%`를 렌더링했을 것이다.
- **문제 4 (원격 보기 경계가 익스포터를 고친 뒤에도 같은 버그를 다시 심음)**: `src/network/metrics_parser.rs`는 시작점으로 `utilization: 0.0, ane_utilization: 0.0, temperature: 0, power_consumption: 0.0`을 가진 원격 `GpuInfo`를 만들고, 스크레이프된 노출에 실제로 등장한 시리즈의 필드만 덮어썼다. 익스포터가 시리즈를 올바르게 생략하고 나면, 파서의 0-초기화 기본값이 익스포터가 애써 생략한 바로 그 값을 조용히 다시 조작해낸다. 그러니 저하된 macOS 노드를 상대로 한 `all-smi view --hosts`는 로컬 TUI에서 여전히 `0.0%`를 보여줬을 것이다.
- **문제 5 (집계가 센티널이나 생략을 계산에 접어 넣음)**: `src/metrics/aggregator.rs`, `src/view/data_collection/aggregator.rs`, `src/api/collection_loop.rs`, `src/snapshot/collector.rs` 모두 부재 판독값을 건너뛰지 않고 원시 `GpuInfo` 필드를 합산하거나 평균 냈다. (부재가 음수 센티널로 인코딩되고 나면) 이는 에너지 총량을 오염시켰을 것이고, (수정 전 모두-0 인코딩 아래서도 이미) GPU 하나가 보고를 멈출 때마다 평균과 히스토리 그래프를 조용히 왜곡시키고 있었다.
- **문제 6 (이 동작을 서술하는 CI 주석 자체가 틀렸음)**: `.github/workflows/ci.yml`의 launchd 잡 주석은 GPU와 섀시 리더가 "각각 0과 없음으로 저하한다"고 적어놓아, 버그를 문제로 지목하는 대신 예상된 동작으로 문서화하고 있었다.

### 1.3 위험성

| 위험 | 영향 | 발생 가능성 |
|---|---|---|
| IOReport 없는 macOS 호스트가 조작된 0짜리 GPU 지표를 게시하고, 유휴 GPU와 구분되지 않음 | 단독으로는 Medium(혼란스럽지만 크래시는 아닌 판독값). 합산되면 더 높음(조작된 0을 평균에 섞는 플릿 대시보드가 실제 사용률을 조용히 과소평가함) | 이 수정 전에는 IOReport 없는 모든 macOS 호스트(VM, 샌드박스, 일부 CI 러너)에서 확실히 발생 |
| 대략 예순 곳의 소비자를 감안하면서 수정 도중 `GpuInfo`의 실시간 필드를 `Option<f64>`로 바꿈 | 여기서 시도했다면 High: 게이지, 스파크라인, 정렬 비교자, 에너지 누적, CLI 셰임, 목 서버, 리더 열둘에 걸친 타입 수준 리팩터가 겹치는 파일을 건드리는 자매 PR들(#333부터 #339까지)과 나란히 착지함 | 대신 인밴드 센티널을 선택해서 회피됨(3.1절) |
| 리더/익스포터는 고쳤지만 원격 스크레이프 파서는 고치지 않음 | 놓쳤다면 High: 익스포터가 올바른 뒤에도 저하된 macOS 노드를 상대로 한 `all-smi view --hosts`는 여전히 조작된 0을 보여줬을 것 | 명시적으로 발견되어 수정됨(`src/network/metrics_parser.rs`, 1.2절 문제 4) |
| 두 집계 버그(음의 줄 에너지 통합, 히스토리 그래프 하강)가 조용해서(크래시가 아니라서) 알려지지 않음 | Medium: 조용히 음수가 되는 에너지 총량, 혹은 실제로 일어난 적 없는 하강을 그리는 히스토리 그래프는 눈에 띄는 증상 없이 모니터링 도구에 대한 신뢰를 갉아먹는 종류의 결함 | 미래의 버그 보고서로 남겨지는 대신 이 PR 작업의 일부로 발견되어 수정됨(2.1절) |

---

## 2. 기술적 검토 사항

### 2.1 정확성

`src/device/macos_native/manager.rs`에 문서화되고 `GpuReader` 트레이트에서 참조되는 통일 규칙은 정확하게 서술되어 있다. 리더는 디바이스에 대해 여전히 참인 뭔가를 말할 수 있는 한 행을 내보내고, 소스를 구할 수 없었던 개별 필드만 부재로 표시하며, 절대 `0`으로 대체하지 않는다. 애플 실리콘 GPU 리더는 이를 만족한다. 신원(`sysctl`)과 통합 메모리(`sysinfo`)는 IOReport와 무관하고 저하 경로에서도 유효하기 때문이다. IOReport/SMC에서 온 다섯 필드(사용률, ANE 전력, 주파수, 소비 전력, 온도)만 부재가 된다.

집계 버그 두 개는 "언제나 그럴듯해 보이는 0"에서 "명시적으로 건너뛰어야 하는 센티널"로 인코딩을 바꾼 직접적인 결과로 발견되어 수정됐는데, 이 점을 정확히 짚어둘 가치가 있다. 둘 다 수정 전 코드에도 존재했다. 그저 덜 눈에 띄었을 뿐이다.

- **에너지 통합**(`src/api/collection_loop.rs`, `integrate_power_samples`): 이 PR 이전에는 `gpu.power_consumption`이 조건 없이 에너지 샘플 목록에 들어갔다. 인밴드 센티널 아래서 이를 그대로 두면 `-1.0`이 실행 중인 줄 총량에 직접 더해져 명백한 오염이 된다. 수정은 `gpu.power_consumption_reading()`을 통과시켜, 판독값이 없는 디바이스는 샘플을 아예 기여하지 않게 한다. 이는 기존의(덜 명백하게 틀렸던) 0-대체 경우에 대해서도 동작상 올바른 수정이다. 조작된 `0.0`은 이제 생략된 샘플이 올바르게 아무것도 기여하지 않는 것과 똑같은 만큼 디바이스의 에너지 소비를 조용히 과소평가했었다.
- **히스토리 그래프**(`src/view/data_collection/aggregator.rs`): `avg_utilization`과 `avg_temperature`는 `sum / state.gpu_info.len()`으로 계산되어 매 주기 조건 없이 `utilization_history`/`temperature_history`에 밀어넣어졌다. 옛 조작된 `0`(혹은 이 PR 이후 순진하게 합산했다면 센티널)을 보고하는 GPU가 있으면, 그게 보고하지 못한 주기마다 평균이 끌려 내려가서, 히스토리 그래프는 정상 보고 디바이스에서는 실제로 일어난 적 없는 하강을 그린다. 수정은 `metrics::gpu_readings::mean_utilization`/`mean_temperature`를 쓰는데, 아무것도 보고되지 않으면 `None`을 반환하고, 그 주기의 히스토리 삽입은 조작된 낮은 값을 밀어넣는 대신 아예 건너뛴다.

`src/metrics/gpu_readings.rs`(신규)는 이런 건너뛰기-인식 집계(`total_power_watts`, `mean_utilization`, `mean_temperature`, `temperature_std_dev`, `first_ane_power_watts`)를 전부 한곳에 모아서, TUI 대시보드·헤더·스파크라인 패널·스냅샷 작성기·클러스터 집계기가 각자 조금씩 다르게 건너뛰기 로직을 재구현하지 못하게 막는다. `temperature_std_dev`는 반환 타입도 `Option<f64>`로 바뀌어, 판독값을 보고한 디바이스가 둘 미만이면 `None`을 반환한다. 기존 코드가 `total_gpus - 1`로 나누고 호출부에서 "실제로 몇 대가 보고했는가"가 아니라 `total_gpus > 1` 가드에 기댔던 잠재된 문제를 바로잡는다.

### 2.2 성능 관점

집계 함수가 이미 하던 것 이상의 주기당 새 비용은 없다. `metrics::gpu_readings`의 함수들은 기존 `&[GpuInfo]` 슬라이스를 한 번 훑으며, 대부분 중간 컬렉션을 할당하지 않고 `filter_map`/`find_map`을 쓴다. Prometheus 익스포터의 필드별 `if let Some(reading) = info.x_reading() { ... }` 가드는 필드당 O(1) 검사이고, 조건 없는 `builder.metric(...)` 호출을 조건부로 건너뛸 수 있는 호출로 바꿀 뿐이다. 스크레이프당 비용 차이는 무시할 만하다.

### 2.3 호환성 및 의존성

- **Breaking Changes**: 실질적이지만 좁은 와이어 포맷 변경이다. `all_smi_gpu_utilization`, `all_smi_gpu_power_consumption_watts`, `all_smi_gpu_temperature_celsius`, `all_smi_gpu_frequency_mhz`, `all_smi_ane_utilization`, `all_smi_ane_power_watts`는 이제 특정 필드에 대한 판독값이 없는 GPU 행에서는 (`0`으로 나오는 대신) 생략된다. 이건 겉치레가 아니라 정확성 수정이지만, 이 시리즈들이 모든 `gpu_index`에서 항상 존재한다고 가정한 PromQL 질의나 알람 규칙(예를 들어 `absent()` 짝 없이 그냥 쓴 `gpu_temp > 80`)이 있다면, Prometheus의 "데이터 없으면 생략" 관례가 요구하는 것과 같은 검토가 필요하다. `all_smi_gpu_info`(신원 시리즈)는 영향받지 않고 디바이스가 검출되면 항상 존재하며, 이제 애플 실리콘에서는 `native_metrics="available"|"unavailable"`을 싣는다.
- **새로운 의존성**: 없다.
- **호환성**: 애플 실리콘을 넘어서는 동작 변화가 두 가지 있는데, 둘 다 이 PR 이전에 TUI가 이미 렌더링하던 것과 노출 로직을 맞춘다. `all_smi_gpu_frequency_mhz`는 이제 이미 `0`을 "클록 프로브 없음"으로 쓰던 리더(Rebellions, Intel Gaudi, WMI 경유 AMD)에서 (정적인 `0`으로 보고되는 대신) 생략되고, `all_smi_gpu_temperature_celsius`는 어떤 플랫폼에서든 센서가 응답하지 않으면 생략된다. `all_smi_ane_utilization`은 비-애플 GPU에서 명시적으로 그대로다. 이들은 "해당 없음"을 뜻하는 리터럴 `0.0`을 설정하고 계속 게시한다. 전용 회귀 테스트(`exporter_keeps_emitting_zero_ane_for_non_apple_gpus`)로 커버되는데, "센티널이면 생략"이라는 순진한 변경이 작동하던 플랫폼을 회귀시킬 수 있는 딱 그 지점이기 때문이다.

### 2.4 코드 품질

저하 경로는 세 단계에서 시험됐지 단위 테스트만으로 고립되어 시험된 게 아니다. 첫째, `get_gpu_info`에서 뽑아낸 `build_gpu_info(static_info, apple_info, sample: Option<&NativeSample>)`는 `cfg(test)` 스위치나 강제 실패 플래그 없이 테스트가 `sample: None`, 즉 macOS VM이 타는 바로 그 분기를 구동할 수 있게 하는 이음매다. `degraded_path_reports_absence_not_zero`, `degraded_path_keeps_identity_and_memory`, `missing_smc_sensors_degrade_only_temperature`가 이를 직접 시험한다. 둘째, 그 저하된 행은 실제 Prometheus 익스포터(`degraded_row_renders_no_gpu_value_series`)와 그 건강한 짝(`healthy_row_renders_every_value_series`)을 통과하므로, 리더-익스포터 이음매가 리더 단독이 아니라 함께 커버된다. 셋째, 그리고 이 PR에서 가장 강력한 검증 형태로, IOReport 없는 실제 `macos-14` 러너에서 도는 `.github/workflows/ci.yml`의 launchd 스모크 테스트 잡이 명시적 부정 단언을 새로 얻었다. `! curl -sf --max-time 10 localhost:9090/metrics | grep -q '^all_smi_gpu_utilization'`이 합성이 아니라 실제 재현 환경에서 러너가 조작된 사용률 시리즈를 전혀 내보내지 않는다는 걸 확인한다. 같은 잡의 "각각 0과 없음으로 저하한다"는 주석도 고쳐진 동작을 서술하도록 바로잡혔다.

`src/network/metrics_parser.rs`는 리더 쪽의 같은 쌍을 반영하는 테스트 두 개를 새로 얻는다. `test_omitted_gpu_series_stay_absent_after_scrape`(신원과 메모리 시리즈만 담은 노출 본문, 정확히 저하된 익스포터가 렌더링하는 것을 파싱해서 모든 `*_reading()` 접근자가 `None`을 반환하는지 단언)와 `test_zero_gpu_series_survives_scrape_as_a_reading`(스크레이프된 본문의 진짜 `0`이 보는 쪽에서 `Some(0.0)`으로 살아남음)이다. 그래서 두 경우가 프로세스 하나 안이 아니라 네트워크 홉을 건너서도 종단으로 구분 가능하게 유지된다.

---

## 3. 기술적 선택과 그 이유

### 3.1 부재를 센티널로 인밴드 인코딩하고, 파급 범위를 근거로 수정 도중 `Option<f64>`를 기각한다

**컨텍스트**: `Option<f64>`는 "값이 있거나 없거나"를 표현하는 타입 안전한 방법이고, 컴파일러가 모든 호출부에서 처리를 강제하게 해준다. `GpuInfo`의 `utilization`, `ane_utilization`, `power_consumption`, `temperature`, `frequency` 필드는 오늘날 옵션이 아닌 평범한 숫자이고, 대략 예순 곳의 호출부(게이지, 스파크라인, LED 그리드, 정렬 비교자, 에너지 누적, CLI 셰임 셋, 목 서버, 디바이스 리더 열둘)가 소비한다.

| 옵션 | 장점 | 단점 |
|---|---|---|
| **기각: `GpuInfo`의 실시간 필드를 `Option<f64>`/`Option<u32>`로** | 타입 시스템이 모든 호출부의 처리를 강제함. 기억해야 할 매직 센티널이 없음 | 겹치는 파일을 건드리는 자매 PR 넷(#333, #334, #335/#336, #338, #339)과 나란히 착지하는 버그 수정 PR 안에서, 예순 곳쯤 되는 소비자 전반에 걸친 타입 수준 리팩터. 대안 대비 얻는 동작상 이득 없이 회귀 표면만 커짐 |
| **채택: 인밴드 센티널. `f64` 필드는 `GPU_METRIC_UNAVAILABLE = -1.0`, `u32` 필드는 `0`. 이름 붙은 접근자 다섯 개로 다시 읽음** | 바뀔 필요 없는 기존 호출부에는 파급 범위가 0. 코드베이스에는 이미 이 정확한 인코딩과 맞아떨어지는, 강제되지 않은 관례가 있었음(아래 참고) | 원시 필드가 아니라 항상 접근자를 통해 읽는 규율이 필요하고, 인코딩이 절대 와이어로 새어나가면 안 됨(내보낸 출력에 ` -1`이 나타나지 않는지 확인하는 전용 테스트로 검증됨) |
| 매니저가 불가용일 때 `GpuInfo` 행 전체를 억제 | 리더 쪽에서 가장 단순한 변경 | 저하 경로에서도 여전히 유효한 신원과 통합 메모리 데이터를 잃음. 문서화된 정책("여전히 참인 뭔가를 말할 수 있으면 행을 내보낸다")과 모순됨 |

**선택 이유**: 채택한 인코딩은 새로운 발명이 아니다. 이 PR 이전에도 `src/ui/renderers/gpu_renderer.rs`와 `src/ui/filter_dsl/eval.rs`는 이미 `utilization < 0.0`과 `power_consumption < 0.0`을 N/A로, `temperature == 0`/`frequency == 0`을 N/A로 읽고 있었다. 그저 그 분기들에 일부러 음수나 0을 먹여주는 생산자가 없었을 뿐이다. 이 PR이 그 생산자를 준다. 센티널 값은 그것이 나타내는 모든 수량의 유효 범위 밖이라(퍼센트는 `0..=100`, 전력 레일은 `>= 0`와트를 끔) 진짜 판독값과 충돌할 수 없고, 접근자(`utilization_reading()`과 나머지 넷)는 내부 인코딩이 새어나가지 못하게 막는 강제된 경계다. 내보낸 Prometheus 출력에 문자열 `" -1"`이 절대 나타나지 않는지 직접 확인하는 테스트로 검사된다.

**받아들인 트레이드오프**: 타입 시스템이 호출자에게 부재 처리를 강제하지 않는다. 미래의 어떤 호출부가 `gpu.utilization_reading()` 대신 `gpu.utilization`을 직접 읽으면, 컴파일 실패 대신 센티널을 진짜 값(깊게 음수인, 그리고 하류에서 아마 클램프되거나 무시될 값)으로 조용히 취급할 것이다. 이 위험은 이 특정 PR 안에서 대안이 치를 비용을 감안하면 받아들일 만하다고 판단됐을 뿐, 영구히 받아들일 만하다고 판단된 건 아니다. 미래에 전용 리팩터가 예순 곳쯤 되는 파급 범위를 감당할 의향이 있다면 `Option<f64>`는 여전히 타입상 올바른 목표로 남는다.

### 3.2 노출 레이어에서는 생략, 신원 레이어에서는 명시적 `native_metrics` 레이블: 둘 다 필요하다. 서로 다른 질문에 답하기 때문이다

**컨텍스트**: "이 GPU의 값 시리즈가 부재함"을 표현할 후보 신호가 둘 있다. 시리즈를 아예 생략하는 것(Prometheus 자신의 데이터 없음 관례), 아니면 소비자가 질의할 수 있는 명시적 센티널/플래그 값을 내보내는 것.

**결정**: 둘 다, 서로 다른 레이어에서. 값 시리즈(`all_smi_gpu_utilization`과 나머지 넷)는 생략되며, `all_smi_gpu_performance_state`와 네 열 임계값 지표군에 대해 #132 이후 이미 있던 익스포터 자신의 부재-시-생략 관례와 맞아떨어진다. *이유*는 검출된 디바이스에 대해 항상 존재하는 신원 시리즈인 `all_smi_gpu_info`에, 새 `native_metrics="available"|"unavailable"` 레이블로 실린다.

**선택 이유**: 생략만으로는 시리즈가 왜 없는지 아무것도 말해주지 않는데, 여기서는 이게 중요하다. "이 호스트에 IOReport가 없다"는 일시적인 공백이 아니라 영구적이고 조치 가능한 상태이고, 이제 다른 어떤 시리즈가 빠졌는지로 추론하지 않고도 직접 질의할 수 있다(`all_smi_gpu_info{native_metrics="unavailable"}`). 생략 대신 와이어에 명시적 센티널 값을 두는 안은 기각됐다. 성능 상태와 열 임계값에 대한 익스포터 기존 관례 옆에 두 번째 부재 관례를 도입하는 것뿐이고 이득이 없기 때문이다. 사라지는 시리즈가 죽은 타깃처럼 보인다는 반론은 여기서는 보기보다 약하다. 이 PR 아래서 타깃은 절대 조용해지지 않는다. `all_smi_up`/`all_smi_build_info`(PR #333)는 무조건적이고, 메모리/CPU/디스크 지표군은 여전히 렌더링되며, 바로 그 디바이스에 대한 `all_smi_gpu_info`도 여전히 렌더링된다. 그러니 특별 취급되는 지표 없이도 "디바이스는 있는데 보고를 안 함"과 "디바이스가 완전히 사라짐"이 계속 구분 가능하다.

### 3.3 영향받은 다섯 지표군 전부에 하나의 부재 정책을, 온도만 따로 떼어내지 않는다

**컨텍스트**: 온도는 다르게 다뤄야 하지 않냐고 논쟁할 만한 유일한 필드다. 사라지는 온도 시리즈는 원리상 시리즈가 항상 존재한다고 가정하는 `max_over_time` 스타일의 열 알람을 깨뜨릴 수 있기 때문이다.

**결정**: 따로 떼어내지 않는다. 온도도 나머지 넷과 같은 부재-시-생략 규칙을 따른다.

**선택 이유**: 대안(알람의 *평가*가 깨지는 걸 막으려고 조작된 온도를 계속 게시)은 생략보다 알람의 *정확성*을 더 나쁘게 깨뜨린다. 시리즈가 정말로 없을 때 `gpu_temp > 80`은 어느 쪽이든 절대 발동하지 않는 반면, 조작된 `0`은 `gpu_temp < N` 알람을 허위로 발동시키고 클러스터 전체 온도 평균을 조용히 끌어내린다. 더 구체적으로, 다섯 필드 모두 IOReport/SMC 구독 하나에서 나오고 단위로 함께 실패하므로, 분리된 정책(온도는 한 방식, 나머지 넷은 다른 방식)은 대시보드 작성자가 이 하드웨어에서는 사실상 하나인 실패 모드에 대해 규칙 두 개를 배우도록 강요할 것이다. 코드베이스 자신의 이전 동작이 이를 뒷받침한다. TUI, 알람 엔진, 필터 DSL은 이 PR 이전에도 이미 `temperature == 0`을 알 수 없음으로 취급했으니, 다르게 인코딩된 온도는 그 반대가 아니라 하나 남은 일관성 없는 필드가 됐을 것이다.

### 3.4 건강/저하 이음매를 `build_gpu_info(..., sample: Option<&NativeSample>)`로 만들고, 실제 하드웨어 없이도 테스트가 매니저-불가용 분기를 구동할 수 있게 구체적으로 뽑아낸다

**컨텍스트**: 매니저-불가용 경로는 개발 머신(IOReport가 동작하는 M1 Ultra)에서 재현할 수 없었다. 그러니 수정은 `NativeMetricsManager` 자체를 목킹하지 않고도 macOS VM이 타는 정확한 코드 경로를 시험할 방법이 필요했다.

**결정**: `get_gpu_info` 안에 인라인으로 있던 행 조립 로직을 자유 함수 `build_gpu_info(static_info: &DeviceStaticInfo, apple_info: Option<&AppleSiliconInfo>, sample: Option<&NativeSample>) -> GpuInfo`로 뽑아내고, `get_gpu_info`는 샘플을 얻어서(`self.native_manager.get().and_then(|m| m.collect_once().ok()).map(...)`) 이를 호출하는 것으로 줄인다.

**선택 이유**: `Option<&NativeSample>`은 하류의 모든 것이 필요로 하는 정보 정확히 한 비트다. 네이티브 소스가 이번 주기에 뭔가를 만들어냈는가다. `build_gpu_info`에 `None`을 넘기면 `cfg(test)` 조건부 컴파일도, 실제 조건을 대신하는 강제 실패 플래그도 없이 macOS VM이 타는 것과 바이트 단위로 똑같은 분기가 구동된다. 이것이 `degraded_row_renders_no_gpu_value_series`가 손으로 `GpuInfo`를 만들어 리더가 실제로 만들어낼 것과 맞기를 바라는 대신, 리더가 구성한 행을 실제 `GpuMetricExporter`에 통과시켜 실제 렌더링 출력을 단언할 수 있게 해준 지점이다.

---

## 4. 구현 상세

### 4.1 아키텍처 변경

```
[변경 전]
NativeMetricsManager::new() 실패 (IOReport 없음)
    │
    ▼
get_gpu_info(): metrics = GpuMetrics::default() (전부 None)
    │
    ▼
GpuInfo { utilization: metrics.utilization.unwrap_or(0.0), ... }   -- 조작된 0
    │
    ├──▶ Prometheus 익스포터: 항상 시리즈를 내보냄, 값 0
    ├──▶ TUI: "0.0%" 렌더링
    └──▶ metrics_parser (원격 보기): 스크레이프 파싱에서 어쨌든 다시 0으로

[변경 후]
NativeMetricsManager::new() 실패 (IOReport 없음)
    │
    ▼
get_gpu_info(): sample = native_manager.get().and_then(|m| m.collect_once().ok()).map(...) = None
    │
    ▼
build_gpu_info(static_info, apple_info, sample: None)
    │  신원 + 통합 메모리: 여전히 유효함 (sysctl / sysinfo)
    │  utilization/ane/power/frequency: GPU_METRIC_UNAVAILABLE 또는 0 (센티널)
    │  detail["native_metrics"] = "unavailable"
    ▼
GpuInfo { utilization: -1.0, ... }   -- 내부 인코딩, 절대 와이어로 안 나감
    │
    ├──▶ Prometheus 익스포터: `if let Some(v) = info.utilization_reading() { emit }` -- 생략됨
    ├──▶ TUI: "N/A" 렌더링, 게이지는 비어서 그려짐
    ├──▶ metrics_parser (원격 보기): 모든 실시간 필드를 부재로 시작, 실제 존재하는 시리즈만 덮어씀
    └──▶ metrics::gpu_readings: 평균/합에서 제외됨 (에너지 통합기, 히스토리 그래프)
```

### 4.2 주요 코드 변경

**파일: `src/device/types.rs`(센티널과 접근자)**
```rust
pub const GPU_METRIC_UNAVAILABLE: f64 = -1.0;

impl GpuInfo {
    pub fn utilization_reading(&self) -> Option<f64> {
        (self.utilization >= 0.0).then_some(self.utilization)
    }
    pub fn temperature_reading(&self) -> Option<u32> {
        (self.temperature > 0).then_some(self.temperature)
    }
    // ane_utilization_reading, power_consumption_reading, frequency_reading도 같은 모양
}
```
**변경 이유**: 내부 "판독값 없음" 인코딩이 중요한 어느 곳에서도 진짜 값으로 읽히지 못하게 막는 단일 경계다. 부재와 진짜 판독값을 구분해야 하는 모든 소비자는 원시 필드가 아니라 이 다섯 함수 중 하나를 거친다.

**파일: `src/device/readers/apple_silicon_native.rs`(건강/저하 이음매)**
```rust
fn build_gpu_info(
    static_info: &DeviceStaticInfo,
    apple_info: Option<&AppleSiliconInfo>,
    sample: Option<&NativeSample>,
) -> GpuInfo {
    ...
    detail.insert(
        "native_metrics".to_string(),
        if sample.is_some() { "available".to_string() } else { "unavailable".to_string() },
    );
    ...
    GpuInfo {
        utilization: sample.map_or(GPU_METRIC_UNAVAILABLE, |s| s.utilization),
        ane_utilization: sample.map_or(GPU_METRIC_UNAVAILABLE, |s| s.ane_power_mw),
        power_consumption: sample.map_or(GPU_METRIC_UNAVAILABLE, |s| s.power_watts),
        frequency: sample.map_or(0, |s| s.frequency),
        // sample과 무관한 신원, 통합 메모리
        ...
    }
}
```
**변경 이유**: 리더 수준의 수정이다. `sample: None`은 정확히 macOS VM이 타는 분기이고, IOReport 자체를 목킹하지 않고도 직접 시험할 수 있다.

**파일: `src/api/metrics/gpu.rs`(노출 수준 수정. 모양이 똑같은 가드 다섯 개 중 하나)**
```rust
if let Some(utilization) = info.utilization_reading() {
    builder
        .help("all_smi_gpu_utilization", "GPU utilization percentage (omitted when the device reports no utilization)")
        .type_("all_smi_gpu_utilization", "gauge")
        .metric("all_smi_gpu_utilization", &base_labels, utilization);
}
```
**변경 이유**: 조건 없는 `builder.metric(...)` 호출이 판독값이 실제로 존재해야만 조건부로 실행되게 바뀐다. 성능 상태와 열 임계값에 대한 익스포터 자신의 기존 관례와 맞아떨어진다.

**파일: `src/network/metrics_parser.rs`(원격 보기 경계, 리더/익스포터 수정이 로컬 전용에 그치지 않게 하는 수정)**
```rust
// Start every live field at "no reading" rather than at zero. A scrape
// only overwrites the fields whose series it actually contains, so a
// zero default silently re-fabricated the exact value the exporter went
// out of its way to omit...
utilization: GPU_METRIC_UNAVAILABLE,
ane_utilization: GPU_METRIC_UNAVAILABLE,
...
power_consumption: GPU_METRIC_UNAVAILABLE,
```
**변경 이유**: 이게 없으면 올바르게 생략하는 익스포터와 올바르게 N/A를 렌더링하는 로컬 TUI가 있어도, 스크레이프를 새 `GpuInfo`로 파싱하면서 스크레이프에 실제로 담긴 시리즈를 적용하기 전에 예전에는 0으로 초기화하던 `all-smi view --hosts`에 의해 여전히 무너진다.

**파일: `src/api/collection_loop.rs`(에너지 통합 버그, 인코딩 변경의 결과로 수정됨)**
```rust
for gpu in &state.gpu_info {
    if let Some(watts) = gpu.power_consumption_reading() {
        samples.push((EnergyKey::gpu(gpu.hostname.clone(), gpu.uuid.clone()), watts));
    }
}
```
**변경 이유**: `if let Some(...)` 가드가 없으면 센티널 `-1.0`이 실행 중인 줄 총량에 그대로 더해져 명백한 오염이 된다. 이는 인밴드 센티널로의 전환이 새로 가능하게 만든 것이고, 이 PR이 같은 변경 안에서 닫는다.

### 4.3 데이터 모델 변경

설정이나 CLI 의미의 스키마 변경은 아니고, 지표 노출 계약 변경이다. 지표 다섯 군이 무조건 내보내지는 대신 특정 필드를 리더가 구할 수 없었던 어떤 `gpu_index`에서든 조건부로 생략된다. `all_smi_gpu_info`는 애플 실리콘 행에 새 조건부 레이블 `native_metrics`를 얻는다. 내부적으로 `GpuInfo`의 실시간 필드는 기존 타입(`f64`/`u32`, `Option` 아님)을 유지하며, 특정 범위 밖 값의 해석이 이 PR로 "안 쓰임"에서 "부재 센티널"로 재정의된다.

---

## 5. 학습 포인트

### 5.1 공유되는 데이터 구조체에서는 한 소비자에 대한 수정만으로는 수정이 아니다

**개념**: 구조체 하나(`GpuInfo`)가 독립된 여러 소비자(HTTP 익스포터, TUI 렌더러, 스냅샷 작성기, 그리고 네트워크 홉을 건너 같은 구조체를 재구성하는 원격 스크레이프 파서)를 먹여 살릴 때, 그 구조체가 "데이터 없음"을 표현하는 방식의 결함은 버그 보고서가 우연히 겨냥한 하나가 아니라 구조체가 각 소비자와 맞닿는 경계마다 고쳐야 한다.

**이 PR에서의 적용**: 접수된 이슈는 Prometheus 노출에 관한 것이었다. `src/api/metrics/gpu.rs`만 고쳤다면 TUI, 그리고 결정적으로 `src/network/metrics_parser.rs`의 원격 보기 경로가 여전히 0을 조작하고 있었을 것이다. 그 파서는 매 스크레이프마다 처음부터 새 `GpuInfo`를 재구성하고, 익스포터가 이제 올바르게 생략하는 것과 별개로 자기 자신의 부재-안전 기본값이 필요했다.

### 5.2 "0으로 보고"를 "부재로 보고"로 고치는 건, 건너뛰기-인식 집계와 짝짓지 않으면 조용한 의미 버그를 조용한 산술 버그로 바꿀 수 있다

**개념**: 부재 인코딩을 그럴듯해 보이는 값(`0`)에서 명시적 센티널로 바꾸는 것만으로는 집계가 자동으로 올바르게 되지 않는다. 모든 합산 지점이 새 인코딩을 위해 동시에 감사되지 않으면, 그냥 오해를 부르던 평균이 실제로 오염시키는 합으로 바뀔 수 있다.

**이 PR에서의 적용**: 에너지 통합기의 음의 줄 버그가 이 점을 날카롭게 보여준다. `0.0`을 에너지 총량에 더하면 조용히 과소평가할 뿐이었지만(이미 틀렸지만 한계가 있음), `-1.0`을 더했다면 시간이 지날수록 총량이 감소했을 것이다. 이는 이 PR 자신의 인코딩 선택 때문에 비로소 *가능해진* 명백한 오염이고, 새로운 결함으로 배포되는 대신 같은 변경 안에서 발견되어 수정됐다.

### 5.3 고쳐진 경우만큼이나 *영향받지 않아야 할* 경우에 대한 회귀 테스트도 중요하다

**개념**: 수정이 조건부로 동작을 바꿀 때(부재면 생략, 존재하면 계속 내보냄), 바뀌면 안 되는 경우도 바뀌어야 하는 경우만큼 하나의 계약이다. 옛 동작이 있어야 할 곳에서 유지된다고 확인하는 테스트가 나중의 "정리" 작업이 새 규칙을 과도하게 적용하는 걸 막는다.

**이 PR에서의 적용**: `exporter_keeps_emitting_zero_ane_for_non_apple_gpus`가 구체적으로 존재하는 이유는, 비-애플 리더가 "해당 없음"을 뜻하려고 `ane_utilization`에 리터럴 `0.0`을 쓰기 때문이다. 이건 타입 수준에서 애플 실리콘의 "불가용" 경우와 구조적으로 동일하지만 다르게 렌더링돼야 한다(생략이 아니라 명시적이고 의미 있는 0). 이 테스트가 없었다면, "센티널이면 생략" 로직을 모든 리더에 걸쳐 통일하려는 미래의 리팩터가 기존의 모든 NVIDIA/AMD 스크레이프를 조용히 깨뜨릴 수 있었을 것이다.

---

## 6. 추가 학습

### 핵심 용어

| 용어 | 설명 | 관련성 |
|---|---|---|
| `GPU_METRIC_UNAVAILABLE` | 리더에 판독값이 없을 때 `GpuInfo`의 `f64` 필드에 쓰이는 `-1.0` 센티널 | 이 PR의 핵심 인코딩 결정. 이름 붙은 접근자 다섯 개로만 다시 읽힘 |
| `native_metrics` 레이블 | `all_smi_gpu_info`에 새로 추가된 조건부 레이블, `"available"`/`"unavailable"` | 애플 실리콘에서 값 시리즈가 생략된 명시적이고 질의 가능한 이유 |
| `build_gpu_info(..., sample: Option<&NativeSample>)` | 애플 실리콘 GPU 리더에서 뽑아낸 행 조립 이음매 | 실제 하드웨어나 IOReport 목킹 없이도 테스트가 매니저-불가용 분기를 구동하게 함 |
| `metrics::gpu_readings` | 건너뛰기-인식 집계(`total_power_watts`, `mean_utilization` 등)를 한곳에 모은 신규 모듈 | "부재 판독값 건너뛰기"가 구현된 단일 지점. TUI, 스냅샷 작성기, 클러스터 집계기가 똑같이 사용 |
| Prometheus 데이터-없으면-생략 관례 | 와이어에 센티널 값을 두는 대신 시리즈를 부재로 둠 | #132 이후 이 익스포터가 성능 상태와 열 임계값에 이미 쓰던 관례. 이 PR이 지표군 다섯 개에 더 확장함 |

### 관련 기술/프레임워크

- 누락된 데이터를 표현하는 Prometheus 노출 관례(`absent()`, 이 PR이 따르는 센티널 대신 생략 관례).
- Rust의 `Option<T>` 대 인밴드 센티널 값. "값 없음"을 나타내는 경쟁하는 방식이며, 이미 널리 쓰이는 옵션 아닌 필드에서 후자를 선택하는 파급 범위 논거.

### 관련 PR/이슈

- 이슈 #325: 이 PR이 닫는 이슈.
- PR #323: 이 PR이 바로잡는 launchd CI 잡 주석. (조작된-0 동작을 예상된 것으로 문서화하고 있었다.)
- PR #333(이슈 #324): 무조건 `all_smi_up`/`all_smi_build_info` 기준선을 추가했다. 이 PR의 익스포터 주석이 디바이스의 값 시리즈가 생략되는 순간에도 타깃이 "절대 조용하지 않다"고 말할 수 있는 이유의 일부다(3.2절).
- PR #334: 20컬럼 하한 아래에서는 도달 불가능하다는 근거로 게이지 렌더러 여덟 지점을 검사 없는 차원 산술로 남겨뒀다. `src/ui/renderers/gpu_renderer.rs`가 그중 하나이고, 이 PR의 해당 diff는 값 표시 다섯 개와 게이지 둘에 한정되지 PR #334가 그대로 둔 차원 산술은 건드리지 않는다.
- 이슈 #132: `all_smi_gpu_performance_state`와 열 임계값 지표군에 대한 부재-시-생략 관례를 세운 이전 작업. 이 PR은 이를 재발명하지 않고 확장한다.

---

## 7. 변경 요약

### 통계

| 항목 | 값 |
|---|---|
| 변경 파일 | 21 |
| 추가 줄 | +1176 |
| 삭제 줄 | -374 |
| 커밋 | 1 |
| 신규 파일 | `src/metrics/gpu_readings.rs` |

### 카테고리별 변경

| 분류 | 내용 |
|---|---|
| 정확성 | 애플 실리콘 GPU 리더가 0을 조작하는 걸 멈춤. 네 macOS 리더가 이제 하나의 문서화된 부재 정책을 따름 |
| 정확성 | 원격 스크레이프 파서(`metrics_parser.rs`)가 실시간 GPU 필드를 부재로 시작. 네트워크 왕복을 거쳐도 생략이 유지됨 |
| 버그 수정(이 PR 도중 발견) | 에너지 통합기가 센티널에서 음의 줄을 누적할 수 있었음. 부재 판독값을 건너뛰어 수정 |
| 버그 수정(이 PR 도중 발견) | GPU가 보고를 멈추면 TUI 히스토리 그래프가 실제로 일어난 적 없는 하강을 그렸음. 건너뛰기-인식 평균으로 수정 |
| 신규 모듈 | `metrics::gpu_readings`: TUI, 스냅샷, 클러스터 지표 레이어 전반에서 쓰이는 중앙화된 건너뛰기-인식 집계 |
| 노출 | 지표군 다섯 개(`all_smi_gpu_utilization`, `_power_consumption_watts`, `_temperature_celsius`, `_frequency_mhz`, `all_smi_ane_utilization`, `all_smi_ane_power_watts`)가 이제 조건부 생략. `all_smi_gpu_info`가 애플 실리콘에서 `native_metrics` 레이블을 얻음 |
| 크로스 플랫폼 | 이미 `0`을 "프로브 없음"으로 쓰던 비-애플 리더에서도 `all_smi_gpu_frequency_mhz`/`_temperature_celsius`가 이제 (`0`이 아니라) 생략됨 |
| CI | launchd 잡의 `.github/workflows/ci.yml` 주석 수정. 실제 `macos-14` 러너에서 조작된 `all_smi_gpu_utilization`이 없다는 걸 확인하는 새 부정 단언 추가 |

### 관련 커밋

| SHA | 유형 | 메시지 |
|---|---|---|
| `e560b945` | fix | omit Apple Silicon GPU metrics instead of reporting 0 |

`main`에 `11ccefa8`로 병합됨. #325를 닫는다.

---

## 8. 후속 조치

### 필수

블로킹으로 확인된 건 없다. 리더/노출/TUI/원격 파서/집계 체인은 단위·통합 테스트에 더해 실제 러너 CI 단언으로 검증됐다(부록 A).

### 모니터링 필요

- PR 자신도 자기 개발 환경에서 검증하지 못한 한 가지를 짚는다. "IOReport 없는 머신에서의 실제 저하 경로. macOS VM이 필요하다." 정확히 그런 호스트(`macos-14`)에서 도는 launchd 스모크 테스트가 이에 대한 실제 검사이고, 그 새 부정 단언(조작된 `all_smi_gpu_utilization` 없음)이 개발 루프 자체에 전용 macOS VM을 두는 것 다음으로 이용 가능한 가장 강한 확인이다.

### 향후 개선 사항

- 의도적으로 취하지 않은 `Option<f64>` 리팩터(3.1절) 외에 PR에서 제안된 건 없다. 미래의 PR이 예순 곳쯤 되는 파급 범위를 스스로 감당할 의향이 있다면, 그게 여전히 타입상 올바른 목표로 남는다.

---

## 부록

### A. 테스트 결과

- `cargo fmt --check`: 클린.
- `cargo clippy --lib --tests -j 9 -- -D warnings`와 `cargo clippy --bin all-smi -j 9 -- -D warnings`: 둘 다 클린. 크레이트가 모듈 트리를 두 번 컴파일하므로(PR #319/#334가 짚은 것과 같은 유형의 검사) 따로 돌림.
- `cargo test --lib -j 9 device::readers::apple_silicon_native`: 8개 통과.
- `cargo test --lib -j 9 api::metrics`: 71개 통과. `metrics::gpu_readings`: 6개 통과. `network::metrics_parser`: 51개 통과. `ui::renderers::gpu_renderer`: 37개 통과.
- `cargo test --lib -j 9` 모듈 그룹별: `ui::` 543개, `network::` 127개, `metrics::` 113개, `device::` 169개, `snapshot::` 47개, `api::` 116개, `app_state` 16개, `parsing::` 19개, 전부 통과.
- `cargo test --test {device_tests,library_api_test,snapshot_test,thermal_pstate_integration_test,hardware_details_integration_test}`: 60개 통과.
- 실제 하드웨어: M1 Ultra에서 `all-smi snapshot --format prometheus`로 건강한 경로가 영향받지 않는지 확인함(여섯 지표군 모두 존재, `native_metrics="available"`, 실제 구독에서 나온 진짜 `all_smi_ane_power_watts ... 0`도 여전히 올바르게 게시됨).
- 개발 환경에서 구체적으로 검증하지 못한 부분: 실제 IOReport 없는 머신에서의 저하 경로. 대신 `macos-14`의 launchd CI 잡으로 검증함(2.4절과 8절의 언급 참고).

### B. 성능 벤치마크

별도로 벤치마크하지 않았다. 필드별 노출 검사는 O(1)이고, `metrics::gpu_readings`의 집계 함수는 기존 GPU 슬라이스를 한 번 훑으며 흔한 경로에서 새 할당이 없다.

### C. 참고 자료

- 이슈 #325: 이 보고서가 근거로 삼은 근본 원인 서술, 근거(수정 전 리더의 정확한 줄 번호), 인수 기준. diff와 교차 확인함.
- `src/device/macos_native/manager.rs`: 이 PR이 세운, 문서화된 네 리더 부재 정책.
- 이슈 #132: 이 PR이 확장하는 부재-시-생략 Prometheus 관례의 선례.
- `.github/workflows/ci.yml`, launchd 잡: 저하 경로에 대해 이용 가능한 가장 강력한 확인인 실제 러너 단언.
