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
use crate::common::config_file::SnapshotSettings;
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
    ///
    /// Thin wrapper over [`Self::from_args_with_settings`] with no
    /// config layer — kept as the legacy entry point for library
    /// consumers and tests that never need the `[snapshot]` section.
    #[allow(dead_code)]
    pub fn from_args(args: &SnapshotArgs) -> Result<Self> {
        Self::from_args_with_settings(args, None)
    }

    /// Construct options merging CLI args with `[snapshot]` config
    /// defaults. Precedence (highest → lowest):
    ///
    /// * CLI flag (`--format`, `--pretty`)
    /// * Config file `[snapshot].default_format` /
    ///   `[snapshot].default_pretty`
    /// * Compiled defaults (json, auto-TTY pretty)
    ///
    /// Called from `main.rs` once the merged [`SnapshotSettings`] are
    /// available. The legacy [`Self::from_args`] wrapper feeds `None`
    /// for callers (tests, library consumers) that do not care about
    /// the config layer.
    pub fn from_args_with_settings(
        args: &SnapshotArgs,
        settings: Option<&SnapshotSettings>,
    ) -> Result<Self> {
        let includes = args
            .includes()
            .map_err(|msg| anyhow::anyhow!("invalid --include: {msg}"))?;
        if includes.is_empty() {
            anyhow::bail!("at least one section must be requested via --include");
        }
        if args.samples == 0 {
            anyhow::bail!("--samples must be >= 1");
        }

        // clap gives `args.format` a hardcoded `default_value_t =
        // SnapshotFormat::Json`. That makes it impossible to tell "no
        // flag given" apart from "--format=json" at parse time. We
        // apply the config-file value only when the CLI chose the
        // compiled default AND the config file has a different value,
        // matching operator expectations ("my config says csv; one-
        // shot runs should honour that").
        let format = match settings.map(|s| s.default_format.as_str()) {
            Some("csv") if args.format == SnapshotFormat::Json => SnapshotFormat::Csv,
            Some("prometheus") if args.format == SnapshotFormat::Json => SnapshotFormat::Prometheus,
            Some("json") | Some(_) | None => args.format,
        };

        // `--pretty` is already an `Option<bool>` so we can distinguish
        // "unset" from "explicitly true/false" — only fall back to the
        // config when the operator did not pass the flag.
        let pretty = args.pretty.or_else(|| settings.map(|s| s.default_pretty));

        Ok(Self {
            format,
            pretty,
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
///
/// `Deserialize` is derived so the `view --replay` subcommand (issue #187)
/// can reconstruct snapshots from the NDJSON recording stream produced by
/// `all-smi record`. All section fields use `#[serde(default)]` so frames
/// that omit a section (per the "absent key" serializer rule) still parse.
#[derive(Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub schema: u32,
    pub timestamp: String,
    pub hostname: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpus: Option<Vec<GpuInfo>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpus: Option<Vec<CpuInfo>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<Vec<MemoryInfo>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chassis: Option<Vec<ChassisInfo>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub processes: Option<Vec<ProcessInfo>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage: Option<Vec<StorageInfo>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
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

    /// `[snapshot].default_format = "csv"` must take effect when the
    /// operator does not pass `--format`. Without the merge the TOML
    /// key was documented-but-ignored. A CLI `--format` value wins
    /// back; we cannot test that case against clap's default here
    /// because the compiled default is `Json`, but the precedence
    /// direction is covered by the explicit-CLI branch below.
    #[test]
    fn snapshot_options_from_settings_uses_config_format() {
        use crate::cli::SnapshotArgs;
        use crate::common::config_file::SnapshotSettings;

        let args = SnapshotArgs {
            format: SnapshotFormat::Json,
            pretty: None,
            include: vec!["gpu".to_string()],
            query: Vec::new(),
            samples: 1,
            interval: 0,
            timeout_ms: 5_000,
            output: None,
        };
        let settings = SnapshotSettings {
            default_format: "csv".to_string(),
            default_pretty: false,
        };
        let opts = SnapshotOptions::from_args_with_settings(&args, Some(&settings)).unwrap();
        assert_eq!(opts.format, SnapshotFormat::Csv);
        // `args.pretty` was `None` so the config `default_pretty` wins.
        assert_eq!(opts.pretty, Some(false));
    }

    /// Explicit `--pretty=true` on the CLI overrides a config
    /// `default_pretty = false`. Verifies the documented precedence
    /// (CLI > config) at the merge boundary.
    #[test]
    fn snapshot_options_cli_pretty_overrides_config() {
        use crate::cli::SnapshotArgs;
        use crate::common::config_file::SnapshotSettings;

        let args = SnapshotArgs {
            format: SnapshotFormat::Json,
            pretty: Some(true),
            include: vec!["gpu".to_string()],
            query: Vec::new(),
            samples: 1,
            interval: 0,
            timeout_ms: 5_000,
            output: None,
        };
        let settings = SnapshotSettings {
            default_format: "json".to_string(),
            default_pretty: false,
        };
        let opts = SnapshotOptions::from_args_with_settings(&args, Some(&settings)).unwrap();
        assert_eq!(opts.pretty, Some(true), "CLI must win over config");
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
