# Technical Report: PR #92 - fix: Add missing build dependencies for Ubuntu PPA workflow

**Date**: 2024-12-23
**Author**: AI Code Reviewer
**Status**: Completed
**Languages**: YAML (GitHub Actions), Debian Control
**Risk Level**: Low

---

## Executive Summary

This PR fixes the Ubuntu PPA upload workflow build failures by adding 6 missing build dependencies to the GitHub Actions workflow and 2 packages to the Debian control file. The changes align the CI environment with the existing Dockerfile configuration.

---

## 1. Problem Statement

### 1.1 Background
The all-smi project uses a GitHub Actions workflow (`launchpad_ppa.yml`) to automatically build and upload Debian source packages to Ubuntu's Launchpad PPA. This enables easy installation of the tool via `apt` on Ubuntu systems.

### 1.2 Existing Issues
The workflow was failing at the `dpkg-buildpackage` step because required build dependencies were not installed in the GitHub Actions runner.

- **Issue 1**: Missing system packages (`pkg-config`, `libssl-dev`, `protobuf-compiler`, `cmake`) for Rust crate compilation
- **Issue 2**: Missing AMD GPU libraries (`libdrm-dev`, `libdrm-amdgpu1`) required by the `libamdgpu_top` crate

### 1.3 Risk Assessment

| Risk | Impact | Likelihood |
|------|--------|------------|
| PPA builds continue to fail | High | High (confirmed issue) |
| Users cannot install via apt | Medium | High |
| Project adoption impacted | Medium | Medium |

---

## 2. Technical Review

### 2.1 Security Assessment

**Review Items:**
- [x] No secrets or credentials exposed
- [x] No new attack vectors introduced
- [x] Packages from official Ubuntu repositories only
- [x] Workflow permissions remain minimal (`contents: read`)

**Findings:** No security issues identified.

### 2.2 Compatibility Assessment

**Review Items:**
- [x] Packages available on target distributions (jammy, noble)
- [x] No version conflicts
- [x] Consistent with Dockerfile configuration

**Dependencies Added:**

| Package | Purpose | Available on Jammy | Available on Noble |
|---------|---------|-------------------|-------------------|
| `pkg-config` | Library discovery | Yes | Yes |
| `libssl-dev` | OpenSSL headers | Yes | Yes |
| `protobuf-compiler` | Protocol Buffers | Yes | Yes |
| `cmake` | Build system | Yes | Yes |
| `libdrm-dev` | DRM headers | Yes | Yes |
| `libdrm-amdgpu1` | AMD GPU DRM library | Yes | Yes |

### 2.3 Code Quality Assessment

- **Consistency**: Changes align with existing `Dockerfile` configuration
- **Documentation**: PR description clearly explains each package's purpose
- **Maintainability**: Centralized dependency list in both workflow and control file

---

## 3. Technical Decisions

### 3.1 Adding libdrm-amdgpu1 as Build Dependency

**Context:**
The `libdrm-amdgpu1` package is typically a runtime dependency, not a build dependency. However, the `libamdgpu_top` Rust crate requires it during compilation.

**Options Considered:**

| Option | Pros | Cons |
|--------|------|------|
| Only add `libdrm-dev` | Standard practice | Build may fail |
| Add both `libdrm-dev` and `libdrm-amdgpu1` | Mirrors working Dockerfile | Slightly redundant |

**Decision:** Add both packages to match the working Dockerfile configuration, ensuring consistent behavior.

**Rationale:** The Dockerfile (line 9-10) uses the same combination successfully. The `libamdgpu_top` crate (v0.11.0) appears to have specific linking requirements.

---

## 4. Implementation Details

### 4.1 Changes to `.github/workflows/launchpad_ppa.yml`

**Location:** Step 5 "Install build dependencies" (lines 109-123)

```yaml
# Before
sudo apt install -y \
  devscripts \
  debhelper \
  ...
  build-essential

# After
sudo apt install -y \
  devscripts \
  debhelper \
  ...
  build-essential \
  pkg-config \
  libssl-dev \
  protobuf-compiler \
  cmake \
  libdrm-dev \
  libdrm-amdgpu1
```

### 4.2 Changes to `debian/control`

**Location:** Build-Depends section (lines 5-14)

```
# Before
Build-Depends: debhelper-compat (= 13),
               ...
               cmake

# After
Build-Depends: debhelper-compat (= 13),
               ...
               cmake,
               libdrm-dev,
               libdrm-amdgpu1
```

---

## 5. Learning Points

### 5.1 Debian Package Build Dependencies

**Concept:**
Debian packages specify their build requirements in the `debian/control` file under `Build-Depends`. These dependencies are automatically installed when building with tools like `pbuilder` or `sbuild`, but must be manually installed in CI environments.

**Application in this PR:**
The GitHub Actions workflow runs on a generic Ubuntu runner which doesn't have the Debian build tools' automatic dependency resolution. Therefore, both the workflow AND the control file need to specify the same dependencies.

**Best Practice:**
- Keep `debian/control` Build-Depends synchronized with CI workflow dependencies
- Use the Dockerfile as a reference for required system packages

### 5.2 Rust Native Dependencies

**Concept:**
Rust crates that wrap native libraries (like `libamdgpu_top` for AMD GPU support) require the corresponding system development packages to be installed.

**Application in this PR:**
The `libamdgpu_top` crate (specified in `Cargo.toml` line 60) requires:
- `libdrm-dev` for DRM headers
- `libdrm-amdgpu1` for AMD-specific DRM functionality

**Related crates with native dependencies:**
- `openssl` -> `libssl-dev`
- `prost/tonic` -> `protobuf-compiler`

---

## 6. Further Learning Resources

### Key Concepts
| Keyword | Description | Relevance |
|---------|-------------|-----------|
| `dpkg-buildpackage` | Debian source package builder | Core tool that failed without dependencies |
| `Build-Depends` | Debian control field | Specifies compilation requirements |
| `libamdgpu_top` | AMD GPU monitoring crate | Source of libdrm dependency |

### Related Documentation
- [Debian Policy Manual - Control Files](https://www.debian.org/doc/debian-policy/ch-controlfields.html)
- [Ubuntu PPA Packaging Guide](https://help.launchpad.net/Packaging/PPA)
- [Rust-sys crate patterns](https://kornel.ski/rust-sys-crate)

---

## 7. Change Summary

### Statistics
| Item | Value |
|------|-------|
| Changed files | 2 |
| Added lines | +10 |
| Deleted lines | -2 |
| Tests added | 0 |

### Changes by Category

| Category | Count | Summary |
|----------|-------|---------|
| CI/CD | 1 | Add 6 packages to workflow |
| Packaging | 1 | Add 2 packages to Build-Depends |

### Related Issues
- Issue #91: Original bug report for build failures

---

## 8. Follow-up Actions

### Verification Required
- [ ] Trigger manual workflow run to verify build succeeds
- [ ] Confirm package uploads to Launchpad successfully
- [ ] Verify PPA builds complete on Launchpad builders

### Monitoring
- Watch for Launchpad build notifications after merge
- Check https://launchpad.net/~lablup/+archive/ubuntu/backend-ai for build status

### Future Improvements
- Consider adding a CI job to test Debian package building on PRs
- Document the relationship between Dockerfile and debian/control dependencies

---

## Appendix

### A. File Locations
- Workflow: `.github/workflows/launchpad_ppa.yml`
- Control file: `debian/control`
- Reference Dockerfile: `Dockerfile`
- Cargo configuration: `Cargo.toml`

### B. Related Files with Same Dependencies
```
Dockerfile:6-10     - Same packages for Docker builds
Cargo.toml:60       - libamdgpu_top crate requiring libdrm
```
