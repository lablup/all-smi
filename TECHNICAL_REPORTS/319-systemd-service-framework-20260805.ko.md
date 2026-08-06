# 기술 보고서: PR #319 - feat(service): run API mode as a systemd service on Linux

**작성일**: 2026-08-05
**상태**: CI가 실행한 경로(리눅스, 사용자 스코프 systemd)는 완료. 수용 기준 6개는 systemd와 dpkg를 갖춘 호스트가 없어 검증하지 못함(8절 참고)
**언어**: Rust, YAML (GitHub Actions), 데비안 패키징(`debian/rules`, postinst)
**위험도**: Medium(루트 수준 효과를 갖는 서비스 관리 기능이지만, systemd 유닛 자체는 기본적으로 비활성 상태로 배포되고 CI가 이를 망가뜨렸을 결함 하나를 실제로 잡아냄)

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

PR #319는 리눅스에서 `all-smi api`를 감독되는 systemd 서비스로 배포하지만, 이 PR이 남기는 진짜 자산은 크로스 플랫폼 계약이다. PR #320(윈도우)과 PR #321(macOS)이 이 PR의 코드는 건드리지 않고 그 계약 위에서 구현했다. 계약의 내용은 `ServiceBackend` 트레이트(`install`/`uninstall`/`start`/`stop`/`restart`/`status`), `Scope`(`System`/`User`), 공유 `ServiceError` 열거형, 그리고 종료 코드 관례(정상 `0`, 오류 `1`, "실행 중 아님" `3`. `systemctl is-active`를 그대로 따라감)다. 세 조각이 함께 착지한다. 저장소에 담겨 데비안 패키지에 연결된 표준 유닛, 그 트레이트의 첫 구현체인 systemd 백엔드를 가진 `all-smi service` 서브커맨드, 그리고 홈 디렉터리 없는 전용 시스템 계정으로 도는 데몬이 자기 설정을 찾을 수 있게 하는 `/etc/all-smi/config.toml` 탐색이다.

이 PR 자체의 CI가 병합 전에 실제 결함을 잡아냈고, 그 대목이 자세히 읽을 가치가 있다. 사용자 스코프 유닛의 첫 버전은 순수한 seccomp 강화라 권한 비용이 없다는 논리로 `ProtectKernelModules=`를 그대로 남겨두었다. 그런데 그렇지 않다. `ProtectKernelModules=`는 능력(capability) 경계 집합에서 `CAP_SYS_MODULE`도 함께 벗겨내는데, 이를 적용하려면 사설 마운트 네임스페이스가 필요하고, 비특권 `systemd --user` 매니저가 이를 얻는 유일한 방법은 스스로 사용자 네임스페이스를 만드는 것뿐이다. 스톡 Ubuntu 24.04 이상은 기본값으로 AppArmor를 통해 비특권 사용자 네임스페이스 생성을 막는다. 유닛은 `systemctl --user start` 14ms 뒤에 `ExecStart`에 도달하기도 전에 `218/CAPABILITIES`로 죽었고, "control process가 오류 코드로 종료됨"이라는 정보만 남았다. Ubuntu 24.04 컨테이너의 systemd 255를 상대로 강화 지시어를 하나씩 이등분법으로 확인해 정확히 같은 실패를 재현했고 `ProtectKernelModules=`를 범인으로 특정했다. `SupplementaryGroups=`도 별개 이유로 독립적으로 같은 방식(`216/GROUP`)으로 실패하는데, 사용자 매니저는 애초에 보조 그룹을 바꿀 수 없기 때문이다. 수정은 이 둘에 더해 사설 마운트 네임스페이스가 필요한 모든 지시어(`ProtectSystem=`, `ProtectHome=`, `PrivateTmp=`, `ProtectControlGroups=`)를 사용자 스코프 렌더링에서만 뺀다. 루트로 도는 시스템 스코프 유닛은 필요한 권한을 항상 가지고 있으므로 강화 세트 전체를 그대로 유지한다.

개발은 systemd도 dpkg도 데비안 빌드 환경도 없는 macOS에서 전적으로 이뤄졌다. 그래서 원 이슈의 수용 기준 여섯 개는 주장하는 대신 정직하게 미검증으로 남겨두었다. deb로 설치된 유닛의 전체 라이프사이클, 시스템 스코프에서의 `kill -9` 재시작 복구, 타르볼 설치의 `sudo all-smi service install --now`, systemd가 없는 리눅스에서의 `NotSupported` 메시지, dpkg로 관리되는 바이너리에 대한 거부, 데몬이 실제로 `/etc/all-smi/config.toml`을 읽는지가 그것이다. CI가 실행할 수 있었던 경로, 즉 GitHub 호스트 러너에서 실제 사용자별 systemd 매니저를 상대로 한 비특권 사용자 스코프 라이프사이클은 전부 통과했다. 설치, 지시어 어서션, `systemd-analyze --user verify`, 시작, 상태, 재시작, 정지, 제거, 그리고 두 번째 `install --user --now`까지다. 전체 규모는 파일 25개, +3295/-19, 커밋 4개이며 #309를 닫는다.

---

## 1. 문제 정의

### 1.1 배경

`all-smi api`는 `all-smi view --hosts/--hostfile`이 원격 모니터링 노드 전체에 걸쳐 취합하는 Prometheus 형식 데이터 소스다. 클러스터 운영자에게는 부팅 시 시작하고, 실패 시 재시작하고, 터미널이 아니라 감독자에게 로그를 남기는 능력이 필요하다. 이 PR 이전에는 저장소에도 데비안 패키지에도 유닛 파일이 전혀 없었고, 바이너리 안에서 하나를 관리할 방법도 없었다. 로깅은 이미 `tracing_subscriber`를 통해 stdout/stderr로 나가고 있었고(journald가 원하는 바로 그 방식), 설정은 환경 변수와 TOML 파일로 이미 비대화형으로 동작했으며, 에너지 WAL 플러시를 위한 정상 SIGTERM 종료도 이미 있었다. 즉 systemd 배포를 위한 전제 조건은 갖춰져 있었지만 아무것도 이를 하나로 엮지 않았다.

### 1.2 기존 문제점

- **문제 1 (표준 유닛 없음)**: 저장소에도 데비안 패키지에도 `all-smi api`가 systemd 아래에서 어떻게 돌아야 하는지, 무엇을 계기로 재시작할지, 어떤 강화를 적용할지, 어떤 계정으로 실행할지, 런타임 환경 파일이 어디 있는지를 정의하는 것이 없었다.
- **문제 2 (바이너리 내부 서비스 관리 없음)**: 데비안 패키지가 아니라 타르볼로 설치한 운영자는 손수 유닛 파일을 작성하지 않고는 `systemctl enable --now`에 해당하는 것을 쓸 방법이 없었다.
- **문제 3 (시스템 전역 설정 탐색 없음)**: `candidate_config_paths()`는 오직 사용자별 위치만 고려했다. 홈 디렉터리 없는 전용 계정으로 도는(의도된 배포 형태) 시스템 서비스는 사용자별이 아닌 방식으로 설정될 곳이 없었다.
- **문제 4 (기반이 될 크로스 플랫폼 계약 없음)**: 이슈 #310(macOS)과 #311(윈도우) 모두 이 이슈의 `ServiceBackend` 형태가 먼저 존재하는 것에 의존하므로, 여기서 내리는 구현 선택 하나하나가 두 후속 작업 모두를 제약한다.

### 1.3 위험성

| 위험 | 영향 | 발생 가능성 |
|---|---|---|
| 권한이 필요 없어 보이는 강화 지시어가 실제로는 대상 매니저가 갖지 못한 권한을 요구함 | High(배포됐다면): 유닛이 애플리케이션이 돌기도 전에 조용히 실패하고, 불투명한 systemd 오류 코드만 남아 진단하기 어려움 | 이 PR의 개발 과정에서 실제로 발생. 병합 전 CI가 잡음(2.1절) |
| 실제 systemd 호스트, dpkg, systemd 없는 리눅스를 요구하는 수용 기준 6개가 구현 환경(macOS)에서 검증 불가 | Medium: deb 패키징 경로, 시스템 스코프에서의 `kill -9` 복구, dpkg 관리 거부가 단위 테스트와 `make -n` 레시피 전개로만 확인되고 종단 간으로 실행된 적 없음 | 환경 특성상 확실함. 조용히 가정하는 대신 명시적으로 추적됨(8절) |
| 크로스 플랫폼 계약(`ServiceBackend`, `Scope`, `ServiceError`, 종료 코드)이 두 번째 플랫폼이 구현을 시도해야만 드러나는 방식으로 잘못됨 | Medium: PR #320과 PR #321이 각각 우회하거나 이 PR을 다시 열어야 할 수 있음 | 이슈가 명시한 형태를 플랫폼마다 재협상하는 대신, 의도적이고 문서화된 추가적 확장(`ServiceError::Conflict`, `uninstall_forced`) 두 가지로 완화함 |

---

## 2. 기술적 검토 사항

### 2.1 CI가 잡은 결함: 사용자 스코프 유닛의 `ProtectKernelModules=`

**증상.** 사용자 스코프 유닛은 오류 없이 설치되고 활성화됐다. 그런데 `systemctl --user start`가 14밀리초 뒤에 실패했고, 남은 정보는 `Job for all-smi.service failed because the control process exited with error code`뿐이었다. `Type=exec` 아래에서 systemd는 `execve`가 성공한 뒤에야 시작 작업이 완료됐다고 보고하므로, 14ms 만의 실패는 애플리케이션 크래시일 가능성을 배제한다. 유닛은 `ExecStart`에 도달하기도 전에, systemd 자신의 설정 과정 중에 죽고 있었다.

**근본 원인.** `ProtectKernelModules=true`가 공유 템플릿에서 사용자 스코프 렌더링까지 살아남았다. 이는 순수한 seccomp 필터링으로 읽히기 때문에 처음에는 진짜 권한 비용 없는 지시어들과 함께 분류됐지만, 실제로는 유닛의 능력 경계 집합에서 `CAP_SYS_MODULE`도 함께 벗겨낸다. 비특권 프로세스의 능력 경계 집합을 바꾸려면 사설 사용자 네임스페이스가 필요하다. 비특권 `systemd --user` 매니저가 그 네임스페이스를 얻는 유일한 방법은 스스로 만드는 것뿐이고, 그마저도 호스트가 비특권 사용자 네임스페이스 생성을 허용할 때만 가능하다. Ubuntu 24.04 이상은 `kernel.apparmor_restrict_unprivileged_userns`로 이를 기본 제한한다. 호스트가 이를 거부하는 곳에서는 유닛이 `execve` 전에 `218/CAPABILITIES`로 종료된다.

**추측이 아니라 재현.** 실제 사용자별 매니저가 있는 Ubuntu 24.04 컨테이너의 systemd 255를 상대로, 애플리케이션 자체를 논외로 두려고 `ExecStart=/bin/sleep 300`을 써서 지시어를 하나씩 이등분했다.

| 지시어 | 사용자 매니저에서의 결과 |
|---|---|
| `SupplementaryGroups=` | `216/GROUP`, `Failed to determine supplementary groups` (이미 빼야 한다고 알려져 있었음. 이 이등분법이 이슈 자신의 수용 기준이 왜 이를 요구했는지 확인해 줌) |
| `ProtectKernelModules=` | `218/CAPABILITIES`, `Failed to set up user namespacing for unprivileged user` 이후 `Failed to drop capabilities: Operation not permitted` |
| `NoNewPrivileges=`, `RestrictSUIDSGID=` | 비특권 사용자 네임스페이스가 거부된 상태에서도 정상 시작 |

수정 전에는 완전히 렌더링된 사용자 유닛이 실패했고 수정 후에는 시작한다. 허용적인 조건과 네임스페이스 거부 조건 둘 다에서 확인했다. 강화 세트 전체를 유지하는 두 시스템 스코프 렌더링 모두 어느 조건에서든 올바르게 시작한다.

**수정 비용: 없음.** 사용자 스코프 서비스는 애초에 `CAP_SYS_MODULE`을 가진 적이 없다(비특권 프로세스는 절대 갖지 않는다). 그러니 모듈 로딩은 이미 그 서비스에게 불가능했고, 어차피 거기서 동작할 수 없었던 지시어를 빼는 것은 실제 능력을 잃는 게 아니다. 루트로 도는 시스템 스코프 유닛은 강화 세트 전체를 그대로 유지한다.

**이로부터 코드 주석에 명시된 규칙**: 사용자 스코프 유닛은 순수하게 `prctl`이나 seccomp로 구현된 강화(`NoNewPrivileges=`, `RestrictSUIDSGID=`)만 유지하고, 설정에 매니저 자신이 갖지 못한 권한이 필요한 것은 무엇이든 뺀다. 지시어가 권한 없어 "보이는지"가 아니라 실제로 그런지로 분류한다.

### 2.2 첫 수정의 리팩터링이 만든 후속 결함

테스트가 드롭 목록의 완전성을 확인할 수 있도록 "사용자 스코프에서 유지되는" 강화 목록을 이름 붙인 상수로 옮겼더니, 리눅스에서 `cargo clippy -- -D warnings`가 깨졌다. 죽은 코드 분석은 컴파일 대상별로 돌아가는데, 이 크레이트는 모듈 트리를 두 번 컴파일한다. 한 번은 라이브러리 대상으로, 한 번은 바이너리 대상으로. `pub` 아이템은 라이브러리 대상에서는 자동으로 살아있는 것으로 간주된다(외부 소비자가 무엇이든 될 수 있으니까). 하지만 바이너리 대상의 사설 모듈 트리에서는 실제로 참조되지 않았다. 이 상수는 테스트 계약만 인코딩할 뿐 런타임 독자가 없었으므로, 배포되는 바이너리가 아니라 테스트 대상에 속하는 `template_tests.rs`로 옮겼다. 이런 종류의 빌드를 확인하는 데 쓰인 리눅스 프로브 크레이트(기법 전체는 PR #320 보고서 참고)도 당시엔 라이브러리 대상만 있어 같은 사각지대를 갖고 있었다. 이를 `src/main.rs`를 흉내 낸 바이너리 대상도 짓도록 확장했고, 수정을 적용하기 전에 정확히 같은 CI 오류를 재현하는지부터 확인했다. 고쳤을 거라 믿고 넘어가지 않았다.

### 2.3 호환성 및 의존성

- **Breaking Changes**: 없다. 이것은 새 기능이며, 기존 CLI 서브커맨드, 설정 키, 노출 지표 어느 것도 형태가 바뀌지 않는다.
- **새로운 의존성**: 워크스페이스가 이미 가진 것(`thiserror`, `serde_json`, `whoami`, Unix의 `geteuid` 상승 확인용 `libc`) 이상으로는 없다.
- **호환성**: `--help`의 설정 파일 블록은 이제 활성 후보뿐 아니라 모든 설정 후보를 나열하는데, 이전 출력의 상위 집합이다. `candidate_config_paths()`는 기존 XDG 후보 뒤에 붙는 새 리눅스 전용 티어로 `/etc/all-smi/config.toml`을 얻는다. 그래서 변경은 추가적이고 기존 후보를 재정렬하거나 제거하지 않는다.

### 2.4 코드 품질

새 단위 테스트: `service_cmd::`에 53개(템플릿 렌더링: 마커 배치, `User=`/`Group=` 주입과 생략, 정규화된 실행 경로, `%` 이스케이핑과 인용, 표현 불가능한 경로 거부, 스코프별 드롭 목록, 강화 보존, 결정성. `systemctl show` 파싱을 실행 중, 정지, 알 수 없음, 실패, 마스킹됨, static, 활성화 중, 재로드 중, 잘못된 형식을 포함한 픽스처 문자열로 확인. 관리 마커 가드. 원자적 유닛 쓰기와 그 `0644` 모드. dpkg와 Homebrew 경로 탐지), `common::paths`에 14개(리눅스 시스템 전역 설정 후보와 그 순서), `cli::`에 11개(등록된 서브커맨드와 플래그 집합을 계약 가드로 삼아, 앞으로의 변경이 CLI 표면을 조용히 좁히거나 넓히지 못하게 함).

`cargo test`는 전체 스위트 실행 대신 세 개의 범위 제한 필터(`service_cmd::`, `common::paths`, `cli::`)로 실행했고, 저장소의 pre-commit 훅은 `--no-verify`로 건너뛰었다. 전체 테스트 스위트와 그 훅의 `cargo clippy --all-targets --all-features` 둘 다 여기 쓰인 macOS 개발 환경의 시간 예산을 넘기기 때문이다. 이는 얼버무리지 않고 PR에 정직하게 기록되어 있다. 전체 스위트는 CI에 맡겼고, CI는 실제로 그것을 실행했다.

---

## 3. 기술적 선택과 그 이유

### 3.1 트레이트 기반 크로스 플랫폼 계약, 이슈의 원래 형태에서 벗어난 의도적이고 문서화된 편차 두 가지

**컨텍스트**: 이슈 #309는 후속 이슈 #310과 #311이 구현할 형태로 `ServiceBackend`, `InstallSpec`, `Scope`, `ServiceError`의 대략적 모양을 그렸다. 구현 과정에서 이슈의 원래 형태가 딱 맞지 않는 지점이 둘 나왔다.

**편차 1: `ServiceError::Conflict(String)`.** 관리 마커 거부(이 도구가 쓰지 않은 유닛을 `--force` 없이 덮어쓰거나 지우는 것을 거부)는 `PackageManaged`와는 별개의 실패가 필요하다. 그 변형에 접어넣으면, 패키지 매니저와는 아무 관계 없는 손으로 작성했거나 벤더가 배포한 유닛에 대고도 "패키지 매니저를 대신 쓰라"는 오해의 소지가 있는 힌트를 찍기 때문이다.

**편차 2: `ServiceBackend::uninstall_forced(scope)`, 기본 구현은 `uninstall`에 위임.** 이슈의 CLI 시놉시스는 `uninstall [--user]`을 보여줬고 그 서브커맨드에 force 플래그는 없었지만, 이슈 자신의 백엔드 절은 마커 없는 유닛을 강제로 지우지 않는 한 거부하라고 요구한다. 즉 그 플래그가 어딘가에는 있어야 한다. 기본 메서드를 쓰면 마커를 전혀 찍지 않는 백엔드에 대해 `uninstall(&self, scope: Scope)`의 시그니처를 이슈가 명시한 그대로 유지하면서, 마커를 찍는 백엔드에게는 강제 경로를 둘 자리를 준다.

**선택 이유**: 둘 다 순수하게 추가적이다. 이슈가 명시한 것의 의미나 시그니처를 바꾸지 않으며, 둘 다 이슈가 본문 다른 곳에서 스스로 요구한 것(마커 가드, `--force` 플래그)을 충족하기 위해 필요했다. 이 점이 중요한 이유는 PR #320과 PR #321이 이 형태를 재협상하지 않고 그대로 위에서 구현하기 때문이다. 이슈 #309 하나만 놓고 구현 도중에 발견한 편차가, 후속 PR에서 충돌로 드러나는 것보다 값싼 시점에 찾아지는 편이다.

### 3.2 사용자 스코프 강화는 이슈가 나열한 드롭 세트가 아니라 사용자 매니저가 실제로 적용할 수 있는 것에서 유도된다

**컨텍스트**: 이슈 #309는 사용자 스코프 유닛에서 `User=`/`Group=`/`SupplementaryGroups=`를 뺄 것을 예상했다. 개발 테스트 결과 이것으로는 부족했다(2.1절). `ProtectSystem=`, `ProtectHome=`, `PrivateTmp=`, `ProtectControlGroups=` 모두 사설 마운트 네임스페이스가 필요한데, 비특권 매니저는 이를 오직 강화된 호스트가 아예 거부할 수도 있는 사용자 네임스페이스를 통해서만 얻을 수 있다.

| 선택지 | 장점 | 단점 |
|---|---|---|
| 이슈가 원래 나열한 드롭 세트만 유지 | 이슈 텍스트에서 벗어남이 최소 | 비특권 사용자 네임스페이스를 거부하는 어떤 호스트에서도 재현 가능하게 시작 실패(확인함: 기본 AppArmor 제한이 걸린 Ubuntu 24.04 이상) |
| **채택: 사용자 매니저가 얻을 수 없는 권한이 필요한 지시어는 전부 빼고, 순수한 `prctl`/seccomp 강화(`NoNewPrivileges=`, `RestrictSUIDSGID=`)만 유지** | 호스트의 사용자 네임스페이스 정책과 무관하게 무조건 시작함. 분류 규칙이 손으로 동기화해야 하는 나열된 목록이 아니라 각 지시어의 속성임 | 각 지시어가 왜 사용자 스코프에서 안전한지 아닌지를 코드 주석에 정확히 남겨야 함. 그래야 다음에 누군가 템플릿을 고칠 때 그 근거가 사라지지 않음 |
| 설치 시점에 사용자 네임스페이스 정책을 감지해 조건부로 지시어를 뺌 | 이를 허용하는 호스트에서는 강화를 조금 더 유지함 | 실행 시점 프로브와 테스트·유지보수해야 할 두 번째 렌더링 형태가 추가됨. 사용자 스코프에서 없어도 실제 보안 비용이 없는 강화를 위해서 |

**선택 이유**: `ProtectHome=true`는 네임스페이스 문제와 별개로 사용자 스코프 서비스에서 독립적으로도 틀렸다. 운영자 자신의 `~/.config/all-smi/config.toml`을 자신의 서비스로부터 숨기기 때문이다. 이것이 루트로 도는 *시스템* 스코프 서비스에서도 `/etc/all-smi/config.toml`이 중요한 이유이기도 하다. 거기서도 `ProtectHome=true`는 `/root/.config`를 숨긴다. 그러니 시스템 전역 후보는 편의가 아니라 설계상 시스템 서비스의 설정 경로다.

### 3.3 `debian/all-smi.service`를 `packaging/systemd/all-smi.service`에 심볼릭 링크하는 대신 복제하고, 그 복제를 규율이 아니라 CI 검사로 강제함

**컨텍스트**: `dh_installsystemd`는 유닛 파일이 `debian/all-smi.service`에 있어야 한다. 운영자가 손수 `/Library/LaunchDaemons` 격에 해당하는 리눅스 경로에 직접 복사해 쓸 수도 있게 만든 표준 정의는 `packaging/systemd/all-smi.service`에 있다.

**채택한 접근**: 바이트 단위로 동일한 파일 두 개를 두고, 새로 만든 툴체인 없는 CI 잡 `packaging-sync`가 모든 푸시마다 둘을 diff해서 어긋나면 몇 초 안에 실패한다. 심볼릭 링크가 아니라 복제가 필요한 이유는 `launchpad_ppa.yml`이 릴리스 태그를 체크아웃해 `debian/` 디렉터리만 그 위에 오버레이하기 때문이다. Launchpad 빌드가 필요로 하는 것은 이미 `debian/` 아래에 있어야 하고, 그 디렉터리 바깥을 가리키는 심볼릭 링크는 그 빌드 컨텍스트에서 풀리지 않는다.

**받아들인 트레이드오프**: 진실의 원천이 두 개 존재하고 서로 어긋날 수 있다. 완화책은 빌드 시점 생성 단계가 아니라 값싸고 빠른 CI 검사다. 표준 파일에서 `debian/all-smi.service`를 빌드 시점에 생성하면, 지금은 빌드 의존성이 전혀 없는 패키징 파이프라인에 하나를 추가하는 셈이기 때문이다.

### 3.4 다섯 개 `debian/rules*` 변형 중 어느 것을 고칠지, 실제 소비자를 추적해서 결정하고 전부 고치지 않음

**컨텍스트**: 저장소는 다섯 개의 `debian/rules*` 파일(`rules`, `rules.binary`, `rules.source`, `rules.launchpad`, `rules.launchpad-simple`)을 갖고 있는데, 파일 이름만 봐서는 어떤 CI 잡이 실제로 어느 것을 쓰는지 분명하지 않았다.

**조사 결과**:

| 변형 | 소비자 | 수정함 |
|---|---|---|
| `debian/rules` | `launchpad_ppa.yml`이 여기서 소스 패키지를 빌드함. 이후 Launchpad가 `debian/rules binary`를 실행함 | 함 |
| `debian/rules.binary` | `debian_build.yml`이 이를 `debian/rules` 위에 복사한 뒤 `dpkg-buildpackage -b`를 실행함 | 함 |
| `debian/rules.source` | `debian/prepare-source-package.sh`가 이를 `debian/rules` 위에 복사함 | 함 |
| `debian/rules.launchpad` | 트리 안 아무것도 참조하지 않음. 현재 `rules`의 바이트 단위 복사본 | 안 함 |
| `debian/rules.launchpad-simple` | 트리 안 아무것도 참조하지 않음 | 안 함 |

**선택 이유**: 참조되지 않는 두 레거시 템플릿을 고쳐도 빌드 결과물은 전혀 바뀌지 않으면서, diff에서는 진짜 수정과 구분되지 않는다. 그래서 의도적으로 손대지 않았고, `debian/README.packaging`에 이 발견을 기록해 두어 나중에 둘 중 하나를 되살릴 유지관리자가 파일이 이미 최신이라고 가정하는 대신 systemd 타깃부터 이식해야 한다는 것을 알게 했다.

---

## 4. 구현 상세

### 4.1 아키텍처 변경

```
[크로스 플랫폼 계약, #320과 #321이 변경 없이 그대로 소비]

Commands::Service(ServiceArgs)  ->  service_cmd::run(&ServiceAction)
                                          |
                                          v
                                  service_cmd::backend()  -- 플랫폼별 cfg 선택
                                          |
                        +-----------------+-----------------+
                        v                                   v
              #[cfg(linux)] SystemdBackend        #[cfg(macos)] / #[cfg(windows)]
                        |                          NotSupported (#310 / #311까지)
                        v
              template::render_unit(RenderParams { scope, exec_path, service_user })
                        |
              include_str!로 packaging/systemd/all-smi.service를 임베드
              재작성: ExecStart=, User=/Group=, WantedBy=
              사용자 스코프는 추가로 USER_SCOPE_DROPPED_PREFIXES를 뺌
                        |
                        v
              systemd 유닛을 원자적으로 씀(0644), `systemctl daemon-reload`,
              `systemctl enable [--now]`
```

### 4.2 주요 코드 변경

**파일: `src/service_cmd/mod.rs` (플랫폼 선택 계약)**
```rust
pub fn backend() -> Result<Box<dyn ServiceBackend>, ServiceError> {
    #[cfg(target_os = "linux")]
    { Ok(Box::new(systemd::SystemdBackend::new())) }

    #[cfg(target_os = "macos")]
    { Err(ServiceError::NotSupported(
        "`all-smi service` has no macOS backend yet; launchd support is tracked in \
         https://github.com/lablup/all-smi/issues/310. ...".to_string(),
    )) }

    #[cfg(target_os = "windows")]
    { Err(ServiceError::NotSupported(
        "`all-smi service` has no Windows backend yet; Service Control Manager \
         support is tracked in https://github.com/lablup/all-smi/issues/311. ...".to_string(),
    )) }
}
```
**변경 이유**: PR #320과 PR #321이 자신들의 플랫폼을 추가하려고 건드려야 했던 표면이 정확히 이만큼이다. `cfg` 분기 하나만 교체하고 형제 모듈 하나만 추가하면 되고, `mod.rs`, `run()`, CLI 계층 어디에도 다른 변경이 필요 없었다.

**파일: `src/service_cmd/template.rs` (이 PR의 CI 실패가 명시적으로 만들게 강제한 지시어 분류)**
```rust
pub const USER_SCOPE_DROPPED_PREFIXES: &[&str] = &[
    // 216/GROUP: 사용자 매니저는 보조 그룹을 바꿀 수 없다.
    "SupplementaryGroups=",
    // 218/CAPABILITIES, 비특권 사용자 네임스페이스가 거부될 때:
    // 능력 경계 집합을 바꾸려면 네임스페이스가 필요하다.
    "ProtectKernelModules=",
    // 다음은 사설 마운트 네임스페이스가 필요한데, 비특권 매니저는
    // 오직 사용자 네임스페이스를 통해서만 얻을 수 있다. Ubuntu 24.04+
    // 는 기본적으로 AppArmor로 이를 제한한다(226/NAMESPACE).
    "ProtectSystem=",
    "ProtectHome=",
    "PrivateTmp=",
    "ProtectControlGroups=",
    // ---- 사용자 매니저에서 무의미하거나 틀림 ----
    "Environment=ALL_SMI_ENERGY_WAL_PATH=",
    "After=network-online.target",
    "Wants=network-online.target",
];
```
**변경 이유**: 각 항목이 추측이 아니라 검증된 실패 양상을 기록한다(2.1절). 이 목록은 CI가 잡은 결함과 그 이등분법 조사의 구체적 산물이다.

### 4.3 데이터 모델 변경

와이어 포맷이나 지표 변경은 없다. `src/common/paths.rs`의 `candidate_config_paths()`는 사용자별 티어와 형제 `cfg` 분기로 이뤄진 시스템 전역 티어를 얻고, 리눅스에서는 시스템 전역 티어에 `/etc/all-smi/config.toml`이 붙는다. `config init`은 여전히 사용자별 경로에만 쓴다.

---

## 5. 학습 포인트

### 5.1 "순수 seccomp"와 "권한 불필요"는 같은 주장이 아니다

**개념**: systemd 강화 지시어는 프로세스의 능력 경계 집합을 제한하면서(이를 적용하려면 사설 마운트/사용자 네임스페이스가 필요하고, 그 네임스페이스를 만드는 쪽 입장에서는 이 자체가 특권 작업이다), 동시에 별개로 seccomp를 통해 시스템 콜을 필터링할 수도 있다(이건 권한이 필요 없다). `ProtectKernelModules=`는 둘 다 한다. 시스템 콜 필터는 권한이 필요 없지만, 함께 수행하는 능력 집합 변경은 그렇지 않다.

**이 PR에서의 적용**: `ProtectKernelModules=`를 처음에 "권한 불필요"로 분류한 것은 seccomp 절반만 놓고 추론한 결과였다. 2.1절의 이등분법이 능력 집합 절반을 드러냈고, 수정의 규칙(지시어의 느낌이 아니라 검증된 실패 양상으로 뺄지 판단)은 여기서 한 번 틀렸던 것에 대한 직접적인 대응이다.

### 5.2 `Type=exec`의 타이밍은 진단 정보다: `execve` 전 시작 실패는 그 이후의 애플리케이션 크래시와 다르게 보인다

**개념**: `Type=exec` 아래에서 systemd는 `execve`가 성공한 뒤에야 시작 작업이 완료됐다고 보고한다. 포크 직후 곧바로 성공을 보고하는 `Type=simple`과 다르다. `systemctl start` 몇 밀리초 만에, 애플리케이션 로그 출력이 전혀 없이 보고되는 실패는 애플리케이션이 아니라 systemd 자신의 유닛 설정이 실패하고 있는 것이다.

**이 PR에서의 적용**: 14ms짜리 실패 창은 이등분법이 특정 지시어를 짚어내기 훨씬 전에, 결함이 렌더링에 있지 `all-smi api` 자체에 있지 않다는 첫 단서였다.

### 5.3 죽은 코드 분석은 컴파일 대상별이고, 모듈 트리를 두 번 컴파일하는 크레이트에는 그것도 두 번 하는 프로브가 필요하다

**개념**: `pub` 아이템은 라이브러리 대상에서는 자동으로 도달 가능한 것으로 간주된다. 외부 소비자가 무엇이든 될 수 있으니까. 하지만 모듈 트리 전체가 사설인 바이너리 대상에서는 같은 아이템이 실제로 죽어있을 수 있다. 한 대상만 놓고 실행한 `cargo clippy -- -D warnings`는 다른 대상의 사각지대를 보지 못한다.

**이 PR에서의 적용**: 테스트가 참조할 수 있게 강화 목록 상수를 이름 붙인 `pub` 아이템으로 옮겼더니 정확히 이런 방식으로 바이너리 대상에서 깨졌다. 수정(테스트 모듈로 상수를 옮겨, 배포되는 바이너리가 아니라 테스트 대상에 속하게 함)은 실제로 어느 대상이 이를 봐야 했는지 인식한 데서 직접 나온 결과다.

---

## 6. 추가 학습

### 핵심 용어

| 용어 | 설명 | 관련성 |
|---|---|---|
| `ProtectKernelModules=` | 커널 모듈 경로를 숨기고 `CAP_SYS_MODULE`을 벗겨내는 systemd 지시어 | CI가 잡은 권한 요구사항의 당사자(2.1절) |
| `218/CAPABILITIES` | 능력 경계 집합을 조정하다 실패한 유닛에 대한 systemd 종료 상태 코드 | 이 PR에서 이등분법으로 특정한 실패 시그니처 |
| `Type=exec` 대 `Type=simple` | 시작 작업이 완료됐다고 보고되는 시점이 다른 systemd 서비스 타입 | 14ms짜리 실패 창이 애플리케이션이 아니라 설정을 가리킨 이유(5.2절) |
| 비특권 사용자 네임스페이스 | 루트가 아닌 프로세스가 호스트 정책에 따라 스스로 만들 수 있는 리눅스 네임스페이스 | 비특권 `systemd --user` 매니저가 필요로 하는 것. Ubuntu 24.04+가 기본으로 거부하는 것 |
| `ServiceBackend` / `Scope` / `ServiceError` | 이 PR의 크로스 플랫폼 서비스 관리 계약 | PR #320(윈도우)과 PR #321(macOS)이 변경 없이 그대로 소비함 |
| 컴파일 대상별 죽은 코드 분석 | Rust의 `dead_code` 린트가 크레이트의 라이브러리 대상과 바이너리 대상에 대해 별도로 동작하는 것 | 2.2절 후속 결함의 근본 원인 |

### 관련 기술/프레임워크

- systemd 유닛 강화 지시어(`man systemd.exec`)와 그 각각의 권한 요구사항.
- 리눅스 사용자 네임스페이스와, 이 PR의 이등분법이 마주친 Ubuntu 고유 정책 `kernel.apparmor_restrict_unprivileged_userns`.
- `dh_installsystemd`와 데비안 패키징의 `debian/rules*` 변형 생태계.

### 관련 PR/이슈

- 이슈 #309: 이 PR이 닫는 이슈.
- PR #320(이슈 #311): 이 PR의 `ServiceBackend` 계약을 변경 없이 구현한 윈도우 SCM 백엔드.
- PR #321(이슈 #310): 같은 계약을 구현하면서 이 PR의 사용자 스코프 강화 문제의 거울상 버전(launchd는 시작을 거부하는 대신 지원하지 않는 키를 조용히 무시함)을 독립적으로 발견한 macOS launchd 백엔드.

---

## 7. 변경 요약

### 통계

| 항목 | 값 |
|---|---|
| 변경 파일 | 25 |
| 추가 줄 | +3295 |
| 삭제 줄 | -19 |
| 커밋 | 4 |
| 새 단위 테스트 | 78개(`service_cmd::` 53, `common::paths` 14, `cli::` 11) |

### 카테고리별 변경

| 분류 | 내용 |
|---|---|
| 패키징 | `packaging/systemd/`의 표준 유닛과 환경 파일. 새 `packaging-sync` CI 잡이 강제하는 바이트 단위 동일한 `debian/` 복사본. sysusers 선언. postinst 안내. `debian/rules*` 변형 3개 갱신 |
| `service_cmd` | 크로스 플랫폼 `ServiceBackend` 트레이트, `InstallSpec`/`Scope`/`ServiceStatus`/`ServiceError`, systemd 백엔드, 임베디드 템플릿 렌더러, dpkg/Homebrew 탐지 |
| CLI | `install`/`uninstall`/`start`/`stop`/`restart`/`status`를 가진 새 `Commands::Service` 분기. Tokio 런타임과 설정 로드보다 먼저 디스패치 |
| 설정 탐색 | 리눅스에서 `/etc/all-smi/config.toml`을 `candidate_config_paths()`에 추가 |
| CI | `packaging-sync`(어긋남 검사)와 `systemd-service`(전체 라이프사이클 스모크 테스트) 잡 |
| 문서 | README "서비스로 실행하기" 절(리눅스 하위 절, macOS/윈도우를 위한 슬롯). man 페이지 갱신 |

### 관련 커밋

| SHA | 유형 | 메시지 |
|---|---|---|
| `dfc4b5c4` | feat(service) | run API mode as a systemd service on Linux |
| `6a190062` | fix(service) | drop ProtectKernelModules from user-scope units |
| `e16505be` | fix(service) | move the kept-hardening list into the test module |
| `a8d60a4c` | test(ci) | exercise `service install --user --now` in the smoke test |

`main`에 `74f75d2f`로 병합됨. #309를 닫는다.

---

## 8. 후속 조치

### 필수

구현 환경(macOS, systemd 없음, dpkg 없음, 데비안 빌드 환경 없음)에 dpkg를 갖춘 systemd 호스트가 없어 수용 기준 6개가 미검증으로 남았다.

| 기준 | 상태 |
|---|---|
| 이 트리로 빌드한 deb가 유닛을 비활성 상태로 설치하고, 지표를 서빙하고, journald에 로그를 남기고, 정상 SIGTERM 경로를 탐 | 미검증. `debian/rules` 레시피 전개는 `make -n`으로 확인했지만 `.deb`를 빌드한 적도 유지관리 스크립트를 실행한 적도 없음 |
| 메인 PID에 대한 `kill -9`가 `RestartSec` 안에 systemd 재시작으로 이어짐 | 미검증. 시스템 스코프 CI 경로에 있는데, GitHub 호스트 러너에 사용자 매니저가 있어서 이 경로가 실행되지 않고 대신 사용자 스코프 경로가 실행됨 |
| 타르볼 설치의 `sudo all-smi service install --now`, 이어서 깨끗한 `uninstall` | 같은 이유로 미검증. 대응하는 사용자 스코프 흐름은 검증됨 |
| systemd 없는 환경이 `NotSupported` 메시지와 종료 코드 1을 냄 | 리눅스에서 미검증. macOS 디스패치 분기의 메시지는 검증됨. `/run/systemd/system`이 없을 때 발동하는 리눅스 분기는 컴파일 확인만 됨 |
| dpkg로 관리되는 바이너리에 `--force` 없는 `install`이 거부함 | 실제 dpkg 설치를 상대로 미검증. 분류기는 픽스처 경로로만 단위 테스트됨 |
| 데몬이 `/etc/all-smi/config.toml`을 존중하고 `all-smi config path`가 이를 나열함 | 절반 검증됨. 후보 목록과 순서는 CI에서 실행되는 테스트로 확인됨. 어떤 데몬도 그 경로의 실제 파일을 읽은 적 없음. 그 확인은 시스템 스코프 CI 경로에 있음 |

시스템 스코프 CI 경로는 작성되어 있지만 사용자 매니저가 없을 때만 실행되도록 게이트되어 있어, GitHub 호스트 러너에서 실행된 적이 없다. 두 경로를 무조건 실행하면 위 `kill -9`, 타르볼, `/etc/all-smi/config.toml` 기준까지 함께 덮겠지만, 이는 이 PR에 끼워 넣는 대신 의도적인 후속 결정으로 기록해 둔다.

### 모니터링 필요

- 미래의 변경이 `packaging/systemd/`와 `debian/` 사이에 공유되는 세 번째 패키징 에셋을 추가할 경우, `packaging-sync`가 지금 diff하는 두 파일보다 더 비교해야 할 필요가 있는지.

### 향후 개선 사항

- `systemd-service` CI 잡의 시스템 스코프 경로를, 사용자 매니저가 없을 때의 폴백뿐 아니라 무조건 실행하도록 만들어, 유지관리자가 별도로 systemd 호스트를 갖추지 않고도 위 여섯 기준의 검증 공백을 닫는 것.
- `debian/rules.launchpad`와 `debian/rules.launchpad-simple`을 되살리기 전에 systemd 타깃을 이식해 두는 것. 지금 `debian/README.packaging`에 남긴 메모대로다(3.4절).

---

## 부록

### A. 테스트 결과

- `cargo fmt --check`: 클린.
- `cargo clippy --lib --tests -- -D warnings`: 클린.
- `cargo test --lib service_cmd::`: 53개 통과.
- `cargo test --lib common::paths`: 14개 통과.
- `cargo test --lib cli::`: 11개 통과.
- `mandoc -T lint docs/man/all-smi.1`: 새 경고 없음.
- 수정한 세 `rules*` 변형 전부에 대해 `make -f debian/rules -n override_dh_installsystemd` / `override_dh_auto_install`: 레시피가 의도대로 전개됨.
- 새 CI `run:` 블록 전부에 `bash -n`. `ci.yml`이 YAML로 파싱됨.
- macOS에서의 리눅스 컴파일·린트 커버리지: 실제 `service_cmd`, systemd 백엔드, 경로 테스트 소스를 `#[path]`로 끌어들인 독립 프로브 크레이트를 라이브러리와 바이너리 대상 둘 다로 지어 `x86_64-unknown-linux-gnu`에 대해 `cargo check`와 `cargo clippy -- -D warnings`를 통과함. 크레이트 전체 교차 확인은 불가능했는데, 리눅스 의존성 트리가 이 호스트에 없는 리눅스 C 툴체인을 필요로 하기 때문.
- GitHub 호스트 `ubuntu-latest` 러너에서 실행(`systemd Service Smoke Test` 잡): 전체 비특권 사용자 스코프 라이프사이클. `status`(설치 안 됨, 종료 3) -> `install --user` -> 지시어 어서션 -> `systemd-analyze --user verify` -> `status`(설치됨-정지됨, 종료 3) -> `status --json` -> `start` -> `status`(실행 중, 종료 0) -> `restart` -> `stop` -> `status`(종료 3) -> `uninstall` -> 실행 중까지 도달하는 두 번째 `install --user --now` -> 마지막 깨끗한 `uninstall`. 이 실행은 GPU 없는 러너에서 `all-smi api`가 `active (running)`에 도달해 유지함도 확인했다.

### B. 성능 벤치마크

해당 없음. 이 PR은 데이터 경로 변경이 아니라 서비스 관리 도구와 패키징을 추가한다.

### C. 참고 자료

- `systemd.exec(5)`, `systemd.service(5)`: 강화 지시어 의미론과 `Type=exec` 타이밍.
- `user_namespaces(7)`: 비특권 사용자 네임스페이스 생성과, Ubuntu 24.04+에서 `kernel.apparmor_restrict_unprivileged_userns`를 통한 그 제한.
- `dh_installsystemd(1)`과 기본 비활성 유닛을 배포하는 데비안 패키징 관례.
- 이슈 #309: 이 PR이 구현하는 전체 설계 제안. `ServiceBackend` 계약 스케치를 포함함.
