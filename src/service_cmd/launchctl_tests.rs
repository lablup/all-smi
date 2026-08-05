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

//! Unit tests for the `launchctl(1)` interface: layout resolution,
//! `launchctl print` and `print-disabled` parsing, the status detail
//! line, and command description. Kept in a sibling file so the
//! implementation stays under the 500-line soft limit.
//!
//! Every fixture below is verbatim `launchctl` output captured on macOS
//! 26.6 (Darwin 25.6), trimmed to the keys the parser reads plus the
//! neighbours that have historically confused key matching.

use std::path::PathBuf;

use super::*;

fn home() -> PathBuf {
    PathBuf::from("/Users/dev")
}

// ── layout ────────────────────────────────────────────────────────────

#[test]
fn system_layout_targets_the_administrator_daemon_directory() {
    let l = layout_from(Scope::System, &home(), 501);
    // /System/Library/LaunchDaemons is Apple's; third-party daemons
    // belong in /Library/LaunchDaemons and only there.
    assert_eq!(
        l.plist,
        PathBuf::from("/Library/LaunchDaemons/com.lablup.all-smi.plist")
    );
    assert_eq!(l.log, PathBuf::from("/var/log/all-smi/all-smi.log"));
    assert_eq!(l.domain, "system");
    assert_eq!(l.target, "system/com.lablup.all-smi");
}

#[test]
fn user_layout_targets_the_gui_domain_of_the_invoking_uid() {
    let l = layout_from(Scope::User, &home(), 501);
    assert_eq!(
        l.plist,
        PathBuf::from("/Users/dev/Library/LaunchAgents/com.lablup.all-smi.plist")
    );
    assert_eq!(
        l.log,
        PathBuf::from("/Users/dev/Library/Logs/all-smi/all-smi.log")
    );
    assert_eq!(l.domain, "gui/501");
    assert_eq!(l.target, "gui/501/com.lablup.all-smi");
}

#[test]
fn the_uid_is_not_hardcoded() {
    let l = layout_from(Scope::User, &home(), 1234);
    assert_eq!(l.domain, "gui/1234");
    assert_eq!(l.target, "gui/1234/com.lablup.all-smi");
}

#[test]
fn the_bootstrap_domain_is_a_prefix_of_the_service_target() {
    // `launchctl bootstrap` takes the domain, every other verb takes
    // the service target. Getting one of them wrong produces a
    // "Bootstrap failed" that says nothing about which.
    for scope in [Scope::System, Scope::User] {
        let l = layout_from(scope, &home(), 501);
        assert_eq!(l.target, format!("{}/{LABEL}", l.domain));
    }
}

#[test]
fn both_scopes_share_the_label_and_the_file_name() {
    let system = layout_from(Scope::System, &home(), 501);
    let user = layout_from(Scope::User, &home(), 501);
    // Different domains, so the same label never collides, and an
    // operator sees one name everywhere.
    assert_eq!(system.plist.file_name(), user.plist.file_name());
    assert_eq!(
        system.plist.file_name().unwrap().to_str().unwrap(),
        PLIST_FILE_NAME
    );
    assert!(PLIST_FILE_NAME.starts_with(LABEL));
}

#[test]
fn a_user_layout_never_escapes_the_home_directory() {
    let l = layout_from(Scope::User, &home(), 501);
    assert!(l.plist.starts_with(home()));
    assert!(l.log.starts_with(home()));
}

// ── `launchctl print` parsing ─────────────────────────────────────────

const PRINT_RUNNING: &str = r#"gui/501/com.lablup.all-smi = {
	active count = 1
	path = /Users/dev/Library/LaunchAgents/com.lablup.all-smi.plist
	type = LaunchAgent
	state = running

	program = /opt/all-smi/bin/all-smi
	default environment = {
		PATH => /usr/bin:/bin:/usr/sbin:/sbin
	}

	domain = gui/501 [100019]
	asid = 100019
	minimum runtime = 10
	exit timeout = 30
	runs = 1
	pid = 4242
	immediate reason = speculative
	forks = 0
	execs = 1
	last exit code = (never exited)

	endpoints = {
		"com.lablup.all-smi.peer" = {
			port = 0x1234
			active = 1
			state = active
		}
	}
	jetsam priority = 4
}
"#;

const PRINT_NOT_RUNNING: &str = r#"gui/501/com.lablup.all-smi = {
	active count = 0
	path = /Users/dev/Library/LaunchAgents/com.lablup.all-smi.plist
	type = LaunchAgent
	state = not running

	program = /opt/all-smi/bin/all-smi
	runs = 3
	last exit code = 0
}
"#;

const PRINT_NOT_FOUND: &str = "Bad request.\nCould not find service \
                               \"com.lablup.all-smi\" in domain for user gui: 501\n";

#[test]
fn a_running_job_yields_its_state_and_pid() {
    let info = parse_print_output(PRINT_RUNNING);
    assert_eq!(info.state, "running");
    assert_eq!(info.pid, Some(4242));
    assert!(info.running());
}

#[test]
fn nested_dictionaries_do_not_shadow_the_jobs_own_state() {
    // `launchctl print` repeats `state` inside every endpoint
    // dictionary with the unrelated value `active`. A last-wins parser
    // reports a running job as `active` and a stopped one as running.
    assert!(PRINT_RUNNING.contains("state = active"));
    assert_eq!(parse_print_output(PRINT_RUNNING).state, "running");
}

#[test]
fn a_loaded_but_stopped_job_is_not_running() {
    let info = parse_print_output(PRINT_NOT_RUNNING);
    assert_eq!(info.state, "not running");
    assert_eq!(info.pid, None);
    assert!(!info.running());
}

#[test]
fn neighbouring_keys_are_not_mistaken_for_pid_or_state() {
    // `last exit code`, `exit timeout`, and `jetsam priority` all sit
    // next to the keys we want and all contain integers.
    let info = parse_print_output(PRINT_RUNNING);
    assert_eq!(info.pid, Some(4242));
    assert_ne!(info.pid, Some(0));
    assert_ne!(info.pid, Some(30));
    assert_ne!(info.pid, Some(4));
}

#[test]
fn a_zero_pid_is_treated_as_absent() {
    let info = parse_print_output("\tstate = not running\n\tpid = 0\n");
    assert_eq!(info.pid, None);
}

#[test]
fn garbage_never_panics_and_never_claims_a_running_job() {
    for raw in [
        "",
        PRINT_NOT_FOUND,
        "= = =\n",
        "state\npid\n",
        "pid = abc\n",
    ] {
        let info = parse_print_output(raw);
        assert!(!info.running(), "must not claim running for: {raw:?}");
    }
}

#[test]
fn only_the_exact_running_state_counts_as_running() {
    // launchd also reports `waiting` and `spawn scheduled`; neither
    // means the program is executing.
    for state in ["not running", "waiting", "spawn scheduled", ""] {
        let info = parse_print_output(&format!("\tstate = {state}\n"));
        assert!(!info.running(), "{state:?} must not count as running");
    }
    assert!(parse_print_output("\tstate = running\n").running());
}

// ── `launchctl print-disabled` parsing ────────────────────────────────

const DISABLED_MODERN: &str = "\tdisabled services = {\n\
                               \t\t\"com.apple.Siri.agent\" => enabled\n\
                               \t\t\"com.lablup.all-smi\" => disabled\n\
                               \t\t\"com.apple.ScriptMenuApp\" => disabled\n\
                               \t}\n";

const ENABLED_MODERN: &str = "\tdisabled services = {\n\
                              \t\t\"com.lablup.all-smi\" => enabled\n\
                              \t}\n";

const DISABLED_LEGACY: &str = "\tdisabled services = {\n\
                               \t\t\"com.lablup.all-smi\" => true\n\
                               \t}\n";

#[test]
fn a_persistent_disable_override_is_detected() {
    assert_eq!(parse_disabled(DISABLED_MODERN, LABEL), Some(true));
    assert_eq!(parse_disabled(ENABLED_MODERN, LABEL), Some(false));
}

#[test]
fn the_legacy_boolean_spelling_is_still_understood() {
    // launchctl printed `=> true` / `=> false` before Sonoma; an
    // operator on an older machine must not silently read as enabled.
    assert_eq!(parse_disabled(DISABLED_LEGACY, LABEL), Some(true));
    assert_eq!(
        parse_disabled("\t\t\"com.lablup.all-smi\" => false\n", LABEL),
        Some(false)
    );
}

#[test]
fn an_unlisted_label_is_not_an_override() {
    assert_eq!(parse_disabled(DISABLED_MODERN, "com.lablup.other"), None);
    assert_eq!(parse_disabled("", LABEL), None);
}

#[test]
fn a_label_that_is_a_prefix_of_ours_is_not_confused_for_it() {
    // The quotes in the fixture are what make this exact rather than a
    // substring match.
    let raw = "\t\t\"com.lablup.all-smi-exporter\" => disabled\n";
    assert_eq!(parse_disabled(raw, LABEL), None);
}
// ── status detail ─────────────────────────────────────────────────────

#[test]
fn a_plist_on_disk_that_is_not_loaded_reads_as_not_loaded() {
    // This is exactly the state `install` without `--now` leaves
    // behind, so it must read as a normal condition, not as a fault.
    assert_eq!(
        not_loaded_detail(
            true,
            "Could not find service \"com.lablup.all-smi\" in domain"
        ),
        "not loaded"
    );
    assert_eq!(
        not_loaded_detail(false, "Could not find service"),
        "not installed"
    );
}

#[test]
fn an_unexpected_launchctl_failure_is_surfaced_rather_than_hidden() {
    // Reporting "not loaded" for a permission failure would be a lie,
    // and the usual cause is querying the system domain from an
    // unprivileged shell.
    let detail = not_loaded_detail(true, "Operation not permitted\nsecond line");
    assert!(detail.starts_with("loaded state unknown:"));
    assert!(detail.contains("Operation not permitted"));
    assert!(!detail.contains("second line"), "one line is enough");
}

#[test]
fn an_empty_failure_still_produces_a_usable_detail() {
    assert_eq!(not_loaded_detail(true, ""), "not loaded");
}

// ── command construction ──────────────────────────────────────────────

#[test]
fn launchctl_invocations_are_described_verbatim_for_diagnostics() {
    // The description is what an operator sees in a CommandFailed
    // error, so it must be a command they can paste back into a shell.
    assert_eq!(
        describe(&["bootstrap", "gui/501", "/Users/dev/x.plist"]),
        "launchctl bootstrap gui/501 /Users/dev/x.plist"
    );
    assert_eq!(
        describe(&["kickstart", "-k", "system/com.lablup.all-smi"]),
        "launchctl kickstart -k system/com.lablup.all-smi"
    );
}

#[test]
fn a_non_utf8_plist_path_is_refused_before_it_reaches_launchctl() {
    #[cfg(unix)]
    {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        let bad = PathBuf::from(OsString::from_vec(vec![0x2f, 0xff]));
        let err = path_arg(&bad).expect_err("launchctl arguments must be UTF-8");
        assert!(matches!(err, ServiceError::Conflict(_)));
    }
}
