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

use std::ffi::OsStr;
use std::process::Command;

/// Create a command with platform-dependent handling
///
/// On Windows, the command is created with the `CREATE_NO_WINDOW` flag to
/// prevent a console window from appearing. This is required when all-smi
/// is used as a library for GUI applications.
///
/// On non-Windows this function is equivalent to `std::process::Command::new`.
#[allow(clippy::disallowed_methods)] // this is the sanctioned Command constructor
pub fn new_command(program: impl AsRef<OsStr>) -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;

        // CREATE_NO_WINDOW constant as described in Windows API docs
        // https://learn.microsoft.com/en-us/windows/win32/procthread/process-creation-flags
        const CREATE_NO_WINDOW: u32 = 0x08000000;

        let mut cmd = Command::new(program);
        cmd.creation_flags(CREATE_NO_WINDOW);
        cmd
    }

    #[cfg(not(windows))]
    Command::new(program)
}
