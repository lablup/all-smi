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

//! Metric recording (`all-smi record`) and replay (`view --replay`).
//!
//! The record side captures one NDJSON frame per collection cycle using
//! the same serializer as the `snapshot` subcommand, plus an optional
//! header frame and sparse index frames every 1000 data frames.
//!
//! The replay side ([`replay::Replayer`]) streams those frames back so the
//! `view` TUI can reconstruct the exact `RenderSnapshot` the operator
//! would have seen live. Compression (`.zst`, `.gz`) is auto-detected from
//! the file extension.
//!
//! See issue #187 for the motivation and user-facing contract.

pub mod replay;
pub mod writer;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::json;

use crate::cli::{RecordArgs, RecordCompression, RecordSource, SnapshotIncludes};
use crate::common::config_file::RecordSettings;
use crate::snapshot::serializers::write_frame_json;
use crate::snapshot::{
    DefaultSnapshotCollector, SNAPSHOT_SCHEMA_VERSION, Snapshot, SnapshotCollector,
    SnapshotOptions, collect_once,
};

use writer::{Codec, RotatingWriter};

/// How many data frames between sparse index checkpoints. Match the
/// issue spec's value exactly — downstream consumers (e.g., external
/// tools that parse the NDJSON stream) rely on this spacing when they
/// walk index frames.
const INDEX_EVERY_N_FRAMES: u64 = 1000;

/// Resolved, validated options for a recording run.
pub struct RecorderOptions {
    pub output: PathBuf,
    pub interval: Duration,
    pub duration: Option<Duration>,
    pub source: RecordSource,
    pub hosts: Vec<String>,
    pub hostfile: Option<String>,
    pub includes: SnapshotIncludes,
    pub max_size: u64,
    pub max_files: u32,
    pub codec: Codec,
}

impl RecorderOptions {
    /// Convert parsed CLI args into orchestrator options. All human-facing
    /// errors are raised here so the `record` subcommand fails fast before
    /// opening any files. Thin wrapper over
    /// [`Self::from_args_with_settings`] with no config layer — kept
    /// for library consumers and tests.
    #[allow(dead_code)]
    pub fn from_args(args: &RecordArgs) -> Result<Self> {
        Self::from_args_with_settings(args, None)
    }

    /// Merged CLI + `[record]` config constructor. Precedence
    /// (highest → lowest):
    ///
    /// * CLI flag (`--output`, `--compress`)
    /// * Config file (`record.output_dir`, `record.compress`)
    /// * Compiled default (`./all-smi-record.ndjson.zst`, codec zstd)
    ///
    /// `record.output_dir` is treated as the *directory* the default
    /// basename is placed under — the operator can always pass `-o
    /// /some/file.ndjson` to bypass it. Tilde expansion is applied so
    /// `output_dir = "~/.cache/all-smi/records"` resolves to the
    /// user's home at record time.
    pub fn from_args_with_settings(
        args: &RecordArgs,
        settings: Option<&RecordSettings>,
    ) -> Result<Self> {
        let includes = args
            .includes()
            .map_err(|msg| anyhow!("invalid --include: {msg}"))?;
        if includes.is_empty() {
            bail!("at least one section must be requested via --include");
        }

        let duration = parse_duration(&args.duration).context("invalid --duration")?;
        let max_size = parse_byte_size(&args.max_size).context("invalid --max-size")?;
        if args.max_files == 0 {
            bail!("--max-files must be >= 1");
        }

        let hosts: Vec<String> = args.hosts.clone().unwrap_or_default();
        if args.source == RecordSource::Remote && hosts.is_empty() && args.hostfile.is_none() {
            bail!("--source=remote requires --hosts or --hostfile");
        }

        // `--compress` wins when set; otherwise the `[record]` config
        // chooses the codec. Without this the `record.compress` key
        // was documented but silently ignored — detection fell through
        // to the file extension.
        let compress_override: Option<RecordCompression> = args.compress.or_else(|| {
            settings.map(|s| s.compress.as_str()).and_then(|c| match c {
                "zstd" => Some(RecordCompression::Zstd),
                "gzip" => Some(RecordCompression::Gzip),
                "none" => Some(RecordCompression::None),
                _ => None, // apply_file_record already validated this
            })
        });

        // Resolve the output path. clap hands us a default basename; if
        // the operator did not pass `-o` AND the config supplied an
        // `output_dir`, redirect the default basename under that dir.
        // Detecting "operator did not pass -o" relies on the clap
        // default being exactly the documented value — if that ever
        // changes, update this guard.
        let default_basename: std::path::PathBuf =
            std::path::PathBuf::from("all-smi-record.ndjson.zst");
        let output = if args.output == default_basename
            && let Some(dir) = settings.map(|s| s.output_dir.as_str())
            && !dir.is_empty()
        {
            crate::common::paths::expand_tilde(std::path::Path::new(dir)).join(&default_basename)
        } else {
            args.output.clone()
        };

        let codec = Codec::detect(&output, compress_override);
        // Sanity-check extension / codec combinations. We accept
        // mismatches (operator override wins) but warn so the file does
        // not end up unreadable-by-default.
        if compress_override.is_some()
            && let Some(warn) = codec_extension_mismatch(&output, compress_override)
        {
            eprintln!("warning: {warn}");
        }

        Ok(Self {
            output,
            interval: Duration::from_secs(args.interval.max(1)),
            duration,
            source: args.source,
            hosts,
            hostfile: args.hostfile.clone(),
            includes,
            max_size,
            max_files: args.max_files,
            codec,
        })
    }
}

/// Entry point for the `record` subcommand.
pub async fn run(opts: RecorderOptions) -> Result<()> {
    let stop = Arc::new(AtomicBool::new(false));
    install_signal_handlers(stop.clone());

    match opts.source {
        RecordSource::Local => run_local(opts, stop).await,
        RecordSource::Remote => run_remote(opts, stop).await,
    }
}

async fn run_local(opts: RecorderOptions, stop: Arc<AtomicBool>) -> Result<()> {
    let collector = Arc::new(DefaultSnapshotCollector::new());
    let hosts = vec![collector.hostname()];

    let mut writer =
        RotatingWriter::new(&opts.output, opts.codec, opts.max_size, opts.max_files)
            .with_context(|| format!("failed to open recording file {}", opts.output.display()))?;

    write_header(&mut writer, &opts, &hosts)?;
    record_loop(&mut writer, &opts, stop, move |includes, timeout| {
        // Clone both into the async move so the returned future is
        // `'static` and not tied to the short-lived `&SnapshotIncludes`
        // reference the closure is invoked with.
        let c = collector.clone();
        let inc = *includes;
        async move { collect_once(c, &inc, timeout).await }
    })
    .await?;
    writer.finish().context("failed to finalize recording")?;
    Ok(())
}

async fn run_remote(opts: RecorderOptions, stop: Arc<AtomicBool>) -> Result<()> {
    use crate::view::data_collection::{
        CollectionConfig, DataCollectionStrategy, RemoteCollectorBuilder,
    };

    let mut builder = RemoteCollectorBuilder::new().with_hosts(opts.hosts.clone());
    if let Some(file) = opts.hostfile.as_deref() {
        builder = builder
            .load_hosts_from_file(file)
            .with_context(|| format!("failed to load hostfile {file}"))?;
    }
    let collector = builder.build();

    let mut writer =
        RotatingWriter::new(&opts.output, opts.codec, opts.max_size, opts.max_files)
            .with_context(|| format!("failed to open recording file {}", opts.output.display()))?;

    // In remote mode the header `hosts` field carries the scrape targets
    // so the replay side can reconstruct the tab list even when no data
    // frames arrive.
    write_header(&mut writer, &opts, &opts.hosts)?;

    let start = std::time::Instant::now();
    let mut seq: u64 = 0;
    while !stop.load(Ordering::Relaxed) {
        if let Some(limit) = opts.duration
            && start.elapsed() >= limit
        {
            break;
        }
        let config = CollectionConfig {
            interval: opts.interval.as_secs(),
            first_iteration: false,
            hosts: opts.hosts.clone(),
        };
        match collector.collect(&config).await {
            Ok(data) => {
                let snap = snapshot_from_collection_data(&data, &opts);
                write_data_frame(&mut writer, &snap, seq)?;
                seq += 1;
            }
            Err(e) => {
                tracing::warn!(error = %e, "record: remote scrape failed");
            }
        }
        sleep_until_next(&opts, start, seq, &stop).await;
    }
    writer.finish().context("failed to finalize recording")?;
    Ok(())
}

async fn record_loop<F, Fut>(
    writer: &mut RotatingWriter,
    opts: &RecorderOptions,
    stop: Arc<AtomicBool>,
    mut collect: F,
) -> Result<()>
where
    F: FnMut(&SnapshotIncludes, Duration) -> Fut,
    Fut: std::future::Future<Output = Snapshot>,
{
    let start = std::time::Instant::now();
    let mut seq: u64 = 0;
    // The snapshot collector runs every reader under a 5s timeout by
    // default; mirror that ceiling so a hung NVML call cannot block the
    // recorder indefinitely. Operators can extend the interval past 5s
    // safely; the timeout is per-reader, not per-cycle.
    let reader_timeout = Duration::from_millis(5_000);

    while !stop.load(Ordering::Relaxed) {
        if let Some(limit) = opts.duration
            && start.elapsed() >= limit
        {
            break;
        }
        let snap = collect(&opts.includes, reader_timeout).await;
        write_data_frame(writer, &snap, seq)?;
        seq += 1;
        sleep_until_next(opts, start, seq, &stop).await;
    }
    Ok(())
}

/// Sleep until the next tick boundary (fixed-interval recording) or
/// break early if SIGTERM landed.
async fn sleep_until_next(
    opts: &RecorderOptions,
    start: std::time::Instant,
    seq: u64,
    stop: &Arc<AtomicBool>,
) {
    let target = start + opts.interval.saturating_mul(seq as u32);
    let now = std::time::Instant::now();
    if target > now {
        let remaining = target.duration_since(now);
        // Wake early on stop so SIGTERM responsiveness is `interval / 10`
        // rather than a full cycle. We poll the atomic at 100ms
        // granularity — plenty fast for human-observable shutdown.
        let mut slept = Duration::ZERO;
        let step = Duration::from_millis(100);
        while slept < remaining && !stop.load(Ordering::Relaxed) {
            let chunk = step.min(remaining - slept);
            tokio::time::sleep(chunk).await;
            slept += chunk;
        }
    }
}

fn write_header(
    writer: &mut RotatingWriter,
    opts: &RecorderOptions,
    hosts: &[String],
) -> Result<()> {
    let header = json!({
        "schema": SNAPSHOT_SCHEMA_VERSION,
        "header": true,
        "interval_ms": opts.interval.as_millis() as u64,
        "hosts": hosts,
        "all_smi_version": env!("CARGO_PKG_VERSION"),
    });
    let mut line = serde_json::to_string(&header).context("failed to serialize header frame")?;
    line.push('\n');
    writer.write_line(line.as_bytes())?;
    Ok(())
}

fn write_data_frame(writer: &mut RotatingWriter, snapshot: &Snapshot, seq: u64) -> Result<()> {
    // Re-use the shared frame writer through an intermediate buffer. A
    // direct `write_frame_json(writer, snapshot)` would bypass the
    // rotation byte counter, so we buffer the line first and push it
    // through `write_line` which tracks size.
    let mut buf: Vec<u8> = Vec::with_capacity(4096);
    write_frame_json(&mut buf, snapshot).context("failed to serialize data frame")?;
    writer.write_line(&buf)?;

    if seq > 0 && seq.is_multiple_of(INDEX_EVERY_N_FRAMES) {
        let idx = json!({
            "schema": SNAPSHOT_SCHEMA_VERSION,
            "index": true,
            "seq": seq,
            "byte_offset": writer.active_bytes(),
        });
        let mut line = serde_json::to_string(&idx).context("failed to serialize index frame")?;
        line.push('\n');
        writer.write_line(line.as_bytes())?;
    }
    Ok(())
}

/// Build a `Snapshot` from the remote collector's aggregated view. Only
/// the sections the operator asked for via `--include` are populated,
/// matching the `snapshot` subcommand's behaviour.
fn snapshot_from_collection_data(
    data: &crate::view::data_collection::strategy::CollectionData,
    opts: &RecorderOptions,
) -> Snapshot {
    let mut snap = Snapshot {
        schema: SNAPSHOT_SCHEMA_VERSION,
        timestamp: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        hostname: "remote".to_string(),
        gpus: None,
        cpus: None,
        memory: None,
        chassis: None,
        processes: None,
        storage: None,
        errors: Vec::new(),
    };
    if opts.includes.gpu {
        snap.gpus = Some(data.gpu_info.clone());
    }
    if opts.includes.cpu {
        snap.cpus = Some(data.cpu_info.clone());
    }
    if opts.includes.memory {
        snap.memory = Some(data.memory_info.clone());
    }
    if opts.includes.chassis {
        snap.chassis = Some(data.chassis_info.clone());
    }
    if opts.includes.process {
        snap.processes = Some(data.process_info.clone());
    }
    snap
}

/// Register signal handlers to cleanly terminate the recorder on
/// SIGTERM / SIGINT (Ctrl-C). The flag is polled in the record loop so
/// we can finish the in-flight frame, flush the encoder, and close the
/// file before exiting.
fn install_signal_handlers(stop: Arc<AtomicBool>) {
    {
        let stop = stop.clone();
        tokio::spawn(async move {
            let _ = tokio::signal::ctrl_c().await;
            stop.store(true, Ordering::Relaxed);
        });
    }
    #[cfg(unix)]
    {
        let stop = stop.clone();
        tokio::spawn(async move {
            if let Ok(mut sig) =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            {
                sig.recv().await;
                stop.store(true, Ordering::Relaxed);
            }
        });
    }
}

/// Parse a human-readable duration string (`"0"`, `"30s"`, `"5m"`,
/// `"1h"`, `"2d"`). Bare integers are treated as seconds.
/// `"0"` means "record until signal".
fn parse_duration(s: &str) -> Result<Option<Duration>> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if trimmed == "0" {
        return Ok(None);
    }
    let (num_part, unit_secs): (&str, u64) = if let Some(stripped) = trimmed.strip_suffix('s') {
        (stripped, 1)
    } else if let Some(stripped) = trimmed.strip_suffix('m') {
        (stripped, 60)
    } else if let Some(stripped) = trimmed.strip_suffix('h') {
        (stripped, 3_600)
    } else if let Some(stripped) = trimmed.strip_suffix('d') {
        (stripped, 86_400)
    } else {
        (trimmed, 1)
    };
    let n: u64 = num_part
        .parse()
        .map_err(|_| anyhow!("expected integer, got `{trimmed}`"))?;
    Ok(Some(Duration::from_secs(n.saturating_mul(unit_secs))))
}

/// Parse a size suffix (`"1K"`, `"10M"`, `"2G"`). Bare integers are
/// bytes. `0` disables rotation.
fn parse_byte_size(s: &str) -> Result<u64> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Ok(0);
    }
    let (num_part, mul): (&str, u64) = if let Some(stripped) = trimmed.strip_suffix('K') {
        (stripped, 1 << 10)
    } else if let Some(stripped) = trimmed.strip_suffix('M') {
        (stripped, 1 << 20)
    } else if let Some(stripped) = trimmed.strip_suffix('G') {
        (stripped, 1 << 30)
    } else {
        (trimmed, 1)
    };
    let n: u64 = num_part
        .parse()
        .map_err(|_| anyhow!("expected integer, got `{trimmed}`"))?;
    Ok(n.saturating_mul(mul))
}

fn codec_extension_mismatch(path: &Path, compress: Option<RecordCompression>) -> Option<String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());
    let ext_codec = match ext.as_deref() {
        Some("zst") => Some("zstd"),
        Some("gz") => Some("gzip"),
        _ => Some("plain"),
    };
    let forced = match compress? {
        RecordCompression::Zstd => "zstd",
        RecordCompression::Gzip => "gzip",
        RecordCompression::None => "plain",
    };
    match (ext_codec, forced) {
        (Some(e), f) if e != f => Some(format!(
            "--compress={f} overrides file extension `.{}` which suggests {e}",
            ext.unwrap_or_default()
        )),
        _ => None,
    }
}

/// Allow the CLI layer to re-expose `SnapshotOptions` via this module for
/// discovery, without making it a required re-export.
#[allow(dead_code)]
fn _type_check_snapshot_reexport(_: SnapshotOptions) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_duration_accepts_suffixes() {
        assert_eq!(parse_duration("0").unwrap(), None);
        assert_eq!(parse_duration("30").unwrap(), Some(Duration::from_secs(30)));
        assert_eq!(
            parse_duration("30s").unwrap(),
            Some(Duration::from_secs(30))
        );
        assert_eq!(
            parse_duration("5m").unwrap(),
            Some(Duration::from_secs(300))
        );
        assert_eq!(
            parse_duration("1h").unwrap(),
            Some(Duration::from_secs(3_600))
        );
        assert_eq!(
            parse_duration("2d").unwrap(),
            Some(Duration::from_secs(172_800))
        );
    }

    #[test]
    fn parse_duration_rejects_junk() {
        assert!(parse_duration("abc").is_err());
        assert!(parse_duration("5x").is_err());
    }

    #[test]
    fn parse_byte_size_accepts_suffixes() {
        assert_eq!(parse_byte_size("0").unwrap(), 0);
        assert_eq!(parse_byte_size("1024").unwrap(), 1024);
        assert_eq!(parse_byte_size("1K").unwrap(), 1024);
        assert_eq!(parse_byte_size("1M").unwrap(), 1 << 20);
        assert_eq!(parse_byte_size("2G").unwrap(), 2 << 30);
    }
}
