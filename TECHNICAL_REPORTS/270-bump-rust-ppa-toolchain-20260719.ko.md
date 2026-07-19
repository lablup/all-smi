# 기술 보고서: PR #270 - chore: bump Rust PPA toolchain to 1.96

**작성일**: 2026-07-19
**작성자**: AI Code Reviewer
**상태**: 완료
**언어**: TOML, Debian 패키징(control/rules), YAML, Rust
**위험도**: Low

---

## 요약

이 PR은 lablup `rustc-release` 의존성 PPA가 `rustc-1.96`을 배포함에 따라, 크레이트 MSRV와 Launchpad/Debian 패키징 경로 전반의 Rust 하한을 1.95에서 1.96으로 올린다. 작업 중 CI에서 이 변경과 무관한 기존 문제가 드러났다. `ubuntu-latest` 러너의 기본 stable 툴체인이 1.97로 이동했고, 1.97의 clippy가 `google_tpu.rs`의 손대지 않은 코드에 대해 새 스타일 린트 두 개를 강제했다. 버전 범프와 clippy 수정 세 건을 함께 반영해 `cargo clippy -- -D warnings` 게이트를 통과시켰다.

---

## 1. 문제 정의

### 1.1 배경
`ppa:lablup/backend-ai` PPA를 위한 Debian/Launchpad 빌드는 빌드 중 툴체인을 내려받을 수 없다(Launchpad 빌드 호스트에는 인터넷이 없다). 그래서 `~lablup/+archive/ubuntu/rustc-release` 의존성 PPA에서 버전이 붙은 Rust 패키지를 가져와 사용한다. 이 PPA는 이전에 `rustc-1.95`를 제공했고, 지금은 `rustc-1.96`을 제공한다. 빌드의 `Build-Depends`와 `debian/rules`의 툴체인 탐지가 버전이 붙은 패키지 이름을 명시하므로, PPA가 실제로 배포하는 버전을 따라가야 한다.

### 1.2 기존 문제점

- **문제 1 (PPA 하한 지연)**: 패키징이 `rustc-1.95 | rustc (>= 1.95)`와 `cargo-1.95`를 참조했고 `debian/rules`가 `rustc-1.95`를 먼저 탐지했다. 의존성 PPA가 버전 패키지를 `1.96`으로 올리면 `rustc-1.95` 이름이 더는 해석되지 않고, 탐지가 배포판 기본 무버전 `rustc`(jammy/noble/resolute에서는 너무 낮음)로 조용히 폴백해 `>= 1.95` 가드에서 실패한다.
- **문제 2 (MSRV/하한 결합)**: 이 저장소는 크레이트 MSRV(`Cargo.toml`의 `rust-version`)와 패키징 하한을 함께 유지한다(선례: 커밋 `48a2a45`이 둘을 함께 1.95로 상향). 패키징만 올리면 둘이 어긋난다.
- **문제 3 (CI에서 드러난 기존 문제)**: `ci.yml`의 Test Suite 잡은 `ubuntu-latest` 기본 stable 툴체인에서 `cargo clippy -- -D warnings`를 실행하는데, 이 툴체인은 고정되어 있지 않다. 해당 툴체인이 1.97로 올라갔고, clippy 1.97은 `src/device/readers/google_tpu.rs`의 `format!("TPU {}", &chip_version)` 호출 세 곳에 대해 `useless_borrows_in_formatting`과 `uninlined_format_args`를 지적한다. `-D warnings` 아래에서 이들은 하드 에러가 된다. 마지막으로 녹색이던 `main` CI는 2026-06-26이었고, 이 실패는 이 PR의 내용이 아니라 툴체인 드리프트가 원인이다.

### 1.3 위험성

| 위험 | 영향도 | 발생 가능성 |
|-----|-------|-----------|
| 의존성 PPA가 1.96으로 롤한 뒤 PPA 빌드가 `rustc-1.95`를 찾지 못함 | High (Launchpad 릴리스 중단) | 구 패키지가 내려가는 순간 High |
| MSRV와 패키징 하한이 어긋남 | Low | Medium |
| 고정되지 않은 CI clippy 툴체인이 무관한 향후 PR을 막음 | Medium (머지 차단) | High (이미 발생) |

---

## 2. 기술적 검토 사항

### 2.1 보안 관점
보안 영향 없음. 변경은 빌드 메타데이터, 패키징 서술자, 그리고 런타임 동작과 외부 입력 처리에 변화가 없는 포매팅 전용 Rust 편집으로 한정된다.

### 2.2 성능 관점
런타임 성능 영향 없음. `google_tpu.rs` 편집은 출력이 동일한 `format!` 문자열 변환이다.

### 2.3 호환성 및 의존성

- **호환성 파괴**: 크레이트 MSRV가 이제 1.96이다. 소스에서 Rust 1.95로 빌드하는 사용자는 Cargo의 `rust-version` 게이트에 의해 거부된다. 이는 의도된 것이며 패키징 하한과 일치한다.
- **새 의존성**: 없음. `Cargo.lock` 변경 없음. 락파일 포맷은 그대로이며 1.96이 기존 v4 락파일을 파싱한다. 워크플로의 `cargo metadata --locked` 검증도 유효하다.
- **배포판 커버리지**: jammy(22.04), noble(24.04), resolute(26.04) 모두 `rustc-1.96`을 위해 `~lablup/+archive/ubuntu/rustc-release`에 의존한다. resolute의 아카이브 `rustc`/`cargo`(1.93)는 하한 아래다.

### 2.4 코드 품질

- **테스트 커버리지**: 변화 없음(테스트 추가 없음. 메타데이터와 포매팅 편집이 전부).
- **린트 상태**: stable 1.97.1 로컬에서 `cargo clippy -- -D warnings`와 `cargo fmt --check` 모두 통과하며, CI 게이트와 일치한다.
- **기술 부채**: 소폭 감소. `google_tpu.rs`의 오래된 `format!` 세 곳이 이제 코드베이스가 이미 채택한 인라인 포맷 인자 스타일과 일관된다.

---

## 3. 기술적 선택과 그 이유

### 3.1 clippy 수정을 별도 PR이 아니라 이 PR에 함께 묶음

**맥락:**
clippy 실패는 기존 문제(툴체인 드리프트)이자 버전 범프와 엄밀히 무관했지만, #270 머지를 막았고 수정 전까지 저장소의 다른 모든 PR도 막았을 것이다.

**검토한 대안:**

| 선택지 | 장점 | 단점 |
|-------|------|------|
| #270에 수정을 묶기 | 즉시 차단 해제, 왕복 1회, 주제상 '새 Rust 툴체인 대응' chore | 패키징 chore에 무관한 소스 수정이 섞임 |
| 별도 `fix:` PR 먼저 뒤 #270 리베이스 | 관심사 분리가 깔끔 | 단계 증가, 다른 PR 머지 전까지 #270이 빨간색 유지 |
| **선택: #270에 묶기**(사용자 확인) | 녹색까지 가장 빠름, '새 Rust 대응' 의도를 한곳에 | 범위가 소폭 넓어짐(명시적으로 수용) |

**근거:**
패키징 범프와 clippy 수정 모두 새 Rust 툴체인으로 이동한 결과이므로, 묶으면 그 의도가 응집된다. 사용자에게 물어 묶기로 결정했다.

**트레이드오프:**
chore PR이 이제 소스 변경도 담는다. 이는 조용히 쓸어담은 것이 아니라 의도적으로 수용했다.

### 3.2 MSRV와 패키징 하한을 함께 이동

패키징만이 아니라 `Cargo.toml`의 `rust-version`도 함께 올림으로써, 선언된 MSRV가 곧 빌드 하한이라는 저장소의 확립된 불변식을 유지한다. 패키지가 크레이트가 주장하는 것보다 높은 버전을 요구하는 혼란스러운 분리를 피한다.

---

## 4. 구현 세부사항

### 4.1 버전 참조 갱신 (1.95 → 1.96)

- `Cargo.toml`: `rust-version = "1.96"` (MSRV 하한).
- `.github/workflows/launchpad_ppa.yml`: 세 배포판 매트릭스(jammy/noble/resolute)의 `vendor_rust: "1.96.0"`와 `toolchain_source` 설명 문자열. 이 잡은 고정된 `dtolnay/rust-toolchain@master` 툴체인으로 의존성을 벤더링하므로, Test Suite를 물었던 `ubuntu-latest` 드리프트의 영향을 받지 않는다.
- `debian/control`, `debian/control.source`: `rustc-1.96 | rustc (>= 1.96)`, `cargo-1.96 | cargo (>= 1.96)`.
- `debian/rules`, `debian/rules.source`, `debian/rules.launchpad`, `debian/rules.launchpad-simple`: 툴체인 탐지가 `rustc-1.96`/`cargo-1.96`를 우선하고, `dpkg --compare-versions` 가드가 `>= 1.96`을 요구한다.
- `debian/README.packaging`: 하한 설명, PPA `provides` 문구, 트러블슈팅 안내.

### 4.2 `src/device/readers/google_tpu.rs`의 clippy 수정

521, 610, 724행의 동일한 `accel_type` 구성 세 곳이 두 clippy 린트를 거쳐 변화했다:

```rust
// 원본 (clippy 1.97의 useless_borrows_in_formatting 실패)
let accel_type = format!("TPU {}", &chip_version);

// 중간 (clippy 1.97의 uninlined_format_args 실패)
let accel_type = format!("TPU {}", chip_version);

// 최종
let accel_type = format!("TPU {chip_version}");
```

**변경 이유:** `cargo clippy -- -D warnings` 아래에서는 두 스타일 린트가 모두 에러다. 최종 형태는 캡처된 식별자를 사용해 둘을 동시에 만족하며 코드베이스의 나머지와 일치한다.

---

## 5. 학습 포인트

### 5.1 고정되지 않은 CI 툴체인은 조용히 드리프트한다

**개념:**
`ci.yml`은 `ubuntu-latest` 이미지가 제공하는 stable Rust에서 clippy를 실행하며 `dtolnay/rust-toolchain` 고정이 없다. GitHub이 그 이미지를 주기적으로 갱신하므로 저장소 변경 없이도 `stable`이 올라간다.

**이 PR에서의 적용:**
Test Suite가 마지막 녹색 실행 이후 아무 커밋도 손대지 않은 코드에서 실패했다. clippy 릴리스마다 `style`/`suspicious` 린트를 승격하거나 추가할 수 있고, `-D warnings` 아래에서는 기존 코드에 걸린 새 린트가 러너 갱신 순간 빌드를 깨는 에러가 된다. 반면 패키징 빌드는 툴체인을 고정하므로 영향받지 않았다.

**일반적 활용:**
- CI clippy/test 툴체인을 알려진 버전(또는 채널 스냅샷)으로 고정해 린트 변화가 의도된 업데이트로 반영되게 한다.
- 또는 PR 게이팅과 분리해 `stable`에서 예약된 clippy 잡을 돌려 드리프트를 조기에 감지한다.

### 5.2 clippy 포맷 인자 린트는 연쇄로 온다

`useless_borrows_in_formatting`을 고치면(`&` 제거) 같은 줄에서 바로 `uninlined_format_args`가 드러날 수 있다. 차용을 제거하면 인자가 캡처 가능한 평범한 식별자가 되기 때문이다. CI와 정확히 같은 툴체인을 로컬에서 재현하니(여기서는 `rustup` stable 1.97.1) CI를 세 번째로 왕복하지 않고 한 번에 둘 다 해결할 수 있었다.

---

## 6. 추가 학습

### 핵심 용어
| 키워드 | 설명 | 이 PR에서의 관련성 |
|-------|------|------------------|
| `rust-version` (MSRV) | Cargo의 최소 지원 Rust 게이트 | 1.96으로 상향, 빌드 시점에 구 툴체인 거부 |
| `~lablup/+archive/ubuntu/rustc-release` | 최신 버전 `rustc`/`cargo`를 제공하는 의존성 PPA | 오프라인 Launchpad 빌드용 `rustc-1.96` 출처 |
| `useless_borrows_in_formatting` | 포맷 인자의 불필요한 `&`에 대한 clippy 린트 | `google_tpu.rs`의 첫 실패 |
| `uninlined_format_args` | 위치 인자보다 `{var}` 캡처를 선호하는 clippy 린트 | 차용 제거 후 두 번째 실패 |

### 관련 기술/프레임워크
- **dtolnay/rust-toolchain**: Rust 툴체인 고정용 GitHub Action. `launchpad_ppa.yml`에는 (고정으로) 쓰이고 `ci.yml`에는 없다.
- **Launchpad / dput**: Ubuntu PPA 빌드/업로드 경로. 빌드가 인터넷 없이 돌아가므로 벤더링 의존성과 버전 빌드 의존성이 필요하다.

### 관련 PR/이슈
- 커밋 `48a2a45`: MSRV를 1.95로 상향. MSRV와 패키징 하한을 결합해 유지하는 직접적 선례.
- 보고서 `92-ppa-build-deps-20251223.md`: PPA 빌드 의존성 집합에 대한 이전 작업.

---

## 7. 변경 요약

### 통계
| 항목 | 값 |
|-----|---|
| 변경 파일 | 10 |
| 추가 라인 | +42 |
| 삭제 라인 | -42 |
| 추가 테스트 | 0 |

### 카테고리별 변경

| 카테고리 | 수 | 요약 |
|---------|---|------|
| 빌드/패키징 | 8 | Cargo.toml, control(.source), rules(+변형), 워크플로의 Rust 하한 1.95 → 1.96 |
| 문서 | 1 | `debian/README.packaging` 하한/PPA 안내 |
| 코드 품질 | 1 | `google_tpu.rs` 인라인 포맷 인자(clippy 1.97 게이트 차단 해제) |

### 관련 커밋
| 해시 | 유형 | 메시지 |
|-----|------|-------|
| `a98b65c` | chore | bump Rust PPA toolchain to 1.96 |
| `5801c15` | fix | drop redundant borrows in google_tpu format args |
| `9079819` | fix | inline format args in google_tpu accel_type |

---

## 8. 후속 조치

### 필수
- [ ] 차단 항목 없음. 다음 릴리스에서 PPA 업로드 워크플로(`launchpad_ppa.yml`)를 실행해 `rustc-1.96`으로 빌드된 패키지를 배포해야 한다.

### 모니터링 필요
- 다음 Launchpad 빌드가 세 배포판(jammy/noble/resolute) 모두에서 의존성 PPA로부터 `rustc-1.96`을 해석하는지 확인한다.

### 향후 개선
- `ci.yml`의 툴체인을 고정하거나(예: `dtolnay/rust-toolchain`) `stable` 예약 clippy 잡을 추가해, 툴체인 드리프트가 `-D warnings` 아래에서 무관한 PR을 깨지 않도록 한다.
- `debian/README_PPA.md`는 여전히 폐기된 `rust-1.85-all` 방식을 설명하며 버전 패키지 경로와 불일치한다. 별도로 정리한다.

---

## 부록

### A. 테스트 결과
CI Test Suite(`cargo test --verbose`, `cargo fmt --check`, `cargo clippy -- -D warnings`), Build Check, license/CLA 모두 통과. Docker Build Check는 경로 필터로 스킵. stable 1.97.1 로컬 검증: `cargo clippy -- -D warnings` 종료 코드 0, `cargo fmt --check` 정상.

### B. 성능 벤치마크
해당 없음(런타임 변경 없음).

### C. 참고 자료
- Clippy 린트 인덱스: `useless_borrows_in_formatting`, `uninlined_format_args` (rust-clippy 문서).
- Launchpad PPA 빌드 제약: `debian/README.packaging`, `debian/README_PPA.md`.
