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

//! Configuration data types for the snapshot subcommand.
//!
//! Kept free of any I/O or orchestration logic so they can be re-exported
//! through [`crate::lib`] as part of the stable library API.

use std::time::Duration;

use anyhow::Result;
use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::cli::{SnapshotArgs, SnapshotFormat, SnapshotIncludes};
use crate::device::{ChassisInfo, CpuInfo, GpuInfo, MemoryInfo, ProcessInfo};
use crate::storage::info::StorageInfo;

/// The JSON schema version emitted by snapshot JSON output. Bumped whenever a
/// breaking field change lands; additive changes keep the same number.
pub const SNAPSHOT_SCHEMA_VERSION: u32 = 1;

/// Pure-data options for a snapshot run.
///
/// Equivalent to [`crate::cli::SnapshotArgs`] but without any clap
/// dependency so the library API stays usable in `no_cli` builds and from
/// embedding contexts that do not want to parse argv.
#[derive(Debug, Clone)]
pub struct SnapshotOptions {
    pub format: SnapshotFormat,
    /// `None` = auto (pretty when stdout is a TTY), `Some(b)` = force.
    pub pretty: Option<bool>,
    pub includes: SnapshotIncludes,
    pub query: Vec<String>,
    pub samples: u32,
    pub interval: Duration,
    pub timeout_per_reader: Duration,
    /// `None` = stdout, `Some(path)` = write to file (`"-"` also means stdout).
    pub output: Option<String>,
}

impl Default for SnapshotOptions {
    fn default() -> Self {
        Self {
            format: SnapshotFormat::Json,
            pretty: None,
            includes: SnapshotIncludes {
                gpu: true,
                cpu: true,
                memory: true,
                chassis: true,
                process: false,
                storage: false,
            },
            query: Vec::new(),
            samples: 1,
            interval: Duration::from_secs(0),
            timeout_per_reader: Duration::from_millis(5_000),
            output: None,
        }
    }
}

impl SnapshotOptions {
    /// Construct options from parsed CLI args.
    pub fn from_args(args: &SnapshotArgs) -> Result<Self> {
        let includes = args
            .includes()
            .map_err(|msg| anyhow::anyhow!("invalid --include: {msg}"))?;
        if includes.is_empty() {
            anyhow::bail!("at least one section must be requested via --include");
        }
        if args.samples == 0 {
            anyhow::bail!("--samples must be >= 1");
        }
        Ok(Self {
            format: args.format,
            pretty: args.pretty,
            includes,
            query: args.query.iter().map(|s| s.trim().to_string()).collect(),
            samples: args.samples,
            interval: Duration::from_secs(args.interval),
            timeout_per_reader: Duration::from_millis(args.timeout_ms),
            output: args.output.clone(),
        })
    }

    /// Whether to pretty-print JSON, resolving the auto-TTY rule when
    /// `pretty` was not explicitly set.
    pub fn effective_pretty(&self, stdout_is_tty: bool) -> bool {
        match self.pretty {
            Some(b) => b,
            None => stdout_is_tty,
        }
    }
}

/// A single reader-level failure surfaced in the snapshot output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotError {
    /// Short section identifier (`"gpu"`, `"cpu"`, `"memory"`,
    /// `"chassis"`, `"process"`, `"storage"`).
    pub section: String,
    /// Error kind: `"timeout"`, `"panic"`, or `"error"`.
    pub kind: String,
    pub message: String,
}

/// A fully collected one-shot snapshot of hardware state.
///
/// Fields are optional because only requested sections are populated — per
/// the spec, missing includes must be *absent* from the output rather than
/// rendered as empty arrays.
///
/// `Debug` is deliberately not derived: `ProcessInfo` in `crate::device`
/// does not implement `Debug`, and adding it to that type is out of scope
/// for this feature. Tests and logs needing a human rendering should serialize
/// to JSON instead via [`serde_json::to_string_pretty`].
#[derive(Clone, Serialize)]
pub struct Snapshot {
    pub schema: u32,
    pub timestamp: String,
    pub hostname: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gpus: Option<Vec<GpuInfo>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpus: Option<Vec<CpuInfo>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory: Option<Vec<MemoryInfo>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chassis: Option<Vec<ChassisInfo>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub processes: Option<Vec<ProcessInfo>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub storage: Option<Vec<StorageInfo>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<SnapshotError>,
}

impl Snapshot {
    pub(crate) fn new(hostname: String) -> Self {
        Self {
            schema: SNAPSHOT_SCHEMA_VERSION,
            timestamp: Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            hostname,
            gpus: None,
            cpus: None,
            memory: None,
            chassis: None,
            processes: None,
            storage: None,
            errors: Vec::new(),
        }
    }

    /// Number of devices collected across all populated sections. Used to
    /// detect "hard failure" = zero devices collected.
    pub fn device_count(&self) -> usize {
        self.gpus.as_ref().map_or(0, Vec::len)
            + self.cpus.as_ref().map_or(0, Vec::len)
            + self.memory.as_ref().map_or(0, Vec::len)
            + self.chassis.as_ref().map_or(0, Vec::len)
            + self.processes.as_ref().map_or(0, Vec::len)
            + self.storage.as_ref().map_or(0, Vec::len)
    }
}

/// Hard-failure marker attached to `anyhow` errors when no devices were
/// collected for any sample. `main.rs` distinguishes this from soft errors
/// so it can map it to exit code 1 specifically while keeping soft failures
/// (all sections returned, some with errors) at exit code 0.
#[derive(Debug)]
pub struct SnapshotHardFailure;

impl std::fmt::Display for SnapshotHardFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "no devices were collected from any reader — snapshot is empty"
        )
    }
}

impl std::error::Error for SnapshotHardFailure {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_options_defaults_to_gpu_cpu_memory_chassis() {
        let opts = SnapshotOptions::default();
        assert!(opts.includes.gpu);
        assert!(opts.includes.cpu);
        assert!(opts.includes.memory);
        assert!(opts.includes.chassis);
        assert!(!opts.includes.process);
        assert!(!opts.includes.storage);
    }

    #[test]
    fn effective_pretty_resolves_auto_tty() {
        let mut opts = SnapshotOptions::default();
        // Auto: on when TTY, off when pipe.
        assert!(opts.effective_pretty(true));
        assert!(!opts.effective_pretty(false));
        // Forced: override regardless of TTY state.
        opts.pretty = Some(false);
        assert!(!opts.effective_pretty(true));
        opts.pretty = Some(true);
        assert!(opts.effective_pretty(false));
    }

    #[test]
    fn snapshot_device_count_adds_across_sections() {
        let mut snap = Snapshot::new("host".to_string());
        assert_eq!(snap.device_count(), 0);
        snap.cpus = Some(vec![]);
        assert_eq!(snap.device_count(), 0);
        snap.storage = Some(vec![StorageInfo {
            mount_point: "/".to_string(),
            total_bytes: 1,
            available_bytes: 1,
            host_id: "h".to_string(),
            hostname: "h".to_string(),
            index: 0,
        }]);
        assert_eq!(snap.device_count(), 1);
    }
}
