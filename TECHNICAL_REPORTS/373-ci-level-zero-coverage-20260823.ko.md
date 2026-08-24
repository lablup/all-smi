# 기술 보고서: PR #373 - CI에서 level_zero 백엔드 컴파일, 린트, 테스트

**일자**: 2026-08-23  
**상태**: 완료  
**관련 항목**: PR #373, 참조 이슈 #372 (Part A), 이슈 #364  
**위험 수준**: 낮음 (CI 설정 전용, 소스 변경 없음)

---

## 요약

`level_zero` cargo 피처는 기본 비활성이었고 이를 켜는 CI 잡이 없었으므로, `src/device/readers/intel_gpu_level_zero/` 아래 전부가 한 번도 컴파일되지 않았고, 린트되지 않았으며, 그 테스트는 모든 실행에서 필터로 걸러졌습니다. 이 변경 전에는 `grep -rn "level_zero" .github/workflows/`가 아무것도 반환하지 않았습니다. 모듈에는 통과하는 테스트 49개가 있었고 그중 하나도 실행되지 않았습니다.

PR #373은 #372의 Part A로 CI 커버리지만 다룹니다. Part B인 릴리스 아티팩트에 피처를 싣는 작업은 의도적으로 이 PR에 넣지 않고 #364에 게이트한 채로 두었습니다. 이후 #365가 백엔드를 Linux/Windows에서 무조건으로 만들면서 완전히 대체됐습니다.

---

## 1. 문제 정의

이 구멍은 이미 구체적인 대가를 치렀습니다. #364의 분석은 다섯 근본 원인 중 둘을 어떤 잡도 컴파일하지 않는 `intel_gpu_level_zero/apply.rs`에 귀속시킵니다. 아무도 보고 있지 않을 때 그곳의 결함은 리뷰만으로 잡을 수 없습니다.

한 번도 돌지 않는 테스트 49개를 가진 모듈은 테스트가 하나도 없는 모듈보다 나쁩니다. 그 테스트 수가 인용되는 모든 요약에서 커버리지로 읽히기 때문입니다.

## 2. 변경 요약

| 항목 | 값 |
|------|----|
| 변경 파일 | 1개 (`.github/workflows/ci.yml`) |
| 추가 줄 | 42줄 |
| 삭제 줄 | 1줄 |
| 소스 변경 | 없음 |
| `Cargo.toml` 변경 | 없음 |

### 추가한 단계

| 잡 | 단계 |
|----|------|
| Linux `test` | `cargo test --verbose --features level_zero` |
| Linux `test` | `cargo clippy --features level_zero --all-targets -- -D warnings` |
| `build-check` | 피처를 켠 릴리스 프로파일 빌드, `--locked` 유지 |
| Windows | 기존 호출에 `--features level_zero` 추가 |

## 3. 기술적 선택과 그 이유

### 3.1 새 단계는 대체가 아니라 추가

기본 피처 빌드는 `cargo install all-smi`와 `default-features = false`를 쓰지 않는 모든 다운스트림 크레이트가 해석하는 대상이므로 독자적으로 계속 덮여야 합니다. 기존 단계를 대체했다면 사각지대를 다른 사각지대로 맞바꾼 셈이 됐을 것입니다.

### 3.2 기존 clippy 단계에는 없는 `--all-targets`를 일부러 붙였다

모듈의 테스트가 같은 피처 게이트 뒤에 있으므로, lib과 bin만 린트하면 그것들이 두 번째로 미검사 상태가 됩니다. `cargo fmt --check`는 피처 독립이므로 그대로 두었습니다.

### 3.3 릴리스 빌드에 `--locked`를 유지한 것은 의도적

`level_zero = []`는 어떤 의존성도 활성화하지 않으므로 `Cargo.lock`을 움직여서는 안 됩니다. 그것이 언젠가 사실이 아니게 되면, 더 엄격한 `--frozen`을 쓰며 발견 장소로는 훨씬 나쁜 vendored Debian 빌드가 아니라 여기서 실패합니다.

### 3.4 Windows 단계는 가용할 때의 커버리지이지 보장이 아니다

`src/device/readers/intel_gpu_windows.rs`는 Level Zero 보강을 `cfg(target_os = "windows")`와 피처 양쪽에 게이트하므로, 플래그가 없으면 그 코드는 CI 어디에서도 컴파일되지 않습니다. 이 잡은 `ENABLE_WINDOWS_SERVICE_SMOKE`로 옵트인이므로, 플래그 추가는 러너가 살아 있을 때 도는 것을 개선할 뿐 그렇지 않을 때 아무것도 약속하지 않습니다. 자체 호스팅 러너가 정지 상태여서 내내 `skipping`이었습니다.

## 4. 검증 결과

### 로컬, Windows 11 Pro 26200, 네이티브 `x86_64-pc-windows-msvc`, rustc 1.97.1

```
cargo test --features level_zero --lib intel_gpu_level_zero
test result: ok. 49 passed; 0 failed; 0 ignored; 0 measured; 1467 filtered out

cargo clippy --features level_zero --all-targets        (exit 0)
```

`intel_gpu_level_zero`에서는 지적이 없습니다. 경고 6건이 나오는데 새 단계가 `-D warnings`를 붙이므로 각각이 Linux 러너에 도달할 수 있는지 확인했습니다. 여섯 건 모두 Windows 전용입니다.

| 위치 | Linux에서 경고하지 않는 이유 |
|------|------------------------------|
| `src/api/shutdown.rs:106` | `#[cfg_attr(not(windows), allow(dead_code))]` 보유 |
| `src/device/readers/windows_gpu_perf.rs:119` | Windows 전용 파일, Linux에서 미컴파일 |
| `src/device/readers/windows_gpu_perf/ids.rs:52` | 위와 동일 |
| `src/main.rs:39` (`SocketSetting`) | `src/main.rs:311`의 `#[cfg(unix)]` 블록 안에서 소비 |
| `src/main.rs:322` (`interval`) | `src/main.rs:338`의 `#[cfg(target_os = "linux")]` 블록 안에서 소비 |
| `src/utils/command_timeout.rs:190` | `#[cfg(unix)]` 테스트 두 개를 뒷받침하므로 Linux에서 import가 살아 있음 |

이들은 #367로 추적 중인 기존 네이티브 Windows 지적이며 이 변경과 무관하게 존재합니다.

build-check 갈래가 `--locked`를 유지하므로 락파일 안정성도 직접 확인했습니다.

```
cargo build --release --target x86_64-pc-windows-msvc --locked --features level_zero   -> exit 0, 2m41s
Cargo.lock sha256 before: 6cd3928b31b8fbd079e9917c3817b16b94f15326dbaddb8eda6013683e286b32
Cargo.lock sha256 after:  6cd3928b31b8fbd079e9917c3817b16b94f15326dbaddb8eda6013683e286b32
```

### CI, `7472d23` 위로 리베이스 후 실행 32616550644

새 단계 셋이 모두 성공했고, 테스트 단계의 수치가 이 PR이 닫는 구멍의 크기를 정확히 보여 줍니다.

| 단계 | lib / bin 통과 | 실행된 `intel_gpu_level_zero::` 테스트 |
|------|----------------|----------------------------------------|
| `Run tests` (기존) | 1596 / 1772 | **0** |
| `Run tests (level_zero)` (신규) | 1645 / 1821 | **49** |

각 타겟에서 정확히 +49이며, Windows 로컬에서 통과한 49와 일치합니다. `Run clippy (level_zero)`는 `--all-targets`로 41.18초에 깨끗하게 끝났고, 이는 어떤 잡이든 통합 테스트 타겟을 린트한 첫 사례입니다. #375가 추가한 `tests/library_api_test.rs`도 포함됩니다. `Build with the level_zero feature`는 `--release --locked`로 통과해 피처가 여전히 `Cargo.lock`을 움직이지 않음을 확인했습니다.

`Windows Service Smoke Test`는 `skipping`으로 남았습니다. 자체 호스팅 Windows 러너가 정지 상태라 그 잡에 대한 `--features level_zero` 변경은 CI에서 미검증입니다.

## 5. 결과 및 후속

- PR #373은 `64a8651`로 `main`에 squash merge되었습니다.
- 이 PR이 의도적으로 하지 않은 것: 피처를 배포하는 일. 릴리스 아티팩트, `debian/rules`, 문서는 #372의 Part B와 C이며 #364에 게이트된 채로 두었습니다. `Cargo.toml`도 변경하지 않아 `cargo build`와 라이브러리 소비자에게는 계속 기본 비활성이었습니다.
- Level Zero 코드 내부의 런타임 부재 저하 경로도 실행하지 않았습니다. oneAPI 런타임이 없는 Intel GPU 호스트가 필요합니다.
- **같은 사이클 안에서 대체됨.** #365가 빌드 cfg로 백엔드를 모든 Linux/Windows 빌드에 컴파일해 넣으면서, 여기서 추가한 `--features level_zero` 단계 두 개가 Linux에서 중복이 됐습니다. 이제 평범한 `cargo test`가 모듈에 닿습니다. clippy 쌍의 `--all-targets` 절반이 지속되는 기여였고 기본 clippy 단계로 옮겨져, 이제 lib과 bin만이 아니라 크레이트의 모든 테스트 타겟이 린트됩니다.

---

## 부록: 핵심 키워드

| 키워드 | 설명 | 관련성 |
|-------|------|--------|
| `--all-targets` | lib/bin에 더해 테스트, 벤치, 예제까지 린트 | 피처 게이트 테스트가 처음으로 린트된 이유 |
| `--locked` / `--frozen` | `Cargo.lock`이 바뀌면 실패 / 네트워크 접근도 금지 | 락파일 어긋남이 Debian 빌드가 아니라 CI에서 실패하는 이유 |
| 기본 비활성 피처 | `default` 집합에 없는 cargo 피처 | 테스트 49개를 보이지 않게 만든 조건 |
