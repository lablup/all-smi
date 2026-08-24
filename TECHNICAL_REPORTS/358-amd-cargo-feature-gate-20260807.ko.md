# 기술 보고서: PR #358 - AMD 백엔드를 기본 활성 `amd` cargo 피처 뒤로 이동

**일자**: 2026-08-07  
**상태**: 완료  
**관련 항목**: PR #358, 이슈 #345  
**위험 수준**: 중간 (다운스트림 `default-features = false`의 해석 결과가 바뀜)

---

## 요약

PR #358은 AMD GPU 백엔드를 새 `amd` cargo 피처 뒤로 옮기고 그 피처를 기본 활성으로 두었습니다. `libamdgpu_top`은 필수 의존성이었고, 이것이 `libdrm.so.2`와 `libdrm_amdgpu.so.1`을 무조건 링크하는 `libdrm_amdgpu_sys`를 끌어옵니다. 따라서 all-smi에 의존하는 모든 Linux 바이너리가 둘을 하드 `NEEDED` 항목으로 상속했고, AMD 유저스페이스 DRM 라이브러리가 없는 호스트(배포의 압도적 다수)는 `main` 실행 전 로더 오류로 기동에 실패했습니다. 프로그램은 이를 잡을 수도, "AMD GPU 없음"으로 완만히 저하할 수도 없습니다.

이제 `default-features = false`를 선언한 다운스트림 크레이트는 그 두 항목을 상속하지 않습니다. 이는 매니페스트를 읽어서가 아니라 두 빌드 구성 사이의 `objdump -p ... | grep NEEDED`를 비교해 확인했습니다.

---

## 1. 문제 정의

이 수정이 다루는 실패는 all-smi 코드가 한 줄도 실행되기 전에 일어납니다. 동적 로더는 프로세스 시작 시점에 `NEEDED` 항목을 해석하므로, `libdrm.so.2` 부재는 프로그램이 취할 분기가 없는 하드 기동 실패입니다. 그 층위에는 완만한 저하 수단이 존재하지 않으며, 이것이 평범한 "장치 부재 처리" 문제와 구분되는 지점입니다.

`lablup/backend.ai-go`는 이미 `all-smi = { version = "0.25.0", default-features = false }`를 선언하고 있었으므로, 명시적으로 옵트아웃한 백엔드 때문에 그 항목들을 지고 있었고 기동 실패를 보고했습니다.

## 2. 변경 요약

| 항목 | 값 |
|------|----|
| 변경 파일 | 13개 |
| 추가 줄 | 212줄 |
| 삭제 줄 | 34줄 |
| 확장한 `cfg` 지점 | 12곳 |
| `Cargo.lock` 이동 | 없음 |

### 매니페스트 형태

```toml
[target.'cfg(all(target_os = "linux", not(target_env = "musl")))'.dependencies]
libamdgpu_top = { version = "=0.11.5", optional = true }

[features]
default = ["cli", "amd"]
amd = ["dep:libamdgpu_top"]
```

기존 핀 주석(semver를 어기는 패치 릴리스, 0.11.5의 파일 디스크립터 누수 수정)은 보존하고 확장했습니다. `[features]` 블록은 `furiosa`, `level_zero`와 같은 어조로 `amd`를 문서화하며, 저 둘이 기본 비활성인데 이것만 기본 활성인 이유도 함께 적었습니다.

### 확장한 12개 `cfg` 지점

| 파일 | 지점 |
|------|------|
| `src/device/readers/mod.rs` | `pub mod amd;` |
| `src/device/reader_factory.rs` | `has_amd` import, `amd` import, `AmdGpuReader` push |
| `src/device/platform_detection.rs` | `has_amd`, `detect_amd`, `introspection::detect_amd` 쌍 |
| `src/utils/system.rs` | sudo 권한 블록 두 곳 |
| `src/doctor/checks/amd.rs` | `check_libamdgpu_top`, `check_build_gate` |
| `src/doctor/checks/platform.rs` | `check_runtime` |

## 3. 기술적 선택과 그 이유

### 3.1 기본 활성, 그리고 그것이 패키징 문제를 정리한 방식

기본 활성이면 기존 배포 경로가 바이트 단위로 동일하게 유지되면서 AMD 지원을 유지합니다. glibc 릴리스 바이너리, `cargo install all-smi`, Homebrew, 평범한 `cargo build` 모두입니다. `Cargo.lock`은 움직이지 않습니다. 릴리스 워크플로, 패키징, Homebrew formula에 아무 변경이 필요 없고, 이것이 기본 비활성 대신 기본 활성을 고른 주된 이유입니다.

트레이드오프는 숨기지 않고 명시했습니다. `--no-default-features`는 `cli`도 함께 떨어뜨리므로, CLI는 원하고 AMD는 원하지 않는 소비자는 `default-features = false, features = ["cli"]`가 필요합니다. 이 PR이 손댄 네 문서 모두에 적었습니다.

### 3.2 `libdrm` 런타임 `dlopen`을 택하지 않은 이유

이슈의 옵션 2는 검토 후 이 PR에서는 기각했습니다. `libdrm`은 서드파티 `libamdgpu_top` 크레이트가 링크하고 `src/device/readers/amd.rs`가 그 타입들을 전반적으로 사용하므로, 런타임 로딩은 그 크레이트를 직접 작성한 FFI로 대체한다는 뜻입니다. 훨씬 큰 변경이며 파급 범위도 훨씬 크고, 보고된 실패를 고치는 데 필요하지도 않습니다. **#359**로 별도 등록했고 아직 열려 있습니다.

### 3.3 새로운 구성이 아니다

`libamdgpu_top`은 이미 musl 빌드에서 제외돼 있었고, `release.yml`은 릴리스마다 `all-smi-linux-x86_64-musl`과 `all-smi-linux-aarch64-musl`을 배포합니다. 이 변경은 검증되지 않은 형태를 새로 만드는 것이 아니라 이미 배포 중인 형태를 glibc에서도 도달 가능하게 만듭니다.

### 3.4 부정 여집합에 표시를 남겨 어긋나지 않게 했다

`introspection`의 여집합은 `not(all(target_os = "linux", not(target_env = "musl"), feature = "amd"))`로, 양의 분기의 정확한 여집합입니다. 주석으로 그렇게 표시해 두어, 이후 편집이 `detect_amd`를 조용히 누락하거나 중복시킬 수 없게 했습니다.

이 코드베이스에는 `cfg` 별칭 선례가 없으므로(`cfg_aliases` 빌드 의존성 없음), PR 하나를 위해 새 메커니즘을 도입하는 대신 12개 지점마다 술어를 그대로 적었습니다.

### 3.5 진단 가능성: 진단기가 표현할 수 없던 세 번째 상태

피처가 꺼진 glibc 빌드는 진단기가 표현할 수 없던 상태였고, 그대로 두면 거짓 musl 설명을 보고했을 것입니다. 이제 피처 off 빌드에서 `all-smi doctor`는 다음을 보고합니다.

```
WARN amd.build.target_env   glibc build without the `amd` cargo feature: AMD support compiled out
SKIP amd.libamdgpu_top.abi  libamdgpu_top not linked: built without the `amd` cargo feature
WARN platform.runtime       target aarch64-unknown-linux-gnu (env=gnu), built without the
                            `amd` cargo feature so AMD GPU support is compiled out
```

기본 빌드에서는 셋 다 기존 메시지 그대로 통과합니다. `amd.build.target_env`는 검사 id를 유지하고, 내부 함수 이름만 `check_musl_gate`에서 `check_build_gate`로 바꿨습니다. 이제 독립적인 게이트 두 개를 보고하기 때문입니다. `doctor --bundle`도 피처를 기록하므로 지원 번들이 질문에 직접 답합니다. `features: cli,amd` 대 `features: cli`.

의도적으로 피처를 끈 빌드에서 경고하고 exit code 1을 내는 것은 기존 musl 동작과 정확히 같으므로, 새 규약을 도입하지 않습니다.

## 4. 검증 결과

`aarch64-unknown-linux-gnu`에서 실행했습니다.

### 결정적 검사: `NEEDED` 비교

```
$ cargo build --release && objdump -p target/release/all-smi | grep NEEDED
  NEEDED  libdrm.so.2
  NEEDED  libdrm_amdgpu.so.1
  NEEDED  libgcc_s.so.1
  NEEDED  libm.so.6
  NEEDED  libc.so.6
  NEEDED  ld-linux-aarch64.so.1

$ cargo build --release --no-default-features --features cli && objdump -p target/release/all-smi | grep NEEDED
  NEEDED  libgcc_s.so.1
  NEEDED  libm.so.6
  NEEDED  libc.so.6
  NEEDED  ld-linux-aarch64.so.1
```

`libdrm` 항목 둘이 사라졌고 나머지는 그대로입니다. 측정 구성이 `--no-default-features --features cli`인 이유는 `--no-default-features`만으로는 `cli`가, 따라서 바이너리가 사라지기 때문입니다.

### 의존성 해석

| 명령 | 결과 |
|------|------|
| `cargo tree -e normal -i libamdgpu_top` | 존재, `all-smi`가 유일한 의존자 |
| 위와 동일, `--no-default-features` | 패키지 없음 (통과 조건) |
| 위와 동일, `--no-default-features --features cli` | 패키지 없음 |
| `cargo tree -e normal --target x86_64-pc-windows-msvc` | exit 0, 0건 |
| 위와 동일, `--target aarch64-apple-darwin` | exit 0, 0건 |
| 위와 동일, `--target x86_64-unknown-linux-musl` | exit 0, 0건 |

크로스 타겟 세 건은 의존성이 선언되지 않은 타겟에서 `amd`가 켜져 있어도 깨끗하게 해석됨을 확인해 주고, musl 결과는 기존 musl 제외가 여전히 유효함을 확인해 줍니다.

### 빌드와 린트

`cargo check`는 기본 피처, `--no-default-features`, `--no-default-features --features cli` 모두에서 통과합니다. `cargo clippy --lib --tests -- -D warnings`는 기본 구성과 피처 off 구성 모두에서 통과합니다. 비활성 피처 뒤에서 dead-code나 unused-import 파생 경고가 나타나지 않았고, 포괄적인 `#[allow]`를 추가하지 않았습니다.

### CI 가드

기존 `build-check` 잡에 `--no-default-features --features cli`로 빌드한 뒤 `NEEDED` 항목 중 `libdrm`에 일치하는 것이 있으면 실패하는 회귀 가드를 추가했습니다.

## 5. 결과 및 후속

- PR #358은 `7320e5c`로 `main`에 squash merge되었습니다.
- 이슈 #345는 PR의 `Closes #345` 링크로 자동 종료되었습니다.
- 문서는 `README.md`, `DEVELOPERS.md`, `docs/ARCHITECTURE.md`, `docs/LIB_mode.md`를 갱신했습니다. `docs/ARCHITECTURE.md`에는 `default = []`라고 주장하는 낙후된 `[features]` 블록이 있었고 이제 `Cargo.toml`과 일치합니다.
- 이 변경은 v0.26.0에 다운스트림 소비자에 대한 동작 변경으로 나갔습니다. `default-features = false`는 이제 CLI와 함께 AMD 지원도 떨어뜨립니다.
- **#359는 열려 있습니다**: 런타임 `dlopen`으로 옵트아웃 소비자의 AMD 감지를 복원하는 것으로, 이 PR이 의도적으로 맡지 않은 부분입니다. `priority:high`입니다.
- 이 PR을 리뷰하다 드러난 진단기 보고 결함 두 건은 PR #363이 이어받았습니다.

---

## 부록: 핵심 키워드

| 키워드 | 설명 | 관련성 |
|-------|------|--------|
| `NEEDED` 항목 | 필요한 공유 라이브러리를 지정하는 ELF 동적 섹션 레코드 | `libdrm` 부재가 `main` 이전에 치명적이었던 바로 그 원인 |
| `dep:` 접두사 | 암시적 피처를 노출하지 않고 선택적 의존성을 활성화하는 Cargo 문법 | `amd = ["dep:libamdgpu_top"]`가 피처 이름 중복을 피하는 방법 |
| 선택적 의존성 | 피처가 활성화할 때만 컴파일되는 매니페스트 의존성 | 항목들이 사라질 수 있게 하는 메커니즘 |
| `default-features = false` | 크레이트 기본 피처 집합에 대한 다운스트림 옵트아웃 | 소비자가 `libdrm`을 떼어 내려면 설정하는 것 |
