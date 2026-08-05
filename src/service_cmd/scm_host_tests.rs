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

//! Windows-only tests for the service host (issue #311).
//!
//! The dispatcher, the control handler, and the status reporting all
//! need a live Service Control Manager, so what is asserted here is the
//! part that does not: the configuration-to-arguments mapping, which is
//! the only place the service can silently diverge from what
//! `all-smi api` would have done on the same host.

use super::*;

#[test]
fn every_api_argument_is_resolved_from_configuration() {
    // The SCM launches the binary with exactly `service run`, so a
    // `None` reaching `run_api_mode` would silently fall back to a
    // compiled default and ignore the operator's config file.
    let settings = Settings::default();
    let args = api_args(&settings);
    assert_eq!(args.port, Some(settings.api.port));
    assert_eq!(args.interval, Some(settings.api.interval_secs));
    assert_eq!(args.processes, Some(settings.api.processes));
}

#[test]
fn a_configured_port_reaches_the_listener_arguments() {
    let mut settings = Settings::default();
    settings.api.port = 19191;
    settings.api.interval_secs = 7;
    settings.api.processes = true;
    let args = api_args(&settings);
    assert_eq!(args.port, Some(19191));
    assert_eq!(args.interval, Some(7));
    assert_eq!(args.processes, Some(true));
}

#[test]
fn the_unexpected_stop_exit_code_is_non_zero() {
    // A zero exit tells the SCM the service stopped on purpose, and the
    // configured failure actions never fire.
    assert_ne!(EXIT_CODE_UNEXPECTED_STOP, 0);
}
