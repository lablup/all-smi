# 기술 보고서: PR #321 - feat(service): run API mode as a launchd service on macOS

**작성일**: 2026-08-05
**상태**: 실제 애플 실리콘 하드웨어의 사용자 스코프에서 실시간 검증 완료. 시스템 도메인(루트 LaunchDaemon) 경로는 미검증(8절 참고)
**언어**: Rust, YAML (GitHub Actions), XML (launchd property list)
**위험도**: Medium(서비스 관리 기능에 더해, 이미 배포 중인 리눅스 systemd 경로에도 영향을 미치는 API 모드의 SIGTERM 처리에 대한 진짜 크로스커팅 버그 수정을 포함함)

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

PR #321은 PR #319가 이 이슈를 가리키는 `NotSupported`로 남겨둔 `service_cmd::backend()`의 macOS 분기를, 같은 `ServiceBackend` 계약을 아무 변경 없이 구현하는 실제 launchd 백엔드로 바꾼다. 메서드도, `Scope` 시맨틱스도, 종료 코드도, 오류 변형도 그대로다. 그 과정에서 API 모드의 정상 종료 경로가 SIGTERM에서 도달 불가능했던, macOS뿐 아니라 모든 플랫폼에 영향을 준 기존 버그를 함께 고친다.

핵심은 이 PR 자신의 조사 과정이 만들어낸 자기 수정이다. 처음 세운 가설은 PR #319의 systemd 발견, 즉 `ProtectKernelModules=`가 `ExecStart` 전에 사용자 스코프 유닛을 `218/CAPABILITIES`로 죽인다는 사실에 유추해서, launchd도 `gui/$UID` LaunchAgent가 지킬 수 없는 키(`UserName`, `GroupName`, `InitGroups`)에 대해 비슷하게 동작하리라 가정한 것이었다. 그렇지 않았다. macOS 26.6에서 `gui/501`에 부트스트랩한 프로브 LaunchAgent에 `UserName root`와 `GroupName wheel`을 모두 설정하고 그 프로그램이 `id`를 찍게 해서 테스트했다. `launchctl bootstrap`은 성공했고, 유효 uid/gid는 루트가 아니라 호출한 사용자 자신의 것인 `501`/`20`이었다. launchd는 사용자별 도메인에서 이 키들을 거부하는 대신 조용히 무시한다. 그래서 이 키들을 사용자 스코프 plist에서 빼는 이유는 크래시를 막기 위해서가 아니다(크래시는 없다). 이 키들을 남겨두면, 머신에서 무엇이 특권으로 도는지 감사하는 누구에게든 그 plist가 루트 잡으로 읽히기 때문이다. 실제로는 그 도메인에서 루트 잡이 될 수도 없는데 말이다. 게다가 `launchd.plist(5)`는 이 키들이 루트를 요구한다고 문서화할 뿐 조용한 무시 폴백은 문서화하지 않으니, 그 동작은 명세되지 않은 채로 남아 미래의 어느 릴리스에서 치명적으로 바뀔 여지가 있다. 부트스트랩 실패가 여기서 회귀를 절대 드러내지 않으므로, 렌더링 시점 테스트가 이 키들의 부재를 직접 확인한다.

이 PR이 고치는 SIGTERM 버그는 launchd와는 무관하다. `run_command`의 시그널 핸들러가 무조건 `std::process::exit(0)`을 호출해 `run_api_mode` 자신의 서빙 후 정리(에너지 WAL 플러시, 유닉스 소켓 제거) 경합에서 이겨버렸다. 그래서 리눅스에서 systemd 아래든 launchd 아래든 똑같이, 모든 SIGTERM이 축적된 마지막 줄 단위 에너지 배치를 떨어뜨리고 낡은 소켓을 남겼다. `Api`는 이제 `Record`가 이미 그랬던 것처럼 자신의 종료를 소유하며, `[general].default_mode` 재디스패치 경로까지 포함한다. M1 Ultra에서 실시간으로 검증했다. LaunchAgent가 내보내는 지표 이름 집합은 포그라운드 `all-smi api` 실행과 바이트 단위로 동일했고, `launchctl bootout`으로 정지시키면 이제 최종 에너지 WAL 플러시 로그 줄에 도달하는데, 이전에는 SIGTERM이 그런 줄을 전혀 남기지 않았던 반면 이제는 0.45초 안에 도달한다. 조사 과정에서 환경 관련 발견 두 가지도 나왔다. `ProcessType Background`(모니터가 자신이 관찰하는 GPU 작업과 P코어 시간을 두고 경쟁하지 않으려면 필요함)는 launchd 아래에서 약 6.3초의 시작 비용이 드는데, 포그라운드의 0.6초와 대비되고 백그라운드 QoS로 도는 IOReport 채널 열거가 이를 지배한다. 그리고 외부 볼륨에 있는 바이너리는 launchd 아래에서 `dyld`의 `open()`에 무한정 멈추는데, 이는 코드 결함이 아니라 launchd가 스폰한, 이동식 볼륨에서 읽는 프로세스에 대한 TCC 게이트다. 전체 규모는 파일 15개, +2581/-59, 커밋 1개이며 #310을 닫는다.

---

## 1. 문제 정의

### 1.1 배경

PR #319는 크로스 플랫폼 `all-smi service` 프레임워크와 그 systemd 구현체를 확립하면서, macOS(이 이슈)와 윈도우(이슈 #311, PR #320이 병행 구현)에 각자의 추적 이슈를 이름 붙인 명시적 `NotSupported` 분기를 남겼다. 이슈 #310은 추가로 Homebrew 관리 경로도 다룬다. `brew services start all-smi`(사용자별, `gui/$UID`)와 `sudo brew services start all-smi`(시스템 도메인, 아무도 로그인하지 않은 채로 재부팅을 견딤)인데, 이는 별도 저장소 `lablup/homebrew-tap`의 폼을라에 `service do` 블록을 추가해야 하고, 이 PR이 직접 할 수 있는 변경이 아니다.

### 1.2 기존 문제점

- **문제 1 (launchd 백엔드 없음)**: `backend()`의 macOS 분기가 `NotSupported`를 반환해서, 리눅스 systemd 경로에는 이미 있던 `all-smi service install`에 해당하는 것이 zip이나 로컬 빌드 설치에는 없었다.
- **문제 2 (macOS 시스템 전역 설정 후보 없음)**: `candidate_config_paths()`에는 PR #319가 리눅스용으로 추가한 `/etc/all-smi/config.toml` 티어에 해당하는 macOS 대응물이 없었다. 그래서 어떤 로그인 세션 밖에서 도는 루트 LaunchDaemon은 사용자별이 아닌 곳에 설정될 방법이 없었다(시스템 LaunchDaemon의 `~/Library/Application Support`는 운영자가 아니라 루트의 홈으로 풀린다).
- **문제 3 (API 모드의 SIGTERM 핸들러가 자신의 정상 종료를 우회함)**: `run_command`는 `Record`를 제외한 모든 서브커맨드에 무조건 `std::process::exit(0)`을 호출하는 핸들러를 설치했고, 이것이 모든 SIGTERM에서 `run_api_mode`의 서빙 후 정리와 경합해 이겼다. 이는 launchd 전용 결함이 아니다. `systemctl stop`도 SIGTERM을 전달하므로 PR #319에서 이미 배포 중인 리눅스 systemd 배포에도 똑같이 영향을 준다.
- **문제 4 (데몬 컨텍스트에서 애플 실리콘 네이티브 리더에 대한 미검증 가정)**: 제어 터미널도, sudo도, GUI 세션도 없는 상태에서 IOReport, SMC, `NSProcessInfo.thermalState`가 올바로 풀리는지는 실제 launchd가 관리하는 프로세스에서 테스트된 적이 없었다.

### 1.3 위험성

| 위험 | 영향 | 발생 가능성 |
|---|---|---|
| launchd의 실제 동작을 측정하지 않고 systemd 유추(PR #319)로만 추론함 | Medium: 실제보다 더 특권 있어 보이는 plist를 배포하거나, 실패가 어떻게 드러나는지에 대한 잘못된 가정을 배포했을 것 | 병합 전 직접 측정으로 잡힘(2.1절). 가정은 틀렸고 수정됨 |
| SIGTERM 버그가 이미 배포된 리눅스 systemd 경로에도 지속됨 | High: PR #319의 리눅스 배포에서 일어나는 모든 `systemctl stop`도 이 PR이 추가하는 macOS 경로뿐 아니라 마지막 에너지 WAL 배치를 떨어뜨리고 낡은 소켓을 남김 | 같은 PR에서 크로스 플랫폼으로 고쳐짐. launchd 전용으로 범위를 좁히지 않음 |
| `ProcessType Background`의 시작 비용이 프로세스 상태만 폴링하는 준비 상태 확인으로는 감안되지 않음 | Medium: launchd가 실행 중이라고 보고하는 순간 서비스가 준비됐다고 가정하는 감독자나 헬스 체크가 익스포터가 실제로 뭔가 수집하기 전에 질의할 것 | 이 PR에 문서화됨. 정확히 이 공백이 CI에서 일으킨 후속 결함은 PR #323에서 별도로 고침 |
| 시스템 도메인(루트 LaunchDaemon) 동작이 미검증 | Medium: 재부팅 지속성, 아무도 로그인하지 않은 채 실행, Homebrew의 `sudo brew services start` 경로 모두 특별히 시스템 도메인에 의존함 | 명시적으로 열어둠. 사용자 스코프 경로는 대체 신호로 완전히 검증됐지 대체품은 아님(8절) |

---

## 2. 기술적 검토 사항

### 2.1 자기 수정: launchd는 지킬 수 없는 것을 거부하지 않고 조용히 무시한다

**유추에 의한 가정.** PR #319는 `ProtectKernelModules=`(비특권 매니저가 얻을 수 없는 권한이 필요함)를 가진 systemd 사용자 스코프 유닛이 `ExecStart` 전에 `218/CAPABILITIES`로 죽는다는 것을 발견했다. launchd의 대응 문제, 즉 `UserName`, `GroupName`, `InitGroups`(`launchd.plist(5)`에 따르면 적용하려면 루트가 필요한 키)를 가진 `gui/$UID` LaunchAgent에 대한 자연스러운 첫 가정은, 그런 에이전트를 부트스트랩하면 비슷하게 실패하리라는 것이었다.

**실제로 측정한 것.** macOS 26.6(Darwin 25.6)에서 `gui/501`에 프로브 LaunchAgent를 부트스트랩했고, 그 프로그램은 `id`를 찍었다.

| Plist 키 | `launchctl bootstrap` | 유효 uid/gid |
|---|---|---|
| 없음 | 성공 | `501` / `20` |
| `UserName root`, `GroupName wheel` | 성공 | `501` / `20` |
| `InitGroups` + `UserName root` | 성공 | `501` / `20` |

모든 경우가 성공했고, 유효 정체성은 항상 호출한 사용자 자신의 것이었지 루트가 아니었다. 사용자별 도메인에서 이 키들은 거부되는 대신 조용히 무시된다.

**아무것도 죽지 않는데도 여전히 이 키들을 빼는 이유.** 실패를 막기 위해서가 아니다. 실패는 없다. 이 키들을 남겨두면 자신을 잘못 표현하는 plist를 배포하는 셈이기 때문이다. `~/Library/LaunchAgents`에 있는 파일이 `UserName root`를 선언하면, 머신에서 무엇이 상승된 권한으로 도는지 감사하는 누구에게든 루트 잡으로 읽힌다. 실제로는 그렇지 않고 그 도메인에서는 그럴 수도 없는데 말이다. 이 키들을 남겨두고 그 이상함을 문서화하는 대신 아예 빼는 것을 강화하는 고려 사항이 두 가지 더 있다. `launchd.plist(5)`는 이 키들이 루트를 요구한다고 문서화할 뿐 조용한 무시 폴백에 대해서는 아무 말도 하지 않으니 그 동작은 명세되지 않았고 미래 macOS 릴리스에서 치명적으로 바뀔 여지가 있다. 그리고 `InitGroups`는 `UserName`이 설정되지 않았을 때는 무시된다고 별도로 문서화되어 있는데, 사용자 스코프 렌더링에서는 이제 항상 그렇다. 그러니 거기 있는 것은 이중으로 무의미하다.

**이것이 테스트에 만드는 귀결.** 부트스트랩 실패가 여기서 회귀를 절대 드러내지 않으므로(요점 자체가 launchd가 불평하지 않는다는 것이다), 정확성은 언젠가의 런타임 신호에 기대는 대신 렌더링 시점에 강제해야 한다. 전용 테스트가 모든 사용자 스코프 렌더링에서 `UserName`, `GroupName`, `InitGroups`의 부재를 확인하고, 두 번째 테스트는 배포되는 plist 템플릿의 모든 최상위 키가 "유지" 또는 "제거" 목록 중 하나로 분류되어 있음을 확인한다. 그래서 템플릿에 분류 결정 없이 새 키가 추가되면 코드가 기본으로 어느 목록에 떨어지든 조용히 넘어가는 대신 CI에서 실패한다.

**거울상 사례도, 가정이 아니라 측정.** `HardResourceLimits`는 위와 반대 이유로 표준 템플릿에서 의도적으로 빠져 있다. 오직 루트만 하드 한도를 *올릴* 수 있으므로, 사용자 스코프 잡은 그것이 있어도 얻는 게 없다. 반면 `SoftResourceLimits`는 두 스코프 모두에 유지되는데, 이미 상속된 하드 한도까지 소프트 rlimit을 올리는 데는 아무 권한도 필요 없기 때문이다.

### 2.2 크로스 플랫폼 버그: API 모드의 SIGTERM 핸들러가 자신의 정리와 경합함

**증상.** `run_command`는 `Record`를 제외한 모든 서브커맨드에 시그널 핸들러를 설치했다.

```rust
let is_record = matches!(cli.command, Some(Commands::Record(_)));
if !is_record {
    tokio::spawn(async {
        signal::ctrl_c().await.expect("Failed to listen for Ctrl+C");
        std::process::exit(0);
    });
}
```

`Api`의 경우, 이 핸들러와 `run_api_mode` 자신의 정상 종료 경로(axum의 `with_graceful_shutdown`, 이어서 에너지 WAL 플러시와 유닉스에서는 소켓 파일 제거)가 같은 시그널을 함께 기다린다. 어느 쪽이 경합에서 이기느냐가 정리가 아예 실행되는지를 결정한다. 스폰된 핸들러의 무조건적인 `std::process::exit(0)`이 상습적으로 이겼고, 그 결과 `launchctl bootout`과 `systemctl stop`이 둘 다 보내는 바로 그 신호인 SIGTERM마다 WAL 플러시나 소켓 정리가 실행될 기회를 갖기 전에 프로세스가 종료됐다.

**일반론이 아니라 서비스에 특히 문제가 되는 이유**: 이는 억지스러운 조건에서만 도달하는 예외적 사례가 아니다. SIGTERM은 정확히 이 프로젝트가 이제 연동하는 두 서비스 관리자(이 PR을 통한 launchd, PR #319에서 이미 배포 중인 systemd) 모두가 관리되는 프로세스를 정지시키는 방법이다. 그러니 이 버그는 가끔이 아니라 두 서비스 중 어느 쪽의 모든 재시작마다 발동했다.

**수정**: `Api`는 이제 `Record`가 이미 그랬던 것처럼 자신의 종료를 소유하며, `[general].default_mode` 재디스패치 경로까지 포함한다. 그 경로에서는 `cli.command`가 `None`이고 모드는 `Settings`를 읽은 뒤에야 알려진다. 그 경로의 재귀 호출은 바깥 호출이 이미 스폰한 핸들러를 해제할 수 없으므로, `owns_shutdown`은 `cli.command`만이 아니라 `settings.general.default_mode`를 통해 실효 모드를 직접 해석한다. 수집기(macOS의 네이티브 지표 매니저, 리눅스의 hl-smi 매니저)는 `view` 모드가 이미 쓰던 순서를 그대로 따라 `run_api_mode`가 반환한 뒤에 정리되며, 경합하는 `exit(0)`에 의해 정리 도중 버려지지 않는다.

**검증된 효과**: 수정 전에는 SIGTERM이 `energy WAL: shutdown requested` 로그 줄을 전혀 남기지 않았다. 수정 후에는 그 줄이 나타나고 프로세스가 0.45초 안에 종료한다.

### 2.3 호환성 및 의존성

- **Breaking Changes**: `ServiceBackend` 계약에는 없다. macOS 분기가 이 PR이 교체하는 유일한 `backend()` 갈래다.
- **새로운 의존성**: 없다. plist 렌더러와 `launchctl` 래퍼는 표준 라이브러리와 기존 워크스페이스 의존성만 쓴다.
- **호환성**: `/Library/Application Support/all-smi/config.toml`이 `candidate_config_paths()`에 모든 사용자별 후보 뒤로 붙는 새 티어 2 후보로 추가된다. PR #319가 리눅스용으로, PR #320이 윈도우용으로 확립한 패턴을 그대로 추가적으로 따른다.

### 2.4 코드 품질

새 단위 테스트: `service_cmd::`에 110개(멀티라인 `<array>`/`<dict>` 값 처리를 포함한 plist 렌더링, XML 이스케이핑과 제어 문자 거부, 계정 이름 검증, 사용자 스코프 제거 키 목록과 그 분류 완전성 가드. `launchctl print`/`print-disabled` 파싱. 관리 마커 가드와 원자적 `0644` plist 쓰기), `common::paths`에 17개(macOS 시스템 전역 후보와 그 순서). `cargo clippy --lib --tests -- -D warnings`와 `cargo clippy --bin all-smi -- -D warnings`를 의도적으로 두 번 따로 실행했다. 라이브러리 대상 확인에서는 아무것도 잡히지 않았지만, 바이너리 대상 확인에서는 `SERVICE_NAME`을 죽은 코드로 지적했다. 이는 systemd 백엔드에서만 도달하는데, 그 백엔드는 리눅스가 아니거나 테스트가 아닌 빌드에서는 컴파일되지 않기 때문이다. 이는 PR #319와 PR #320이 각각 독립적으로 기록한 것과 같은 컴파일 대상별 죽은 코드 사각지대인데, 여기서는 가정이 아니라 라이브러리 확인 쪽에서 관측된 것이다.

---

## 3. 기술적 선택과 그 이유

### 3.1 생존이 아니라 정직성 때문에 사용자 스코프 plist에서 `UserName`/`GroupName`/`InitGroups`를 제거

2.1절에서 전체를 다뤘다. 이 PR의 핵심 기술적 결정으로 여기 다시 기록하는 이유는, 이것이 다른 서비스 관리자의 동작에 대한 그럴듯하지만 틀린 유추에 기반한 초기 가정을 뒤집었고, 문서 자체가 그 폴백 동작을 서술하지 않으므로 문서가 아니라 직접 측정의 힘으로 그렇게 했기 때문이다.

### 3.2 launchd 동사 매핑: `install`은 로드 없이 씀. `install --now`는 언로드 후 다시 로드함. `stop`은 언로드하고 plist는 남김

**컨텍스트**: launchd에는 systemd의 `enable`/`disable`처럼 별도의 "부팅 시 활성화됨" 상태가 없다. `LaunchDaemons`나 `LaunchAgents`에 존재하는 plist는 부팅이나 로그인 시 자동으로 부트스트랩되고, `RunAtLoad`가 거기서 시작한다. `launchctl`은 로드된 잡의 정의를 캐시하므로, 이미 로드된 것 위에 바뀐 plist를 부트스트랩하면 그 자리에서 교체되는 대신 실패한다.

| 동사 | 채택한 동작 | 근거 |
|---|---|---|
| `install`(`--now` 없음) | plist를 쓰고 멈춤 | 이것이 정확히 "부팅 시 활성화됨, 아직 실행 중은 아님"이며, systemd 백엔드에서 `--now` 없는 `install`이 갖는 시맨틱스와 일치함 |
| `install --now` | 잡을 언로드했다가 다시 로드함 | 이미 로드된 잡 위에 바뀐 정의를 로드하면 launchd에서 실패함. 언로드 후 재로드 순서가 새 정의를 적용시키는 방법임 |
| `stop` | 잡을 언로드하고 plist는 남김 | `systemctl stop`과 일치함. 서비스가 다음 부팅/로그인에 돌아옴. plist("활성화됨" 상태)가 여전히 존재하기 때문 |
| `status` | 디스크의 plist 존재 여부로 `installed`를, `launchctl print-disabled`로 `enabled`를 판단 | `launchctl print`는 현재 로드된 잡만 안다. 그래서 "설치됨"에 답하는 것은 로드 상태가 아니라 plist 존재 여부임 |
| `install`도 `launchctl enable`을 실행함 | 영구적인 비활성화 오버라이드를 지움 | `launchctl disable` 오버라이드는 plist와 재부팅 둘 다보다 오래 남음. 새로 `install`한 뒤에도 그대로 두면 방금 설치한 잡이 조용히 영영 시작하지 못하게 됨 |
| `uninstall`은 의도적으로 `disable`을 하지 않음 | `install`/`enable` 짝짓기와 대칭 | 제거 시 비활성화하면 이후 `install`을 조용히 막는 남은 오버라이드가 생김. `install`의 `enable` 호출이 존재하는 것과 같은 문제 |

**선택 이유**: 각 매핑 결정은 systemd 백엔드 동사와의 일대일 대응을 가정하는 대신, 검증된 특정 launchd 동작(로드된 정의 캐싱, plist보다 오래 남는 영구적 비활성화 오버라이드)에서 나온다.

### 3.3 `--service-user`는 계정 이름을 미러링하는 대신 `GroupName`을 제거함, systemd 렌더러와 다름

**컨텍스트**: systemd 템플릿 렌더러(PR #319)는 `--service-user`에서 `User=`와 `Group=`을 둘 다 설정하는데, `systemd-sysusers`가 관례적으로 계정 이름을 딴 그룹을 만든다는 데 기댄다. launchd 렌더러는 여기서 갈라진다.

**채택한 접근**: `--service-user`는 `UserName`을 설정하고 `GroupName`은 계정 이름을 그대로 옮기는 대신 완전히 제거한다.

**선택 이유**: macOS에는 일반 계정이나 서비스 계정이 자신과 이름이 같은 그룹을 소유한다는 대응 관례가 없다. `dscl`로 만든 서비스 계정은 어떤 기본 그룹이든 가질 수 있다. `GroupName`을 생략하면 launchd가 패스워드 데이터베이스에서 곧바로 계정의 실제 기본 그룹으로 물러나는데, 이는 항상 옳다. 반면 계정 이름을 `GroupName`에 그대로 옮기면 존재하지 않거나 계정의 실제 그룹 소속과 맞지 않는 그룹을 조용히 지목할 수 있다.

### 3.4 SIGTERM 버그를 별도 PR로 미루지 않고 이 PR에서 고침

**컨텍스트**: 이 버그(2.2절)는 launchd 전용이 아니다. 이미 병합된 PR #319의 리눅스 systemd 경로에도 똑같이 영향을 준다.

**채택한 접근**: 별도 후속 이슈로 등록하는 대신 같은 PR에서 바로 고쳤다.

**선택 이유**: 이 버그는 정확히 이 PR이 자신의 실시간 검증 체크리스트를 위해 `stop`(즉, SIGTERM을 보내는 `launchctl bootout`)이 에너지 WAL 플러시에 도달해야 했기 때문에 발견됐다. 수정을 미뤘다면 자신의 수용 기준조차 통과를 보여줄 수 없는 launchd 백엔드를 배포했을 것이다. 코드 주석과 PR 설명에 크로스 플랫폼 범위를 명시하면서 그 자리에서 고치는 것이, 더 좁은 macOS 전용 우회책보다 유용하다고 판단했다.

---

## 4. 구현 상세

### 4.1 아키텍처 변경

```
[#319의 크로스 플랫폼 계약, 변경 없음]

service_cmd::backend()
    #[cfg(target_os = "macos")]  ->  LaunchdBackend   (이전: NotSupported, #310으로 추적)

[신규: launchd 백엔드 자체의 계층 구조]

LaunchdBackend (impl ServiceBackend)
    -> plist.rs      render_plist(): <key>/값 쌍을 (줄이 아니라) 훑어서, 멀티라인
                     <array>/<dict> 값이 한 단위로 제거되게 함. ProgramArguments,
                     로그 경로, 스코프별 UserName/GroupName을 재작성
    -> launchctl.rs  레이아웃 해석(plist 경로, 로그 경로, 도메인, 서비스 대상),
                     `launchctl` 호출, `launchctl print` / `print-disabled` 파싱
    -> launchd.rs    동사 정책(3.2절)과 디스크 측 절반: 관리 마커 가드,
                     원자적 0644 plist 쓰기, 로그 디렉터리 생성

[크로스커팅 수정, launchd 전용 아님]

src/main.rs   Api가 이제 Record와 마찬가지로 자신의 SIGTERM 종료를 소유함.
              owns_shutdown이 명령 없음(None) 재디스패치 경로에 대해
              [general].default_mode를 통해 해석됨
```

### 4.2 주요 코드 변경

**파일: `src/main.rs` (SIGTERM 소유권 수정)**
```rust
// * `Api`도 같은 이유로 자신의 종료를 소유한다. `run_api_mode`는
//   axum의 정상 종료를 기다린 뒤 최종 에너지 WAL 플러시와 fsync를
//   수행하고 유닉스 소켓 파일을 제거한다. 아래의 무조건적 exit이
//   그 경합에서 이겼으므로, 모든 SIGTERM이 축적된 마지막 줄 단위
//   Joule 배치를 떨어뜨리고 낡은 소켓을 남겼다. 이는 서비스에게
//   예외적 사례가 아니다. SIGTERM은 정확히 `launchctl bootout`과
//   `systemctl stop`이 이를 끝내는 방법이며, 재시작마다 발동했다
//   (이슈 #191, #309, #310).
let owns_shutdown = match &cli.command {
    Some(Commands::Record(_) | Commands::Api(_)) => true,
    // `None`은 아래에서 [general].default_mode를 통해 재디스패치되고,
    // 그 재귀 호출은 바깥 호출이 이미 스폰한 핸들러를 해제할 수 없다.
    // 여기서 실효 모드를 해석한다.
    None => settings.general.default_mode == "api",
    _ => false,
};
if !owns_shutdown {
    tokio::spawn(async {
        signal::ctrl_c().await.expect("Failed to listen for Ctrl+C");
        std::process::exit(0);
    });
}
```
**변경 이유**: 2.2절과 3.4절에서 설명한 수정이다. `None => settings.general.default_mode == "api"` 분기가 틀리기 쉬운 부분이다. `default_mode`를 통한 재디스패치는 CLI 수준의 `cli.command`만으로는 "이 실행이 자신의 종료를 소유하는가"에 답할 수 없다는 뜻이다. 답이 핸들러 결정 시점에는 아직 읽히지 않은 설정 값에 달려 있기 때문이다.

**파일: `src/service_cmd/plist.rs` (이 PR의 측정이 만든 사용자 스코프 키 분류)**
```rust
/// 사용자 스코프 LaunchAgent를 렌더링할 때 제거되는 키.
///
/// `gui/$UID` 도메인은 비특권 사용자가 소유하며 다른 계정으로
/// `setuid`할 수 없다. systemd는 이를 유닛을 아예 거부하는 것으로
/// 답한다... launchd는 그러지 않는다. `UserName`, `GroupName`,
/// `InitGroups`를 가진 LaunchAgent를 부트스트랩하면 성공하고,
/// 잡은 실행되며, 키들은 **조용히 무시된다**.
///
/// | Plist 키 | `launchctl bootstrap` | 유효 uid/gid |
/// |---|---|---|
/// | 없음 | 성공 | `501` / `20` |
/// | `UserName root`, `GroupName wheel` | 성공 | `501` / `20` |
///
/// 그러니 이들은 크래시를 막으려고 빼는 게 아니라, 남겨두면
/// 거짓말하는 plist를 배포하는 셈이라 뺀다.
pub const USER_SCOPE_DROPPED_KEYS: &[&str] = &["UserName", "GroupName", "InitGroups"];
```
**변경 이유**: 이 상수와, 그 뒤에 있는 측정된(가정이 아닌) 동작을 기록한 문서 주석이 2.1절 자기 수정의 직접적인 산물이다.

### 4.3 데이터 모델 변경

와이어 포맷이나 지표 변경은 없다. `/Library/Application Support/all-smi/config.toml`은 macOS의 `candidate_config_paths()`에서 모든 사용자별 후보 뒤에 붙는 새 티어 2 후보다.

---

## 5. 학습 포인트

### 5.1 같은 "비특권 프로세스, 특권 지시어" 문제를 푸는 서비스 관리자 둘이 정반대 실패 양상을 고를 수 있다

**개념**: systemd는 실패 시 닫힌다. 적용할 수 없는 지시어를 가진 사용자 스코프 유닛은 시작을 거부하며, (조사 없이는 불투명하더라도) 특정 종료 상태를 낸다. launchd는 실패 시 열린다. 사용자별 도메인에서 지킬 수 없는 키를 가진 LaunchAgent는 그냥 그 키를 무시하고 어쨌든 실행된다.

**이 PR에서의 적용**: PR #319의 systemd 발견에서 launchd의 동작을 유추로 예측하는 것은, 검증하지 않고 남겨뒀다면 어느 방향으로든 틀렸을 것이다. launchd도 거부하리라 가정했다면 절대 일어나지 않는 실패를 위한 테스트를 작성했을 것이다. 직접 측정한 것(2.1절)만이 올바른 테스트 전략을 드러냈다. 렌더링 시점에 키의 부재를 확인하는 것인데, 어떤 런타임 신호도 그 키의 존재를 절대 잡지 못하기 때문이다.

### 5.2 "하나만 빼고 모든 서브커맨드"를 위해 설치된 시그널 핸들러는 새 서브커맨드가 같은 예외를 필요로 할 때마다 재검토해야 하는 정책이다

**개념**: 무조건 종료 시그널 핸들러를 설치하는 조건으로서의 `!is_record`는 "`Record`를 제외한 모든 서브커맨드의 종료 시맨틱스는 같다"는 것을 인코딩한다. 그 주장은 두 번째 서브커맨드(`Api`)가 같은 예외를 필요로 하는 순간 조용히 성립하지 않게 되고, 원래 코드의 어느 것도 그 공백을 알려주지 않았다. 실제 종료 동작을 테스트해봐야 발견될 수 있었다.

**이 PR에서의 적용**: 수정은 조건을 이름 붙인 예외 하나가 아니라, 서브커맨드별로(그리고 `None`/재디스패치 경우에는 실효 모드별로) 계산되는 `owns_shutdown`으로 일반화한다. 이것이 같은 예외가 필요한 다음 서브커맨드를 조건의 형태를 재협상하는 문제가 아니라 매치 분기 하나 추가하는 문제로 만든다.

### 5.3 `ProcessType Background`의 비용은 단순히 "더 느림"이 아니라, 어떤 준비 상태 신호를 신뢰할 수 있는지를 바꾼다

**개념**: launchd의 `ProcessType Background`는 프로세스 전체에 대해 백그라운드 QoS 스케줄링을 선택한다. 자신이 관찰하는 워크로드와 P코어 시간을 두고 경쟁해서는 안 되는 모니터링 도구에게는 옳은 선택이지만, CPU 바운드 시작 작업(IOReport 채널 열거)을 대략 한 자릿수 정도 측정 가능하게 느리게 만든다(이 PR의 측정에서 포그라운드 0.6초 대 LaunchAgent 6.3초).

**이 PR에서의 적용**: `service status`는 launchd가 프로그램을 스폰한 순간 "실행 중"이라고 보고하는데, 이는 그 시작 작업은커녕 첫 지표 수집 주기가 끝나기도 훨씬 전이다. 이 PR은 이 공백을 문서화하고 자신의 CI 준비 대기를 `service status`가 아니라 `/metrics` 내용을 폴링하도록 조정한다. PR #323 보고서는 같은 발견의 더 정밀한 버전(정확한 창 크기와 그것이 CI 어서션에 미치는 영향)을 다룬다.

---

## 6. 추가 학습

### 핵심 용어

| 용어 | 설명 | 관련성 |
|---|---|---|
| `gui/$UID` 도메인 | 부트스트랩된 LaunchAgent를 위한 launchd의 사용자별 세션 도메인 | `UserName`/`GroupName`/`InitGroups` 조용한 무시 동작이 측정된 곳 |
| `launchctl bootstrap` / `bootout` | 잡을 launchd 도메인에 로드하거나 제거하는 명령 | `install --now`의 언로드-후-재로드, `stop`의 언로드-후-plist-유지 매핑 뒤의 메커니즘 |
| `RunAtLoad` | 부트스트랩되면 자동으로 잡을 시작하는 plist 키 | launchd에 systemd 같은 별도 "활성화됨" 개념이 없는 이유 |
| `ProcessType Background` | 백그라운드 QoS 스케줄링을 선택하는 launchd 키 | 포그라운드 0.6초 대비 측정된 약 6.3초 시작 비용의 원천(5.3절) |
| launchd가 스폰한 프로세스에 대한 TCC 게이트 | launchd가 특정 볼륨에서 시작하는 프로세스에 영향을 주는 macOS 개인정보보호/보안 제한 | 이 PR의 검증에서 기록된 외부 볼륨 멈춤의 근본 원인 |
| `owns_shutdown` | 원래의 `!is_record` 특수 사례를 대체하는 이 PR의 일반화된 조건 | 크로스 플랫폼 SIGTERM 수정(2.2, 5.2절) |

### 관련 기술/프레임워크

- `launchd.plist(5)`와 launchd의 잡 관리 모델(도메인, bootstrap/bootout, `RunAtLoad`, `KeepAlive`).
- 백그라운드 QoS 스케줄링 아래의 IOKit/IOReport와 `NSProcessInfo.thermalState`.
- Homebrew의 `service do` DSL, `brew services`를 위한 launchd plist 생성, 그리고 이 PR의 서브커맨드 기반 설치 경로와의 상호작용(또는 상호작용 없음).

### 관련 PR/이슈

- 이슈 #310: 이 PR이 닫는 이슈.
- PR #319(이슈 #309): 이 PR이 변경 없이 구현하는 `ServiceBackend` 계약을 정의함. 이 PR의 초기(그리고 수정된) 추론이 끌어온 `ProtectKernelModules=` 발견의 출처.
- PR #320(이슈 #311): 같은 계약을 상대로 병행 개발된 윈도우 SCM 백엔드.
- PR #323: 이 PR의 `ProcessType Background` 시작 비용 발견이 예고하고 그 PR이 정밀하게 측정하는 launchd CI 스모크 테스트 경합.
- 이슈 #191, #309, #310: 이 PR이 고치는 SIGTERM 버그의 영향을 받는 것으로 이 PR 자신의 코드 주석에 언급된 세 이슈.

---

## 7. 변경 요약

### 통계

| 항목 | 값 |
|---|---|
| 변경 파일 | 15 |
| 추가 줄 | +2581 |
| 삭제 줄 | -59 |
| 커밋 | 1 |
| 새 단위 테스트 | 127개(`service_cmd::` 110, `common::paths` 17) |

### 카테고리별 변경

| 분류 | 내용 |
|---|---|
| launchd 백엔드 | `plist.rs`(렌더러, 370줄), `launchctl.rs`(호출과 파싱, 349줄), `launchd.rs`(동사 정책과 디스크 연산, 319줄) |
| 크로스 플랫폼 버그 수정 | `src/main.rs`: `Api`가 이제 자신의 SIGTERM 종료를 소유함. 리눅스 systemd 경로에도 영향을 주던 결함을 고침 |
| 설정 탐색 | macOS의 `candidate_config_paths()`에 `/Library/Application Support/all-smi/config.toml` 추가 |
| 탐지 | `service_cmd/detect.rs`에 macOS 전용 Homebrew 거부 힌트(`sudo brew services start all-smi`) 추가 |
| CI | `macos-14`에 새 게이트된 `launchd-service` 잡 |
| 문서 | README와 man 페이지의 macOS 하위 절. `lablup/homebrew-tap`의 `service do` 블록을 위한 수동 적용 체크리스트 |

### 관련 커밋

| SHA | 유형 | 메시지 |
|---|---|---|
| `3541e77f` | feat(service) | run API mode as a launchd service on macOS (#310) |

`main`에 `8c822aa4`로 병합됨. #310을 닫는다.

---

## 8. 후속 조치

### 필수

- **시스템 도메인 LaunchDaemon 경로 검증.** `/Library/LaunchDaemons`에 쓴 적도 `sudo launchctl bootstrap system`을 실행한 적도 없다. 이 경로는 단위 테스트와, 루트 없이 올바로 거부하고 아무것도 쓰지 않는지 확인하는 실시간 확인으로만 실행됐다.
- **재부팅 지속성 검증.** 서브커맨드 형태든 `sudo brew services` 형태든. 시스템 도메인 없이는 도달 불가능함.
- **`service do` 블록을 `lablup/homebrew-tap`에 적용.** 탭에 아무것도 푸시하지 않았다. 검증된 diff가 PR 설명에 있고, 스크래치 사본을 상대로 로컬에서 `ruby -c`와 `brew style`로 확인했다(패치 안 된 업스트림 파일에 이미 있는 위반 4건과 일치하며, service 블록이 새로 추가한 위반은 없음).
- **위 블록이 적용된 뒤 `sudo brew services start all-smi`와 탭 폼을라를 종단 간으로 검증.**
- **실제 Homebrew 경로 거부 검증.** `detect::classify`는 세 Homebrew 접두사 전부에 대해 단위 테스트됐고 macOS 전용 힌트 문구도 어서션되어 있지만, 실제로 거부를 발동시키려고 `/opt/homebrew` 아래에 바이너리를 놓아본 적은 없다.

### 모니터링 필요

- 시스템 도메인이 실행되면 `/Library/Application Support/all-smi/config.toml`이 실제로 로드되는지. 후보 목록과 순서는 단위 테스트됐지만, 그 경로의 파일을 실제 데몬이 읽은 적은 아직 없다.
- `service status`가 아니라 실제 내용을 폴링하는 어떤 소비자에게든 준비 상태 확인 경합의 원천이 되는 `ProcessType Background` 시작 비용 공백(5.3절). PR #323이 이미 이것이 드러난 구체적 사례다.

### 향후 개선 사항

- 위 필수 항목 이상으로 제안된 것은 없다. 이슈 #310 자신의 수용 기준이 이미 남은 것을 나열하고 있다.

---

## 부록

### A. 테스트 결과

- `cargo fmt --check`: 클린. `cargo clippy --lib --tests -- -D warnings`: 클린. `cargo clippy --bin all-smi -- -D warnings`: 바이너리 대상 확인에서만 잡힌 `SERVICE_NAME` 죽은 코드 발견을 고친 뒤 클린.
- `cargo test --lib service_cmd::`: 110개 통과. `cargo test --lib common::paths`: 17개 통과.
- 렌더링된 plist에 대한 `plutil -lint`: 클린. `man ./docs/man/all-smi.1`이 깨끗이 렌더링됨.
- M1 Ultra, macOS 26.6, **사용자 스코프**에서 실시간: 설치 전 `status`는 종료 3을 냄. `install --user`가 plist를 쓰고 `~/Library/Logs/all-smi`를 만들고 잡을 언로드 상태로 남기며, `status`는 `installed, enabled, stopped`(종료 3)를 보고함. `start`가 부트스트랩하고 `status`는 PID와 함께 실행 중을 보고함. `install --user --now`, `restart`, `stop`, `uninstall`, 그리고 도구 자신의 plist 위에 두 번째 `install`(멱등)까지 전부 검증됨. `curl localhost:9090/metrics`가 LaunchAgent에서 `all_smi_*` 72줄을 서빙함. `stop`(`launchctl bootout`)이 최종 에너지 WAL 플러시에 도달하며 로그에 보임. 손으로 작성한 plist는 `install`과 `uninstall` 둘 다에 거부되고 그대로 남으며, `--force`가 거부를 풀어줌. `launchctl disable`은 `"enabled": false`로 보고되고 이후 `install`이 이를 지움. 시스템 스코프는 루트 없이 "requires root"로 거부하고 `/Library/LaunchDaemons`에 아무것도 쓰지 않음. `all-smi config path`가 `/Library/Application Support/all-smi/config.toml`을 마지막 후보로 나열함.
- launchd 아래의 애플 실리콘 지표: LaunchAgent의 지표 이름 집합이 포그라운드 `all-smi api` 실행과 바이트 단위로 동일함(정렬된 지표 이름의 `diff`: 차이 없음). 실시간으로 캡처한 샘플 값: `all_smi_gpu_utilization 17.19`, `all_smi_gpu_power_consumption_watts 0.56`, `all_smi_gpu_temperature_celsius 52`, `all_smi_cpu_temperature_celsius 65`, `all_smi_ane_power_watts 0`, `all_smi_thermal_pressure_info 1`, `all_smi_cpu_p_cluster_frequency_mhz 3223`, `all_smi_cpu_e_cluster_frequency_mhz 1978`, `all_smi_chassis_power_watts 48.12`. 클러스터 주파수가 0이 아닌 것은 PR #317 덕분임.
- 시작 비용 측정: 포그라운드 바인드 0.6초, `taskpolicy -b` 아래 2.9초, LaunchAgent로는 6.3초. 백그라운드 QoS로 도는 IOReport 채널 열거가 이를 지배함.
- 외부 볼륨 발견: 외부 볼륨의 바이너리를 가리키는 plist는 launchd 아래에서 `dyld`의 `open()`에 무한정 멈췄음. 내부 디스크 경로를 대신 가리켜서 코드 문제가 아님을 재확인함.

### B. 성능 벤치마크

위 `ProcessType Background` 시작 비용 측정이 이 PR의 유일한 정량적 벤치마크다. 정식 벤치마크 스위트라기보다 정성적인 것을 정량화한 것으로, `service status`가 실행 중이라고 보고하는 시점과 익스포터가 실제로 데이터를 담아 `/metrics`에 응답하는 시점 사이의 그렇지 않으면 놀라운 지연을 설명하려고 특별히 모은 것이다.

### C. 참고 자료

- `launchd.plist(5)`: `UserName`, `GroupName`, `InitGroups`, `HardResourceLimits`, `SoftResourceLimits`의 문서화된(그리고 여기서 측정한 조용한 무시 동작에 대해서는 문서화되지 않은) 시맨틱스.
- `launchctl(1)`: `bootstrap`, `bootout`, `enable`, `disable`, `print`, `print-disabled`.
- Homebrew의 `service do` DSL과 `Homebrew::Service#process_type`.
- 이슈 #310: 이 PR이 닫는 이슈.
- PR #319 보고서: 이 PR의 초기(이후 수정된) 추론이 끌어온 `ProtectKernelModules=` 발견.
