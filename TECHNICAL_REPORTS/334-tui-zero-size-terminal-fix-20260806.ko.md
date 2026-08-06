# 기술 보고서: PR #334 - fix(tui): survive a terminal that reports no size (#326)

**작성일**: 2026-08-06
**상태**: 완료
**언어**: Rust
**위험도**: Low (산술 방어 로직과 정책 모듈 하나 추가. 단위 테스트뿐 아니라 실제 비정상 pty에서 종단 검증까지 완료)

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

`all-smi local`은 창 크기가 없는 pty를 받는 순간 곧바로 죽었다. `TIOCGWINSZ`가 0을 보고하고 `crossterm::terminal::size`는 그걸 그대로 `Ok((0, 0))`으로 돌려주며, 디버그 빌드의 `print_function_keys`에서 `rows - 1`이 언더플로를 일으켰다. 이슈는 두 지점(`chrome.rs:113`, `chrome.rs:80`)을 지목했는데, `src/ui`와 `src/view` 전체를 대상으로 차원 산술을 훑은 `ast-grep` 스윕이 이슈가 언급하지 않은 세 곳을 더 찾아냈다. `chrome.rs:43`은 10컬럼 아래에서 언더플로하고, `chrome.rs:49`는 정확히 10컬럼에서 0으로 나누며, `event_handler.rs:1220`은 마우스 클릭 경로에서 언더플로한다. 다섯 곳 모두 saturating 연산이나 가드로 고쳤다.

이 PR의 본질은 산술 수정만으로는 답할 수 없는 설계 판단에 있다. 보고된 크기 0이 실제로 무엇을 뜻하는지, 그리고 렌더링이 무의미할 만큼 작은 크기에서 TUI가 무엇을 해야 하는지다. `TIOCSWINSZ`를 받은 적 없는 pty는 크기를 통보받은 적이 없는 것이고, 그 반대편에는 거의 항상 평범한 터미널이 있다. 그래서 새로 만든 `ui::viewport` 모듈은 차원별로 0을 "기하 정보 없음"으로 취급하고, 환경변수 `$COLUMNS`/`$LINES`가 쓸 만한 값을 주면 그것을, 아니면 80x24를 대입한다. ncurses가 쓰는 것과 같은 폴백 순서다. 정말로 작은 터미널은 정반대로 취급한다. 12x2는 기하 정보가 빠진 게 아니라 운영자가 창을 그만큼 줄인 것이므로, 여기에 크기를 대입하면 거짓말이 된다. 그래서 실측한 최소값 20x3 아래에서는 루프가 한 줄짜리 안내문만 그리고 프레임 조립을 건너뛰며, 다음 `UiEvent::Resize`에서 자동으로 복구된다. 20컬럼이라는 하한은 취향으로 고른 숫자가 아니다. 렌더러 집합에서 검사 없는 폭 뺄셈 전부보다 위에 있는 값이고, 그 기준이 되는 건 14가 필요한 3게이지 GPU 행이다. `forkpty()` 하네스로 종단 검증했다(진짜 터미널은 문제가 없지만 샌드박스 안 에이전트 셸에는 제어 터미널이 없다). 수정 전 바이너리는 보고된 그대로 `chrome.rs:113:38`에서 패닉이 재현됐고, 수정 후 바이너리는 동일한 크기 0 조건에서 완전한 80x24 프레임을 렌더링했다. 반면 실제 12x2 pty는 다른 분기를 타서 깨진 프레임 대신 82바이트짜리 `12x2 < 20x3`을 출력했다. 전체 규모는 파일 6개, +745/-47, 커밋 1개, #326을 닫는다.

---

## 1. 문제 정의

### 1.1 배경

`script -q /dev/null ./all-smi local`(그리고 일부 CI 하네스, 특정 터미널 멀티플렉서의 예외 상황)은 창 크기를 통보받은 적 없는 pty를 프로세스에 넘긴다. 이런 pty에서 `TIOCGWINSZ`는 행과 열 모두 0을 보고하고, `crossterm::terminal::size()`는 이를 에러가 아니라 `Ok((0, 0))`으로 돌려준다. 그러니 0이 평범하게 읽어들인 값처럼 이를 소비하는 모든 렌더러로 그대로 흘러들어간다.

### 1.2 기존 문제점

- **문제 1 (보고된 패닉)**: `src/ui/chrome.rs:113`의 `print_function_keys`는 `cursor::MoveTo(0, rows - 1)`을 계산했는데 `rows: u16`이므로 `rows == 0`에서 언더플로가 나서 디버그 빌드가 죽었다.
- **문제 2 (이슈가 지목한 두 번째 지점)**: `src/ui/chrome.rs:80`의 `print_loading_indicator`는 `((rows - status_start_y) - 1).min(10)`을 계산했는데, `status_start_y`가 마지막 행에 이르렀거나 넘어서면 언더플로한다.
- **문제 3 (스윕이 찾아냈지만 이슈엔 없던 것)**: `src/ui/chrome.rs:43`의 `40.min(cols as usize - SCREEN_MARGIN)`은 10컬럼(`SCREEN_MARGIN`) 아래에서 언더플로한다.
- **문제 4 (스윕이 찾아낸 것)**: `src/ui/chrome.rs:49`의 `position % (bar_width as u64 * 2)`는 `cols == SCREEN_MARGIN`일 때, 즉 `bar_width`가 0으로 계산될 때 0으로 나눈다.
- **문제 5 (스윕이 찾아낸 것)**: `src/view/event_handler.rs:1220`의 `handle_process_header_click`은 마우스 클릭 경로에서 `half_rows - 1`을 계산하는데, 보고된 행이 0이나 1일 때 언더플로한다.
- **문제 6 (실제로 작은 터미널에 대한 정책 부재)**: 산술을 안전하게 만든 뒤에도, 말이 안 될 만큼 작지만 실제인 크기에서 무엇을 그려야 하는지 코드베이스 어디에도 결정된 게 없었다. saturating 연산만으로는 패닉을 막을 뿐 1~2행짜리 창에 여전히 깨진 프레임을 그리게 된다.
- **문제 7 (TUI를 자동화 세션으로 구동하기 어려웠음)**: PR #317의 TUI 검증 절차는 바로 이 패닉 때문에 `script` 아래에서 진짜 세션을 구동하지 못하고, 살아있는 `CpuInfo`에 대해 `print_cpu_info`를 직접 호출해야 했다. 이 수정 전까지는 렌더러를 종단으로 시험하기가 어려웠다.

### 1.3 위험성

| 위험 | 영향 | 발생 가능성 |
|---|---|---|
| 크기 없는 pty를 넘겨받는 어떤 환경이든 저하 대신 크래시함 | 그 환경 기준으로 High(시작 자체가 완전히 실패). 일반 대화형 터미널 기준으로는 0 | 이 수정 전에는 `script`, 일부 CI 하네스, 특정 멀티플렉서 예외 상황에서 확실히 발생 |
| 최소 렌더 가능 크기를 정하지 않고 산술만 saturating으로 처리 | Medium: 패닉은 멈추지만 1행이나 1열짜리 터미널에 못 쓰거나 깨진 프레임을 그리게 됨 | 산술 수정을 `ui::viewport` 정책과 20x3 하한과 함께 짝지어서 회피 |
| 정말 작은 터미널(12x2)을 기하 정보가 빠진 pty(0x0)와 똑같이 취급 | Medium: 실제로, 의도적으로 좁힌 창에 크기를 대입해서 그리면 운영자가 원치 않은 내용을 렌더링하거나 창이 너무 작다는 사실 자체를 감추게 됨 | 분리로 회피: 0은 대체되고, 하한 아래의 0이 아닌 실제 크기는 믿고 안내문을 보여줌 |

---

## 2. 기술적 검토 사항

### 2.1 정확성

패닉 방어 수정과 크기 정책 결정은 의도적으로 층을 나눴다. `chrome.rs`의 가드(`print_loading_indicator`의 `if cols == 0 || rows == 0 { return; }`, `print_function_keys`의 `if rows == 0 { return; }`, 그리고 전반적인 saturating 뺄셈)는 호출자가 누구든 렌더링 함수 자체가 패닉하지 않도록 하는 바닥이다. 12x2 같은 실제-하지만-작은 크기에서 무엇을 할지라는 정책 질문은 각 가드 지점의 코드 주석에서 명시적으로 `ui::viewport`에 위임된다. 그래서 두 층이 조용히 어긋날 여지가 없다. `Viewport`를 거쳐 기하 정보를 얻는 호출자는 애초에 `chrome.rs`에 0을 넘기지 않는다(0은 대체될 뿐, `chrome.rs`가 받을 수 있는 입력에서 사라지는 건 아니라서 패닉 방어 바닥은 독립적으로 계속 의미가 있다).

스윕이 커버리지를 주장하는 방식은 반증 가능했고, 그냥 가정한 게 아니라 실제로 확인했다. `src/ui`와 `src/view` 전체에서 `$A - $B`, `$A / $B`, `$A % $B`를 차원 관련 피연산자로 필터링해 훑은 `ast-grep`은 이슈가 지목한 두 곳과 세 곳을 더 찾아냈다. PR은 스윕이 찾아낸 나머지 매치 전부를 기록하고 각각이 이미 안전한 구체적 이유(`.min()`으로 상한 걸림, 조기 반환으로 가드됨, 음수가 될 수 없는 루프 경계, `#[allow(dead_code)]`)를 밝힌다. 렌더러 집합 전체의 산술식을 조용히 다 넓혀버리는 대신 이렇게 처리했다. 게이지 렌더러 여덟 지점(`gpu_renderer.rs`, `cpu_renderer.rs`, `chassis_renderer.rs`, `storage_renderer.rs`, `help.rs:348`)은 검사 없는 뺄셈을 그대로 두고 보고만 했는데, 이 PR이 세운 20컬럼 하한 아래에서는 도달 불가능하다는 근거에서다. 이는 단언만이 아니라 `frame_renderer.rs`의 새 populated-snapshot 스윕으로 검증됐다.

### 2.2 성능 관점

`Viewport::resolve`와 `Viewport::current`는 `ui_loop.rs`의 렌더 루프에서 프레임당 한 번 호출되며, `terminal::size()` 직접 호출을 대체하되 차원별 폴백 로직 이상의 비용은 추가하지 않는다. 환경변수(`std::env::var`)는 0인 경로에서만 조회하므로, 평범한 경우(터미널이 실제 크기를 보고하는 경우)에는 추가 비용이 없다. 새로운 할당, 락, 백그라운드 작업은 생기지 않는다.

### 2.3 호환성 및 의존성

- **Breaking Changes**: CLI나 지표 표면에는 없다. 이 PR은 `src/ui/`와 `src/view/`에 한정된다.
- **새로운 의존성**: 없다. `ui::viewport`는 이미 의존성인 `crossterm::terminal`과 `std::env`만 쓴다.
- **호환성**: `src/view/event_handler.rs`와 `src/view/ui_loop.rs`는 `terminal::size()`/`size()` 직접 호출 네 곳(ioctl 실패 시 죽었을 `.unwrap()` 세 개, `Err(_) => return Err(...)` 하나)을 `Viewport::current()`로 대체한다. `Viewport::current()`는 ioctl 실패를 하드 에러로 전파하지 않고, 읽기 실패를 보고된 0과 똑같이 취급해서 폴백한다.

### 2.4 코드 품질

회귀 커버리지는 폭만이 아니라 두 축 모두를 의도적으로 다룬다. 보고된 패닉은 행에서 났으므로 폭만 훑었다면 이걸 놓쳤을 것이다. `chrome.rs`의 새 테스트 모듈은 `DEGENERATE_SIZES`라는 공유 표를 써서 `print_function_keys`와 `print_loading_indicator`를 열한 가지 비정상 기하에 대해 구동한다. `(0, 24)`와 `(80, 0)`의 혼합 사례, `cols == SCREEN_MARGIN`인 폭 0짜리 바 사례를 포함한다. `frame_renderer.rs`의 새 테스트는 20x3부터 32x9까지 모든 크기를 `render_main`, `render_loading`, `render_help`, `render_alert_panel`에 걸쳐 GPU 두 개(하나는 3게이지 애플 실리콘 레이아웃, 즉 렌더러 집합에서 가장 폭에 민감한 행을 고르도록 일부러 이름 붙임)와 CPU 하나를 담은 스냅샷으로 훑는다. 이게 바로 20컬럼 하한이 게이지 레이아웃 절벽을 실제로 넘긴다는 걸 시험하는 부분이지, 스위트 다른 곳에서 쓰는 빈 스냅샷 스모크 테스트만으로는 확인되지 않는다. `ui::viewport` 자체의 테스트 열한 개는 차원별 대체, 환경변수 파싱(형식이 잘못됐거나 범위를 벗어난 값 포함), 렌더 가능 하한의 경계 포함 여부, 너무 작은 안내문의 폭 맞춤 저하(긴 문장에서 짧은 형태로, 최후에는 문자 단위 절단으로)를 다룬다.

PR 본문은 `cargo clippy --bin all-smi --tests -- -D warnings`를 라이브러리 타깃 실행과 별개로 돌렸다고 따로 기록한다. 이 크레이트가 모듈 트리를 두 번 컴파일하기 때문이고, 따로 돌린 결과 라이브러리 타깃에서는 살아있고 바이너리 타깃에서는 죽은 `pub` 아이템을 잡아냈다고 한다. PR #319의 보고서가 다른 심벌에 대해 문서화한 것과 같은 유형의 결함이다. 다만 병합된 diff에는 무엇을 고쳤는지 흔적이 없는데, 이 PR의 단일 커밋보다 그 수정이 앞서 있었기 때문으로 보인다. 이 보고서는 diff로는 이를 확인하지 못했고, 그 사실을 독자에게 알린다(8절 참고).

---

## 3. 기술적 선택과 그 이유

### 3.1 0은 "기하 정보 없음"을 뜻하지 "크기가 0인 터미널"을 뜻하지 않는다

**컨텍스트**: `TIOCSWINSZ` 없이 할당된 pty는 `(0, 0)`을 보고하고, `crossterm::terminal::size`는 이를 에러가 아니라 성공한 읽기로 돌려준다. 그러니 값만 봐서는 "기하 정보가 설정된 적 없음"과 "정말로 행이나 열이 0인 터미널"을 하위 어디서도 구분할 수 없다.

| 옵션 | 장점 | 단점 |
|---|---|---|
| 크기 (0, 0)에서는 아무것도 그리지 않음 | 가장 단순하고 값 그대로에 부합 | `script`나 비슷한 하네스 아래에서 TUI를 구동할 수 없는 상태가 계속되며, 이게 바로 PR #317의 TUI 검증이 실제 세션을 쓰지 못하게 만든 원인 |
| **채택: 쓸 만하면 `$COLUMNS`/`$LINES`, 아니면 80x24를 차원별로 대입** | ncurses 자체의 폴백 순서와 일치. `script` 아래에서도 TUI를 구동 가능하게 함. 크기 없는 pty의 반대편에는 거의 항상 평범한 터미널이 있으니, 그리기를 거부하는 건 신호를 잘못 읽는 것 | 정말로 아주 작지만 우연히 정확히 0으로 보고되는 차원(실제 하드웨어에서는 불가능하지만, 버그가 있거나 악의적인 pty라면 상상 가능)은 믿는 대신 없는 것으로 취급됨 |
| 리사이즈 이벤트가 실제 기하를 줄 때까지 종료하거나 대기 | 크기를 추측할 필요가 아예 없음 | 창이 작다고 종료하거나 멈추는 TUI는 없다. 합리적인 추정으로 그리는 것보다 UX가 나쁨 |

**선택 이유**: 폴백은 전부-아니면-전무가 아니라 차원별이다. 실제 폭은 보고했지만 높이는 보고하지 않은 터미널은 자신이 보고한 폭을 그대로 유지하고 높이만 폴백한다. 이것이 이 대체를 "작음"에 대한 광범위한 재정의가 아니라 "없음"에 대한 좁은 패치로 만드는 지점이다.

### 3.2 정말로 작은 터미널은 대체하지 않고 믿는다

**컨텍스트**: 0을 "없음"으로 취급하기로 한 순간, 정반대 경우, 즉 운영자가 창을 그만큼 줄여서 정말로 작은 0이 아닌 크기(가령 12x2)를 보고하는 터미널에는 그 나름의 정책이 필요해진다. 여기에 폴백 크기를 대입하면 운영자가 그 크기에서 요청한 적 없는 내용을 렌더링하게 된다.

| 옵션 | 장점 | 단점 |
|---|---|---|
| 어떤 문턱 아래에서도 폴백 크기로 대체 | "없음"과 "작음"을 코드 경로 하나가 다 처리함 | 거짓말을 그리는 셈이다. 운영자의 창은 실제로 12x2인데 여기에 80컬럼 프레임을 그리면 깨지거나 그냥 틀리게 나옴 |
| **채택: `MIN_COLS`x`MIN_ROWS` 아래에서는 한 줄짜리 안내문만 그리고 조립은 건너뜀. `UiEvent::Resize`에서 자동 복구** | 터미널의 실제 상태를 정직하게 반영함. 루프가 리사이즈 이벤트를 이미 처리하므로 복구는 공짜 | 하한을 정하고 그 근거를 방어해야 함(3.3절) |
| 하한 아래에서 프로세스를 종료 | 단순함 | 창을 좁혀서 종료하는 TUI는 없음. PR에서 이 이유로 명시적으로 기각됨 |
| 하한 아래에서 렌더링을 멈추고 대기만 함 | 안내문과 비슷한 비용이지만 왜 아무것도 안 나오는지 운영자에게 알리지 않음 | 한 줄 안내문을 보여주는 것보다 명백히 나쁘다고 판단해 기각 |

**선택 이유**: 안내문은 운영자에게 상황을 설명하기도 하고(`too_small_notice()`가 요구 사항과 실제 크기를 밝히며, 그마저 안 들어가면 짧은 형태로, 최후에는 문자 단위 절단으로 저하한다), 새로운 이벤트 처리 장치가 필요하지도 않다. `UiEvent::Resize`가 이미 렌더 루프를 깨우기 때문이다.

### 3.3 최소 크기는 취향이 아니라 렌더러 집합을 기준으로 실측했다

**컨텍스트**: TUI가 프레임 조립을 거부하는 하한은 구체적 근거가 있어야 한다. 그렇지 않으면 미래의 렌더러 변경이 검증 안 된 그 가정을 조용히 무효화할 수 있는 임의의 숫자가 되어버린다.

**발견한 사실**: `MIN_COLS = 20`은 스윕(2.1절) 이후 남은 검사 없는 폭 뺄셈 전부보다 위에 있다. 기준이 되는 제약은 `gpu_renderer.rs`의 3게이지 GPU 행으로, `width.saturating_sub(10)` 위에서 `available_width - (num_gauges - 1) * 2`를 계산하므로 `width >= 14`가 필요하다. 애플 실리콘 CPU 행은 12가 필요하고, 섀시·스토리지·단일 게이지 CPU 행은 5가 필요하다. 20은 마침 가장 짧은 상태 바 텍스트(`h:Help q:Exit`, 13컬럼)가 화면 전체 줄이 되는 걸 멈추는 지점이기도 하다. `MIN_ROWS = 3`은 헤더 한 행, 콘텐츠 한 행, 그리고 항상 마지막 행을 차지하는 상태 바다. 2행이었다면 사이에 아무것도 없는 순수 크롬만 남았을 것이다.

**단언이 아니라 검증**: `frame_renderer.rs`의 새 테스트는 20x3부터 32x9까지를 채워진 스냅샷(빈 스냅샷이 아니라 실제 GPU와 CPU 행)에 대해 모든 렌더 경로(`render_main`, `render_loading`, `render_help`, `render_alert_panel`)로 훑는다. 하한이 게이지 절벽을 실제로 넘긴다는 걸 증명하는 건 이 부분이지, 주석에 숫자를 적어두는 것만으로는 안 된다.

---

## 4. 구현 상세

### 4.1 아키텍처 변경

```
[변경 전]
terminal::size() -> (cols, rows), 어쩌면 (0, 0)
    │
    ▼
chrome.rs / event_handler.rs: cols/rows에 대한 원시 산술
    │
    ▼
rows == 0에서 rows - 1이 언더플로 -> 패닉

[변경 후]
terminal::size() -> (raw_cols, raw_rows)
    │
    ▼
Viewport::resolve(raw_cols, raw_rows)
    │  차원별: raw > 0 ? raw : $COLUMNS/$LINES ? 그 값 : FALLBACK (80x24)
    ▼
Viewport { cols, rows }   -- 더는 (0, N)이나 (N, 0)이 나오지 않음
    │
    ├─ is_renderable()? (>= 20x3)
    │     아니오 -> too_small_notice() 렌더링, 조립 건너뜀, Resize 대기
    │     예     -> 정상 프레임 조립
    ▼
chrome.rs: 내부적으로 여전히 saturating/가드 처리, 독립적인 패닉 방어 바닥으로
```

### 4.2 주요 코드 변경

**파일: `src/ui/viewport.rs`(신규, 크기 해석 정책)**
```rust
pub const MIN_COLS: u16 = 20;   // 검사 없는 폭 뺄셈 전부보다 위
pub const MIN_ROWS: u16 = 3;    // 헤더 + 콘텐츠 한 행 + 상태 바
pub const FALLBACK_COLS: u16 = 80;
pub const FALLBACK_ROWS: u16 = 24;

pub fn resolve(raw_cols: u16, raw_rows: u16) -> Self {
    Self {
        cols: resolve_dimension(raw_cols, "COLUMNS", FALLBACK_COLS),
        rows: resolve_dimension(raw_rows, "LINES", FALLBACK_ROWS),
    }
}

fn resolve_dimension(raw: u16, env_key: &str, fallback: u16) -> u16 {
    if raw > 0 {
        return raw;
    }
    dimension_from_env(std::env::var(env_key).ok().as_deref(), fallback)
}
```
**변경 이유**: 원시 터미널 크기가 TUI가 실제로 렌더링할 크기로 바뀌는 유일한 지점이다. 다른 모든 호출부(`ui_loop.rs`, `event_handler.rs`)는 이제 `crossterm::terminal::size()`를 직접 부르는 대신 이걸 거친다.

**파일: `src/ui/chrome.rs`(정책 층과 독립적인 패닉 방어 바닥)**
```rust
// A terminal with no cells has nowhere to put any of this, and every
// `MoveTo` below would address a position outside the window. Bail out
// rather than emit cursor motion into nothing (issue #326).
//
// This is only the panic-safety floor. The policy question of what to
// show on a terminal that is real but too small to be useful belongs to
// `ui::viewport`, which gates this function's callers well above 1x1.
if cols == 0 || rows == 0 {
    return;
}
```
**변경 이유**: `chrome.rs`의 함수들은 자기 자신 기준으로 0 입력에 안전해진다. 그래서 미래의 호출자가 `Viewport`를 우회하더라도 패닉할 수 없다.

**파일: `src/view/ui_loop.rs`(너무 작을 때의 분기와 복구)**
```rust
let viewport = Viewport::current();
let (cols, rows) = (viewport.cols, viewport.rows);

if !viewport.is_renderable() {
    if !self.previous_too_small {
        self.previous_too_small = true;
        self.view_cache.invalidate_all();
        self.differential_renderer.force_clear().ok();
    }
    if self
        .differential_renderer
        .render_differential(&viewport.too_small_notice(), cols, rows)
        .is_err()
    {
        break;
    }
    continue;
}

if self.previous_too_small {
    self.previous_too_small = false;
    self.view_cache.invalidate_all();
    if self.differential_renderer.force_clear().is_err() {
        break;
    }
}
```
**변경 이유**: `previous_too_small`이 양방향 전환을 추적한다. 안내문으로 들어갈 때는 화면을 강제로 지워서(캐시에 남은 이전 프레임 상태를 버림) 처리하고, 다시 빠져나올 때도 안내문이 남긴 줄 위에 덧그리는 대신 처음부터 정상 프레임을 다시 그리게 한다.

### 4.3 데이터 모델 변경

해당 없음. 지표, 설정, 와이어 포맷 변경은 없다. 이 PR은 터미널 렌더링에 한정된다.

---

## 5. 학습 포인트

### 5.1 구조적 스윕은 표적 수정이 찾지 못하는 것을 찾아낸다

**개념**: 버그 보고서가 지목한 지점만 정확히 고치면 보고된 증상은 잡히지만 결함 유형까지 잡히지는 않는다. 영향받는 서브시스템 전체에서 같은 *모양*의 표현식(차원 뺄셈, 나눗셈, 나머지)을 찾는 도구 보조 스윕이 있어야 "패닉을 고친다"가 "유형을 고친다"로 바뀐다.

**이 PR에서의 적용**: 이슈는 두 지점을 지목했다. `ast-grep` 스윕은 같은 모양을 가진 도달 가능한 결함 세 곳을 더 찾아냈고, 이미 안전한 매치 집합도 문서화해서 방어적으로 "고치는" 대신 그대로 남겨두었다. 덕분에 diff가 실제로 고장 난 부분에만 집중할 수 있었다.

### 5.2 같은 원시 값이 정반대 두 가지를 뜻할 수 있고, 이를 뒤섞으면 잘못된 수정이 나온다

**개념**: 크기 조회에서 나오는 `(0, 0)`은 "호출자가 커널에 이 창 크기를 알려준 적이 없다"를 뜻할 수도 있고, 원리상 "창이 정말로 크기가 0이다"를 뜻할 수도 있다. 둘을 항상 대체하거나 항상 믿는 식으로 똑같이 다루면, 두 경우 중 하나에서는 틀린다.

**이 PR에서의 적용**: 이 분리(0은 대체하고, 0이 아닌 작은 실제 크기는 믿는다)가 바로 산술 안전성을 넘어선 이 PR의 실질 내용이다. 12x2가 80x24 전체 프레임이 아니라 `12x2 < 20x3`을 렌더링하는 이유, `script`로 구동된 pty가 한 줄짜리 안내문이 아니라 전체 프레임을 렌더링하는 이유가 여기에 있다.

### 5.3 최소 크기 하한은 렌더러 집합에 대한 주장이고, 단언이 아니라 그 집합에 비추어 검증해야 한다

**개념**: 하드코딩된 최소 차원은 그것이 보호하는 실제 코드의 산술에서 도출되고 주기적으로 재검증될 때만 의미가 있다. 미래의 렌더러 변경이 검증되지 않은 가정을 조용히 무효화할 수 있기 때문이다.

**이 PR에서의 적용**: `MIN_COLS`/`MIN_ROWS`는 구체적인 렌더러 계산(3게이지 GPU 행의 `width >= 14`)을 근거로 주석에서 정당화되고, 하한 주변을 채워진 스냅샷으로 스윕해서 검증됐다. 단순히 상수로 문서화만 해둔 게 아니다.

---

## 6. 추가 학습

### 핵심 용어

| 용어 | 설명 | 관련성 |
|---|---|---|
| `TIOCGWINSZ` | 터미널 창 크기를 조회하는 ioctl | 크기를 통보받은 적 없는 pty에서 전부 0을 보고함. 이 PR이 다루는 근본 조건 |
| `Viewport` | 원시 터미널 기하를 TUI가 실제로 렌더링할 크기로 해석하는 신규 구조체(`ui::viewport`) | 이 PR 이후 터미널 크기의 단일 진입점. `terminal::size()` 직접 호출 네 곳을 대체함 |
| `MIN_COLS` / `MIN_ROWS` | 프레임을 조립하지 않는 실측 하한(20x3) | 임의로 고른 게 아니라 렌더러 집합 중 가장 폭에 민감한 것(3게이지 GPU 행)에서 도출됨 |
| Saturating 연산 | 오버플로/언더플로 대신 값을 고정하는 `u16::saturating_sub` 등 | 크기 정책 층 아래에 독립적으로 놓인 패닉 방어 층 |
| `ast-grep` | 구조적, 문법 인식 기반 코드 검색 도구 | 보고된 패닉을 낳은 차원 산술 모양을 찾기 위해 `src/ui`, `src/view` 전체를 스윕하는 데 사용 |
| `forkpty()` | 새 pty에 연결된 프로세스를 만드는 POSIX API | 제어 터미널이 없는 환경(에이전트의 샌드박스 셸)에서 종단 검증에 사용. `script`와 같은 크기 0 조건을 재현함 |

### 관련 기술/프레임워크

- `crossterm::terminal::size`, 그리고 크기 없는 pty에서 에러가 아니라 `Ok((0, 0))`을 돌려주는 동작.
- 터미널 기하에 대한 ncurses의 폴백 순서(`$COLUMNS`/`$LINES` 다음 관용적 기본값). 이 PR의 `Viewport::resolve`가 의도적으로 이를 그대로 따름.

### 관련 PR/이슈

- 이슈 #326: 이 PR이 닫는 이슈.
- PR #317: 그 TUI 검증 절이 이 패닉 때문에 실제 세션으로 렌더러를 시험하기 어려웠다는 직접적 증거다.
- PR #319: "라이브러리 타깃에서는 살아있고 바이너리 타깃에서는 죽은 `pub` 아이템"이라는 같은 유형의 `cargo clippy` 발견을 다른 심벌에 대해 문서화한 이전 보고서. 이 PR 본문도 같은 유형을 언급한다(구체적 확인은 8절 참고).
- PR #337: 이 PR이 (20컬럼 하한 아래에서는 도달 불가능하다는 이유로) 검사 없는 산술을 의도적으로 남겨둔 여덟 파일 중 하나인 `src/ui/renderers/gpu_renderer.rs`를 나중에 건드린다. PR #337 본문은 그쪽 diff가 값 표시와 게이지에 한정되고 이 PR이 그대로 둔 차원 산술은 건드리지 않는다고 밝힌다.

---

## 7. 변경 요약

### 통계

| 항목 | 값 |
|---|---|
| 변경 파일 | 6 |
| 추가 줄 | +745 |
| 삭제 줄 | -47 |
| 커밋 | 1 |
| 신규 파일 | `src/ui/viewport.rs` |

### 카테고리별 변경

| 분류 | 내용 |
|---|---|
| 정확성 | 차원 산술의 언더플로/0으로 나누기에 대해 다섯 지점 방어. 이슈가 지목한 두 곳과 스윕이 찾은 세 곳 |
| 신규 모듈 | `ui::viewport`: `Viewport::resolve`/`current`, `is_renderable`, `too_small_notice`, 실측한 `MIN_COLS`/`MIN_ROWS`/`FALLBACK_*` 상수 |
| 동작 | 20x3 아래에서 TUI는 프레임 대신 한 줄짜리 안내문을 렌더링하고 다음 리사이즈에서 자동 복구 |
| 리팩터링 | `event_handler.rs`와 `ui_loop.rs`의 `terminal::size()`/`size()` 직접 호출 네 곳을 `Viewport::current()`로 교체, ioctl 실패 시 죽을 수 있던 `.unwrap()` 세 개 제거 |
| 테스트 | `ui::chrome` 테스트 7개, `ui::viewport` 테스트 11개, 채워진 스냅샷을 20x3부터 32x9까지 훑는 `frame_renderer` 테스트 3개 신규 |

### 관련 커밋

| SHA | 유형 | 메시지 |
|---|---|---|
| `2503a736` | fix(tui) | survive a terminal that reports no size |

`main`에 `c4c17d8d`로 병합됨. #326을 닫는다.

---

## 8. 후속 조치

### 필수

아래의 미확인 항목 외에 PR에서 확인된 것은 없다.

### 모니터링 필요

- 너무 작다는 안내문에서 정상 프레임으로 복구하는 건 루프가 이미 처리하는 `UiEvent::Resize`에 의존한다. PR은 이 전환을 코드 검토와 전환 양쪽의 `force_clear` 호출로 검증했다고 밝히지만, 실제 세션 중간에 살아있는 pty를 리사이즈하는 건 검증 하네스가 필요로 한 범위를 벗어나서 종단으로 구동하지는 않았다.
- PR이 서술하는 개발 시간 예산 안에서는 전체 스위트 `cargo test`를 돌리지 않았다. 부록에 실린 범위 지정 실행이 이 PR이 건드리는 모듈 전부를 커버하고, 나머지는 CI가 돈다.

### 향후 개선 사항

- 검사 없는 산술을 그대로 둔 게이지 렌더러 여덟 지점(`gpu_renderer.rs`, `cpu_renderer.rs`, `chassis_renderer.rs`, `storage_renderer.rs`, `help.rs:348`)은 고치지 않고 보고만 했다. 여덟 곳을 다 방어하면 PR #337이 동시에 건드리던 파일 전반으로 diff가 넓어지는데, 20컬럼 게이트 아래에서는 어차피 동작이 바뀌지 않기 때문이다. 하한을 낮추게 되면 후속 작업할 가치가 있다.

**이 보고서가 diff로 뒷받침하지 못한 주장**: PR 본문은 `cargo clippy --bin all-smi --tests -- -D warnings`를 라이브러리 타깃 실행과 별개로 돌려서 "라이브러리 타깃에서는 살아있고 바이너리 타깃에서는 죽은 `pub` 아이템"을 잡았다고 서술한다. 이 유형의 발견은 PR #319의 보고서가 다른 심벌에 대해 문서화한 결함과 같은 종류지만, 이 PR의 병합된 diff에는 무엇을 고쳤는지 흔적이 전혀 없다(`ui/viewport.rs`에서 공개 생성자는 `resolve`와 `current`뿐이고, `pub fn new` 같은 게 제거된 흔적도 없다). 있었다면 그 수정이 이 PR의 단일 squash 커밋보다 앞섰기 때문일 것이다. 이 보고서는 PR 본문에 적힌 대로 "클립피가 타깃 간 죽은 코드를 잡았다"는 일반적 주장은 기록하되, 구체적 심벌은 코드 자체로 확인하지 못했다. 사실로 단정하지 않고 독자에게 알린다.

---

## 부록

### A. 테스트 결과

- `cargo test --lib ui::`: 559개 통과. 신규 `ui::chrome` 테스트 7개, `ui::viewport` 테스트 11개 포함.
- `cargo test --bin all-smi view::`: 123개 통과. `view`는 바이너리 타깃에만 있어서 `--lib`로는 컴파일되지 않는다. 신규 `frame_renderer` 테스트 3개가 여기 있다.
- `cargo clippy --lib --tests -- -D warnings`: 클린.
- `cargo clippy --bin all-smi --tests -- -D warnings`: 클린. 의도적으로 따로 돌림(8절의 미확인 주장 참고).
- `cargo fmt --check`: 클린.
- 종단 검증, `forkpty()` 하네스로 창 크기를 미설정 상태로 둠(`script -q /dev/null`과 동일한 조건): 수정 전 `1f540e1`에서 `[pty winsize: rows=0 cols=0]` 뒤에 `thread 'main' (32556106) panicked at src/ui/chrome.rs:113:38: attempt to subtract with overflow`가 뒤따랐고, 종료 상태는 25856(`101 << 8`)이었다. 수정 후 같은 하네스는 캡처한 이스케이프 스트림에서 재구성한 완전한 80x24 프레임을 렌더링했다. 실제 12x2 pty는 다른 분기를 타서 82바이트짜리 `12x2 < 20x3`을 출력했다. 1x1 pty는 들어맞는 문자 하나로 저하했다. 크기 0인 pty 위에서 `COLUMNS=100 LINES=12`를 주면 완전한 100x12 프레임이 렌더링되어, 환경변수 폴백 경로를 확인했다.

### B. 성능 벤치마크

별도로 벤치마크하지 않았다. `Viewport::current()`는 렌더 루프 반복마다 한 번 호출되며 입력이 0일 때만 환경을 조회한다. 새로운 할당이나 락은 도입되지 않았다.

### C. 참고 자료

- 이슈 #326: 이 보고서가 근거로 삼은 재현 절차, 근거, 인수 기준. diff와 교차 확인함.
- 터미널 크기에 대한 ncurses의 폴백 순서(`$COLUMNS`/`$LINES` 다음 관용적 기본값). `Viewport::resolve`가 따르는 모델.
- PR #317 보고서의 TUI 검증 절: 이 패닉이 `script` 아래에서 렌더러 구동을 어렵게 만들었다는 직접적인 사전 증거.
