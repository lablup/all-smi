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

use std::path::Path;
use std::process::{Command, Output};

use all_smi::utils::command::new_command;

fn all_smi_command(home: &Path) -> Command {
    let mut command = new_command(env!("CARGO_BIN_EXE_all-smi"));
    command
        .env("HOME", home)
        .env("XDG_CACHE_HOME", home.join("cache"))
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env_remove("ALL_SMI_VIEW_HOSTS");
    command
}

fn run_all_smi(home: &Path, args: &[&str]) -> Output {
    all_smi_command(home)
        .args(args)
        .output()
        .expect("run all-smi")
}

fn entered_alternate_screen(output: &Output) -> bool {
    const ENTER_ALTERNATE_SCREEN: &[u8] = b"\x1b[?1049h";
    output
        .stdout
        .windows(ENTER_ALTERNATE_SCREEN.len())
        .any(|bytes| bytes == ENTER_ALTERNATE_SCREEN)
}

#[test]
fn view_rejects_invalid_host_before_entering_the_tui() {
    let home = tempfile::tempdir().expect("create isolated home");
    let output = run_all_smi(
        home.path(),
        &["view", "--hosts", "host-a:9090,host-b:not-a-port"],
    );
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!output.status.success());
    assert!(stderr.contains("host-b:not-a-port"), "{stderr}");
    assert!(stderr.contains("invalid port"), "{stderr}");
    assert!(
        !entered_alternate_screen(&output),
        "invalid host must be rejected before alternate-screen output"
    );
}

#[test]
fn view_validates_config_and_environment_hosts_before_entering_the_tui() {
    let home = tempfile::tempdir().expect("create isolated home");
    let config = home.path().join("config.toml");
    std::fs::write(&config, "[view]\nhosts = [\"host-a:9090\", \"http://\"]\n")
        .expect("write config");
    let config_arg = config.to_string_lossy().into_owned();
    let from_config = run_all_smi(home.path(), &["--config", &config_arg, "view"]);
    let config_stderr = String::from_utf8_lossy(&from_config.stderr);
    assert!(!from_config.status.success());
    assert!(config_stderr.contains("http://"), "{config_stderr}");
    assert!(config_stderr.contains("missing host"), "{config_stderr}");
    assert!(!entered_alternate_screen(&from_config));

    let from_env = all_smi_command(home.path())
        .env("ALL_SMI_VIEW_HOSTS", "host-a:9090,,https://host-b:9443")
        .arg("view")
        .output()
        .expect("run all-smi with environment hosts");
    let env_stderr = String::from_utf8_lossy(&from_env.stderr);
    assert!(!from_env.status.success());
    assert!(env_stderr.contains("empty entry"), "{env_stderr}");
    assert!(!entered_alternate_screen(&from_env));
}

#[test]
fn remote_record_rejects_invalid_host_before_creating_output() {
    let home = tempfile::tempdir().expect("create isolated home");
    let recording = home.path().join("must-not-exist.ndjson");
    let recording_arg = recording.to_string_lossy().into_owned();
    let output = run_all_smi(
        home.path(),
        &[
            "record",
            "--source=remote",
            "--hosts",
            "host-a:9090,ftp://host-b:9090",
            "--output",
            &recording_arg,
        ],
    );
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!output.status.success());
    assert!(stderr.contains("ftp://host-b:9090"), "{stderr}");
    assert!(stderr.contains("unsupported scheme"), "{stderr}");
    assert!(
        !recording.exists(),
        "invalid host must be rejected before output creation"
    );
}
