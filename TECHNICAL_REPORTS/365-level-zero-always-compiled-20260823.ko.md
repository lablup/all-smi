# 기술 보고서: PR #365 - Intel Level Zero 백엔드를 모든 Linux/Windows 빌드에 컴파일

**일자**: 2026-08-23  
**상태**: 패키징 결정과 결함 4건 기준 완료, Sysman 메트릭 호출은 하드웨어 미실행 (7절 참조)  
**관련 항목**: PR #365, 참조 이슈 #364 및 #372, PR #376 위에 구축  
**위험 수준**: 높음 (모든 Linux/Windows 아티팩트의 내용이 바뀌고 보고 결함 4건을 교정)

---

## 요약

PR #365는 `build.rs`가 `all_smi_level_zero` cfg를 방출하게 하고, 모든 소비자가 `feature = "level_zero"` 대신 그것에 게이트하도록 바꿨습니다. 이 cfg는 **모든 Linux/Windows 타겟**에서 켜지고 macOS를 포함한 나머지에서는 꺼지며, `level_zero` cargo 피처는 `--features level_zero`와 그것을 나열한 다운스트림 매니페스트가 계속 빌드되도록 허용되는 no-op으로만 남습니다.

브랜치가 열려 있는 동안 #376이 #364의 분류/용량 절반을 처리했기 때문에, 브랜치를 현재 `main`(`64a8651`) 위로 재작성했습니다. 겹치는 것은 전부 사라졌고, 남은 것은 #376이 닿지 못한 부분입니다. Intel 백엔드가 Windows에 존재하게 만드는 패키징 결정과, 그 결정이 드러낸 결함 네 건입니다. 원래 커밋은 `744dab9`에 보존돼 있습니다.

---

## 1. 결정

| 타겟 | `--features level_zero` | 백엔드 컴파일 여부 |
|------|-------------------------|--------------------|
| Linux | 무관 | **예** |
| Windows | 무관 | **예** |
| macOS | 무관 | 아니오 |

옵트인이 아니라 무조건으로 둔 이유 네 가지입니다.

- **의존성을 추가하지 않습니다.** 로더는 두 타겟 모두에서 이미 무조건 의존성인 `libloading`을 통해 `dlopen`되므로, 백엔드를 컴파일해 넣어도 `NEEDED` 항목도 import 테이블 항목도 늘지 않습니다. `tpu_pjrt`가 이미 musl 가드 없이 Linux에서 dlopen하므로 musl 아티팩트도 새로운 종류의 동작을 얻지 않습니다.
- **하드웨어가 없으면 비용이 0입니다.** `reader_factory`는 Intel GPU가 실제로 존재할 때만 Intel 리더를 만들며, Linux에서는 `/sys/class/drm`에 나타나는지가 기준입니다. 게이트는 CPU 벤더가 아니라 GPU 존재 여부이므로 AMD 호스트는 로더를 결코 열지 않습니다. GPU는 있는데 런타임이 없으면 실패한 로드가 기존 `OnceCell` 뒤에 프로세스 전역으로 캐시되고 sysfs 또는 WMI 기준선이 유지됩니다.
- **타겟당 아티팩트 하나를 배포합니다.** 옵트인 백엔드는 같은 플랫폼에 대해 Intel용과 비 Intel용 패키지를 따로 내야 한다는 뜻입니다. 그러지 않으면 Intel Arc 소유자는 벤더 백엔드가 주는 것을 얻으려고 소스에서 빌드해야 합니다.
- **Windows에서는 다른 무엇도 그 필드를 공급할 수 없습니다.** GPU 온도, 전력, 주파수는 WMI, DXGI, PDH 어느 쪽에도 소스가 없습니다.

얼버무리지 않고 명시할 가치가 있는 비대칭 하나: **Linux에는 그 필드들에 대한 sysfs 기준선이 있으므로**, 거기서 백엔드는 데이터와 빈 열의 차이가 아니라 업그레이드입니다(sysfs가 닿을 수 없는 XMX `COMPUTE_SINGLE` 엔진 클래스, 에너지 카운터 기반 전력, 전용 메모리 상태). 앞의 세 근거는 양쪽에 동일하게 적용됩니다.

cargo는 타겟별 피처 기본값을 표현할 수 없으므로 매니페스트 항목이 아니라 cfg를 씁니다. 실질적 결과는 지원 번들의 `features:` 줄이 더 이상 이 백엔드에 대해 아무것도 말하지 않는다는 것이며, 그래서 `all-smi doctor`가 cfg에서 도출한 별도의 `level_zero: compiled-in | absent` 줄을 `version.txt`에 씁니다.

이는 #372의 Part B 질문도 정리합니다. 타겟이 결정하므로 릴리스 워크플로에서 켤 것이 없습니다.

## 2. 변경 요약

| 항목 | 값 |
|------|----|
| 변경 파일 | 20개 |
| 추가 줄 | 1204줄 |
| 삭제 줄 | 188줄 |
| 테스트 추가 | 27개 |
| 맨 `cargo test`가 도달하는 `intel_gpu_level_zero::` 테스트 | 0개 → 56개 |

### 핵심 파일

| 파일 | 목적 |
|------|------|
| `build.rs` | Linux/Windows 타겟에 `all_smi_level_zero` cfg 방출. |
| `Cargo.toml` | `level_zero`를 허용되는 no-op으로 유지, 문서화. |
| `src/device/readers/detail_keys.rs` | 신규, 무조건 컴파일되는 공유 헬퍼 (3.2 참조). |
| `src/device/readers/intel_gpu_level_zero/apply.rs` | Sysman의 DXGI 용량 덮어쓰기 수정과 `Note` 키 개편. |
| `src/device/readers/windows_gpu_perf.rs`, `pdh.rs` | 공유 사용량 풀과 DXGI→PDH 전달. |
| `src/device/readers/intel_gpu_names.rs` | Panther Lake PCI 디바이스 ID 범위. |
| `src/doctor/bundle.rs` | `version.txt`의 `level_zero:` 줄. |
| `.github/workflows/ci.yml` | 실제 로더 설치, 로더 단언 무장, 중복이 된 피처 단계 제거. |

## 3. 이 결정이 드러낸 결함 네 건

### 3.1 Sysman이 DXGI가 방금 해석한 용량을 덮어썼다

#376은 DXGI 계층이 내장 부품의 128 MiB stolen carve-out 대신 공유 aperture를 보고하도록 가르쳤습니다. 그런데 `intel_gpu_level_zero::apply`가 Sysman 전용 풀에서 `total_memory`를 **무조건** 대입해 carve-out을 곧바로 되돌려 놓았습니다. B390에서는 17.88 GiB가 128 MiB로 보고되는 것이며, 이는 #364의 원래 증상이 두 번째 문으로 들어온 것입니다.

이제 Sysman 수치는 `VRAM Dedicated (L0)`로 보관하고 aperture가 유지됩니다. 외장 카드는 여전히 Sysman 총량을 취하며 Linux는 영향받지 않습니다.

### 3.2 `Metrics Source`가 append가 아니라 assign이었다

각 계층은 자기 자신만 알기 때문에, 마지막에 실행된 계층이 나머지의 기록을 지웠습니다. 전체 스택을 갖춘 Windows 호스트가 `WMI + Level Zero Sysman`을 보고하며 DXGI와 PDH를 잃었고, Linux에서는 Sysman이 판독값을 내는 순간 `(engine counters)` 한정어가 사라졌습니다.

헬퍼를 새로 만든, 항상 컴파일되는 `src/device/readers/detail_keys.rs`로 옮겼습니다. **이 키들을 쓰는 계층들이 서로소인 `cfg` 게이트 뒤에 있어서 그중 어느 하나 안에 든 헬퍼를 호출할 수 없었기 때문**입니다. 그것이 이 버그가 존재한 이유이자, 이 모듈이 Windows 게이트가 아니라 무조건인 이유입니다.

### 3.3 Windows의 모든 Intel/AMD iGPU가 사용 중 0바이트를 보고했다

PDH가 `Dedicated Usage`만 샘플링했는데, 내장 부품에서는 carve-out에서 할당되는 것이 없어 그 인스턴스가 일률적으로 0입니다. 이제 용량이 공유 aperture인 어댑터에 대해 `Shared Usage` 패밀리를 어댑터별, 프로세스별로 읽습니다.

이 카운터는 **DXGI가 그런 어댑터를 보고한 뒤에만** 쿼리에 추가됩니다. `snapshot()`이 DXGI를 먼저 열거하고 그 답을 `pdh::sample`에 넘기므로, 외장 카드만 있는 머신은 카운터를 추가하지 않고 어떤 폴링에서도 비용을 치르지 않습니다.

**교차 풀 폴백은 의도적으로 없습니다.** `Source: Memory Used`가 같은 플래그에서 레이블을 붙으므로, 대체된 수치는 그것을 설명하지 않는 레이블 아래 놓이게 됩니다.

### 3.4 `Note` 키가 포괄 문자열이었다

모든 폴링이 "Detailed metrics require Level Zero / xpu-smi"를 게시했고, 이제 이는 양방향으로 틀립니다. 백엔드는 가서 설치할 대상이 될 수 없고, 이 문구는 사용 불가라고 주장하는 바로 그 필드 옆에서 발화했습니다.

이제 어떤 계층도 공급할 수 없었던 필드를 이름으로 지목하고, 그런 필드가 없으면 아무 말도 하지 않습니다. 내장 부품은 정당하게 Sysman 열 센서를 노출하지 않으므로 "누락 없음"과 "온도 누락"이 둘 다 정상이며, 이 머신이 어느 쪽인지 말해 주는 것이 유용한 부분입니다.

### 함께: Panther Lake 디바이스 ID 범위

마케팅 이름 표에 `0xB080-0xB08F`를 추가했습니다. Linux 리더에는 읽을 마케팅 문자열이 없고 sysfs에서 이름을 해석하므로, 표 항목이 없으면 보고 호스트의 `8086:b080`이 `Intel Graphics (device 0xb080)`가 되고 이어서 `Unknown`으로 분류됩니다. 모든 아키텍처 규칙이 이름을 키로 삼기 때문입니다.

## 4. CI에서 실제 로더 적재

Level Zero 로더는 Intel GPU 드라이버와 별개인 자체 Ubuntu 패키지로 배포되므로, GPU가 없는 러너도 로더를 적재할 수 있습니다. Linux 잡이 이제 이를 설치하고 `ALL_SMI_EXPECT_LEVEL_ZERO_LOADER=1`을 설정해, 새 테스트를 skip에서 단언으로 바꿉니다. `LIBZE_PATHS`의 모든 경로가 해석돼야 하고, `LzApi`의 모든 필수 심볼이 실제 라이브러리가 익스포트하는 철자와 같아야 합니다. 둘 다 컴파일러에게 보이지 않으며, 어느 하나라도 틀리면 실하드웨어에서 백엔드 전체가 조용한 no-op이 됩니다.

여기서 `zeInit`까지는 의도적으로 가지 않습니다. Intel GPU가 없는 러너에는 로더가 돌려줄 드라이버가 없으므로 그곳에서의 초기화 실패는 결함이 아니라 올바른 동작이며, 그것에 단언을 걸면 테스트가 잘못된 이유로 실패하게 됩니다.

이것이 **사 주지 않는 것**도 적어 둡니다. GitHub 호스팅 러너가 Intel CPU를 가졌다는 사실은 여기서 무관합니다. GPU가 없으므로 `has_intel_client_gpu()`가 false이고, 리더가 생성되지 않으며, Sysman 호출이 한 번도 일어나지 않습니다. 하드웨어 없이 메트릭 경로를 실행하려면 스텁 `libze_loader.so.1`이 필요하고, 이것이 #379가 되어 PR #382로 처리됐습니다.

### 중복이 된 CI 단계

플래그 없는 `cargo test`와 `cargo clippy`가 이제 Linux에서 백엔드에 닿으므로, #373이 추가한 `--features level_zero` 단계 두 개는 같은 코드를 두 번 테스트하고 있었습니다. 그 쌍의 `--all-targets` 절반이 실질적 기여였고 기본 clippy 단계로 옮겨져, 이제 lib과 bin만이 아니라 크레이트의 모든 테스트 타겟이 린트됩니다. 피처 전용으로 남는 것은 no-op 계약 자체입니다. 플래그가 여전히 허용됨을 단언하는 디버그 빌드 하나와, 그것이 `Cargo.lock`을 움직이지 않음을 단언하는 기존 릴리스 빌드입니다.

## 5. 검증 결과

이 브랜치의 CI 실행 32621525738은 정지된 자체 호스팅 러너에서 `skipping`인 Windows 잡을 제외하고 전부 green이었습니다.

**Linux 상시 활성 변경이 주장한 대로 동작합니다.** 플래그 없는 맨 `cargo test`가 **서로 다른 `intel_gpu_level_zero::` 테스트 56개**를 실행했습니다. #373 이전에는 모든 구성에서 그 수가 0이었고, #373 이후에는 명시적 `--features level_zero`가 있어야 도달했습니다. 총계는 lib 1672, bin 1848입니다.

**로더가 실제로 적재됩니다.** `libze1`이 `libze_loader.so.1`을 `/lib/x86_64-linux-gnu`에 설치하고, 전용 단계가 테스트가 단언 후에만 출력하는 마커를 찍었습니다.

```
running 1 test
all-smi: level-zero-loader-assertion-ran
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 1672 filtered out
```

즉 `LIBZE_PATHS`의 모든 경로가 실제 로더에 대해 해석되고, `LzApi`의 모든 필수 심볼이 라이브러리가 익스포트하는 철자와 같습니다. 그 로직의 나머지 세 분기는 스크래치 프로브로 로컬에서 실행했습니다. 환경 키가 없으면 테스트가 skip하고 통과하며, 로더가 없는 호스트에서 키를 설정하면 의도한 메시지로 실패하고, 그 실패 사례에서는 마커가 없으므로 grep 가드가 헛되이 통과할 수 없습니다.

**macOS 로컬**: 바이너리 23개에 걸쳐 3446 테스트, 실패 0. `cargo fmt --check`와 `cargo clippy --all-targets -- -D warnings` 깨끗. 백엔드를 컴파일해 넣고 Windows 리더 게이트를 넓힌 스크래치 프로브에서 lib 테스트 1630개가 돌았고, `annotate_missing_metrics`에 일부러 타입 오류를 주입해 그것이 드러나는 것으로 도달성을 증명했습니다.

신규 테스트 27개는 모두 Linux 러너가 도달 가능합니다. `detail_keys` 계약, DXGI↔Sysman 메모리 전달 양방향, 사용량 풀 선택과 프로세스별 분리, 디바이스 ID 범위와 그 경계, `version.txt`의 `level_zero:` 줄, 로더 심볼 해석입니다.

## 6. 검증되지 않은 것

B390도, Windows 호스트도 없고 자체 호스팅 Windows 러너가 정지 상태였으므로, Sysman 메트릭 호출과 PDH 카운터 경로는 실하드웨어에서 실행되지 않았습니다. 위 로더 단계가 그 공백 중 도달 가능한 가장 가까운 부분을 닫았고, 남은 것은 장치 또는 스텁 로더가 필요합니다. 여기 나머지는 전부 그 호출들 **주위에서** 내린 결정이며, 결함 네 건이 살던 곳이 바로 거기입니다.

로더 테스트의 패키지 이름은 Level Zero 로더가 없는 macOS 호스트에서 확인할 수 없었습니다. 이 브랜치의 CI 실행이 그것을 확인해 주며, 이름이 틀렸다면 그 단계는 조용히 skip하지 않고 지목된 메시지와 함께 크게 실패합니다.

## 7. 결과 및 후속

- PR #365는 `dd64e21`로 `main`에 squash merge되었습니다.
- PR을 넓히는 대신 후속 두 건을 등록했습니다.
  - 하드웨어 없이 Sysman 경로에 닿기 위한 CI용 스텁 `libze_loader.so.1`. 이것이 **#379**가 되어 PR #382로 처리됐습니다.
  - 런타임에 대한 진단 검사. `level_zero: compiled-in`은 백엔드가 바이너리 안에 있다는 것만 알려 주고 로더가 실제로 올라왔는지는 아무것도 말해 주지 않기 때문입니다. 이것이 **#380**이 되어 PR #381로 처리됐습니다.
- **이슈 #377은 열려 있습니다**: Arc B390 사용률 증상과 #364에서 남은 `apply.rs` 지점.
- **이슈 #378은 열려 있습니다**: Windows Intel/AMD 리더가 어떤 계층도 소싱하지 않은 사용률과 전력에 대해 `GPU_METRIC_UNAVAILABLE` 센티널 대신 여전히 0을 게시합니다.
- 이 변경은 v0.26.0에 나갔습니다. 소비자에게 실질적인 변화는 지원 번들의 `features:`가 더 이상 Level Zero 질문에 답하지 않는다는 점입니다. 대신 `doctor`의 `level_zero:` 줄을 읽으면 됩니다.

---

## 부록: 핵심 키워드

| 키워드 | 설명 | 관련성 |
|-------|------|--------|
| 빌드 cfg | 매니페스트에 선언하지 않고 `build.rs`가 방출하는 `cfg` | cargo가 표현할 수 없는 타겟별 기본값을 표현하는 방법 |
| 허용되는 no-op 피처 | 아무것도 게이트하지 않지만 기존 매니페스트가 계속 빌드되도록 남긴 피처 | 전환 후에도 `--features level_zero`가 동작하는 이유 |
| `OnceCell` 로드 캐시 | 로더가 올라왔는지에 대한 프로세스 전역 기록 | 런타임 부재 비용이 폴링당 한 번이 아니라 전체 한 번인 이유 |
| 공유 aperture 대 전용 풀 | DXGI가 구분하는 두 메모리 레이아웃 | Sysman이 덮어쓰고 있던 구분 |
