# Technical Report: PR #423 - Rebellions ATOM Max Card Power

**Date**: 2026-09-20
**Status**: Completed
**Languages**: Rust, Markdown
**Risk Level**: Medium

---

## Executive Summary

PR #423 stops the Rebellions reader from counting a physical card's power once per die. `rbln-stat` enumerates dies and repeats each card's `card_power` on all of them, so an 8-card ATOM Max node (32 dies) reported 1369.7 W against a real draw of 341.5 W. Dies are now grouped into cards by `sid`, one die per card carries the reading, and the rest publish the `GPU_METRIC_UNAVAILABLE` sentinel, so every consumer that sums power counts each card exactly once. A new `card_power_watts` detail key keeps the card figure visible on the rows that no longer show their own power.

---

## 1. Problem Statement

### 1.1 Background

Rebellions ATOM Max (`RBLN-CA25`) puts four dies on one physical card, and `rbln-stat --json` lists one entry per die. The `card_power` field on each entry is the board's power, repeated identically on all four dies of that board. The reader built one `GpuInfo` per die and set `power_consumption` from `card_power` on every one of them, which turned a per-board measurement into four per-device readings of the same value.

ATOM Plus (`RBLN-CA22`) is one die per card, so summing per-die power happens to be correct there. That is why the defect stayed hidden until an ATOM Max node was measured.

### 1.2 Existing Issues

- **4x overcount on every total**: measured on real captures, an 8-card ATOM Max node reported `total_power_watts` of 1369.7 W against a real NPU draw of 341.5 W, while the equivalent ATOM Plus node reported 143.3 W against 143.3 W.
- **A correct-looking average hid the defect**: `avg_power` divided the inflated total by 32 devices and produced 42.8 W, which is the true per-card power, so the dashboard read plausibly while the total was four times wrong.
- **Prometheus inherited the same error**: the per-device power series was emitted once per die, so `sum by (instance)` over an ATOM Max host inflated by the dies-per-card factor.
- **A missing `sid` failed the whole response**: `sid` was a required field in the deserializer, so one malformed device entry hid every NPU on the host rather than that one device.

### 1.3 Risk Assessment

| Risk | Impact | Likelihood |
|------|--------|------------|
| Capacity planning and power budgeting act on a 4x inflated cluster total | High | High |
| Fixing the total by dividing `card_power` per die puts a derived value in a field whose contract is "directly measured" | Medium | Medium |
| Leaving 24 of 32 rows with no power reading is read as a broken reader rather than as correct accounting | Medium | High |
| A die-position-based reporter choice moves between polls and breaks per-uuid series continuity | Medium | Medium |

---

## 2. Technical Decisions

### 2.1 One Reporting Die per Card, Not a Derived Share and Not Collapsed Rows

Issue #418 put three options to the maintainer and the PR implements option (b). Option (a), dividing `card_power` by the dies per card, was rejected because `power_consumption` is otherwise a directly measured field and a quarter-share is a computed one. Option (c), collapsing four die rows into one card row, was rejected because it changes the device count users see and would require summing per-die memory, which is already correct as one row per die (32 x 15.72 GiB matches the hardware).

Option (b) needs no new machinery: `total_power_watts` in `src/metrics/gpu_readings.rs` already skips absent readings through `filter_map`, so marking three of four dies absent makes the total correct with every remaining value still measured.

### 2.2 Group by `sid`, the Board Serial

`sid` is the only field in `rbln-stat --json` that yields one group per physical card on both variants. `group_id` collapses all eight ATOM Plus devices into a single group, `location` encodes a die position within a board rather than a card index, and `npu` repeats once per card on ATOM Max. A device whose `sid` is empty or missing has no siblings to match and is treated as a card of its own, and the field is now `#[serde(default)]` so its absence no longer fails the whole response.

### 2.3 The Reporting Die Is the Lowest Kernel Index, Not the First Listed

Within a card, `card_power_roles` picks the die with the lowest index parsed from the kernel device name (`rbln12` is 12). Names that do not parse sort after every name that does, and ties fall back to the order `rbln-stat` listed them in.

Position alone was rejected because `rbln-stat` does not list devices in index order: the ATOM Plus capture reads rbln0, rbln2, rbln3 and so on. The reporter must also not hop between dies from one poll to the next, because both the power series and the energy-counter key are keyed by device uuid, and both have to stay continuous across polls.

### 2.4 A Vendor-Neutral `card_power_watts` Detail Key, Deliberately Not Named `power`

The card figure is published on every die of a multi-die card as a detail entry, which becomes a label on the device identity series. The key is snake_case on purpose: every detail key passes through `sanitize_label_name`, and a snake_case key survives unchanged, so the string a local reader writes and the string the remote parser stores are identical. This follows the existing `power_limit_max` pattern.

The key must not be called `power` or `power_draw`. The generic NPU exporter turns those names into `all_smi_npu_power_watts` and `all_smi_npu_power_draw_watts` series on every device that carries them, which would reintroduce the original defect through a different series.

### 2.5 Single-Die Cards Do Not Carry the Key

Every ATOM Plus device is a card of its own, and its `power_consumption` already is the card value, so it gets no `card_power_watts` label. The alternative, labelling every device uniformly, would make every ATOM Plus identity series churn on every poll for users who never had the bug. The cost is accepted only on ATOM Max, where each die's identity series now changes each poll, in the same way Tenstorrent's already does with live current and clock readings in `detail`.

---

## 3. Implementation Details

### 3.1 Reader: Roles Decided Over the Whole Poll

`card_power_roles` runs once per response, because a die's role depends on its siblings. It returns a `CardPowerRole` per device with two fields: `reports_card_power`, true for exactly one die per card, and `shared_card_watts`, the reporter's parsed value published on every die of a multi-die card.

```rust
let power = if role.reports_card_power {
    parse_power_safe(&device.card_power)
} else {
    GPU_METRIC_UNAVAILABLE
};
```

The card value is validated before publication: `parse_power` followed by a finite, non-negative filter. An unparsable reporter value publishes no `card_power_watts` at all rather than a placeholder.

The first commit, `cd7ab44`, extracted `gpu_info_from_response` so the conversion step of a poll can be driven by tests without executing `rbln-stat`, including the static-cache path that only the second and later polls take.

### 3.2 Consumers of the Sum

`mean_power_watts` was added to `src/metrics/gpu_readings.rs` and divides by the rows that reported, not by every row. `MetricsAggregator::aggregate_gpu_metrics` now calls it instead of open-coding the same filter, and `draw_system_view` uses it for the dashboard's Avg. Power, rendering `N/A` when nothing reported rather than `0.0W`. Apple Silicon keeps its previous meaning there, the combined CPU+GPU+ANE figure of its single GPU row.

`inject_gpu_power` in the local collector was summing `power_consumption` raw, which would subtract one watt per silent die because the sentinel is `-1.0`. It now calls `total_power_watts`, which filters absences.

### 3.3 Display and Remote Round Trip

A die with no reading of its own renders its card's value in parentheses, for example `Pwr:   (43W)`, so every die of a card shows its board's draw while the parentheses mark the value as neither this row's own reading nor part of any total. An invalid card value still renders `N/A`.

The remote metrics parser was extended to keep `card_power_watts` among the identity labels it stores in `detail`, so a remote viewer sees the same parenthesised value a local one does.

### 3.4 The Exporter Was Already Correct

Issue #418 stated that `all_smi_npu_power_watts` double-counts. The PR corrects this: Rebellions never emits that series. The per-device series is `all_smi_gpu_power_consumption_watts`, which already omits rows whose reading is absent, so the exporter needed no change at all. Snapshot JSON and CSV and record/replay serialize non-reporting dies as `power_consumption: -1.0`, the existing sentinel contract, and snapshot Prometheus omits them.

---

## 4. Learning Points

### 4.1 An Absence Sentinel Is a Contract, Not a Magic Number

The fix works because `GPU_METRIC_UNAVAILABLE` already meant "not measured" everywhere that matters, and the aggregation helpers already honoured it. The only places that needed changing were the two that bypassed the helpers: the raw `.map(...).sum()` in `inject_gpu_power` and the open-coded reporting count in the aggregator. When a project has an absence encoding, the audit that matters is for code that sums the field directly instead of going through the helper.

### 4.2 A Label Name Can Recreate the Bug It Documents

Naming the new detail key `power` would have been the obvious choice and would have fed the generic NPU exporter, producing a per-device power series on all four dies again. The label namespace and the metric namespace are joined by the exporter's name-based rules, so a detail key is an interface decision, not a display string.

### 4.3 A Correct-Looking Average Can Conceal a Wrong Total

The average read correctly at 42.8 W only because the overcount factor and the row count were the same number. Two derived figures from one wrong total can disagree about whether anything is wrong, and the one that looks right is not evidence. This is also why the mean had to move to a per-reporting-row denominator in the same change: fixing only the total would have made the average wrong by the same factor in the other direction.

---

## 5. Change Summary

### Statistics

| Item | Value |
|------|-------|
| Files changed | 13 |
| Lines added | +1777 |
| Lines deleted | -25 |
| Tests added | 20 |
| Fixtures added | 1 (`tests/fixtures/rbln-stat-atom-max-synthetic.json`, 838 lines) |

### Changes by Category

| Category | Count | Summary |
|----------|-------|---------|
| Correctness | 4 | Per-card power role in the reader, reporting-row mean, sentinel-safe chassis sum, `sid` made optional |
| Display | 2 | Parenthesised card value in the GPU renderer, `N/A` instead of `0.0W` for Avg. Power |
| Interface | 2 | `card_power_watts` detail key with a validating accessor, remote parser passthrough |
| Documentation | 2 | `API.md` power section for ATOM Max, man page line |
| Tests | 1 | ATOM Max regression module plus per-consumer tests |

### Related Commits

| Hash | Type | Message |
|------|------|---------|
| `cd7ab44` | refactor | Extract Rebellions rbln-stat response conversion |
| `9e68dbd` | fix | Count Rebellions ATOM Max card power once per card |
| `610da0b` | squash merge | Merge PR #423 into `main` |

---

## 6. Validation and Follow-up

### Completed Validation

- The regression test fails on the pre-fix reader with `total NPU power 1366 W, expected 341.5 W`, and passes after it.
- Reader tests: 25 passed, 10 new, covering the regression, reporter stability under reordering, ATOM Plus left unchanged, per-die memory, missing and empty `sid`, a `/metrics` render whose 8 power series sum to 341.5 W with no sentinel and no `all_smi_npu_power_watts`, and a remote round trip at 341.5 W.
- Filtered runs on the committed tree: `cargo test --lib` over the reader and every touched module, 154 passed; `cargo test --bin all-smi` likewise plus the local collector test, 93 passed; `cargo test --test user_facing_text_test`, 5 passed.
- Unfiltered runs on the final tree: `cargo test --bin all-smi` 1849 passed; `cargo test --lib` 1667 passed with 2 failures, both `storage::disk_cache` timing tests in untouched code that pass when rerun alone and in the bin target.
- `cargo clippy --lib --tests -- -D warnings` and `cargo fmt --check` clean; the pre-commit hook's `cargo clippy --all-targets --all-features -- -D warnings` passed on both commits.
- CI on `9e68dbd`: Build Check, Test Suite, launchd and systemd smoke tests, Packaging Sync Check, and CLA all passed.

### Required Follow-up

- Replace `tests/fixtures/rbln-stat-atom-max-synthetic.json` with a real ATOM Max capture when one is available. The fixture is synthetic: 8 `sid`s of 4 dies with card powers summing to 341.5 W, written in the exact shape of the ATOM Plus capture. Issue #418 mentions a real capture; it was not the file that landed here.
- Confirm the change on ATOM Max and ATOM Plus hardware. Neither was available, so the whole behaviour is pinned by fixtures only.

### Remaining Constraints

- `card_power_watts` is an interpretation the maintainer can flip: single-die cards, which means all of ATOM Plus, deliberately do not carry it.
- On ATOM Max, each die's `all_smi_gpu_info` series now changes every poll because the label carries a live value.
- The Users tab clamps an absent power reading to 0 W when attributing power to a process, so a process running on a non-reporting die is attributed no card power.
- Left untouched on purpose: the `0.0` fallback inside `parse_power_safe`, and the ambiguous context-to-device join on ATOM Max.

### References

- [PR #423](https://github.com/lablup/all-smi/pull/423)
- [Issue #418](https://github.com/lablup/all-smi/issues/418)
- `API.md`, Rebellions NPU metrics, power on ATOM Max
