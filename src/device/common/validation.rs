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

// Input validation utilities for command execution

use std::path::Path;

/// Validates a command name is safe to execute
/// Returns true if the command is allowed, false otherwise
pub fn validate_command(command: &str) -> bool {
    // Reject empty commands
    if command.is_empty() {
        return false;
    }

    // Reject commands with shell metacharacters
    const DANGEROUS_CHARS: &[char] = &[
        ';', '&', '|', '>', '<', '$', '`', '\n', '\r', '(', ')', '{', '}',
    ];
    if command.chars().any(|c| DANGEROUS_CHARS.contains(&c)) {
        eprintln!("Potentially dangerous command rejected: {command}");
        return false;
    }

    // Reject path traversal attempts
    if command.contains("..") {
        eprintln!("Command with path traversal rejected: {command}");
        return false;
    }

    true
}

/// Validates command arguments are safe
/// Returns true if all arguments are safe, false otherwise
pub fn validate_args(args: &[&str]) -> bool {
    for arg in args {
        // Reject empty arguments
        if arg.is_empty() {
            continue; // Empty args are often harmless
        }

        // Reject arguments with shell metacharacters that could cause injection
        const DANGEROUS_CHARS: &[char] = &[';', '&', '|', '`', '\n', '\r', '$'];
        if arg.chars().any(|c| DANGEROUS_CHARS.contains(&c)) {
            eprintln!("Potentially dangerous argument rejected: {arg}");
            return false;
        }
    }

    true
}

/// Validates a path is safe to use as a command
/// Returns true if the path is safe, false otherwise
#[allow(dead_code)]
pub fn validate_command_path(path: &Path) -> bool {
    // Must be an absolute path
    if !path.is_absolute() {
        eprintln!("Non-absolute command path rejected: {path:?}");
        return false;
    }

    // Must not contain path traversal
    if let Some(path_str) = path.to_str()
        && path_str.contains("..")
    {
        eprintln!("Command path with traversal rejected: {path_str}");
        return false;
    }

    // Should exist and be executable (on Unix)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(metadata) = path.metadata() {
            let permissions = metadata.permissions();
            // Check if any execute bit is set
            if permissions.mode() & 0o111 == 0 {
                eprintln!("Non-executable command path: {path:?}");
                return false;
            }
        } else {
            // Path doesn't exist
            eprintln!("Command path does not exist: {path:?}");
            return false;
        }
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_command() {
        // Valid commands
        assert!(validate_command("nvidia-smi"));
        assert!(validate_command("/usr/bin/echo"));

        // Invalid commands
        assert!(!validate_command(""));
        assert!(!validate_command("echo; rm -rf /"));
        assert!(!validate_command("echo && malicious"));
        assert!(!validate_command("cat | grep"));
        assert!(!validate_command("../../bin/evil"));
        assert!(!validate_command("echo $(whoami)"));
    }

    #[test]
    fn test_validate_args() {
        // Valid arguments
        assert!(validate_args(&["--json", "--output", "file.txt"]));
        assert!(validate_args(&["-L", "-v"]));

        // Invalid arguments
        assert!(!validate_args(&["; rm -rf /"]));
        assert!(!validate_args(&["$(whoami)"]));
        assert!(!validate_args(&["file.txt | cat"]));
    }

    #[test]
    fn test_validate_command_path() {
        use std::path::PathBuf;

        // Valid paths
        assert!(validate_command_path(&PathBuf::from("/bin/ls")));

        // Invalid paths
        assert!(!validate_command_path(&PathBuf::from("relative/path")));
        assert!(!validate_command_path(&PathBuf::from("/usr/../etc/passwd")));
    }
}
