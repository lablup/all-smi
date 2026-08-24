# 기술 보고서: PR #363 - 진단기에서 고정된 libamdgpu_top 버전과 level_zero 보고

**일자**: 2026-08-08  
**상태**: 완료  
**관련 항목**: PR #363, 이슈 #362, PR #358 리뷰 중 발견  
**위험 수준**: 낮음 (진단 보고 전용, 런타임 동작 변경 없음)

---

## 요약

PR #363은 `doctor` 모듈의 보고 결함 두 건을 고쳤습니다. 둘 다 실행을 깨뜨리지는 않지만 `doctor`가 거짓이거나 불완전한 것을 진술하게 만들며, 이 모듈이 이미 무언가 잘못됐을 때 신뢰받기 위해 존재한다는 점에서 중요합니다.

`amd.libamdgpu_top.abi`는 메시지에 `env!("CARGO_PKG_VERSION")`을 넣고 있어 의존성이 아니라 all-smi 자신의 버전을 보고했습니다. 0.25.0 기본 빌드가 핀은 `=0.11.5`인데 `linked libamdgpu_top 0.25.0`이라고 보고했습니다. 별개로, 지원 번들 패커의 `enabled_features()`에는 `level_zero` 분기가 없어서, 그 피처를 켠 빌드가 생성하는 모든 번들에서 자신을 과소 보고했습니다.

---

## 1. 문제 정의

두 결함 모두 #358을 리뷰하다 발견했고, 그 PR의 범위를 유지하기 위해 의도적으로 거기서 뺐습니다.

두 번째는 이름 붙일 가치가 있는 구조적 원인을 갖습니다. `enabled_features()`의 분기들은 `#[cfg]` 게이트가 걸려 있으므로, 런타임 테스트는 테스트 바이너리 자신이 빌드될 때 켜진 피처만 관측합니다. **꺼진 피처에 대한 분기 누락은 그런 테스트에게 정의상 보이지 않습니다.** `level_zero`가 피처가 들어온 날부터 계속 빠져 있었고 `amd`도 #358 전까지 빠져 있었던 이유가 정확히 이것입니다.

## 2. 변경 요약

| 항목 | 값 |
|------|----|
| 변경 파일 | 2개 |
| 추가 줄 | 219줄 |
| 삭제 줄 | 3줄 |
| 테스트 추가 | 4개 |
| 런타임 동작 변경 | 없음 |

### 파일

| 파일 | 변경 |
|------|------|
| `src/doctor/checks/amd.rs` | ABI 메시지의 `env!("CARGO_PKG_VERSION")`을 대체하는 `LIBAMDGPU_TOP_PINNED_VERSION` 상수, `pinned_version_matches_cargo_toml`, `libamdgpu_top_is_pinned_exactly` 추가. |
| `src/doctor/bundle.rs` | 누락된 `#[cfg(feature = "level_zero")]` 분기, `bundle_covers_every_declared_feature`, `enabled_features_matches_build_configuration` 추가. |

## 3. 기술적 선택과 그 이유

### 3.1 옵션 A(상수 + 매니페스트 파싱 가드 테스트), 옵션 B(`build.rs`에서 방출)가 아님

옵션 B는 값을 복제하지 않고 도출하므로 구조적으로 어긋남을 불가능하게 만든다는 점에서 매력적입니다. 두 가지 근거로 기각했습니다.

**첫째, 수용 기준은 핀을 올리고 대응 갱신을 하지 않았을 때 테스트가 실패할 것을 요구합니다.** 옵션 B에서는 보고 값이 핀을 조용히 따라가므로 실패할 것이 남지 않습니다. 파서가 깨지는 경우를 생각하기 전까지는 이것이 엄격히 더 나아 보입니다. 빌드 스크립트와 더 이상 맞지 않게 `Cargo.toml`이 재배치되면 잘해야 빌드 실패, 최악에는 조용히 틀린 컴파일 타임 값이 나오고, 그것을 말해 줄 테스트가 없습니다. 옵션 A는 실패하는 모습을 볼 수 있는 진짜 단언을 남기며, 이것이 가드와 가정의 차이입니다.

**둘째, 옵션 B는 파싱을 `build.rs`로 옮깁니다.** 거기서는 `amd` 피처를 결코 켜지 않을 소비자를 포함한 모든 소비자의 모든 빌드에서 실행되고, 실패는 테스트 이름이 아니라 빌드 스크립트 오류로 나타납니다. 옵션 A는 같은 파싱을 테스트 하네스에 가둡니다. 비용은 전사한 문자열 하나이며, 그 비용을 안전하게 만드는 것이 테스트입니다.

### 3.2 피처 커버리지 테스트는 소스 텍스트를 파싱하며, 그것만이 작동한다

`bundle_covers_every_declared_feature`는 `Cargo.toml`의 `[features]` 표를 파싱해 `enabled_features`의 **소스 텍스트**에서 찾은 피처 게이트와 비교합니다. 둘 다 `include_str!`로 임베드하므로 경로 추측이 개입하지 않습니다.

이는 이례적이며, 그 이유는 1절에 적은 그대로입니다. 런타임 테스트는 자신이 함께 컴파일되지 않은 피처의 분기 누락을 관측할 수 없습니다. 존재하지 않는 분기를 보는 유일한 방법이 소스를 읽는 것입니다.

깨지기 쉬움은 수용한 것이 아니라 제한했습니다.

- 스캔 범위를 `fn enabled_features()`부터 다음 최상위 `fn`까지로 한정해, 파일의 다른 곳에 있는 게이트나 테스트 모듈 자체에 등장하는 피처 이름이 단언을 충족할 수 없게 했습니다.
- 매니페스트 파싱과 함수 탐색 모두 헛된 통과로 저하하는 대신 설명이 달린 메시지와 함께 크게 실패합니다.
- 루프 실행 전에 비어 있거나 형식이 잘못된 피처 목록을 sanity 단언이 거부합니다.
- `enabled_features_matches_build_configuration`이 런타임 절반을 양방향으로 덮습니다. 컴파일된 피처는 나타나야 하고, 컴파일되지 않은 피처는 나타나면 안 됩니다.

### 3.3 상수의 cfg가 dead code가 되지 않게 한다

`LIBAMDGPU_TOP_PINNED_VERSION`은 보고 분기 자신의 cfg와 `test`에 게이트돼 있으므로, 아무도 읽지 않는 musl, 비 Linux, `amd` off 빌드에서는 아예 없고 결코 dead code가 되지 않으면서, 가드 테스트는 모든 구성에서 계속 돕니다.

#358이 도입한 3상태 보고는 그대로입니다. 링크됨, musl 게이트로 컴파일 제외, `amd` 피처로 컴파일 제외. 나머지 정당한 `env!("CARGO_PKG_VERSION")` 사용처 6곳도 그대로입니다.

## 4. 검증 결과

두 가드 모두 통과를 관측하기 전에 실패를 관측했습니다. 가드가 가드임을 아는 유일한 방법입니다.

**결함 1 가드.** 핀을 일시적으로 `=0.11.4`로 옮긴 뒤 `cargo test --lib doctor::checks::amd`:

```
pinned_version_matches_cargo_toml ... FAILED
libamdgpu_top is pinned to 0.11.4 in Cargo.toml but amd.libamdgpu_top.abi reports 0.11.5
```

핀 복원 후 같은 명령: `2 passed; 0 failed`. 커밋 전 `git diff Cargo.toml`은 비어 있었습니다.

**결함 2 가드.** `level_zero` 분기를 제거한 뒤 `cargo test --lib doctor::bundle`:

```
bundle_covers_every_declared_feature ... FAILED
arms found: ["cli", "amd", "mock", "furiosa"]
```

이는 정확히 수정 이전 상태입니다. 분기 복원 후 같은 명령: `7 passed; 0 failed`.

| 게이트 | 결과 |
|--------|------|
| `cargo test --lib doctor` | 28 통과, 0 실패 |
| `cargo run --bin all-smi -- doctor` (aarch64 glibc, 기본 피처) | `PASS amd.libamdgpu_top.abi  linked libamdgpu_top 0.11.5`, 이전에 보고하던 0.25.0에서 교정됨. `amd.build.target_env`는 메시지 변경 없이 여전히 통과 |
| `cargo check --no-default-features --features cli` | 통과 |
| `cargo clippy --lib --tests -- -D warnings` | 기본 피처와 `--no-default-features --features cli` 모두 깨끗 |
| `cargo build --features level_zero` | 컴파일됨. 그 빌드의 번들이 `version.txt`에 `features: cli,amd,level_zero` 기록 |
| `cargo fmt --check` | 깨끗 |

**로컬 미검증**: 개발 호스트에 `aarch64-unknown-linux-gnu`만 설치돼 있어 musl과 비 Linux 분기는 확인하지 못했습니다. 두 분기는 이 PR이 변경하지 않았고, 새 상수는 cfg에 의해 양쪽에서 컴파일 제외되면서 가드 테스트는 `test` 아래에서 계속 돕니다.

## 5. 결과 및 후속

- PR #363은 `b92ea0a`로 `main`에 squash merge되었습니다.
- 이슈 #362는 PR의 `Closes #362` 링크로 자동 종료되었습니다.
- 이 PR이 추가한 `level_zero` 분기는 2주 뒤 부분적으로 무의미해졌습니다. #365가 백엔드를 Linux/Windows에서 컴파일 타임 무조건으로 만들고, cargo 피처가 더 이상 그 질문에 답할 수 없게 되면서 보고를 `version.txt`의 별도 `level_zero:` 줄로 옮겼기 때문입니다. 커버리지 테스트는 여전히 피처인 것들에 대해 가치가 남습니다.

---

## 부록: 핵심 키워드

| 키워드 | 설명 | 관련성 |
|-------|------|--------|
| `env!("CARGO_PKG_VERSION")` | **포함하는** 크레이트의 버전을 내는 컴파일 타임 매크로 | 의존성 핀 자리에 잘못 대입된 값 |
| `include_str!` | 컴파일 타임에 파일 텍스트를 임베드 | 테스트가 경로 추측 없이 `Cargo.toml`과 자기 소스를 함께 읽는 방법 |
| 헛된 통과 | 성질이 성립해서가 아니라 아무것도 관측하지 못해서 통과하는 테스트 | 크게 실패하는 경로와 sanity 단언이 막는 것 |
| 정확 핀(`=x.y.z`) | 버전 하나만 허용하는 Cargo 요구사항 | 전사한 상수를 매니페스트와 대조할 수 있게 하는 전제 |
