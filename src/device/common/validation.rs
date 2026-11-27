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
}
