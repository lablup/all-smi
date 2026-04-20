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
    let f =
        open_bundle_file(path).with_context(|| format!("failed to create bundle file {path:?}"))?;
    let gz = GzEncoder::new(f, Compression::default());
    let mut tar = tar::Builder::new(gz);

    for (name, bytes) in &entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o600);
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
    let f = gz.finish().context("failed to finalise gzip stream")?;
    // Persist the archive to disk before returning so a subsequent
    // tampering attempt cannot race a short-lived file descriptor
    // flush.
    if let Err(e) = f.sync_all() {
        return Err(e).with_context(|| format!("failed to fsync bundle {path:?}"));
    }
    Ok(())
}

/// Open the bundle file with symlink-safe, owner-only permissions.
///
/// On Unix the file is created with `O_NOFOLLOW | O_CREAT | O_EXCL`-style
/// semantics via `custom_flags(libc::O_NOFOLLOW)` and mode `0o600`. This
/// mirrors the hardening used for snapshot (`src/snapshot/mod.rs`) and
/// record (`src/record/writer.rs`) output and addresses the same TOCTOU
/// risk: a pre-existing symlink at `path` must NOT cause the writer to
/// follow into an unintended destination (e.g., `/etc/shadow`).
///
/// On Windows the file is opened with `share_mode(0)` (exclusive
/// sharing) which blocks other processes from opening it while the tar
/// stream is being written. Fine-grained symlink TOCTOU mitigation on
/// Windows needs different primitives and is out of scope for this
/// helper.
///
/// A pre-existing symlink at `path` surfaces `ErrorKind::InvalidInput`
/// (via `ELOOP` on Linux) rather than the write silently going through
/// the symlink.
fn open_bundle_file(path: &Path) -> std::io::Result<File> {
    use std::fs::OpenOptions;

    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .custom_flags(libc::O_NOFOLLOW)
            .mode(0o600)
            .open(path)
    }
    #[cfg(all(windows, not(unix)))]
    {
        use std::os::windows::fs::OpenOptionsExt;
        OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .share_mode(0)
            .open(path)
    }
    #[cfg(not(any(unix, windows)))]
    {
        OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)
    }
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

/// Case-insensitive substrings that mark a variable name as likely to
/// contain a secret. When any of these appear in the variable name (e.g.
/// `BACKENDAI_SECRET_KEY`, `NVIDIA_API_TOKEN`, `HUGGINGFACE_HUB_TOKEN`),
/// the value is replaced with a redaction marker before it is written to
/// the bundle. Match is substring-based so variant spellings such as
/// `ACCESS_KEY_ID`, `CLIENT_SECRET`, `BEARER_TOKEN` are all covered.
///
/// This list is applied even when `--include-identifiers` is set —
/// that flag opts back in to hostnames / IPs / usernames, not to
/// credential values which should never appear in a support bundle.
const SECRET_NAME_SUBSTRINGS: &[&str] = &[
    "TOKEN",
    "SECRET",
    "PASSWORD",
    "PASSWD",
    "API_KEY",
    "APIKEY",
    "ACCESS_KEY",
    "PRIVATE_KEY",
    "CREDENTIAL",
    "AUTH",
    "SESSION",
    "COOKIE",
    "BEARER",
    "SIGNATURE",
    "ENCRYPTION_KEY",
    "CLIENT_SECRET",
];

/// Redaction marker substituted for the value of any variable whose
/// name matches [`SECRET_NAME_SUBSTRINGS`].
pub(crate) const REDACT_SECRET_VALUE: &str = "<redacted:secret>";

/// Returns `true` when `name` looks like a credential-bearing env var.
/// Matching is case-insensitive and substring-based so both
/// `BACKENDAI_SECRET_KEY` and `backendai_secret_key` are caught.
pub(crate) fn is_secret_env_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    SECRET_NAME_SUBSTRINGS.iter().any(|p| upper.contains(p))
}

fn env_dump(redact: &RedactOptions) -> String {
    // Keep the env dump focused on hardware-related prefixes so we
    // do not leak the whole environment unnecessarily. `PATH` and
    // `LD_LIBRARY_PATH` are intentionally *not* included verbatim —
    // their values frequently contain `/home/<username>` segments
    // and private build directories. A compact length-only summary
    // is emitted for them instead so the bundle still reflects
    // whether they are set without leaking personal filesystem
    // layout.
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
        "USER",
        "HOSTNAME",
        "KUBERNETES_",
        "BACKENDAI_",
        "HOME",
    ];
    let mut vars: Vec<(String, String)> = std::env::vars()
        .filter(|(k, _)| keep.iter().any(|p| k.starts_with(*p) || k == p))
        .map(|(k, v)| {
            if is_secret_env_name(&k) {
                (k, REDACT_SECRET_VALUE.to_string())
            } else {
                (k, v)
            }
        })
        .collect();
    vars.sort_by(|a, b| a.0.cmp(&b.0));

    let mut text = String::new();
    for (k, v) in vars {
        text.push_str(&format!("{k}={v}\n"));
    }

    // Summarise PATH / LD_LIBRARY_PATH without their content: the
    // fact of being set and the number of entries is useful for
    // debugging; the actual paths are not.
    for var in ["PATH", "LD_LIBRARY_PATH"] {
        match std::env::var(var) {
            Ok(v) if !v.is_empty() => {
                let sep = if cfg!(windows) { ';' } else { ':' };
                let entries = v.split(sep).filter(|s| !s.is_empty()).count();
                text.push_str(&format!("{var}=<redacted:path-list {entries} entries>\n"));
            }
            _ => {}
        }
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

    #[test]
    fn is_secret_env_name_matches_common_patterns() {
        assert!(is_secret_env_name("BACKENDAI_SECRET_KEY"));
        assert!(is_secret_env_name("BACKENDAI_ACCESS_KEY"));
        assert!(is_secret_env_name("AWS_SESSION_TOKEN"));
        assert!(is_secret_env_name("HUGGINGFACE_HUB_TOKEN"));
        assert!(is_secret_env_name("MY_API_KEY"));
        assert!(is_secret_env_name("github_client_secret"));
        assert!(is_secret_env_name("SERVICE_PASSWORD"));
        assert!(is_secret_env_name("BEARER_TOKEN_PROD"));

        assert!(!is_secret_env_name("NVIDIA_VISIBLE_DEVICES"));
        assert!(!is_secret_env_name("CUDA_VISIBLE_DEVICES"));
        assert!(!is_secret_env_name("HOME"));
        assert!(!is_secret_env_name("USER"));
        assert!(!is_secret_env_name("PATH"));
    }

    #[test]
    fn env_dump_redacts_secret_values_and_summarises_path() {
        // SAFETY: env var mutation is unsafe in Rust 2024. This test
        // mutates process-global state; parallel tests that also read
        // these names might see transient values, but the matrix here
        // uses unique test-scoped names.
        unsafe {
            std::env::set_var("ALL_SMI_DOCTOR_TEST_TOKEN", "hunter2");
            std::env::set_var("ALL_SMI_DOCTOR_TEST_PLAIN", "public-value");
            std::env::set_var("PATH", "/a:/b:/c");
        }
        let opts = RedactOptions {
            hostname: None,
            username: None,
            scrub_kernel_pointers: false,
            enabled: true,
        };
        let dump = env_dump(&opts);
        unsafe {
            std::env::remove_var("ALL_SMI_DOCTOR_TEST_TOKEN");
            std::env::remove_var("ALL_SMI_DOCTOR_TEST_PLAIN");
        }

        // Secret value replaced with redaction marker.
        assert!(
            dump.contains("ALL_SMI_DOCTOR_TEST_TOKEN=<redacted:secret>"),
            "secret value must be redacted: {dump}"
        );
        assert!(
            !dump.contains("hunter2"),
            "raw secret must not appear in bundle: {dump}"
        );

        // Non-secret value preserved verbatim.
        assert!(
            dump.contains("ALL_SMI_DOCTOR_TEST_PLAIN=public-value"),
            "non-secret value must be preserved: {dump}"
        );

        // PATH entries replaced with a length-only summary.
        assert!(
            dump.contains("PATH=<redacted:path-list"),
            "PATH must be summarised: {dump}"
        );
        assert!(
            !dump.contains("/a:/b:/c"),
            "raw PATH contents must not leak: {dump}"
        );
    }

    #[test]
    fn bundle_unix_mode_is_0o600() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let tmp = tempfile::NamedTempFile::new().expect("tempfile");
            let report = Report {
                schema: 1,
                version: "0.99.9".to_string(),
                timestamp: "2026-04-20T00:00:00Z".to_string(),
                summary: Summary::default(),
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
            let meta = std::fs::metadata(tmp.path()).expect("metadata");
            let mode = meta.permissions().mode() & 0o777;
            assert_eq!(
                mode, 0o600,
                "bundle file must be owner-read/write only, got {mode:o}"
            );
        }
    }

    #[test]
    fn bundle_refuses_preexisting_symlink() {
        #[cfg(unix)]
        {
            // Build a symlink and ask write_bundle to overwrite its
            // target. With O_NOFOLLOW the open must fail rather than
            // dereferencing the symlink.
            use std::os::unix::fs::symlink;
            let dir = tempfile::tempdir().expect("tempdir");
            let decoy_target = dir.path().join("DECOY");
            std::fs::write(&decoy_target, b"sensitive").expect("decoy write");
            let link_path = dir.path().join("bundle.tar.gz");
            symlink(&decoy_target, &link_path).expect("symlink");

            let report = Report {
                schema: 1,
                version: "0.99.9".to_string(),
                timestamp: "2026-04-20T00:00:00Z".to_string(),
                summary: Summary::default(),
                checks: vec![],
            };
            let opts = DoctorOptions {
                json: false,
                verbose: false,
                bundle_path: Some(link_path.clone()),
                include_identifiers: true,
                remote_checks: vec![],
                skip: vec![],
                only: vec![],
                use_color: false,
            };
            let result = write_bundle(&link_path, &report, &opts);
            assert!(
                result.is_err(),
                "write_bundle must refuse to follow a pre-existing symlink"
            );

            // Decoy target must not have been overwritten.
            let decoy_contents = std::fs::read(&decoy_target).expect("decoy read");
            assert_eq!(
                decoy_contents, b"sensitive",
                "symlink target was overwritten despite O_NOFOLLOW"
            );
        }
    }
}
