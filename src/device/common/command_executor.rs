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

// Standardized command execution utilities for device modules.
//
// Goals:
// - Centralize timeout behavior
// - Normalize stdout/stderr handling (UTF-8 lossy conversion)
// - Provide an optional status check
// - Keep a default helper compatible with existing call sites

use crate::device::common::{DeviceError, DeviceResult, validate_args, validate_command};
use crate::utils::{command_timeout::run_command_with_timeout, run_command_fast_fail};
use std::time::Duration;

/// Options to control command execution behavior.
#[derive(Debug, Clone, Default)]
pub struct CommandOptions {
    /// Optional timeout to use. If None, uses the environment-aware fast-fail timeout.
    pub timeout: Option<Duration>,
    /// If true, non-zero exit statuses will return an error.
    pub check_status: bool,
}

/// Normalized command output.
#[derive(Debug, Clone)]
pub struct CommandOutput {
    /// Process exit code (or -1 if unavailable)
    pub status: i32,
    /// UTF-8 (lossy) decoded stdout
    pub stdout: String,
    /// UTF-8 (lossy) decoded stderr
    pub stderr: String,
}

/// Execute a command with the provided CommandOptions.
///
/// - Validates command and arguments for security
/// - If options.timeout is Some, uses run_command_with_timeout with that duration
/// - Otherwise, uses run_command_fast_fail (which adapts to container environments)
/// - When options.check_status is true and exit code != 0, returns DeviceError::CommandFailed
pub fn execute_command(
    command: &str,
    args: &[&str],
    options: &CommandOptions,
) -> DeviceResult<CommandOutput> {
    // Validate command and arguments for security
    if !validate_command(command) {
        return Err(DeviceError::Other(format!(
            "Invalid command rejected: {command}"
        )));
    }

    if !validate_args(args) {
        return Err(DeviceError::Other(format!(
            "Invalid arguments rejected for command: {command}"
        )));
    }

    let output = if let Some(timeout) = options.timeout {
        run_command_with_timeout(command, args, timeout)?
    } else {
        run_command_fast_fail(command, args)?
    };

    let status_code = output.status.code().unwrap_or(-1);
    let out = CommandOutput {
        status: status_code,
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    };

    if options.check_status && status_code != 0 {
        let full_command = if args.is_empty() {
            command.to_string()
        } else {
            format!("{command} {}", args.join(" "))
        };

        eprintln!("Command execution failed: '{full_command}' (exit code: {status_code})");

        if !out.stderr.is_empty() {
            eprintln!("Stderr output: {}", out.stderr);
        }

        return Err(DeviceError::CommandFailed {
            command: full_command,
            code: Some(status_code),
            stderr: out.stderr.clone(),
        });
    }

    Ok(out)
}

/// Convenience helper that mirrors legacy behavior:
/// - Uses environment-aware fast-fail timeout
/// - Does NOT enforce status checking (caller may inspect `status`)
pub fn execute_command_default(command: &str, args: &[&str]) -> DeviceResult<CommandOutput> {
    execute_command(command, args, &CommandOptions::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_execute_command_default_success() {
        let out = execute_command_default("echo", &["hello"]).expect("echo should succeed");
        assert_eq!(out.status, 0);
        assert!(out.stdout.contains("hello"));
    }

    #[test]
    fn test_execute_command_with_status_check() {
        let opts = CommandOptions {
            timeout: Some(Duration::from_secs(2)),
            check_status: true,
        };
        // Use `false` which returns non-zero status on Unix
        let err = execute_command("false", &[], &opts).unwrap_err();
        match err {
            DeviceError::CommandFailed { .. } => {}
            _ => panic!("Expected CommandFailed error"),
        }
    }
}
