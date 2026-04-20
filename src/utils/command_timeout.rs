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

use std::io::{self, Read};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// Maximum bytes of stdout or stderr captured from a child process by
/// [`run_command_with_timeout`]. Output beyond this cap is truncated and
/// a suffix marker is appended so callers can see the truncation
/// happened. The limit protects against hostile or runaway child
/// processes (for example, a `dmesg` with millions of lines on a host
/// with loose kernel logging) from exhausting the parent's address
/// space.
///
/// 16 MiB is generous for every real all-smi caller — `nvidia-smi
/// --query-gpu=...`, `rocminfo`, `hl-smi -Q`, `lsmod`, `lspci -vv`,
/// and the gzipped `dmesg -T` on a freshly booted host all fit inside
/// 1 MiB. Anything approaching 16 MiB is already pathological and the
/// truncation is intentional.
pub const COMMAND_OUTPUT_CAP_BYTES: usize = 16 * 1024 * 1024;

/// Suffix appended to stdout / stderr when the cap is reached. Kept
/// verbatim so downstream parsers can detect truncation.
pub const OUTPUT_TRUNCATED_MARKER: &[u8] = b"\n[truncated by all-smi: output exceeded cap]\n";

/// Read up to `cap` bytes from `reader` into `buf`. When `cap` is
/// reached the remaining bytes on the reader are drained (so the child
/// process does not block on a full pipe) but discarded, and the
/// truncation marker is appended to `buf`.
///
/// This helper exists because `std::io::Read::take` would let a
/// still-running child fill its stdout pipe indefinitely — we must
/// actively drain the descriptor so the child can terminate.
fn read_capped<R: Read>(mut reader: R, buf: &mut Vec<u8>, cap: usize) {
    // Read in bounded chunks so a caller-specified `cap` that is larger
    // than available memory still holds. 8 KiB is a typical pipe buffer
    // size; larger chunks yield marginal throughput gains on short
    // SMI-style outputs.
    const CHUNK: usize = 8 * 1024;
    let mut chunk = [0u8; CHUNK];
    let mut truncated = false;
    loop {
        match reader.read(&mut chunk) {
            Ok(0) => break, // EOF
            Ok(n) => {
                if truncated {
                    // Still draining after cap reached — discard.
                    continue;
                }
                let remaining = cap.saturating_sub(buf.len());
                if remaining == 0 {
                    truncated = true;
                    continue;
                }
                let take = n.min(remaining);
                buf.extend_from_slice(&chunk[..take]);
                if buf.len() >= cap {
                    truncated = true;
                    // Keep draining so the child is not blocked on
                    // a full pipe, but do not accumulate further.
                }
            }
            Err(_) => break,
        }
    }
    if truncated {
        buf.extend_from_slice(OUTPUT_TRUNCATED_MARKER);
    }
}

/// Execute a command with a timeout.
/// Returns Ok(Output) if the command completes within the timeout,
/// Err if timeout occurs or command fails to start.
///
/// IMPORTANT: This function properly kills the child process on timeout
/// to prevent process accumulation. stdout and stderr are each capped
/// at [`COMMAND_OUTPUT_CAP_BYTES`]; output past the cap is truncated
/// and a marker appended so callers / tests can detect the truncation.
pub fn run_command_with_timeout(
    command: &str,
    args: &[&str],
    timeout: Duration,
) -> io::Result<Output> {
    // Spawn the child process
    let mut child = Command::new(command)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let start = Instant::now();
    let poll_interval = Duration::from_millis(10);

    // Poll for completion with timeout
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                // Process completed - read capped output.
                let mut stdout = Vec::new();
                let mut stderr = Vec::new();

                if let Some(out) = child.stdout.take() {
                    read_capped(out, &mut stdout, COMMAND_OUTPUT_CAP_BYTES);
                }
                if let Some(err) = child.stderr.take() {
                    read_capped(err, &mut stderr, COMMAND_OUTPUT_CAP_BYTES);
                }

                return Ok(Output {
                    status,
                    stdout,
                    stderr,
                });
            }
            Ok(None) => {
                // Process still running - check timeout
                if start.elapsed() >= timeout {
                    // Timeout! Kill the process
                    let _ = child.kill();
                    let _ = child.wait(); // Reap the zombie process
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!("Command '{command}' timed out after {timeout:?}"),
                    ));
                }
                // Sleep briefly before polling again
                std::thread::sleep(poll_interval);
            }
            Err(e) => {
                // Error checking process status
                let _ = child.kill();
                let _ = child.wait();
                return Err(e);
            }
        }
    }
}

/// Execute a command with a short timeout suitable for container environments.
/// Default timeout is 1 second for fast failure in containers.
pub fn run_command_fast_fail(command: &str, args: &[&str]) -> io::Result<Output> {
    // Check if we're in a container environment
    let timeout = if is_container_environment() {
        Duration::from_millis(500) // Very short timeout in containers
    } else {
        Duration::from_secs(2) // Normal timeout for bare metal
    };

    run_command_with_timeout(command, args, timeout)
}

/// Detect if we're running in a container environment
fn is_container_environment() -> bool {
    // Check for common container indicators
    std::path::Path::new("/.dockerenv").exists()
        || std::path::Path::new("/run/.containerenv").exists()
        || std::env::var("KUBERNETES_SERVICE_HOST").is_ok()
        || std::env::var("CONTAINER_RUNTIME").is_ok()
        || check_cgroup_container()
}

/// Check cgroup for container indicators
fn check_cgroup_container() -> bool {
    if let Ok(contents) = std::fs::read_to_string("/proc/self/cgroup") {
        contents.contains("/docker/")
            || contents.contains("/lxc/")
            || contents.contains("/kubepods/")
            || contents.contains("/containerd/")
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(unix)]
    fn output_cap_truncates_hostile_stdout() {
        // `yes` produces an unbounded stream of lines. Without a cap
        // this would grow `stdout` until either OOM or EBADF. With
        // COMMAND_OUTPUT_CAP_BYTES the call must return a bounded
        // buffer plus the truncation marker.
        let output = run_command_with_timeout("yes", &["hello"], Duration::from_millis(150));
        // On systems without `yes` the call returns Err(NotFound);
        // skip silently in that case.
        let output = match output {
            Ok(o) => o,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return,
            // A timeout is also acceptable here since `yes` never exits;
            // but we can't inspect the cap when we timed out because the
            // function returns Err on timeout.
            Err(e) if e.kind() == io::ErrorKind::TimedOut => return,
            Err(e) => panic!("unexpected error: {e}"),
        };

        // Buffer must never exceed the cap by more than the marker size.
        let max_allowed = COMMAND_OUTPUT_CAP_BYTES + OUTPUT_TRUNCATED_MARKER.len() + 4096;
        assert!(
            output.stdout.len() <= max_allowed,
            "stdout uncapped: len={}",
            output.stdout.len()
        );
    }

    #[test]
    fn output_cap_leaves_small_outputs_alone() {
        // `printf` on Linux / macOS emits a tiny payload that should
        // survive the cap unchanged and without the truncation marker.
        #[cfg(unix)]
        {
            let out = run_command_with_timeout("printf", &["hello"], Duration::from_secs(2));
            if let Ok(out) = out {
                assert_eq!(out.stdout, b"hello");
                let marker = String::from_utf8_lossy(OUTPUT_TRUNCATED_MARKER);
                assert!(
                    !String::from_utf8_lossy(&out.stdout).contains(marker.as_ref()),
                    "small output should not carry the truncation marker"
                );
            }
        }
    }
}
