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

use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

// Config subcommand argument types live in a sibling module so the main
// CLI file stays within the 500-line soft limit.
pub use crate::cli_config::{
    ConfigAction, ConfigArgs, ConfigPrintArgs, ConfigPrintFormat, ConfigValidateArgs,
};

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
pub struct Cli {
    /// Override the default TOML config file path. When omitted the loader
    /// probes the platform-appropriate locations (see `all-smi config init`
    /// for the canonical path). When given, a missing or malformed file
    /// produces a hard error; no silent fallback.
    #[arg(long, global = true, value_name = "PATH")]
    pub config: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Run in API mode, exposing metrics in Prometheus format.
    Api(ApiArgs),
    /// Run in local mode, monitoring local GPUs/NPUs. (default)
    Local(LocalArgs),
    /// Run in remote view mode, monitoring remote nodes via API endpoints.
    View(ViewArgs),
    /// Collect a one-shot machine-readable snapshot of hardware state.
    ///
    /// Emits JSON (default), CSV, or Prometheus exposition to stdout or a file.
    /// Intended for scripting, CI probes, Slurm prolog/epilog hooks, and quick
    /// `jq`/`yq` piping. Does not start a long-running server.
    Snapshot(SnapshotArgs),
    /// Record a live metric stream to an NDJSON file for later replay.
    ///
    /// Each collection cycle produces one JSON line whose shape matches the
    /// `snapshot` subcommand (same serializer). Subsequent `all-smi view
    /// --replay <file>` invocations reconstruct the exact TUI frames the
    /// operator would have seen live, making post-hoc incident investigation
    /// possible without a Prometheus retention store. See issue #187.
    Record(RecordArgs),
    /// Run self-diagnosis checks and optionally produce a support bundle.
    ///
    /// Emits a PASS/WARN/FAIL/SKIP report covering platform, privileges,
    /// container runtime, every supported hardware backend (NVIDIA, AMD,
    /// Apple, Gaudi, TPU, Tenstorrent, Rebellions, Furiosa, Windows), the
    /// relevant environment variables, and optional remote endpoint
    /// connectivity. Every check is read-only and bounded by a hard 3-second
    /// timeout. See issue #188.
    Doctor(DoctorArgs),
    /// Inspect, initialise, or validate the TOML configuration file
    /// (issue #192). See `all-smi config --help` for subcommands.
    Config(ConfigArgs),
}

#[derive(Parser, Clone)]
pub struct ApiArgs {
    /// The port to listen on for the API server. Use 0 to disable TCP listener.
    ///
    /// When omitted, value is taken from the config file, the
    /// `ALL_SMI_API_PORT` env var, or the compiled default (9090).
    #[arg(short, long)]
    pub port: Option<u16>,
    /// The interval in seconds at which to update the GPU information.
    ///
    /// When omitted, value is taken from the config file, the
    /// `ALL_SMI_API_INTERVAL_SECS` env var, or the compiled default (3).
    #[arg(short, long)]
    pub interval: Option<u64>,
    /// Include the process list in the API output.
    ///
    /// When omitted, value is taken from the config file or the
    /// `ALL_SMI_API_PROCESSES` env var.
    #[arg(long)]
    pub processes: bool,
    /// Unix domain socket path for local IPC (Unix only).
    /// When specified without a value, uses platform default:
    /// - Linux: /var/run/all-smi.sock (fallback to /tmp/all-smi.sock if no permission)
    /// - macOS: /tmp/all-smi.sock
    #[cfg(unix)]
    #[arg(short, long, num_args = 0..=1, default_missing_value = "")]
    pub socket: Option<String>,
}

#[derive(Parser, Clone)]
pub struct LocalArgs {
    /// The interval in seconds at which to update the GPU information.
    #[arg(short, long)]
    pub interval: Option<u64>,
    /// Temperature in Celsius at which GPUs trigger a `warn` alert.
    /// The matching `crit` threshold is auto-set 10°C higher unless an
    /// explicit config file overrides it.
    #[arg(long)]
    pub alert_temp: Option<u32>,
    /// Minutes of sustained idle utilization (below `util_idle_pct`) after
    /// which the alerter emits an `ok → warn` transition.
    #[arg(long)]
    pub alert_util_low_mins: Option<u32>,
}

#[derive(Parser, Clone)]
pub struct ViewArgs {
    /// A list of host addresses to connect to for remote monitoring.
    #[arg(long, num_args = 1..)]
    pub hosts: Option<Vec<String>>,
    /// A file containing a list of host addresses to connect to for remote monitoring.
    #[arg(long)]
    pub hostfile: Option<String>,
    /// The interval in seconds at which to update the GPU information. If not specified, uses adaptive interval based on node count.
    #[arg(short, long)]
    pub interval: Option<u64>,
    /// Temperature in Celsius at which GPUs trigger a `warn` alert.
    /// The matching `crit` threshold is auto-set 10°C higher unless an
    /// explicit config file overrides it.
    #[arg(long)]
    pub alert_temp: Option<u32>,
    /// Minutes of sustained idle utilization (below `util_idle_pct`) after
    /// which the alerter emits an `ok → warn` transition.
    #[arg(long)]
    pub alert_util_low_mins: Option<u32>,

    // --- Replay mode (issue #187) --------------------------------------
    /// Replay a recorded NDJSON stream instead of collecting live data.
    ///
    /// Accepts `.ndjson`, `.ndjson.zst`, `.ndjson.gz` — compression is
    /// auto-detected from the file extension. In replay mode `--hosts`
    /// and `--hostfile` are ignored; tabs and GPUs come entirely from the
    /// recorded frames. See issue #187.
    #[arg(long)]
    pub replay: Option<PathBuf>,
    /// Playback speed multiplier for `--replay`. Valid discrete values:
    /// 0.25, 0.5, 1.0 (default), 2.0, 4.0, 8.0. Other values are accepted
    /// at startup but the in-TUI `+`/`-` controls cycle through the
    /// discrete ladder.
    #[arg(long, default_value_t = 1.0)]
    pub speed: f32,
    /// Seek to the given timecode (`HH:MM:SS`, `MM:SS`, or bare seconds)
    /// before starting playback.
    #[arg(long)]
    pub start: Option<String>,
    /// Loop playback: when the last frame is reached, jump back to the
    /// first frame and continue. Renamed from `loop` to avoid the Rust
    /// keyword; the CLI surface remains `--loop`.
    #[arg(long = "loop")]
    pub replay_loop: bool,
}

/// Output format for the `snapshot` subcommand.
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum SnapshotFormat {
    /// JSON object (or JSON array when `--samples > 1`).
    Json,
    /// Flat CSV with a header row.
    Csv,
    /// Prometheus exposition format.
    ///
    /// MUST match byte-for-byte the output of the `api` subcommand's
    /// `/metrics` endpoint for the same collection cycle.
    Prometheus,
}

impl std::fmt::Display for SnapshotFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Json => write!(f, "json"),
            Self::Csv => write!(f, "csv"),
            Self::Prometheus => write!(f, "prometheus"),
        }
    }
}

#[derive(Parser, Clone)]
pub struct SnapshotArgs {
    /// Output format.
    #[arg(long, value_enum, default_value_t = SnapshotFormat::Json)]
    pub format: SnapshotFormat,

    /// Pretty-print JSON output. Auto-off when stdout is not a TTY.
    ///
    /// Use `--pretty=false` to force compact output; use `--pretty=true` to
    /// force pretty output even when piping.
    #[arg(long, num_args = 0..=1, default_missing_value = "true")]
    pub pretty: Option<bool>,

    /// Sections to include in the output. Comma-separated.
    ///
    /// Valid values: `gpu`, `cpu`, `memory`, `chassis`, `process`, `storage`.
    /// `process` and `storage` are opt-in because they are expensive.
    #[arg(long, value_delimiter = ',', default_value = "gpu,cpu,memory,chassis")]
    pub include: Vec<String>,

    /// Comma-separated dot-path fields to select for CSV column layout.
    ///
    /// When omitted, CSV output uses a sensible default per included section.
    /// Dot paths are resolved against the device's JSON representation; missing
    /// paths yield empty cells rather than errors. Example:
    /// `--query index,name,utilization,memory.used,memory.total`.
    #[arg(long, value_delimiter = ',')]
    pub query: Vec<String>,

    /// Collect multiple samples spaced `--interval` seconds apart.
    #[arg(long, default_value_t = 1)]
    pub samples: u32,

    /// Seconds between samples. Requires `--samples > 1` to have any effect.
    #[arg(long, default_value_t = 0)]
    pub interval: u64,

    /// Per-reader timeout in milliseconds.
    ///
    /// Slow readers (TPU, Gaudi) that exceed this budget are recorded in the
    /// top-level `errors` array (JSON) / `errors` column (CSV) / stderr
    /// (Prometheus) instead of hanging the process.
    #[arg(long, default_value_t = 5_000)]
    pub timeout_ms: u64,

    /// Write output to this file instead of stdout. Use `-` for stdout.
    ///
    /// On Unix the file is created with mode `0o600` (owner-only) and the
    /// writer refuses to follow symlinks; the command fails if the target
    /// path already exists as a symlink. The write is atomic: output first
    /// goes to a sibling `<path>.tmp` file, is fsynced, then renamed over
    /// the destination. On Windows the file is opened with exclusive
    /// sharing; symlink-based TOCTOU has different mitigations on that
    /// platform which are out of scope for this flag.
    #[arg(long, short)]
    pub output: Option<String>,
}

impl SnapshotArgs {
    /// Parse and normalise `--include` into a [`SnapshotIncludes`] flag set.
    ///
    /// Unknown section names produce an error with a descriptive message —
    /// clap reports this as a runtime error rather than a flag parse error,
    /// so the caller can surface it through the standard `anyhow` chain.
    pub fn includes(&self) -> Result<SnapshotIncludes, String> {
        let mut set = SnapshotIncludes::default();
        for raw in &self.include {
            let name = raw.trim().to_ascii_lowercase();
            match name.as_str() {
                "" => continue,
                "gpu" => set.gpu = true,
                "cpu" => set.cpu = true,
                "memory" => set.memory = true,
                "chassis" => set.chassis = true,
                "process" | "processes" => set.process = true,
                "storage" | "disk" => set.storage = true,
                other => {
                    return Err(format!(
                        "unknown --include section `{other}` (valid: gpu, cpu, memory, chassis, process, storage)"
                    ));
                }
            }
        }
        Ok(set)
    }
}

/// Which sections are requested for the snapshot.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct SnapshotIncludes {
    pub gpu: bool,
    pub cpu: bool,
    pub memory: bool,
    pub chassis: bool,
    pub process: bool,
    pub storage: bool,
}

impl SnapshotIncludes {
    /// Returns `true` if no section was requested.
    pub fn is_empty(&self) -> bool {
        !(self.gpu || self.cpu || self.memory || self.chassis || self.process || self.storage)
    }
}

/// Source of live data for the `record` subcommand.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum RecordSource {
    /// Collect from the local hardware readers (default).
    #[default]
    Local,
    /// Scrape remote `/metrics` endpoints — requires `--hosts` or
    /// `--hostfile`. Uses the same HTTP path as `view`.
    Remote,
}

impl std::fmt::Display for RecordSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Local => write!(f, "local"),
            Self::Remote => write!(f, "remote"),
        }
    }
}

/// Explicit compression codec for `record --compress`.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum RecordCompression {
    /// zstd — default on recent platforms, small + fast.
    #[default]
    Zstd,
    /// gzip — universally available, slightly larger output.
    Gzip,
    /// No compression (pure NDJSON).
    None,
}

impl std::fmt::Display for RecordCompression {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Zstd => write!(f, "zstd"),
            Self::Gzip => write!(f, "gzip"),
            Self::None => write!(f, "none"),
        }
    }
}

#[derive(Parser, Clone, Debug)]
pub struct RecordArgs {
    /// Output path for the NDJSON stream.
    ///
    /// Extension drives compression: `.ndjson.zst` → zstd,
    /// `.ndjson.gz` → gzip, anything else → plain NDJSON. Use
    /// `--compress` to override detection.
    #[arg(long, short = 'o', default_value = "all-smi-record.ndjson.zst")]
    pub output: PathBuf,

    /// Interval in seconds between frames.
    #[arg(long, short = 'i', default_value_t = 3)]
    pub interval: u64,

    /// How long to record. `0` (or negative via env overrides) means
    /// record until `SIGTERM`/`SIGINT`. Accepts a bare integer (seconds)
    /// or a suffixed form like `30s`, `5m`, `1h`, `1d`.
    #[arg(long, default_value = "0")]
    pub duration: String,

    /// Live data source.
    #[arg(long, value_enum, default_value_t = RecordSource::Local)]
    pub source: RecordSource,

    /// Remote hosts to scrape when `--source=remote`.
    #[arg(long, num_args = 1..)]
    pub hosts: Option<Vec<String>>,

    /// File containing remote hosts (one per line) when `--source=remote`.
    #[arg(long)]
    pub hostfile: Option<String>,

    /// Sections to include in each frame. Comma-separated.
    /// Valid values: `gpu`, `cpu`, `memory`, `chassis`, `process`.
    #[arg(long, value_delimiter = ',', default_value = "gpu,cpu,memory,chassis")]
    pub include: Vec<String>,

    /// Rotation threshold for the active segment. Accepts a size in bytes
    /// (`1048576`) or with suffix (`1K`, `10M`, `2G`). `0` disables
    /// rotation. Matches the issue spec: `--max-size 1K --max-files 3`
    /// caps total on-disk footprint to three segments.
    #[arg(long, default_value = "100M")]
    pub max_size: String,

    /// Maximum number of rotated segments to keep (including the active
    /// one). Oldest segment is evicted when the limit is reached.
    #[arg(long, default_value_t = 10)]
    pub max_files: u32,

    /// Override compression codec selected by file extension.
    #[arg(long, value_enum)]
    pub compress: Option<RecordCompression>,
}

impl RecordArgs {
    /// Parse `--include` into a [`SnapshotIncludes`] flag set. Shares
    /// exactly the same semantics as [`SnapshotArgs::includes`] so the
    /// `record` and `snapshot` subcommands stay in sync.
    pub fn includes(&self) -> Result<SnapshotIncludes, String> {
        let mut set = SnapshotIncludes::default();
        for raw in &self.include {
            let name = raw.trim().to_ascii_lowercase();
            match name.as_str() {
                "" => continue,
                "gpu" => set.gpu = true,
                "cpu" => set.cpu = true,
                "memory" => set.memory = true,
                "chassis" => set.chassis = true,
                "process" | "processes" => set.process = true,
                other => {
                    return Err(format!(
                        "unknown --include section `{other}` (valid: gpu, cpu, memory, chassis, process)"
                    ));
                }
            }
        }
        Ok(set)
    }
}

/// Arguments for the `doctor` subcommand (issue #188).
///
/// The flag surface intentionally mirrors the issue spec so scripts and
/// support templates can depend on stable names across versions.
#[derive(Parser, Clone, Debug)]
pub struct DoctorArgs {
    /// Emit a machine-readable JSON report instead of the human-readable
    /// text output. The JSON schema is versioned via a top-level `schema`
    /// field; see `src/doctor/report.rs` for the current version.
    #[arg(long)]
    pub json: bool,

    /// Include extra diagnostic detail per check (slow `system_profiler`
    /// on macOS, verbose environment dump in the bundle, etc.).
    #[arg(long)]
    pub verbose: bool,

    /// Write a tar.gz support bundle to this path. The archive contains
    /// `report.txt`, `report.json`, and applicable system context files
    /// (env, uname, lspci, lsmod, dmesg-gpu, version, and macOS-only
    /// `system_profiler SPDisplaysDataType` when `--verbose` is set).
    #[arg(long, value_name = "PATH")]
    pub bundle: Option<PathBuf>,

    /// By default the report and bundle scrub hostnames, IP addresses,
    /// MAC addresses, and local usernames so the output is safe to attach
    /// to public issues. Set this flag to preserve identifiers verbatim.
    #[arg(long)]
    pub include_identifiers: bool,

    /// Opt-in remote endpoints to probe (host, host:port, or a full URL).
    /// Each argument triggers DNS resolution, TCP reachability, latency
    /// measurement, and an HTTP GET against `/metrics` when a URL is
    /// given. May be passed multiple times.
    #[arg(long = "remote-check", value_name = "HOST_OR_URL", num_args = 1..)]
    pub remote_check: Vec<String>,

    /// Skip checks whose ID starts with any of these prefixes. Example:
    /// `--skip nvidia` omits every `nvidia.*` check; `--skip
    /// nvidia.nvml.loadable` omits only that specific one. Comma-separated
    /// values are accepted.
    #[arg(long, value_name = "CHECK_ID", value_delimiter = ',')]
    pub skip: Vec<String>,

    /// Run only checks whose ID starts with any of these prefixes. Takes
    /// precedence over `--skip`. Example: `--only privileges` runs only
    /// the privileges group. Comma-separated values are accepted.
    #[arg(long, value_name = "CHECK_ID", value_delimiter = ',')]
    pub only: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_includes_is_empty_when_all_false() {
        let inc = SnapshotIncludes::default();
        assert!(inc.is_empty());
    }

    #[test]
    fn snapshot_includes_not_empty_when_one_set() {
        let inc = SnapshotIncludes {
            gpu: true,
            ..Default::default()
        };
        assert!(!inc.is_empty());
    }

    #[test]
    fn snapshot_args_includes_rejects_unknown_section() {
        let args = SnapshotArgs {
            format: SnapshotFormat::Json,
            pretty: None,
            include: vec!["gpu".to_string(), "unknown_section".to_string()],
            query: Vec::new(),
            samples: 1,
            interval: 0,
            timeout_ms: 5_000,
            output: None,
        };
        let result = args.includes();
        assert!(result.is_err());
        let msg = result.unwrap_err();
        assert!(
            msg.contains("unknown_section"),
            "error must name the unknown section, got: {msg}"
        );
    }

    #[test]
    fn snapshot_args_includes_accepts_process_alias() {
        // Both "process" and "processes" are valid include names.
        let args = SnapshotArgs {
            format: SnapshotFormat::Json,
            pretty: None,
            include: vec!["processes".to_string(), "disk".to_string()],
            query: Vec::new(),
            samples: 1,
            interval: 0,
            timeout_ms: 5_000,
            output: None,
        };
        let result = args
            .includes()
            .expect("process/disk aliases should be accepted");
        assert!(result.process);
        assert!(result.storage);
    }

    #[test]
    fn snapshot_format_display() {
        assert_eq!(SnapshotFormat::Json.to_string(), "json");
        assert_eq!(SnapshotFormat::Csv.to_string(), "csv");
        assert_eq!(SnapshotFormat::Prometheus.to_string(), "prometheus");
    }
}
