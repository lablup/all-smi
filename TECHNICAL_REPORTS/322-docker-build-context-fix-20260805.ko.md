# 기술 보고서: PR #322 - fix(ci): include packaging assets in the Docker build context

**작성일**: 2026-08-05
**상태**: 완료
**언어**: Dockerfile, Rust (테스트 하네스)
**위험도**: Low(CI 전용 수정과 새 계약 테스트. 애플리케이션 코드 경로 변경 없음)

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

PR #319(이슈 #309)가 병합된 직후 `main`이 깨졌다. Docker 이미지 빌드가 `error: couldn't read src/service_cmd/../../packaging/systemd/all-smi.service`로 컴파일에 실패했다. PR #319는 이 저장소 역사상 처음으로 `include_str!`로 임베드하는 파일을 도입했는데, `src/` 아래에서 `include_str!`/`include_bytes!`로 닿을 수 있는 경로 집합과 Dockerfile의 빌더 스테이지가 실제로 빌드 컨텍스트에 복사하는 경로 집합이 일치하는지 아무것도 강제하지 않았다. Dockerfile은 `Cargo.toml`, `Cargo.lock`, `build.rs`, `proto/`, `src/`만 복사하는데(전체 체크아웃보다 좁음) `packaging/` 항목이 없었다. 그래서 같은 PR에서 다른 모든 CI 게이트(`cargo test`, `cargo clippy`, `cargo build`)가 통과했음에도 이미지 안에서는 크레이트가 컴파일되지 않았다.

이것이 병합 전에 보이지 않았던 이유는 단순한 불운이 아니라 구조적이다. `.github/workflows/ci.yml`의 `docker-check`는 `github.event_name == 'push' && github.ref == 'refs/heads/main'`로 게이트되어 있어서, 애초에 풀 리퀘스트에서는 절대 실행되지 않는다. 다른 모든 검사는 전체 체크아웃에서 빌드하므로 구조상 좁혀진 빌드 컨텍스트 문제를 볼 수 없다. 수정은 두 부분이다. Dockerfile의 빌더 스테이지에 `COPY packaging/ ./packaging/`를 추가하는 것, 그리고 `tests/docker_build_context_test.rs`를 추가하는 것이다. 이 계약 테스트는 `src/` 아래의 모든 `include_str!`/`include_bytes!` 리터럴을 뽑아 그것을 임베드하는 파일에 대고 풀어내고, 그 결과가 빌더 스테이지 `COPY`로 덮이면서 `.dockerignore`로 다시 빠지지는 않는지 확인한다. 이렇게 하면 이 결함 부류가 `main`이 아니라 풀 리퀘스트로 옮겨가고, 빠진 `COPY` 줄 하나를 잡는 비용이 그린 기본 브랜치를 잃는 대신 `cargo test` 몇 밀리초로 줄어든다. 테스트 자체의 정확성은 독립적인 부정 대조 두 개로 확인했다. Dockerfile의 `COPY` 줄을 되돌리면 새 테스트가 정확한 수정법을 짚어주는 메시지와 함께 실패하고, 이 Dockerfile의 정확한 `COPY` 집합에 목표한 `RUN test -f` 어서션을 더한 처음부터의 Docker 빌드가 수정 없이는 실패하고 수정과 함께는 성공한다. 전체 규모는 파일 2개, +340/-0, 커밋 1개이며 연결된 이슈는 없다.

---

## 1. 문제 정의

### 1.1 배경

Dockerfile은 `all-smi`를 두 스테이지로 빌드한다. 의도적으로 좁혀진 소스 경로 집합(빌드 컨텍스트와 캐시를 작게 유지하려고 전체 체크아웃이 아닌)을 복사해 릴리스 바이너리를 컴파일하는 빌더 스테이지, 그리고 `COPY --from=builder`로 빌드된 바이너리만 빌더에서 뽑아오는 런타임 스테이지다. 이 좁힘은 늘 암묵적이었다. 크레이트의 컴파일된 결과물이 `src/`, `proto/`, 매니페스트 파일 두 개 아래에서 `cargo build`가 봐야 할 것 이상으로 실제로 무엇에 의존하는지, 저장소 안 어디에도 선언되거나 확인된 적이 없었다.

PR #319(이슈 #309)는 `src/service_cmd/template.rs`에서 `include_str!("../../packaging/systemd/all-smi.service")`로 임베드되는 `packaging/systemd/all-smi.service`를 추가했다. `src/` 바깥에서 컴파일된 바이너리로 임베드된, 이 저장소 역사상 첫 에셋이다. Dockerfile의 빌더 스테이지에는 `packaging/`에 대한 `COPY` 줄이 없었으므로 임베드된 경로가 빌드 컨텍스트 안에서 닿을 수 없었고, `rustc`는 `include_str!` 매크로 전개 시점에 평범한 파일 없음 오류로 실패했다.

### 1.2 기존 문제점

- **문제 1(실제 깨짐)**: PR #319가 병합된 직후 `docker build`가 빌더 스테이지 안에서 `error: couldn't read src/service_cmd/../../packaging/systemd/all-smi.service`로 실패했다. [실행 30997457472](https://github.com/lablup/all-smi/actions/runs/30997457472)에서 확인됨.
- **문제 2(병합 전에 아무것도 잡지 못한 이유)**: `ci.yml`의 `docker-check`는 `github.event_name == 'push' && github.ref == 'refs/heads/main'`로 게이트되어 있어, 구조적으로 풀 리퀘스트에서 실행될 수 없다. PR #319는 실제로 실행된 모든 검사에서 완전히 그린이었다. `cargo test`, `cargo clippy`, `cargo build` 모두 전체 체크아웃에서 빌드하며 좁혀진 Docker 빌드 컨텍스트라는 개념 자체가 없기 때문이다.
- **문제 3(독립적으로 유지되는 두 목록 사이에 강제되는 계약이 없음)**: `src/`의 `include_str!`/`include_bytes!`로 닿을 수 있는 경로 집합과 Dockerfile 빌더 스테이지가 `COPY`하는 경로 집합은 반드시 일치해야 하는 저장소에 대한 두 사실인데, 아무것도 그것을 확인하지 않았고, 미래에 새로 임베드되는 에셋이 같은 공백에 빠지는 것도 아무것도 잡아주지 않았을 것이다.

### 1.3 위험성

| 위험 | 영향 | 발생 가능성 |
|---|---|---|
| 미래의 PR이 Dockerfile의 `COPY` 집합 바깥 경로에서 새 에셋을 임베드해 정확히 같은 깨짐을 반복함 | Medium: `main`을 다시 깨뜨림. 이 사고와 정확히 같은 방식으로 병합 후에야 발견됨 | 모든 PR의 일반 `cargo test` 스위트에서 실행되는 새 계약 테스트로 닫힘 |
| `COPY` 줄이 명목상 덮는데도 `.dockerignore`가 에셋을 다시 빼냄 | Low(오늘 이 저장소에서 관측되지는 않음) | 새 테스트의 `dockerignore_hit` 확인이 이 경우를 명시적으로 다루지만, 여기서는 잘못된 것을 찾지 못함 |
| `docker-check`가 계속 풀 리퀘스트에서 실행되지 않아, 이 새 테스트가 볼 수 없는 결함 부류(예: 빌더 스테이지에 빠진 시스템 의존성)를 놓침 | Medium: 이 수정은 사각지대를 임베디드 에셋 도달 가능성으로 구체적으로 좁힐 뿐, 다른 빌드를 깨뜨리는 변경은 여전히 `main`에서만 드러날 수 있음 | 이 PR의 범위 밖으로 명시적으로 남김. 별도로 등록될 CI 비용 결정으로 기록함 |

---

## 2. 기술적 검토 사항

### 2.1 수정 자체의 정확성

Dockerfile 변경은 추가된 줄 하나, `COPY packaging/ ./packaging/`이며, 기존 `COPY src/ ./src/` 옆 빌더 스테이지에 놓인다. PR #319가 도입한 파일 하나만이 아니라 `packaging/` 디렉터리 전체를 복사하므로, PR #321이 같은 디렉터리 트리 아래 추가한 launchd plist도 그 PR을 위한 두 번째 Dockerfile 변경 없이 함께 덮는다.

### 2.2 검사만으로가 아니라 부정 대조로 확립한 계약 테스트 자체의 정확성

항상 통과하기만 하는 테스트는 자신이 막겠다고 주장하는 결함을 실제로 잡을 수 있는지에 대해 아무것도 증명하지 못한다. 정확히 이를 배제하려고 독립적인 대조 두 개를 실행했다.

- **`COPY packaging/` 줄을 되돌리고** `cargo test --test docker_build_context_test`를 다시 실행하면 `embedded_assets_are_inside_the_docker_build_context`가 실패하는데, 메시지가 정확히 빠진 에셋과 그것을 임베드하는 파일, 추가해야 할 정확한 `COPY` 줄을 짚어준다. 이는 이 테스트가 지금 수정이 적용된 상태에서 통과한다는 것만이 아니라, 원래 사고 자체를 잡았을 것임을 확인해 준다.
- **처음부터의 Docker 빌드**를 이 Dockerfile의 정확한 빌더 스테이지 `COPY` 집합에, 같은 빌드 안에 목표한 `RUN test -f packaging/systemd/all-smi.service` 어서션을 더해 실행하면, 수정 없이는 그 `RUN` 단계에서 `exit code: 1`로 실패하고 수정과 함께는 성공하며, 결과 이미지 안에 유닛 템플릿과 환경 파일 예시가 둘 다 존재함을 확인했다. 이는 테스트 자신의 주장(이름 붙인 `COPY` 줄이 에셋을 도달 가능하게 만든다)을 테스트가 수행하는 Rust 수준 정적 분석 로직만이 아니라 실제 도구(`docker build`)로 확인한 것이다.

### 2.3 호환성 및 의존성

- **Breaking Changes**: 없다. Dockerfile 변경은 빌더 스테이지에 `COPY` 줄 하나를 추가할 뿐이다. 런타임 스테이지와 결과 이미지의 내용은(빌드를 아예 성공시키는 것 말고는) 영향받지 않는다.
- **새로운 의존성**: 없다. 테스트는 표준 라이브러리의 `std::fs`, `std::path`, `std::collections::BTreeSet`만 쓴다.
- **호환성**: 새 테스트는 `tests/` 아래에 있고 일반 `cargo test` 스위트의 일부로 실행되며, Docker 데몬도 네트워크 접근도 특별한 CI 설정도 필요 없다. 전적으로 체크아웃된 소스 트리와 Dockerfile/`.dockerignore` 텍스트에 대해서만 동작한다.

### 2.4 코드 품질

`tests/docker_build_context_test.rs`의 테스트 다섯 개: 주 계약 확인(`embedded_assets_are_inside_the_docker_build_context`)에 더해, 그것이 의존하는 파싱 헬퍼를 위한 단위 테스트 네 개(`copy_coverage_matches_directories_and_exact_files`, `dockerignore_patterns_are_recognised`, `embedded_path_extraction_finds_literals`, `builder_stage_copies_are_isolated_from_the_runtime_stage`). 마지막 것은 이후 스테이지의 `COPY --from=builder ...` 줄이 컨텍스트로 복사된 소스 집합에서 올바로 제외되는지를 구체적으로 확인한다. 그 줄은 빌드 컨텍스트가 아니라 이전 스테이지의 결과물에서 끌어오기 때문이다.

새 파일에 대해 `cargo fmt --check`와 `cargo clippy --all-targets --all-features -- -D warnings` 둘 다 클린으로 보고됐다.

---

## 3. 기술적 선택과 그 이유

### 3.1 Dockerfile만 고치는 대신, `include_str!`/`include_bytes!` 리터럴에 대한 정적 분석 계약 테스트

**컨텍스트**: 당장의 수정(`COPY` 줄 하나 추가)은 이 특정 사고는 해결하지만, 다음에 현재 `COPY` 집합 바깥 경로에서 에셋이 임베드될 때 같은 부류의 깨짐을 막지는 못한다.

| 선택지 | 장점 | 단점 |
|---|---|---|
| Dockerfile만 고침 | 최소 변경, 보고된 깨짐을 즉시 해결 | 근본 공백(반드시 일치해야 하는, 독립적으로 유지되는 두 목록)이 전혀 강제되지 않은 채 남음. 다음 새 임베디드 에셋에서 정확히 같은 사고가 재발함 |
| **채택: Dockerfile을 고치고, 모든 임베디드 에셋 리터럴을 뽑아 Dockerfile의 실제 `COPY` 집합에 대고 확인하는 계약 테스트를 추가함** | 이 결함 부류를 병합 후 `main`이 아니라 풀 리퀘스트(`cargo test`에서 몇 밀리초)로 옮김. Dockerfile과 소스 트리를 직접 읽으므로 확인할 고정된 에셋 목록을 인코딩하는 대신 자체 갱신됨 | `include_str!`/`include_bytes!` 리터럴 파서가 의도적으로 단순함(순수 문자열 리터럴만). `concat!`으로 만든 경로는 잡지 못함. 조용한 공백이 아니라 문서화되어 받아들인 제약임 |
| 대신 풀 리퀘스트에서 `docker-check`를 켬 | 이 특정 사고도 잡았을 것이고, 이 테스트가 잡을 수 없는 다른 Docker 빌드 깨짐 부류(예: 빠진 시스템 패키지)도 잡음 | 이 특정 결함 부류와는 별개인 CI 비용 결정(모든 PR에서의 빌드 시간, 자원 사용). 이 PR에 끼워 넣는 대신 범위 밖으로 명시하고 별도로 등록함 |

**선택 이유**: 오늘 알려진 에셋 하나를 하드코딩하는 대신 실제 Dockerfile과 실제 소스 트리를 읽는 테스트가, 이 보증이 이 특정 사고를 넘어 지속되게 만드는 것이다. 모든 풀 리퀘스트에서 전체 Docker 빌드를 켜는 것은 엄밀히 더 강한 보증이겠지만, 이 PR이 보고자 자신의 판단으로 의도적으로 하지 않는, 실질적으로 다르고 더 비싼 변경이다. 이 수정과 동등하다고 가정하는 대신 유지관리자를 위한 후속 결정으로 명시적으로 짚어둔다.

### 3.2 `include_str!` 경로를 `canonicalize`가 아니라 어휘적으로 해석

**컨텍스트**: 테스트는 상대 경로 리터럴(예: `"../../packaging/systemd/all-smi.service"`)을 그것을 담은 소스 파일에 대고 풀어서, Dockerfile의 `COPY` 집합과 대조할 저장소 상대 경로를 얻어야 한다.

**채택한 접근**: `Path::canonicalize`를 호출하는 대신, `..`/`.` 경로 요소를 누적된 경로에 손으로 적용하는 작은 어휘적 `normalize()` 함수.

**선택 이유**: `canonicalize`는 대상이 디스크에 실제로 존재해야 하므로, 이 테스트가 명확히 보고하도록 설계된 바로 그 실패 사례, 즉 참조는 되지만 실제로는 닿을 수 없는 에셋에서는 쓸 수 없다. 어휘적 해석은 대상이 존재하든 말든 동작하며, 이것이 테스트 자신의 실패 메시지가 문제를 서술하기도 전에 관련 없는 I/O 오류로 실패하는 대신 특정한 빠지거나 닿을 수 없는 경로를 짚어줄 수 있게 하는 이유다.

---

## 4. 구현 상세

### 4.1 아키텍처 변경

```
[변경 전]
Dockerfile 빌더 스테이지 COPY 집합: Cargo.toml, Cargo.lock, build.rs, proto/, src/
src/service_cmd/template.rs:  include_str!("../../packaging/systemd/all-smi.service")
                                                    |
                                                    v
                              Docker 빌드 컨텍스트 안에서 닿을 수 없음
                              -> `main`에서 `docker build`가 컴파일 시점에 실패

[변경 후]
Dockerfile 빌더 스테이지 COPY 집합: Cargo.toml, Cargo.lock, build.rs, proto/, src/, packaging/
                                                    |
                                                    v
                              tests/docker_build_context_test.rs가 모든 `cargo test`마다 확인:
                                src/ 아래 모든 include_str!/include_bytes! 리터럴에 대해:
                                  풀어낸 경로가 디스크에 존재함
                                  풀어낸 경로가 빌더 스테이지 COPY로 덮임
                                  풀어낸 경로가 .dockerignore로 다시 빠지지 않음
```

### 4.2 주요 코드 변경

**파일: `Dockerfile`**
```dockerfile
# Copy packaging assets embedded into the binary with include_str!
# (service unit templates). Keep this in sync with the paths asserted by
# tests/docker_build_context_test.rs.
COPY packaging/ ./packaging/
```
**변경 이유**: 보고된 깨짐에 대한 수정이다. 주석이 구체적으로 새 테스트를 가리켜서, 이 두 파일 중 하나를 나중에 편집하는 사람이 다른 하나의 존재를 알고 서로 일치시켜야 함을 알게 한다.

**파일: `tests/docker_build_context_test.rs` (계약 확인)**
```rust
if !copies.iter().any(|c| copy_covers(c, asset_path)) {
    failures.push(format!(
        "  {asset}\n    embedded by: {owner}\n    To fix: add `COPY {dir}/ ./{dir}/` to the builder stage of the Dockerfile."
    ));
}
```
**변경 이유**: 실패 메시지가 문제가 존재한다는 것만이 아니라 정확한 수정법(추가할 특정 `COPY` 줄)을 짚어준다. 이것이 이 테스트를 자기 출력만으로 실행 가능하게 만드는 부분이다. 독자가 PR #322 자신이 빌드 로그에서 재도출해야 했던 수정법을 다시 도출할 필요가 없다.

### 4.3 데이터 모델 변경

해당 없음. 이 PR은 CI/빌드 인프라만 바꾸고, 와이어 포맷이나 Prometheus 지표는 바꾸지 않는다.

---

## 5. 학습 포인트

### 5.1 전체 체크아웃보다 좁은 빌드 컨텍스트는 소스 트리와의 암묵적이고 확인되지 않는 계약이다

**개념**: 저장소의 일부(전체 트리가 아니라)를 빌드 컨텍스트에 복사하는 어떤 Docker 빌드든 암묵적인 주장을 하고 있는 셈이다. "컴파일된 결과물은 이 부분집합 바깥의 어떤 것에도 의존하지 않는다." 그 주장은 소스 트리가 진화함에 따라 무언가 자동으로 확인해주지 않는 한, 누군가 마지막으로 손으로 검증한 시점만큼만 유효하다.

**이 PR에서의 적용**: `include_str!`/`include_bytes!`가 정확히 그 주장을 조용히 무효화할 수 있는 메커니즘이다. `cargo build`의 평범한 `src/`-플러스-매니페스트 시야가 달리 기대하지 않는 파일 경로에 대한 컴파일 시점 의존성을 추가하기 때문이다. 이 수정은 유지되는 목록이 아니라 소스 트리 안의 실제 매크로 호출에서 확인을 도출함으로써 알려진 에셋 하나를 넘어 일반화한다.

### 5.2 기본 브랜치의 `push`에 한정된 CI 게이트는 구조상 풀 리퀘스트가 만드는 회귀를 병합 전에 잡을 수 없다

**개념**: `if: github.event_name == 'push' && github.ref == 'refs/heads/main'`는 팀이 모든 PR에서 돌리고 싶지 않은 비싼 검사(여기서는 전체 Docker 이미지 빌드)에 흔히 쓰이는 패턴이다. 이 트레이드오프는 우연이 아니라 구조적이다. 그 게이트만 잡을 수 있는 결함 부류는 같은 조건에 의해 병합 이후에만 발견될 수 있는 결함 부류이기도 하다.

**이 PR에서의 적용**: 정확히 이런 일이 벌어졌다. PR #319에서 실제로 실행된 모든 게이트(테스트 스위트, clippy, `cargo build`)는 좁혀진 컨텍스트에서 빌드하지 않으므로 Docker 빌드 컨텍스트 문제를 볼 방법이 없었다. 이 수정의 계약 테스트는 의도적으로 Docker를 전혀 요구하지 않는데, 구체적으로 모든 풀 리퀘스트에서 실제로 실행되는 일반 `cargo test` 스위트 안에서 돌 수 있도록, 게이트의 범위 자체를 바꾸지 않고도 이 특정 결함 부류를 `push` 전용 게이트 뒤에서 빼내려는 것이다.

### 5.3 테스트 자체의 가치는 현재 통과한다는 증거만이 아니라 실패할 수 있다는 증거에 달려 있다

**개념**: 이미 고쳐진 코드베이스에서만 돌려본 새로 작성된 검사는 올바른 입력을 받아들인다는 것을 보여줄 뿐, 그것이 대응해서 작성된 깨진 입력을 실제로 거부했을지에 대해서는 아무것도 말해주지 않는다.

**이 PR에서의 적용**: 두 부정 대조(Dockerfile `COPY` 줄 되돌리기, 그 줄이 있고 없을 때의 처음부터의 Docker 빌드) 모두 정확히 그 공백을 닫으려고 존재한다. 이 테스트가 목표로 삼은 정확한 회귀에서 유익하게 실패하는지, 그리고 기저 주장(추가된 `COPY` 줄이 에셋을 도달 가능하게 만든다)이 테스트 자신의 경로 해석 로직만이 아니라 실제 도구에 대해서도 성립하는지 둘 다 확인해 준다.

---

## 6. 추가 학습

### 핵심 용어

| 용어 | 설명 | 관련성 |
|---|---|---|
| Docker 빌드 컨텍스트 | 빌드를 위해 Docker 데몬에 보내지는 파일 집합. `COPY`/`ADD` 명령이 닿을 수 있는 것과 `.dockerignore`가 제외하는 것으로 결정됨 | 이 PR의 테스트가 임베디드 에셋 도달 가능성을 대조하는 대상 |
| `include_str!` / `include_bytes!` | 컴파일 시점에 파일 내용을 컴파일된 바이너리에 임베드하는 Rust 매크로. 매크로를 담은 소스 파일 기준 상대 경로로 해석됨 | PR #319가 자기도 모르게 만들고 있던 Docker 빌드 컨텍스트 의존성을 도입한 메커니즘 |
| 멀티 스테이지 Docker 빌드 | `FROM`이 둘 이상인 Dockerfile. 이후 스테이지가 이전 스테이지의 결과물을 선택적으로 `COPY --from=`할 수 있음 | 테스트가 `COPY --from=` 줄을 컨텍스트로 복사된 소스 집합에서 명시적으로 제외하는 이유(2.4절) |
| `.dockerignore` | `COPY` 명령이 무엇을 지목하든 상관없이 Docker 빌드 컨텍스트에서 제외할 패턴을 나열하는 파일 | `COPY` 존재 여부와 나란히 테스트가 확인하는 커버리지의 나머지 절반 |
| 부정 대조(테스트 검증의 의미에서) | 테스트가 올바른 코드에서 통과한다는 것만 확인하는 대신, 알려진 결함을 고의로 되살려 테스트가 이를 잡는지 확인하는 것 | 이 PR 자신의 새 테스트를 검증하는 데 쓴 방법론(2.2절) |

### 관련 기술/프레임워크

- Docker 멀티 스테이지 빌드와 `COPY`/`ADD`/`.dockerignore` 빌드 컨텍스트 모델.
- Rust의 `include_str!`/`include_bytes!` 컴파일 시점 파일 임베딩.
- 잡을 특정 이벤트 유형과 ref로 범위 짓는 GitHub Actions `if:` 조건.

### 관련 PR/이슈

- PR #319(이슈 #309): `packaging/systemd/all-smi.service`와 그 `include_str!` 임베딩을 도입해 `main`의 Docker 빌드를 깨뜨린 변경.
- PR #321(이슈 #310): 이 PR의 `COPY packaging/` 줄이 이미 덮는 같은 `packaging/` 트리 아래에 `packaging/launchd/com.lablup.all-smi.plist`를 추가함. 추가 Dockerfile 변경이 필요 없었음.
- 연결된 GitHub 이슈 없음. 이 PR은 `main`의 깨짐에 대고 직접 등록됐다.

---

## 7. 변경 요약

### 통계

| 항목 | 값 |
|---|---|
| 변경 파일 | 2 (`Dockerfile`, `tests/docker_build_context_test.rs`) |
| 추가 줄 | +340 |
| 삭제 줄 | 0 |
| 커밋 | 1 |
| 새 테스트 | 5 |

### 카테고리별 변경

| 분류 | 내용 |
|---|---|
| CI / 빌드 수정 | Dockerfile 빌더 스테이지에 `COPY packaging/ ./packaging/` 추가 |
| 회귀 방지 | 모든 풀 리퀘스트의 일반 `cargo test` 스위트에서 실행되는 새 `tests/docker_build_context_test.rs` 계약 테스트 |

### 관련 커밋

| SHA | 유형 | 메시지 |
|---|---|---|
| `9989a860` | fix(ci) | include packaging assets in the Docker build context |

`main`에 `acb3c946`로 병합됨. 연결된 이슈 없음.

---

## 8. 후속 조치

### 필수

없음. 당장의 깨짐은 고쳤고 Rust 수준 테스트와 실제 `docker build` 둘 다에 대고 검증했다.

### 모니터링 필요

- 미래에 새로 임베드되는 에셋(`include_str!`/`include_bytes!`를 통한)이 테스트의 단순한 문자열 리터럴 파서가 볼 수 없는 위치(예: `concat!`으로 만든 경로)에 `src/` 아래 추가되는지. 이는 조용한 공백이 아니라 파서의 문서화되어 받아들인 제약이다.

### 향후 개선 사항

- **풀 리퀘스트에서 `docker-check` 실행하기.** 이 PR의 범위에서 명시적으로 제외했고 CI 비용 결정으로 별도 등록함. 이 PR의 정적 분석 테스트가 설계상 볼 수 없는 더 넓은 부류의 Docker 빌드 깨짐(예: 빌더 스테이지의 빠진 시스템 의존성)을 잡을 것이다.

---

## 부록

### A. 테스트 결과

- `cargo test --test docker_build_context_test`: 5개 통과.
- 부정 대조 1(Dockerfile): `COPY packaging/` 줄을 제거하면 `embedded_assets_are_inside_the_docker_build_context`가 2.4절에 설명한 정확한 수정 메시지와 함께 실패함.
- 부정 대조 2(실제 Docker 빌드): 이 PR의 정확한 빌더 스테이지 `COPY` 집합에 `RUN test -f packaging/systemd/all-smi.service`를 더해 빌드하면 수정과 함께는 성공하고 수정 없이는 그 `RUN` 줄에서 `exit code: 1`로 실패함. 결과 이미지 안에 유닛 템플릿과 환경 파일 예시가 둘 다 있음을 확인함.
- `cargo fmt --check`: 클린.
- `cargo clippy --all-targets --all-features -- -D warnings`: 클린.

### B. 성능 벤치마크

해당 없음. 이 PR은 빌드 설정과 정적 분석 테스트 변경이며 영향받는 런타임 데이터 경로가 없다.

### C. 참고 자료

- Docker 문서: 멀티 스테이지 빌드, `COPY --from=`, `.dockerignore` 시맨틱스.
- Rust 레퍼런스: `include_str!`와 `include_bytes!` 매크로 경로 해석(호출하는 소스 파일 기준 상대).
- 실패한 실행: [github.com/lablup/all-smi/actions/runs/30997457472](https://github.com/lablup/all-smi/actions/runs/30997457472).
