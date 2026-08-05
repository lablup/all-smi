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

//! Unit tests for launchd plist rendering. Kept in a sibling file so the
//! implementation stays under the 500-line soft limit.

use std::path::{Path, PathBuf};

use super::*;

const EXEC: &str = "/opt/all-smi/bin/all-smi";
const SYSTEM_LOG: &str = "/var/log/all-smi/all-smi.log";
const USER_LOG: &str = "/Users/dev/Library/Logs/all-smi/all-smi.log";

/// Keys a `gui/$UID` LaunchAgent applies without any privilege. The
/// complement of [`USER_SCOPE_DROPPED_KEYS`], asserted here as a test
/// contract rather than declared in `plist.rs`, because the renderer
/// never consults it and a `pub` item nothing reads is dead code in the
/// binary target.
const USER_SCOPE_KEPT_KEYS: &[&str] = &[
    "Label",
    "ProgramArguments",
    "RunAtLoad",
    "KeepAlive",
    "ThrottleInterval",
    "ExitTimeOut",
    "ProcessType",
    "WorkingDirectory",
    "SoftResourceLimits",
    "StandardOutPath",
    "StandardErrorPath",
];

fn render(scope: Scope, service_user: Option<&str>) -> String {
    let log = if scope == Scope::User {
        USER_LOG
    } else {
        SYSTEM_LOG
    };
    render_plist(&RenderParams {
        scope,
        exec_path: Path::new(EXEC),
        log_path: Path::new(log),
        service_user,
    })
    .expect("canonical inputs must render")
}

fn has_key(rendered: &str, key: &str) -> bool {
    rendered
        .lines()
        .any(|l| l.trim() == format!("<key>{key}</key>"))
}

// ── template invariants ───────────────────────────────────────────────

#[test]
fn template_is_the_shipped_daemon_plist() {
    // The embedded copy is the packaging artifact, not a second
    // definition: it must stay a valid, self-contained LaunchDaemon.
    assert!(PLIST_TEMPLATE.starts_with("<?xml version=\"1.0\""));
    assert!(PLIST_TEMPLATE.contains("<!DOCTYPE plist"));
    assert!(PLIST_TEMPLATE.contains(&format!("<string>{LABEL}</string>")));
    assert!(PLIST_TEMPLATE.trim_end().ends_with("</plist>"));
}

#[test]
fn template_carries_no_marker_of_its_own() {
    // The packaged file is not "managed by all-smi service"; only the
    // rendered copy is. Otherwise `uninstall` would happily delete a
    // plist an operator installed by hand from the source tree.
    assert!(!is_managed(PLIST_TEMPLATE));
}

#[test]
fn template_omits_hard_resource_limits() {
    // Only root can raise a hard rlimit, and lowering one buys nothing
    // here. Keeping it out means the user scope has one less key to
    // drop and one less way to fail at spawn time.
    assert!(!has_key(PLIST_TEMPLATE, "HardResourceLimits"));
}

// ── marker and provenance ─────────────────────────────────────────────

#[test]
fn rendered_plist_carries_the_marker() {
    for scope in [Scope::System, Scope::User] {
        let out = render(scope, None);
        assert!(is_managed(&out), "{scope} render must carry the marker");
    }
}

#[test]
fn marker_follows_the_xml_declaration() {
    // XML forbids anything before `<?xml ... ?>`, so unlike a systemd
    // unit the marker cannot be the first line. It has to land after
    // the DOCTYPE and before `<plist>`.
    let out = render(Scope::System, None);
    let lines: Vec<&str> = out.lines().collect();
    assert!(lines[0].starts_with("<?xml"));
    assert!(lines[1].starts_with("<!DOCTYPE"));
    assert_eq!(lines[2], MANAGED_MARKER);
    let marker_at = out.find(MANAGED_MARKER).unwrap();
    let plist_at = out.find("<plist").unwrap();
    assert!(marker_at < plist_at);
}

#[test]
fn comments_never_contain_a_double_hyphen() {
    // `--` inside an XML comment is a hard parse error, so a stray one
    // makes launchd reject the whole job. It is an easy thing to
    // reintroduce: the marker and the provenance note are the only
    // free-form prose in the file, and `--force` reads perfectly
    // naturally in either of them.
    let out = render(Scope::System, None);
    let mut inside = false;
    for line in out.lines() {
        let mut body = line;
        if !inside {
            let Some(rest) = line.trim_start().strip_prefix("<!--") else {
                continue;
            };
            inside = true;
            body = rest;
        }
        if let Some(rest) = body.strip_suffix("-->") {
            inside = false;
            body = rest;
        }
        assert!(
            !body.contains("--"),
            "comment body must not contain a double hyphen: {line}"
        );
    }
    assert!(!inside, "an XML comment was opened and never closed");
}

// ── substitutions ─────────────────────────────────────────────────────

#[test]
fn program_arguments_use_the_running_binary_and_api() {
    let out = render(Scope::System, None);
    assert!(out.contains(&format!("<string>{EXEC}</string>")));
    assert!(out.contains("<string>api</string>"));
    // The packaged placeholder must be gone, not merely accompanied.
    assert!(!out.contains("/usr/local/bin/all-smi"));
}

#[test]
fn log_paths_follow_the_scope() {
    let system = render(Scope::System, None);
    assert_eq!(system.matches(SYSTEM_LOG).count(), 2, "stdout and stderr");

    let user = render(Scope::User, None);
    assert_eq!(user.matches(USER_LOG).count(), 2);
    assert!(!user.contains(SYSTEM_LOG));
}

#[test]
fn system_scope_defaults_to_root_and_wheel() {
    let out = render(Scope::System, None);
    assert!(has_key(&out, "UserName"));
    assert!(has_key(&out, "GroupName"));
    assert!(out.contains("<string>root</string>"));
    assert!(out.contains("<string>wheel</string>"));
}

#[test]
fn service_user_replaces_username_and_drops_groupname() {
    // macOS has no convention that an account owns an eponymous group,
    // so mirroring the name the way the systemd renderer does would
    // produce a GroupName that often does not resolve. Omitting the key
    // makes launchd use the account's primary group from the password
    // database.
    let out = render(Scope::System, Some("_all-smi"));
    assert!(out.contains("<string>_all-smi</string>"));
    assert!(has_key(&out, "UserName"));
    assert!(!has_key(&out, "GroupName"));
    assert!(!out.contains("<string>wheel</string>"));
}

// ── the user-scope drop list ──────────────────────────────────────────

#[test]
fn user_scope_drops_every_privileged_key() {
    // This has to fail here, at render time, rather than on an
    // operator's machine. It cannot fail at bootstrap time: launchd
    // accepts these keys in a LaunchAgent and silently ignores them
    // (measured on macOS 26.6, see USER_SCOPE_DROPPED_KEYS), so a
    // regression would ship a plist that claims to run as root while
    // running as the operator, with nothing anywhere reporting it.
    let out = render(Scope::User, None);
    for key in USER_SCOPE_DROPPED_KEYS {
        assert!(
            !has_key(&out, key),
            "{key} cannot take effect in a gui/$UID domain and must not survive into a LaunchAgent"
        );
    }
    // The values have to go with their keys, or the plist is malformed.
    assert!(!out.contains("<string>root</string>"));
    assert!(!out.contains("<string>wheel</string>"));
}

#[test]
fn user_scope_ignores_service_user() {
    // `run()` already warns that --service-user is meaningless in user
    // scope; the renderer must not smuggle it in anyway.
    let out = render(Scope::User, Some("_all-smi"));
    assert!(!out.contains("_all-smi"));
    assert!(!has_key(&out, "UserName"));
}

#[test]
fn user_scope_keeps_every_unprivileged_key() {
    let out = render(Scope::User, None);
    for key in USER_SCOPE_KEPT_KEYS {
        assert!(
            has_key(&out, key),
            "{key} needs no privilege and must survive into a LaunchAgent"
        );
    }
}

#[test]
fn the_two_key_lists_partition_the_template() {
    // Every key in the shipped plist must be classified, so adding one
    // without deciding whether a LaunchAgent can honour it fails here
    // rather than at bootstrap time on an operator's machine.
    let top_level: Vec<&str> = PLIST_TEMPLATE
        .lines()
        .filter(|l| l.starts_with("\t<key>"))
        .filter_map(|l| {
            l.trim()
                .strip_prefix("<key>")
                .and_then(|r| r.strip_suffix("</key>"))
        })
        .collect();
    assert!(!top_level.is_empty());
    for key in top_level {
        assert!(
            USER_SCOPE_DROPPED_KEYS.contains(&key) || USER_SCOPE_KEPT_KEYS.contains(&key),
            "{key} is in the plist but classified by neither list; decide whether a gui/$UID \
             LaunchAgent can apply it"
        );
    }
}

// ── structural soundness ──────────────────────────────────────────────

/// Every `<key>` must be immediately followed by a value element, and
/// every container must close. A renderer that drops a key but keeps its
/// value (or the reverse) produces a plist launchd rejects wholesale,
/// and the operator only finds out at bootstrap time.
fn assert_structurally_sound(rendered: &str, label: &str) {
    const VALUE_OPENERS: &[&str] = &[
        "<string>",
        "<integer>",
        "<real>",
        "<data>",
        "<date>",
        "<true/>",
        "<false/>",
        "<array>",
        "<dict>",
    ];

    let mut depth: i32 = 0;
    let mut expect_value = false;
    for line in rendered.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with("<?") || t.starts_with("<!") {
            continue;
        }
        if expect_value {
            assert!(
                VALUE_OPENERS.iter().any(|o| t.starts_with(o)),
                "{label}: <key> is not followed by a value, found: {t}"
            );
            expect_value = false;
            if !t.starts_with("<array>") && !t.starts_with("<dict>") {
                continue;
            }
        }
        if t.starts_with("<key>") {
            assert!(
                t.ends_with("</key>"),
                "{label}: key element spans lines: {t}"
            );
            expect_value = true;
            continue;
        }
        if t.starts_with("<dict>") || t.starts_with("<array>") {
            depth += 1;
        }
        if t.starts_with("</dict>") || t.starts_with("</array>") {
            depth -= 1;
            assert!(depth >= 0, "{label}: container closed too many times");
        }
    }
    assert!(!expect_value, "{label}: trailing <key> with no value");
    assert_eq!(depth, 0, "{label}: unbalanced containers");
}

#[test]
fn every_render_stays_structurally_sound() {
    for (scope, user) in [
        (Scope::System, None),
        (Scope::System, Some("_all-smi")),
        (Scope::User, None),
        (Scope::User, Some("_all-smi")),
    ] {
        let out = render(scope, user);
        assert_structurally_sound(&out, &format!("{scope} scope, service_user={user:?}"));
    }
    assert_structurally_sound(PLIST_TEMPLATE, "packaged template");
}

#[test]
fn keepalive_survives_as_a_whole_dict() {
    // KeepAlive is the only nested container in the template; a value
    // walker that mishandles nesting truncates the job definition
    // exactly here.
    for scope in [Scope::System, Scope::User] {
        let out = render(scope, None);
        assert!(out.contains("<key>KeepAlive</key>"));
        assert!(out.contains("<key>SuccessfulExit</key>"));
        assert!(out.contains("<false/>"));
    }
}

// ── escaping and rejection ────────────────────────────────────────────

#[test]
fn xml_metacharacters_in_paths_are_escaped() {
    let out = render_plist(&RenderParams {
        scope: Scope::System,
        exec_path: Path::new("/opt/a&b/all-smi"),
        log_path: Path::new("/var/log/<x>/all-smi.log"),
        service_user: None,
    })
    .expect("metacharacters are escapable, not fatal");
    assert!(out.contains("/opt/a&amp;b/all-smi"));
    assert!(out.contains("/var/log/&lt;x&gt;/all-smi.log"));
    assert!(!out.contains("/opt/a&b/"));
}

#[test]
fn spaces_in_paths_need_no_quoting() {
    // Unlike a systemd ExecStart=, ProgramArguments is a real array, so
    // a space is just a character.
    let out = render_plist(&RenderParams {
        scope: Scope::System,
        exec_path: Path::new("/Applications/All SMI.app/Contents/MacOS/all-smi"),
        log_path: Path::new(SYSTEM_LOG),
        service_user: None,
    })
    .expect("a path with a space must render");
    assert!(out.contains("<string>/Applications/All SMI.app/Contents/MacOS/all-smi</string>"));
}

#[test]
fn control_characters_in_paths_are_rejected() {
    let err = render_plist(&RenderParams {
        scope: Scope::System,
        exec_path: Path::new("/opt/all\nsmi"),
        log_path: Path::new(SYSTEM_LOG),
        service_user: None,
    })
    .expect_err("XML 1.0 cannot represent a control character at all");
    assert!(matches!(err, RenderError::UnsafePath(_)));
}

#[test]
fn a_hostile_account_name_is_rejected() {
    for name in ["", "root</string><key>X</key><string>y", "a b", "root;id"] {
        let err = render_plist(&RenderParams {
            scope: Scope::System,
            exec_path: Path::new(EXEC),
            log_path: Path::new(SYSTEM_LOG),
            service_user: Some(name),
        })
        .expect_err("only POSIX-portable account names may reach a root-owned plist");
        assert!(matches!(err, RenderError::UnsafeAccount(_)), "name: {name}");
    }
}

#[test]
fn ordinary_account_names_are_accepted() {
    for name in ["_all-smi", "allsmi", "all.smi", "svc_1"] {
        render_plist(&RenderParams {
            scope: Scope::System,
            exec_path: Path::new(EXEC),
            log_path: Path::new(SYSTEM_LOG),
            service_user: Some(name),
        })
        .unwrap_or_else(|e| panic!("{name} must be accepted: {e}"));
    }
}

#[test]
fn non_utf8_paths_are_rejected() {
    // Only constructible on Unix; on Windows every OsStr is WTF-16 and
    // `to_str` failure cannot be forced this way.
    #[cfg(unix)]
    {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        let bad = PathBuf::from(OsString::from_vec(vec![0x2f, 0xff, 0xfe]));
        let err = render_plist(&RenderParams {
            scope: Scope::System,
            exec_path: &bad,
            log_path: Path::new(SYSTEM_LOG),
            service_user: None,
        })
        .expect_err("a non-UTF-8 path cannot be written into XML");
        assert!(matches!(err, RenderError::NonUtf8Path(_)));
    }
    #[cfg(not(unix))]
    let _ = PathBuf::new();
}

// ── marker detection ──────────────────────────────────────────────────

#[test]
fn is_managed_tolerates_indentation_but_not_a_near_miss() {
    assert!(is_managed(&format!(
        "<plist>\n  {MANAGED_MARKER}\n</plist>"
    )));
    assert!(!is_managed("<!-- Managed by homebrew -->"));
    assert!(!is_managed("<plist version=\"1.0\"><dict/></plist>"));
}
