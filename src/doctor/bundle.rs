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

//! Support-bundle packer — writes a tar.gz containing the rendered
//! report plus a curated set of system context files.

use std::fs::File;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use flate2::Compression;
use flate2::write::GzEncoder;

use crate::doctor::exec::try_exec;
use crate::doctor::redact::{RedactOptions, scrub};
use crate::doctor::report::{render_human_string, render_json_string};
use crate::doctor::{DoctorOptions, Report};

/// Build the support bundle at `path`. The archive layout is:
///
/// ```text
/// all-smi-doctor/
/// +-- report.txt         (human-readable)
/// +-- report.json        (machine-readable)
/// +-- env.txt            (filtered env vars, redacted)
/// +-- uname.txt          (Unix only)
/// +-- lspci.txt          (Linux only, GPU/accel keyword filter)
/// +-- lsmod.txt          (Linux only)
/// +-- dmesg-gpu.txt      (Linux only, last 200 GPU-keyword lines)
/// +-- version.txt        (package name+version+features+target)
/// +-- system_profiler_display.txt   (macOS only, --verbose only)
/// ```
pub fn write_bundle(path: &Path, report: &Report, opts: &DoctorOptions) -> Result<()> {
    let redact = opts.redact_options();

    // Compose the archive in-memory first so we can include derived pieces
    // (like the short-form version.txt that references the other files).
    let entries = collect_entries(report, opts, &redact)?;

    // Wrap the file in a gzip encoder feeding a tar builder. Both layers
    // are buffered; we only need one `finish()` per wrapper.
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create bundle parent {parent:?}"))?;
    }
    let f = File::create(path).with_context(|| format!("failed to create bundle file {path:?}"))?;
    let gz = GzEncoder::new(f, Compression::default());
    let mut tar = tar::Builder::new(gz);

    for (name, bytes) in &entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_mtime(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        );
        header.set_cksum();
        tar.append_data(&mut header, name, bytes.as_slice())
            .with_context(|| format!("failed to append {name} to bundle"))?;
    }

    let gz = tar.into_inner().context("failed to finalise tar stream")?;
    gz.finish().context("failed to finalise gzip stream")?;
    Ok(())
}

fn collect_entries(
    report: &Report,
    opts: &DoctorOptions,
    redact: &RedactOptions,
) -> Result<Vec<(String, Vec<u8>)>> {
    let mut out: Vec<(String, Vec<u8>)> = vec![
        (
            "all-smi-doctor/report.txt".to_string(),
            render_human_string(report, redact, opts)?.into_bytes(),
        ),
        (
            "all-smi-doctor/report.json".to_string(),
            render_json_string(report, redact)?.into_bytes(),
        ),
        (
            "all-smi-doctor/env.txt".to_string(),
            env_dump(redact).into_bytes(),
        ),
        (
            "all-smi-doctor/version.txt".to_string(),
            version_dump(report).into_bytes(),
        ),
    ];

    if let Some(bytes) = uname_bytes(redact) {
        out.push(("all-smi-doctor/uname.txt".to_string(), bytes));
    }
    if let Some(bytes) = lspci_bytes(redact) {
        out.push(("all-smi-doctor/lspci.txt".to_string(), bytes));
    }
    if let Some(bytes) = lsmod_bytes(redact) {
        out.push(("all-smi-doctor/lsmod.txt".to_string(), bytes));
    }
    if let Some(bytes) = dmesg_gpu_bytes(redact) {
        out.push(("all-smi-doctor/dmesg-gpu.txt".to_string(), bytes));
    }

    #[cfg(target_os = "macos")]
    if opts.verbose
        && let Some(bytes) = macos_system_profiler_bytes(redact)
    {
        out.push((
            "all-smi-doctor/system_profiler_display.txt".to_string(),
            bytes,
        ));
    }

    // TODO: once the effective merged config file (issue #192) ships,
    // append `all-smi-doctor/config.toml` here with the sensitive fields
    // redacted. Intentionally skipped for now because the config-file
    // tree does not yet exist.

    // Silence unused variable warnings on non-macOS builds.
    let _ = opts;

    Ok(out)
}

fn env_dump(redact: &RedactOptions) -> String {
    // Keep the env dump focused on hardware-related prefixes so we don't
    // leak the whole environment unnecessarily.
    let keep = [
        "ALL_SMI_",
        "CUDA_",
        "NVIDIA_",
        "ROCR_",
        "HIP_",
        "HSA_",
        "TPU_",
        "CLOUD_TPU_",
        "HL_",
        "HABANA_",
        "NO_COLOR",
        "LD_LIBRARY_PATH",
        "PATH",
        "USER",
        "HOSTNAME",
        "KUBERNETES_",
        "BACKENDAI_",
        "HOME",
    ];
    let mut vars: Vec<(String, String)> = std::env::vars()
        .filter(|(k, _)| keep.iter().any(|p| k.starts_with(*p) || k == p))
        .collect();
    vars.sort_by(|a, b| a.0.cmp(&b.0));
    let mut text = String::new();
    for (k, v) in vars {
        text.push_str(&format!("{k}={v}\n"));
    }
    scrub(&text, redact)
}

fn version_dump(report: &Report) -> String {
    let features = enabled_features().join(",");
    let triple = crate::doctor::checks::platform::checks()
        .iter()
        .find(|c| c.id == "platform.runtime")
        .map(|c| (c.run)(&Default::default()))
        .map(|r| r.message().to_string())
        .unwrap_or_else(|| "target unknown".to_string());
    let version = &report.version;
    let schema = report.schema;
    let timestamp = &report.timestamp;
    format!(
        "all-smi {version}\nschema: {schema}\ntimestamp: {timestamp}\nfeatures: {features}\nruntime: {triple}\n"
    )
}

fn enabled_features() -> Vec<&'static str> {
    let mut v: Vec<&'static str> = Vec::new();
    #[cfg(feature = "cli")]
    v.push("cli");
    #[cfg(feature = "mock")]
    v.push("mock");
    #[cfg(feature = "furiosa")]
    v.push("furiosa");
    if v.is_empty() {
        v.push("none");
    }
    v
}

fn uname_bytes(redact: &RedactOptions) -> Option<Vec<u8>> {
    #[cfg(unix)]
    {
        let out = try_exec("uname", &["-a"], Duration::from_millis(500))?;
        if out.success() {
            return Some(scrub(out.stdout.trim_end(), redact).into_bytes());
        }
        None
    }
    #[cfg(not(unix))]
    {
        let _ = redact;
        None
    }
}

fn lspci_bytes(redact: &RedactOptions) -> Option<Vec<u8>> {
    #[cfg(target_os = "linux")]
    {
        let out = try_exec("lspci", &["-vv"], Duration::from_millis(2_500))?;
        if !out.success() {
            return None;
        }
        // Filter to GPU-relevant lines plus their indented continuations
        // so reviewers see the accompanying capability / driver block.
        let mut keep: Vec<String> = Vec::new();
        let mut in_match = false;
        let keywords = [
            "VGA",
            "3D",
            "Display",
            "NVIDIA",
            "AMD",
            "Habana",
            "Tenstorrent",
            "Accel",
        ];
        for line in out.stdout.lines() {
            let trimmed = line.trim_start();
            if trimmed == line && !line.is_empty() {
                // New device block — decide whether to keep it.
                in_match = keywords.iter().any(|k| line.contains(k));
            }
            if in_match {
                keep.push(line.to_string());
            }
        }
        let text = keep.join("\n");
        Some(scrub(&text, redact).into_bytes())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = redact;
        None
    }
}

fn lsmod_bytes(redact: &RedactOptions) -> Option<Vec<u8>> {
    #[cfg(target_os = "linux")]
    {
        let out = try_exec("lsmod", &[], Duration::from_millis(1_000))?;
        if !out.success() {
            return None;
        }
        Some(scrub(&out.stdout, redact).into_bytes())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = redact;
        None
    }
}

fn dmesg_gpu_bytes(redact: &RedactOptions) -> Option<Vec<u8>> {
    #[cfg(target_os = "linux")]
    {
        // `dmesg` on modern kernels requires CAP_SYSLOG or `kernel.dmesg_restrict=0`.
        // If it fails (permission denied) we silently omit the file, per the
        // issue spec.
        let out = try_exec("dmesg", &["-T"], Duration::from_millis(2_500))?;
        if !out.success() {
            return None;
        }
        let keywords = ["nvidia", "amdgpu", "i915", "habanalabs", "drm", "tt-kmd"];
        let filtered: Vec<&str> = out
            .stdout
            .lines()
            .filter(|l| keywords.iter().any(|k| l.to_lowercase().contains(k)))
            .collect();
        // Last 200 lines only.
        let start = filtered.len().saturating_sub(200);
        let text = filtered[start..].join("\n");
        Some(scrub(&text, redact).into_bytes())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = redact;
        None
    }
}

#[cfg(target_os = "macos")]
fn macos_system_profiler_bytes(redact: &RedactOptions) -> Option<Vec<u8>> {
    // system_profiler SPDisplaysDataType is expensive — gated behind
    // --verbose in the CLI surface.
    let out = try_exec(
        "system_profiler",
        &["SPDisplaysDataType"],
        Duration::from_millis(2_900),
    )?;
    if !out.success() {
        return None;
    }
    Some(scrub(&out.stdout, redact).into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doctor::Summary;

    #[test]
    fn bundle_writes_expected_entries() {
        let tmp = tempfile::NamedTempFile::new().expect("tempfile");
        let report = Report {
            schema: 1,
            version: "0.99.9".to_string(),
            timestamp: "2026-04-20T00:00:00Z".to_string(),
            summary: Summary {
                pass: 1,
                warn: 0,
                fail: 0,
                skip: 0,
            },
            checks: vec![],
        };
        let opts = DoctorOptions {
            json: false,
            verbose: false,
            bundle_path: Some(tmp.path().to_path_buf()),
            include_identifiers: true,
            remote_checks: vec![],
            skip: vec![],
            only: vec![],
            use_color: false,
        };
        write_bundle(tmp.path(), &report, &opts).expect("bundle ok");
        let bytes = std::fs::read(tmp.path()).expect("read bundle");
        // Cheap sanity check: the gzip header magic should be present.
        assert!(bytes.len() > 2);
        assert_eq!(bytes[0], 0x1f);
        assert_eq!(bytes[1], 0x8b);
    }
}
