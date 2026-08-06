# 기술 보고서: PR #333 - fix(api): always emit a metrics baseline and add /-/ready

**작성일**: 2026-08-06
**상태**: 완료
**언어**: Rust
**위험도**: Low (기존 지표 이름, 타입, 레이블을 하나도 지우지 않는 추가 성격의 노출 변경과 라우트 하나 추가)

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

`/metrics`는 axum이 리스너를 바인딩한 순간부터 첫 수집 주기가 끝날 때까지 `200 OK`에 바이트 길이 0인 본문을 실어 보냈다. 노출 로직이 `AppState`를 그대로 렌더링하고, 그 안의 모든 익스포터가 스스로를 필터링하는 구조였기 때문이다. 이 구간에 떨어진 Prometheus 스크레이프는 샘플 0개짜리 *성공한* 스크레이프로 기록된다. 실패한 타깃이 아니라 시계열에 조용히 뚫린 구멍이라서 알람도 울리지 않는다. 이슈 #324는 서로 배타적이지 않은 세 가지 해법을 제시했다. 첫 수집 주기 뒤로 200 응답을 늦추거나, 별도의 준비 상태 신호를 두거나, 최소 한 줄은 항상 내보내는 것이다. PR #333은 두 번째와 세 번째를 결합하고 첫 번째는 명시적으로 기각했다. `all_smi_up`/`all_smi_build_info` 기준선이 이제 모든 디바이스 지표군보다 앞에서 무조건 렌더링되고, 새 `GET /-/ready` 라우트는 첫 수집 주기가 끝나기 전에는 `Retry-After: 1`을 붙인 `503`을, 끝난 뒤에는 `200`을 반환한다. `/metrics` 자체는 프로세스 생애 전체에서 계속 `200`을 답하므로 기존 스크레이퍼의 상태 코드 처리는 하나도 바뀌지 않는다.

더 무게가 실린 결정은 이 PR이 손대지 않은 부분에 있다. Windows SCM이 `SERVICE_RUNNING`을 보고하게 하고 launchd/systemd 래치를 여는 함수 `mark_serving()`은 첫 수집 주기 뒤로 미뤄지지 않고 그대로 바인드 시점에 남았다. 그 근거는 이제 `src/api/shutdown.rs`에 기록되어 있는데, 세 가지 모두 코드를 근거로 검증된 것이지 그냥 주장한 것이 아니다. SCM에는 준비 상태(readiness) 개념 자체가 없고 살아있음(liveness) 개념만 있다(`SERVICE_START_PENDING`/`SERVICE_RUNNING`). 이슈 #324가 우려했던 불일치는 반대편에서 이미 해결된다. 래치가 열리는 순간 `/metrics`는 이제 정의된, 비어있지 않은 본문을 실어 나른다. 그리고 `src/service_cmd/scm_host.rs`는 `StartPending`을 정확히 한 번, 10초짜리 대기 힌트(`TRANSITION_WAIT_HINT_SECS`, `src/service_cmd/scm.rs:70`)와 함께 보고하고 체크포인트는 절대 증가하지 않는다. 그러니 느린 첫 수집(GPU가 많거나 드라이버가 멈춘 호스트에서의 콜드 WMI·NVML 열거)에 래치를 걸면 SCM이 시작 실패로 판단하고 같은 느린 경로로 재시작을 반복할 위험이 있다. 정작 텔레메트리가 가장 필요한 호스트에서 부팅 루프가 도는 셈이다. 곁다리로 고친 것도 하나 있다. man 페이지의 `API ENDPOINTS` 절이 라우터에 존재한 적도 없는 `/health` 엔드포인트를 문서화하고 있었는데, 이번에 실제 표면(`/-/ready` 포함)으로 바로잡았다. 전체 규모는 파일 15개, +932/-23, 커밋 2개, #324를 닫는다.

---

## 1. 문제 정의

### 1.1 배경

`all-smi api`는 `AppState`에서 Prometheus 형식 지표를 노출하는데, 이 상태는 HTTP 리스너 바인딩 후 얼마 지나 첫 패스를 도는 백그라운드 수집 루프(`src/api/collection_loop.rs`)가 채운다. `src/api/metrics/render.rs`의 모든 익스포터는 스스로 필터링한다. 데이터가 없는 디바이스 지표군은 응답 본문에 아무것도 기여하지 않는다. 이 PR 이전에는 노출 대상 전체가 이 방식이었으므로, 완전히 빈 `AppState`는 바이트 길이 0인 문자열을 렌더링했고, `render_prometheus_exposition`의 모듈 문서와 테스트 `empty_inputs_render_empty_string`이 이를 의도된 동작으로 그대로 못 박고 있었다.

### 1.2 기존 문제점

- **문제 1 (성공한 스크레이프가 샘플 0개를 실어 나를 수 있음)**: `/metrics`는 리스너 바인딩부터 첫 수집 주기가 `AppState`에 쓸 때까지 바이트 0개로 `200 OK`를 답한다. Prometheus는 이 구간의 스크레이프를 샘플 없는 성공으로 기록한다. 실패한 타깃과 달리 알람이 울리지 않는 조용한 시계열 구멍이다.
- **문제 2 (오케스트레이터를 위한 예/아니오 신호 부재)**: `/metrics`를 겨눈 쿠버네티스 `readinessProbe`나 로드밸런서 헬스체크는 리스너가 바인딩되는 순간, 즉 아직 내놓을 게 아무것도 없을 때 통과한다. 엔드포인트가 관측 가능한 유일한 신호가 상태 코드뿐이었기 때문이다.
- **문제 3 (Windows SCM 래치가 정의되지 않은 응답 위로 열림)**: `src/api/latch.rs`는 서빙 래치가 "리스너가 바인딩되는 즉시" 열린다고 문서화하고 있고, `mark_serving()`은 바인딩 성공 직후 호출된다(`src/api/shutdown.rs`). 그러니 `/metrics`가 아직 바이트 0개를 내보내는 와중에도 `SERVICE_RUNNING`이 보고될 수 있었다.
- **문제 4 (노출 체계에 무조건 나오는 줄이 없음)**: 체인 안 어떤 지표군도 데이터 유무와 무관하게 나오지 않았다. 그래서 "떴지만 아직 수집되지 않음"과 "떴고 보고할 게 정말 없음"을 인밴드로 구분할 방법이 없었다.
- **문제 5 (문서 표류)**: man 페이지의 `API ENDPOINTS (API MODE)` 절은 `/health`가 `"OK"`를 반환한다고 적어놓았다. `src/api/server.rs`는 이 PR 이전에 그런 라우트를 마운트한 적이 없다. 존재한 건 `/metrics`, `/events`, `/snapshot`뿐이었다.

### 1.3 위험성

| 위험 | 영향 | 발생 가능성 |
|---|---|---|
| 수집 전 구간에 떨어진 스크레이프나 준비 상태 프로브가 정상으로 처리됨 | Medium (시계열에 조용히 뚫린 구멍, 혹은 실제 데이터가 없는데도 준비 완료로 표시된 쿠버네티스 파드) | 이 수정 전에는 모든 프로세스 시작마다 확실히 발생 |
| 구간 문제를 근원에서 고치겠다며 `mark_serving()`을 첫 수집 주기 뒤로 미룸 | High (실행했다면): `scm_host.rs`가 10초 대기 힌트로 `StartPending`을 딱 한 번만 보고하고 체크포인트가 증가하지 않으므로, 느린 첫 수집(GPU가 많은 호스트에서의 콜드 WMI/NVML 열거)이 SCM의 시작 실패와 같은 느린 경로로의 재시작 반복을 낳을 수 있음 | 이 PR이 `mark_serving()`을 바인드 시점에 남기기로 하면서 회피됨. 실수가 아니라 검토 후 기각한 선택지로 기록됨 |
| `all_smi_up`과 `/-/ready`가 독립적으로 계산되어 서로 어긋남 | Medium (한쪽으로 게이트를 걸고 다른 쪽으로 알람을 거는 소비자는 풀 방법 없는 모순을 만나게 됨) | 구조적으로 회피됨: 둘 다 단일 술어 `api::handlers::ready::is_ready`를 읽고, 전환 구간 양쪽에서 절대 어긋나지 않음을 테스트가 확인함 |

---

## 2. 기술적 검토 사항

### 2.1 정확성

노출 변경은 렌더링 단계에서 순수하게 추가적이다. `ExporterStatusMetricExporter`(`src/api/metrics/exporter_status.rs`, 신규)는 체인에서 유일하게 스스로 필터링하지 않는 익스포터이고, `render_prometheus_exposition`의 출력 맨 앞에 모든 디바이스 지표군보다 먼저 붙는다. 그래서 응답 앞부분만 읽는 소비자나 첫 주기가 오기 전에 스크레이프하는 소비자도 `all_smi_up`과 `all_smi_build_info`는 여전히 본다. 디바이스 익스포터는 손대지 않았고 여전히 스스로 필터링하는데, `empty_inputs_render_only_the_baseline_families`가 완전히 빈 입력 집합이 두 기준선 지표군 이상을 렌더링하지 않음을 확인한다.

준비 상태 술어는 함수 하나, `api::handlers::ready::is_ready(state: &AppState) -> bool`이고 `!state.loading`으로 정의된다. `/-/ready`의 상태 코드와 `all_smi_up` 게이지 값 둘 다 같은 `AppState` 읽기에 대해 이 함수를 호출해서 얻는다. 어긋남을 그저 드물게 만드는 게 아니라 구조적으로 불가능하게 만드는 지점이 바로 여기다. `metrics_handler`는 디바이스 데이터를 읽는 것과 같은 락 획득 아래에서 `ready: is_ready(&state)`를 `MetricsRenderInputs`에 넘기고, `ready_handler`는 동일한 함수를 호출한다. `tests/api_readiness_test.rs`의 `ready_endpoint_and_up_gauge_never_disagree`는 렌더러 수준이 아니라 실제 라우터를 상대로 전환 구간 양쪽에서 이를 확인하는데, 핸들러가 `ready`를 넘기는 걸 깜빡하거나 라우트를 마운트하지 않는 유형의 버그를 정확히 잡아낸다.

스냅샷 시리얼라이저(`src/snapshot/serializers/prometheus.rs`)는 `ready: true`를 고정값으로 넣는다. `snapshot --format prometheus`는 동기 수집이 이미 반환한 뒤에야 이 코드에 도달하기 때문이다. 라이브 스크레이프와 원샷 스냅샷 사이의 바이트 단위 동일성은 `prometheus_output_is_byte_identical_to_api_exporter_for_same_data`가 확인하는데, 동일성 테스트 자신의 입력에도 같은 `ready: true`를 맞춰 넣어서 유지된다.

### 2.2 성능 관점

`ExporterStatusMetricExporter::export_metrics`는 스크레이프마다 시스템 콜을 도는 대신 프로세스 전역 `Lazy<String>`을 읽는 `get_hostname()`을 호출한다. 그러니 요청당 추가 비용은 기존 디바이스 익스포터들 앞에 놓이는 `MetricBuilder` 항목 두 개(두 지표군 합쳐 레이블 여섯 개)뿐이다. 백그라운드 작업, 락, 폴링은 새로 생기지 않는다. `/-/ready`도 `/metrics` 핸들러가 이미 쓰던 `AppState` 읽기 락을 그대로 재사용한다.

### 2.3 호환성 및 의존성

- **Breaking Changes**: 와이어 수준에서는 없다. 기존 지표군은 이름, 타입, 레이블 전부 그대로다. 기존 소비자에게 유일하게 바뀌는 동작은 시작 구간에 `/metrics` 본문이 더 이상 바이트 0개가 아니라는 점이다. 빈 본문에 특별히 의존하던 스크레이퍼가 있었다면(알려진 사례는 없다) 다른 상태 코드가 아니라 다른 내용을 보게 된다.
- **새로운 의존성**: 없다.
- **호환성**: `/-/ready`는 새로 추가된 라우트다(`src/api/server.rs`). 이전에 해석되던 경로가 이제 404가 되는 일은 없다. man 페이지 수정은 동작하던 기능을 제거하는 게 아니라 문서화만 되어 있고 실체는 없던 `/health`를 지운 것이다.

### 2.4 코드 품질

`tests/api_readiness_test.rs`는 신규 파일로, 테스트 전용 목이 아니라 루프백 TCP 연결 위에서 raw HTTP/1.1 요청으로 실제 axum 라우터를 구동한다. 렌더러 수준 테스트로는 `ready`를 넘기는 걸 깜빡한 핸들러나 `/-/ready`를 마운트하지 않은 라우터를 잡아낼 수 없기 때문에 이렇게 짰다. 일곱 개 테스트 케이스가 각각 확인하는 것은 이렇다. 첫 수집 전에도 `/metrics`가 `200`이고 비어있지 않다는 것, `all_smi_up`이 전환 구간에서 `0`에서 `1`로 바뀐다는 것, 첫 요청부터 `^all_smi_` 접두사가 붙은 줄이 존재한다는 것(PR #323의 CI 우회책이 폴링하던 바로 그 패턴이 이제 무조건 성립함), `/-/ready`가 이전엔 `Retry-After: 1`과 `Cache-Control: no-store`를 붙인 `503`, 이후엔 `200`이라는 것, 엔드포인트와 게이지가 전환 구간 내내 어긋나지 않는다는 것, 그리고 마운트되지 않은 이웃 경로(`/-/healthy`)는 여전히 404를 낸다는 것이다. 마지막 항목은 누군가 특이한 `/-/` 접두사를 "고친다"며 와일드카드 라우트를 넣는 걸 막는 방어선이다. `render.rs`와 `prometheus.rs`의 테스트 스위트는 첫 커밋이 CI에서 `empty_snapshot_renders_empty_string`을 깨뜨린(31100471191번 실행) 뒤 두 번째 커밋에서 갱신됐다. 개발 중 썼던 범위 필터(`--lib api::`, `--lib snapshot::serializers`)가 이 테스트까지는 닿지 않았던 탓이다. 수정은 그 단언을 그냥 고치는 대신 `empty_snapshot_still_renders_the_baseline`으로 대체해서, 실패만 땜질하지 않고 고쳐진 계약 자체를 못 박았다.

---

## 3. 기술적 선택과 그 이유

### 3.1 별도의 준비 상태 엔드포인트와 인밴드 기준선을 결합하고, 200 지연은 기각

**컨텍스트**: 이슈 #324는 서로 배타적이지 않은 세 선택지를 제시했다. (1) 첫 수집 주기가 `AppState`를 채울 때까지 `200`을 늦춘다. 이러면 Windows SCM의 `SERVICE_RUNNING` 전환과 어떤 `readinessProbe`든 실제 준비 상태 뒤로 함께 밀린다. (2) `/ready` 같은 별도 준비 상태 신호를 두고 `/metrics` 의미는 손대지 않는다. (3) 최소 한 줄은 항상 내보내서, 이 구간의 스크레이프를 디바이스가 없는 호스트의 스크레이프와 구분되게 한다.

| 옵션 | 장점 | 단점 |
|---|---|---|
| 옵션 1: 200 지연 | 정신 모델이 가장 단순하다: 준비 안 됐으면 응답하지 않는다 | `/metrics`의 상태 코드를 내부 준비 상태에 결합해서, 느리게 뜨는 `/metrics`를 실패한 타깃으로 취급하던 기존 스크레이퍼나 헬스체크를 깨뜨린다. SCM/래치 전환도 같은 느린 경로 뒤로 함께 밀린다(3.2절) |
| 옵션 2 단독: `/-/ready`만 | 오케스트레이터용 목적 지향 예/아니오 신호 | 새 엔드포인트를 폴링하지 않는 누군가에게는, 이 구간의 평범한 `/metrics` 스크레이프가 여전히 조용한 샘플 0개짜리 구멍 |
| 옵션 3 단독: 기준선 지표만 | 모든 스크레이프가 자기 상태를 설명함 | `readinessProbe`나 로드밸런서용 전용 예/아니오 게이트가 없다. 상태 코드가 아니라 `all_smi_up` 값을 해석해서 준비 상태를 추론해야 함 |
| **채택: 옵션 2 + 옵션 3, 옵션 1은 기각** | `/metrics`는 상태 코드 계약을 절대 바꾸지 않아서 기존 스크레이퍼 어느 것도 영향받지 않는다. 스크레이프만 하는 소비자는 `all_smi_up`을 얻고, 오케스트레이터는 `/-/ready`라는 전용 게이트를 얻는다 | 두 표면을 계속 맞춰야 하는데, 둘 다 준비 상태를 두 번 계산하는 대신 같은 술어(`is_ready`)를 읽게 해서 해결 |

**선택 이유**: 채택한 두 옵션은 서로 다른 소비자에게 서로 다른 질문에 답하고, 둘을 결합하는 비용은 같은 술어로 뒷받침하는 것 이상으로 들지 않는다. 옵션 1을 기각한 것은 PR #323의 CI 우회책을 비롯해 외부 스크레이퍼 전반이 이미 의존하는 기존 `/metrics` 계약(바인드 시점부터 200)을 지키기 위해서이기도 하고, SCM의 liveness 전환을 하드웨어 열거 지연에 다시 묶지 않기 위해서이기도 하다(3.2절).

**트레이드오프**: 흑백이 분명한 게이트가 필요한 소비자는 `/metrics`가 아니라 `/-/ready`를 봐야 한다는 걸 알아야 한다. 인밴드 `all_smi_up` 게이지는 자동화 관점에서는 상태 코드보다 약한 신호(상태 코드가 아니라 PromQL 질의가 필요)지만, 순수 스크레이프에서 볼 수 있는 유일한 신호이기도 하다.

### 3.2 `mark_serving()`은 첫 수집 주기 뒤로 밀리지 않고 바인드 시점에 남는다

**컨텍스트**: 이슈 #324의 근거 섹션은 Windows SCM 준비 상태 래치(PR #320, 이슈 #311)가 "리스너가 바인딩되는 즉시" 열린다는 점을 구체적으로 짚으면서, 이제 진짜 준비 상태 신호가 생겼으니 이걸 바꿔야 하는지 물었다.

**결정**: 옮기지 않는다. 세 가지 이유가 `src/api/shutdown.rs`의 `mark_serving` 문서 주석에 기록되어 있다.

1. **두 개는 서로 다른 질문이다.** SCM의 상태 기계는 `SERVICE_START_PENDING`과 `SERVICE_RUNNING`을 제공한다. `SERVICE_RUNNING`은 준비 상태 판정이 아니라 liveness 판정이고, 네트워크 익스포터의 자연스러운 liveness 경계는 "리스너가 응답한다"는 것이다. 이는 쿠버네티스와 Prometheus 생태계가 이미 쓰는 것과 같은 liveness/readiness 분리이며, 준비 상태는 이제 필요한 누구든 별도로 조회할 수 있다.
2. **불일치는 반대편에서 해결된다.** 이 PR 이전에는 래치가 바이트 0개를 내보내는 엔드포인트 위로 열려서 `SERVICE_RUNNING`이 사실상 아무것도 보장하지 못했다. 이제 래치가 열리는 순간 `/metrics`는 정의된, 비어있지 않은 응답(`all_smi_up 0`과 빌드 정보)을 실어 나른다. `/metrics`에 바닥을 깔아주는 것으로 불일치가 풀렸으니 래치를 늦춰서 두 번째로 풀 필요는 없다.
3. **옮기면 막으려는 것보다 더 나쁜 실패 모드가 생긴다.** `src/service_cmd/scm_host.rs`는 `StartPending`을 정확히 한 번, `wait_hint = TRANSITION_WAIT_HINT_SECS`(10초, `src/service_cmd/scm.rs:70`)와 `checkpoint: 0`으로 보고한다. 체크포인트가 절대 증가하지 않으므로 이 단 한 번의 보고가 SCM이 허용하는 시작 예산 전체다. Windows에서 첫 수집 주기는 콜드 COM/WMI 초기화에 NVML 열거까지 포함하는데, GPU가 많은 호스트나 멈춘 드라이버 뒤에서는 10초를 넘기기 쉽다. 그러면 SCM은 시작 실패로 판정하고 설정된 복구 동작을 적용해 같은 느린 경로로 재시작을 반복한다. 텔레메트리가 가장 중요한 바로 그 호스트에서 부팅 루프가 도는 셈이다.

**받아들인 트레이드오프**: Windows 서비스가 `all_smi_up`이 아직 `0`인 상태에서도 `SERVICE_RUNNING`을 보고할 수 있다. 이건 그저 감수하는 게 아니라 올바른 것으로 판단했다. 이제는 조용히 비어있는 상태가 아니라 정직하게 보고된 상태(`all_smi_up 0`이 눈에 보임)이기 때문이다.

### 3.3 준비 상태 술어를 한 번 계산해 렌더링 입력으로 넘기고, 렌더러 안에서 `AppState`를 다시 읽지 않는다

**컨텍스트**: `render_prometheus_exposition`과 그 입력 구조체 `MetricsRenderInputs`는 스냅샷 시리얼라이저에서도 쓰이는데, 이쪽은 렌더링 시점에 잠글 살아있는 `AppState`가 없다.

| 옵션 | 장점 | 단점 |
|---|---|---|
| 렌더러가 `&AppState`를 받아 내부에서 `is_ready`를 계산 | `MetricsRenderInputs`의 필드 하나가 줄어듦 | 살아있는 상태가 없는 스냅샷 경로를 포함해 모든 호출자가 `AppState`를 조작하거나 잠그도록 강제함. 순수해야 할 렌더링 함수가 라이브 서버 타입에 결합됨 |
| **채택: `MetricsRenderInputs`에 `ready: bool` 추가, 각 호출자가 계산** | 라이브 핸들러와 스냅샷 시리얼라이저 각자 자기 실행 모델에 맞는 값을 넘긴다. 렌더러는 입력에 대한 순수 함수로 남는다 | 호출자가 올바른 값을 넘기는 걸 잊지 않아야 함. `tests/api_readiness_test.rs`가 라이브 핸들러를 구체적으로 검증해서 완화됨 |

**선택 이유**: `render_prometheus_exposition`을 `MetricsRenderInputs`에 대한 순수 함수로 유지하는 것이야말로 애초에 `prometheus_output_is_byte_identical_to_api_exporter_for_same_data`가 API 경로와 스냅샷 경로 사이의 바이트 단위 동일성을 확인할 수 있게 해주는 조건이다. 여기에 살아있는 `AppState` 참조를 꿰어 넣으면 그 대칭성이 아무 이득 없이 깨진다. 스냅샷 경로에는 애초에 살아있는 상태가 없기 때문이다.

---

## 4. 구현 상세

### 4.1 아키텍처 변경

```
[변경 전]
AppState (첫 주기 전까지 비어있음)
    │
    ▼
render_prometheus_exposition(inputs)  -- 모든 익스포터가 스스로 필터링
    │
    ▼
"" (바이트 0개 본문, 200 OK)

[변경 후]
AppState.loading  ──────────────┐
    │                            │  is_ready(&state)
    ▼                            ▼
metrics_handler            ready_handler
    │                            │
    ▼                            ▼
MetricsRenderInputs{ready}   /-/ready: 503 (Retry-After: 1) | 200
    │
    ▼
render_prometheus_exposition(inputs)
    │
    ├─ ExporterStatusMetricExporter (무조건: all_smi_up, all_smi_build_info)
    └─ 디바이스 익스포터 전부 (여전히 스스로 필터링)
    │
    ▼
비어있지 않은 본문, 200 OK, 항상
```

### 4.2 주요 코드 변경

**파일: `src/api/handlers/ready.rs`(신규, 준비 상태 술어 하나)**
```rust
/// [`AppState::loading`] starts `true` and is cleared by
/// [`crate::api::collection_loop::run_collection_loop`] at the end of its
/// first iteration, which is exactly the transition being described. It is
/// never set back to `true` on the API path.
pub fn is_ready(state: &AppState) -> bool {
    !state.loading
}

pub async fn ready_handler(State(state): State<SharedState>) -> Response {
    let ready = is_ready(&*state.read().await);
    readiness_response(ready)
}
```
**변경 이유**: 함수 하나가 계약의 양쪽(`/-/ready` 상태 코드와 `all_smi_up` 게이지)을 모두 떠받친다. 그러니 둘이 준비 상태를 독립적으로 계산해서 어긋날 여지가 없다.

**파일: `src/api/shutdown.rs`(`mark_serving()`을 바인드 시점에 남기기로 한 결정)**
```rust
/// Third, moving it has a concrete failure mode that is worse than the
/// one it would prevent. `crate::service_cmd::scm_host` reports
/// `StartPending` exactly once, with `wait_hint =
/// TRANSITION_WAIT_HINT_SECS` (10 s) and `checkpoint: 0`. Because the
/// checkpoint never increments, that single report is the entire start
/// budget the SCM grants. A first collection cycle on Windows means cold
/// COM/WMI initialization plus NVML enumeration, which on a many-GPU
/// host or a wedged driver can exceed 10 s. The SCM would then fail the
/// start and apply the configured recovery actions, restarting the
/// process into the same slow path...
pub(crate) fn mark_serving() {
    serving_latch().trigger();
}
```
**변경 이유**: 이 주석은 3.2절 결정의 산물이다. "SCM이 데이터도 없는데 running을 보고한다"는 질문을 나중에 다시 마주칠 기여자가 `scm_host.rs`의 타이밍 제약을 처음부터 다시 파헤치지 않도록 남겨둔 것이다.

**파일: `src/api/metrics/render.rs`(기준선을 뒤가 아니라 앞에 붙임)**
```rust
// Baseline first (issue #324), so a consumer that reads only the head
// of the response, or scrapes before the first collection cycle has
// landed, still learns whether this exporter is up and which build it
// is. Everything below this line self-filters; this block does not.
let status_exporter = ExporterStatusMetricExporter::new(inputs.ready);
all_metrics.push_str(&status_exporter.export_metrics());
```
**변경 이유**: 순서는 의도적이다. 응답 본문을 잘라서 보는 소비자나 그냥 훑어보는 사람이나, 어떤 디바이스 지표군보다 먼저 기준선을 보게 된다.

### 4.3 데이터 모델 변경

스키마 변경은 아니다. `MetricsRenderInputs`에 필드 하나, `pub ready: bool`이 늘었다. 내부적으로는 기존에 있던 `AppState::loading`을 이제 임시방편으로 참조하는 대신 이름 붙은 술어(`is_ready`)로 읽고, `/-/ready`와 `all_smi_up` 게이지 둘 다 이 술어 하나만 쓴다.

---

## 5. 학습 포인트

### 5.1 Liveness와 readiness는 다른 질문이고, 둘 중 하나만 가진 서비스 관리 API에 나머지 하나를 억지로 답하게 해선 안 된다

**개념**: liveness는 "프로세스가 살아있고 멈추지 않았는가"에 답하고, readiness는 "프로세스가 서비스할 실제 작업을 갖고 있는가"에 답한다. 쿠버네티스가 이 둘을 별개의 프로브로 모델링하는 이유가 바로 이것이다. Windows SCM의 `SERVICE_RUNNING`처럼 liveness 모양의 상태 기계만 노출하는 서비스 관리자에 readiness 조건을 몰래 끼워넣으면, 오케스트레이터의 재시작 정책이 readiness 조건이 참이 되는 데 걸리는 시간에 결합되어 버린다.

**이 PR에서의 적용**: `mark_serving()`/`SERVICE_RUNNING`은 liveness 신호로 남았고, `/-/ready`와 `all_smi_up`이 완전히 분리된 readiness 신호가 됐다. readiness를 SCM 전환에 접어 넣는 대안을 택했다면, 느린 하드웨어 열거가 서비스 관리자가 유발하는 재시작 루프로 바뀌었을 것이다.

### 5.2 모든 입력을 명시적 구조체로 꿴 순수 렌더링 함수가 경로 간 동일성을 테스트 가능하게 만든다

**개념**: 같은 출력을 서로 다른 실행 모델을 가진 두 코드 경로(살아있는 비동기 핸들러와 동기 원샷 컬렉터)가 만들어야 할 때, 공유 로직을 주변 상태를 뒤지는 함수가 아니라 명시적 입력 구조체에 대한 순수 함수로 유지하는 것이 두 경로가 같은 입력에서 동일한 출력을 낸다고 테스트로 확인할 수 있게 하는 조건이다.

**이 PR에서의 적용**: `render_prometheus_exposition(&MetricsRenderInputs)`는 `&AppState`를 받는 대신 `ready: bool`을 명시적 필드로 추가하면서 순수함을 유지했다. `prometheus_output_is_byte_identical_to_api_exporter_for_same_data`가 계속 성립할 수 있는 건 이 선택 덕분이다.

### 5.3 단일 술어 함수는 "이 둘은 일치해야 한다"보다 강한 정확성 보장이다

**개념**: 같은 사실을 나타내야 하는 두 값을 독립적으로 계산하면, 한쪽 코드 경로만 바뀌는 순간 어긋나기 시작한다. 둘을 하나의 함수로 라우팅하면 그 버그 유형 자체가 아예 존재할 수 없게 된다. 단순히 그렇지 않은지 테스트하는 것과는 다르다.

**이 PR에서의 적용**: `is_ready(state: &AppState) -> bool`은 `metrics_handler`(`all_smi_up`을 설정하기 위해)와 `ready_handler`(`/-/ready` 상태 코드를 설정하기 위해) 둘 다에서 호출되며, 지표 경로에서는 같은 락 획득 아래에서 호출된다. `ready_endpoint_and_up_gauge_never_disagree`는 이 속성을 확인하는 테스트일 뿐, 그것을 보장하는 메커니즘이 아니다. 메커니즘은 호출할 함수가 애초에 하나뿐이라는 사실이다.

---

## 6. 추가 학습

### 핵심 용어

| 용어 | 설명 | 관련성 |
|---|---|---|
| Liveness vs. readiness | 서로 다른 두 헬스 질문: 프로세스가 살아있는가 vs. 실제 데이터를 갖고 있는가 | 이 PR이 `mark_serving()`/SCM liveness와 `/-/ready`/`all_smi_up` readiness 사이에 그은 경계 |
| `all_smi_up` | 새로 추가된 무조건 게이지. 첫 수집 주기 전엔 0, 이후 1 | 순수 스크레이프에도 보이는, readiness 계약의 인밴드 절반 |
| `all_smi_build_info` | 새로 추가된 무조건 게이지. 항상 1이고 내용은 레이블(`version`, `os`, `arch`)에 담김 | `version`을 다른 시계열에 조인하는 `node_exporter`/Prometheus의 build-info 관용구를 따름 |
| `GET /-/ready` | 새로 추가된 아웃오브밴드 준비 상태 라우트 | 오케스트레이터를 위한 전용 예/아니오 게이트. Prometheus 생태계의 `/-/` 관례를 따름 |
| `mark_serving()` / 서빙 래치 | 기존 프리미티브(PR #320/#321). Windows SCM과 launchd/systemd가 프로세스를 running으로 보고하게 함 | readiness 뒤로 옮기지 않고 바인드 시점에 의도적으로 남김(3.2절) |
| `TRANSITION_WAIT_HINT_SECS` | 대기 중인 시작에 대해 SCM이 부여하는 10초짜리 대기 힌트. 정확히 한 번 보고됨 | `mark_serving()`을 옮기지 않기로 한 결정을 뒷받침한 구체적 제약 |

### 관련 기술/프레임워크

- Prometheus 노출 관례: Prometheus, Alertmanager, Pushgateway가 쓰는 `/-/ready`, `/-/healthy` 경로 접두사와 `node_exporter`·Prometheus 자신이 쓰는 상수-1 `*_build_info` 관용구.
- Windows Service Control Manager(SCM) 상태 기계: `SERVICE_START_PENDING`, `SERVICE_RUNNING`, 대기 힌트, 체크포인트.
- 쿠버네티스 liveness/readiness 프로브 의미론. 이 PR이 그은 경계의 모델로 참조됨.

### 관련 PR/이슈

- 이슈 #324: 이 PR이 닫는 이슈.
- PR #323: 이 PR의 무조건 기준선이 무의미하게 만드는 CI 우회책(`grep -q '^all_smi_'`). PR #335가 그 CI 잡을 `/-/ready`로 옮긴다.
- PR #320(Windows SCM), PR #321(launchd): 이 PR이 의도적으로 손대지 않은 서빙 래치와 `mark_serving()`을 추가했다.
- PR #335: systemd·launchd 스모크 테스트 둘 다를 콘텐츠 기반 게이팅에서 `/-/ready`로 옮긴다. 실제로 이 PR의 readiness 계약에 의존하는 쪽이다.

---

## 7. 변경 요약

### 통계

| 항목 | 값 |
|---|---|
| 변경 파일 | 15 |
| 추가 줄 | +932 |
| 삭제 줄 | -23 |
| 커밋 | 2 |
| 신규 파일 | `src/api/handlers/ready.rs`, `src/api/metrics/exporter_status.rs`, `tests/api_readiness_test.rs` |

### 카테고리별 변경

| 분류 | 내용 |
|---|---|
| API | `GET /-/ready` 라우트 신규. 모든 `/metrics` 응답 앞에 무조건 붙는 `all_smi_up`/`all_smi_build_info` 기준선 신규 |
| 문서 | `mark_serving()` 결정을 `src/api/shutdown.rs`와 `src/api/latch.rs`에 기록. README.md·API.md에 "Readiness and the Startup Window" 절 추가. man 페이지의 존재하지 않던 `/health` 항목을 실제 엔드포인트 목록으로 교체 |
| 테스트 | `tests/api_readiness_test.rs` 신규(실제 라우터를 상대로 한 통합 테스트 7개). `src/api/metrics/render.rs`와 `src/snapshot/serializers/prometheus.rs`의 테스트가 빈 문자열이 아니라 기준선을 확인하도록 갱신 |
| 호환성 | 기존 지표 이름·타입·레이블 변경 없음. `/metrics`의 상태 코드 계약(바인드부터 200)도 그대로 |

### 관련 커밋

| SHA | 유형 | 메시지 |
|---|---|---|
| `612a7a01` | fix(api) | always emit a metrics baseline and add /-/ready |
| `4c3a8fa4` | test | supersede the snapshot serializer's empty-output assertion |

`main`에 `5f2fa816`로 병합됨. #324를 닫는다.

---

## 8. 후속 조치

### 필수

PR에서 확인된 것은 없다. 계약은 실제 바이너리에 대해 검증됐고(아래 부록의 라이브 검증 참고), 렌더러 단독이 아니라 실제 라우터를 상대로 도는 통합 테스트로 고정되어 있다.

### 모니터링 필요

- Windows SCM 아래에서의 동작은 이 PR에서 라이브 SCM 실행이 아니라 `scm_host.rs`(단일 `StartPending` 보고, 10초 대기 힌트, 절대 증가하지 않는 체크포인트)를 읽어서 도출됐다. PR 자신도 이 점을 명시하고 있고, 그 경로의 코드는 아무것도 바뀌지 않았으니 새로 검증되지 않은 주장이 아니라 기존 동작에 새로 문서화된 근거가 붙은 것이다.
- 이 PR 시점에는 launchd·systemd 스모크 잡이 여전히 콘텐츠 기반으로 게이팅되고 있었다. PR #335가 이들을 `/-/ready`로 옮긴다.

### 향후 개선 사항

- PR #335를 위해 이미 계획된 이관 외에 PR 자체에서 제안된 것은 없다.

---

## 부록

### A. 테스트 결과

- `cargo test --test api_readiness_test`: 7개 통과.
- `cargo test --lib api::`: 113개 통과.
- `cargo test --test snapshot_test`: 13개 통과. `prometheus_output_is_byte_identical_to_api_exporter_for_same_data` 포함.
- `cargo clippy --lib --tests -- -D warnings`와 `cargo clippy --bin all-smi -- -D warnings`: 둘 다 클린. 크레이트가 모듈 트리를 두 번 컴파일하고 PR #319/#320/#321이 각각 라이브러리 타깃에서는 살아있고 바이너리 타깃에서는 죽은 `pub` 아이템에 걸렸던 전례가 있어 따로 돌림.
- `cargo fmt --check`: 클린.
- 실제 바이너리에서 첫 수집 전 구간을 일부러 경합시킴: `/metrics`가 0바이트가 아니라 399바이트로 `HTTP 200`을 반환했고, `all_smi_up{...} 0`과 `all_smi_build_info{...,version="0.25.0",os="macos",arch="aarch64"} 1`을 담고 있었다. 같은 구간에서 `/-/ready`는 `retry-after: 1`과 `cache-control: no-store`를 붙인 `503 Service Unavailable`을 반환했다. 주기가 끝난 뒤 `/-/ready`는 `all-smi is ready.`와 함께 `200 OK`를 반환했고 `/metrics`는 `all_smi_up ... 1`을 보고했다.

### B. 성능 벤치마크

별도로 벤치마크하지 않았다. 요청당 추가된 비용은 기존 익스포터 앞에 렌더링되는 레이블 집합 두 개뿐이다. 새로운 락이나 백그라운드 작업은 도입되지 않았다.

### C. 참고 자료

- 이슈 #324: 이 보고서가 근거로 삼은 근본 원인 서술, 세 가지 선택지, 인수 기준. diff와 교차 확인함.
- `src/service_cmd/scm_host.rs`, `src/service_cmd/scm.rs`: 3.2절 결정을 이끈 SCM 타이밍 제약(`TRANSITION_WAIT_HINT_SECS`, `checkpoint: 0`).
- Prometheus, Alertmanager, Pushgateway의 `/-/ready` 관례.
