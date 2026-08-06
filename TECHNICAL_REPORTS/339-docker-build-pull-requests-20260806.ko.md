# 기술 보고서: PR #339 - ci: build the Docker image on pull requests

**작성일**: 2026-08-06
**상태**: 완료
**언어**: YAML(GitHub Actions), Rust(테스트)
**위험도**: Low(CI 트리거와 캐싱 정책 변경에 테스트 두 개 추가. 애플리케이션 소스는 건드리지 않음)

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

`docker-check`는 `github.event_name == 'push' && github.ref == 'refs/heads/main'`에 게이팅되어 있어서, 이미지 손상은 절대 pull request로는 나타나지 않고 오직 red인 기본 브랜치로만 나타날 수 있었다. 이슈 #328은 정확히 그런 일이 벌어진 뒤 접수됐다. PR #319(#309)가 크레이트 최초로 `include_str!`로 임베드한 자산을 추가했는데, Dockerfile의 빌더 스테이지가 그게 든 디렉터리를 복사하지 않았고, 모든 PR 검사는 통과했으며, `main`은 병합 커밋 `74f75d2`에서 red가 됐다. 이슈는 추정이 아니라 측정된 비용으로 결정을 내려달라고 요구했고, 그 결정은 `main`의 10회 실행에서 `gh api .../actions/runs/<id>/jobs`로 뽑아낸 숫자 두 개에 근거한다. `docker-check`와 `build-check`는 둘 다 `needs: test`라서 함께 시작하고, `docker-check`는 모든 PR에서 이미 도는 `build-check`보다 평균 2초 늦게 끝난다. 저장소가 공개라서 표준 러너 분(minute)은 과금되지 않으므로, 모든 PR에서 전체 이미지 빌드를 도는 측정된 한계 비용은 대략 2초의 벽시계 시간이고, 이 PR 자신의 CI 실행(31106052626)에서 확인됐다. Docker 빌드는 게이팅하는 `build-check` 잡보다 109초 먼저 끝났다.

기각된 대안은 이 이슈가 애초에 왜 존재했는지에 대한 고리를 닫는다. `Dockerfile`/`.dockerignore`/`Cargo.toml`/`Cargo.lock`/`build.rs`에 대한 경로 필터였다면 `main`의 최근 40개 커밋 중 19개(48%)에서 빌드를 건너뛰었을 것이고, 그 건너뛴 집합에는 실제로 `main`을 부순 커밋인 `74f75d2` 자신이 들어 있다. 소스 수준의 `include_str!`을 추가했을 뿐 필터 경로 어디도 건드리지 않았기 때문이다. 이미 0에 수렴하는 비용을 아끼려고 기록에 남은 유일한 실제 실패를 놓치는 필터는 나쁜 거래로 기각됐다. `cache-to`는 push 전용으로 바뀐다(pull request는 `main`의 따뜻한 캐시를 읽되 아무것도 다시 쓰지 않는다). PR이 촉발하는 `mode=max` 내보내기가 `main`이 의존하는, 공유되고 LRU 방식으로 축출되는 약 13.3GB짜리 캐시를 흔들지 못하게 하기 위해서다. `tests/docker_build_context_test.rs`는 기존 임베드-자산 테스트에는 없던 반대 방향 검사를 얻는다. 빌더 스테이지의 모든 `COPY` 소스가 실제로 빌드 컨텍스트에 존재해야 한다는 것이고, 무관한 변경이 이름을 바꾸거나 지운 경로를 여전히 가리키는 `COPY`를 잡아낸다. 같은 입력에 대해 실제 `docker buildx build`가 2초 만에 실패하는 것으로도 검증됐다. 전체 규모는 파일 2개, +169/-11, 커밋 1개, #328을 닫는다. 이슈 댓글에 기록됐지만 여기서 고치지는 않은 부수적 발견 하나: `docker build --platform linux/arm64`는 이 이슈와 무관하게 깨져 있다. `Cargo.toml`의 `aarch64`+`gnu`용 벤더드 OpenSSL 경로가 perl의 `FindBin`을 필요로 하는데, `rust:1.96-slim`에는 그게 없다.

---

## 1. 문제 정의

### 1.1 배경

`.github/workflows/ci.yml`의 `docker-check` 잡은 `docker/build-push-action`을 통해 프로젝트의 Docker 이미지를 빌드하며, 이 PR 이전부터 GHA 레이어 캐싱(`cache-from: type=gha`, `cache-to: type=gha,mode=max`)이 이미 연결되어 있었다. 이 PR 이전에는 `main`으로의 push에서만 돌았기 때문에 절대 pull request를 게이팅할 수 없었다. 이미지 손상이 만들어내는 유일한 신호는 사후에 발견되는 red인 기본 브랜치뿐이었다.

### 1.2 기존 문제점

- **문제 1 (손상을 잡았어야 할 게이트가 그걸 들여온 PR에서는 한 번도 안 돎)**: PR #319(#309)는 `include_str!`로 임베드한 `packaging/systemd/all-smi.service`를 추가했다. Dockerfile의 빌더 스테이지는 `packaging/` 디렉터리를 빌드 컨텍스트에 복사하지 않아서, 이미지 안의 `cargo build`가 `couldn't read src/service_cmd/../../packaging/systemd/all-smi.service`로 실패했다. `docker-check`가 `main` push에만 게이팅되어 있어서 PR #319 자체는 완전히 green으로 보였고, 실패는 병합 커밋에서만 나타났으며 별도로 PR #322가 고쳤다.
- **문제 2 (기존 임베드-자산 테스트는 한 방향만 커버함)**: `tests/docker_build_context_test.rs`의 `embedded_assets_are_inside_the_docker_build_context`(PR #322가 추가)는 모든 `include_str!`/`include_bytes!` 대상이 어떤 빌더 스테이지 `COPY`로든 도달 가능한지 확인하는데, 이건 PR #319가 걸린 유형이지만 반대 방향에는 아무 말도 하지 않는다. 컨텍스트에 아예 존재하지 않는 경로를 이름으로 삼은 `COPY`는, 무관한 이름 변경이나 삭제가 들여올 수 있다.
- **문제 3 (PR에서 전체 빌드를 도는 비용이 측정된 적 없음)**: 이슈는 모든 PR에서 빌드를 돌지 아니면 더 싼 변형을 채택할지 결정하기 전에 추정이 아니라 측정된 비용을 명시적으로 요구했다.

### 1.3 위험성

| 위험 | 영향 | 발생 가능성 |
|---|---|---|
| 모든 PR 검사가 green인 채로 이미지를 부수는 변경이 `main`에 병합되고, 사후에야 발견됨 | 발생 건당 High(PR #319/#322에서 이미 그랬듯 red인 기본 브랜치) | 이미 한 번 시연됨. 이 PR이 닫는 게이트가 그 직접적인 수정 |
| 측정 없이 고른 경로 기반 트리거가 빌드를 부수는 딱 그 유형의 변경을 건너뛰는 것으로 드러남 | 골랐다면 High: 최근 40개 커밋을 직접 대조해 검증한 결과, 경로 필터는 그중 19개(48%)를 건너뛰었을 것이고, `main`을 실제로 부순 것으로 알려진 커밋 하나도 포함됨 | 직관이 아니라 측정된 근거로 경로 필터 옵션을 기각해서 회피(3.1절) |
| 모든 PR에서 도는 `cache-to: type=gha,mode=max`가 `main` 자신의 빌드가 의존하는 캐시 레이어를 축출함 | Medium: 공유되고 LRU 방식으로 축출되는 캐시(이 PR 시점 41개 항목에 걸쳐 약 13.3GB)는 어차피 다른 PR에서는 읽을 수도 없는 동시다발적인 PR 범위 쓰기로 흔들릴 수 있음 | `cache-to`를 push 전용으로 만들어서 회피(3.2절) |
| `linux/arm64` 이미지 빌드가 조용히 깨져 있음 | 오늘은 Low(CI는 `linux/amd64`만 빌드함)지만 멀티아치 이미지가 필요해지는 순간 즉시 드러남 | 부수적 발견으로 확인됐고 이 PR에서는 일부러 고치지 않음(8절) |

---

## 2. 기술적 검토 사항

### 2.1 정확성

트리거 게이트를 제거하기로 한 결정은 "Docker 빌드는 항상 느리다"는 가정이 아니라 구체적이고 반증 가능한 측정에 근거한다. `main`에서의 10회 실행은 빌드 컨텍스트가 바뀌었을 때 `docker-check`가 313~354초(평균 326초), 바뀌지 않았을 때 12~20초(평균 15초)만 든다는 걸 보여준다. 그 사이는 없는, 정말로 이봉 분포다. PR 자신의 주석 블록이 이유를 설명한다. `COPY src/`는 소스가 바뀌면 자기 레이어를 무효화하고, 의존성 사전 빌드 스테이지가 없어서(cargo-chef도, 더미 main 트릭도 없음) `cargo build --release`가 소스 편집마다 전체를 다시 돈다. 베이스 이미지와 apt 레이어만 재사용될 뿐이다. 이건 결정에서 중요한데, "캐시가 어차피 충분히 빠르게 만들어줄 것"이라는 이유로 `build-check` 대비 한계 비용을 직접 측정하지 않고 넘어가는 걸 배제해주기 때문이다.

절대 비용이 아니라 한계 비용 숫자가 결정을 좌우한다. `docker-check`와 `build-check`는 둘 다 `needs: test`라서 같은 순간에 시작하고, 둘 다 사실상 같은 작업(크레이트의 릴리스 컴파일)을 한다. 그러니 `docker-check`가 `build-check`보다 평균 2초 늦게 끝난다(표본 10회 실행에서 0~15초 범위)는 건 PR 작성자가 실제로 겪는 벽시계 비용, 즉 필수 검사가 전부 green이 될 때까지의 시간이 바뀌지 않는다는 뜻이다. 이 PR 자신의 CI 실행(31106052626)이 정확히 이 주장에 대한 양성 대조군이다. `Docker Build Check`는 277초 만에 13:35:33에 끝났고, `Build Check`는 386초 만에 13:37:22에 끝났다. 이미지 빌드가 이미 모든 PR을 게이팅하는 잡보다 109초 *먼저* 끝났고, Docker 빌드 자체도 PR이 `main`의 캐시를 읽되 다시 쓰지 않기 때문에 `main` 자신의 313~354초 대역보다 더 빨랐다.

### 2.2 성능 관점

위에서 다뤘다. 이 변경 자체가 별도로 검토할 성능 프로파일이 있는 게 아니라 비용 결정 자체다. 추가로 다룬 비용 차원이 하나 있다. `cache-to`가 조건부가 된다(`${{ github.event_name == 'push' && 'type=gha,mode=max' || '' }}`). 그래서 PR은 `cache-from: type=gha`를 통해 `main`의 따뜻한 캐시로부터 이득을 계속 보면서도 GHA 캐시 소모에 더 이상 기여하지 않는다.

### 2.3 호환성 및 의존성

- **Breaking Changes**: 애플리케이션에는 없다. 이 PR은 CI 트리거 조건, 캐싱 정책만 바꾸고 테스트 커버리지를 추가한다.
- **새로운 의존성**: 없다.
- **호환성**: `docker-check`는 이제 `main`으로의 push에 더해 pull request에서도 돈다. 어디도 좁아지지 않고 커버리지만 엄밀히 넓어진 것이다.

### 2.4 코드 품질

새 테스트 `builder_stage_copy_sources_exist_in_the_context`는 자기 로직만 믿지 않고 짝을 이룬 부정 대조군으로 검증됐다. 빌더 스테이지에 `COPY nonexistent-assets/`를 추가하면 새 테스트가 실행 가능한 메시지("COPY 줄을 지우거나, 이 경로가 옮겨간 곳을 다시 가리키세요")로 실패하고, 동일한 Dockerfile에 대한 실제 `docker buildx build --target builder`도 독립적으로 2초 만에 BuildKit 자신의 `"/nonexistent-assets": not found`로 실패해서, 새 Rust 수준 테스트가 Docker 자신이 보고할 내용과 일치한다는 걸(다만 대략 두 자릿수 배 더 빠르고 더 친절한 메시지로) 확인한다. 기존 임베드-자산 테스트의 대조군(`COPY packaging/` 제거, 원래 PR #322 자신의 검증)은 여전히 유효하다고 가정하지 않고 다시 돌려서 재검증됐고, 이제 실패 출력에서 하나가 아니라 임베드된 자산 둘 다 이름을 밝힌다. 짝을 이루는 단위 테스트 `glob_copy_sources_are_recognised`는 `is_glob` 헬퍼의 동작을 직접 못 박는다(`packaging/*.service`, `src/**`, `file?.txt`, `[abc].txt`는 패턴으로, `src/`, `Cargo.toml`, 리터럴 패키징 경로, `.`은 패턴이 아닌 것으로 인식됨). 이게 중요한 이유는 새 컨텍스트-존재 검사가 BuildKit 자신의 글롭 의미론을 해석하려 시도하는 대신 글롭 소스를 의도적으로 건너뛰기 때문이다. 이 저장소의 Dockerfile이 오늘날 리터럴 `COPY` 소스만 쓴다는 근거로 안전하다고 문서화된 범위 제한이다.

---

## 3. 기술적 선택과 그 이유

### 3.1 pull request에서 전체 이미지 빌드를 돌리고, 측정된 근거로 경로 필터를 기각한다

**컨텍스트**: 이슈는 모든 PR에서 전체 `docker build`를 하는 게 러너 예산에 너무 비싼지 물었고, 그렇다면 아이디어를 버리기 전에 더 싼 변형 셋을 평가하라고 했다. 경로 필터 트리거, 빌더 스테이지 전용 빌드, 컴파일 없는 컨텍스트 조립 검사다.

| 옵션 | 장점 | 단점 |
|---|---|---|
| `Dockerfile`/`.dockerignore`/`Cargo.toml`/`Cargo.lock`/`build.rs`에 대한 경로 필터 | 평가하기 쌈. 대부분 커밋에서 빌드를 아예 건너뜀 | `main`의 최근 40개 커밋에 대고 검증한 결과: 40개 중 19개(48%)를 건너뛰었을 것이고, 기록에 남은 유일한 실제 빌드 손상 커밋인 `74f75d2`도 포함된다. 소스 수준의 `include_str!`을 추가했을 뿐 필터 경로 어디도 건드리지 않았기 때문이다. 이미 대략 2초로 측정된 비용을 아끼려고 유일하게 확인된 실패를 놓치는 필터는 나쁜 거래다 |
| `--target builder`만 | 런타임 스테이지(apt, `useradd`, `COPY --from=builder`)를 건너뜀. 약 5.5분짜리 컴파일 대비 몇 초 | 빌드된 바이너리가 런타임 스테이지가 찾는 곳에 실제로 놓이는지 검증하지 못하게 됨. 전체 빌드가 잡아내고 이 변형은 잡지 못하는 실질적인 결함 유형 |
| 컴파일 없는 컨텍스트 조립 검사 | 가장 저렴한 옵션. `tests/docker_build_context_test.rs`가 임베드 자산에 이미 하던 걸 직접 일반화한 것 | 빌더 스테이지의 누락된 시스템 의존성이나 `cargo`가 이미지 안에서 실제로 돌아야만 나타나는 것은 보지 못함. 대체가 아니라 보완으로 채택됨(이 PR이 정확히 이 검사를 확장한다) |
| **채택: 게이트 없이 모든 PR에서 전체 빌드를 돌림** | 측정된 대략 2초짜리 한계 비용에서(2.1절), 더 싼 변형은 전부 아무것도 아닌 수준으로 줄어든 절감을 위해 실제 커버리지를 희생하는 거래다. 이 이슈를 촉발한 실제 실패 유형을 잡아냄 | 측정된 비용 수준에서는 확인된 단점이 없음. PR은 한계 비용 숫자가 알려지고 나면 다른 모든 대안이 엄밀히 더 나쁘다고 프레이밍함 |

**선택 이유**: 이슈 자신의 인수 기준은 결정 전에 비용을 추정이 아니라 측정할 것을 요구했다. 측정되고 나면 "더 싼" 변형들은 실제 비용이 아니라 실제 커버리지와만 맞바꾸는 거래가 되고, 그게 결정을 확정 짓는다. 경로 필터 옵션이 가장 날카롭게 기각된 이유는 가상의 실패가 아니라 이 이슈가 막으려는 바로 그 실패에 대고 검증됐기 때문이다.

### 3.2 모든 PR에서 내보내는 대신 `cache-to`를 push 전용으로 만든다

**컨텍스트**: `cache-to: type=gha,mode=max`는 이 PR 이전부터 무조건이었다. 그 스텝에 도달하는 모든 실행에서 전체 레이어 캐시를 내보냈고, 이 PR이 트리거 게이트를 제거한 뒤로는 모든 pull request도 여기에 포함된다.

**결정**: `cache-to: ${{ github.event_name == 'push' && 'type=gha,mode=max' || '' }}`. PR은 `cache-from`으로 `main`의 캐시를 읽되 아무것도 다시 쓰지 않는다.

**선택 이유**: GHA 캐시는 공유되고 LRU 방식으로 축출되는 예산(이 PR 시점 41개 항목에 걸쳐 약 13.3GB)이고, 동시에 도는 모든 PR에서의 `mode=max` 내보내기는 그 예산을 흔들고 `main` 자신의 빌드가 의존하는 따뜻한 레이어를 축출할 수 있는데, 그러면서도 얻는 이득은 없다. PR 범위의 캐시 쓰기는 어차피 다른 PR에서는 읽을 수 없기 때문이다. 이건 트리거 게이트를 제거한 것의 직접적인 결과다. 캐싱 정책은 같은 변경 안에서 의도적으로 재검토해야지, 잡을 더 자주 도는 것의 미처 고려하지 못한 부작용으로 남겨두면 안 된다.

### 3.3 `tests/docker_build_context_test.rs`를 반대 방향 검사로 확장하고, 어느 쪽도 중복이라고 취급하지 않고 테스트와 전체 이미지 빌드 둘 다 유지한다

**컨텍스트**: 전체 이미지 빌드가 이제 모든 PR에서 도니, 빠른 Rust 수준 컨텍스트 테스트가 불필요해졌다는 주장도 가능하다. 어차피 느린 빌드가 같은 종류의 실패를 잡아낼 것이기 때문이다.

**결정**: `builder_stage_copy_sources_exist_in_the_context`(모든 빌더 스테이지 `COPY` 소스가 컨텍스트에 존재해야 함)로 테스트 스위트를 확장하고, 테스트를 대체된 것으로 취급하지 않고 테스트와 이미지 빌드 둘 다 유지한다.

**선택 이유**: 두 메커니즘은 PR에 정확히 서술된 것처럼 진짜로 서로 다른 유형을 커버한다. 테스트는 임베드 자산 도달 가능성과 `COPY` 소스 존재를 커버하는데, 둘 다 `cargo`나 `docker`를 아예 호출하지 않고도 확인할 수 있는 저장소의 구조적 속성이다. 이미지 빌드는 둘 다 할 수 없는 것, 즉 빌더 스테이지의 누락된 시스템 의존성이나 `cargo`가 이미지 환경 안에서 실제로 컴파일해야만 드러나는 것을 커버한다. 이미지 빌드가 PR에서 돈다고 테스트가 중복이 되는 건 아니다. 테스트는 `test` 잡에서 돌고, `docker-check`는 `needs: test`이며, 테스트는 밀리초 안에 끝난다. 그러니 테스트가 커버하는 유형에 대해서는 5~6분짜리 이미지 빌드는 시작조차 하지 않고, 실패는 몇 분짜리 컴파일 도중 튀어나오는 생짜 BuildKit 에러가 아니라 문제의 경로와 수정 방법을 직접 이름으로 밝힌다.

---

## 4. 구현 상세

### 4.1 아키텍처 변경

```
[변경 전]
docker-check:
  needs: test
  if: github.event_name == 'push' && github.ref == 'refs/heads/main'
  cache-to: type=gha,mode=max   (무조건)
  -> 병합 후에만 돎. 손상은 red인 기본 브랜치로 나타남

tests/docker_build_context_test.rs:
  embedded_assets_are_inside_the_docker_build_context   (한 방향만)

[변경 후]
docker-check:
  needs: test
  (if: 게이트 없음. pull_request와 push 모두에서 돎)
  cache-to: ${{ push && 'type=gha,mode=max' || '' }}   (push 전용)
  -> 모든 PR에서 build-check와 나란히 돌아 그보다 약 2초 늦게 끝남(측정치)

tests/docker_build_context_test.rs:
  embedded_assets_are_inside_the_docker_build_context        (방향 그대로)
  builder_stage_copy_sources_exist_in_the_context   (신규: 반대 방향)
  glob_copy_sources_are_recognised                  (신규: is_glob의 범위를 못박음)
```

### 4.2 주요 코드 변경

**파일: `.github/workflows/ci.yml`(게이트 제거, 결정 기록)**
```yaml
  # Decision: run the full build on pull requests rather than one of
  # the cheaper variants, because the full build turns out to be
  # nearly free in wall-clock terms. Measured over 10 runs on main:
  #   - docker-check takes 313-354s (mean 326s) when the build context
  #     changed, and 12-20s when it did not...
  #   - docker-check therefore finishes 0-15s after build-check
  #     (mean 2s). It is not on the critical path...
  docker-check:
    name: Docker Build Check
    runs-on: ubuntu-latest
    needs: test
    steps:
      ...
      - uses: docker/build-push-action@...
        with:
          ...
          cache-from: type=gha
          cache-to: ${{ github.event_name == 'push' && 'type=gha,mode=max' || '' }}
```
**변경 이유**: 이 잡이 pull request에서는 절대 돌지 못하게 막던 `if:` 게이트가 제거된다. `cache-to`는 같은 변경 안에서 조건부가 되어, 게이트를 제거하는 것이 모든 PR을 조용히 캐시 쓰기 주체로 만들지 않게 한다.

**파일: `tests/docker_build_context_test.rs`(반대 방향 검사)**
```rust
#[test]
fn builder_stage_copy_sources_exist_in_the_context() {
    ...
    for source in &copies {
        if is_glob(source) { continue; }
        ...
        if !root.join(relative).exists() {
            failures.push(format!(
                "  COPY {source}\n    No such path in the build context.\n    \
                 To fix: drop the COPY line, or repoint it at wherever this path moved to."
            ));
            continue;
        }
        if let Some(pattern) = dockerignore_hit(&ignore_patterns, relative) {
            failures.push(format!(
                "  COPY {source}\n    The path exists, but .dockerignore pattern `{pattern}` \
                 strips it back out of the context.\n    \
                 To fix: narrow that pattern or add a `!` negation for this path."
            ));
        }
    }
    ...
}
```
**변경 이유**: `embedded_assets_are_inside_the_docker_build_context`가 커버하지 않던 방향이다. 무관한 변경이 이름을 바꾸거나 지운 경로를 여전히 가리키는 `COPY`를, 5~6분짜리 이미지 빌드나 최악의 경우 `main`으로의 병합에 이르기 전에 잡아낸다.

**파일: `tests/docker_build_context_test.rs`(모듈 문서, 새 트리거에 맞게 수정)**
```rust
//! Since #328 that gate is gone and `docker-check` builds the image on
//! pull requests too, so a real `docker build` now backs every PR. These
//! tests are still the first line of defence rather than a leftover:
//! they run in the `test` job, `docker-check` is `needs: test`, and they
//! finish in milliseconds.
```
**변경 이유**: 이전 모듈 문서는 `docker-check`가 "main으로의 push에서만 돈다"고 명시적으로 적어놓았는데, 이 PR이 이를 거짓으로 만든다. 더 이상 존재하지 않는 게이트를 서술하는 낡은 문서를 그대로 두면 다음 독자가 테스트가 왜 여전히 중요한지 오해할 수 있다.

### 4.3 데이터 모델 변경

해당 없음. 소스 코드, 와이어 포맷, 지표 정의는 하나도 바뀌지 않았다. 이 PR은 CI 트리거/캐싱 정책과 테스트 커버리지다.

---

## 5. 학습 포인트

### 5.1 비용 기반 CI 결정은 절대 수치가 아니라 한계 수치로 판단해야 한다

**개념**: 검사 하나가 같은 이벤트를 게이팅하는 다른 검사와 이미 병렬로 돌 때(여기서는 `docker-check`와 `build-check` 둘 다 `needs: test` 아래에서 함께 시작함), 검사를 추가하는 게 누군가 체감하는 대기 시간을 바꾸는지 결정하는 숫자는 새 검사가 독립적으로 걸리는 시간이 아니라 기존 검사 대비 얼마나 늦게 끝나느냐다.

**이 PR에서의 적용**: `docker-check`의 절대 비용(콜드 상태로 313~354초)은 단독으로 보면 비싸 보인다. 실제로 결정을 좌우한 건 한계 비용(이미 필수였던 `build-check` 이후 0~15초)이었고, 이 PR은 절대 수치로 추론하는 대신 그 숫자를 직접 측정했다.

### 5.2 경로 필터는 그것이 막으려는 구체적 실패를 포함할 수 있는 만큼만 좋다

**개념**: 경로 기반 트리거 필터는 직관적으로 매력적인 최적화지만, 그 정확성은 주어진 빌드에 "보통" 중요한 파일에 대한 가정이 아니라 실제로 막으려는 역사적 실패에 대고 확인해야 한다.

**이 PR에서의 적용**: 기각된 경로 필터(`Dockerfile`/`.dockerignore`/`Cargo.*`/`build.rs`)는 "Docker 빌드에 영향을 주는 파일"에 대한 그럴듯한 첫 추측이지만, 기록에 남은 유일한 실패인 `74f75d2`는 `include_str!`로 도달 가능한 소스 파일을 추가해서 빌드를 부쉈다. 어떤 그럴듯한 Docker 전용 경로 필터도 포함하지 않았을 변경이다. 필터를 만들어낸 직관을 신뢰하는 대신 실제 사건에 대고 검증한 것이 이를 드러냈다.

### 5.3 게이트를 제거하면 캐싱 정책에 결과가 생길 수 있고, 이는 우연한 부작용이 아니라 명시적으로 다시 검토해야 한다

**개념**: 하나의 트리거 조건(`main` push만) 아래서 짜인 캐싱 설정은 암묵적 가정(한 번에 쓰는 사람 하나, 낮은 경합)을 담을 수 있고, 나중에 트리거 조건(PR도 포함)이 바뀌면 그 가정이 조용히 무효화될 수 있다.

**이 PR에서의 적용**: `cache-to: type=gha,mode=max`는 `main` push만 거기 닿을 수 있을 때는 안전했다. PR도 같은 스텝에 닿을 수 있게 되면, 같은 설정은 동시에 도는 모든 PR이 크기 제한된 공유 예산에 대고 전체 캐시 쓰기를 내보내게 둘 것이다. 이 PR은 이를 나중에 발견할 우연한 결과가 아니라 의도적으로 내려야 할 결정(push 전용 `cache-to`)으로 다룬다.

---

## 6. 추가 학습

### 핵심 용어

| 용어 | 설명 | 관련성 |
|---|---|---|
| `needs: test` | 공유하는 전제 조건이 끝나면 두 잡이 같은 시점에 시작하게 만드는 GitHub Actions 잡 의존성 | `docker-check`와 `build-check`가 함께 시작하는 이유, 그리고 둘의 *상대적* 종료 시간이 의미 있는 비용 지표가 되는 이유 |
| `cache-from`/`cache-to`(GHA 캐시 백엔드) | Docker Buildx의 GitHub Actions 캐시 가져오기/내보내기 지시자 | PR이 촉발하는 캐시 소모를 피하려고 이 PR이 비대칭으로(항상 읽고, push에서만 씀) 만드는 메커니즘 |
| `include_str!` | 컴파일 시점에 파일 내용을 바이너리에 임베드하는 Rust 매크로 | 원래의 `74f75d2` 실패 뒤에 있는 메커니즘: Docker 빌드 컨텍스트가 담지 않은 임베드된 자산 |
| BuildKit 컨텍스트 해석 | `docker build`가 `.dockerignore`를 포함해 조립된 빌드 컨텍스트에 대고 `COPY` 소스를 해석하는 방식 | 새 `builder_stage_copy_sources_exist_in_the_context` 테스트가 Docker를 호출하지 않고 Rust 테스트 수준에서 재현하는 것 |
| 이봉 빌드 비용 | 두 군집이 있고 그 사이는 없는 비용 분포(여기서는 12~20초 대 313~354초) | GHA 레이어 캐싱이 비싼 경로에는 도움이 안 된다는 실증적 신호(`COPY src/`는 소스 변경마다 자기 레이어를 무효화함) |

### 관련 기술/프레임워크

- Docker BuildKit과 그 `--cache-from`/`--cache-to` GitHub Actions 캐시 백엔드, `mode=max`의 전체 레이어 내보내기 동작 포함.
- GitHub Actions 잡 의존성 그래프(`needs:`)와 어떤 잡이 동시에 시작하는지 순차로 시작하는지에 미치는 영향.
- Rust의 벤더드 OpenSSL 빌드 경로와 perl `FindBin` 모듈에 대한 의존성. 부수적인 `linux/arm64` 발견(8절) 뒤에 있는 메커니즘.

### 관련 PR/이슈

- 이슈 #328: 이 PR이 닫는 이슈.
- PR #319(이슈 #309): 병합 커밋(`74f75d2`)이 실제로 `main`의 Docker 이미지 빌드를 부순 PR. 이 이슈를 촉발한 사건.
- PR #322: 즉각적인 `74f75d2` 손상을 고치고 `tests/docker_build_context_test.rs`의 원래 임베드-자산 검사를 추가했다. 이 PR이 반대 방향 검사로 이를 확장한다.
- PR #337: 같은 병합 시퀀스에서 이 PR 다음에 착지한다. 내용상 무관하지만 그 `ci.yml` 편집은 `docker-check`가 아니라 launchd 잡에 있어서 두 PR의 diff가 충돌하지 않는다.

---

## 7. 변경 요약

### 통계

| 항목 | 값 |
|---|---|
| 변경 파일 | 2 |
| 추가 줄 | +169 |
| 삭제 줄 | -11 |
| 커밋 | 1 |

### 카테고리별 변경

| 분류 | 내용 |
|---|---|
| CI 커버리지 | `docker-check`의 `if:` 게이트 제거. 잡이 이제 `main`으로의 push뿐 아니라 pull request에서도 돎 |
| CI 비용 정책 | `cache-to`가 조건부(push 전용)가 되어, pull request는 자기 것을 내보내지 않고 `main`의 캐시를 읽음 |
| 테스트 | 새 `builder_stage_copy_sources_exist_in_the_context`(반대 방향 컨텍스트 검사)와 `glob_copy_sources_are_recognised`(글롭 감지 헬퍼의 범위를 못박음) |
| 문서 | `.github/workflows/ci.yml` 주석에 측정된 비용과 기각된 대안 전부를 기록. `tests/docker_build_context_test.rs`의 모듈 문서와 실패 메시지를 새 트리거에 맞게 수정 |

### 관련 커밋

| SHA | 유형 | 메시지 |
|---|---|---|
| `d84a213d` | ci | build the Docker image on pull requests |

`main`에 `89c621f7`로 병합됨. #328을 닫는다.

---

## 8. 후속 조치

### 필수

블로킹으로 확인된 건 없다.

### 모니터링 필요

- 이 PR 이후에도 여전히 커버되지 않는 것: 옮겨지거나 재태깅된 베이스 이미지는 PR 단위가 아니라 자기 일정대로 깨진다. 그러니 열려 있는 PR이 아니라 다음에 어쩌다 도는 빌드에서 나타날 것이다. PR은 이게 문제가 되면 예약된 빌드가 올바른 도구일 거라고 짚지만, 구현하지는 않는다.

### 향후 개선 사항

- **`linux/arm64` 빌드가 이 이슈와 무관하게 깨져 있음**. 여기서 고치지 않고 이슈 #328의 댓글에 기록했다. `Cargo.toml`은 `cfg(all(target_arch = "aarch64", target_env = "gnu"))`에 대해 벤더드 OpenSSL을 활성화하는데, 소스에서 OpenSSL을 빌드하려면 perl의 `FindBin`이 필요하고 `rust:1.96-slim`에는 그게 없다(`perl-base`만 있지 전체 `perl`은 없다). CI는 오늘 `linux/amd64`만 빌드하고 이 경로는 거기서는 비활성이라 실제로는 아무것도 깨지지 않는다. 이슈 댓글은 "멀티아치 이미지가 언젠가 필요해지면 별도 이슈로 만들 가치가 있다"고 적어놓았다. 이 보고서는 이 발견이 PR 본문이나 diff 자체 어디에도 서술되어 있지 않다는 걸 확인했다. 연결된 이슈의 댓글 스레드에만 기록되어 있고, 이 보고서는 그걸 직접 교차 확인했다.

---

## 부록

### A. 테스트 결과

같은 입력에 대한 실제 `docker build`와 짝지은 부정 대조군:

- **새 검사**: 빌더 스테이지에 `COPY nonexistent-assets/`를 추가하면 `builder_stage_copy_sources_exist_in_the_context`가 실행 가능한 메시지로 실패한다. 동일한 Dockerfile에 대한 실제 `docker buildx build --target builder`도 독립적으로 2초 만에 `failed to compute cache key: ... "/nonexistent-assets": not found`로 실패한다.
- **기존 검사, 재검증됨**: `COPY packaging/`를 제거하면 `embedded_assets_are_inside_the_docker_build_context`가 실패하는데, 이제 임베드된 자산 둘 다 이름을 밝힌다. 같은 `COPY` 집합에 대한 프로브 빌드가 그 자산이 조립된 컨텍스트에서 정말로 빠져 있다는 걸 확인했다. 수정하지 않은 집합에는 있었다.
- `cargo test --test docker_build_context_test`: 7개 통과.
- `cargo check --lib --tests`: 클린.
- `cargo clippy --lib --tests -- -D warnings`와 `cargo clippy --bin all-smi -- -D warnings`: 둘 다 클린.
- `cargo fmt --check`: 클린.
- `actionlint .github/workflows/ci.yml`: `main`에 이미 있던 5건(SC2015 3건, SC2251 1건, 알 수 없는 셀프 호스팅 러너 라벨 1건) 외에 새 발견 없음. `docker-check` 안에는 없음.
- 이 PR 자신의 CI 실행(31106052626)이 트리거 변경 자체에 대한 양성 대조군이다. `docker-check`가 pull request에 나타나 이미지를 빌드했다. `Docker Build Check`는 277초 만에 13:35:33에 끝났고, `Build Check`는 386초 만에 13:37:22에 끝나서, `docker-check`가 109초 유리한 격차를 냈다.

### B. 성능 벤치마크

이 PR의 핵심 정량 결과. `main`의 10회 실행에 대해 `gh api repos/lablup/all-smi/actions/runs/<id>/jobs`로 뽑음:

| | 시간 |
|---|---|
| `docker-check`, 빌드 컨텍스트 변경됨 | 313~354초(평균 326초), n=7 |
| `docker-check`, 컨텍스트 변경 안 됨(문서/워크플로 전용 커밋) | 12~20초(평균 15초), n=3 |
| `build-check`, 이미 모든 PR에서 돎 | 319~378초(평균 339초) |
| `docker-check`가 `build-check` 이후 끝남 | 0~15초, 평균 2초 |

### C. 참고 자료

- 이슈 #328: 근본 원인 서술(`74f75d2` 사건), 범위, 인수 기준. diff와 교차 확인함.
- 이슈 #328의 댓글 스레드: 전체 측정 비용 표, 대안별 기각 근거, 부수적인 `linux/arm64`/`FindBin` 발견. 이 중 어느 것도 PR 본문이나 diff 자체에는 나타나지 않는다.
- PR #322: `74f75d2`에 대한 이전 수정과, 이 PR이 확장하는 원래 `tests/docker_build_context_test.rs`.
