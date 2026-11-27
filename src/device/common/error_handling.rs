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

// Common error types and result aliases for device modules.

use std::fmt;
use std::io;

#[derive(Debug)]
pub enum DeviceError {
    Io(io::Error),
    CommandFailed {
        command: String,
        code: Option<i32>,
        stderr: String,
    },
    ParseError(String),
    Other(String),
}

pub type DeviceResult<T> = Result<T, DeviceError>;

impl fmt::Display for DeviceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DeviceError::Io(e) => write!(f, "IO error: {e}"),
            DeviceError::CommandFailed {
                command,
                code,
                stderr,
            } => {
                write!(
                    f,
                    "Command failed: '{command}' (code: {code:?}) stderr: {stderr}"
                )
            }
            DeviceError::ParseError(msg) => write!(f, "Parse error: {msg}"),
            DeviceError::Other(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for DeviceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            DeviceError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for DeviceError {
    fn from(value: io::Error) -> Self {
        DeviceError::Io(value)
    }
}
