# Technical Report: PR #98 - feat: Add Windows x64 build target

**Date**: 2024-12-24
**Author**: AI Code Reviewer
**Status**: Completed
**Languages**: Rust, YAML (GitHub Actions)
**Risk Level**: Low

---

## Executive Summary

This PR adds Windows x86_64 (MSVC) target support to the all-smi build pipeline. The changes include GitHub Actions workflow modifications for Windows-specific build steps and a Rust code addition for Windows process priority handling.

---

## 1. 문제 정의 (Problem Statement)

### 1.1 배경
all-smi는 Linux 및 macOS를 지원하지만 Windows 사용자를 위한 공식 빌드가 없었다. Issue #97에서 Windows 지원 요청이 있었다.

### 1.2 기존 문제점
- **문제 1**: Windows용 공식 바이너리 배포가 없어 Windows 사용자가 NVIDIA GPU 모니터링을 위해 all-smi를 사용할 수 없었다.
- **문제 2**: Windows에서 빌드 시 프로세스 우선순위 관련 코드가 컴파일 오류를 발생시킬 수 있었다.

### 1.3 위험성
| 위험 | 영향도 | 발생 가능성 |
|-----|-------|-----------|
| Windows 사용자 접근 불가 | Medium | High |

---

## 2. 기술적 검토 사항 (Technical Review)

### 2.1 보안 관점

**검토 항목:**
- [x] CI/CD 워크플로우에 인젝션 취약점 없음
- [x] PowerShell 스크립트에 보안 문제 없음
- [x] 비밀 키/토큰 노출 없음

**발견된 이슈:**
| 이슈 | 심각도 | 상태 |
|-----|-------|-----|
| 없음 | - | - |

### 2.2 성능 관점

**검토 항목:**
- [x] 빌드 시간 영향 최소화
- [x] 런타임 성능 영향 없음

**성능 영향:**
변경 사항이 런타임 성능에 영향을 미치지 않음. Windows에서 프로세스 우선순위 조회 시 기본값을 반환하므로 시스템 호출 오버헤드가 없다.

### 2.3 호환성/의존성 관점

- **Breaking Changes**: 없음
- **새로운 의존성**: 없음 (기존 Windows 지원 코드 활용)
- **호환성**: Windows 10/11 x64 with MSVC toolchain

### 2.4 코드 품질 관점

- **코드 복잡도**: 변화 없음 (단순 조건부 컴파일)
- **기술 부채**: 유지

---

## 3. 기술적 선택과 그 이유 (Technical Decisions)

### 3.1 Windows 프로세스 우선순위 처리

**컨텍스트:**
Unix 시스템은 nice 값(-20~19)을 사용하고 Windows는 Priority Class를 사용한다. sysinfo 크레이트는 Windows 우선순위를 직접 노출하지 않는다.

**고려한 대안:**

| 옵션 | 장점 | 단점 |
|-----|-----|-----|
| Windows API 직접 호출 | 정확한 우선순위 표시 | 코드 복잡도 증가 |
| **기본값 반환 (선택)** | 단순함, 기존 패턴 유지 | 우선순위 정보 미표시 |
| sysinfo 업스트림 기능 대기 | 깔끔한 해결책 | 시간 불확실 |

**선택 이유:**
- 프로세스 우선순위는 GPU 모니터링 핵심 기능이 아님
- 기존 코드베이스의 다른 플랫폼별 처리와 일관성 유지
- 향후 Windows API 통합 시 쉽게 확장 가능

### 3.2 protoc 스킵

**컨텍스트:**
TPU 지원은 Linux 전용이므로 Windows에서 protoc 설정이 불필요하다.

**선택 이유:**
- 불필요한 의존성 설치 회피
- 빌드 시간 단축
- 명확한 플랫폼 분리

---

## 4. 구현 상세 (Implementation Details)

### 4.1 GitHub Actions 워크플로우 변경

```yaml
# Windows 빌드 매트릭스 추가
- target: x86_64-pc-windows-msvc
  os: windows-latest
  artifact_name: all-smi.exe
  asset_name: all-smi-windows-x86_64
  archive_ext: ".zip"
  protoc_platform: ""  # TPU는 Linux 전용

# Windows 전용 패키징
- name: Package Windows binary (zip)
  if: runner.os == 'Windows'
  shell: pwsh
  run: |
    New-Item -ItemType Directory -Force -Path package
    Copy-Item $BIN -Destination package/
    Compress-Archive -Path package/* -DestinationPath $ASSET

# Windows 전용 체크섬 생성
- name: Generate checksum (Windows)
  if: runner.os == 'Windows'
  shell: pwsh
  run: |
    $hash = (Get-FileHash -Path $FILE -Algorithm SHA256).Hash.ToLower()
    "$hash  $FILE" | Out-File -FilePath "$FILE.sha256" -Encoding ASCII -NoNewline
```

### 4.2 Rust 코드 변경

```rust
#[cfg(target_os = "windows")]
{
    // Windows는 Unix nice 값 대신 Priority Class 사용
    // sysinfo가 우선순위를 직접 노출하지 않으므로 기본값 반환
    return (20, 0);
}
```

---

## 5. 학습 포인트 (Learning Points)

### 5.1 Rust 조건부 컴파일 (cfg 속성)

**개념:**
Rust의 `#[cfg()]` 속성을 사용하면 컴파일 타임에 플랫폼별 코드를 선택적으로 포함할 수 있다.

**이 PR에서의 적용:**
```rust
#[cfg(target_os = "windows")]
{
    // Windows 전용 코드
}

#[cfg(unix)]
{
    // Unix 계열 전용 코드
}
```

**일반적인 사용 사례:**
- 플랫폼별 시스템 호출
- OS별 라이브러리 바인딩
- 아키텍처별 최적화

### 5.2 GitHub Actions 매트릭스 빌드

**개념:**
`strategy.matrix`를 사용하여 여러 환경에서 동시에 빌드를 실행할 수 있다.

**이 PR에서의 적용:**
Windows 타겟을 기존 Linux/macOS 매트릭스에 추가하여 동일한 워크플로우에서 모든 플랫폼 빌드 수행.

---

## 6. 추가 학습 리소스 (Further Learning)

### 핵심 키워드
| 키워드 | 설명 | 관련성 |
|-------|-----|-------|
| `cfg attribute` | Rust 조건부 컴파일 | 플랫폼별 코드 분기 |
| `MSVC toolchain` | Windows C++ 빌드 도구 | Windows 바이너리 컴파일 |
| `PowerShell` | Windows 스크립팅 | CI/CD 패키징 |

### 관련 기술/프레임워크
- **Rust cfg**: https://doc.rust-lang.org/reference/conditional-compilation.html
- **GitHub Actions**: https://docs.github.com/en/actions/using-workflows

### 관련 PR/이슈
- Issue #97: Windows 지원 요청

---

## 7. 변경 요약 (Change Summary)

### 통계
| 항목 | 값 |
|-----|---|
| 변경된 파일 수 | 2 |
| 추가된 라인 | +41 |
| 삭제된 라인 | -3 |
| 테스트 추가 | 0 |

### 카테고리별 변경

| 카테고리 | 변경 수 | 주요 내용 |
|---------|--------|----------|
| CI/CD | 1 | Windows 빌드 매트릭스 및 패키징 |
| Platform Support | 1 | Windows 프로세스 우선순위 핸들러 |

### 관련 커밋
| Hash | Type | Message |
|------|------|---------|
| `b4c962e` | feat | Add Windows x64 build target support |

---

## 8. 후속 조치 (Follow-up Actions)

### 완료 필요
- [ ] Windows GitHub Actions 빌드 검증
- [ ] 생성된 바이너리 동작 테스트

### 향후 개선 사항
- Windows 코드 사이닝 추가 고려
- Windows 패키지에 README 포함 고려
- Windows 프로세스 우선순위 API 직접 구현 고려

---

## Appendix

### A. 기존 Windows 지원 코드

프로젝트는 이미 다음 Windows 지원 모듈을 포함하고 있다:
- `src/device/cpu_windows.rs`: Windows CPU 모니터링
- `src/device/memory_windows.rs`: Windows 메모리 모니터링
- `src/device/readers/amd_windows.rs`: Windows AMD GPU 지원
- `src/utils/system.rs`: Windows sudo 권한 처리 (불필요하므로 스킵)

### B. 빌드 매트릭스 구성

| 타겟 | OS | 아키텍처 | 패키지 형식 |
|-----|-----|---------|-----------|
| x86_64-unknown-linux-gnu | ubuntu-22.04 | x86_64 | tar.gz |
| x86_64-unknown-linux-musl | ubuntu-latest | x86_64 | tar.gz |
| aarch64-unknown-linux-gnu | ubuntu-22.04-arm | aarch64 | tar.gz |
| aarch64-unknown-linux-musl | ubuntu-24.04-arm | aarch64 | tar.gz |
| aarch64-apple-darwin | macos-14 | aarch64 | zip |
| **x86_64-pc-windows-msvc** | **windows-latest** | **x86_64** | **zip** |
