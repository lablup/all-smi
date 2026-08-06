# 기술 보고서: PR #320 - feat(service): run API mode as a Windows service

**작성일**: 2026-08-05
**상태**: 타깃에 대해 타입 검사와 린트만 통과함. 실행되거나 링크된 적은 없음(8절 참고)
**언어**: Rust, YAML (GitHub Actions), PowerShell
**위험도**: 검증 없이 배포하면 High. 구현 환경(macOS)에는 윈도우 머신도, 서비스 제어 관리자도, MSVC 링커도 없어 런타임 수용 기준이 전부 미검증임

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

PR #320은 PR #319가 `NotSupported`로 남겨둔 `service_cmd::backend()`의 윈도우 분기를 구현해서, 로그인한 사용자 없이도 윈도우 모니터링 노드가 부팅 시점부터 지표를 내보낼 수 있게 한다. PR #319가 정의한 크로스 플랫폼 계약대로 `cfg` 분기 딱 하나만 교체하고, 새 `ServiceError` 변형도 추가하지 않는다. 다른 등록 경로 아래 존재하는 서비스는 리눅스 백엔드가 마커 없는 유닛 파일에 쓰는 것과 같은 변형 `Conflict`를 재사용하고, 기존 `uninstall_forced` 훅을 통해 `--force`가 이를 풀어준다.

이 보고서의 핵심 기술 문제는 SCM 연동 자체가 아니라, 그 무엇도 실행할 수 없는 호스트에서 이를 어떻게 검증했는가다. `cargo check --target x86_64-pc-windows-msvc`는 이 PR의 코드에 한 줄도 도달하기 전에 실제 크레이트에서 실패한다. `zstd-sys`의 빌드 스크립트가 윈도우 헤더 없이 `cc --target=x86_64-pc-windows-msvc`를 호출하다 `string.h`를 찾지 못해 죽는다. 우회책은 실제 `service_cmd`, `common`, `cli`, `cli_service`, `utils::command` 소스를 `#[path]`로 끌어들이고, 실패하는 의존성 트리를 끌고 오는 모듈 두 개(`api`, `device`)만 스텁으로 대체하는 독립 프로브 크레이트다. 이 프로브는 라이브러리와 바이너리 대상 둘 다로 짓는다. 크레이트의 죽은 코드 분석이 대상별로 따로 돌아서, `pub` 아이템이 라이브러리 대상에서는 살아있어도 바이너리의 사설 모듈 루트 뒤에서는 실제로 참조되지 않을 수 있기 때문이다. PR #319 자신의 프로브가 정확히 이 틈에 걸렸었고, 이 PR은 그로부터 확장된 방식을 처음부터 적용했다. 프로브의 커버리지는 가정하지 않고 검증했다. `scm_backend.rs`, `scm_host.rs`, `scm_log.rs` 각각에 고의로 타입 오류를 주입했더니 매번 프로브를 통해 컴파일 오류가 났고, 이 PR이 배포되기 전에 프로브가 실제 결함 두 개를 잡았다. `raw_code`/`describe`가 사설인데 모듈 경계를 넘어 쓰이고 있었고, `build_service_info`가 clippy의 `ptr_arg` 린트가 요구하는 `&Path` 대신 `&PathBuf`를 받고 있었다. 결론은 이렇다. 윈도우 백엔드의 모든 줄이 실제 타깃에 대해 타입 검사와 린트를 통과했다. 그중 어느 것도 실행된 적은 없고, 이 호스트에는 MSVC 링커가 없어 프로브 자체도 링크되지 않으므로 링크 시점 문제는 여전히 완전히 가능하다.

이 PR은 윈도우 전용이 전혀 아닌 크로스 플랫폼 수정도 함께 가져온다. `shutdown_signal`(`src/api/server.rs`에서 새 `src/api/shutdown.rs`로 옮김)이 외부에서 트리거할 수 있는 소스를 얻는다. SCM이 Stop 컨트롤을 관찰할 OS 시그널이 없는 전용 핸들러 스레드에서 전달하기 때문이다. 이게 없었다면 핸들러는 `std::process::exit`를 호출해야 했을 것이고, 에너지 WAL 플러시가 좌초되고 재시작 사이 카운터 단조성이 깨졌을 것이다. 이는 PR #321이 나중에 macOS에서 다른 코드 경로를 통해 독립적으로 발견하고 고친 것과 같은 부류의 결함이다. 새 `Latch` 원시 타입(`tokio::sync::watch` 채널 위의 일방향 불리언 게이트, `send`가 아니라 `send_replace`를 씀)이 어떤 리스너도 아직 구독하지 않은 상태에서 도착한 Stop이 이후 모든 대기자에 대해서도 계속 풀리게 만드는 장치다. 이는 이 호스트에서 테스트했고 윈도우 전용도 아니다. 전체 규모는 파일 23개, +3351/-92, 커밋 8개이며 #311을 닫는다.

---

## 1. 문제 정의

### 1.1 배경

PR #319는 크로스 플랫폼 `all-smi service` 프레임워크를 확립했다. `ServiceBackend` 트레이트, `Scope`, `ServiceError`, 종료 코드 계약과 함께 리눅스 systemd 구현체를, macOS와 윈도우에는 각자의 추적 이슈를 이름 붙인 명시적 `NotSupported` 분기를 두었다. 이슈 #311이 윈도우 후속이다. `sc.exe`를 셸아웃하는 대신 `windows-service` 크레이트로 SCM 백엔드를 구현하고, NVML·WMI 온도 존·벤더 GPU 도구가 계속 닿을 수 있도록 프로세스를 LocalSystem 서비스로 등록하고, SCM 아래에서는 stdout이 사라지므로 서비스에게 로그를 남길 곳을 마련하는 것이다.

### 1.2 기존 문제점

- **문제 1 (이 환경에서 윈도우 전용 코드를 빌드하거나 린트할 방법이 없음)**: 실제 크레이트에 대한 `cargo check --target x86_64-pc-windows-msvc`는 이 PR의 코드에 도달하기 전에 `zstd-sys`의 빌드 스크립트 안에서 실패한다. 그 스크립트가 윈도우 타깃으로 C 컴파일러를 호출하는데 macOS에는 윈도우 C 헤더가 없기 때문이다.
- **문제 2 (SCM Stop은 걸어둘 시그널이 없음)**: `shutdown_signal`은 오직 `ctrl_c`와, 유닉스에서는 `SIGTERM`에만 대고 select했다. SCM은 Stop을 전용 컨트롤 핸들러 스레드의 콜백으로 전달하는데, 둘 다 아니다.
- **문제 3 (SCM 아래에서 stdout에 닿을 수 없음)**: 서비스 프로세스에는 콘솔이 없다. SCM 아래에서 stdout이나 stderr에 쓰는 것은 그냥 사라지므로, 시작 실패를 진단하려면 완전히 다른 싱크가 필요하다.
- **문제 4 (LocalSystem의 `%APPDATA%`는 운영자가 편집할 수 있는 경로가 아님)**: 기존 사용자별 설정 후보들은 LocalSystem 아래에서 `C:\Windows\System32\config\systemprofile\AppData\Roaming`으로 풀리는데, 이는 운영자가 절대 손으로 편집하지 않을 경로다.

### 1.3 위험성

| 위험 | 영향 | 발생 가능성 |
|---|---|---|
| 윈도우 백엔드가 컴파일과 린트는 깨끗하지만 어느 검사도 잡지 못하는 방식으로 런타임에 실패함 | High: 런타임 수용 기준(설치, 시작, 정지 시맨틱스, 크래시 복구, 로깅, 설정 탐색) 전부가 이 환경에서는 미검증 | 게이트된 CI 잡이 자체 호스팅 윈도우 러너에서 실행될 때까지 확실함(8절) |
| 프로브 크레이트의 스텁이 실제 `api`/`device` 모듈과 달라져 실제 컴파일 오류를 가림 | Medium: 프로브의 가치 전체가 스텁이 실제 크레이트의 스텁 아닌 모듈을 충실히 대변하는가에 달림 | 실제 타입 오류를 주입해 프로브가 이를 잡는지 확인함으로써 완화(2.1절) |
| SCM의 "리스너 구독 전 Stop" 경합이 종료 신호를 조용히 놓침 | Medium: Stop 요청이 유실되어 프로세스가 예상 정지 시점을 넘겨 계속 돌거나, WAL 플러시가 비정상 종료와 경합함 | `Latch`의 `send_replace`와 `wait()`의 사전 확인으로 닫힘(3.2절), 이 호스트에서 테스트됨 |
| 미래의 의존성 업그레이드가 이 PR과 무관한 이유로 `zstd-sys` 빌드 실패를 되살려 `cargo check --target x86_64-pc-windows-msvc`가 조용히 회귀함 | Low. 다만 윈도우 CI 밖에서 이 백엔드에 대한 유일한 검증 신호를 없앨 것 | 이 PR에서 완화하지 않음. 지속되는 제약으로 기록해 둠 |

---

## 2. 기술적 검토 사항

### 2.1 빌드할 수 없는 머신에서 윈도우 전용 Rust를 검증하기

**벽.** 실제 `all-smi` 크레이트에 대한 `cargo check --target x86_64-pc-windows-msvc`는 이 PR 자신의 코드에서 실패하지 않는다. `zstd-sys`의 빌드 스크립트 안에서 실패하는데, 이 스크립트가 `--target=x86_64-pc-windows-msvc`로 `cc`를 호출하지만 호스트에 윈도우 시스템 헤더가 없어 `fatal error: 'string.h' file not found`를 낸다. macOS에서 리눅스 백엔드를 교차 확인하려던 PR #319가 부딪힌 것과 같은 부류의 벽이고, 다만 빠진 툴체인 구성 요소가 다를 뿐이다.

**우회책.** 독립 프로브 크레이트가 실제 `service_cmd`, `common`, `cli`, `cli_service`, `utils::command` 소스 파일을 `#[path]`로 직접 끌어들이고, 교차 컴파일을 실패하게 만드는 의존성 하위 트리를 끌고 오는 모듈 딱 둘만 스텁으로 대체한다. `crate::api`(웹 서버와 `zstd-sys`를 포함한 전이 의존성을 끌고 옴)와 `crate::device`(플랫폼 GPU/CPU 리더)다. 나머지는 전부 실제 소스로 컴파일된다.

**프로브가 라이브러리와 바이너리 대상 둘 다 필요한 이유.** Rust의 죽은 코드 분석은 컴파일 대상별로 돌아가고, 이 크레이트의 모듈 트리는 라이브러리로 한 번, 바이너리로 한 번, 두 번 컴파일된다. `pub` 아이템은 라이브러리 대상에서는 외부 소비자가 무엇이든 될 수 있어 자동으로 도달 가능하다고 간주되지만, 같은 아이템이 바이너리의 사설 모듈 루트 뒤에서는 실제로 죽어있을 수 있다. PR #319의 리눅스 프로브도 처음에는(라이브러리 대상만) 정확히 이 사각지대를 갖고 있었고, 실제 CI 실패가 그 틈을 드러낸 뒤 바이너리 대상까지 확장됐다(PR #319 보고서 2.2절 참고). 이 PR의 윈도우 프로브는 같은 이유로 처음부터 두 대상 모두로 지었다.

**커버리지는 가정하지 않고 검증했다.** `scm_backend.rs`, `scm_host.rs`, `scm_log.rs` 각각에 고의로 타입 오류를 주입했고, 매번 프로브를 통해 컴파일 오류가 났다. 이것이 "이 프로브는 실수를 잡아야 한다"와 "이 프로브는 실제로 실수를 잡는다"의 차이이고, `service_cmd/mod.rs`의 주석에 구체적으로 기록해 두었다. 이 세 파일을 컴파일하는 CI 잡이 없어서, 다음에 이 파일들을 건드릴 사람이 그러지 않으면 자기 작업을 확인할 방법이 없기 때문이다.

**프로브는 이 PR이 배포되기 전에 실제 결함 두 개를 잡았다.** 가상이 아니다. `raw_code`와 `describe`가 사설인데 모듈 경계를 넘어 쓰이고 있었고, `build_service_info`가 clippy의 `ptr_arg` 린트가 요구하는 `&Path` 대신 `&PathBuf`를 받고 있었다. 둘 다 프로브가 존재했기 때문에 잡혔지, 누군가 특별히 윈도우 관례를 염두에 두고 코드를 들여다봐서가 아니었다.

**이것으로 증명되지 않는 것.** 윈도우 백엔드의 모든 줄이 `x86_64-pc-windows-msvc`에 대해 타입 검사와 린트를 통과했다. 그중 어느 것도 실행된 적은 없고, 이 호스트에는 MSVC 링커가 없어 프로브 자체도 링크되지 않는다. 그래서 링크 시점 문제(누락된 심벌, `windows-service` FFI 경계의 ABI 불일치)는 완전히 가능한 채로 남아 있고, 이 검증 방법이 잡을 수 있는 범위 밖에 있음을 명시적으로 밝혀둔다.

### 2.2 호환성 및 의존성

- **Breaking Changes**: 없다. `backend()`의 윈도우 분기가 이 PR이 교체하는 유일한 `cfg` 분기이고, macOS 분기(이 PR 시점에는 여전히 미구현인 이슈 #310을 가리킴)와 PR #319의 리눅스 백엔드는 손대지 않는다.
- **새로운 의존성**: 둘 다 `[target.'cfg(windows)'.dependencies]` 아래에 범위가 한정되어 다른 어떤 대상도 끌고 오지 않는다. `windows-service = "0.8.1"`(이 PR 시점 최신 릴리스, MIT OR Apache-2.0, `rust-version` 1.71.0으로 프로젝트의 MSRV 1.96 대비 여유 있음), `tracing-appender = "0.2.5"`(MIT, `rust-version` 1.63.0).
- **호환성**: `%PROGRAMDATA%\all-smi\config.toml`이 `candidate_config_paths()`의 티어 2에 기존 `%APPDATA%` 후보 뒤로 추가되며, 리눅스에서 `/etc/all-smi/config.toml`에 대해 PR #319가 확립한 패턴을 그대로 따라 추가적이다.

### 2.3 코드 품질

추가된 단위 테스트: `SERVICE_STATUS` 상태와 시작 유형 매핑(대기 상태와 낡은 PID 사례 포함), 실행 인자 매핑, Win32 오류 변환(권한 상승 거부와 `--user` 거부 포함), SCM 명령줄 파싱(따옴표 있음, 없음, 종료되지 않음, `\\?\` 리터럴 접두사 형태)과 설치 멱등성 가드 뒤의 대소문자 무시 경로 비교, `%PROGRAMDATA%` 경로 구성, 그리고 새 `Latch` 종료 소스(이전 트리거, 이후 트리거에서 풀리고 모든 대기자가 동시에 풀리며, 결정적으로 어떤 소스도 발동하지 않은 동안에는 풀리지 않음).

이 호스트에서 `cargo test --lib`: 1260개 통과, 3개 무시(`#[cfg(windows)]` 모듈. 이 호스트에서 모듈별 `cfg(test)` 게이팅에 따라 컴파일은 되지만 윈도우 전용 어서션을 실행하지는 않음). `cargo fmt --check`, `cargo clippy --lib --tests -- -D warnings`, `cargo check --all-targets` 모두 호스트 툴체인에서 클린.

이 PR에 특정된 죽은 코드 세부 사항 하나: `service_cmd/mod.rs`의 포괄적 `#[cfg_attr(not(target_os = "linux"), allow(dead_code))]`는 이 PR 이전부터 있었는데, 이 PR이 추가하는 윈도우 전용 트리 전체에 걸쳐 미사용 아이템을 가렸을 것이다. 새 모듈마다 내부 `#![warn(dead_code)]`로 린트를 스스로 다시 켜서, 아직 윈도우 구현이 없던 코드를 위한 포괄적 억제를 윈도우 백엔드가 물려받지 않게 했다.

---

## 3. 기술적 선택과 그 이유

### 3.1 프로세스 토큰의 권한 상승 플래그 대신 실제 권한 요구사항을 프로브함

**컨텍스트**: 이슈 #311은 프로세스 토큰의 권한 상승 플래그로 권한 상승을 감지하자고 제안했다. 흔한 윈도우 패턴이다.

**채택한 접근, 이슈에서 의도적으로 벗어남**: 각 동작이 실제로 필요한 SCM 핸들만 정확히 열고, 그 결과 나오는 `ERROR_ACCESS_DENIED`를 "관리자 권한으로 실행" 메시지가 붙은 `ServiceError::NeedsElevation`으로 옮긴다. 토큰 플래그를 미리 확인하는 대신이다.

| 선택지 | 장점 | 단점 |
|---|---|---|
| 프로세스 토큰 권한 상승 플래그(이슈의 제안) | 관례적인 윈도우 패턴. 값싸고 단일한 확인 | 실제 요구사항이 아니라 그 대리물을 테스트함. 이 개발 호스트에서는 단 한 번도 실행할 수 없는 unsafe FFI 프로브가 필요함 |
| **채택: 동작이 필요로 하는 SCM 핸들을 열고 `ERROR_ACCESS_DENIED`를 변환** | 실제로 필요한 능력을 테스트함. 권한 상승이 필요 없는 `status`는 필요 없는 권한을 절대 요청하지 않으므로 상승 없이도 계속 동작함. 눈감고 새 unsafe 프로브를 쓸 필요 없음 | 동작마다 올바른 핸들을 요청하는 세심함이 필요함. 여기서 실수하면 잘못된 권한 상승 확인이 아니라 잘못된 오류 메시지로 드러남 |

**선택 이유**: 권한 상승 플래그 확인은 정확히 이 PR이 "컴파일된다" 이상으로 검증할 수 없었을 코드였을 것이다. 실행 경로가 전혀 없는 unsafe FFI이기 때문이다. 대리물이 아니라 실제 SCM 권한을 테스트하는 쪽이 원칙적으로도 더 정확하고(플래그는 참이어도 특정 핸들 열기가 다른 이유로 여전히 실패할 수 있다), 실행해본 적 없이 배포하기에도 더 안전하다.

### 3.2 절대 놓쳐서는 안 되는 신호를 위해 `tokio::sync::Notify` 대신 `watch` 기반 `Latch`

**컨텍스트**: `all-smi api`는 이전에는 오직 두 소스, `Ctrl+C`와 `SIGTERM`에서만 종료를 알았다. SCM에는 둘 다 없다. Stop 컨트롤은 기다릴 async 시그널이 없는 핸들러 스레드에 도착하고, API 서버가 리스너를 스폰하기도 전에 도착할 수 있다.

| 선택지 | 장점 | 단점 |
|---|---|---|
| `tokio::sync::Notify` | 가볍고 정확히 이런 종류의 외부 웨이크업을 위해 설계됨 | `notify_one`은 저장된 단일 허가를 먼저 구독한 대기자에게 넘길 뿐이고, `notify_waiters`는 어떤 대기자도 구독하기 전에 발동하면 완전히 유실됨. 정확히 SCM의 이른 Stop 시나리오임 |
| **채택: `tokio::sync::watch` 위의 일방향 불리언 래치, `Clone` 가능, `send_replace`로 트리거** | 트리거 이후에 생긴 것을 포함해 모든 대기자가 풀림. `send_replace`는 모든 수신자가 드롭됐더라도 항상 값을 씀. 그래서 어떤 구독보다 앞선 트리거도 조용히 유실되지 않음 | `Notify`보다 약간 무거움. `subscribe()`가 현재 값을 이미 본 것으로 표시하므로 `wait()`에서 첫 `.changed()` 대기 전에 사전 읽기가 필요함 |
| `std::sync::atomic::AtomicBool`과 수동 폴링 | 가능한 가장 단순한 원시 타입 | async 웨이크업이 전혀 없음. 깔끔한 `select!` 분기 대신 폴링 루프가 필요함 |

**선택 이유**: `watch::Sender`의 `send`는 모든 수신자가 드롭되면 실패하고, 그 경로에서는 저장된 값을 건드리지 않는다. 정확히 어떤 리스너 태스크도 구독하기 전에 도착한 Stop 컨트롤을 조용히 잃어버리는 실패 양상이다. `send_replace`는 수신자 상태와 무관하게 항상 값을 쓴다. 거울상 보증, 즉 `wait()`가 트리거가 이미 발동한 뒤에 생긴 "늦은" 구독자에 대해서도 즉시 풀려야 한다는 것은 `wait()`에 "첫 대기 전에 현재 값을 읽는" 단계를 명시적으로 요구했다. `watch::Receiver`의 `subscribe()`가 구독 시점의 값을 이미 본 것으로 표시하기 때문에, 그러지 않으면 트리거 이후의 `.changed()` 대기가 영원히 끝나지 않았을 것이다.

### 3.3 윈도우 전용 변형을 추가하는 대신 남의 등록 서비스에 `ServiceError::Conflict`를 재사용

**컨텍스트**: SCM은 systemd 백엔드가 유닛 파일에 찍는 관리 마커 주석에 해당하는 것을 제공하지 않는다. 텍스트 유닛 파일이 허용하는 방식으로 정체성 마커를 숨겨둘 곳이 윈도우 서비스 등록에는 없다.

**채택한 접근**: 대신 등록된 바이너리 경로를 정체성 확인으로 삼는다. 이름이 `all-smi`인데 등록된 실행 파일 경로가 `current_exe()`와 일치하지 않는 서비스는, 리눅스 백엔드가 마커 주석 없는 유닛 파일을 다루는 것과 같은 방식으로 취급되어 `ServiceError::Conflict`를 낸다. PR #319가 이미 정의해둔 `uninstall_forced` 훅을 통해 `--force`가 거부를 풀어준다.

**선택 이유**: 새 `ServiceError` 변형을 추가하지 않으므로 PR #319가 확립한 크로스 플랫폼 계약이 명시된 그대로 유지되면서도, SCM이 실제로 표현할 수 있는 것(찍힌 주석이 아니라 등록 경로 비교)에 맞춰 마커 주석 가드와 기능적으로 동등한 메커니즘을 윈도우에도 준다.

---

## 4. 구현 상세

### 4.1 아키텍처 변경

```
[#319의 크로스 플랫폼 계약, 변경 없음]

service_cmd::backend()
    #[cfg(target_os = "windows")]  ->  ScmBackend   (이전: NotSupported, #311로 추적)

[신규: SCM 백엔드 자체의 계층 구조]

ScmBackend (impl ServiceBackend)
    install/uninstall/start/stop/restart/status
        -> scm.rs        순수 매핑: SERVICE_STATUS <-> ServiceStatus, Win32 오류 변환,
                          멱등성 확인용 명령줄 파싱 (cfg(any(windows, test)))
        -> scm_backend.rs  windows-service 크레이트 호출 자체 (cfg(windows) 전용)

all-smi service run  (숨겨진 CLI 동작, backend()보다 먼저 디스패치됨)
    -> scm_host.rs    service_dispatcher::start, 컨트롤 핸들러, START_PENDING -> RUNNING
                      -> STOP_PENDING -> STOPPED, Tokio 런타임을 소유함
    -> scm_log.rs     %PROGRAMDATA%\all-smi\logs 아래 롤링 파일 로깅(일 단위, 14개 보관)

[크로스 플랫폼 종료 배관, 윈도우 전용 아님]

src/api/latch.rs      Latch: watch 기반 일방향 불리언 게이트, Clone, send_replace
src/api/shutdown.rs   shutdown_signal()이 이제 다음에 대고 select함: ctrl_c | SIGTERM(유닉스) | Latch::wait()
src/api/server.rs     각 리스너가 성공적 바인드 이후 mark_serving()을 호출함.
                      run_api_mode는 tracing 구독자에 init() 대신 try_init()을 씀
                      (서비스 호스트가 파일 구독자를 먼저 설치함)
```

### 4.2 주요 코드 변경

**파일: `src/api/latch.rs` (SCM Stop 경로가 의존하는 원시 타입)**
```rust
pub struct Latch { tx: Arc<watch::Sender<bool>> }

impl Latch {
    pub fn trigger(&self) {
        // `send`는 모든 수신자가 드롭되면 실패하고 그 경로에서 저장된
        // 값을 건드리지 않는데, 이는 어떤 리스너 태스크도 구독하기 전에
        // 도착한 Stop 컨트롤을 조용히 잃어버리는 것이다.
        // `send_replace`는 항상 값을 쓴다.
        self.tx.send_replace(true);
    }

    pub async fn wait(&self) {
        let mut rx = self.tx.subscribe();
        // `subscribe`는 현재 값을 이미 본 것으로 표시하므로, 이 호출보다
        // 앞서 일어난 트리거는 변경으로 나타나지 않는다.
        if *rx.borrow() {
            return;
        }
        while rx.changed().await.is_ok() {
            if *rx.borrow_and_update() {
                return;
            }
        }
        std::future::pending::<()>().await
    }
}
```
**변경 이유**: 이것이 SCM 자신의 핸들러 스레드에서, 완전히 async 코드 바깥에서 전달되는 Stop 컨트롤이 `run_api_mode`의 정상 종료 경로에 도달하게 만드는 메커니즘이고, 리스너가 언제 구독하는지와 Stop이 언제 도착하는지의 순서와 무관하게 신호를 절대 잃지 않는다.

**파일: `src/api/server.rs` (서비스 호스트에 필요한 준비 상태와 구독자 충돌 수정)**
```rust
// init 대신 try_init: 윈도우 서비스 호스트가 먼저 파일 기반 tracing
// 구독자를 설치한다. SCM 아래에서는 stdout이 사라지기 때문이다.
// init()은 두 번째 등록에서 패닉한다.
if tracing_subscriber::registry()
    .with(...)
    .with(tracing_subscriber::fmt::layer())
    .try_init()
    .is_err()
{
    tracing::debug!("a tracing subscriber is already installed; keeping the host's");
}
...
mark_serving();  // 각 리스너의 성공적 바인드 이후
```
**변경 이유**: SCM은 포트가 실제로 연결을 받아들이기 시작한 뒤에야 `SERVICE_RUNNING`을 보고해야 하고, 서비스 호스트는 다른 모든 진입점도 함께 쓰는 공유 `run_api_mode` 경로를 깨뜨리지 않고 자기 로그 싱크를 먼저 설치할 수 있어야 한다.

### 4.3 데이터 모델 변경

와이어 포맷이나 지표 변경은 없다. `%PROGRAMDATA%\all-smi\config.toml`은 `candidate_config_paths()`의 새 티어 2 후보로, 기존 `%APPDATA%` 후보 뒤에 추가된다. `all-smi config path`와 `--help` 블록 둘 다 이미 같은 함수에서 읽어오므로 자동으로 반영된다.

---

## 5. 학습 포인트

### 5.1 실제 크레이트를 빌드할 수 없는 교차 컴파일 타깃도 실제 코드를 검증할 방법이 여전히 필요하다

**개념**: 실제 의존성 트리가 특정 타깃으로 교차 컴파일되지 않을 때(여기서는 `zstd-sys`의 빌드 스크립트가 macOS에 없는 윈도우 헤더를 요구함), 선택지는 "완전히 검증한다" 대 "아무것도 검증하지 않는다"가 아니다. 실제 소스 파일을 `#[path]`로 포함하고 깨진 의존성 하위 트리를 끌고 오는 특정 모듈만 스텁으로 대체하는 독립 크레이트는, 스텁 모듈 자체의 정확성이 이 방법으로는 검증되지 않는다는 대가로 나머지 전부에 대한 타입 검사와 린트를 유지한다.

**이 PR에서의 적용**: `api`와 `device`는 `zstd-sys`를 전이적으로 끌고 오는 주범이라 스텁으로 대체됐다. 이 PR의 실제 주제인 `service_cmd`, `common`, `cli`, `cli_service`, `utils::command`는 프로브를 통해 실제 소스로 컴파일된다.

### 5.2 죽은 코드 분석은 컴파일 대상별이고, 프로브 크레이트가 신뢰할 만하려면 그 형태를 그대로 반영해야 한다

**개념**: 이는 PR #319 보고서가 리눅스 프로브에 대해 기록한 것과 같은 교훈이고, 윈도우 프로브에 대해서는 독립적으로 다시 발견됐다기보다는 이번에는 처음부터 적용됐다. `pub` 아이템은 라이브러리 대상에서는 자동으로 도달 가능한 것으로 친다. 같은 아이템이 바이너리의 사설 모듈 트리 뒤에서는 실제로 참조되지 않을 수 있다. 라이브러리 대상만 짓는 프로브는 라이브러리 대상의 사각지대를 그대로 물려받는다.

**이 PR에서의 적용**: 윈도우 프로브는 처음부터 라이브러리와 바이너리 대상 둘 다로 지어졌고, PR #319 자신의 프로브가 실제 CI 실패로 그 틈이 드러난 뒤에야 확장을 거쳐야 했던 것을 피했다.

### 5.3 테스트 하네스가 실수를 잡는다는 것을 확인하는 일은 하네스를 작성하는 것과는 다르다

**개념**: 코드를 타입 검사하고 린트하는 프로브 크레이트는, 올바른 코드를 받아들인다는 증거가 아니라 실제로 깨진 코드를 거부한다는 증거만큼만 신뢰할 만하다. 둘은 같은 주장이 아니다.

**이 PR에서의 적용**: 세 윈도우 전용 모듈 각각에 고의로 타입 오류를 주입해 매번 프로브가 컴파일 오류를 내는지 확인한 것이, "이 프로브는 실수를 잡아야 한다"를 "이 프로브는 실제로 실수를 잡는다"로 바꾸는 지점이다. 그 과정에서 프로브가 잡은 실제 결함 두 개(가시성 실수, `ptr_arg` 린트 위반)는 이것이 가상의 연습이 아니었다는 독립적인 확인이다.

---

## 6. 추가 학습

### 핵심 용어

| 용어 | 설명 | 관련성 |
|---|---|---|
| `windows-service` 크레이트 | `sc.exe`를 셸아웃하는 대신 쓰는 Win32 서비스 제어 관리자 API의 Rust 바인딩 | 이 백엔드가 근거로 삼는 의존성 |
| `zstd-sys` 빌드 스크립트 교차 컴파일 | C 의존성의 빌드 스크립트가 윈도우가 아닌 호스트에서 `x86_64-pc-windows-msvc`로 교차 컴파일되지 못함 | 프로브 크레이트를 필요하게 만든 벽(2.1절) |
| 대상별 죽은 코드 분석 | 크레이트의 라이브러리와 바이너리 컴파일 대상에 대해 따로 평가되는 Rust의 `dead_code` 린트 | 프로브가 두 종류 대상 모두 필요한 이유(3절, PR #319 보고서 2.2절) |
| `tokio::sync::watch` 대 `Notify` | 유실된 웨이크업에 대한 동작이 다른 두 async 알림 원시 타입 | `Latch`가 `watch` 위에 지어진 이유(3.2절) |
| `send_replace` | 살아있는 수신자가 없어도 항상 값을 쓰는 `watch::Sender` 메서드 | 이른 SCM Stop이 조용히 유실되는 것을 막는 장치 |
| `ERROR_ACCESS_DENIED` 변환 | Win32 오류 코드를 `ServiceError::NeedsElevation`으로 매핑 | 토큰 플래그 프로브 대신 채택한 권한 상승 전략(3.1절) |

### 관련 기술/프레임워크

- Win32 서비스 제어 관리자 API와 그 위의 `windows-service` 크레이트 추상화.
- 원샷 래치 시맨틱스에서 `Notify`보다 유실된 웨이크업에 강한 대안으로서의 `tokio::sync::watch` 채널.
- Rust의 컴파일 대상별 죽은 코드 분석과, 라이브러리·바이너리 대상을 동시에 배포하는 모든 크레이트에 대한 그 함의.

### 관련 PR/이슈

- 이슈 #311: 이 PR이 닫는 이슈.
- PR #319(이슈 #309): 이 PR이 변경 없이 구현하는 `ServiceBackend` 계약을 정의함. 그 자신의 리눅스 프로브 크레이트가 이 PR의 프로브가 처음부터 피하도록 지어진 대상별 죽은 코드 틈에 먼저 걸렸었음.
- PR #321(이슈 #310): macOS launchd 백엔드. SCM의 Stop 컨트롤에 대한 이 PR의 `Latch`/`shutdown_signal` 작업이 다루는 것과 같은 부류의 "SIGTERM에서 정상 종료가 안 됨" 버그를, 다른 코드 경로(`src/api/server.rs`가 아니라 `src/main.rs`)를 통해 독립적으로 발견하고 고침.

---

## 7. 변경 요약

### 통계

| 항목 | 값 |
|---|---|
| 변경 파일 | 23 |
| 추가 줄 | +3351 |
| 삭제 줄 | -92 |
| 커밋 | 8 |
| 호스트에서의 `cargo test --lib` | 1260개 통과, 3개 무시(윈도우 전용 모듈) |

### 카테고리별 변경

| 분류 | 내용 |
|---|---|
| SCM 백엔드 | `scm.rs`(순수 매핑과 파싱), `scm_backend.rs`(`windows-service` 위의 `ServiceBackend` 구현), `scm_host.rs`(`service run` 디스패처와 컨트롤 핸들러), `scm_log.rs`(롤링 파일 로깅) |
| 크로스 플랫폼 종료 | 새 `src/api/latch.rs`(`Latch` 원시 타입)와 `src/api/shutdown.rs`(`shutdown_signal`을 `server.rs`에서 분리하고 래치 소스로 확장) |
| 설정 탐색 | 윈도우에 `%PROGRAMDATA%\all-smi\config.toml`을 `candidate_config_paths()`에 추가 |
| CLI | 숨겨진 `ServiceAction::Run` 변형. `service_subcommand_is_registered` 테스트를 `run`이 유일한 숨겨진 동작임을 확인하도록 강화 |
| 검증 도구 | 독립 윈도우 타깃 프로브 크레이트(라이브러리 + 바이너리 대상), 주입된 타입 오류로 검증됨 |
| CI | 자체 호스팅 러너의 게이트된 `windows-service` 잡. 저장소 변수로 기본 비활성화됨. 실행된 적 없음 |
| 문서 | README "Windows (Service Control Manager)" 하위 절. man 페이지 갱신 |

### 관련 커밋

| SHA | 유형 | 메시지 |
|---|---|---|
| `2550593e` | feat(service) | run API mode as a Windows service (#311) |
| `96eb5bc2` | fix(cli) | register `service run` in the subcommand contract test |
| `c2bcf6a3` | docs | fix two intra-doc links in the shutdown modules |
| `db217d5f` | docs | drop an em dash from the %PROGRAMDATA% config doc comment |
| `0c0b2316` | docs(service) | record how to check the Windows-only modules |
| `e6f7b925` | merge | origin/main into feature/issue-311-windows-scm-service |
| `0ccbd1fa` | docs | list the Windows machine-wide config path in the README table |
| `e3304c1b` | docs(service) | correct the unsupported-platform message after #310 |

`main`에 `464bfdda`로 병합됨. #311을 닫는다.

---

## 8. 후속 조치

### 필수

macOS 구현 환경에서는 아래 런타임 수용 기준 전부가 미검증이고, 각각은 주장하는 대신 원 이슈에 체크되지 않은 채로 남겨두었다.

- **설치, 시작, 재부팅 지속성.** `service install --now`가 등록하고 시작함, `/metrics`가 응답함, `service status`가 PID를 보고함, 로그인 없이 재부팅을 견딤.
- **SCM 정지 시맨틱스.** `service stop`과 `services.msc`가 시작한 정지 둘 다 대기 힌트 안에 `SERVICE_STOPPED`에 도달함, 에너지 WAL 플러시 줄이 로그에 나타남, 고아 프로세스 없음.
- **메인 프로세스에 대한 `taskkill /F` 이후 실패 조치 복구.**
- **비상승 거부가 실제로 발동함.** `ERROR_ACCESS_DENIED`에서 `NeedsElevation`으로의 매핑은 단위 테스트됨. SCM이 install, uninstall, start, stop 각각에 대해 비상승 호출자에게 실제로 그 코드를 반환하는지는 관측된 바 없음.
- **콘솔에서 `service run`이 우아하게 실패함.** `ERROR_FAILED_SERVICE_CONTROLLER_CONNECT`에 대한 메시지는 단위 테스트됨. `StartServiceCtrlDispatcher`가 SCM 밖에서 실제로 그 코드를 반환하는지는 관측된 바 없음.
- **`%PROGRAMDATA%\all-smi\logs` 아래 로그 파일**이 나타나고 설정대로 회전함.
- **`%PROGRAMDATA%` 설정이 종단 간으로 준수됨**: `api.port`를 바꾸고 재시작해서 리스너가 실제로 옮겨감.
- **윈도우 자체에서의 `cargo test`**, 그리고 그에 따른 `#[cfg(windows)]` 테스트 모듈(`scm_backend_tests.rs`, `scm_host_tests.rs`). 여기서 컴파일은 되지만 실행된 적은 없고, `scm.rs`의 원시 Win32 상수가 `windows-service` 크레이트 자체의 열거형과 여전히 일치하는지에 대한 어서션도 포함됨.
- **자체 호스팅 러너의 `windows-service` 잡은 실행된 적이 없다.** 병합 시점에 설정되지 않은 저장소 변수 `ENABLE_WINDOWS_SERVICE_SMOKE`로 게이트되어 있다. 첫 실제 실행은 이 기능의 남은 작업의 일부로 취급해야지, 실패하더라도 회귀 신호로 취급해서는 안 된다.

### 모니터링 필요

- `RUST_LOG`를 위한 `Environment` `REG_MULTI_SZ` 레지스트리 값이 문서대로 동작하는지. 이는 이 변경이 아니라 윈도우 자체가 구현하는 것이고, 실행해본 적이 없다.

### 향후 개선 사항

- `ENABLE_WINDOWS_SERVICE_SMOKE`를 켜고 게이트된 CI 잡을 자체 호스팅 `windows-on-macmini02-x64` 러너에 대고 최소 한 번 실행하는 것. 유지관리자 자신의 윈도우 접근 없이 위 검증 공백을 닫는 유일한 경로다.
- 프로브 크레이트의 스텁 경계(`api`, `device`)가 그 모듈들이 진화하면서 실제 모듈의 공개 표면과 어긋나지 않았는지 확인하는 것. 프로브의 가치는 스텁이 실제 비호환성을 가릴 만큼 실제와 어긋나지 않는 데 달려 있다.

---

## 부록

### A. 테스트 결과

- `cargo fmt --check`: 클린.
- `cargo clippy --lib --tests -- -D warnings`: 클린.
- `cargo check --all-targets`: 클린.
- `cargo test --lib`: 1260개 통과, 3개 무시.
- 프로브 크레이트를 통한 `cargo clippy --target x86_64-pc-windows-msvc --lib --bins --tests -- -D warnings`: 클린.
- 프로브 커버리지 검증: `scm_backend.rs`, `scm_host.rs`, `scm_log.rs` 각각에 고의로 주입한 타입 오류가 매번 프로브를 통해 컴파일 오류를 냄.
- 병합 전 프로브가 잡은 실제 결함 두 개: `raw_code`/`describe`가 모듈 경계를 넘어 쓰이는데 가시성이 사설이었던 것. `build_service_info`가 `&Path` 대신 `&PathBuf`를 받던 것.
- **미검증**: 윈도우에서의 실제 실행이나 링크가 필요한 모든 것. 이 호스트에는 MSVC 링커가 없어 프로브 크레이트도 링크되지 않으므로, 링크 시점 정확성(심벌 해석, `windows-service`와의 ABI 합치)은 미검증이다.

### B. 성능 벤치마크

해당 없음. 이 PR에서 벤치마크할 만큼 실행된 것이 없다. 정성적 주장(성공적인 리스너 바인드 이후에만 준비 상태를 보고함, 14개 파일 롤링 로그 보관)은 코드 리뷰와 단위 테스트로 검증된 구조적 사실이지, 측정된 것이 아니다.

### C. 참고 자료

- `windows-service` 크레이트 문서와 그 `service_dispatcher`/`define_windows_service!` 매크로.
- Win32 서비스 제어 관리자 API: `SERVICE_STATUS`, `StartServiceCtrlDispatcher`, `ERROR_ACCESS_DENIED`, `ERROR_FAILED_SERVICE_CONTROLLER_CONNECT`.
- `tokio::sync::watch`: 채널 시맨틱스, `send_replace`, `borrow_and_update`.
- PR #319 보고서: 이 PR의 프로브 크레이트가 처음부터 피하도록 지어진 대상별 죽은 코드 분석 틈.
- 이슈 #311: 이 PR이 닫는 이슈.
