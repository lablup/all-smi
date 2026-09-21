# 기술 보고서: PR #432 - proc_pidinfo로 읽는 macOS 프로세스 지표

**날짜**: 2026-09-21
**상태**: 후속 조치 필요
**언어**: Rust, Markdown
**위험도**: 중간

---

## 요약

PR #432는 macOS 로컬 수집기가 다섯 틱 중 네 틱에서 sysinfo의 프로세스 갱신을 부르지 않게 하고, 프로세스별 CPU 사용률, 메모리, 상태, 실행 시간을 `proc_pidinfo`로 직접 읽는다. 다섯 번째 전체 틱에서는 여전히 sysinfo가 프로세스를 발견하고 정적 메타데이터를 채우며, 그 뒤 새 샘플러가 모든 PID를 읽으므로 모든 행의 CPU 사용률은 그 프로세스 자신의 태스크 시간 증분을 자신의 경과 시간으로 나눈 값이 된다. M1 Ultra에서 선택 프로세스 갱신은 16.28 ms에서 4.24 ms로, 정상 상태 틱 전체는 28.87 ms에서 20.90 ms로, `all-smi local --interval 1`은 코어 하나의 3.70 %에서 2.53 %로 줄었다. 전체 틱은 두 리더를 모두 지불하게 되어 31.67 ms에서 35.18 ms로 늘었다. 이 PR은 #427과 #414를 닫는다. PR #426이 측정만 하고 의도적으로 구현하지 않은 #414의 마지막 항목을 구현한 것이다. 작업 도중 발견한 sysinfo의 두 번째 결함도 함께 고쳤다. 다섯 틱마다 한 번만 갱신되는 프로세스는 그 틱에서 실제 CPU 사용률의 약 다섯 배로 읽혔고, 부풀려진 그 값이 표에 보일 500개 프로세스를 정했다.

---

## 1. 문제 정의

### 1.1 배경

PR #426은 IOReport와 SMC 읽기를 매 틱 경로에서 빼냈고, 그 결과 `local`에서 가장 큰 틱당 비용으로 프로세스 갱신이 남았다. #426의 독립 벤치(프로세스 967개 중 500개 추적, 20회)는 선택 갱신을 27.2 ms로 측정했고, 그중 20.8 ms가 `KERN_PROCARGS2` sysctl 한 쌍이었으며, 수집기가 실제로 보관하는 값을 담은 `proc_pidinfo` 세 호출은 1.9 ms였다. 그래도 PR #426은 `proc_pidinfo` 우회를 기각했다. sysinfo의 `cpu_usage`(sysinfo 내부 구간과 프로세스별 기준선 위에서 계산한 태스크 시간)도, sysinfo `status`의 출처도 재현할 수 없어서 선택 틱마다 표시 값이 바뀐다는 이유였다. 남은 길로는 상위 sysinfo 가드와 버전 올림을 꼽았고, #414는 그 항목 하나 때문에 열려 있었다. 이슈 #427은 sysinfo 0.39.6 소스를 근거로 이 질문을 다시 열었다.

수집기는 추적 대상 상위 `MAX_DISPLAY_PROCESSES`(500)개 PID를 매 틱, 전체 PID를 `FULL_REFRESH_INTERVAL`(5) 틱마다 갱신한다. 이 PR은 그 일정을 그대로 두고, 틱 종류별로 누가 읽는지를 바꾼다.

### 1.2 기존 문제

- **어떤 갱신 종류로도 끌 수 없는 인자 복사**: sysinfo 0.39.6에서 `update_process`(`src/unix/apple/macos/process.rs:750`)는 갱신 대상 프로세스마다 `get_process_infos`를 부르고, `get_process_infos`(`:539-648`)는 `exe`, `cmd`, `environ`이 갱신이 필요한지 확인하기(`:630-641`) 전에 인자와 환경 영역 전체를 복사하는 `KERN_PROCARGS2` sysctl 두 개를 발행한다. 이를 끄는 `ProcessRefreshKind`는 없고, 수집기는 복사된 바이트를 전부 버렸다. #426의 벤치 기준으로 선택 틱당 약 20.8 ms다. Linux와 Windows는 갱신 종류가 요구할 때만 명령행을 읽는다.
- **모든 프로세스에 하나의 전역 구간**: `get_time_interval`(`src/unix/apple/macos/system.rs:113-152`)은 직전 갱신 호출 이후 흐른 CPU 틱으로 `time_interval`을 구하고, `compute_cpu_usage`(`process.rs:285-306`)는 프로세스마다 태스크 시간 증분을 이 값으로 나눈다. 추적 대상 밖의 프로세스는 전체 틱에서만 갱신되므로, 그 틱에서 다섯 틱치 증분이 한 틱치 구간으로 나뉜다. 무시(ignored) 테스트 `sysinfo_inflates_untracked_processes_on_a_full_refresh`가 `yes` 자식 프로세스로 이를 재현했다. 100.21 %, 전체 틱에서 499.53 %, 그다음 100.07 %였다. 이 값은 `local_collector.rs`에서 표시할 상위 500개를 고르는 정렬 키이므로, 무관한 프로세스가 다섯 틱마다 화면에 튀어 올라올 수 있었고, 부풀려진 순위가 다음 틱의 추적 대상까지 정했다. #426이 제안한 `KERN_PROCARGS2` 복사에 대한 상위 가드로는 이 결함을 고칠 수 없었다.
- **0으로 돌아오지 않는 CPU 사용률**: `compute_cpu_usage`는 태스크 시간 증분이 양수일 때만 `cpu_usage`를 대입한다(`process.rs:297-302`, 상위 master에서도 그대로). 그래서 한 번 CPU를 쓰고 유휴 상태가 된 프로세스는 마지막 0이 아닌 값을 계속 유지했다. 1초 동안 1.44 %를 쓴 자식 프로세스는 이어진 유휴 5초 내내 1.44 %로 읽혔고, `photolibraryd`는 카운터가 움직이지 않은 1초 동안 33.78 %로 읽혔다.
- **재사용된 PID가 이전 프로세스의 신원을 유지**: `update_process_cache`는 행을 PID로만 맞추고 동적 필드만 갱신했으므로, 새 프로세스가 가져간 PID는 이전 프로세스의 이름, 사용자, 명령행을 그대로 보였다.

### 1.3 위험 평가

| 위험 | 영향 | 발생 가능성 |
|------|------|-------------|
| 네이티브 리더가 의도하지 않은 표시 값을 바꾼다. #426이 이를 이유로 기각했다 | 중간 | 중간 |
| `ESRCH`를 따로 확인하지 않으면 `EPERM` 읽기(일반 사용자 기준 985개 중 349개)를 종료로 오인해 살아 있는 행이 사라진다 | 높음 | 높음 |
| 샘플러가 종료를 보고하지 않으면, sysinfo가 선택 틱에서 더 이상 정리하지 않으므로 죽은 추적 PID가 화면에 남는다 | 중간 | 높음 |
| 새 `unsafe` FFI가 초기화되지 않았거나 일부만 쓰인 버퍼를 읽는다 | 높음 | 낮음 |
| 공유 수집기 코드가 Linux나 Windows 프로세스 경로를 퇴행시킨다 | 중간 | 낮음 |

---

## 2. 기술적 결정

### 2.1 PR #426이 불가능하다고 한 우회가 이번에 들어간 이유

#426의 반대 근거는 우회하면 선택 틱마다 표시 값이 바뀐다는 것이었다. #432는 같은 M1 Ultra(macOS 27.0 26A428, 프로세스 985개, 로드 2.1~3.3)에서 프로브와 무시 테스트 `sampler_matches_sysinfo_across_the_process_table`로 측정해 답했다.

- **`status`를 정확히 재현한다.** sysinfo의 macOS 상태는 프로세스를 처음 봤을 때 읽은 `pbi_status`다. 단 `SRUN`이면 `PROC_PIDTHREADINFO`로 읽은 스레드 id 0의 상태로 대체하고(그 호출이 실패하면 `R`, 검사 가능한 프로세스 636개 중 371개에서 실패했다), 처음 봤을 때 BSD 정보를 읽지 못한 프로세스(`new_empty`)는 `?`다. 샘플러는 같은 필드에 같은 규칙을 적용하며, 검사 가능한 PID 637개 중 637개가 일치했다. `EPERM` 프로세스 349개는 행이 sysinfo 값을 유지하므로 sysinfo의 `?`를 그대로 가진다.
- **메모리, 가상 메모리, 실행 시간은 같은 커널 필드에서 온다.** rss는 637개 중 636개가 일치했고(예외 하나는 몇 밀리초 간격의 두 읽기 사이에 값이 바뀌었다), vms는 637개 중 637개, 실행 시간은 637개 모두 1초 이내로 일치했다.
- **화면에 보이는 프로세스의 CPU 사용률이 일치한다.** sysinfo의 `time_interval`과 샘플러의 mach 경과 시간은 둘 다 벽시계 시간을 잰다. 같은 1초 구간에서 `yes` 100.30 대 99.98, `sharingd` 4.14 대 4.14, `rapportd` 2.03 대 2.02, `ghostty` 1.40 대 1.40, `WindowManager` 0.59 대 0.58이었다. 카운터가 움직인 PID 55~100개에 대한 평균 절대 차이는 0.02~0.09 포인트였고, 0.5 포인트를 넘는 차이는 두 구간 사이에 부하가 바뀐 프로세스, 예를 들어 테스트 바이너리 자신(1.57 대 3.67)의 것이었다.

표시 동작 세 가지는 의도적으로 바뀐다. 셋 다 결함 수정이다.

1. **추적 대상 밖 프로세스가 전체 틱에서 더 이상 부풀려지지 않는다.** 실제 `collect_steady_state` 경로에서 새 빌드는 `yes` 자식을 전체 틱에서 99.94 %, 1초 뒤 99.94 %로 읽는다. 여기서 sysinfo와 맞추는 것은 결함을 유지하는 것과 같다.
2. **카운터가 움직이지 않을 때 CPU 사용률이 남아 있지 않는다.** 그런 행은 이제 0으로 읽힌다. 한 프로브 실행에서 검사 가능한 프로세스 639개 중 28개, 29개, 46개가 연속 세 1초 구간에서 오래된 sysinfo 값을 들고 있었다.
3. **재사용된 PID는 행을 다시 만든다.** 전체 틱은 시작 시간을 비교해 sysinfo 메타데이터로 행을 다시 만들고, 선택 틱은 다음 전체 틱이 다시 발견할 때까지 그 행을 뺀다.

셋 중 어느 것도 값을 지어내지 않는다. PID를 처음 볼 때는 sysinfo와 마찬가지로 CPU 값이 없다.

수치에 대한 두 가지 메모. PR 본문은 오래된 값의 개수를 "636개 중 28~49개"로 적었는데, 커밋 `88163d6`이 이것을 한 프로브 실행의 범위와 다른 실행의 분모를 섞은 값으로 확인했다. 위의 단일 실행 수치는 지금 `sampler_macos.rs` 모듈 문서에 있는 수정된 값이다. 같은 모듈 문서에는 모양이 같은 다른 `yes` 실행(98.87, 501.87, 100.11)도 인용되어 있다. 1.2절의 수열은 PR 본문에 있는, 무시 테스트에서 나온 실행이다.

### 2.2 틱 종류 둘, 경로 둘

**선택 틱**에서는 프로세스에 대해 sysinfo를 전혀 부르지 않는다. 샘플러가 추적 PID를 읽고 캐시를 직접 순회한다. 추적 행은 샘플 값을 받고, 샘플러가 프로세스가 사라졌거나 시작 시간이 바뀌었다고 보고하면 빠지고, 커널이 답하지 않으면 값을 유지한다. 추적 대상이 아닌 행은 전과 같이 직전 전체 틱의 값을 유지한다. sysinfo의 프로세스 맵은 의도적으로 순회하지 않는다. 그 맵에는 직전 전체 틱 이후 죽은 추적 PID가 모두 남아 있어서, 순회하면 그 행들이 되살아나기 때문이다.

**전체 틱**에서는 sysinfo가 전부 갱신한다. 새 프로세스를 발견하고 이름, 사용자, 부모, 시작 시간, 명령행을 채우는 것은 여전히 sysinfo이기 때문이다. 그 뒤 샘플러가 sysinfo가 가진 모든 PID를 읽고, 각 행의 동적 필드는 샘플에서 온다. 이 두 번째 패스가 부풀림을 없애는 값이다. 전체 틱은 샘플러 패스 3.90 ms만큼 늘고, 선택 틱 네 개는 각각 약 12 ms를 아낀다. sysinfo의 발견 단계를 네이티브 `proc_listallpids` 패스로 바꾸면 남은 31 ms의 대부분을 없앨 수도 있지만, #427은 이를 나중에 가능한 단계로 보고 범위 밖에 두었다.

### 2.3 샘플러 자신의 경과 시간으로 계산하는 CPU 사용률

샘플러는 PID마다 `pti_total_user + pti_total_system`과 그것을 읽은 시점의 `mach_absolute_time`을 보관하고, 태스크 시간 증분을 그 PID 자신의 경과 시간으로 나눠 보고한다. 두 카운터 모두 mach absolute time 단위이므로 비율에 타임베이스 변환이 필요 없다. 기준선이 없는 PID는 값이 없고(`None`), 이는 sysinfo가 방금 발견한 프로세스에 값을 주지 않는 것과 같다. 움직이지 않은 카운터는 `Some(0.0)`, 경과 시간이 없으면 `None`(`checked_sub`), 거꾸로 간 카운터는 0(`saturating_sub`)이다. 구간이 PID별이므로 그 PID를 한 틱 전에 읽었는지 다섯 틱 전에 읽었는지는 더 이상 결과에 영향을 주지 않는다.

### 2.4 `ESRCH`는 사라짐, 나머지는 읽을 수 없음

일반 사용자에게 커널은 프로세스 표의 약 3분의 2에 대해서만 `proc_pidinfo`에 답하고, 나머지에는 0바이트와 `EPERM`을 돌려준다. 이슈의 구현 메모도 0바이트가 종료가 아니라고 이미 짚었다. `pidinfo_macos::read`는 호출 전에 `errno`를 지우고 호출 직후 다시 읽으며, `ESRCH`일 때만 `PidInfoError::Gone`을 돌려준다. 그 밖의 실패(`EPERM`, 짧은 쓰기, `c_int` 범위 밖의 PID)는 모두 `Unreadable`이고, 갱신 로직은 이를 "가진 값을 유지"로 다룬다. 선택 틱에서 `Gone` 샘플은 행을 뺀다. 전체 틱에서는 sysinfo가 목록에 올렸지만 샘플러가 사라졌다고 본 PID를 한 틱 늦게가 아니라 즉시 뺀다.

### 2.5 측정하는 모든 곳이 쓰는 하나의 진입점

macOS에서 틱당 프로세스 패스는 `process_list::refresh_processes` 하나뿐이다. 수집기의 정상 상태, 첫 반복, `tests/perf_tick_stages.rs`, `local_collector/tests.rs`가 모두 이를 부르므로, 측정과 테스트는 인라인 복제본이 아니라 실제로 배포되는 경로를 돈다. #427은 `update_process_cache`에 macOS 전용 매개변수를 더하자고 제안했지만, 구현은 대신 `refresh_macos.rs`를 추가해 샘플 맵을 직접 받는 전체 틱용, 선택 틱용 캐시 순회를 따로 두었다. #427의 제안과 마찬가지로 캐시에 대한 사후 패스는 피한다. Linux와 Windows에서는 예전 인라인 코드가 그대로 `cfg(not(target_os = "macos"))` `process_pass`로 옮겨졌고, `update_process_cache`는 그쪽에서만 컴파일된다.

---

## 3. 구현 내용

### 3.1 `pidinfo_macos`: 공유되는 단일 읽기

`read::<T>(pid)`는 `MaybeUninit<T>`를 0으로 채우고 정확한 크기를 `proc_pidinfo`에 넘기며, 커널이 정확히 그 바이트 수를 썼다고 보고할 때만 `assume_init`을 부른다. `T`는 `proc_taskinfo`, `proc_bsdinfo`, `proc_threadinfo`에만 구현된 봉인(sealed) 트레이트 `PidInfo`를 구현해야 하므로, 0으로 채운 뒤 커널을 믿는 패턴이 plain-data 구조체로 제한되고, 각 구조체는 자신의 flavor 상수를 가진다. `PROC_PIDTHREADINFO`는 sysinfo처럼 항상 스레드 id 0으로 부른다. 목적이 sysinfo의 상태 열을 재현하는 것이기 때문이다. `priority_macos::base_priority`도 이제 같은 함수를 부르므로, 자체 `proc_pidinfo` 읽기에 쓰던 `unsafe` 블록 두 개가 이쪽으로 옮겨왔다.

### 3.2 `sampler_macos`: 기준선과 상태 규칙

`ProcessSampler::sample`은 PID 반복자를 받아 PID마다 `Sampled`를 돌려준다. `Live(ProcessSample)`, `Unreadable`, `Gone` 중 하나다. `Readings::read`는 태스크 정보 호출을 먼저 하고, 그것이 실패하면 BSD와 스레드 호출을 건너뛴다. `EPERM` 프로세스가 실패 호출 한 번, 검사 가능한 프로세스가 세 번의 비용을 치르는 이유다. `Readings`를 `fold`와 분리해 두었으므로 단위 테스트가 커널 없이 계산을 검증할 수 있다. 기준선은 시작 시간, 처음 봤을 때의 `pbi_status`(sysinfo의 `process_status`처럼 PID 수명 동안 고정), 태스크 시간, 샘플 시점을 담는다. 같은 PID 아래 `pbi_start_tvsec`가 달라지면 기준선을 버리고, `Gone`이면 지운다. `state_code`는 처음 봤을 때 BSD 정보를 읽지 못한 경우를 `?`로, `SRUN`을 스레드 0의 상태(`R`, `S`, `T`, 그 밖의 스레드 상태는 `?`, 스레드 호출이 실패하면 `R`)로, `SIDL`, `SSLEEP`, `SSTOP`, `SZOMB`를 `I`, `S`, `T`, `Z`로 매핑한다. `convert_process_state`가 만드는 것과 같은 글자다. 실행 시간은 epoch 초에서 `pbi_start_tvsec`를 뺀 값으로, sysinfo의 `run_time()`이 쓰는 시계와 같다.

### 3.3 `refresh_macos`: 샘플을 캐시에 반영

`refresh_processes`는 요청이 있거나 추적 대상이 비어 있으면 전체 틱을, 아니면 선택 틱을 돌리고, `perf_tick_stages`를 위해 `ProcessRefreshTimings`(sysinfo, 샘플러, 캐시)를 돌려준다. `update_cache_full`은 sysinfo 맵을 순회하면서 캐시의 시작 시간이 sysinfo와 다른 행을 다시 만들고, 샘플이 있으면 적용한다. 읽을 수 없는 행은 전과 같이 sysinfo 값(메모리 0, 상태 `?`)을 받는다. `update_cache_selective`는 `retain`으로 캐시를 순회한다. `apply_sample`은 샘플에 CPU 값이 없으면 CPU 사용률을 건드리지 않으므로, 처음 본 PID는 새 행이면 sysinfo 값을, 기존 행이면 직전 값을 유지한다. 어느 쪽 순회든 끝나면 `sampler.retain(|pid| cache.contains_key(&pid))`가 캐시에서 빠진 PID의 기준선을 지워 쌓이지 않게 한다.

### 3.4 수집기와 측정 연결

`LocalCollector`에 macOS 전용 `process_sampler: Arc<std::sync::Mutex<ProcessSampler>>`가 생겼다. `process_pass`는 전역 sysinfo 락, 캐시 쓰기 락, 샘플러 락 순서로 잡고, 캐시 락이 이미 그랬듯 두 락 모두 poison에서 복구한다. `perf_tick_stages`는 macOS 갱신 행을 sysinfo와 샘플러의 합으로 보고하고, 이전 실행과 비교할 수 있도록 기존 행 이름 열두 개를 모두 유지하며, "process sampler (full ticks)"와 "process sampler (selective ticks)" 두 행을 더한다. `docs/ARCHITECTURE.md`에 새 구조를 적었다.

### 3.5 unsafe 범위

새 파일에는 `unsafe` 지점이 여섯 곳 있다. `pidinfo_macos.rs`에 `__error()`를 통한 `errno` 초기화, `proc_pidinfo` 호출, `assume_init`, `errno` 재확인의 네 곳, `sampler_macos.rs`에 `mach_absolute_time`의 `unsafe extern "C"` 선언(`libc` 바인딩이 deprecated라서이며, IOReport 리더도 같은 방식으로 선언한다)과 그 호출 한 곳이다. `proc_pidinfo` 호출과 `assume_init`은 `priority_macos.rs`에서 지운 두 블록을 대체하고, `errno`를 지웠다가 다시 읽는 패턴은 `priority_macos::nice`가 `getpriority`에 이미 쓰던 것이다.

---

## 4. M1 Ultra 측정

Mac13,2, macOS 27.0 26A428, 프로세스 약 990개, 컴파일러 미실행. 기준선 바이너리(`31ab474`, 병합된 #426 트리)와 이 브랜치(`b9c9f32`로 빌드, `88163d6`은 주석만 바꿈)를 번갈아 `perf_tick_stages`를 네 번 돌렸고 `PERF_TICKS=30`, 시각 21:18~21:25, 패스 시작 시점 로드 애버리지는 1.90~3.56이었다. 각 열은 두 패스의 평균이다. `top -l 4 -s 8 -pid`의 2~4번째 샘플을 쓰고, `local`은 160x50 의사 터미널에서 돌렸다.

| 정상 상태 틱당 | 기준선 | 이 브랜치 |
|---|---|---|
| 프로세스 갱신, 선택 | 16.28 ms | 4.24 ms |
| 프로세스 갱신, 전체(5틱마다) | 31.67 ms | 35.18 ms(sysinfo 전체 갱신과 모든 PID에 대한 샘플러) |
| 그중 샘플러 | 해당 없음 | 선택 4.21 ms, 전체 3.90 ms |
| `update_process_cache` 행 | 선택 1.12 ms, 전체 0.86 ms | 선택 0.97 ms, 전체 0.92 ms |
| 병합 + 정렬 + 자르기 | 0.81 ms | 1.07 ms |
| 틱 전체 | 28.87 ms | 20.90 ms |
| 틱 동안 프로세스 CPU | 1 s에서 코어 하나의 2.86 % | 2.04 % |
| `all-smi local --interval 1`, 프로세스 전체 | 코어 하나의 3.70 % | 2.53 % |
| `all-smi api --interval 1`, 프로세스 전체 | 코어 하나의 0.77 % | 0.72 % |

선택 갱신은 74 %, 틱 전체는 28 % 줄었다. 전체 틱은 3.5 ms 늘었다. 샘플러 패스 3.90 ms를 빼면 sysinfo 몫은 31.28 ms로 이전의 31.67 ms와 같으므로, 늘어난 것은 추가 패스뿐이다. 다섯 틱 주기로 평균하면 프로세스 갱신은 19.4 ms에서 10.4 ms로 줄었다((4 x 16.28 + 31.67) / 5 대 (4 x 4.24 + 35.18) / 5). `api`는 대조군이다. Apple Silicon에서는 프로세스를 수집하지 않고, 잡음 범위 안에서 변화가 없다. 병합, 정렬, 자르기는 0.26 ms 늘었는데 PR은 그 원인을 밝히지 않았다. 틱이 아낀 8 ms에 비하면 작다.

선택 틱 샘플러 패스가 전체 틱 패스와 비슷한 비용인 이유는, CPU 기준 추적 상위 500개가 거의 정확히 검사 가능한 프로세스(각각 호출 세 번)이고 `EPERM` 프로세스 349개는 실패 호출 한 번씩만 치르기 때문이다. `sample()` 문서는 처음에 PID당 따로 잰 2.6 us에서 외삽해 500개에 약 1.3 ms라고 적었지만, 틱 안에서 잰 패스는 4.1 ms였고 `b9c9f32`가 문서를 그렇게 고쳤다.

이 수치와 #426의 벤치는 서로 다른 측정 도구다. 벤치의 선택 갱신 27.2 ms와 `KERN_PROCARGS2` 쌍 20.8 ms는 추적 PID 500개에 대한 전용 루프에서 나왔고, `perf_tick_stages` 안에서 같은 선택 갱신은 #426의 두 열에서 13.59 ms와 15.93 ms, 이번에는 16.28 ms로 읽혔다. #432는 벤치가 아니라 자신의 교차 기준선과 비교해야 한다.

같은 호스트에서의 개발 중 단일 실행(라벨 `dev1`, 프로세스 986개, 로드는 기준선 3.68~3.09, 빌드 2.52~2.34)도 같은 방향이었다. 선택 갱신 16.264 ms에서 4.138 ms, 전체 31.790 ms에서 33.738 ms, 틱 전체 28.877 ms에서 19.889 ms였고, 첫 틱에서는 전체 프로세스 갱신이 13.458 ms 대 12.710 ms, 첫 틱 전체가 313.931 ms 대 302.844 ms였다. 기록 수치는 위의 교차 실행이다.

---

## 5. 배운 점

### 5.1 동등성은 측정으로 확인하는 것이고, 의존성의 숫자도 틀릴 수 있다

#426은 sysinfo 값을 재현할 수 없다는 전제로 우회를 기각했다. sysinfo 소스를 읽자 그 전제는 답이 다른 두 질문으로 나뉘었다. `status`는 `proc_pidinfo`가 노출하는 필드에 대한 결정적 규칙이므로 정확히 재현할 수 있었고, 표 전체 A/B가 검사 가능한 프로세스 637개 중 637개에서 이를 확인했다. `cpu_usage`는 다르게 나오는 지점에서 결함이 있으므로 재현하면 안 됐다. 대체 구현이 "값을 바꾼다"고 판단하기 전에 어떤 값이 왜 바뀌는지 측정하고, 의존성과 맞추겠다고 약속하기 전에 그 숫자가 맞는지 확인해야 한다.

### 5.2 증분과 그 분모는 같은 구간을 덮어야 한다

sysinfo의 결함은 일반적인 형태다. 항목별 증분을 전역 구간으로 나누는 계산은 매 호출마다 갱신되는 항목에만 맞다. 일부 항목을 덜 자주 갱신하는 일정(추적 집합, N틱마다 전체 갱신)이 있으면 그 항목의 비율은 조용히 N배가 된다. 분모를 증분 옆에 항목별로 두면 갱신 일정이 결과에 영향을 주지 않는다. Linux 경로에도 같은 종류의 결함이 더 심한 형태로 있다(#428).

### 5.3 읽기 실패는 종료가 아니다

`proc_pidinfo`는 사라진 프로세스에도, 호출자가 검사할 수 없는 프로세스에도 0바이트를 돌려주고, 일반 사용자에게 후자는 표의 3분의 1이다. 둘을 구분하는 것은 `errno`뿐이며, 그것도 호출 전에 지우고 다른 무엇이 돌기 전에 다시 읽을 때만 가능하다. 둘을 같이 다루면 살아 있는 행 수백 개를 버리거나 죽은 행을 남기게 된다.

### 5.4 따로 잰 호출당 비용은 틱 안의 비용을 과소평가한다

PID 하나를 따로 잰 2.6 us로는 500개에 1.3 ms가 예상됐지만, 실제 틱 안에서 패스는 4.1 ms였다. 문서 주석은 외삽값으로 두지 않고 측정값으로 고쳤다.

### 5.5 부하 걸린 러너에서 도는 테스트는 안정적인 것을 비교해야 한다

`macOS Unit Tests` CI 잡은 호스팅 `macos-14` 러너에서 `cargo test --lib device::process_list`를 돌리므로, `b9c9f32`가 두 종류의 테스트를 보강했다. 실행 중인 테스트 바이너리의 스레드 0은 두 읽기 사이에 실행과 대기를 오가므로, 정확한 상태 비교는 이제 스레드 0이 안정적인 잠든 자식 프로세스에만 적용한다. 부풀림 테스트는 전체 틱 값을 1초 기준값에 대한 비율로만 묶었는데, 기준 1초가 CPU를 못 받으면 실패할 수 있었다. 이제는 결함이 약 500 %로 읽는 상황에서 값이 200 % 미만이어야 한다는 조건을 더하고, 비율 허용치는 3배로 두었다.

---

## 6. 변경 요약

### 통계

| 항목 | 값 |
|------|-----|
| 변경 파일 | 11 |
| 추가 줄 | +1764 |
| 삭제 줄 | -160 |
| 추가 테스트 | 22(샘플러 15, 갱신 6(그중 2개는 무시되는 하드웨어 테스트), 수집기 1) |
| 새 모듈 | 3(`process_list/pidinfo_macos.rs`, `process_list/sampler_macos.rs`, `process_list/refresh_macos.rs`) |

### 분류별 변경

| 분류 | 개수 | 요약 |
|------|------|------|
| 성능 | 1 | 선택 틱은 sysinfo 갱신 대신 `proc_pidinfo`로 읽음 |
| 정확성 | 4 | 전체 틱 부풀림 제거, 남아 있는 CPU 사용률 제거, PID 재사용 시 행 재생성, `ESRCH`와 `EPERM` 구분 |
| FFI | 1 | 봉인 트레이트 위의 공유 `pidinfo_macos::read::<T>`, 우선순위 조회도 사용 |
| 측정 | 1 | `perf_tick_stages`가 `refresh_processes`를 돌리고 샘플러 행 두 개 추가 |
| 테스트 | 파일 3개 | 샘플러 계산과 상태 규칙, 갱신 경로, 수집기 전체 틱 부풀림 |
| 문서 | 2 | `docs/ARCHITECTURE.md`, sysinfo 파일과 줄 번호를 담은 모듈 문서 |

### 관련 커밋

| 해시 | 유형 | 메시지 |
|------|------|--------|
| `ffee283` | update | 틱마다 sysinfo 대신 macOS 프로세스 지표를 네이티브로 읽음 |
| `b9c9f32` | test | 부하 걸린 CI 러너에 맞춰 macOS 샘플러 테스트 보강 |
| `88163d6` | docs | macOS 샘플러 주석의 프로브 수치 두 개 수정 |
| `d5e4be3` | squash merge | PR #432를 `main`에 병합 |

---

## 7. 검증과 후속

### 완료한 검증

- M1 Ultra에서 `cargo test --lib device::process_list`: 28개 통과, 2개 무시, 연속 세 번 실행.
- `cargo test --bin all-smi view::data_collection::local_collector`: 7개 통과. 실제 `collect_steady_state` 경로를 도는 `full_tick_does_not_inflate_untracked_processes` 포함.
- 무시 하드웨어 테스트 두 개를 `--ignored --nocapture`로 실행: sysinfo 부풀림 재현과 2.1절의 표 전체 A/B.
- `cargo clippy --lib --tests -- -D warnings`, `cargo fmt --check`, `cargo test --test user_facing_text_test`.
- PR 검사 일곱 개 모두 통과. `macos-14`의 `macOS Unit Tests`(무시되지 않은 `device::process_list` 테스트 실행)와 비 macOS 경로를 컴파일하는 Linux `Test Suite` 포함.
- `api`와 `local`의 `top` 측정을 곁들인 교차 `perf_tick_stages` 패스 네 번. 4장에 있다.

### 리뷰

이 PR에는 `pr-security-checker`를 돌리지 않았다. FFI 건전성 검토는 대신 정확성 리뷰어에게 맡겼고, 버퍼 규율, `errno` 처리, 짧은 쓰기, 오버플로, 수명을 다뤘다. LOW를 넘는 발견은 없었고, 메인테이너는 그 근거로 병합을 선택했다. GitHub의 PR에는 리뷰도 코멘트도 없으므로 이 보고서가 그 결정의 기록이다. 그 리뷰의 FFI 부분이 다룬 것은 3.5절의 `unsafe` 지점 여섯 곳이다.

### 검증하지 못한 것

- Linux와 Windows 런타임 동작. 호스트가 없었다. 비 macOS 경로는 이전 코드를 `process_pass`로 옮긴 것이고 Linux 빌드는 `Test Suite` 잡이 확인하지만, 두 플랫폼 모두에서 실행하거나 측정한 것은 없다.
- 이 변경의 Windows 컴파일. `windows-selfhosted.yml`은 PR이 아니라 `main` 푸시에서 돈다. 이 보고서를 쓰는 시점에 `d5e4be3`에 대한 실행은 아직 대기 중이었고, 그 앞의 완료된 `main` 실행 네 번(`c840c36`, `133b978`, `61710d8`, `31ab474`)은 `cargo test --lib` 단계에서 실패했다. `d5e4be3`에 대한 `main` CI 실행도 아직 진행 중이었다.
- 이 M1 Ultra 외의 Apple 칩, 그리고 같은 `target_os = "macos"` 경로를 타는 Intel Mac.
- #427의 검증 절에 있던 `local` 행과 `top`의 수동 비교는 PR에 기록되어 있지 않다.
- `full_tick_does_not_inflate_untracked_processes`는 바이너리 크레이트에 있고 macOS CI 잡은 이를 돌리지 않으므로(`cargo test --lib` 필터 세 개만 실행), 메인테이너 호스트에서만 돌았다.

### 완료 필요

- 이슈 #428, 여전히 `status:ready`로 OPEN: CPU 사용률 결함의 Linux 대응 항목. Linux의 sysinfo는 매 갱신마다 목록의 모든 프로세스를 하나의 전역 구간으로 다시 계산하므로, 추적 대상 밖 프로세스는 전체 틱뿐 아니라 매 틱 부풀려진다. 재현, 수정, `perf_tick_stages` 실행 모두 Linux 호스트가 필요하다.
- `d5e4be3`의 `Windows Self-Hosted` 결과가 나오면 확인할 것. 이 잡은 이 PR 이전부터 `cargo test --lib`에서 실패하고 있었다는 점을 감안해야 한다.

### 남은 제약

- 전체 틱은 여전히 모든 프로세스에 대한 `KERN_PROCARGS2` 복사를 포함한 sysinfo 전체 갱신을 치르며, 다섯 틱마다 약 31 ms다. `proc_listallpids`를 통한 네이티브 발견으로 대부분을 없앨 수도 있지만 #427에서 범위 밖이었고, 이를 추적하는 이슈는 아직 없다.
- 일반 사용자가 검사할 수 없는 행(프로브에서 985개 중 349개)은 전과 같이 sysinfo 값, 즉 메모리 0과 상태 `?`를 유지한다.
- Linux와 Windows의 `update_process_cache`는 여전히 행을 PID로만 맞추므로, 그쪽에서는 재사용된 PID가 이전 프로세스의 이름, 사용자, 명령행을 유지한다.
- macOS 라이브러리 사용자에게 `all_smi::device::process_list::update_process_cache`는 더 이상 컴파일되지 않으며, 그 자리를 `refresh_processes`와 `ProcessSampler`가 대신한다. 이 저장소 안에서는 수집기와 `perf_tick_stages`가 유일한 호출자였고 둘 다 수정되었다.
- macOS CI 잡은 clippy를 돌리지 않으므로 macOS 전용 모듈의 `-D warnings`는 로컬에서만 확인했다(#429가 추가를 추적한다).

### 참고

- [PR #432](https://github.com/lablup/all-smi/pull/432)
- [이슈 #427](https://github.com/lablup/all-smi/issues/427): 문제 정의, sysinfo 소스 참조, 수용 기준
- [이슈 #414](https://github.com/lablup/all-smi/issues/414)와 [PR #426](https://github.com/lablup/all-smi/pull/426): 측정과 이 항목을 미룬 이유
- [이슈 #428](https://github.com/lablup/all-smi/issues/428): Linux의 같은 종류 CPU 사용률 결함
- 이슈 #429: macOS CI 잡의 clippy
- sysinfo 0.39.6: `src/unix/apple/macos/process.rs`(`update_process`, `get_process_infos`, `compute_cpu_usage`)와 `src/unix/apple/macos/system.rs`(`get_time_interval`)
