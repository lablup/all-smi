# Technical Report: PR #383 - Refresh Service Help Backend Text

**Date**: 2026-08-23  
**Status**: Completed  
**Related**: PR #383, Issue #355  
**Risk Level**: Low (help text and one regression test)

---

## Executive Summary

`all-smi service --help` had drifted from what the subcommand actually supports. PR #383 refreshes it so it names the shipped backend set from PR #321 and keeps the `--user` scope wording accurate, and adds a regression test that fails if the same drift returns.

---

## 1. Problem Statement

Three backends shipped over #319, #320, and #321: Linux systemd, Windows SCM, and macOS launchd. The help text was written before the last of them landed and still claimed launchd was tracked separately, while omitting macOS from the supported set.

It also described `--user` without saying where it applies. `--user` is refused on Windows, where the Service Control Manager has no per-user scope, so a Windows operator reading the help had no way to learn that from the help itself.

Help text is the primary documentation for a subcommand an operator runs once and then forgets. Text that lists the wrong platforms sends someone to build a workaround for a backend that already exists.

## 2. Change Summary

| Item | Value |
|------|-------|
| Files changed | 1 (`src/cli.rs`) |
| Lines added | 35 |
| Lines deleted | 8 |
| Tests added | 1 |
| Runtime behavior changed | No |

### What changed

- The service help text names Linux systemd, macOS launchd, and Windows SCM as supported backends.
- The same text clarifies that `--user` applies to Linux and macOS, not Windows.
- A focused `src/cli.rs` regression test fails if the help text again claims launchd is tracked separately or omits macOS support.

## 3. Technical Decisions

### 3.1 The regression test asserts on absence as well as presence

Asserting only that the new wording appears would let a future edit add a correct sentence next to the stale one. The test therefore also asserts that the stale claims are gone, which is the same shape #351 used for the detail-key convention.

This is cheap to keep correct because it tests a string the same file owns, and it is the only mechanism that notices help drift at all: nothing else in the build compares the help text against the backends that exist.

### 3.2 The Homebrew receipt upgrade path was explicitly left out of scope

On 2026-08-23 the tap `service do` block and the Homebrew-installed binary refusal path from #310 were manually verified outside this repository.

The pre-service-block 0.25.0 Homebrew receipt upgrade path was deliberately not addressed, because the next release would carry the new metadata. This PR therefore adds no tap revision and no compatibility workaround for existing receipts. That reasoning held: v0.26.0 shipped the day after.

## 4. Validation Results

| Gate | Result |
|------|--------|
| `cargo test --lib cli::tests` | pass |
| `cargo fmt --check` | pass |
| `cargo run --quiet --bin all-smi -- service --help` | inspected, matches the shipped backend set |

## 5. Outcome and Follow-up

- PR #383 was squash-merged into `main` as `edeaa10`, the last commit before the v0.26.0 release preparation.
- Issue #355 closed automatically through the PR's `Closes #355` link.
- The remaining hardware-verification issues in that group stay open: **#354** (systemd path on a Linux host with dpkg), **#356** (launchd system scope with root and a reboot), and **#357** (Windows SCM backend, `priority:high`). None can be settled without the corresponding machine.

---

## Appendix: Related PRs and Issues

| Number | Relationship |
|--------|--------------|
| PR #319 | Added the `all-smi service` framework and the Linux systemd backend |
| PR #320 | Added the Windows SCM backend, which has no per-user scope |
| PR #321 | Added the macOS launchd backend, the one the help text had not caught up with |
| Issue #310 | The launchd issue whose Homebrew refusal path was verified alongside this PR |
| Issue #332 | The hardware verification backlog tracking #354, #356, and #357 |
