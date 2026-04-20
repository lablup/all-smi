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

//! Bounded-timeout child-process helpers used by every check that shells
//! out. Every invocation goes through this module so the "no
//! `Command::output()` without a wrapper" rule in issue #188 is enforced
//! mechanically rather than by reviewer vigilance.

use std::time::Duration;

use crate::utils::command_timeout::run_command_with_timeout;

/// Normalised command outcome. Matches [`crate::device::common::CommandOutput`]
/// but is duplicated here to keep `doctor` independent of the `device`
/// module layout (and to avoid pulling `device::common`'s validation layer
/// into every check).
#[derive(Debug, Clone)]
pub struct ExecOutput {
    /// Process exit code, or `-1` when unavailable.
    pub status: i32,
    /// UTF-8 (lossy) decoded stdout.
    pub stdout: String,
    /// UTF-8 (lossy) decoded stderr.
    pub stderr: String,
    /// Whether the command timed out.
    pub timed_out: bool,
}

impl ExecOutput {
    /// Shorthand: process exited with status 0.
    pub fn success(&self) -> bool {
        self.status == 0 && !self.timed_out
    }
}

/// Run `cmd` with `args` under a hard timeout. Returns `None` when the
/// binary is missing or otherwise cannot be launched; on timeout the
/// returned [`ExecOutput`] has `timed_out == true`.
///
/// Binaries are looked up on PATH by the underlying OS spawn; callers that
/// want to probe a specific path should pass it directly (e.g.
/// `"/usr/bin/hl-smi"`).
pub fn try_exec(cmd: &str, args: &[&str], timeout: Duration) -> Option<ExecOutput> {
    match run_command_with_timeout(cmd, args, timeout) {
        Ok(out) => Some(ExecOutput {
            status: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            timed_out: false,
        }),
        Err(e) if e.kind() == std::io::ErrorKind::TimedOut => Some(ExecOutput {
            status: -1,
            stdout: String::new(),
            stderr: format!("timed out after {timeout:?}"),
            timed_out: true,
        }),
        Err(_) => None,
    }
}

/// Convenience wrapper that strips the `ExecOutput` down to stdout when the
/// process succeeded, or returns `None` on failure / missing binary /
/// timeout. Kept around for checks that only care about the stdout of a
/// best-effort probe (no current caller — new check authors often reach
/// for this first, so leaving it exposed avoids re-adding later).
#[allow(dead_code)]
pub fn exec_stdout_ok(cmd: &str, args: &[&str], timeout: Duration) -> Option<String> {
    let out = try_exec(cmd, args, timeout)?;
    if out.success() {
        Some(out.stdout)
    } else {
        None
    }
}

/// Return `true` when `cmd` resolves to an existing executable on PATH or
/// is an absolute/relative path that exists on disk. Bounded by a tiny
/// internal timeout via the probe-command approach (`which cmd` /
/// `where cmd`).
pub fn which(cmd: &str) -> Option<String> {
    // Absolute or relative paths: resolve directly, no spawn needed.
    let p = std::path::Path::new(cmd);
    if p.is_absolute() || p.components().count() > 1 {
        return if p.exists() {
            Some(cmd.to_string())
        } else {
            None
        };
    }

    #[cfg(unix)]
    let probe = ("which", cmd);
    #[cfg(windows)]
    let probe = ("where", cmd);

    let out = try_exec(probe.0, &[probe.1], Duration::from_millis(500))?;
    if out.success() {
        out.stdout.lines().next().map(|l| l.trim().to_string())
    } else {
        None
    }
}
