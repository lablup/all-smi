# Technical Report: PR #318 - chore(ci): reduce the blast radius of the Homebrew tap token

**Date**: 2026-08-05
**Status**: Completed on the workflow side. A dry run against a real tag is explicitly deferred (see section 8).
**Languages**: YAML (GitHub Actions), Bash, Python (test harness)
**Risk Level**: High. The job holds `HOMEBREW_TAP_TOKEN`, write access to `lablup/homebrew-tap`, and this PR is entirely about narrowing what can reach that token and where it can leak.

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

PR #313 closed the injection and traversal defects in `update_homebrew_formula.yml` but explicitly deferred seven findings about the job's overall privilege posture: the tap push token persisting on disk, an overbroad `permissions:` grant, no concurrency control, an unpaginated asset listing, artifacts downloaded inside the tap's own working tree, checksums and URLs validated independently rather than as pairs, and an unconfigured `packaging` environment. Issue #316 tracked those seven findings; PR #318 addresses all seven, six by changing the workflow and one by recording a deliberate decision not to change anything.

The centerpiece is how the tap push token is scoped now. The tap clone is anonymous (`lablup/homebrew-tap` is public, and has to be for `brew tap lablup/tap` to work with no credential at all), and the token reaches the job in exactly one place: the environment of the single `git push` invocation, supplied through a `credential.helper` that reads it from `$HOMEBREW_TAP_TOKEN` and never writes it anywhere. That closes all four leak paths a git push token normally has: disk (the old code recorded the token in the clone URL, which git writes verbatim into `.git/config`), argv (a `-c http.extraheader=...` alternative would be visible via `ps` and in any `set -x` trace), the `set -x` trace itself (the helper string is single-quoted, so bash hands git the literal name `$HOMEBREW_TAP_TOKEN` rather than its value, and xtrace does not propagate into the shell git spawns to run the helper), and the OS keychain (resetting `credential.helper` to empty first drops the runner's default helper, so there is nothing left to persist a successful authentication after the fact).

One premise in issue #316 did not hold up under investigation: the issue cited "recent releases already carry 28 assets" as evidence that the unpaginated `assets` array was close to truncating. The actual count is 18, and truncation of that embedded array could not be reproduced against real GitHub releases with far more assets than this project will have soon (`electron/electron` returns all 76 inline, `denoland/deno` all 56). The pagination fix stands anyway, on a documentation argument rather than an observed one: the embedded array carries no `Link` header and no completeness guarantee, whereas `/releases/{id}/assets` is a documented paginated collection, and this workflow treats an absent Intel asset as a decision (skip it, or refuse a version-skew formula), so depending on an undocumented completeness property for that decision is the part worth removing regardless of whether it has bitten yet. Verification is `tests/homebrew-formula-workflow/`, committed this time (PR #313's harness existed but was not checked in), covering 69 assertions executed against the real committed step bodies, with three deliberately broken copies of the workflow run through the same harness to confirm the tests are not vacuous. Total: 1 workflow file plus a new test harness, +1541/-25 across 5 commits, closing #316.

---

## 1. Problem Statement

### 1.1 Background

`update_homebrew_formula.yml` runs after every GitHub Release, downloads that release's artifacts, and rewrites `Formula/all-smi.rb` in the external `lablup/homebrew-tap` repository with new checksums and URLs. PR #313 (issue #308) taught it to also serve the Intel Mac artifact and, in the course of a security review, fixed a shell-injection defect and a path-traversal defect. That same review surfaced seven further findings about the job's privilege posture that were deliberately left for a separate PR, because they concerned how broadly the job's credentials could reach rather than the Intel-artifact feature itself. Issue #316 is that follow-up.

### 1.2 Existing Issues

- **Issue 1 (token persisted on disk for the whole job)**: the tap was cloned with the token embedded in the URL, `https://x-access-token:<token>@github.com/lablup/homebrew-tap.git`. Git records that URL verbatim as `origin` in `homebrew-tap/.git/config`, so from the clone step onward the token sat in the workspace, readable by every later step and by anything those steps ran, third-party actions included. Any code execution anywhere in the job, not just in the intended steps, was a tap compromise.
- **Issue 2 (`permissions: contents: write` broader than needed)**: the job's own reads go through `gh api`, which authenticates separately. `GITHUB_TOKEN`'s `contents: write` scope served no step in this workflow.
- **Issue 3 (no concurrency control)**: two releases published close together both clone the tap at the same commit and rewrite the same file from different tags; the second push is a non-fast-forward and fails having done nothing, silently leaving whichever tag pushed first, not necessarily the newer one.
- **Issue 4 (asset list read unpaginated)**: the release's embedded `assets` array carries no pagination controls and no documented completeness guarantee, unlike the dedicated `/releases/{id}/assets` collection.
- **Issue 5 (artifacts downloaded into the tap working tree)**: relying on the commit step naming `Formula/all-smi.rb` explicitly to keep four downloaded release archives out of the commit is one `git add -A` away from publishing binaries into a Homebrew tap.
- **Issue 6 (checksums and URLs validated independently)**: checking "every expected checksum is present" and "every URL names the right release" separately accepts a formula where two artifacts have exchanged checksums, since every value individually still checks out.
- **Issue 7 (`packaging` environment gates nothing)**: `protection_rules: []` and `deployment_branch_policy: null` mean naming the environment scopes the secret to the job but restricts nothing about who or what can trigger it.

### 1.3 Risk Assessment

| Risk | Impact | Likelihood |
|---|---|---|
| Any code execution in the job (a compromised or misbehaving third-party action) reads the tap push token off disk | Critical: full write access to the tap serving every `brew install all-smi` | Closed by this PR; was open on `main` since the tap integration existed |
| Two releases publish within the concurrency window and race on the tap | Medium: the losing run silently leaves a stale formula with no error visible on a green release | Closed by a `concurrency:` group |
| A url/checksum pair is swapped by a future rewrite defect | High if it happened: `brew install` fetches one artifact and verifies it against another's checksum | Closed by pairing url and sha256 together at validation time |
| The `packaging` environment continues to gate nothing | Medium: `HOMEBREW_TAP_TOKEN` remains reachable from `workflow_dispatch` on any branch | Deliberately left open; recorded as a maintainer decision, not fixed here |
| A dry run against a real tag has never executed this code | Medium: the push mechanics are proven against a local git server, not against GitHub's actual acceptance of the token | Open, documented in section 8 |

---

## 2. Technical Review

### 2.1 Security: how the token is scoped now, and why each leak path is closed

Four distinct leak paths exist for a token used to authenticate a `git push`, and this PR closes all four:

**Not disk.** The clone is now anonymous (`git clone https://github.com/lablup/homebrew-tap.git`, no embedded credential), which is possible because `lablup/homebrew-tap` is public and has to be for `brew tap lablup/tap` to work without any credential in the first place. The token is introduced only at the push step, and nothing in that step's command line or environment writes it to a remote URL or a config file.

**Not argv.** The natural alternative, `-c http.extraheader="Authorization: Basic $(printf ... | base64)"`, would place the encoded token directly on the command line, which is readable through `ps` for as long as the process runs and would appear in full if any later step enabled `set -x`. The chosen form avoids this because the value never appears as a literal in any command line; it is read from an environment variable inside a shell function.

**Not the `set -x` trace.** This is the subtle one, and the reason for the specific quoting:

```bash
# shellcheck disable=SC2016
git \
  -c credential.helper= \
  -c 'credential.helper=!f() { if [ "$1" = get ]; then printf "username=x-access-token\npassword=%s\n" "$HOMEBREW_TAP_TOKEN"; fi; }; f' \
  push origin main
```

The helper string is single-quoted, so the shell invoking `git push` never expands `$HOMEBREW_TAP_TOKEN`; git receives the literal text `$HOMEBREW_TAP_TOKEN` as the helper program to run. If this line were traced with `set -x`, the trace would show that same unexpanded name, not the token. The variable is only resolved inside the throwaway shell git itself spawns to execute the helper (`sh -c 'f() { ... }; f'`), reading it from that shell's inherited environment; xtrace does not propagate across that process boundary because bash does not export `SHELLOPTS` to a child invoked this way. The suppressed `SC2016` lint is the marker of intent here: the single quotes are the point, not an oversight.

**Not the keychain.** `-c credential.helper=` (empty) is applied first, which resets the helper list rather than appending to it, dropping whatever the runner image configures globally (`osxkeychain` on the `macos-latest` runner this job uses). Without that reset, a successful authentication is handed to the previously configured helper for storage after the fact, which is disk by a different route than the URL. The custom helper responds only to a `get` request; git's post-authentication `store` call still fires, but the helper's `if` makes it a no-op that exits 0, so git does not mistake the silence for a failed helper and does not retry or error.

### 2.2 What issue #316's "28 assets" premise got wrong

Issue #316 justified the pagination fix with "recent releases already carry 28 assets, so this is close to mattering." Investigation found the real count is 18, not 28, and truncation of the embedded `assets` array could not be reproduced at all: `electron/electron` (76 assets) and `denoland/deno` (56 assets) both return their full asset lists inline through the same endpoint this workflow reads, with no sign of a cap at any count this project is likely to reach soon.

The fix is retained anyway, but the justification changes from an observed defect to a documented-contract argument: the embedded `assets` array carries no `Link` header and GitHub does not document a completeness guarantee for it, whereas `/releases/{id}/assets` is a documented, explicitly paginated collection. This workflow reads the absence of the Intel asset from that list as an active decision (skip the stanza update, or refuse a version-skew formula in the next step), so building that decision on an undocumented property is the part worth removing, independent of whether the undocumented behavior has ever actually truncated a list. This is a case worth recording precisely because the fix is correct for a different reason than the one that motivated it.

### 2.3 Compatibility and Dependencies

- **Breaking changes**: none to the tap formula's shape. The four asset/stanza states PR #313 defined and verified (asset+stanza present, both absent, asset present only, stanza present only) behave identically; this PR re-verifies all four by executing the committed step bodies rather than assuming the refactor preserved them.
- **New dependencies**: none in the workflow itself. The test harness adds Python (`check-workflow-shape.py`, `extract-step.py`, `git-http-server.py`) and a `Makefile` target under `tests/`, none of which affects `cargo build` or `cargo test` (no `.rs` files are added).
- **Compatibility**: `actions/checkout` for this repository now runs with `persist-credentials: false`, closing the same class of problem (a token written to `.git/config`) for `GITHUB_TOKEN` even though no step in this checkout performs a git operation that would need it.

### 2.4 Code Quality

`tests/homebrew-formula-workflow/` is committed this time; PR #313's equivalent harness existed during that PR's development but was never checked into the repository, so it could not be rerun against future changes. The harness reads step bodies directly out of the committed YAML by step name (`extract-step.py`), so what is under test is the file that actually ships, not a hand-copied approximation of it that could silently drift.

69 assertions across three case files (`cases-token.sh`, `cases-download.sh`, `cases-formula.sh`) plus a workflow-shape checker (`check-workflow-shape.py`) and a real local git server (`git-http-server.py`) for exercising the push mechanically rather than only its string construction. Three negative controls were run to confirm the suite is not vacuous: reverting the pagination change fails 8 assertions, restoring the token-in-clone-URL pattern fails the shape check, and swapping the credential scoping back to an `http.extraheader` variant fails 4 assertions, including the trace-leak check specifically (that variant authenticates correctly but leaks the raw token into an `set -x` trace, which is exactly the defect this PR closes).

`actionlint` on the workflow is clean; `main` previously reported one `SC2086` info-level finding (`>> $GITHUB_ENV` unquoted), fixed in passing. The one remaining `SC2016` (the single-quoted credential helper) is suppressed inline with the reason recorded, since the unexpanded variable is the mechanism being relied on, not an accident.

---

## 3. Technical Decisions

### 3.1 Anonymous clone plus a `credential.helper` scoped to one command, over an `http.extraheader`

**Context**: the token needs to reach exactly one git operation (the final push) and nowhere else in the job.

| Option | Pros | Cons |
|---|---|---|
| Keep the token in the clone URL, rely on the working tree being ephemeral | No code change | This is exactly the defect being fixed: the token is readable by every step and everything those steps invoke for the whole job |
| `-c http.extraheader="Authorization: Basic $(...)"` at push time | Common pattern, scoped to one command in principle | The encoded token appears as a literal in the command's argv, visible via `ps` and via any `set -x` trace anywhere in the step |
| **Chosen: anonymous clone; push-time `credential.helper` reading the token from an environment variable inside a shell function** | Token never appears as a literal anywhere: not in the clone URL, not in argv, not in a trace. Reset of the helper list also prevents keychain persistence | Requires the specific single-quoting discipline documented in section 2.1; a careless edit (double-quoting the helper string) would silently reintroduce the argv/trace leak |

**Rationale**: the anonymous clone is possible only because the tap is public, which it must be for ordinary `brew tap` usage anyway, so there is no loss of capability in dropping the clone credential entirely. Scoping the remaining credential to the environment of one command, rather than to that command's arguments, is what defeats both `ps` visibility and `set -x` tracing at once, since both attack the command line, not the process environment.

### 3.2 Validate url and checksum as pairs, not as independent sets

**Context**: the previous validation checked "every expected sha256 value appears somewhere in the file" and, separately, "every expected URL appears somewhere in the file." A formula where two artifacts have swapped checksums passes both checks individually.

**Chosen fix**: pairs are extracted from the formula the same way `set_artifact` writes them, a `url` stanza followed by the next `sha256` stanza beneath it, and each expected `(url, sha256)` pair is checked as a unit against that extracted list:

```bash
awk '
  /^[[:space:]]*url "/ { u = $0; sub(...); pending = 1; next }
  pending && /^[[:space:]]*sha256 "/ { s = $0; sub(...); printf "%s\t%s\n", u, s; pending = 0 }
' Formula/all-smi.rb > "$pairs"
```

**Rationale**: swapped checksums are not a hypothetical failure mode; they are precisely what a rewrite that matched one stanza and wrote into another would produce, which is the exact class of bug the whole update step exists to prevent. A test suite control confirms the pre-#318 independent checks accept a fixture with deliberately swapped checksums, which is what motivates treating the pairing as the unit of validation rather than as two separate memberships.

### 3.3 Record the `packaging` environment decision instead of configuring it

**Context**: naming a GitHub Environment on a job scopes that job's access to the environment's secrets, but only gates access when the environment itself has protection rules (required reviewers, a deployment branch policy). `packaging` has neither.

| Option | Pros | Cons |
|---|---|---|
| Add required reviewers or a branch policy in this PR | Closes the gap completely | Repository administration change that decides who can ship a release; not something to fold quietly into a workflow-hardening PR |
| **Chosen: record the decision not to configure it, in a comment on the job plus this PR's own description** | Leaves the decision, and the reasoning, with a maintainer where it belongs; satisfies the issue's stated acceptance criterion, which explicitly allows "configure it, or record an explicit decision not to" | The gap remains open until a maintainer acts |

**Rationale**: who is authorized to trigger a workflow that can push to a production Homebrew tap is a policy question, not an implementation one. Recording the reasoning next to `environment: packaging` in the workflow itself, rather than only in a PR description that will eventually scroll out of view, keeps the open decision visible to the next person who reads the file.

---

## 4. Implementation Details

### 4.1 Architecture Changes

```
[Before]
git clone https://x-access-token:<token>@github.com/...   # token in .git/config for the whole job
  -> downloads land in homebrew-tap/tmp/                   # inside the git working tree
  -> validation checks url set and sha256 set independently
  -> git push origin main                                  # token already present in the remote url

[After]
git clone https://github.com/lablup/homebrew-tap.git       # anonymous; public repo needs no credential
  -> downloads land in $RUNNER_TEMP/all-smi-artifacts/      # outside the tap clone entirely
  -> asset presence read via paginated /releases/{id}/assets
  -> validation checks (url, sha256) as pairs read the way set_artifact writes them
  -> git -c credential.helper= -c 'credential.helper=!f(){ ... }; f' push origin main
       (token exists only in this command's environment, for this command's duration)
```

### 4.2 Key Code Changes

**File: `.github/workflows/update_homebrew_formula.yml` (job-level hardening)**
```yaml
permissions:
  contents: read

concurrency:
  group: update-homebrew-formula
  cancel-in-progress: false
```
**Reason for change**: `contents: read` is the actual requirement (the job's own reads go through `gh api`, authenticated separately; the tap push uses `HOMEBREW_TAP_TOKEN`, not `GITHUB_TOKEN`). The `concurrency` group is a constant naming the tap, the actually-contended resource, rather than a ref or tag, so two releases publishing close together serialize instead of racing on the same file.

**File: `.github/workflows/update_homebrew_formula.yml` (paginated asset listing)**
```bash
RELEASE_ID=$(gh api "repos/${GITHUB_REPOSITORY}/releases/tags/v${VERSION}" --jq '.id')
ASSETS=$(gh api --paginate "repos/${GITHUB_REPOSITORY}/releases/${RELEASE_ID}/assets?per_page=100" --jq '.[].name')
```
**Reason for change**: reads asset names from the documented, explicitly paginated collection rather than from the release object's embedded `assets` array, which carries no pagination controls or completeness guarantee (section 2.2).

**File: `.github/workflows/update_homebrew_formula.yml` (artifacts outside the tap clone)**
```bash
ARTIFACT_DIR="${RUNNER_TEMP}/all-smi-artifacts"
mkdir -p "$ARTIFACT_DIR"
```
**Reason for change**: previously artifacts landed in `homebrew-tap/tmp/`, inside the same git working tree the next two steps rewrite and commit. Only the commit step naming `Formula/all-smi.rb` explicitly kept them out of the commit; moving the download target outside the clone removes that dependency entirely.

### 4.3 Data Model Changes

Not a wire-format or metrics change; this PR is entirely CI workflow logic plus a new, previously-uncommitted test harness. No change to the tap formula's DSL shape or to any Prometheus metric.

---

## 5. Learning Points

### 5.1 A credential embedded in a clone URL persists for the life of the working tree, not just the clone command

**Concept**: `git clone https://user:token@host/repo.git` is convenient exactly because git remembers it, recording the URL verbatim as the `origin` remote in `.git/config`. That persistence is the feature for a long-lived clone and the vulnerability for a CI job whose later steps, and anything those steps invoke, can all read that file.

**Application in this PR**: this is precisely why an anonymous clone plus a push-time-only credential closes the gap that a scoped clone credential cannot: the credential simply never touches anything that outlives the one command needing it.

### 5.2 `set -x` traces the command as written, not as the shell would expand double quotes

**Concept**: bash's xtrace prints each command after word-splitting and expansion but before the child process (or, for a spawned shell, before that shell's own expansion). A single-quoted string that itself contains an unresolved `$VAR` reference is exactly what appears in the trace: the literal text, because nothing in the outer shell ever expanded it.

**Application in this PR**: the credential helper string is single-quoted specifically so that `$HOMEBREW_TAP_TOKEN` is resolved only inside the throwaway shell git spawns to run the helper, never by the shell executing the `git push` line itself. A trace of that line shows the unexpanded variable name; the actual value never appears in any log.

### 5.3 An independently-correct pair of checks is not the same guarantee as a paired check

**Concept**: "every expected value A appears somewhere" and "every expected value B appears somewhere," checked separately, do not imply "every A is paired with its correct B." A formula that swaps two artifacts' checksums passes both independent checks while failing the actual invariant `brew install` depends on.

**Application in this PR**: extracting `(url, sha256)` as adjacent pairs and checking the pair as a unit is what closes this gap, and the PR's test suite specifically verifies the failure mode exists (a fixture with swapped checksums passes the old independent checks) before confirming the new pairing check rejects it.

---

## 6. Further Learning

### Key Terms

| Keyword | Description | Relevance |
|---|---|---|
| `credential.helper` | Git's pluggable mechanism for supplying (and optionally storing) authentication for a remote operation | The mechanism this PR uses to scope the tap token to exactly one command |
| `http.extraheader` | An alternative git config for supplying an `Authorization` header directly | Rejected here because the encoded token would appear as a command-line literal (section 3.1) |
| GitHub Actions `set -x` / xtrace | Bash's command-echo tracing, often enabled for CI debugging | The leak path a naively-quoted credential helper would fall into (section 2.1, 5.2) |
| `/releases/{id}/assets` vs. embedded `assets[]` | Two ways to read a release's asset list from the GitHub API, one paginated and documented, one embedded and not | Section 2.2's "right fix, different reason than assumed" finding |
| `concurrency:` group | GitHub Actions primitive serializing workflow runs sharing a group name | Prevents two releases from racing on the same tap push |
| GitHub Environment protection rules | Required reviewers / deployment branch policies attached to a named environment | What `packaging` still lacks; recorded as a maintainer decision (section 3.3) |

### Related Technologies and Frameworks

- Git credential helpers and the `-c` config-override mechanism for scoping a setting to a single invocation.
- GitHub Actions `concurrency:` groups and `permissions:` scoping.
- GitHub REST API pagination (`Link` header, `--paginate`) versus embedded, undocumented array fields.

### Related PRs and Issues

- Issue #316: the issue this PR closes.
- PR #313 (issue #308): added the Intel Mac artifact to this same workflow, fixed a shell-injection and a path-traversal defect, and deferred the seven findings this PR addresses.
- Issue #306 / PR #312: the Intel Mac release target whose artifact PR #313 first served from this workflow.

---

## 7. Change Summary

### Statistics

| Item | Value |
|---|---|
| Files changed | 14 |
| Lines added | +1541 |
| Lines removed | -25 |
| Commits | 5 |
| Test assertions | 69, across the committed `tests/homebrew-formula-workflow/` harness |

### Changes by Category

| Category | Summary |
|---|---|
| Security | Token scoped to one command via an anonymous clone plus a push-time `credential.helper`; `permissions` reduced to `contents: read`; `actions/checkout` for this repo runs with `persist-credentials: false` |
| Reliability | `concurrency:` group serializes overlapping release runs; artifacts downloaded to `$RUNNER_TEMP` instead of inside the tap clone |
| Correctness | Asset listing reads the paginated `/releases/{id}/assets` collection; url/sha256 validated as pairs instead of independent sets |
| Documentation | `packaging` environment's lack of protection rules recorded as a deliberate, unresolved decision, in-line in the workflow |
| Testing | `tests/homebrew-formula-workflow/` committed (69 assertions), with three negative controls proving it is not vacuous |

### Related Commits

| SHA | Type | Message |
|---|---|---|
| `6cbb743b` | chore(ci) | reduce the blast radius of the Homebrew tap token |
| `4731e914` | chore(ci) | add a make target for the Homebrew workflow tests |
| `1699220c` | test(ci) | keep the push-step sandbox out of the real git config |
| `bd46a17a` | docs(ci) | correct the push-step credential comment |
| `3cf1a602` | test(ci) | stop leaking the push-test git server |

Merged to `main` as `7c5b321a`. Closes #316.

---

## 8. Follow-up Actions

### Required

- **A dry run against a real tag.** Not attempted, deliberately: it would mean publishing a release or dispatching the workflow against the production tap `lablup/homebrew-tap`, which is not something to do as part of a hardening PR. The push mechanics are instead verified against a local, authenticating git smart-HTTP server (`git-http-server.py`), which proves the credential-scoping logic but not GitHub's acceptance of the real token against the real repository.

### Left Open for Maintainers

- **The `packaging` environment's lack of protection rules.** Recorded as a deliberate decision (section 3.3), not fixed here; a maintainer needs to decide whether to add required reviewers or a deployment branch policy.
- **Real GitHub API pagination under load.** The `--paginate` behavior is exercised against a stub in the test harness; live calls confirmed the endpoint returns the same 18 asset names as the embedded array for the current release, but no release exists yet with enough assets to force a genuine second page.
- **`concurrency` under an actual overlapping release.** The group is declared and its shape verified to parse; two genuinely concurrent release publications were not staged against the real workflow.

### Not a Defect, Documented Behavior

- Issue #316's "28 assets" premise does not hold (the real count is 18, and truncation could not be reproduced); the pagination fix is retained on the documented-contract argument in section 2.2, not on an observed truncation.

---

## Appendix

### A. Test Results

- `tests/homebrew-formula-workflow/run-workflow-steps.sh`: 69 assertions, all passing, entirely offline; nothing in it contacts GitHub or the real tap.
- Four asset/stanza states re-verified after the refactor: all four behave as PR #313 originally documented.
- Swapped-checksum fixture: rejected by the new pairing check; a control assertion confirms the pre-#318 independent checks would have accepted the same file.
- Traversal-shaped tag and stale-URL fixtures: both still refused, confirming PR #313's fixes survived the refactor.
- Real push mechanics: exercised against a local authenticating git smart-HTTP server. The push succeeds, the bare repository receives the expected commit, and the server records `Authorization: Basic base64(x-access-token:<token>)`. After the push, the token appears in no file anywhere in the workspace, not in `.git/config`, and the `set -x` trace shows `$HOMEBREW_TAP_TOKEN` unexpanded rather than its value.
- Negative controls (three): reverting pagination fails 8 assertions; restoring the token-in-clone-URL pattern fails the shape check; swapping the credential scoping for `http.extraheader` fails 4 assertions including the trace-leak check specifically.
- `actionlint`: clean. `shellcheck -x` on the harness scripts: clean. `python3 -m py_compile` on the Python: clean.
- `cargo metadata`: the new `tests/` subdirectory adds no cargo test target, so `cargo test` is unaffected.
- CI on this PR: Test Suite and license/CLA checks passed; Docker Build Check skipped (not workflow-relevant).

### B. References

- Issue #316: the issue this PR closes, and the source of the "28 assets" premise addressed in section 2.2.
- PR #313 (issue #308): the predecessor PR whose security review generated the seven findings this PR resolves.
- Git documentation: `credential.helper`, `http.extraheader`, and the credential-helper protocol (`get`/`store`/`erase`).
- GitHub REST API: release asset pagination (`/releases/{id}/assets`) versus the embedded `assets[]` field on the release object.
- GitHub Actions: `concurrency:` groups, `permissions:` scoping, environment protection rules.
