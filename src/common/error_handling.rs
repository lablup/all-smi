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

use std::fmt;

/// Common error types used throughout the application
#[derive(Debug)]
#[allow(dead_code)] // Future error handling architecture
#[allow(clippy::enum_variant_names)] // Clear naming convention
pub enum AppError {
    TerminalError(String),
    NetworkError(String),
    ConfigError(String),
    DataError(String),
    IOError(String),
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AppError::TerminalError(msg) => write!(f, "Terminal error: {msg}"),
            AppError::NetworkError(msg) => write!(f, "Network error: {msg}"),
            AppError::ConfigError(msg) => write!(f, "Configuration error: {msg}"),
            AppError::DataError(msg) => write!(f, "Data error: {msg}"),
            AppError::IOError(msg) => write!(f, "IO error: {msg}"),
        }
    }
}

impl std::error::Error for AppError {}

/// Result type with common error
#[allow(dead_code)] // Future error handling architecture
pub type AppResult<T> = Result<T, AppError>;

/// Error handling utilities
#[allow(dead_code)] // Future error handling architecture
pub struct ErrorHandler;

#[allow(dead_code)] // Future error handling architecture
impl ErrorHandler {
    /// Log error and return default value
    pub fn log_and_default<T: Default>(error: impl std::error::Error, context: &str) -> T {
        eprintln!("Warning [{context}]: {error}");
        T::default()
    }

    /// Log error and return provided default
    pub fn log_and_return<T>(error: impl std::error::Error, context: &str, default: T) -> T {
        eprintln!("Warning [{context}]: {error}");
        default
    }

    /// Log error and continue (for non-critical errors)
    pub fn log_and_continue(error: impl std::error::Error, context: &str) {
        eprintln!("Warning [{context}]: {error}");
    }

    /// Convert common errors to AppError
    pub fn from_io_error(err: std::io::Error, context: &str) -> AppError {
        AppError::IOError(format!("{context}: {err}"))
    }

    pub fn from_network_error(err: reqwest::Error, context: &str) -> AppError {
        AppError::NetworkError(format!("{context}: {err}"))
    }

    pub fn from_parse_error<E: std::error::Error>(err: E, context: &str) -> AppError {
        AppError::DataError(format!("{context}: {err}"))
    }
}

/// Macro for simplified error handling with context
#[macro_export]
macro_rules! handle_error {
    ($result:expr_2021, $context:expr_2021) => {
        match $result {
            Ok(val) => val,
            Err(err) => return Err(AppError::from(format!("{}: {}", $context, err))),
        }
    };
}

/// Macro for logging errors and using default values
#[macro_export]
macro_rules! log_error_default {
    ($result:expr_2021, $context:expr_2021) => {
        match $result {
            Ok(val) => val,
            Err(err) => {
                eprintln!("Warning [{}]: {}", $context, err);
                Default::default()
            }
        }
    };
}

/// Macro for logging errors and using provided default
#[macro_export]
macro_rules! log_error_return {
    ($result:expr_2021, $context:expr_2021, $default:expr_2021) => {
        match $result {
            Ok(val) => val,
            Err(err) => {
                eprintln!("Warning [{}]: {}", $context, err);
                $default
            }
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_display() {
        let error = AppError::TerminalError("Failed to initialize".to_string());
        assert_eq!(error.to_string(), "Terminal error: Failed to initialize");
    }

    #[test]
    fn test_error_handler_log_and_default() {
        let result: i32 = ErrorHandler::log_and_default(
            std::io::Error::new(std::io::ErrorKind::NotFound, "File not found"),
            "test",
        );
        assert_eq!(result, 0); // Default for i32
    }

    #[test]
    fn test_error_handler_log_and_return() {
        let result = ErrorHandler::log_and_return(
            std::io::Error::new(std::io::ErrorKind::NotFound, "File not found"),
            "test",
            42,
        );
        assert_eq!(result, 42);
    }
}
