# Technical Report: PR #334 - fix(tui): survive a terminal that reports no size (#326)

**Date**: 2026-08-06
**Status**: Completed
**Languages**: Rust
**Risk Level**: Low (arithmetic hardening plus one new policy module; verified end to end under a real degenerate pty, not only unit tests)

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

`all-smi local` aborted the moment it was handed a pty with no window size: `TIOCGWINSZ` reports zero, `crossterm::terminal::size` faithfully returns `Ok((0, 0))`, and `rows - 1` in `print_function_keys` underflowed in a debug build. The issue named two sites (`chrome.rs:113`, `chrome.rs:80`); an `ast-grep` sweep of `src/ui` and `src/view` for dimension arithmetic found three more reachable faults the issue did not name: `chrome.rs:43` underflows below 10 columns, `chrome.rs:49` divides by zero at exactly 10 columns, and `event_handler.rs:1220` underflows on the mouse-click path. All five are fixed with saturating or guarded arithmetic.

The substance of the PR is a design decision the arithmetic fix alone does not answer: what does a reported size of zero actually mean, and what should the TUI do at sizes that are real but too small to render? A pty with no `TIOCSWINSZ` has never been told its size, and there is almost always an ordinary terminal on the far end, so a new `ui::viewport` module treats zero, per dimension, as "no geometry available" and substitutes `$COLUMNS`/`$LINES` when the environment supplies a usable value and 80x24 otherwise, the same fallback order ncurses uses. A genuinely tiny terminal is treated as the opposite case: 12x2 is not missing geometry, it is an operator who dragged the window that small, and substituting a size there would be a lie, so below a measured floor of 20x3 the loop renders a one-line notice and skips composition, recovering automatically on the next `UiEvent::Resize`. The 20-column floor is not a taste judgment: it sits above every unchecked width subtraction in the renderer set, the binding one being the three-gauge GPU row at 14. Verified end to end under a `forkpty()` harness (a real terminal has no controlling-terminal problem, but the sandboxed agent shell does): the reported panic reproduces on the pre-fix binary at `chrome.rs:113:38`, and the post-fix binary renders a full 80x24 frame under the identical zero-size condition, while a real 12x2 pty takes the other branch and emits `12x2 < 20x3` in 82 bytes rather than a garbled frame. Total: 6 files, +745/-47, one commit, closing #326.

---

## 1. Problem Statement

### 1.1 Background

`script -q /dev/null ./all-smi local` (and some CI harnesses, and certain terminal-multiplexer edge cases) hands the process a pty that was never told its window size. `TIOCGWINSZ` on such a pty reports zero rows and zero columns, and `crossterm::terminal::size()` returns `Ok((0, 0))` rather than an error, so the zero propagates as an ordinary, successfully-read size into every renderer that consumes it.

### 1.2 Existing Issues

- **Issue 1 (the reported panic)**: `src/ui/chrome.rs:113`, `print_function_keys`, computed `cursor::MoveTo(0, rows - 1)` with `rows: u16`; at `rows == 0` this underflowed and aborted the process in a debug build.
- **Issue 2 (the issue's second named site)**: `src/ui/chrome.rs:80`, `print_loading_indicator`, computed `((rows - status_start_y) - 1).min(10)`, which underflows whenever `status_start_y` is at or past the last row.
- **Issue 3 (found by the sweep, not named in the issue)**: `src/ui/chrome.rs:43`, `40.min(cols as usize - SCREEN_MARGIN)`, underflows below 10 columns (`SCREEN_MARGIN`).
- **Issue 4 (found by the sweep)**: `src/ui/chrome.rs:49`, `position % (bar_width as u64 * 2)`, divides by zero exactly at `cols == SCREEN_MARGIN`, where `bar_width` computes to 0.
- **Issue 5 (found by the sweep)**: `src/view/event_handler.rs:1220`, `handle_process_header_click`, computed `half_rows - 1` on the mouse-click path, underflowing at 0 or 1 reported rows.
- **Issue 6 (no policy for a real, tiny terminal)**: even after the arithmetic is made safe, nothing in the codebase decided what should be drawn at an implausibly small but real size; saturating arithmetic alone would stop the panic while still drawing a garbled frame into a one- or two-row window.
- **Issue 7 (the TUI was hard to drive in an automated session)**: PR #317's own TUI verification had to call `print_cpu_info` directly on a live `CpuInfo` rather than drive a real session under `script`, specifically because of this panic, which made the renderer difficult to exercise end to end before this fix.

### 1.3 Risk Assessment

| Risk | Impact | Likelihood |
|---|---|---|
| Any environment handing the process a size-less pty crashes instead of degrading | High for that environment (total failure to start), zero for an ordinary interactive terminal | Certain in `script`, some CI harnesses, and certain multiplexer edge cases prior to this fix |
| Saturating the arithmetic without deciding a minimum renderable size | Medium: stops the panic but draws an unusable, potentially garbled frame into a 1-row or 1-column terminal | Avoided by pairing the arithmetic fix with the `ui::viewport` policy and its 20x3 floor |
| Treating a genuinely tiny terminal (12x2) the same as a missing-geometry pty (0x0) | Medium: substituting a size for a real, deliberately narrow window would render content the operator did not ask to see, or would hide the fact that the window is too small | Avoided by the split: zero is replaced, a real nonzero size below the floor is believed and shown a notice instead |

---

## 2. Technical Review

### 2.1 Correctness

The panic-safety fix and the size-policy decision are deliberately layered rather than merged into one check. `chrome.rs`'s guards (`if cols == 0 || rows == 0 { return; }` in `print_loading_indicator`, `if rows == 0 { return; }` in `print_function_keys`, plus saturating subtraction throughout) are a floor that makes the rendering functions themselves panic-safe regardless of caller. The policy question, what to do at a real-but-tiny size like 12x2, is explicitly delegated to `ui::viewport` in code comments at each guard site, so the two layers cannot silently drift: a caller that resolves geometry through `Viewport` never hands `chrome.rs` a zero in the first place (zero is only replaced, not eliminated as a possible input to `chrome.rs`, which is why the panic-safety floor still matters independently).

The sweep's coverage claim is falsifiable and was checked rather than assumed: `ast-grep` over `src/ui` and `src/view` for `$A - $B`, `$A / $B`, and `$A % $B` on dimension operands surfaced both the two issue-named sites and three more. The PR records every remaining match found by the sweep and the specific reason each is already safe (bounded by `.min()`, guarded by an early return, a loop bound that cannot go negative, or `#[allow(dead_code)]`), rather than silently widening every arithmetic expression in the renderer set. Eight gauge-renderer sites (`gpu_renderer.rs`, `cpu_renderer.rs`, `chassis_renderer.rs`, `storage_renderer.rs`, `help.rs:348`) are explicitly left with unchecked subtraction and reported rather than fixed, on the grounds that they are unreachable below the 20-column floor this PR establishes, verified by `frame_renderer.rs`'s new populated-snapshot sweep rather than by assertion alone.

### 2.2 Performance

`Viewport::resolve` and `Viewport::current` are called once per frame in `ui_loop.rs`'s render loop, replacing a direct `terminal::size()` call with no added cost beyond the per-dimension fallback logic, which only consults the environment (`std::env::var`) on the zero path, so the common case (a terminal that reports a real size) costs nothing extra. No new allocation, locking, or background work is introduced.

### 2.3 Compatibility and Dependencies

- **Breaking changes**: none in the CLI or metrics surface; this PR is confined to `src/ui/` and `src/view/`.
- **New dependencies**: none; `ui::viewport` uses `crossterm::terminal` (already a dependency) and `std::env`.
- **Compatibility**: `src/view/event_handler.rs` and `src/view/ui_loop.rs` retire four direct `terminal::size()`/`size()` calls (three `.unwrap()`s that would have aborted on a failed ioctl, one `Err(_) => return Err(...)`) in favor of `Viewport::current()`, which never propagates an ioctl failure as a hard error; it treats a failed read the same as a reported zero and falls back.

### 2.4 Code Quality

Regression coverage spans both axes deliberately, not just width: the reported panic was on rows, so a width-only sweep would have missed it. `chrome.rs`'s new test module drives `print_function_keys` and `print_loading_indicator` across eleven degenerate geometries, including the mixed `(0, 24)` and `(80, 0)` cases and the `cols == SCREEN_MARGIN` zero-width-bar case, using a shared `DEGENERATE_SIZES` table rather than one-off calls. `frame_renderer.rs`'s new tests sweep every size from 20x3 to 32x9 through `render_main`, `render_loading`, `render_help`, and `render_alert_panel` against a snapshot carrying two GPUs (one deliberately named to select the three-gauge Apple Silicon layout, the narrowest-tolerant row in the renderer set) and a CPU, which is what actually exercises the gauge-layout branches the 20-column floor claims to clear, rather than only the empty-snapshot smoke tests used elsewhere in the suite. `ui::viewport`'s own eleven tests cover per-dimension substitution, environment-variable parsing (including malformed and out-of-range values), the renderable floor's inclusive boundary, and the too-small notice's width-fitting degradation (long sentence, then short form, then character-level truncation).

The PR body separately records that `cargo clippy --bin all-smi --tests -- -D warnings` was run apart from `cargo clippy --lib --tests -- -D warnings` specifically because this crate compiles its module tree twice, and that running it separately caught a `pub` item that was live in the library target and dead in the binary target, the same class of defect PR #319's report documented for a different symbol. The merged diff does not show what was fixed, since the fix predates the PR's single commit; this report could not confirm the specific symbol from the diff and flags that for the reader (see the note in section 8).

---

## 3. Technical Decisions

### 3.1 Zero means "no geometry available," not "a terminal of size zero"

**Context**: a pty allocated without `TIOCSWINSZ` reports `(0, 0)`, and `crossterm::terminal::size` returns this as a successful read rather than an error, so nothing downstream can distinguish "no geometry was ever set" from "a terminal that truly has zero rows or columns" by inspecting the value alone.

| Option | Pros | Cons |
|---|---|---|
| Render nothing at size (0, 0) | Simplest, matches the literal value | Keeps the TUI undrivable under `script` and similar harnesses, which is precisely what made PR #317's own TUI verification unable to use a real session |
| **Chosen: substitute `$COLUMNS`/`$LINES` when usable, else 80x24, per dimension** | Matches ncurses' own fallback order; makes the TUI drivable under `script`; a size-less pty almost always has an ordinary terminal on the far end, so refusing to draw would misread the signal | A dimension that is genuinely tiny but happens to be reported as exactly 0 (not possible on real hardware, but conceivable from a buggy or adversarial pty) would be treated as missing rather than believed |
| Exit or block until a resize event supplies real geometry | Avoids guessing a size entirely | No TUI quits or freezes because the window is small; this would be worse UX than rendering at a reasonable guess |

**Rationale**: the fallback is per-dimension, not all-or-nothing: a terminal that reports a real width but no height keeps the width it reported and only the height falls back. This is what makes the substitution a narrow patch for "missing" rather than a broad override of "small."

### 3.2 A real, tiny terminal is believed, not replaced

**Context**: once zero is treated as "missing," the opposite case, a terminal that genuinely reports a small nonzero size such as 12x2 because an operator dragged the window that small, needs its own policy. Substituting a fallback size there would render content the operator never asked to see at that size.

| Option | Pros | Cons |
|---|---|---|
| Substitute a fallback size below some threshold too | One code path handles both "missing" and "small" | Renders a lie: the operator's window really is 12x2, and drawing an 80-column frame into it would be garbled or simply wrong |
| **Chosen: below `MIN_COLS`x`MIN_ROWS`, render a one-line notice and skip composition; recover automatically on `UiEvent::Resize`** | Honest about what the terminal actually is; the loop already handles resize events, so recovery is free | Requires a floor to be chosen and defended (section 3.3) |
| Exit the process below the floor | Simple | No TUI quits when dragged narrow; rejected explicitly in the PR for this reason |
| Block (stop rendering, wait) below the floor | Similar cost to the notice without informing the operator why nothing is happening | Rejected as strictly worse than showing the one-line notice |

**Rationale**: the notice both explains the situation to the operator (`too_small_notice()` states the requirement and the actual size, degrading to a shorter form and finally to character-level truncation if even that will not fit) and requires no new event-handling machinery, since `UiEvent::Resize` already wakes the render loop.

### 3.3 The minimum size is measured against the renderer set, not chosen by taste

**Context**: a floor below which the TUI refuses to compose a frame needs a concrete justification, or it is an arbitrary number that could be silently invalidated by a future renderer change.

**Finding**: `MIN_COLS = 20` sits above every unchecked width subtraction that remained after the sweep (section 2.1). The binding constraint is the three-gauge GPU row in `gpu_renderer.rs`, which computes `available_width - (num_gauges - 1) * 2` over `width.saturating_sub(10)` and therefore needs `width >= 14`; the Apple Silicon CPU row needs 12; the chassis, storage, and single-gauge CPU rows need 5. Twenty also happens to be the point where the shortest status-bar text (`h:Help q:Exit`, 13 columns) stops being the entire visible line. `MIN_ROWS = 3` is one header row, one content row, and the status bar that always occupies the last row; two rows would be pure chrome with nothing between it.

**Verification, not assertion**: `frame_renderer.rs`'s new test sweeps 20x3 through 32x9 against a populated snapshot (real GPU and CPU rows, not an empty one) through every render path (`render_main`, `render_loading`, `render_help`, `render_alert_panel`), which is what actually proves the floor clears the gauge cliffs rather than merely asserting a number in a comment.

---

## 4. Implementation Details

### 4.1 Architecture Changes

```
[Before]
terminal::size() -> (cols, rows), possibly (0, 0)
    │
    ▼
chrome.rs / event_handler.rs: raw arithmetic on cols/rows
    │
    ▼
rows - 1 underflows at rows == 0 -> panic

[After]
terminal::size() -> (raw_cols, raw_rows)
    │
    ▼
Viewport::resolve(raw_cols, raw_rows)
    │  per dimension: raw > 0 ? raw : $COLUMNS/$LINES ? that : FALLBACK (80x24)
    ▼
Viewport { cols, rows }   -- never (0, N) or (N, 0) again
    │
    ├─ is_renderable()? (>= 20x3)
    │     no  -> render too_small_notice(), skip composition, wait for Resize
    │     yes -> compose a normal frame
    ▼
chrome.rs: still saturating/guarded internally, as an independent panic-safety floor
```

### 4.2 Key Code Changes

**File: `src/ui/viewport.rs` (new; the size-resolution policy)**
```rust
pub const MIN_COLS: u16 = 20;   // above every unchecked width subtraction
pub const MIN_ROWS: u16 = 3;    // header + one content row + status bar
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
**Reason for change**: this is the single place a raw terminal size becomes a size the TUI will actually render at; every other call site (`ui_loop.rs`, `event_handler.rs`) now goes through it instead of calling `crossterm::terminal::size()` directly.

**File: `src/ui/chrome.rs` (panic-safety floor, independent of the policy layer)**
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
**Reason for change**: `chrome.rs`'s functions are made safe against a zero input on their own terms, so they cannot panic even if a future caller bypasses `Viewport`.

**File: `src/view/ui_loop.rs` (the too-small branch and recovery)**
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
**Reason for change**: `previous_too_small` tracks the transition in both directions so the screen is force-cleared on the way into the notice (dropping stale cached frame state) and on the way back out (repainting a normal frame from scratch rather than composing onto the notice's leftover line).

### 4.3 Data Model Changes

Not applicable. No metric, config, or wire-format change; this PR is confined to terminal rendering.

---

## 5. Learning Points

### 5.1 A structural sweep finds what a targeted fix cannot

**Concept**: fixing exactly the sites a bug report names addresses the reported symptom but not the class of defect. A tool-assisted sweep for the same *shape* of expression (dimension subtraction, division, modulo) across the whole affected subsystem is what turns "fix the panic" into "fix the class."

**Application in this PR**: the issue named two sites; the `ast-grep` sweep found three more reachable faults with the identical shape, plus a documented set of already-safe matches that were deliberately left alone rather than "fixed" defensively, keeping the diff focused on what was actually broken.

### 5.2 An identical raw value can mean two opposite things, and conflating them produces the wrong fix

**Concept**: `(0, 0)` from a size query can mean "the caller never told the kernel how big this window is" or, in principle, "the window really is zero-sized." Treating both the same way, either by always substituting or by always believing, is wrong for one of the two cases.

**Application in this PR**: the split (zero is replaced, a small-but-nonzero real size is believed) is the actual content of this PR beyond arithmetic safety. It is why 12x2 renders `12x2 < 20x3` rather than a full 80x24 frame, and why a `script`-driven pty renders a full frame rather than a one-line notice.

### 5.3 A minimum-size floor is a claim about the renderer set and needs to be checked against it, not asserted

**Concept**: a hardcoded minimum dimension is only meaningful if it is derived from (and periodically re-verified against) the actual arithmetic in the code it protects, since a future renderer change can silently invalidate an untested assumption.

**Application in this PR**: `MIN_COLS`/`MIN_ROWS` are justified in comments by specific renderer computations (the three-gauge GPU row's `width >= 14`) and verified by a populated-snapshot sweep across the floor's neighborhood, not merely documented as a constant.

---

## 6. Further Learning

### Key Terms

| Keyword | Description | Relevance |
|---|---|---|
| `TIOCGWINSZ` | The ioctl used to query a terminal's window size | Reports all-zero on a pty that was never told its size, which is the root condition this PR handles |
| `Viewport` | New struct (`ui::viewport`) resolving raw terminal geometry into a size the TUI will render at | The single entry point for terminal size after this PR; replaces four direct `terminal::size()` call sites |
| `MIN_COLS` / `MIN_ROWS` | The measured floor (20x3) below which a frame is not composed | Derived from the narrowest renderer in the set (the three-gauge GPU row) rather than chosen arbitrarily |
| Saturating arithmetic | `u16::saturating_sub` and friends, clamping instead of wrapping/panicking | The panic-safety layer, independent of and beneath the size-policy layer |
| `ast-grep` | Structural, syntax-aware code search tool | Used to sweep `src/ui` and `src/view` for the dimension-arithmetic shape that caused the reported panic |
| `forkpty()` | POSIX API to create a process attached to a new pty | Used for end-to-end verification in an environment (the agent's sandboxed shell) with no controlling terminal, reproducing the same zero-size condition as `script` |

### Related Technologies and Frameworks

- `crossterm::terminal::size`, and its behavior of returning `Ok((0, 0))` rather than an error on a size-less pty.
- ncurses' fallback order for terminal geometry (`$COLUMNS`/`$LINES`, then a conventional default), which this PR's `Viewport::resolve` deliberately mirrors.

### Related PRs and Issues

- Issue #326: the issue this PR closes.
- PR #317: its TUI verification section is the direct evidence that this panic made the renderer hard to exercise under a real session before this fix.
- PR #319: the prior report documenting the same class of "`pub` item live in the library target, dead in the binary target" `cargo clippy` finding this PR's body also references (see section 8 for the unverified specifics).
- PR #337: later touches `src/ui/renderers/gpu_renderer.rs`, one of the eight files this PR deliberately left with unchecked arithmetic (because it is unreachable below the 20-column floor); the PR body notes the diff there is confined to value readouts and gauges, not the dimension arithmetic this PR left alone.

---

## 7. Change Summary

### Statistics

| Item | Value |
|---|---|
| Files changed | 6 |
| Lines added | +745 |
| Lines removed | -47 |
| Commits | 1 |
| New files | `src/ui/viewport.rs` |

### Changes by Category

| Category | Summary |
|---|---|
| Correctness | Five sites hardened against dimension-arithmetic underflow/division-by-zero: two named by the issue, three found by the sweep |
| New module | `ui::viewport`: `Viewport::resolve`/`current`, `is_renderable`, `too_small_notice`, and the measured `MIN_COLS`/`MIN_ROWS`/`FALLBACK_*` constants |
| Behavior | Below 20x3 the TUI renders a one-line notice instead of a frame and recovers automatically on the next resize |
| Refactor | Four direct `terminal::size()`/`size()` call sites in `event_handler.rs` and `ui_loop.rs` replaced with `Viewport::current()`, retiring three `.unwrap()`s that could abort on a failed ioctl |
| Tests | 7 new `ui::chrome` tests, 11 new `ui::viewport` tests, 3 new `frame_renderer` tests sweeping a populated snapshot from 20x3 to 32x9 |

### Related Commits

| SHA | Type | Message |
|---|---|---|
| `2503a736` | fix(tui) | survive a terminal that reports no size |

Merged to `main` as `c4c17d8d`. Closes #326.

---

## 8. Follow-up Actions

### Required

None identified in the PR beyond the unverified item below.

### Monitoring Required

- Recovery from the too-small notice back to a normal frame relies on `UiEvent::Resize`, which the loop already handles; the PR states this transition was verified by reading the code and by the `force_clear` call on both edges of the transition, but was not driven end to end, since resizing a live pty mid-session was outside what the verification harness needed to do.
- The whole-suite `cargo test` was not run under the development time budget the PR describes; the scoped runs listed in the appendix cover every module this PR touches, and CI runs the rest.

### Future Improvements

- The eight gauge-renderer sites left with unchecked arithmetic (`gpu_renderer.rs`, `cpu_renderer.rs`, `chassis_renderer.rs`, `storage_renderer.rs`, `help.rs:348`) are reported rather than fixed, since hardening all eight would widen this diff across files PR #337 was concurrently touching, for no behavior change below the 20-column gate. Worth a follow-up if the floor is ever lowered.

**A claim this report could not substantiate from the diff**: the PR body states that running `cargo clippy --bin all-smi --tests -- -D warnings` separately from the library-target run caught "a `pub` item that was live in the library and dead in the binary." This class of finding matches a defect PR #319's report documented for a different symbol, but the merged diff for this PR contains no trace of what was fixed (no `pub fn new` or similar removed from `ui/viewport.rs`, whose only public constructors are `resolve` and `current`), because the fix, if any, predates this PR's single squashed commit. This report records the general clippy-catches-cross-target-dead-code claim as stated in the PR body but cannot confirm the specific symbol from the code itself; flagged for the reader rather than asserted as fact.

---

## Appendix

### A. Test Results

- `cargo test --lib ui::`: 559 passed, including 7 new `ui::chrome` tests and 11 new `ui::viewport` tests.
- `cargo test --bin all-smi view::`: 123 passed. `view` is only in the binary target, so `--lib` never compiles it; the 3 new `frame_renderer` tests live here.
- `cargo clippy --lib --tests -- -D warnings`: clean.
- `cargo clippy --bin all-smi --tests -- -D warnings`: clean, run separately on purpose (see the unverified-claim note in section 8).
- `cargo fmt --check`: clean.
- End-to-end, `forkpty()` harness leaving the window size unset (identical condition to `script -q /dev/null`): before the fix, on `1f540e1`, `[pty winsize: rows=0 cols=0]` followed by `thread 'main' (32556106) panicked at src/ui/chrome.rs:113:38: attempt to subtract with overflow`, exit status 25856 (`101 << 8`). After the fix, the same harness renders a full 80x24 frame reconstructed from the captured escape stream. A real 12x2 pty takes the other branch and emits 82 bytes: `12x2 < 20x3`. A 1x1 pty degrades to the single character that fits. `COLUMNS=100 LINES=12` over a zero-size pty renders a full 100x12 frame, confirming the environment-variable fallback path.

### B. Performance Benchmarks

Not separately benchmarked. `Viewport::current()` is called once per render-loop iteration and only touches the environment on the zero-input path; no new allocation or locking is introduced.

### C. References

- Issue #326: reproduction, evidence, and acceptance criteria this report draws from, cross-checked against the diff.
- ncurses' terminal-size fallback order (`$COLUMNS`/`$LINES`, then a conventional default), the model `Viewport::resolve` follows.
- PR #317's report, section on TUI verification: the direct prior evidence that this panic made the renderer hard to drive under `script`.
