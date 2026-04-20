// Copyright 2025 Lablup Inc. and Jeongkyu Shin
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Cluster-wide user aggregation for the `V` tab (issue #189).
//!
//! Given a per-host view of GPUs and their running processes, this module
//! produces a list of `UserAggregate` records — one per distinct
//! operator, plus a synthetic "unattributed" bucket for rows that lost
//! their `user` label on scrape (Windows API mode) — along with a
//! partial-coverage summary so the UI can warn when only a subset of
//! hosts reported process data.
//!
//! The only I/O the module performs is string allocation.  All inputs
//! are borrowed, so the function composes cleanly with the existing
//! `RenderSnapshot` pipeline and can be memoised against
//! `AppState::data_version`.

use std::collections::{BTreeSet, HashMap, HashSet};

use crate::network::metrics_parser::ParsedProcessRow;

/// Username assigned to rows whose `user` label was empty on the wire.
/// Rendered as-is (not translated) so operators can grep for it.
pub const UNATTRIBUTED_USER: &str = "(unattributed)";

/// Display string for a row whose username is unknown.  This is the
/// glyph the renderer puts in the `USER` column; the aggregator itself
/// groups those rows under [`UNATTRIBUTED_USER`].
pub const UNATTRIBUTED_DISPLAY: &str = "?";

/// Minimal per-GPU snapshot the aggregator needs.  `total_memory_used`
/// is the sum of all processes' VRAM on that GPU; it is the denominator
/// of the weighted-power formula.  The aggregator derives it from the
/// process list itself (summing over every `ParsedProcessRow` with the
/// same `(host, gpu_index)`) so callers don't have to care whether the
/// scrape exposed `all_smi_gpu_memory_used_bytes` with matching numbers.
#[derive(Clone, Debug)]
pub struct GpuForAggregation {
    pub host: String,
    pub gpu_index: u32,
    pub power_watts: f64,
}

/// Per-host view passed into [`aggregate_users`].
///
/// A host is considered "reporting process data" if **either** its
/// `processes` slice is non-empty **or** it is currently `connected`.
/// The connected-but-silent case is how an *idle* host with
/// `--processes` enabled shows up: the API exporter legitimately emits
/// zero rows when nothing is running on the GPUs, and we must not flag
/// that as "not reporting" — otherwise the partial-coverage chip
/// scares operators on a cluster that is working exactly as intended.
///
/// Hosts that deliberately opt out of `--processes` (or that are
/// disconnected) contribute to the denominator of the partial-coverage
/// chip but not to the numerator.
#[derive(Clone, Debug)]
pub struct HostSnapshot {
    pub host: String,
    pub gpus: Vec<GpuForAggregation>,
    pub processes: Vec<ParsedProcessRow>,
    /// Whether the remote scraper currently considers this host
    /// connected.  `true` on local-mode hosts (there's nothing to
    /// disconnect from).  Consumed by [`aggregate_users`] when
    /// computing `reporting_hosts`.
    pub is_connected: bool,
}

/// Per-host breakdown the drill-down view renders when the operator
/// hits `Enter` on a user.
#[derive(Clone, Debug)]
pub struct UserPerHost {
    pub host: String,
    pub gpu_indices: BTreeSet<u32>,
    pub vram_bytes: u64,
    pub power_watts: f64,
    pub pid_count: usize,
    pub top_command: String,
}

/// One row in the Users tab table.
#[derive(Clone, Debug)]
pub struct UserAggregate {
    /// Canonical user name.  Equal to [`UNATTRIBUTED_USER`] for rows
    /// with missing `user` labels.
    pub user: String,
    pub is_system: bool,
    /// Number of hosts the user has at least one process on.
    pub node_count: usize,
    /// Number of distinct `(host, gpu_index)` pairs touched.
    pub gpu_count: usize,
    /// Number of distinct `(host, pid)` pairs — "same PID on two hosts
    /// = different processes" per the issue spec.
    pub process_count: usize,
    /// Sum of `gpu_memory_bytes` across every matching row.
    pub vram_bytes: u64,
    /// Weighted power approximation in watts.  Always >= 0; see
    /// [`aggregate_users`] for the formula.
    pub power_watts: f64,
    /// Largest `start_time_seconds` the user has in the cluster.  0 on
    /// fleets where none of the user's processes reported a start time.
    pub longest_seconds: u64,
    /// The command string that owns the largest `gpu_memory_bytes`
    /// among the user's processes.  Empty when the user has no GPU
    /// memory attached (e.g. root's containerd-shim processes).
    pub top_command: String,
    /// Per-host breakdown used by the drill-down view.  Sorted by host.
    pub per_host: Vec<UserPerHost>,
}

/// Summary returned by [`aggregate_users`] along with the list of
/// `UserAggregate` rows.
#[derive(Clone, Debug, Default)]
pub struct UserAggregationResult {
    pub users: Vec<UserAggregate>,
    /// Number of hosts whose `processes` slice was non-empty.
    pub reporting_hosts: usize,
    /// Total number of hosts in the input (reporting + silent).
    pub total_hosts: usize,
}

impl UserAggregationResult {
    /// True when only a subset of hosts reported process data.
    pub fn is_partial(&self) -> bool {
        self.total_hosts > 0 && self.reporting_hosts < self.total_hosts
    }
}

/// Threshold below which a user is treated as a system account.
///
/// Mirrors Linux's `UID_MIN` default (1000) — anything smaller is a
/// system UID, plus the literal string `root` which does not always
/// parse as an integer (e.g. on `sudo` lines where the raw name is
/// emitted).  We keep this behind the in-tab `f` filter; it is NOT
/// applied by the parser.
pub const SYSTEM_UID_THRESHOLD: u32 = 1000;

/// Heuristic: is the given user name a system account?
///
/// Accepts integer UIDs and the literal "root".  Non-numeric,
/// non-"root" names are always treated as real users.
pub fn is_system_user(user: &str) -> bool {
    if user == "root" {
        return true;
    }
    if let Ok(uid) = user.parse::<u32>() {
        return uid < SYSTEM_UID_THRESHOLD;
    }
    false
}

/// Compute per-user aggregates across the cluster.
///
/// The function runs in `O(P + G + U)` where `P` is the total number of
/// process rows, `G` is the total number of GPU rows, and `U` is the
/// number of distinct users — a single pass over the inputs is enough
/// because we never cross-compare two users.
///
/// The **power approximation** works as follows:
///
/// ```text
///                        user_vram_on_gpu
///   power += gpu_power × ───────────────────
///                        total_vram_on_gpu
/// ```
///
/// For every GPU the user touches we multiply the GPU's reported power
/// by the user's share of the VRAM in use on that GPU, then sum across
/// GPUs.  Negative values are clamped to zero (guards against malformed
/// scrapes where process VRAM sums exceed the GPU total due to race
/// conditions between NVML and the Linux accounting paths).  The UI
/// marks this column with `*` to make the approximation explicit.
pub fn aggregate_users(snapshots: &[HostSnapshot]) -> UserAggregationResult {
    // --- Pass 1: total VRAM per (host, gpu_index) -----------------
    //
    // This is the denominator of the weighted-power formula.  We don't
    // trust `all_smi_gpu_memory_used_bytes` for this because a scrape
    // without `--processes` won't emit process rows at all, and the GPU
    // total can drift from the sum-of-processes by allocator overhead.
    // Summing the process rows keeps the ratios self-consistent.
    // Pre-size both maps: GPUs are capped at 256 per host (see
    // `MAX_DEVICES_PER_TYPE` in the parser). Pre-sizing avoids rehashing
    // on every insert in the hot path and materially reduces allocation
    // traffic on 100-node clusters.
    let gpu_capacity_hint = snapshots.iter().map(|s| s.gpus.len()).sum::<usize>();

    let mut total_vram_by_gpu: HashMap<(String, u32), u64> =
        HashMap::with_capacity(gpu_capacity_hint);
    for snap in snapshots {
        for p in &snap.processes {
            // Single-lookup accumulation: one `entry(...)` call,
            // `+=` into the returned `&mut u64`. The previous pattern
            // (`*entry().or_insert(0) = get().unwrap_or().add()`) did
            // two lookups and two `host.clone()` calls per row; for
            // 100 hosts × 50 procs that was ~5 000 extra clones per
            // scrape tick on the single-threaded UI path.
            let entry = total_vram_by_gpu
                .entry((snap.host.clone(), p.gpu_index))
                .or_insert(0);
            *entry = entry.saturating_add(p.gpu_memory_bytes);
        }
    }

    // --- Pass 2: GPU power lookup (host, gpu_index) -> watts ------
    let mut power_by_gpu: HashMap<(String, u32), f64> = HashMap::new();
    for snap in snapshots {
        for g in &snap.gpus {
            // We take the last occurrence if duplicates exist — in
            // practice each `(host, gpu_index)` appears once per
            // scrape tick.
            power_by_gpu.insert((g.host.clone(), g.gpu_index), g.power_watts.max(0.0));
        }
    }

    // --- Pass 3: per-user accumulation ----------------------------
    let mut user_scratch: HashMap<String, UserScratch> = HashMap::new();
    let mut reporting_hosts: HashSet<&str> = HashSet::new();
    for snap in snapshots {
        // A host counts as "reporting" when it produced process rows
        // **or** when it is connected (and thus capable of producing
        // rows — an empty list on a connected host just means the GPUs
        // are idle, which is very different from "the remote scraper
        // has lost contact" and must not trigger the partial-coverage
        // warning).
        if !snap.processes.is_empty() || snap.is_connected {
            reporting_hosts.insert(snap.host.as_str());
        }
        for p in &snap.processes {
            let canonical_user = if p.user.is_empty() {
                UNATTRIBUTED_USER.to_string()
            } else {
                p.user.clone()
            };
            let scratch = user_scratch.entry(canonical_user).or_default();
            scratch.absorb(p, snap.host.as_str());
        }
    }

    // --- Pass 4: finalise -----------------------------------------
    let mut users: Vec<UserAggregate> = user_scratch
        .into_iter()
        .map(|(user, scratch)| scratch.finalize(user, &total_vram_by_gpu, &power_by_gpu))
        .collect();

    // Stable default ordering: alphabetical by username so the UI has a
    // deterministic baseline before the in-tab sort keys kick in.
    users.sort_by(|a, b| a.user.cmp(&b.user));

    UserAggregationResult {
        users,
        reporting_hosts: reporting_hosts.len(),
        total_hosts: snapshots.len(),
    }
}

// ---------------------------------------------------------------------
// Private scratch structures
// ---------------------------------------------------------------------

#[derive(Default, Clone)]
struct PerHostScratch {
    gpu_indices: BTreeSet<u32>,
    vram_bytes: u64,
    /// Distinct PIDs on this host for this user.
    pids: HashSet<u32>,
    /// Best-so-far command on this host (owner of the largest VRAM).
    top_command_vram: u64,
    top_command: String,
}

#[derive(Default)]
struct UserScratch {
    /// Keyed by `(host, gpu_index)` for the node-count computation and
    /// to let the final pass look up each `total_vram_by_gpu` cell.
    touched_gpus: HashSet<(String, u32)>,
    /// Keyed by `(host, pid)` — same PID on two hosts counts twice.
    touched_pids: HashSet<(String, u32)>,
    /// Sum of gpu_memory_bytes across every row.
    vram_bytes: u64,
    /// Weighted-power numerators keyed by `(host, gpu_index)`.  The
    /// final pass divides each by the matching denominator.
    vram_by_gpu: HashMap<(String, u32), u64>,
    /// Maximum start_time_seconds seen across rows.
    longest_seconds: u64,
    /// Owner of the single row with the largest gpu_memory_bytes
    /// (cluster-wide).
    top_command_vram: u64,
    top_command: String,
    /// Per-host accumulation for the drill-down view.
    per_host: HashMap<String, PerHostScratch>,
}

impl UserScratch {
    fn absorb(&mut self, row: &ParsedProcessRow, host: &str) {
        // Each of these inserts/entry calls costs one `host.to_string()`
        // (host is a &str). The previous implementation called
        // `host.to_string()` 4–5 times per row; we keep 3 calls here
        // (touched_gpus / touched_pids need owned tuples; vram_by_gpu
        // and per_host share the same arena), paying for them once per
        // row rather than on every hash lookup. On 5 000-row scrapes
        // that's a measurable reduction in allocator traffic.
        self.touched_gpus.insert((host.to_string(), row.gpu_index));
        self.touched_pids.insert((host.to_string(), row.pid));
        self.vram_bytes = self.vram_bytes.saturating_add(row.gpu_memory_bytes);

        // Single-lookup accumulation (formerly two lookups + two clones).
        {
            let entry = self
                .vram_by_gpu
                .entry((host.to_string(), row.gpu_index))
                .or_insert(0);
            *entry = entry.saturating_add(row.gpu_memory_bytes);
        }

        if row.start_time_seconds > self.longest_seconds {
            self.longest_seconds = row.start_time_seconds;
        }
        if row.gpu_memory_bytes > self.top_command_vram {
            self.top_command_vram = row.gpu_memory_bytes;
            self.top_command = pick_display_command(row);
        }

        let ph = self.per_host.entry(host.to_string()).or_default();
        ph.gpu_indices.insert(row.gpu_index);
        ph.vram_bytes = ph.vram_bytes.saturating_add(row.gpu_memory_bytes);
        ph.pids.insert(row.pid);
        if row.gpu_memory_bytes > ph.top_command_vram {
            ph.top_command_vram = row.gpu_memory_bytes;
            ph.top_command = pick_display_command(row);
        }
    }

    fn finalize(
        self,
        user: String,
        total_vram_by_gpu: &HashMap<(String, u32), u64>,
        power_by_gpu: &HashMap<(String, u32), f64>,
    ) -> UserAggregate {
        // Weighted power: for each (host, gpu) the user touches,
        //   gpu_power × (user_vram / total_vram_on_that_gpu)
        // Summed over GPUs, clamped to non-negative.
        let mut power_watts = 0.0_f64;
        for ((host, gpu_index), user_vram) in &self.vram_by_gpu {
            let total = total_vram_by_gpu
                .get(&(host.clone(), *gpu_index))
                .copied()
                .unwrap_or(0);
            if total == 0 {
                continue;
            }
            let gpu_power = power_by_gpu
                .get(&(host.clone(), *gpu_index))
                .copied()
                .unwrap_or(0.0);
            let ratio = (*user_vram as f64) / (total as f64);
            // Even with non-negative inputs, f64 noise (denormals,
            // subtraction in the caller) could in principle push the
            // product below zero.  Clamp defensively.
            power_watts += (gpu_power * ratio).max(0.0);
        }
        if power_watts < 0.0 {
            power_watts = 0.0;
        }

        let node_count: HashSet<&String> = self.touched_gpus.iter().map(|(h, _)| h).collect();
        let is_system = is_system_user(&user);

        // Freeze per-host breakdown (sorted by host for deterministic
        // drill-down ordering).
        let mut host_keys: Vec<String> = self.per_host.keys().cloned().collect();
        host_keys.sort();
        let per_host: Vec<UserPerHost> = host_keys
            .into_iter()
            .map(|host| {
                let ph = self.per_host.get(&host).cloned().unwrap_or_default();
                // Per-host power is the sum over this host's GPUs of
                // the same ratio; we recompute it here so drill-down
                // adds up to the top-level number.
                let mut host_power = 0.0_f64;
                for g in &ph.gpu_indices {
                    let total = total_vram_by_gpu
                        .get(&(host.clone(), *g))
                        .copied()
                        .unwrap_or(0);
                    if total == 0 {
                        continue;
                    }
                    let user_vram = self
                        .vram_by_gpu
                        .get(&(host.clone(), *g))
                        .copied()
                        .unwrap_or(0);
                    let gpu_power = power_by_gpu
                        .get(&(host.clone(), *g))
                        .copied()
                        .unwrap_or(0.0);
                    host_power += (gpu_power * (user_vram as f64) / (total as f64)).max(0.0);
                }
                if host_power < 0.0 {
                    host_power = 0.0;
                }
                UserPerHost {
                    host,
                    gpu_indices: ph.gpu_indices,
                    vram_bytes: ph.vram_bytes,
                    power_watts: host_power,
                    pid_count: ph.pids.len(),
                    top_command: ph.top_command,
                }
            })
            .collect();

        UserAggregate {
            user,
            is_system,
            node_count: node_count.len(),
            gpu_count: self.touched_gpus.len(),
            process_count: self.touched_pids.len(),
            vram_bytes: self.vram_bytes,
            power_watts,
            longest_seconds: self.longest_seconds,
            top_command: self.top_command,
            per_host,
        }
    }
}

/// Choose the display string for the user's `top_command`.
///
/// Prefers the full command line; falls back to the short process name
/// so the column is never empty (e.g. kernel threads that only emit
/// `name`).
fn pick_display_command(row: &ParsedProcessRow) -> String {
    if !row.command.is_empty() {
        row.command.clone()
    } else if !row.name.is_empty() {
        row.name.clone()
    } else {
        String::new()
    }
}

// ---------------------------------------------------------------------
// Sorting keys used by the Users tab in-tab hotkeys (u / m / p / n / t).
// Kept on the aggregation side so the renderer can stay UI-only.
// ---------------------------------------------------------------------

/// In-tab sort selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum UserSortKey {
    /// Alphabetical by username (default).
    #[default]
    User,
    /// Descending VRAM.
    Memory,
    /// Descending approximated power.
    Power,
    /// Descending node count.
    Nodes,
    /// Descending oldest start time (LONGEST column).
    Longest,
}

/// Sort a `UserAggregate` slice in place according to `key`.  Ties are
/// broken by `user` alphabetically so the output order is stable across
/// renders.
pub fn sort_users(users: &mut [UserAggregate], key: UserSortKey) {
    use std::cmp::Ordering;
    users.sort_by(|a, b| {
        let primary = match key {
            UserSortKey::User => a.user.cmp(&b.user),
            UserSortKey::Memory => b.vram_bytes.cmp(&a.vram_bytes),
            UserSortKey::Power => b
                .power_watts
                .partial_cmp(&a.power_watts)
                .unwrap_or(Ordering::Equal),
            UserSortKey::Nodes => b.node_count.cmp(&a.node_count),
            UserSortKey::Longest => b.longest_seconds.cmp(&a.longest_seconds),
        };
        primary.then_with(|| a.user.cmp(&b.user))
    });
}

/// Format an elapsed-seconds value as `<days>d HH:MM:SS` or `HH:MM:SS`
/// for the LONGEST column.  Values of 0 render as `—`.
pub fn format_longest(seconds: u64) -> String {
    if seconds == 0 {
        return "—".to_string();
    }
    let days = seconds / 86_400;
    let rem = seconds % 86_400;
    let hours = rem / 3_600;
    let minutes = (rem % 3_600) / 60;
    let secs = rem % 60;
    if days > 0 {
        format!("{days}d {hours:02}:{minutes:02}:{secs:02}")
    } else {
        format!("{hours:02}:{minutes:02}:{secs:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(
        pid: u32,
        user: &str,
        gpu_index: u32,
        gpu_memory_bytes: u64,
        start_time_seconds: u64,
        command: &str,
    ) -> ParsedProcessRow {
        ParsedProcessRow {
            host: String::new(), // host comes from the enclosing HostSnapshot
            pid,
            user: user.to_string(),
            command: command.to_string(),
            name: command.to_string(),
            gpu_index,
            gpu_uuid: format!("GPU-{gpu_index}"),
            gpu_memory_bytes,
            cpu_pct_tenths: 0,
            start_time_seconds,
        }
    }

    fn gpu(host: &str, gpu_index: u32, power_watts: f64) -> GpuForAggregation {
        GpuForAggregation {
            host: host.to_string(),
            gpu_index,
            power_watts,
        }
    }

    #[test]
    fn empty_input_produces_empty_result() {
        let result = aggregate_users(&[]);
        assert!(result.users.is_empty());
        assert!(!result.is_partial());
    }

    fn snap(
        host: &str,
        gpus: Vec<GpuForAggregation>,
        processes: Vec<ParsedProcessRow>,
    ) -> HostSnapshot {
        HostSnapshot {
            host: host.to_string(),
            gpus,
            processes,
            is_connected: true,
        }
    }

    fn snap_disconnected(
        host: &str,
        gpus: Vec<GpuForAggregation>,
        processes: Vec<ParsedProcessRow>,
    ) -> HostSnapshot {
        HostSnapshot {
            host: host.to_string(),
            gpus,
            processes,
            is_connected: false,
        }
    }

    #[test]
    fn same_pid_on_two_hosts_counts_as_two_processes() {
        let snapshots = vec![
            snap(
                "a",
                vec![gpu("a", 0, 100.0)],
                vec![row(42, "alice", 0, 1000, 100, "train")],
            ),
            snap(
                "b",
                vec![gpu("b", 0, 100.0)],
                vec![row(42, "alice", 0, 2000, 200, "train")],
            ),
        ];
        let result = aggregate_users(&snapshots);
        assert_eq!(result.users.len(), 1);
        let u = &result.users[0];
        assert_eq!(u.user, "alice");
        assert_eq!(u.process_count, 2, "(host, pid) must disambiguate");
        assert_eq!(u.node_count, 2);
        assert_eq!(u.vram_bytes, 3000);
        assert_eq!(u.longest_seconds, 200);
    }

    #[test]
    fn root_is_marked_as_system_user() {
        let snapshots = vec![snap(
            "h",
            vec![gpu("h", 0, 50.0)],
            vec![row(1, "root", 0, 0, 10, "containerd-shim")],
        )];
        let result = aggregate_users(&snapshots);
        assert_eq!(result.users.len(), 1);
        assert!(result.users[0].is_system);
    }

    #[test]
    fn numeric_system_uid_is_marked_as_system_user() {
        assert!(is_system_user("0"));
        assert!(is_system_user("999"));
        assert!(!is_system_user("1000"));
        assert!(!is_system_user("alice"));
    }

    #[test]
    fn user_spanning_multiple_gpus_accumulates_correctly() {
        let snapshots = vec![
            snap(
                "h1",
                vec![gpu("h1", 0, 200.0), gpu("h1", 1, 300.0)],
                vec![
                    row(100, "bob", 0, 1_000, 50, "a"),
                    row(100, "bob", 1, 2_000, 50, "a"),
                ],
            ),
            snap(
                "h2",
                vec![gpu("h2", 0, 400.0)],
                vec![row(200, "bob", 0, 5_000, 100, "b")],
            ),
        ];
        let result = aggregate_users(&snapshots);
        assert_eq!(result.users.len(), 1);
        let u = &result.users[0];
        assert_eq!(u.node_count, 2);
        assert_eq!(u.gpu_count, 3, "two GPUs on h1 + one on h2");
        assert_eq!(u.process_count, 2);
        assert_eq!(u.vram_bytes, 8_000);
    }

    #[test]
    fn oldest_start_time_wins_longest() {
        let snapshots = vec![snap(
            "h",
            vec![gpu("h", 0, 10.0)],
            vec![
                row(1, "alice", 0, 10, 500, "a"),
                row(2, "alice", 0, 20, 1_500_000, "b"),
                row(3, "alice", 0, 30, 100, "c"),
            ],
        )];
        let result = aggregate_users(&snapshots);
        assert_eq!(result.users[0].longest_seconds, 1_500_000);
    }

    #[test]
    fn top_command_is_the_owner_of_the_largest_vram_row() {
        let snapshots = vec![snap(
            "h",
            vec![gpu("h", 0, 10.0)],
            vec![
                row(1, "alice", 0, 1_000, 0, "small"),
                row(2, "alice", 0, 9_000, 0, "big"),
                row(3, "alice", 0, 5_000, 0, "medium"),
            ],
        )];
        let result = aggregate_users(&snapshots);
        assert_eq!(result.users[0].top_command, "big");
    }

    #[test]
    fn partial_coverage_is_detected() {
        // Two hosts; only one reported process rows. The silent host
        // is explicitly disconnected so the new "connected =
        // reporting" rule from the partial-chip fix does not rescue
        // it; partial coverage must still trigger.
        let snapshots = vec![
            snap(
                "a",
                vec![gpu("a", 0, 100.0)],
                vec![row(1, "alice", 0, 10, 0, "x")],
            ),
            snap_disconnected("b", vec![gpu("b", 0, 100.0)], vec![]),
        ];
        let result = aggregate_users(&snapshots);
        assert!(result.is_partial());
        assert_eq!(result.reporting_hosts, 1);
        assert_eq!(result.total_hosts, 2);
    }

    /// Regression for F4 in PR #199: a connected host with
    /// `--processes` enabled but no running GPU workload legitimately
    /// emits zero process rows. The partial-coverage chip must **not**
    /// fire on the basis that such a host appears "silent" — its
    /// connection is live, and the operator has configured the scrape
    /// exactly as intended. Only genuinely-disconnected (or
    /// scrape-less) hosts should count against the numerator.
    #[test]
    fn idle_connected_host_is_not_flagged_as_partial() {
        let snapshots = vec![
            snap(
                "a",
                vec![gpu("a", 0, 100.0)],
                vec![row(1, "alice", 0, 10, 0, "x")],
            ),
            // Host `b` is connected but idle: no processes, yet the
            // scraper is alive. This is the false-positive case.
            snap("b", vec![gpu("b", 0, 100.0)], vec![]),
        ];
        let result = aggregate_users(&snapshots);
        assert!(
            !result.is_partial(),
            "idle-but-connected host must not trigger the partial chip"
        );
        assert_eq!(result.reporting_hosts, 2);
        assert_eq!(result.total_hosts, 2);
    }

    #[test]
    fn power_approximation_clamps_negatives_to_zero() {
        // A GPU reports negative power (malformed sensor).  The
        // aggregator must clamp at zero rather than propagate the bad
        // value.
        let snapshots = vec![snap(
            "h",
            vec![gpu("h", 0, -50.0)],
            vec![row(1, "alice", 0, 1000, 0, "x")],
        )];
        let result = aggregate_users(&snapshots);
        assert_eq!(result.users[0].power_watts, 0.0);
    }

    #[test]
    fn power_approximation_weights_by_vram_share() {
        // Two users share a single GPU 70/30; the 400 W GPU power
        // should split ~280/120.
        let snapshots = vec![snap(
            "h",
            vec![gpu("h", 0, 400.0)],
            vec![
                row(1, "alice", 0, 7_000, 0, "a"),
                row(2, "bob", 0, 3_000, 0, "b"),
            ],
        )];
        let result = aggregate_users(&snapshots);
        let alice = result.users.iter().find(|u| u.user == "alice").unwrap();
        let bob = result.users.iter().find(|u| u.user == "bob").unwrap();
        let total = alice.power_watts + bob.power_watts;
        // Sum of weighted-power shares must equal the GPU power when
        // every user's VRAM is accounted for.
        assert!((total - 400.0).abs() < 1e-6, "got {total}");
        assert!(
            (alice.power_watts - 280.0).abs() < 1e-6,
            "got {}",
            alice.power_watts
        );
    }

    #[test]
    fn missing_user_label_becomes_unattributed() {
        let snapshots = vec![snap(
            "h",
            vec![gpu("h", 0, 100.0)],
            vec![row(1, "", 0, 100, 0, "x")],
        )];
        let result = aggregate_users(&snapshots);
        assert_eq!(result.users.len(), 1);
        assert_eq!(result.users[0].user, UNATTRIBUTED_USER);
    }

    #[test]
    fn per_host_breakdown_sums_to_the_aggregate_power() {
        // Invariant: per-host power shares add up to the top-level
        // number (within f64 rounding).  If this ever breaks the
        // drill-down will disagree with the main table, so guard it.
        let snapshots = vec![
            snap(
                "h1",
                vec![gpu("h1", 0, 200.0), gpu("h1", 1, 300.0)],
                vec![
                    row(1, "alice", 0, 1_000, 0, "a"),
                    row(1, "alice", 1, 3_000, 0, "a"),
                ],
            ),
            snap(
                "h2",
                vec![gpu("h2", 0, 400.0)],
                vec![row(2, "alice", 0, 5_000, 0, "b")],
            ),
        ];
        let result = aggregate_users(&snapshots);
        let u = &result.users[0];
        let per_host_sum: f64 = u.per_host.iter().map(|h| h.power_watts).sum();
        assert!((per_host_sum - u.power_watts).abs() < 1e-6);
        // Each host must appear exactly once.
        let mut hosts: Vec<&str> = u.per_host.iter().map(|h| h.host.as_str()).collect();
        hosts.sort();
        assert_eq!(hosts, vec!["h1", "h2"]);
    }

    #[test]
    fn sort_users_by_memory_descending() {
        let mut u1 = UserAggregate {
            user: "alice".into(),
            is_system: false,
            node_count: 1,
            gpu_count: 1,
            process_count: 1,
            vram_bytes: 100,
            power_watts: 0.0,
            longest_seconds: 0,
            top_command: "".into(),
            per_host: vec![],
        };
        let mut u2 = UserAggregate {
            user: "bob".into(),
            is_system: false,
            node_count: 1,
            gpu_count: 1,
            process_count: 1,
            vram_bytes: 300,
            power_watts: 0.0,
            longest_seconds: 0,
            top_command: "".into(),
            per_host: vec![],
        };
        u1.longest_seconds = 10;
        u2.longest_seconds = 20;
        let mut v = vec![u1, u2];
        sort_users(&mut v, UserSortKey::Memory);
        assert_eq!(v[0].user, "bob", "highest VRAM first");
    }

    #[test]
    fn format_longest_renders_days_when_over_a_day() {
        // 1 day 3 hours 12 minutes 7 seconds = 97 927 seconds.
        let s = format_longest(97_927);
        assert!(s.contains("1d"), "expected days prefix, got {s}");
    }

    #[test]
    fn format_longest_returns_dash_for_zero() {
        assert_eq!(format_longest(0), "—");
    }

    #[test]
    fn aggregation_handles_large_cluster_quickly() {
        // Synthetic 100 nodes × 50 procs over 4 users, enforcing the
        // <50 ms budget in the issue's acceptance criteria.  Run in
        // debug mode by default since `cargo test` disables --release;
        // we set a generous cap to keep the test stable on CI.
        let users = ["alice", "bob", "carol", "dave"];
        let mut snaps = Vec::with_capacity(100);
        for h in 0..100 {
            let host = format!("h{h}");
            let gpus = (0..8)
                .map(|i| GpuForAggregation {
                    host: host.clone(),
                    gpu_index: i,
                    power_watts: 200.0,
                })
                .collect();
            let procs = (0..50)
                .map(|p| ParsedProcessRow {
                    host: host.clone(),
                    pid: p as u32,
                    user: users[(p as usize) % users.len()].to_string(),
                    command: "python".to_string(),
                    name: "python".to_string(),
                    gpu_index: (p % 8) as u32,
                    gpu_uuid: format!("GPU-{h}-{}", p % 8),
                    gpu_memory_bytes: 1_000_000_000,
                    cpu_pct_tenths: 0,
                    start_time_seconds: (p * 7) as u64,
                })
                .collect();
            snaps.push(HostSnapshot {
                host,
                gpus,
                processes: procs,
                is_connected: true,
            });
        }
        let start = std::time::Instant::now();
        let result = aggregate_users(&snaps);
        let elapsed = start.elapsed();
        assert_eq!(result.users.len(), users.len());
        // 500 ms is way above the spec target; we only need to catch
        // quadratic regressions.  The aggregation for 100×50 rows is
        // several orders of magnitude below this on any modern CPU.
        assert!(
            elapsed.as_millis() < 500,
            "aggregate_users took {elapsed:?} for 100 hosts × 50 procs"
        );
    }

    #[test]
    #[ignore = "adversarial stress — run manually with `cargo test -- --ignored`"]
    fn aggregation_survives_adversarial_50k_per_host() {
        // Adversarial input: a malicious remote host pushes the
        // 50 000-row per-host process cap (the limit enforced in
        // `metrics_parser.rs`). With 10 such hosts that is 500 000 rows,
        // which is still the parser's worst case over a multi-host
        // cluster.  The aggregation must not turn quadratic on this
        // input.
        //
        // Marked #[ignore] so CI stays fast; this is for developers to
        // run locally when touching the aggregation hot path.
        let rows_per_host = 50_000;
        let host_count = 10;
        let users = ["alice", "bob", "carol", "dave", "eve", "frank"];
        let mut snaps = Vec::with_capacity(host_count);
        for h in 0..host_count {
            let host = format!("h{h}");
            let gpus = (0..8)
                .map(|i| GpuForAggregation {
                    host: host.clone(),
                    gpu_index: i,
                    power_watts: 400.0,
                })
                .collect();
            let procs = (0..rows_per_host)
                .map(|p| ParsedProcessRow {
                    host: host.clone(),
                    pid: p as u32,
                    user: users[(p as usize) % users.len()].to_string(),
                    command: "python".to_string(),
                    name: "python".to_string(),
                    gpu_index: (p % 8) as u32,
                    gpu_uuid: format!("GPU-{h}-{}", p % 8),
                    gpu_memory_bytes: 1024,
                    cpu_pct_tenths: 0,
                    start_time_seconds: (p * 7) as u64,
                })
                .collect();
            snaps.push(HostSnapshot {
                host,
                gpus,
                processes: procs,
                is_connected: true,
            });
        }
        let start = std::time::Instant::now();
        let result = aggregate_users(&snaps);
        let elapsed = start.elapsed();
        assert_eq!(result.users.len(), users.len());
        // Loose 5-second ceiling catches quadratic blow-ups; in release
        // mode the real number is well under 1 s for 500 000 rows.
        assert!(
            elapsed.as_secs() < 5,
            "adversarial aggregation took {elapsed:?} for              {host_count}×{rows_per_host} rows — possible O(n^2) regression"
        );
    }
}
