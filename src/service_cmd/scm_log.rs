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

//! File logging for the Windows service context (issue #311).
//!
//! Under the Service Control Manager the process has no console:
//! `println!` and the stdout `tracing` layer both write into a handle
//! nobody will ever read. `service run` therefore installs a rolling
//! file subscriber before it does anything else, so a service that fails
//! to start still leaves an explanation behind.
//!
//! Writes are synchronous rather than going through
//! `tracing_appender::non_blocking`. A metrics exporter logs a handful
//! of lines a minute, so the throughput argument for a background writer
//! does not apply, and the buffered variant can lose the last lines when
//! the process exits, which are exactly the lines that matter: the
//! energy WAL flush and the reason for a shutdown.

#![warn(dead_code)]

use std::path::PathBuf;

use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use super::scm;
use crate::common::paths;

/// Filter applied when `RUST_LOG` is unset.
///
/// `info` rather than the `debug` the foreground `all-smi api` defaults
/// to: this stream lands on a system volume and is retained for two
/// weeks, so it has to stay quiet by default. Set `RUST_LOG` in the
/// service's `Environment` registry value to raise it.
pub const DEFAULT_FILTER: &str = "all_smi=info,tower_http=warn";

/// `%PROGRAMDATA%\all-smi\logs`.
pub fn log_dir() -> PathBuf {
    paths::program_data_app_dir(&paths::program_data_root()).join(scm::LOG_DIR_NAME)
}

/// Install the rolling file subscriber.
///
/// Returns the directory the logs were opened in, or a description of
/// why they could not be. The caller reports the failure through the
/// SCM rather than aborting: a service that cannot write logs is
/// degraded, not broken, and refusing to export metrics because of it
/// would be the worse outcome.
pub fn init() -> Result<PathBuf, String> {
    let dir = log_dir();
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("could not create the log directory {}: {e}", dir.display()))?;

    let appender = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix(scm::LOG_FILE_PREFIX)
        .filename_suffix(scm::LOG_FILE_SUFFIX)
        .max_log_files(scm::LOG_RETENTION_FILES)
        .build(&dir)
        .map_err(|e| format!("could not open a log file in {}: {e}", dir.display()))?;

    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(DEFAULT_FILTER));

    tracing_subscriber::registry()
        .with(filter)
        .with(
            tracing_subscriber::fmt::layer()
                // A log file is not a terminal; escape sequences in it
                // would corrupt every downstream reader.
                .with_ansi(false)
                .with_writer(appender),
        )
        .try_init()
        .map_err(|e| format!("could not install the file logging subscriber: {e}"))?;

    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_log_directory_sits_under_the_program_data_app_dir() {
        let dir = log_dir();
        assert!(dir.ends_with(scm::LOG_DIR_NAME), "got {}", dir.display());
        assert!(
            dir.starts_with(paths::program_data_root()),
            "logs must live under %PROGRAMDATA%: {}",
            dir.display()
        );
    }

    #[test]
    fn the_default_filter_is_quieter_than_the_foreground_default() {
        // The foreground `all-smi api` default is `all_smi=debug`; two
        // weeks of that on a system volume is not acceptable.
        assert!(DEFAULT_FILTER.contains("all_smi=info"));
        assert!(!DEFAULT_FILTER.contains("debug"));
    }
}
