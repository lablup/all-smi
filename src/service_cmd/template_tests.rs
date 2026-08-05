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

//! Unit tests for `service_cmd::template`. Kept in a sibling file so the
//! implementation stays under the 500-line soft limit.

use super::*;
use std::path::PathBuf;

fn system_unit(service_user: Option<&str>) -> String {
    render_unit(&RenderParams {
        scope: Scope::System,
        exec_path: Path::new("/usr/local/bin/all-smi"),
        service_user,
    })
    .expect("plain ASCII path must render")
}

fn user_unit() -> String {
    render_unit(&RenderParams {
        scope: Scope::User,
        exec_path: Path::new("/home/dev/.cargo/bin/all-smi"),
        service_user: None,
    })
    .expect("plain ASCII path must render")
}

#[test]
fn marker_is_the_first_line() {
    let unit = system_unit(None);
    assert_eq!(
        unit.lines().next(),
        Some(MANAGED_MARKER),
        "the marker must lead the file so `head -1` identifies it"
    );
    assert!(is_managed(&unit));
}

#[test]
fn is_managed_rejects_a_foreign_unit() {
    assert!(!is_managed(UNIT_TEMPLATE));
    assert!(!is_managed("[Unit]\nDescription=hand written\n"));
}

#[test]
fn exec_start_uses_the_canonicalized_binary_path() {
    let unit = system_unit(None);
    assert!(
        unit.contains("ExecStart=/usr/local/bin/all-smi api\n"),
        "ExecStart must point at the running binary, got:\n{unit}"
    );
    assert!(
        !unit.contains("ExecStart=/usr/bin/all-smi api\n"),
        "the packaged ExecStart path must have been replaced, got:\n{unit}"
    );
    assert_eq!(
        unit.matches("ExecStart=").count(),
        1,
        "exactly one ExecStart line must survive"
    );
}

#[test]
fn system_scope_without_service_user_runs_as_root() {
    let unit = system_unit(None);
    assert!(
        !unit.contains("\nUser="),
        "no User= means systemd runs the unit as root, got:\n{unit}"
    );
    assert!(!unit.contains("\nGroup="), "Group= must be dropped too");
    // Hardening and the supplementary groups still apply to root.
    assert!(unit.contains("SupplementaryGroups=video render"));
    assert!(unit.contains("NoNewPrivileges=true"));
    assert!(unit.contains("ProtectSystem=strict"));
}

#[test]
fn system_scope_injects_the_requested_account() {
    let unit = system_unit(Some("metrics"));
    assert!(
        unit.contains("\nUser=metrics\n"),
        "--service-user must set User=, got:\n{unit}"
    );
    assert!(
        unit.contains("\nGroup=metrics\n"),
        "--service-user must set Group= to match, got:\n{unit}"
    );
    assert!(
        !unit.contains("User=all-smi"),
        "the packaged account must not leak through, got:\n{unit}"
    );
}

#[test]
fn system_scope_keeps_the_wal_pin_and_boot_target() {
    let unit = system_unit(None);
    assert!(unit.contains("Environment=ALL_SMI_ENERGY_WAL_PATH=/var/cache/all-smi/energy-wal.bin"));
    assert!(unit.contains("WantedBy=multi-user.target"));
    assert!(unit.contains("Wants=network-online.target"));
}

#[test]
fn user_scope_drops_privileged_directives() {
    let unit = user_unit();
    assert!(!unit.contains("User="), "got:\n{unit}");
    assert!(!unit.contains("Group="), "got:\n{unit}");
    assert!(
        !unit.contains("SupplementaryGroups="),
        "a user manager cannot grant supplementary groups, got:\n{unit}"
    );
}

#[test]
fn user_scope_drops_the_wal_pin() {
    // The user cache directory resolves normally, so pinning the WAL
    // into /var/cache would point it somewhere unwritable.
    let unit = user_unit();
    assert!(!unit.contains("ALL_SMI_ENERGY_WAL_PATH"), "got:\n{unit}");
}

#[test]
fn user_scope_targets_default_target_not_multi_user() {
    let unit = user_unit();
    assert!(
        unit.contains("WantedBy=default.target"),
        "a user manager has no multi-user.target, got:\n{unit}"
    );
    assert!(!unit.contains("multi-user.target"), "got:\n{unit}");
}

#[test]
fn user_scope_drops_network_online_dependencies() {
    // network-online.target does not exist in a user manager;
    // keeping it only produces a startup warning.
    let unit = user_unit();
    assert!(!unit.contains("network-online.target"), "got:\n{unit}");
    // The rest of the [Unit] section survives.
    assert!(unit.contains("Description=all-smi GPU/NPU metrics exporter (API mode)"));
}

#[test]
fn system_scope_keeps_every_hardening_directive() {
    let unit = system_unit(None);
    for directive in [
        "NoNewPrivileges=true",
        "ProtectSystem=strict",
        "ProtectHome=true",
        "PrivateTmp=true",
        "ProtectKernelModules=true",
        "ProtectControlGroups=true",
        "RestrictSUIDSGID=true",
    ] {
        assert!(
            unit.contains(directive),
            "{directive} must be preserved, got:\n{unit}"
        );
    }
}

/// Hardening a per-user systemd manager *can* apply, because it is
/// implemented purely through `prctl` or a seccomp filter and needs no
/// privilege.
///
/// The complement of [`USER_SCOPE_DROPPED_PREFIXES`]. Dropping what a
/// user manager cannot apply must never become an excuse to weaken what
/// it can, so this list is asserted positively below. It lives in the
/// test module rather than beside the drop list because the renderer
/// never consults it: only the tests do.
const USER_SCOPE_KEPT_HARDENING: &[&str] = &["NoNewPrivileges=", "RestrictSUIDSGID="];

#[test]
fn user_scope_keeps_the_privilege_free_hardening() {
    // Dropping what a user manager cannot apply must not become an
    // excuse to drop what it can. These two are prctl and seccomp only,
    // so they need no privilege and belong in every rendering.
    let unit = user_unit();
    for directive in USER_SCOPE_KEPT_HARDENING {
        assert!(
            unit.contains(directive),
            "{directive} needs no privilege and must be preserved, got:\n{unit}"
        );
    }
}

/// The regression guard for the CI failure this list exists to prevent.
///
/// Every directive a per-user manager cannot apply must be absent from
/// the user render. Leaving one in does not degrade the service, it
/// stops it from starting at all: the unit dies before `ExecStart` and
/// `systemctl --user start` reports only "control process exited with
/// error code".
#[test]
fn user_scope_drops_everything_a_user_manager_cannot_apply() {
    let unit = user_unit();
    for directive in USER_SCOPE_DROPPED_PREFIXES {
        assert!(
            !unit.contains(directive),
            "{directive} must be dropped in user scope or the unit fails before ExecStart, got:\n{unit}"
        );
    }
}

/// `ProtectKernelModules=` is the trap: it reads as pure seccomp, but it
/// also alters the capability bounding set, which an unprivileged
/// manager can only do from inside a user namespace. Where the host
/// denies unprivileged user namespaces (stock Ubuntu 24.04 and later),
/// the unit dies with 218/CAPABILITIES. Reproduced against systemd 255
/// with `ExecStart=/bin/sleep` to rule the application out.
///
/// It costs nothing to drop: an unprivileged process never holds
/// `CAP_SYS_MODULE` in the first place, so module loading is already
/// impossible for a user service.
#[test]
fn user_scope_drops_protect_kernel_modules() {
    let unit = user_unit();
    assert!(
        !unit.contains("ProtectKernelModules"),
        "ProtectKernelModules needs a user namespace to alter the capability bounding set; \
         leaving it in makes `systemctl --user start` fail with 218/CAPABILITIES, got:\n{unit}"
    );
    // It is still correct, and still applied, for a system unit.
    assert!(system_unit(None).contains("ProtectKernelModules=true"));
}

/// The two lists must not overlap, or a directive would be both
/// required and forbidden and one of the assertions above would be
/// unsatisfiable.
#[test]
fn kept_and_dropped_directive_lists_are_disjoint() {
    for kept in USER_SCOPE_KEPT_HARDENING {
        assert!(
            !USER_SCOPE_DROPPED_PREFIXES.contains(kept),
            "`{kept}` appears in both the kept and the dropped list"
        );
    }
}

/// Everything the user render keeps from the `[Service]` section must be
/// either a drop-list survivor by design or explicitly known-safe. This
/// catches a directive added to the shipped unit that nobody classified:
/// a new privileged directive would otherwise silently reach user-scope
/// installs and break them exactly the way ProtectKernelModules did.
#[test]
fn every_service_directive_in_the_user_render_is_classified() {
    // Directives that need no privilege and are intentionally kept.
    const KNOWN_SAFE: &[&str] = &[
        "Type=",
        "ExecStart=",
        "EnvironmentFile=",
        "Restart=",
        "RestartSec=",
        "RuntimeDirectory=",
        "CacheDirectory=",
        "NoNewPrivileges=",
        "RestrictSUIDSGID=",
    ];

    let unit = user_unit();
    let mut in_service = false;
    for line in unit.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_service = line == "[Service]";
            continue;
        }
        if !in_service || line.is_empty() || line.starts_with('#') {
            continue;
        }
        assert!(
            KNOWN_SAFE.iter().any(|k| line.starts_with(k)),
            "unclassified directive `{line}` reached the user-scope render. Decide whether a \
             per-user systemd manager can apply it: add it to KNOWN_SAFE here if it needs no \
             privilege, otherwise add it to USER_SCOPE_DROPPED_PREFIXES."
        );
    }
}

/// Not a style nit: a directive listed for dropping that no longer
/// exists in the shipped unit means the classification has gone stale,
/// and the next directive rename would slip an unapplicable one into
/// user-scope installs unnoticed.
#[test]
fn kept_hardening_all_appears_in_the_shipped_unit() {
    for prefix in USER_SCOPE_KEPT_HARDENING {
        assert!(
            UNIT_TEMPLATE
                .lines()
                .any(|l| l.trim_start().starts_with(prefix)),
            "`{prefix}` is listed as kept user-scope hardening but the shipped unit no longer \
             sets it"
        );
    }
}

#[test]
fn user_scope_can_still_read_the_operator_config() {
    // ProtectHome=true would hide ~/.config/all-smi/config.toml from
    // the operator's own service, which defeats the point of a user
    // service. This is the behavioural statement behind the drop.
    let unit = user_unit();
    assert!(!unit.contains("ProtectHome"), "got:\n{unit}");
}

#[test]
fn neither_scope_adds_a_device_or_proc_restriction() {
    // PrivateDevices would hide /dev/nvidia* and /dev/dri from NVML
    // and the AMD/Intel readers; ProtectProc/ProcSubset would hide
    // the processes the process-metrics reader enumerates. Both must
    // stay absent in every rendering.
    for unit in [system_unit(None), system_unit(Some("all-smi")), user_unit()] {
        assert!(!unit.contains("PrivateDevices"), "got:\n{unit}");
        assert!(!unit.contains("ProtectProc"), "got:\n{unit}");
        assert!(!unit.contains("ProcSubset"), "got:\n{unit}");
    }
}

#[test]
fn dropped_prefixes_all_appear_in_the_shipped_unit() {
    // A stale entry in the drop list is silently useless. Every
    // prefix must correspond to a line the template actually has, so
    // a directive renamed in the unit breaks this test instead of
    // quietly surviving into user-scope units.
    for prefix in USER_SCOPE_DROPPED_PREFIXES {
        assert!(
            UNIT_TEMPLATE
                .lines()
                .any(|l| l.trim_start().starts_with(prefix)),
            "`{prefix}` is in the user-scope drop list but no longer exists in the shipped unit"
        );
    }
}

#[test]
fn rendered_unit_ends_with_a_newline() {
    // systemd tolerates a missing trailing newline, but a unit file
    // without one is awkward to append drop-ins to and trips lint.
    assert!(system_unit(None).ends_with('\n'));
    assert!(user_unit().ends_with('\n'));
}

#[test]
fn exec_path_with_spaces_is_quoted() {
    let unit = render_unit(&RenderParams {
        scope: Scope::System,
        exec_path: Path::new("/opt/my tools/all-smi"),
        service_user: None,
    })
    .expect("a path with spaces is representable when quoted");
    assert!(
        unit.contains("ExecStart=\"/opt/my tools/all-smi\" api\n"),
        "got:\n{unit}"
    );
}

#[test]
fn exec_path_percent_is_escaped() {
    let unit = render_unit(&RenderParams {
        scope: Scope::System,
        exec_path: Path::new("/opt/100%/all-smi"),
        service_user: None,
    })
    .expect("percent is escapable");
    assert!(
        unit.contains("ExecStart=/opt/100%%/all-smi api\n"),
        "% starts a systemd specifier and must be doubled, got:\n{unit}"
    );
}

#[test]
fn exec_path_with_a_quote_is_rejected() {
    let err = render_unit(&RenderParams {
        scope: Scope::System,
        exec_path: Path::new("/opt/we\"ird/all-smi"),
        service_user: None,
    })
    .expect_err("an embedded double quote must be refused");
    assert!(matches!(err, RenderError::UnsafePath(_)));
}

#[test]
fn exec_path_with_a_backslash_is_rejected() {
    let err = render_unit(&RenderParams {
        scope: Scope::System,
        exec_path: Path::new("/opt/back\\slash/all-smi"),
        service_user: None,
    })
    .expect_err("a backslash is a systemd escape and must be refused");
    assert!(matches!(err, RenderError::UnsafePath(_)));
}

#[test]
fn template_is_the_shipped_unit_verbatim() {
    // Regression guard: the embedded template must be the file the
    // deb also installs, not a divergent inline copy.
    assert!(UNIT_TEMPLATE.contains("[Unit]"));
    assert!(UNIT_TEMPLATE.contains("ExecStart=/usr/bin/all-smi api"));
    assert!(UNIT_TEMPLATE.contains("EnvironmentFile=-/etc/default/all-smi"));
    assert!(UNIT_TEMPLATE.contains("Type=exec"));
    assert!(UNIT_TEMPLATE.contains("Restart=on-failure"));
    assert!(UNIT_TEMPLATE.contains("RestartSec=5"));
}

#[test]
fn render_is_deterministic() {
    let path = PathBuf::from("/usr/local/bin/all-smi");
    let params = RenderParams {
        scope: Scope::System,
        exec_path: &path,
        service_user: Some("all-smi"),
    };
    assert_eq!(
        render_unit(&params).unwrap(),
        render_unit(&params).unwrap(),
        "reinstall must produce byte-identical output so it is a no-op"
    );
}
