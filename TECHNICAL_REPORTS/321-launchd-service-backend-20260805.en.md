# Technical Report: PR #321 - feat(service): run API mode as a launchd service on macOS

**Date**: 2026-08-05
**Status**: Completed and verified live in user scope on real Apple Silicon hardware; system-domain (root LaunchDaemon) path unverified (see section 8)
**Languages**: Rust, YAML (GitHub Actions), XML (launchd property list)
**Risk Level**: Medium (a service-management feature plus a genuine cross-cutting bug fix to API mode's SIGTERM handling that affects the already-shipping Linux systemd path too)

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

PR #321 replaces the macOS arm of `service_cmd::backend()`, which PR #319 left as `NotSupported` pointing at this issue, with a real launchd backend implementing the same `ServiceBackend` contract with no change to it: same methods, same `Scope` semantics, same exit codes, same error variants. Along the way it fixes a pre-existing bug that made API mode's graceful shutdown path unreachable on SIGTERM, on every platform, not only macOS.

The centerpiece is a self-correction the PR's own investigation produced. The initial reasoning, by analogy with PR #319's systemd finding that `ProtectKernelModules=` kills a user-scope unit at `218/CAPABILITIES` before `ExecStart`, assumed launchd would behave the same way toward keys a `gui/$UID` LaunchAgent cannot honor (`UserName`, `GroupName`, `InitGroups`). It does not. A probe LaunchAgent bootstrapped into `gui/501` on macOS 26.6 with `UserName root` and `GroupName wheel` both set was tested by having its program print `id`: `launchctl bootstrap` succeeded, and the effective uid/gid was `501`/`20`, the invoking user's own identity, not root's. launchd silently ignores these keys in a per-user domain rather than refusing to start. The keys are therefore dropped from a user-scope plist not to prevent a crash, since there is none, but because keeping them would ship a plist that reads, to anyone auditing what runs privileged on the machine, as a root job when it is not one, and because `launchd.plist(5)` documents the keys as requiring root without documenting the silent-ignore fallback, leaving it free to become fatal in some later release. Since no bootstrap failure will ever catch a regression here, a render-time test asserts the keys' absence directly.

The SIGTERM bug this PR fixes is unrelated to launchd specifically: `run_command`'s signal handler called `std::process::exit(0)` unconditionally, which won the race against `run_api_mode`'s own post-serve cleanup (the energy WAL flush, Unix socket removal), so every SIGTERM, on Linux under systemd exactly as much as under launchd, dropped the last batch of accumulated Joules and left a stale socket behind. `Api` now owns its shutdown the same way `Record` already did, including through the `[general].default_mode` redispatch path. Verified live on an M1 Ultra: the LaunchAgent's exported metric name set is byte-identical to a foreground `all-smi api` run, and stopping it via `launchctl bootout` now reaches the final energy-WAL flush log line, in 0.45 seconds where SIGTERM previously produced no such line at all. Two environment findings surfaced along the way: `ProcessType Background` (required so the monitor does not compete with the GPU workload it watches for P-core time) costs roughly 6.3 seconds of startup under launchd versus 0.6 seconds in the foreground, dominated by IOReport channel enumeration running at background QoS; and a binary on an external volume hangs indefinitely in `dyld`'s `open()` under launchd, a TCC gate on launchd-spawned processes reading from removable volumes, not a code defect. Total: 15 files, +2581/-59, one commit, closing #310.

---

## 1. Problem Statement

### 1.1 Background

PR #319 established the cross-platform `all-smi service` framework and its systemd implementation, leaving explicit `NotSupported` arms for macOS (this issue) and Windows (issue #311, implemented in parallel by PR #320) naming their tracking issues. Issue #310 additionally covers the Homebrew-managed path: `brew services start all-smi` (per-user, `gui/$UID`) and `sudo brew services start all-smi` (system domain, survives reboot with nobody logged in), which needs a `service do` block added to the formula in the separate `lablup/homebrew-tap` repository, a change this PR cannot make directly.

### 1.2 Existing Issues

- **Issue 1 (no launchd backend)**: `backend()`'s macOS arm returned `NotSupported`, so a zip or local-build install had no equivalent of `all-smi service install` that the Linux systemd path already had.
- **Issue 2 (no system-wide macOS config candidate)**: `candidate_config_paths()` had no macOS analogue to the `/etc/all-smi/config.toml` tier PR #319 added for Linux, so a root LaunchDaemon running outside any login session had nowhere non-per-user to be configured (a system LaunchDaemon's `~/Library/Application Support` resolves to root's home, not the operator's).
- **Issue 3 (API mode's SIGTERM handler bypassed its own graceful shutdown)**: `run_command` installed an unconditional handler calling `std::process::exit(0)` for every subcommand except `Record`, which won the race against `run_api_mode`'s post-serve cleanup on every SIGTERM. This is not a launchd-specific defect: it affects the already-shipping Linux systemd deployment from PR #319 exactly as much, since `systemctl stop` also delivers SIGTERM.
- **Issue 4 (unverified assumption about Apple Silicon native readers in a daemon context)**: whether IOReport, the SMC, and `NSProcessInfo.thermalState` resolve correctly with no controlling terminal, no sudo, and no GUI session had never been tested from an actual launchd-managed process.

### 1.3 Risk Assessment

| Risk | Impact | Likelihood |
|---|---|---|
| Reasoning from the systemd analogy (PR #319) without measuring launchd's actual behavior | Medium: would have shipped a plist that reads as more privileged than it is, or a wrong assumption about how the failure surfaces | Caught by direct measurement before merge (section 2.1); the assumption was wrong and corrected |
| The SIGTERM bug persists on the already-shipping Linux systemd path | High: every `systemctl stop` on a Linux deployment from PR #319 also drops the final energy WAL batch and leaves a stale socket, not just the macOS path this PR adds | Fixed in this same PR, cross-platform, not scoped to launchd only |
| `ProcessType Background`'s startup cost is not accounted for by a readiness check that only polls process state | Medium: a supervisor or health check assuming the service is ready the moment launchd reports it running would query before the exporter has actually collected anything | Documented in this PR; the follow-on defect this exact gap caused in CI is fixed separately in PR #323 |
| System-domain (root LaunchDaemon) behavior is unverified | Medium: reboot persistence, running with nobody logged in, and the Homebrew `sudo brew services start` path all depend on the system domain specifically | Explicitly left open; the user-scope path is fully verified as a substitute signal, not a replacement (section 8) |

---

## 2. Technical Review

### 2.1 The self-correction: launchd does not refuse what it cannot honor, it silently ignores it

**The assumption, by analogy.** PR #319 found that a systemd user-scope unit carrying `ProtectKernelModules=` (which needs a privilege an unprivileged manager cannot obtain) dies at `218/CAPABILITIES` before `ExecStart`. The natural first assumption for launchd's equivalent problem, a `gui/$UID` LaunchAgent carrying `UserName`, `GroupName`, or `InitGroups` (keys that require root to apply, per `launchd.plist(5)`), was that bootstrapping such an agent would similarly fail.

**What was actually measured.** A probe LaunchAgent was bootstrapped into `gui/501` on macOS 26.6 (Darwin 25.6), with a program that printed `id`:

| Plist keys | `launchctl bootstrap` | Effective uid/gid |
|---|---|---|
| none | succeeds | `501` / `20` |
| `UserName root`, `GroupName wheel` | succeeds | `501` / `20` |
| `InitGroups` + `UserName root` | succeeds | `501` / `20` |

Every case succeeded, and the effective identity was always the invoking user's own, never root's. The keys are silently ignored in a per-user domain rather than refused.

**Why the keys are still dropped, given that nothing crashes.** Not to prevent a failure, since there is none, but because keeping them would ship a plist that misrepresents itself: a file in `~/Library/LaunchAgents` declaring `UserName root` reads, to anyone auditing what runs with elevated privilege on the machine, as a root job, when in fact it is not one and cannot be one in that domain. Two further considerations reinforce dropping the keys rather than leaving them and documenting the quirk: `launchd.plist(5)` documents these keys as requiring root and says nothing about a silent-ignore fallback, so that behavior is unspecified and free to become fatal in a future macOS release; and `InitGroups` is separately documented as ignored whenever `UserName` is unset, which in a user-scope render it now always is, making its presence there doubly meaningless.

**The corollary this produces for testing.** Since no bootstrap failure will ever surface a regression here (the whole point is that launchd does not complain), correctness has to be enforced at render time instead of relying on an eventual runtime signal. A dedicated test asserts the absence of `UserName`, `GroupName`, and `InitGroups` from every user-scope render, and a second test asserts that every top-level key in the shipped plist template is classified by one of the "kept" or "dropped" lists, so a future key added to the template without a classification decision fails in CI rather than silently reaching whichever list the code happened to default to.

**The mirror-image case, also measured rather than assumed**: `HardResourceLimits` is deliberately absent from the canonical template, for the reason opposite to the one above. Only root can *raise* a hard limit, so a user-scope job gains nothing from one being present, while `SoftResourceLimits` is kept in both scopes, since raising a soft rlimit up to the already-inherited hard limit needs no privilege at all.

### 2.2 The cross-platform bug: API mode's SIGTERM handler raced its own cleanup

**Symptom.** `run_command` installed a signal handler for every subcommand except `Record`:

```rust
let is_record = matches!(cli.command, Some(Commands::Record(_)));
if !is_record {
    tokio::spawn(async {
        signal::ctrl_c().await.expect("Failed to listen for Ctrl+C");
        std::process::exit(0);
    });
}
```

For `Api`, this handler and `run_api_mode`'s own graceful-shutdown path (axum's `with_graceful_shutdown`, followed by the energy WAL flush and, on Unix, the socket file's removal) both listen for the same signal. Whichever wins the race determines whether cleanup runs at all. The unconditional `std::process::exit(0)` in the spawned handler routinely won, so every SIGTERM, the exact signal `launchctl bootout` and `systemctl stop` both send, terminated the process before the WAL flush or socket cleanup had a chance to run.

**Why this matters for a service specifically, not just in general**: this is not an edge case reachable only under contrived conditions. SIGTERM is precisely how both service managers this project now integrates with (launchd via this PR, systemd already shipping from PR #319) stop a managed process, so the bug fired on every single restart of either service, not occasionally.

**The fix**: `Api` now owns its own shutdown the same way `Record` already did, including through the `[general].default_mode` redispatch path, where `cli.command` is `None` and the mode is only known after reading `Settings`; the recursive call in that path cannot uninstall a handler the outer call already spawned, so `owns_shutdown` resolves the effective mode from `settings.general.default_mode` directly rather than from `cli.command` alone. The collectors (the native metrics manager on macOS, the hl-smi manager on Linux) are torn down after `run_api_mode` returns, mirroring the order `view` mode already used, rather than being abandoned mid-cleanup by a competing `exit(0)`.

**Verified effect**: before the fix, SIGTERM produced no `energy WAL: shutdown requested` log line at all. After, the line appears and the process exits in 0.45 seconds.

### 2.3 Compatibility and Dependencies

- **Breaking changes**: none to the `ServiceBackend` contract; the macOS arm is the only `backend()` branch this PR replaces.
- **New dependencies**: none. The plist renderer and `launchctl` wrapper use only the standard library and existing workspace dependencies.
- **Compatibility**: `/Library/Application Support/all-smi/config.toml` is appended to `candidate_config_paths()` as a new Tier 2 candidate, ordered after every per-user candidate, additively, matching the pattern PR #319 established for Linux and PR #320 for Windows.

### 2.4 Code Quality

New unit tests: 110 in `service_cmd::` (plist rendering including multi-line `<array>`/`<dict>` value handling, XML escaping and control-character rejection, account-name validation, the user-scope dropped-key list and its classification-completeness guard; `launchctl print`/`print-disabled` parsing; the managed-marker guard and atomic `0644` plist writing), 17 in `common::paths` (the macOS system-wide candidate and its ordering). `cargo clippy --lib --tests -- -D warnings` and `cargo clippy --bin all-smi -- -D warnings` were run as two separate invocations deliberately: the library-target check caught nothing, but the binary-target check flagged `SERVICE_NAME` as dead code, since it is reachable only from the systemd backend, which is compiled in neither a non-Linux nor a test build; this is the same per-compilation-target dead-code blindness PR #319 and PR #320 both independently document, observed here from the library-check side rather than assumed absent.

---

## 3. Technical Decisions

### 3.1 Drop `UserName`/`GroupName`/`InitGroups` from a user-scope plist for integrity, not for survival

Covered in full in section 2.1; recorded here as the PR's central technical decision because it reverses an initial assumption based on a plausible but wrong analogy to a different service manager's behavior, and does so on the strength of a direct measurement rather than documentation, since the documentation itself does not describe the fallback behavior.

### 3.2 launchd verb mapping: `install` writes without loading; `install --now` unloads and reloads; `stop` unloads and leaves the plist

**Context**: launchd has no separate "enabled at boot" state the way systemd's `enable`/`disable` does. A plist present in `LaunchDaemons` or `LaunchAgents` is bootstrapped automatically at boot or login, and `RunAtLoad` starts it from there; `launchctl` caches a loaded job's definition, so bootstrapping a changed plist over an already-loaded one fails rather than replacing it in place.

| Verb | Chosen behavior | Rationale |
|---|---|---|
| `install` (no `--now`) | Write the plist, stop | This *is* "enabled at boot, not running yet", matching the semantics `install` without `--now` has on the systemd backend |
| `install --now` | Boot the job out, then back in | Loading over an already-loaded job with a changed definition fails on launchd; the out-then-in sequence is what makes the new definition take effect |
| `stop` | Boot the job out, leave the plist | Matches `systemctl stop`: the service returns at the next boot/login, since the plist (the "enabled" state) still exists |
| `status` | `installed` from plist presence on disk; `enabled` from `launchctl print-disabled` | `launchctl print` only knows about currently-loaded jobs, so plist presence, not load state, is what answers "installed" |
| `install` also runs `launchctl enable` | Clears a persistent disable override | A `launchctl disable` override outlives both the plist and a reboot; leaving it in place after a fresh `install` would silently prevent the newly-installed job from ever starting |
| `uninstall` deliberately does not `disable` | Symmetric with the `install`/`enable` pairing | Disabling on uninstall would leave a lingering override that silently blocks a future `install`, the same problem `install`'s `enable` call exists to clear |

**Rationale**: each mapping decision follows from a specific, verified launchd behavior (caching a loaded definition, a persistent disable override outliving the plist) rather than from assuming a one-to-one correspondence with the systemd backend's verbs.

### 3.3 `--service-user` drops `GroupName` instead of mirroring the account name, unlike the systemd renderer

**Context**: the systemd template renderer (PR #319) sets both `User=` and `Group=` from `--service-user`, relying on `systemd-sysusers` conventionally creating a group named after the account. The launchd renderer diverges here.

**Chosen approach**: `--service-user` sets `UserName` and drops `GroupName` entirely, rather than mirroring the account name into it.

**Rationale**: macOS has no equivalent convention that a regular or service account owns an eponymous group; a service account created with `dscl` may have any primary group at all. Omitting `GroupName` lets launchd fall back to the account's actual primary group straight from the password database, which is always correct, whereas mirroring the account name into `GroupName` could silently name a group that does not exist or does not match the account's real group membership.

### 3.4 Fix the SIGTERM bug in this PR rather than deferring it to a separate one

**Context**: the bug (section 2.2) is not launchd-specific; it affects the already-merged Linux systemd path from PR #319 equally.

**Chosen approach**: fixed here, in the same PR, rather than filed as a separate follow-up issue.

**Rationale**: the bug was discovered specifically because this PR needed `stop` (i.e., `launchctl bootout`, which sends SIGTERM) to reach the energy-WAL flush for its own live verification checklist; deferring the fix would have shipped a launchd backend whose own acceptance criteria could not be demonstrated to pass. Fixing it in place, with the cross-platform scope made explicit in code comments and the PR description, was judged more useful than a narrower macOS-only workaround.

---

## 4. Implementation Details

### 4.1 Architecture Changes

```
[Cross-platform contract from #319, unmodified]

service_cmd::backend()
    #[cfg(target_os = "macos")]  ->  LaunchdBackend   (was: NotSupported, tracked as #310)

[New: the launchd backend's own layering]

LaunchdBackend (impl ServiceBackend)
    -> plist.rs      render_plist(): walks <key>/value pairs (not lines) so a multi-line
                     <array>/<dict> value is dropped as a unit; rewrites ProgramArguments,
                     log paths, UserName/GroupName per scope
    -> launchctl.rs  layout resolution (plist path, log path, domain, service target),
                     `launchctl` invocation, `launchctl print` / `print-disabled` parsing
    -> launchd.rs    verb policy (section 3.2) and the on-disk half: managed-marker guard,
                     atomic 0644 plist write, log-directory creation

[Cross-cutting fix, not launchd-specific]

src/main.rs   Api now owns its own SIGTERM shutdown, matching Record; owns_shutdown
              resolves through [general].default_mode for the None-command redispatch path
```

### 4.2 Key Code Changes

**File: `src/main.rs` (the SIGTERM ownership fix)**
```rust
// * `Api` owns its shutdown for the same reason. `run_api_mode`
//   waits for axum's graceful shutdown, then performs the final
//   energy-WAL flush and fsync and removes the Unix socket file.
//   The unconditional exit below won that race, so every SIGTERM
//   dropped the last batch of accumulated Joules and left a stale
//   socket behind. That is not an edge case for a service: SIGTERM
//   is exactly how `launchctl bootout` and `systemctl stop` end it,
//   so it happened on every restart (issues #191, #309, #310).
let owns_shutdown = match &cli.command {
    Some(Commands::Record(_) | Commands::Api(_)) => true,
    // `None` redispatches through `[general].default_mode` further
    // down, and the recursive call cannot uninstall a handler this
    // call already spawned. Resolve the effective mode here.
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
**Reason for change**: this is the fix described in sections 2.2 and 3.4. The `None => settings.general.default_mode == "api"` arm is the part that is easy to get wrong: the redispatch through `default_mode` means the CLI-level `cli.command` alone cannot answer "does this run own its shutdown," since the answer depends on a config value not yet read at the point the handler decision is made.

**File: `src/service_cmd/plist.rs` (the user-scope key classification this PR's measurement produced)**
```rust
/// Keys stripped when rendering a user-scope LaunchAgent.
///
/// A `gui/$UID` domain is owned by an unprivileged user and cannot
/// `setuid` to another account. systemd answers that by refusing the
/// unit outright... launchd does not. Bootstrapping a LaunchAgent that
/// carries `UserName`, `GroupName`, or `InitGroups` succeeds, the job
/// runs, and the keys are **silently ignored**.
///
/// | Plist keys | `launchctl bootstrap` | Effective uid/gid |
/// |---|---|---|
/// | none | succeeds | `501` / `20` |
/// | `UserName root`, `GroupName wheel` | succeeds | `501` / `20` |
///
/// So these are dropped not to avoid a crash but because keeping them
/// would ship a plist that lies.
pub const USER_SCOPE_DROPPED_KEYS: &[&str] = &["UserName", "GroupName", "InitGroups"];
```
**Reason for change**: this constant, and the doc comment recording the measured (not assumed) behavior behind it, is the direct artifact of the self-correction in section 2.1.

### 4.3 Data Model Changes

No wire-format or metrics change. `/Library/Application Support/all-smi/config.toml` is a new Tier 2 candidate in `candidate_config_paths()` on macOS, appended after every per-user candidate.

---

## 5. Learning Points

### 5.1 Two service managers solving the same "unprivileged process, privileged directive" problem can choose opposite failure modes

**Concept**: systemd fails closed: a user-scope unit carrying a directive it cannot apply refuses to start, with a specific (if opaque without investigation) exit status. launchd fails open: a LaunchAgent carrying a key it cannot honor in a per-user domain simply ignores that key and runs anyway.

**Application in this PR**: reasoning by analogy from PR #319's systemd finding to predict launchd's behavior would have been wrong in both directions if left unverified: assuming launchd also refuses would have led to writing tests for a failure that never occurs; only measuring it directly (section 2.1) revealed the correct testing strategy (assert the keys' absence at render time, since no runtime signal will ever catch their presence).

### 5.2 A signal handler installed for "every subcommand except one" is a policy that has to be revisited every time a new subcommand needs the same exception

**Concept**: `!is_record` as the condition for installing an unconditional-exit signal handler encodes "every subcommand's shutdown semantics are the same, except `Record`'s." That claim silently stops being true the moment a second subcommand (`Api`) needs the exception, and nothing about the original code would have flagged the gap; it had to be discovered by testing the actual shutdown behavior.

**Application in this PR**: the fix generalizes the condition to `owns_shutdown`, computed per subcommand (and, for the `None`/redispatch case, per effective mode) rather than as a single named exception, which is what makes the next subcommand needing the same exception a matter of adding a match arm instead of renegotiating the condition's shape.

### 5.3 `ProcessType Background`'s cost is not merely "slower," it changes which readiness signal is trustworthy

**Concept**: launchd's `ProcessType Background` selects background QoS scheduling for the whole process, which is the right choice for a monitoring tool that should not compete with the workload it watches for P-core time, but it measurably slows CPU-bound startup work (IOReport channel enumeration) by roughly an order of magnitude (0.6s foreground vs. 6.3s as a LaunchAgent in this PR's measurement).

**Application in this PR**: `service status` reports "running" the moment launchd has spawned the program, which is well before that startup work, let alone the first metrics collection cycle, has completed. This PR documents the gap and adjusts its own CI readiness waits to poll `/metrics` content rather than `service status`; PR #323's report covers the further, more precise version of this same finding (the exact window size and its effect on a CI assertion).

---

## 6. Further Learning

### Key Terms

| Keyword | Description | Relevance |
|---|---|---|
| `gui/$UID` domain | launchd's per-user session domain for a bootstrapped LaunchAgent | Where the `UserName`/`GroupName`/`InitGroups` silent-ignore behavior was measured |
| `launchctl bootstrap` / `bootout` | Commands loading a job into, or removing it from, a launchd domain | The mechanics behind `install --now`'s unload-then-reload and `stop`'s unload-and-leave-plist mapping |
| `RunAtLoad` | Plist key starting a job automatically once bootstrapped | Why launchd has no separate "enabled" concept the way systemd does |
| `ProcessType Background` | launchd key selecting background QoS scheduling | Source of the measured ~6.3s startup cost versus 0.6s foreground (section 5.3) |
| TCC gate on launchd-spawned processes | macOS privacy/security restriction affecting processes launchd starts from certain volumes | Root cause of the external-volume hang noted in this PR's verification |
| `owns_shutdown` | This PR's generalized condition replacing the original `!is_record` special case | The cross-platform SIGTERM fix (sections 2.2, 5.2) |

### Related Technologies and Frameworks

- `launchd.plist(5)` and the launchd job-management model (domains, bootstrap/bootout, `RunAtLoad`, `KeepAlive`).
- IOKit/IOReport and `NSProcessInfo.thermalState` under background QoS scheduling.
- Homebrew's `service do` DSL, generating a launchd plist for `brew services`, and its interaction (or lack of interaction) with this PR's subcommand-based install path.

### Related PRs and Issues

- Issue #310: the issue this PR closes.
- PR #319 (issue #309): defines the `ServiceBackend` contract this PR implements unmodified, and whose `ProtectKernelModules=` finding this PR's initial (and corrected) reasoning was drawn from.
- PR #320 (issue #311): the Windows SCM backend, developed in parallel against the same contract.
- PR #323: the launchd CI smoke test race condition this PR's `ProcessType Background` startup-cost finding foreshadows and which that PR measures precisely.
- Issues #191, #309, #310: all three referenced in this PR's own code comment as affected by the SIGTERM bug this PR fixes.

---

## 7. Change Summary

### Statistics

| Item | Value |
|---|---|
| Files changed | 15 |
| Lines added | +2581 |
| Lines removed | -59 |
| Commits | 1 |
| New unit tests | 127 (110 `service_cmd::`, 17 `common::paths`) |

### Changes by Category

| Category | Summary |
|---|---|
| launchd backend | `plist.rs` (renderer, 370 lines), `launchctl.rs` (invocation and parsing, 349 lines), `launchd.rs` (verb policy and on-disk operations, 319 lines) |
| Cross-platform bug fix | `src/main.rs`: `Api` now owns its SIGTERM shutdown, fixing a defect that also affected the Linux systemd path |
| Config discovery | `/Library/Application Support/all-smi/config.toml` added to `candidate_config_paths()` on macOS |
| Detection | `service_cmd/detect.rs` gains a macOS-specific Homebrew refusal hint (`sudo brew services start all-smi`) |
| CI | New gated `launchd-service` job on `macos-14` |
| Docs | README and man page macOS subsections; a hand-apply checklist for the `lablup/homebrew-tap` `service do` block |

### Related Commits

| SHA | Type | Message |
|---|---|---|
| `3541e77f` | feat(service) | run API mode as a launchd service on macOS (#310) |

Merged to `main` as `8c822aa4`. Closes #310.

---

## 8. Follow-up Actions

### Required

- **Verify the system-domain LaunchDaemon path.** No `/Library/LaunchDaemons` write and no `sudo launchctl bootstrap system` were performed; this path is exercised only by unit tests plus a live check that it correctly refuses without root and writes nothing.
- **Verify reboot persistence**, in either the subcommand or the `sudo brew services` form; unreachable without the system domain.
- **Apply the `service do` block to `lablup/homebrew-tap`.** Nothing was pushed to the tap. A verified diff is included in the PR description, checked locally against a scratch copy with `ruby -c` and `brew style` (matching the 4 pre-existing offenses on the unpatched upstream file, none newly introduced by the service block).
- **Verify `sudo brew services start all-smi` and the tap formula end to end** once the block above is applied.
- **Verify the live Homebrew-path refusal.** `detect::classify` is unit-tested for all three Homebrew prefixes and a macOS-specific hint text is asserted, but no binary was actually placed under `/opt/homebrew` to trigger the refusal for real.

### Monitoring Required

- Whether `/Library/Application Support/all-smi/config.toml` is actually loaded once the system domain is exercised; the candidate list and its ordering are unit-tested, but no file at that path has been read by a real daemon yet.
- The `ProcessType Background` startup-cost gap (section 5.3) as a source of readiness-check races in any consumer polling `service status` rather than actual content; PR #323 is the concrete instance of this already surfacing.

### Future Improvements

- None proposed beyond the required items above; issue #310's own acceptance criteria already enumerate what remains.

---

## Appendix

### A. Test Results

- `cargo fmt --check`: clean. `cargo clippy --lib --tests -- -D warnings`: clean. `cargo clippy --bin all-smi -- -D warnings`: clean after fixing the `SERVICE_NAME` dead-code finding caught only by the binary-target check.
- `cargo test --lib service_cmd::`: 110 passed. `cargo test --lib common::paths`: 17 passed.
- `plutil -lint` on the rendered plist: clean. `man ./docs/man/all-smi.1` renders cleanly.
- Live on an M1 Ultra, macOS 26.6, **user scope**: `status` before install exits 3; `install --user` writes the plist, creates `~/Library/Logs/all-smi`, leaves the job unloaded, `status` reports `installed, enabled, stopped` (exit 3); `start` bootstraps it and `status` reports running with a PID; `install --user --now`, `restart`, `stop`, `uninstall`, and a second `install` over the tool's own plist (idempotent) all verified; `curl localhost:9090/metrics` serves 72 `all_smi_*` lines from the LaunchAgent; `stop` (`launchctl bootout`) reaches the final energy-WAL flush, visible in the log; a hand-written plist is refused by both `install` and `uninstall`, left untouched, with `--force` lifting the refusal; `launchctl disable` is reported as `"enabled": false` and cleared by a subsequent `install`; system scope without root refuses with "requires root" and writes nothing to `/Library/LaunchDaemons`; `all-smi config path` lists `/Library/Application Support/all-smi/config.toml` as the last candidate.
- Apple Silicon metrics under launchd: the LaunchAgent's metric name set is byte-identical to a foreground `all-smi api` run (`diff` of sorted metric names: none). Sample values captured live: `all_smi_gpu_utilization 17.19`, `all_smi_gpu_power_consumption_watts 0.56`, `all_smi_gpu_temperature_celsius 52`, `all_smi_cpu_temperature_celsius 65`, `all_smi_ane_power_watts 0`, `all_smi_thermal_pressure_info 1`, `all_smi_cpu_p_cluster_frequency_mhz 3223`, `all_smi_cpu_e_cluster_frequency_mhz 1978`, `all_smi_chassis_power_watts 48.12`. Cluster frequencies are non-zero thanks to PR #317.
- Startup cost measurement: 0.6s foreground bind, 2.9s under `taskpolicy -b`, 6.3s as a LaunchAgent, dominated by IOReport channel enumeration running at background QoS.
- External-volume finding: a plist pointing at a binary on an external volume hung indefinitely in `dyld`'s `open()` under launchd; re-verified as not a code issue by pointing at an internal-disk path instead.

### B. Performance Benchmarks

The `ProcessType Background` startup-cost measurement above is the only quantitative benchmark in this PR; it is qualitative-turned-quantitative rather than a formal benchmark suite, gathered specifically because it explains an otherwise-surprising delay between `service status` reporting running and the exporter actually answering `/metrics` with data.

### C. References

- `launchd.plist(5)`: documented (and, for the silent-ignore behavior measured here, undocumented) semantics of `UserName`, `GroupName`, `InitGroups`, `HardResourceLimits`, `SoftResourceLimits`.
- `launchctl(1)`: `bootstrap`, `bootout`, `enable`, `disable`, `print`, `print-disabled`.
- Homebrew's `service do` DSL and `Homebrew::Service#process_type`.
- Issue #310: the issue this PR closes.
- PR #319's report: the `ProtectKernelModules=` finding this PR's initial (later corrected) reasoning drew from.
