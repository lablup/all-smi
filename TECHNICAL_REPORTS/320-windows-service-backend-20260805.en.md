# Technical Report: PR #320 - feat(service): run API mode as a Windows service

**Date**: 2026-08-05
**Status**: Type-checked and linted for the target; never executed or linked (see section 8)
**Languages**: Rust, YAML (GitHub Actions), PowerShell
**Risk Level**: High to ship blind. Every runtime acceptance criterion is unverified because the implementation environment (macOS) has no Windows machine, no Service Control Manager, and no MSVC linker.

---

## Table of Contents

1. [Executive Summary](#executive-summary)
2. [Problem Statement](#1-problem-statement)
3. [Technical Review](#2-technical-review)
4. [Technical Decisions](#3-technical-decisions)
5. [Implementation Details](#4-implementation-details)
6. [Learning Points](#5-learning-points)
7. [Further Learning](#6-further-learning)
8. [Change Summary](#7-change-summary)
9. [Follow-up Actions](#8-follow-up-actions)
10. [Appendix](#appendix)

---

## Executive Summary

PR #320 implements the Windows arm of `service_cmd::backend()` that PR #319 left as `NotSupported`, so a Windows monitoring node can export metrics from boot with no user logged in. It replaces exactly one `cfg` arm, per the cross-platform contract PR #319 defined, and adds no new `ServiceError` variant: a service that exists under a different registered binary path reuses `Conflict`, the same variant the Linux backend raises for a foreign unit file, with `--force` lifting it through the existing `uninstall_forced` hook.

The report's central technical problem is not the SCM integration itself but how it was verified at all from a host that cannot run any of it. `cargo check --target x86_64-pc-windows-msvc` fails on the real crate before a single line of this PR's code is even reached: `zstd-sys`'s build script invokes `cc --target=x86_64-pc-windows-msvc` with no Windows headers available, and dies on a missing `string.h`. The workaround is an isolated probe crate that pulls in the real `service_cmd`, `common`, `cli`, `cli_service`, and `utils::command` sources via `#[path]`, stubbing only the two modules (`api`, `device`) that drag in the dependency tree that breaks. The probe is built with both a library and a binary target, because a crate's dead-code analysis runs separately per target and a `pub` item can be live in the library target while genuinely unreferenced behind a binary's private module root; PR #319's own probe hit exactly this gap before this PR extended it. Coverage of the probe was verified, not assumed: a deliberate type error injected into each of `scm_backend.rs`, `scm_host.rs`, and `scm_log.rs` in turn produced a compile error through the probe every time, and the probe caught two real defects before this PR shipped, `raw_code`/`describe` being private but used across module boundaries, and `build_service_info` taking `&PathBuf` where clippy's `ptr_arg` lint demands `&Path`. So: every line of the Windows backend has been type-checked and linted for the real target. None of it has ever run, and the probe cannot link on this host (no MSVC linker), so link-time problems remain entirely possible.

The PR also carries a cross-platform fix that is not Windows-specific at all: `shutdown_signal` (moved from `src/api/server.rs` into a new `src/api/shutdown.rs`) gains an externally triggerable source, because the SCM delivers a Stop control on a dedicated handler thread with no OS signal to observe. Without it, the handler would have had to call `std::process::exit`, stranding the energy WAL flush and breaking counter monotonicity across a restart, the same class of defect PR #321 later found and fixed independently on macOS via a different code path. The new `Latch` primitive (a one-way boolean gate over a `tokio::sync::watch` channel, using `send_replace` rather than `send`) is what makes a Stop arriving before any listener has subscribed still resolve for every future waiter, which is tested on this host and is not itself Windows-specific. Total: 23 files, +3351/-92, across 8 commits, closing #311.

---

## 1. Problem Statement

### 1.1 Background

PR #319 established the cross-platform `all-smi service` framework: the `ServiceBackend` trait, `Scope`, `ServiceError`, and the exit-code contract, with a Linux systemd implementation and explicit `NotSupported` arms for macOS and Windows naming their respective tracking issues. Issue #311 is the Windows follow-up: implement the SCM backend using the `windows-service` crate rather than shelling out to `sc.exe`, register the process as a LocalSystem service so NVML, WMI thermal zones, and vendor GPU tooling remain reachable, and give the service somewhere to log, since stdout is void once a process runs under the SCM.

### 1.2 Existing Issues

- **Issue 1 (no way to build or lint the Windows-only code from this environment)**: `cargo check --target x86_64-pc-windows-msvc` on the real crate fails inside `zstd-sys`'s build script before reaching any of this PR's code, because that script invokes the C compiler with a Windows target and no Windows C headers are available on macOS.
- **Issue 2 (SCM Stop has no signal to hook into)**: `shutdown_signal` only ever selected on `ctrl_c` and, on Unix, `SIGTERM`. The SCM delivers Stop as a callback on a dedicated control-handler thread, which is neither.
- **Issue 3 (stdout is unreachable under the SCM)**: a service process has no console; anything written to stdout or stderr under the SCM is simply lost, so diagnosing a startup failure needs a different sink entirely.
- **Issue 4 (LocalSystem's `%APPDATA%` is not an operator-editable path)**: the existing per-user config candidates resolve, under LocalSystem, into `C:\Windows\System32\config\systemprofile\AppData\Roaming`, a path no operator will ever hand-edit.

### 1.3 Risk Assessment

| Risk | Impact | Likelihood |
|---|---|---|
| The Windows backend compiles and lints clean but fails at runtime in a way neither check could catch | High: every runtime acceptance criterion (install, start, stop semantics, crash recovery, logging, config discovery) is unverified from this environment | Certain until the gated CI job runs on the self-hosted Windows runner (section 8) |
| The probe crate's stubs diverge from the real `api`/`device` modules in a way that hides a real compile error | Medium: the probe's whole value depends on it faithfully representing the real crate's non-stubbed modules | Mitigated by injecting real type errors and confirming the probe catches them (section 2.1) |
| The SCM's Stop-before-listener-subscribed race silently drops the shutdown signal | Medium: a Stop request would be lost, leaving the process running past its expected stop point, or the WAL flush would race an unclean exit | Closed by `Latch`'s `send_replace` plus an up-front check in `wait()` (section 3.2), tested on this host |
| `cargo check --target x86_64-pc-windows-msvc` silently regresses because a future dependency bump reintroduces the `zstd-sys` build failure for reasons unrelated to this PR | Low, but would remove the only verification signal available for this backend outside of Windows CI | Not mitigated in this PR; noted as a standing constraint |

---

## 2. Technical Review

### 2.1 Verifying Windows-only Rust from a machine that cannot build it

**The wall.** `cargo check --target x86_64-pc-windows-msvc` on the real `all-smi` crate does not fail in this PR's own code. It fails inside `zstd-sys`'s build script, which invokes `cc` with `--target=x86_64-pc-windows-msvc` and no Windows system headers on the host, producing `fatal error: 'string.h' file not found`. This is the same class of wall PR #319 hit trying to cross-check the Linux backend from macOS, just against a different missing toolchain component.

**The workaround.** An isolated probe crate `#[path]`-includes the real `service_cmd`, `common`, `cli`, `cli_service`, and `utils::command` source files directly, and stubs only the two modules that drag in the dependency subtree that fails to cross-compile: `crate::api` (which pulls in the web server and its transitive dependencies, `zstd-sys` among them) and `crate::device` (the platform GPU/CPU readers). Everything else compiles as the genuine source.

**Why the probe needs both a library and a binary target.** Rust's dead-code analysis runs per compilation target, and this crate's module tree compiles twice, once as a library, once as a binary. A `pub` item is automatically considered reachable in the library target, since any external consumer could reach it, but the same item can be genuinely dead behind a binary's private module root. PR #319's Linux probe had exactly this blind spot at first (library-target-only) and was extended to a binary target as well after a real CI failure exposed the gap (see PR #319's report, section 2.2); this PR's Windows probe was built with both targets from the outset, for the same reason.

**Coverage was verified, not assumed.** A deliberate type error was injected into each of `scm_backend.rs`, `scm_host.rs`, and `scm_log.rs` in turn, and each produced a compile error through the probe. This is the difference between "the probe should catch mistakes here" and "the probe does catch mistakes here," and it is recorded in a comment in `service_cmd/mod.rs` specifically because no CI job compiles those three files and the next person to touch them otherwise has no way to check their own work.

**The probe found two real defects before this PR shipped**, not hypothetically: `raw_code` and `describe` were private but used across a module boundary, and `build_service_info` took `&PathBuf` where clippy's `ptr_arg` lint demands `&Path`. Both were caught and fixed because the probe existed, not because anyone was staring at the code with Windows conventions specifically in mind.

**What this does not prove.** Every line of the Windows backend has been type-checked and linted for `x86_64-pc-windows-msvc`. None of it has ever been executed, and the probe itself does not link on this host, since there is no MSVC linker available, so link-time problems (missing symbols, ABI mismatches in the `windows-service` FFI boundary) remain entirely possible and are explicitly out of what this verification method can catch.

### 2.2 Compatibility and Dependencies

- **Breaking changes**: none. The Windows arm of `backend()` is the only `cfg` arm this PR replaces; the macOS arm (still pointing at issue #310, unimplemented at this PR's time) and the Linux backend from PR #319 are untouched.
- **New dependencies**, both scoped under `[target.'cfg(windows)'.dependencies]` so no other target pulls them in: `windows-service = "0.8.1"` (current release as of this PR, MIT OR Apache-2.0, `rust-version` 1.71.0 against the project's 1.96 MSRV) and `tracing-appender = "0.2.5"` (MIT, `rust-version` 1.63.0).
- **Compatibility**: `%PROGRAMDATA%\all-smi\config.toml` is appended to Tier 2 of `candidate_config_paths()` after the existing `%APPDATA%` candidate, additively, matching the pattern PR #319 established for `/etc/all-smi/config.toml` on Linux.

### 2.3 Code Quality

Unit coverage added: `SERVICE_STATUS` state and start-type mapping (including pending states and a stale-PID case), launch-argument mapping, Win32 error translation (including the elevation refusal and the `--user` refusal), SCM command-line parsing (quoted, unquoted, unterminated, and the `\\?\` verbatim-prefix form) and the case-insensitive path comparison behind the install idempotency guard, `%PROGRAMDATA%` path composition, and the new `Latch` shutdown source (resolves on a prior trigger, on a later trigger, and for every waiter simultaneously, and, critically, does not resolve while no source has fired).

`cargo test --lib` on this host: 1260 passed, 3 ignored (the `#[cfg(windows)]` modules, which compile on this host under `cfg(test)` per module gating but do not execute their Windows-only assertions here). `cargo fmt --check`, `cargo clippy --lib --tests -- -D warnings`, and `cargo check --all-targets` are all clean on the host toolchain.

One dead-code detail specific to this PR: `service_cmd/mod.rs`'s blanket `#[cfg_attr(not(target_os = "linux"), allow(dead_code))]`, which predates this PR, would have hidden unused items across the entire Windows-specific tree this PR adds. Each new module re-enables the lint for itself with an inner `#![warn(dead_code)]`, so the Windows backend does not inherit a blanket suppression meant for code that had no Windows implementation yet.

---

## 3. Technical Decisions

### 3.1 Probe the actual privilege requirement instead of the process token's elevation flag

**Context**: issue #311 proposed detecting elevation via the process token's elevation flag, a common Windows pattern.

**Chosen approach, deliberately deviating from the issue**: each action opens exactly the SCM handles it actually needs and translates a resulting `ERROR_ACCESS_DENIED` into `ServiceError::NeedsElevation` with a "Run as administrator" message, rather than checking the token flag up front.

| Option | Pros | Cons |
|---|---|---|
| Process token elevation flag (issue's proposal) | Conventional Windows pattern; a single, cheap check | Tests a proxy for the actual requirement, not the requirement itself; requires an unsafe FFI probe that could not be executed even once from this development host |
| **Chosen: open the SCM handle the action needs, translate `ERROR_ACCESS_DENIED`** | Tests the capability actually required; `status`, which needs no elevated right, keeps working unelevated because it never asks for a right it does not need; no new unsafe probe to write blind | Requires per-action care that the right handle is requested; a mistake here would surface as a wrong error message rather than a wrong elevation check |

**Rationale**: an elevation-flag check would have been exactly the kind of code this PR could not verify beyond "it compiles," since it is unsafe FFI with no execution path available. Testing the actual SCM permission instead of a proxy for it is both more correct in principle (a flag can be true while a specific handle-open still fails for other reasons) and safer to ship without ever having run it.

### 3.2 A `watch`-backed `Latch` instead of `tokio::sync::Notify`, for a signal that must never be lost

**Context**: `all-smi api` previously learned about shutdown from exactly two sources, `Ctrl+C` and `SIGTERM`. The SCM has neither; a Stop control arrives on a handler thread with no async signal to await, and it can arrive before the API server has even spawned its listeners.

| Option | Pros | Cons |
|---|---|---|
| `tokio::sync::Notify` | Lightweight, designed for exactly this kind of external wakeup | `notify_one` hands its single stored permit to whichever waiter subscribes first, and `notify_waiters` is lost entirely if it fires before any waiter has subscribed, which is exactly the SCM's early-Stop scenario |
| **Chosen: a one-way boolean latch over `tokio::sync::watch`, `Clone`-able, triggered via `send_replace`** | Every waiter, including ones created after the trigger, resolves; `send_replace` always writes the value even if every receiver has been dropped, so a trigger before any subscription is never silently lost | Slightly heavier than `Notify`; requires an up-front read in `wait()` before the first `.changed()` await, since `subscribe()` marks the current value as already seen |
| `std::sync::atomic::AtomicBool` plus manual polling | Simplest possible primitive | No async wakeup at all; would require a polling loop instead of a clean `select!` arm |

**Rationale**: `send` on a `watch::Sender` fails once every receiver has dropped and, on that path, would leave the stored value untouched, exactly the failure mode that would silently lose a Stop control arriving before any listener task has subscribed. `send_replace` always writes regardless of receiver state. The mirror-image guarantee, that `wait()` also resolves immediately for a *late* subscriber (one created after the trigger already fired), required an explicit "read the current value before the first await" step in `wait()`, since `subscribe()` on a `watch::Receiver` marks the value at subscription time as already seen, which would otherwise make a post-trigger `.changed()` wait forever.

### 3.3 Reuse `ServiceError::Conflict` for a foreign registered service rather than adding a Windows-specific variant

**Context**: the SCM offers no equivalent of the systemd backend's managed-by marker comment stamped into a unit file; there is nowhere in a Windows service registration to stash an identity marker the way a text unit file allows.

**Chosen approach**: the registered binary path is the identity check instead. A service named `all-smi` whose registered executable path does not match `current_exe()` is treated the same way the Linux backend treats a unit file lacking its marker comment, raising `ServiceError::Conflict`, with `--force` lifting the refusal through the existing `uninstall_forced` hook PR #319 already defined.

**Rationale**: this adds no new `ServiceError` variant, keeping the cross-platform contract PR #319 established exactly as specified, while still giving Windows a mechanism functionally equivalent to the marker-comment guard, adapted to what the SCM can actually express (a registered path comparison rather than a stamped comment).

---

## 4. Implementation Details

### 4.1 Architecture Changes

```
[Cross-platform contract from #319, unmodified]

service_cmd::backend()
    #[cfg(target_os = "windows")]  ->  ScmBackend   (was: NotSupported, tracked as #311)

[New: the SCM backend's own layering]

ScmBackend (impl ServiceBackend)
    install/uninstall/start/stop/restart/status
        -> scm.rs        pure mapping: SERVICE_STATUS <-> ServiceStatus, Win32 error translation,
                          idempotency-check command-line parsing (cfg(any(windows, test)))
        -> scm_backend.rs  the windows-service crate calls themselves (cfg(windows) only)

all-smi service run  (hidden CLI action, dispatched ahead of backend())
    -> scm_host.rs    service_dispatcher::start, control handler, START_PENDING -> RUNNING
                      -> STOP_PENDING -> STOPPED, owns the Tokio runtime
    -> scm_log.rs     rolling file logging under %PROGRAMDATA%\all-smi\logs (daily, 14 kept)

[Cross-platform shutdown plumbing, not Windows-specific]

src/api/latch.rs      Latch: watch-backed one-way boolean gate, Clone, send_replace
src/api/shutdown.rs   shutdown_signal() now selects on: ctrl_c | SIGTERM (unix) | Latch::wait()
src/api/server.rs     each listener calls mark_serving() after a successful bind;
                      run_api_mode uses try_init() instead of init() on the tracing subscriber
                      (the service host installs the file subscriber first)
```

### 4.2 Key Code Changes

**File: `src/api/latch.rs` (the primitive the SCM Stop path relies on)**
```rust
pub struct Latch { tx: Arc<watch::Sender<bool>> }

impl Latch {
    pub fn trigger(&self) {
        // `send` fails once every receiver has dropped and leaves the stored
        // value untouched on that path, which would silently lose a Stop
        // control that arrived before any listener task subscribed.
        // `send_replace` always writes.
        self.tx.send_replace(true);
    }

    pub async fn wait(&self) {
        let mut rx = self.tx.subscribe();
        // `subscribe` marks the current value as seen, so a trigger that
        // happened before this call would never show up as a change.
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
**Reason for change**: this is the mechanism that lets a Stop control delivered on the SCM's own handler thread, entirely outside async code, reach `run_api_mode`'s graceful-shutdown path, and it does so without ever losing the signal regardless of when a listener subscribes relative to when Stop arrives.

**File: `src/api/server.rs` (readiness and subscriber-conflict fixes needed for the service host)**
```rust
// try_init rather than init: the Windows service host installs a file-based
// tracing subscriber first, since stdout is void under the SCM, and init()
// panics on a second registration.
if tracing_subscriber::registry()
    .with(...)
    .with(tracing_subscriber::fmt::layer())
    .try_init()
    .is_err()
{
    tracing::debug!("a tracing subscriber is already installed; keeping the host's");
}
...
mark_serving();  // after each successful listener bind
```
**Reason for change**: the SCM should report `SERVICE_RUNNING` only once the port is genuinely accepting connections, and the service host must be free to install its own log sink first without crashing the shared `run_api_mode` path that every other entry point also uses.

### 4.3 Data Model Changes

No wire-format or metrics change. `%PROGRAMDATA%\all-smi\config.toml` is a new Tier 2 candidate in `candidate_config_paths()`, appended after the existing `%APPDATA%` candidate; `all-smi config path` and the `--help` block pick it up automatically since both already read from that same function.

---

## 5. Learning Points

### 5.1 A cross-compilation target that cannot build the real crate still needs a way to verify real code against it

**Concept**: when the actual dependency tree cannot cross-compile for a target (here, `zstd-sys`'s build script needing Windows headers unavailable on macOS), the choice is not "verify fully" versus "verify nothing." An isolated crate that includes the genuine source files via `#[path]` and stubs only the specific modules that drag in the broken dependency subtree preserves type-checking and linting for everything else, at the cost of the stubbed modules' own correctness being unverified by this method.

**Application in this PR**: `api` and `device` were stubbed because they are what pulls `zstd-sys` in transitively; `service_cmd`, `common`, `cli`, `cli_service`, and `utils::command`, the actual subject of this PR, compile as the real source through the probe.

### 5.2 Dead-code analysis is per compilation target, and a probe crate needs to mirror that shape to be trustworthy

**Concept**: this is the same lesson PR #319's report records for the Linux probe, independently rediscovered (or rather, applied from the start this time) for the Windows one. A `pub` item automatically counts as reachable in a library target; the same item can be genuinely unreferenced behind a binary's private module tree. A probe that only builds a library target inherits the library target's blind spot.

**Application in this PR**: the Windows probe was built with both a library and a binary target from the outset, avoiding the gap PR #319's own probe had to be extended to close after a real CI failure exposed it.

### 5.3 Verifying that a test harness catches mistakes is different from writing the harness

**Concept**: a probe crate that type-checks and lints code is only as trustworthy as evidence that it actually rejects broken code, not just that it accepts correct code. The two are not the same claim.

**Application in this PR**: deliberately injecting a type error into each of the three Windows-only modules and confirming the probe produced a compile error each time is what turns "this probe should catch mistakes" into "this probe does catch mistakes," and the two real defects it caught along the way (a visibility mistake, a `ptr_arg` lint violation) are independent confirmation that this was not a hypothetical exercise.

---

## 6. Further Learning

### Key Terms

| Keyword | Description | Relevance |
|---|---|---|
| `windows-service` crate | Rust bindings to the Win32 Service Control Manager API, avoiding shelling out to `sc.exe` | The dependency this backend is built on |
| `zstd-sys` build-script cross-compilation | A C dependency's build script failing to cross-compile for `x86_64-pc-windows-msvc` from a non-Windows host | The wall that made a probe crate necessary (section 2.1) |
| Per-target dead-code analysis | Rust's `dead_code` lint evaluated separately for a crate's library and binary compilation targets | Why the probe needs both target kinds (section 3, PR #319's report section 2.2) |
| `tokio::sync::watch` vs. `Notify` | Two async notification primitives with different lost-wakeup behavior | Why `Latch` is built on `watch` (section 3.2) |
| `send_replace` | A `watch::Sender` method that always writes, even with no live receivers | What prevents an early SCM Stop from being silently lost |
| `ERROR_ACCESS_DENIED` translation | Mapping a Win32 error code onto `ServiceError::NeedsElevation` | The elevation strategy chosen over a token-flag probe (section 3.1) |

### Related Technologies and Frameworks

- Win32 Service Control Manager API and the `windows-service` crate's abstraction over it.
- `tokio::sync::watch` channels as a lost-wakeup-resistant alternative to `Notify` for one-shot latch semantics.
- Rust's per-compilation-target dead-code analysis and its implications for any crate that ships both a library and a binary target.

### Related PRs and Issues

- Issue #311: the issue this PR closes.
- PR #319 (issue #309): defines the `ServiceBackend` contract this PR implements unmodified, and whose own Linux probe crate first hit the per-target dead-code gap this PR's probe was built to avoid from the start.
- PR #321 (issue #310): the macOS launchd backend, which independently found and fixed the same class of graceful-shutdown-on-SIGTERM bug this PR's `Latch`/`shutdown_signal` work addresses for the SCM's Stop control, via a different code path (`src/main.rs` rather than `src/api/server.rs`).

---

## 7. Change Summary

### Statistics

| Item | Value |
|---|---|
| Files changed | 23 |
| Lines added | +3351 |
| Lines removed | -92 |
| Commits | 8 |
| `cargo test --lib` on the host | 1260 passed, 3 ignored (Windows-only modules) |

### Changes by Category

| Category | Summary |
|---|---|
| SCM backend | `scm.rs` (pure mapping and parsing), `scm_backend.rs` (`ServiceBackend` impl over `windows-service`), `scm_host.rs` (`service run` dispatcher and control handler), `scm_log.rs` (rolling file logging) |
| Cross-platform shutdown | New `src/api/latch.rs` (`Latch` primitive) and `src/api/shutdown.rs` (`shutdown_signal` moved out of `server.rs`, extended with the latch source) |
| Config discovery | `%PROGRAMDATA%\all-smi\config.toml` added to `candidate_config_paths()` on Windows |
| CLI | Hidden `ServiceAction::Run` variant; `service_subcommand_is_registered` test tightened to assert `run` is the *only* hidden action |
| Verification tooling | Isolated Windows-target probe crate (library + binary targets), verified via injected type errors |
| CI | Gated `windows-service` job on a self-hosted runner, disabled by default via a repository variable, never executed |
| Docs | README "Windows (Service Control Manager)" subsection; man page updates |

### Related Commits

| SHA | Type | Message |
|---|---|---|
| `2550593e` | feat(service) | run API mode as a Windows service (#311) |
| `96eb5bc2` | fix(cli) | register `service run` in the subcommand contract test |
| `c2bcf6a3` | docs | fix two intra-doc links in the shutdown modules |
| `db217d5f` | docs | drop an em dash from the %PROGRAMDATA% config doc comment |
| `0c0b2316` | docs(service) | record how to check the Windows-only modules |
| `e6f7b925` | merge | origin/main into feature/issue-311-windows-scm-service |
| `0ccbd1fa` | docs | list the Windows machine-wide config path in the README table |
| `e3304c1b` | docs(service) | correct the unsupported-platform message after #310 |

Merged to `main` as `464bfdda`. Closes #311.

---

## 8. Follow-up Actions

### Required

Every runtime acceptance criterion below is unverified from the macOS implementation environment, and each is left unticked on the originating issue rather than claimed:

- **Install, start, and reboot persistence.** `service install --now` registering and starting, `/metrics` answering, `service status` reporting a PID, and the service surviving a reboot with no login.
- **SCM stop semantics.** `service stop` and a `services.msc`-initiated stop both reaching `SERVICE_STOPPED` within the wait hint, the energy WAL flush line appearing in the log, and no orphan process.
- **Failure-action recovery** after `taskkill /F` on the main process.
- **The non-elevated refusal actually firing.** The mapping from `ERROR_ACCESS_DENIED` to `NeedsElevation` is unit tested; that the SCM actually returns that code to an unelevated caller for each of install, uninstall, start, and stop has not been observed.
- **`service run` from a console failing gracefully.** The message for `ERROR_FAILED_SERVICE_CONTROLLER_CONNECT` is unit tested; that `StartServiceCtrlDispatcher` actually returns that code outside the SCM has not been observed.
- **Log files** appearing under `%PROGRAMDATA%\all-smi\logs` and rotating as configured.
- **The `%PROGRAMDATA%` config honored end to end**: changing `api.port`, restarting, and the listener actually moving.
- **`cargo test` on Windows itself**, and therefore the `#[cfg(windows)]` test modules (`scm_backend_tests.rs`, `scm_host_tests.rs`), compiled here but never executed, including the assertions that the raw Win32 constants in `scm.rs` still agree with the `windows-service` crate's own enums.
- **The `windows-service` job on the self-hosted runner has never executed**, gated behind the repository variable `ENABLE_WINDOWS_SERVICE_SMOKE`, unset at merge time. Its first real run should be treated as part of the outstanding work for this feature, not as a regression signal if it fails.

### Monitoring Required

- Whether the `Environment` `REG_MULTI_SZ` registry value for `RUST_LOG` behaves as documented; this is implemented by Windows itself rather than by this change, and was not exercised.

### Future Improvements

- Enabling `ENABLE_WINDOWS_SERVICE_SMOKE` and running the gated CI job at least once against the self-hosted `windows-on-macmini02-x64` runner, which is the only path to closing the verification gap above without a maintainer's own Windows access.
- Confirming the probe crate's stub boundary (`api`, `device`) has not drifted from the real modules' public surface as those modules evolve, since the probe's value depends on the stubs remaining faithful enough to not mask a real incompatibility.

---

## Appendix

### A. Test Results

- `cargo fmt --check`: clean.
- `cargo clippy --lib --tests -- -D warnings`: clean.
- `cargo check --all-targets`: clean.
- `cargo test --lib`: 1260 passed, 3 ignored.
- `cargo clippy --target x86_64-pc-windows-msvc --lib --bins --tests -- -D warnings` (via the probe crate): clean.
- Probe coverage verification: a deliberate type error injected into each of `scm_backend.rs`, `scm_host.rs`, and `scm_log.rs` produced a compile error through the probe in every case.
- Two real defects caught by the probe before merge: `raw_code`/`describe` visibility across a module boundary; `build_service_info` taking `&PathBuf` instead of `&Path`.
- **Not verified**: anything requiring actual execution or linking on Windows. The probe crate does not link on this host (no MSVC linker), so link-time correctness (symbol resolution, ABI agreement with `windows-service`) is unverified.

### B. Performance Benchmarks

Not applicable; nothing in this PR has been executed to benchmark. The qualitative claims (readiness reported only after a successful listener bind, rolling log retention of 14 files) are structural, verified by code review and unit test, not by measurement.

### C. References

- `windows-service` crate documentation and its `service_dispatcher`/`define_windows_service!` macros.
- Win32 Service Control Manager API: `SERVICE_STATUS`, `StartServiceCtrlDispatcher`, `ERROR_ACCESS_DENIED`, `ERROR_FAILED_SERVICE_CONTROLLER_CONNECT`.
- `tokio::sync::watch`: channel semantics, `send_replace`, and `borrow_and_update`.
- PR #319's report: the per-compilation-target dead-code analysis gap this PR's probe crate was built to avoid from the outset.
- Issue #311: the issue this PR closes.
