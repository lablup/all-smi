# 기술 보고서: PR #343 - 죽은 vendored OpenSSL 의존성 제거

**일자**: 2026-08-06  
**상태**: 완료  
**관련 항목**: PR #343, 이슈 #341  
**위험 수준**: 낮음 (의존성 제거, 소스 변경 없음)

---

## 요약

PR #343은 `Cargo.toml`에서 타겟 조건부 `openssl = { features = ["vendored"] }` 블록 두 개를 삭제했습니다. 트리 안에서 OpenSSL을 쓰는 코드는 없었고, 이 두 항목이 OpenSSL이 의존성 그래프에 들어와 있던 유일한 이유였습니다. 릴리스 워크플로가 빌드하는 `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-gnu`, `aarch64-unknown-linux-musl`이 정확히 두 `cfg`가 덮던 타겟이었기 때문에, 모든 릴리스가 아무도 링크하지 않는 라이브러리를 소스에서 세 번씩 컴파일하고 있었습니다.

변경은 `Cargo.toml`과 `Cargo.lock`에 걸친 17줄 삭제이며 Rust 소스는 건드리지 않았습니다. 검증은 의존성 그래프 조회만이 아니라 실제 크로스 타겟 릴리스 빌드로 수행했습니다.

---

## 1. 문제 정의

커밋 `f1eeb4e`가 두 블록을 추가할 당시 매니페스트는 reqwest 0.12를 선언하고 있었고, 이는 native-tls로 해석되어 살아 있는 전이 `openssl-sys`를 끌고 왔습니다. `openssl`을 `vendored` 피처와 함께 직접 선언하면 그것이 정적 링크되므로 musl과 크로스 컴파일 aarch64 릴리스 바이너리에 적합했습니다.

이후 커밋 `3de545d`가 reqwest 0.13으로 옮겨 갔고, 0.13은 rustls를 기본으로 씁니다. 살아 있던 전이 의존성이 사라졌고 두 블록은 그때부터 흔적만 남은 상태였습니다.

비용은 이론적인 것이 아니었습니다. OpenSSL vendored 빌드는 C 라이브러리를 소스에서 컴파일하며, 태그가 붙는 릴리스마다 릴리스 매트릭스의 세 타겟에서 실행됐습니다. 소비자가 없는 의존성을 위해서였습니다.

## 2. 변경 요약

| 항목 | 값 |
|------|----|
| 변경 파일 | 2개 |
| 추가 줄 | 0줄 |
| 삭제 줄 | 17줄 |
| Rust 소스 변경 | 없음 |
| 테스트 추가 | 0개 |

### 파일

| 파일 | 목적 |
|------|------|
| `Cargo.toml` | `cfg(target_env = "musl")`와 `cfg(all(target_arch = "aarch64", target_env = "gnu"))` 의존성 블록 제거. |
| `Cargo.lock` | `all-smi` 의존성 목록에서 `openssl`을 제거하고, `openssl-src` 패키지 항목과 `openssl-sys`에서 나가는 엣지를 삭제. |

## 3. 기술적 선택과 그 이유

### 3.1 매니페스트를 읽는 대신 독립적으로 확인

"아무도 안 쓴다"는 주장하기는 쉽고 틀렸을 때 비싼 종류의 전제입니다. 여기서 실수하면 릴리스 바이너리에서만, 그것도 개발 머신이 빌드하지 않는 타겟에서만 TLS가 깨집니다. 그래서 삭제 전에 독립적인 확인을 두 번 받았습니다.

- `grep -rn openssl src/ --include='*.rs'`는 아무것도 반환하지 않습니다.
- 변경 전 `cargo tree -i openssl`은 `aarch64-unknown-linux-gnu`, `aarch64-unknown-linux-musl`, `x86_64-unknown-linux-musl`, `--target all`에서 `all-smi` 자신을 유일한 역의존으로 보고했고, `x86_64-unknown-linux-gnu`에서는 아무것도 반환하지 않았습니다.

결정적인 것은 두 번째입니다. 이 크레이트가 그래프에 있던 이유는 무언가가 끌어와서가 아니라 이 매니페스트가 넣었기 때문입니다.

### 3.2 "Cargo.lock에서 사라졌는가"는 잘못된 성공 기준

변경 후에도 `openssl`, `openssl-sys`, `foreign-types`, `native-tls`는 `Cargo.lock`에 남아 있습니다. 이는 잔재가 아니라 정상입니다. 락파일은 피처 게이트가 걸린 엣지를 포함한 그래프의 합집합을 기록하며, `furiosa-smi-rs -> attohttpc -> native-tls -> openssl` 경로가 이 프로젝트가 빌드하는 어떤 타겟에서도 활성화되지 않는데도 항목을 유지시킵니다.

올바른 확인은 `cargo tree --target all -i openssl`이 아무것도 반환하지 않는 것이고, 지금 그렇습니다.

### 3.3 `openssl-probe`는 건드리지 않는다

이름과 달리 `openssl-probe`는 `openssl` 크레이트와 무관합니다. `reqwest -> rustls-platform-verifier -> rustls-native-certs -> openssl-probe` 경로로 살아 있고 모든 Linux 빌드에서 여전히 컴파일되며, 이것이 rustls의 정상 동작입니다. 이름이 겹친다는 이유로 지웠다면 인증서 탐색이 깨졌을 것입니다.

## 4. 검증 결과

`cargo tree`는 논증이지 빌드가 아니므로, 브랜치에서 Linux 컨테이너로 크로스 타겟 빌드를 실행했습니다. 세 건 모두 실제 릴리스 바이너리를 만들었고 어느 것도 `openssl-sys`를 컴파일하지 않았습니다.

| 타겟 | 결과 | 시간 | 바이너리 | `openssl-*` 빌드 디렉터리 |
|------|------|------|----------|---------------------------|
| `aarch64-unknown-linux-gnu` | 빌드됨 | 2m58s | 9.6 MB | 없음 |
| `aarch64-unknown-linux-musl` | 빌드됨 | 2m40s | 9.0 MB | 없음 |
| `x86_64-unknown-linux-musl` | 빌드됨 (에뮬레이션 amd64) | 19m28s | 12.0 MB | 없음 |

- `cargo tree --target all -i openssl`은 아무것도 반환하지 않으며, 세 타겟 개별로도 마찬가지입니다.
- `cargo tree --target all -i openssl-probe`는 여전히 `rustls-native-certs`를 통해 해석되어 살아 있는 경로가 유지됨을 확인했습니다.

## 5. 결과 및 후속

- PR #343은 `59b3c9c`로 `main`에 squash merge되었습니다.
- 이슈 #341은 PR의 `Closes #341` 링크로 자동 종료되었습니다.
- 영향받는 세 타겟의 릴리스 빌드 시간이 vendored OpenSSL 컴파일 비용만큼 줄어듭니다. 위 측정값은 그 상한을 보여 주지만 분리하지는 못합니다. 같은 호스트의 before/after 쌍이 아니라 전체 빌드 시간이기 때문입니다.
- 남은 후속 작업은 없습니다. 향후 어떤 의존성이 살아 있는 `openssl-sys` 엣지를 다시 들여온다면, `vendored` 결정은 이 이력에서 복원할 것이 아니라 그 시점의 근거로 다시 내려야 합니다.

---

## 부록: 핵심 키워드

| 키워드 | 설명 | 관련성 |
|-------|------|--------|
| `vendored` 피처 | 시스템 라이브러리를 링크하는 대신 번들된 C 소스에서 OpenSSL을 빌드 | 죽은 의존성이 단지 존재하는 수준이 아니라 비쌌던 이유 |
| `cargo tree -i` | 역의존 조회: 이 크레이트에 누가 의존하는가 | `all-smi`가 유일한 역의존임을 증명한 검사 |
| 피처 게이트 락파일 엣지 | `Cargo.lock`은 어떤 타겟에서도 활성화되지 않는 엣지도 기록 | 제거 후에도 `openssl`이 락파일에 남는 이유 |
