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

//! Integration tests for the `all-smi doctor` subcommand (issue #188).
//!
//! These tests do not depend on any real GPU hardware being present —
//! they exercise the check framework, filtering, JSON/human renderers,
//! and redaction independently of hardware state.

#![cfg(feature = "cli")]

use all_smi::cli::DoctorArgs;
use all_smi::doctor::{
    CheckOutcome, DoctorOptions, Summary, checks as doctor_checks, passes_filter, run,
};

fn empty_doctor_args() -> DoctorArgs {
    DoctorArgs {
        json: false,
        verbose: false,
        bundle: None,
        include_identifiers: false,
        remote_check: Vec::new(),
        skip: Vec::new(),
        only: Vec::new(),
    }
}

#[tokio::test]
async fn doctor_run_returns_valid_report_shape() {
    let opts = DoctorOptions::from_args(&empty_doctor_args());
    let report = run(opts).await.expect("doctor run should succeed");
    // Schema version must match the compiled constant.
    assert_eq!(report.schema, all_smi::doctor::REPORT_SCHEMA_VERSION);
    // Version string is sourced from CARGO_PKG_VERSION and must not be empty.
    assert!(!report.version.is_empty());
    // Timestamp must parse as ISO-8601.
    assert!(chrono::DateTime::parse_from_rfc3339(&report.timestamp).is_ok());
    // Every check ID is stable and contains a dot.
    for outcome in &report.checks {
        assert!(!outcome.id.is_empty(), "check id must not be empty");
        assert!(
            outcome.id.contains('.'),
            "check id {:?} must contain a dot separator",
            outcome.id,
        );
        assert!(
            matches!(outcome.status, "pass" | "warn" | "fail" | "skip"),
            "unexpected status tag {:?}",
            outcome.status,
        );
    }
    // Summary tallies agree with the check list.
    let recomputed = report.checks.iter().fold(Summary::default(), |mut s, o| {
        match o.status {
            "pass" => s.pass += 1,
            "warn" => s.warn += 1,
            "fail" => s.fail += 1,
            "skip" => s.skip += 1,
            _ => {}
        }
        s
    });
    assert_eq!(report.summary.pass, recomputed.pass);
    assert_eq!(report.summary.warn, recomputed.warn);
    assert_eq!(report.summary.fail, recomputed.fail);
    assert_eq!(report.summary.skip, recomputed.skip);
}

#[tokio::test]
async fn doctor_json_output_is_parseable() {
    let opts = DoctorOptions::from_args(&empty_doctor_args());
    let report = run(opts.clone()).await.expect("run ok");

    let redact = opts.redact_options();
    let mut buf = Vec::new();
    all_smi::doctor::report::render_json(&report, &redact, &mut buf).expect("json render");

    let parsed: serde_json::Value =
        serde_json::from_slice(&buf).expect("JSON output must parse as serde_json::Value");
    assert_eq!(parsed["schema"].as_u64(), Some(1));
    assert!(parsed["summary"].is_object());
    assert!(parsed["checks"].is_array());
}

#[tokio::test]
async fn doctor_only_filter_limits_checks() {
    let mut args = empty_doctor_args();
    args.only = vec!["platform".to_string()];
    let opts = DoctorOptions::from_args(&args);
    let report = run(opts).await.expect("run ok");

    assert!(
        !report.checks.is_empty(),
        "platform filter must still run at least one check"
    );
    for outcome in &report.checks {
        assert!(
            outcome.id.starts_with("platform."),
            "expected only platform.* checks, got {}",
            outcome.id,
        );
    }
}

#[tokio::test]
async fn doctor_skip_filter_excludes_checks() {
    let mut args = empty_doctor_args();
    args.skip = vec!["nvidia".to_string()];
    let opts = DoctorOptions::from_args(&args);
    let report = run(opts).await.expect("run ok");

    for outcome in &report.checks {
        assert!(
            !outcome.id.starts_with("nvidia."),
            "nvidia filter should have been skipped: {}",
            outcome.id,
        );
    }
}

#[tokio::test]
async fn doctor_specific_check_ids_are_registered() {
    // These IDs are part of the stable public surface documented in the
    // issue. Any change must be intentional.
    let required = [
        "platform.os",
        "platform.runtime",
        "privileges.user",
        "container.runtime",
        "nvidia.nvml.loadable",
        "env.all_smi",
        "env.cuda",
        "network.dns",
    ];
    let all: Vec<&str> = doctor_checks::all().iter().map(|c| c.id).collect();
    for id in required {
        assert!(
            all.contains(&id),
            "required stable check id {id} missing from registry"
        );
    }
}

#[test]
fn filter_matches_expected_behaviour() {
    // Covered in the doctor::tests module too, but exercised here as an
    // integration-level smoke test against the public API.
    let only = vec!["nvidia".to_string()];
    let skip = vec!["nvidia.mig.mode".to_string()];
    assert!(passes_filter("nvidia.smi.binary", &only, &skip));
    assert!(!passes_filter("nvidia.mig.mode", &only, &skip));
    assert!(!passes_filter("platform.os", &only, &skip));
}

#[test]
fn redaction_scrubs_default_targets() {
    use all_smi::doctor::redact::{REDACT_IPV4, REDACT_MAC, RedactOptions, scrub};

    let opts = RedactOptions {
        hostname: Some("myserver".to_string()),
        username: Some("alice".to_string()),
        scrub_kernel_pointers: true,
        enabled: true,
    };
    let text = "user alice on myserver with eth0 02:42:ac:11:00:02 and 10.1.2.3";
    let scrubbed = scrub(text, &opts);
    assert!(scrubbed.contains(REDACT_IPV4));
    assert!(scrubbed.contains(REDACT_MAC));
    assert!(!scrubbed.contains("alice"));
    assert!(!scrubbed.contains("myserver"));
}

#[test]
fn summary_exit_code_matches_spec() {
    let s = Summary {
        pass: 3,
        warn: 0,
        fail: 0,
        skip: 1,
    };
    assert_eq!(s.exit_code(), 0);

    let s = Summary {
        pass: 3,
        warn: 2,
        fail: 0,
        skip: 1,
    };
    assert_eq!(s.exit_code(), 1);

    let s = Summary {
        pass: 3,
        warn: 0,
        fail: 1,
        skip: 1,
    };
    assert_eq!(s.exit_code(), 2);
}

#[test]
fn outcome_serde_shape_is_stable() {
    // Spot-check the field names our JSON consumers depend on.
    let outcome = CheckOutcome {
        id: "example.id".to_string(),
        title: "Example".to_string(),
        status: "pass",
        message: "ok".to_string(),
        fix: None,
        duration_ms: 42,
    };
    let j = serde_json::to_value(&outcome).expect("serialize");
    assert_eq!(j["id"], "example.id");
    assert_eq!(j["status"], "pass");
    assert_eq!(j["duration_ms"], 42);
    // `fix` omitted when None so consumers don't have to handle null.
    assert!(j.get("fix").is_none());
}
