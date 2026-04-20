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

//! End-to-end integration coverage for the SSH transport (issue #194).
//!
//! These tests exercise the pure-data path: parser → decision tree →
//! connection status. They deliberately do NOT require a running
//! sshd (that would make the suite slow and flaky); the SSH client
//! itself is covered by the unit tests inside
//! `src/network/ssh_client.rs`.

use all_smi::app_state::ConnectionStatus;
use all_smi::network::nvidia_smi_shim::parse_nvidia_smi_csv;
use all_smi::network::rocm_smi_shim::parse_rocm_smi_json;
use all_smi::network::ssh_decision::{ProbeOutcomes, ProbeResult, select_transport};
use all_smi::network::ssh_target::{parse_hostfile_content, parse_ssh_arg};
use all_smi::network::ssh_transport::{SshFallbackPolicy, SshTransport};

#[test]
fn end_to_end_nvidia_smi_pipeline() {
    // The SSH strategy runs this exact CSV through the parser, then
    // publishes the resulting `GpuInfo` vector into AppState. Assert
    // that the end-to-end shape is what the renderer expects.
    let csv = "0, GPU-abc, A100, 550.54, 95, 70000, 81920, 72, 1410, 350.0\n";
    let gpus = parse_nvidia_smi_csv(csv, "ops@dgx01:22", "dgx01", "2026-04-20T00:00:00Z")
        .expect("golden csv must parse");
    assert_eq!(gpus.len(), 1);
    assert_eq!(gpus[0].hostname, "dgx01");
    assert_eq!(gpus[0].uuid, "GPU-abc");
    assert_eq!(gpus[0].utilization, 95.0);
}

#[test]
fn end_to_end_rocm_smi_pipeline() {
    let json = r#"{
        "card0": {
            "GPU use (%)": "90",
            "VRAM Total Memory (B)": "1024",
            "VRAM Total Used Memory (B)": "512",
            "Temperature (Sensor edge) (C)": "60",
            "Average Graphics Package Power (W)": "200",
            "Card series": "MI250X",
            "Unique ID": "0x01"
        }
    }"#;
    let gpus = parse_rocm_smi_json(json, "ops@amd01:22", "amd01", "2026-04-20T00:00:00Z").unwrap();
    assert_eq!(gpus.len(), 1);
    assert_eq!(gpus[0].uuid, "0x01");
    assert_eq!(gpus[0].utilization, 90.0);
}

#[test]
fn decision_tree_picks_native_over_fallbacks() {
    let policy = SshFallbackPolicy {
        try_nvidia_smi: true,
        try_rocm_smi: true,
    };
    let outcomes = ProbeOutcomes {
        native: ProbeResult::Available,
        nvidia_smi: ProbeResult::Available,
        rocm_smi: ProbeResult::Available,
    };
    assert_eq!(select_transport(&outcomes, &policy), SshTransport::Native);
}

#[test]
fn decision_tree_skips_nvidia_when_policy_disables_it() {
    let policy = SshFallbackPolicy {
        try_nvidia_smi: false,
        try_rocm_smi: true,
    };
    let outcomes = ProbeOutcomes {
        native: ProbeResult::NotAvailable,
        nvidia_smi: ProbeResult::Available,
        rocm_smi: ProbeResult::Available,
    };
    assert_eq!(select_transport(&outcomes, &policy), SshTransport::RocmSmi);
}

#[test]
fn hostfile_with_comments_and_ports_round_trips() {
    let content = "\
# bastion
admin@gw:2200
# dgx cluster
admin@dgx-01
admin@dgx-02:22 # default port
";
    let targets = parse_hostfile_content(content).unwrap();
    assert_eq!(targets.len(), 3);
    assert_eq!(targets[0].host, "gw");
    assert_eq!(targets[0].port, 2200);
    assert_eq!(targets[1].port, 22);
    assert_eq!(targets[2].port, 22);
}

#[test]
fn cli_arg_and_hostfile_combine() {
    // `parse_ssh_arg` and `parse_hostfile_content` both produce the
    // same `SshTarget` type so the caller can concatenate them.
    let cli = parse_ssh_arg("user@a").unwrap();
    let file = parse_hostfile_content("# file\nuser@b:2222\n").unwrap();
    let mut merged = cli;
    merged.extend(file);
    assert_eq!(merged.len(), 2);
    assert_eq!(merged[0].host, "a");
    assert_eq!(merged[1].host, "b");
    assert_eq!(merged[1].port, 2222);
}

#[test]
fn connection_errors_produce_typed_chips() {
    // Acceptance criterion: connection errors (timeout, auth fail,
    // host key mismatch) surface as per-host state, never a crash.
    let mut cs = ConnectionStatus::new("u@h:22".into(), "ssh://u@h".into());

    cs.mark_failure("auth-failed: SSH authentication failed for u@h:22".into());
    assert_eq!(cs.connection_state.as_deref(), Some("auth-failed"));

    cs.mark_failure("timeout: SSH connect timeout after 10s".into());
    assert_eq!(cs.connection_state.as_deref(), Some("timeout"));

    cs.mark_failure("host-key-rejected: host key not trusted".into());
    assert_eq!(cs.connection_state.as_deref(), Some("host-key-rejected"));

    // Any other error lands as "disconnected".
    cs.mark_failure("disconnected: TCP stream closed".into());
    assert_eq!(cs.connection_state.as_deref(), Some("disconnected"));

    // Recovery path.
    cs.mark_success();
    assert_eq!(cs.connection_state.as_deref(), Some("connected"));
    assert!(cs.is_connected);
}

#[test]
fn transport_chip_string_labels_stable_for_ui() {
    // The TUI renders these strings verbatim; any change here is a
    // breaking UI contract change.
    assert_eq!(SshTransport::Native.chip_label(), "native");
    assert_eq!(SshTransport::NvidiaSmi.chip_label(), "nvidia-smi");
    assert_eq!(SshTransport::RocmSmi.chip_label(), "rocm-smi");
    assert_eq!(SshTransport::Unsupported.chip_label(), "unsupported");
}
