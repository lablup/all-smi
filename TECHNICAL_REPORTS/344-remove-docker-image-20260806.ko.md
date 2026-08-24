# 기술 보고서: PR #344 - Docker 이미지와 CI 잡 제거

**일자**: 2026-08-06  
**상태**: 완료  
**관련 항목**: PR #344, 이슈 #342  
**위험 수준**: 중간 (문서화된 배포 경로 제거, 런타임 코드 변경 없음)

---

## 요약

PR #344는 Docker 이미지와 그것을 빌드하기 위해 존재하던 모든 것을 삭제했습니다. `Dockerfile`, compose 예제, `docker-check` CI 잡, `docker-build-container` Makefile 타겟, 그리고 하루 전에 추가된 빌드 컨텍스트 테스트가 대상입니다. 이제 컨테이너 배포는 all-smi를 실행하는 지원 경로가 아닙니다.

이 이미지는 어떤 아키텍처에서도 동작한 적이 없었고, 저장소나 릴리스 프로세스 어디에서도 소비되지 않았습니다. 메인테이너의 결정은 수리가 아니라 제거였으므로, 이 PR은 #342에 기록된 glibc 불일치나 누락된 런타임 라이브러리를 고치려 시도하지 않습니다. all-smi가 자신이 컨테이너 **안에서** 돌고 있음을 감지하는 `src/` 내부의 컨테이너 인식 코드는 별개의 제품 기능이며 그대로 두었습니다.

---

## 1. 문제 정의

이슈 #342는 이미지가 모든 아키텍처에서 실행에 실패한다고 기록했습니다. 수리 비용은 실재했고(glibc 불일치에 더해 누락된 런타임 라이브러리), 그 비용을 지불해서 얻을 것은 없었습니다. 어떤 워크플로도 이미지를 게시하지 않았고, 어떤 릴리스 아티팩트도 이를 참조하지 않았으며, 문서화된 설치 경로 중 이것을 지나는 것이 없었습니다.

소비처 조사에서 같은 방향을 가리키는 세 가지가 나왔습니다.

- **compose 예제가 존재하지 않는 설정을 구성하고 있었습니다.** `examples/docker-compose.yml`은 `HOST_PROC_PATH: /host/proc`을 설정했지만 바이너리는 그 변수를 읽지 않습니다. 트리의 다른 어디에도 등장하지 않으며, `src/device/container_utils.rs`는 하드코딩된 목록(`/host/proc`, `/hostproc`, `/proc_host`)을 탐색하는 방식으로 호스트 procfs를 찾습니다. 없는 설정을 구성하는 예제는 누군가 실제로 돌려 봤다는 가정과 맞추기 어렵습니다.
- **`.dockerignore`는 한 번도 추적된 적이 없습니다.** `.gitignore`에 포괄적인 `.*` 규칙이 있어 로컬 사본은 조용히 커밋되지 않았고, CI는 git에서 체크아웃합니다. 따라서 모든 `docker-check` 실행이 제외 규칙 없이 빌드하며 `target/`과 `.git/`을 빌드 컨텍스트로 보냈습니다.
- **`docker-build-container` Makefile 타겟은 문서화만 안 된 것이 아니라 죽어 있었습니다.** 트리 전체에서 이 저장소를 `docker build`하는 유일한 지점이었고, `f3745e2`(#31)에서 추가된 뒤 한 번도 참조되지 않았습니다. `.PHONY`에도 `make help`에도 없고, 어떤 스크립트나 워크플로, 문서도 호출하지 않습니다. `DEVELOPERS.md`는 타겟이 아니라 원시 `docker build` 명령을 문서화하고 있었으므로 문서조차 그 존재를 몰랐습니다.

## 2. 변경 요약

| 항목 | 값 |
|------|----|
| 변경 파일 | 7개 |
| 추가 줄 | 6줄 |
| 삭제 줄 | 642줄 |
| 런타임 코드 변경 | 없음 |
| 제거된 CI 잡 | 1개 |

### 파일

| 파일 | 변경 |
|------|------|
| `Dockerfile` | 삭제 (61줄). |
| `examples/docker-compose.yml` | 삭제 (58줄). |
| `tests/docker_build_context_test.rs` | 삭제 (437줄). |
| `.github/workflows/ci.yml` | `docker-check` 잡과 그에 딸린 57줄 비용 분석 주석 헤더 제거. |
| `Makefile` | `docker-build-container` 타겟 제거. |
| `DEVELOPERS.md` | `docker build` 지침, `Docker Check` CI 항목, `Docker images` 릴리스 항목 제거. |
| `README.md` | Installation 절 끝에 컨테이너가 지원 경로가 아님을 밝히는 짧은 주석 추가. |

## 3. 기술적 선택과 그 이유

### 3.1 빌드 컨텍스트 테스트 삭제는 의도된 것

`tests/docker_build_context_test.rs`는 부수 피해가 아닙니다. PR #322가 하루 전에 #319가 `main`을 깨뜨린 뒤 Docker 빌드 컨텍스트를 지키려고 명시적으로 추가한 테스트입니다. `Dockerfile`이 없으면 대상이 사라지고, 조용히 통과하는 것이 아니라 아예 실패합니다. 일곱 테스트 중 둘이 `.expect("Dockerfile must exist")`를 호출하기 때문입니다. `Dockerfile`과 같은 커밋에서 사라져야 했고, 그 가드의 소실이 조용히 흡수되지 않도록 여기에 명시합니다.

### 3.2 성격이 다른 문서 결함 두 건

둘 중 하나만 이 PR이 만든 것이며, 그 구분은 유지할 가치가 있습니다.

- `DEVELOPERS.md`는 이 PR이 삭제하는 `Docker Check` 잡을 설명하고 있었습니다. **이 변경으로 무효화됨.**
- `DEVELOPERS.md`는 릴리스가 게시하는 항목에 `Docker images`를 올려 두었습니다. **이것은 이 변경 이전부터 이미 거짓**이었고, 그 줄이 존재한 내내 그랬습니다. 어떤 워크플로도 이미지를 게시한 적이 없습니다. `.github/` 아래 어디에도 `docker/login-action`, `docker push`, `ghcr.io` 참조가 없습니다. 이 줄의 삭제는 이 PR이 거짓으로 만든 주장이 아니라 애초에 참인 적이 없던 주장을 바로잡는 것입니다.

지적했으나 의도적으로 고치지 않은 것: 같은 `DEVELOPERS.md` CI 절이 잡을 세 개로 적고 있으나 `ci.yml`에는 실제로 일곱 개가 있습니다. 이 낙후는 이 PR보다 앞서고 고치는 것은 범위 확장이므로 별도 문서 정리에 남깁니다.

### 3.3 README 주석의 목적

Installation 절은 여섯 개 설치 옵션을 나열하면서 컨테이너에 대해 아무 말도 하지 않았기 때문에, 일곱 번째를 기대한 독자는 그 부재를 추론해야 했습니다. 주석은 지원되는 대안을 명시합니다. 릴리스 바이너리, Homebrew tap, Debian 패키지와 Ubuntu PPA, `cargo install all-smi`, 그리고 systemd/launchd/Windows SCM 아래에서 API 모드를 감독 실행하는 `all-smi service`입니다. 전부 현재 동작하며 CI가 실행합니다. 이미지는 한 번도 그러지 못했습니다.

### 3.4 명시적으로 건드리지 않은 것

- **`src/` 아래 컨테이너 인식 코드**: `/.dockerenv` 탐색, cgroup 파싱, `ContainerRuntime::Docker`, Docker 인식 디스크 필터링. all-smi가 자신이 컨테이너 **안에서** 돌고 있음을 감지하는 것은 이미지 배포와 무관한 제품 기능입니다.
- **컨테이너 테스트 하네스**: `tests/` 아래 셸 스크립트와 세 개의 `docker-dev` Makefile 타겟. 이들은 그 기능을 검증하려고 stock `rust:1.88` 컨테이너 안에서 all-smi를 실행하며, 어느 것도 이 저장소의 이미지를 빌드하지 않았습니다. `DEVELOPERS.md`는 이들의 문서를 유지하되 무엇을 하는지 명확히 하는 주석을 덧붙였습니다.
- **이력**: `README.md`와 `debian/changelog`의 변경 이력 줄, #322와 #339에 대한 `TECHNICAL_REPORTS/` 항목은 실제로 일어난 일을 기록한 것이며 여전히 참입니다.

## 4. 검증 결과

- `actionlint .github/workflows/ci.yml`은 변경 전과 동일한 기존 지적 5건을 보고하며 새 지적은 없습니다. 기준선은 편집 전에 확보했습니다.
- `ci.yml`의 PyYAML 파싱이 성공합니다. 잡은 이제 `test`, `packaging-sync`, `systemd-service`, `launchd-service`, `windows-service`, `build-check`이며 `docker-check`는 없고 남은 모든 `needs:` 대상이 실존 잡으로 해석됩니다.
- 제거 이전에도 `docker-check`를 참조한 잡이 없었으므로 그래프 수리가 필요 없었습니다. 이 문자열은 자기 주석 헤더 안과 잡 키로만 등장했습니다.
- `main`은 브랜치 보호가 걸려 있지 않아 `Docker Build Check`를 요구하는 필수 상태 체크가 없고, 잡 제거가 머지를 막을 수 없습니다.
- 통합 테스트 타겟 19개 전부가 개별 실행에서 통과합니다. 라이브러리 단위 테스트 1363개 통과.
- `cargo fmt --check`, `cargo clippy --lib --tests -- -D warnings`, `cargo clippy --bin all-smi -- -D warnings` 모두 깨끗합니다.
- `TECHNICAL_REPORTS/`, `debian/changelog`, `README.md` 변경 이력을 제외하면 `Dockerfile`, `docker-check`, `docker-build-container`, `docker_build_context_test`, `docker-compose`에 대한 잔여 참조가 없습니다.

## 5. 결과 및 후속

- PR #344는 `dd17ebd`로 `main`에 squash merge되었습니다.
- 이슈 #342는 PR의 `Closes #342` 링크로 자동 종료되었습니다.
- 이 변경은 v0.26.0에 breaking change로 나갔습니다. 로컬에서 이미지를 빌드하던 사용자는 지원되는 설치 경로 중 하나로 옮겨야 합니다.
- `DEVELOPERS.md`의 CI 잡 목록은 여전히 낙후 상태(세 개 표기, 실제 일곱 개)이며 별도 문서 정리에 남겨 두었습니다.
- #322의 빌드 컨텍스트 가드는 대상과 함께 사라졌습니다. 향후 컨테이너 빌드를 다시 도입한다면 그 가드는 복원이 아니라 재작성해야 합니다. 더 이상 존재하지 않는 `Dockerfile`을 단언하던 테스트이기 때문입니다.

---

## 부록: 관련 PR 및 이슈

| 번호 | 관계 |
|------|------|
| 이슈 #342 | 모든 아키텍처에서 이미지 실행 실패를 기록, 이 PR로 종료 |
| PR #322 | #319가 `main`을 깨뜨린 뒤 이 PR이 삭제하는 빌드 컨텍스트 테스트를 추가 |
| PR #339 | push 전용 게이트를 제거해 PR에서도 이미지를 빌드하게 함, 하루 뒤 무의미해짐 |
| PR #343 | vendored OpenSSL 의존성 제거, 인접 정리 작업이며 파일 중복 없음 |
