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

//! Human-readable and JSON renderers for the doctor [`Report`].

use std::io::Write;

use anyhow::{Context, Result};

use crate::doctor::redact::{RedactOptions, scrub};
use crate::doctor::{DoctorOptions, Report};

/// ANSI colour codes used when `opts.use_color` is true. Emitted with
/// raw `\x1b` escape sequences instead of the `crossterm` crate to keep
/// the doctor renderers callable from library contexts (integration
/// tests, future embedding) that don't initialise crossterm.
const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const RED: &str = "\x1b[31m";
const BLUE: &str = "\x1b[34m";

/// Render the human-readable report.
///
/// Width is not wrapped; `message` and `fix` lines use `textwrap` via
/// naive indent to keep the output predictable in CI logs. `redact` is
/// applied to both the message and the fix hint.
pub fn render_human<W: Write>(
    report: &Report,
    redact: &RedactOptions,
    opts: &DoctorOptions,
    mut w: W,
) -> Result<()> {
    let use_color = opts.use_color;

    let header = format!("all-smi doctor — {}", report.version);
    writeln!(w, "{}", colorize(&header, BOLD, use_color)).context("header write failed")?;
    writeln!(w, "Running {} checks…", report.checks.len()).context("count write failed")?;
    writeln!(w).context("blank line write failed")?;

    for outcome in &report.checks {
        let status = outcome.status;
        let (tag, colour) = match status {
            "pass" => ("PASS", GREEN),
            "warn" => ("WARN", YELLOW),
            "fail" => ("FAIL", RED),
            _ => ("SKIP", BLUE),
        };
        let tag_col = colorize(tag, colour, use_color);
        let id = format!("{:<32}", outcome.id);
        let message = scrub(&outcome.message, redact);
        writeln!(w, "{tag_col} {id} {message}").context("row write failed")?;
        if let Some(fix) = outcome.fix.as_deref() {
            let fix_scrub = scrub(fix, redact);
            let prefix = colorize("     -> Fix:", DIM, use_color);
            writeln!(w, "{prefix} {fix_scrub}").context("fix write failed")?;
        }
    }

    writeln!(w).context("blank line write failed")?;
    let summary = format!(
        "Summary: {} pass, {} warn, {} fail, {} skipped",
        report.summary.pass, report.summary.warn, report.summary.fail, report.summary.skip,
    );
    writeln!(w, "{}", colorize(&summary, BOLD, use_color)).context("summary write failed")?;
    writeln!(w, "Exit code: {}", report.summary.exit_code()).context("exit write failed")?;
    Ok(())
}

/// Render the JSON report. `redact` is applied to each outcome's message
/// and fix field.
pub fn render_json<W: Write>(report: &Report, redact: &RedactOptions, mut w: W) -> Result<()> {
    // Build a scrubbed clone so serde writes redacted strings without
    // mutating the original report.
    let scrubbed = Report {
        schema: report.schema,
        version: report.version.clone(),
        timestamp: report.timestamp.clone(),
        summary: report.summary,
        checks: report
            .checks
            .iter()
            .map(|o| crate::doctor::CheckOutcome {
                id: o.id.clone(),
                title: o.title.clone(),
                status: o.status,
                message: scrub(&o.message, redact),
                fix: o.fix.as_ref().map(|f| scrub(f, redact)),
                duration_ms: o.duration_ms,
            })
            .collect(),
    };
    let text =
        serde_json::to_string_pretty(&scrubbed).context("failed to serialize doctor report")?;
    writeln!(w, "{text}").context("json write failed")?;
    Ok(())
}

fn colorize(text: &str, code: &str, enabled: bool) -> String {
    if enabled {
        format!("{code}{text}{RESET}")
    } else {
        text.to_string()
    }
}

/// Render the human report into a `String` — used by the bundle writer so
/// the archived `report.txt` is identical to what the user saw on stdout,
/// minus any ANSI colour codes.
pub fn render_human_string(
    report: &Report,
    redact: &RedactOptions,
    opts: &DoctorOptions,
) -> Result<String> {
    // Force colour off for the archived copy.
    let mut plain_opts = opts.clone();
    plain_opts.use_color = false;
    let mut buf = Vec::new();
    render_human(report, redact, &plain_opts, &mut buf)?;
    String::from_utf8(buf).context("doctor report produced non-UTF8 bytes")
}

/// Render the JSON report into a `String` for bundling.
pub fn render_json_string(report: &Report, redact: &RedactOptions) -> Result<String> {
    let mut buf = Vec::new();
    render_json(report, redact, &mut buf)?;
    String::from_utf8(buf).context("doctor JSON produced non-UTF8 bytes")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doctor::{CheckOutcome, Summary};

    fn sample_report() -> Report {
        Report {
            schema: 1,
            version: "0.99.9".to_string(),
            timestamp: "2026-04-20T00:00:00Z".to_string(),
            summary: Summary {
                pass: 1,
                warn: 1,
                fail: 0,
                skip: 0,
            },
            checks: vec![
                CheckOutcome {
                    id: "platform.os".to_string(),
                    title: "OS".to_string(),
                    status: "pass",
                    message: "Linux 6.17.0".to_string(),
                    fix: None,
                    duration_ms: 12,
                },
                CheckOutcome {
                    id: "nvidia.nvml.loadable".to_string(),
                    title: "NVML".to_string(),
                    status: "warn",
                    message: "driver hung".to_string(),
                    fix: Some("reboot".to_string()),
                    duration_ms: 42,
                },
            ],
        }
    }

    #[test]
    fn json_output_has_schema_version_and_checks() {
        let redact = RedactOptions::passthrough();
        let s = render_json_string(&sample_report(), &redact).expect("render ok");
        assert!(s.contains("\"schema\": 1"));
        assert!(s.contains("\"platform.os\""));
        assert!(s.contains("\"status\": \"pass\""));
        assert!(s.contains("\"fix\": \"reboot\""));
    }

    #[test]
    fn human_output_contains_tag_and_fix_hint() {
        let redact = RedactOptions::passthrough();
        let opts = DoctorOptions {
            json: false,
            verbose: false,
            bundle_path: None,
            include_identifiers: true,
            remote_checks: vec![],
            skip: vec![],
            only: vec![],
            use_color: false,
        };
        let s = render_human_string(&sample_report(), &redact, &opts).expect("render ok");
        assert!(s.contains("PASS platform.os"));
        assert!(s.contains("WARN nvidia.nvml.loadable"));
        assert!(s.contains("-> Fix: reboot"));
        assert!(s.contains("Exit code: 1"));
    }
}
