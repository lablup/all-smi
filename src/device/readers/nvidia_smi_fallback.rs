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

//! Back-off for the `nvidia-smi` fallback of the NVIDIA reader.
//!
//! When the shared NVML handle cannot start, NVML init itself is backed off
//! (`nvml_handle`), but the reader then falls back to `nvidia-smi`. That tool
//! is built on NVML, so on a host where NVML cannot start it usually fails or
//! hangs as well, and each spawn can block a collection tick for the full
//! command timeout. Both fallback queries (`--query-gpu` and
//! `--query-compute-apps`) therefore share one gate: after a failed spawn
//! neither runs again until the deadline passes.

use std::time::{Duration, Instant};

use crate::device::common::command_executor::CommandOutput;
use crate::device::common::execute_command_default;
use crate::device::readers::common_cache::RetryGate;

/// First delay after a failed fallback spawn. Same schedule as the NVML init
/// back-off in `nvml_handle`.
const FALLBACK_INITIAL_BACKOFF: Duration = Duration::from_secs(30);
/// Ceiling the delay doubles up to.
const FALLBACK_MAX_BACKOFF: Duration = Duration::from_secs(300);

/// Shared by both fallback queries of every `NvidiaGpuReader`.
static FALLBACK_GATE: RetryGate = RetryGate::new();

/// Run one `nvidia-smi` fallback query and return its stdout, or `None` when
/// the fallback is backed off or the query failed.
pub(crate) fn run_nvidia_smi_fallback(args: &[&str]) -> Option<String> {
    gated_stdout(&FALLBACK_GATE, Instant::now(), || {
        execute_command_default("nvidia-smi", args).ok()
    })
}

/// Run `run` unless `gate` is blocked at `now`.
///
/// `run` returns `None` for a spawn failure or a timeout. That, and a
/// non-zero exit status, start (or lengthen) the back-off; a zero status
/// clears it. While the gate is blocked `run` is not called at all.
fn gated_stdout(
    gate: &RetryGate,
    now: Instant,
    run: impl FnOnce() -> Option<CommandOutput>,
) -> Option<String> {
    if gate.is_blocked(now) {
        return None;
    }
    match run() {
        Some(output) if output.status == 0 => {
            gate.clear();
            Some(output.stdout)
        }
        _ => {
            gate.fail_with_backoff(now, FALLBACK_INITIAL_BACKOFF, FALLBACK_MAX_BACKOFF);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn output(status: i32, stdout: &str) -> Option<CommandOutput> {
        Some(CommandOutput {
            status,
            stdout: stdout.to_string(),
            stderr: String::new(),
        })
    }

    #[test]
    fn a_timeout_blocks_both_queries_until_the_deadline() {
        let gate = RetryGate::new();
        let calls = Cell::new(0);
        let t0 = Instant::now();

        // The device query times out (`None`).
        let first = gated_stdout(&gate, t0, || {
            calls.set(calls.get() + 1);
            None
        });
        assert!(first.is_none());
        assert_eq!(calls.get(), 1);

        // Inside the window neither the device query nor the process query
        // reaches the runner.
        for offset in [0, 1, 29] {
            let now = t0 + Duration::from_secs(offset);
            for _query in ["--query-gpu", "--query-compute-apps"] {
                let result = gated_stdout(&gate, now, || {
                    calls.set(calls.get() + 1);
                    output(0, "unreachable")
                });
                assert!(result.is_none());
            }
        }
        assert_eq!(calls.get(), 1);

        // At the deadline one call goes through.
        let after = gated_stdout(&gate, t0 + FALLBACK_INITIAL_BACKOFF, || {
            calls.set(calls.get() + 1);
            output(0, "0, GPU-x, name")
        });
        assert_eq!(after.as_deref(), Some("0, GPU-x, name"));
        assert_eq!(calls.get(), 2);
    }

    #[test]
    fn a_non_zero_status_backs_off_and_discards_stdout() {
        let gate = RetryGate::new();
        let t0 = Instant::now();
        let result = gated_stdout(&gate, t0, || output(9, "NVIDIA-SMI has failed"));
        assert!(result.is_none());
        assert!(gate.is_blocked(t0 + Duration::from_secs(29)));
    }

    #[test]
    fn success_clears_the_back_off_and_its_doubling() {
        let gate = RetryGate::new();
        let t0 = Instant::now();

        assert!(gated_stdout(&gate, t0, || None).is_none());
        let t1 = t0 + FALLBACK_INITIAL_BACKOFF;
        // Second consecutive failure doubles the delay to 60 s.
        assert!(gated_stdout(&gate, t1, || None).is_none());
        assert!(gate.is_blocked(t1 + Duration::from_secs(59)));

        let t2 = t1 + Duration::from_secs(60);
        assert!(gated_stdout(&gate, t2, || output(0, "ok")).is_some());
        assert!(!gate.is_blocked(t2));

        // The next failure starts again from the initial delay.
        assert!(gated_stdout(&gate, t2, || None).is_none());
        assert!(!gate.is_blocked(t2 + FALLBACK_INITIAL_BACKOFF));
    }
}
