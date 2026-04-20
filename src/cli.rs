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

use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
pub struct Cli {
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
}

#[derive(Parser)]
pub struct ApiArgs {
    /// The port to listen on for the API server. Use 0 to disable TCP listener.
    #[arg(short, long, default_value_t = 9090)]
    pub port: u16,
    /// The interval in seconds at which to update the GPU information.
    #[arg(short, long, default_value_t = 3)]
    pub interval: u64,
    /// Include the process list in the API output.
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
