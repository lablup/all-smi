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

use super::*;
use crate::device::process_list::get_process_priority_nice;
use crate::utils::command::new_command;
use std::time::{Duration, Instant};

/// The lookup behind the process list's priority column on macOS.
fn lookup(pid: u32) -> (i32, i32) {
    get_process_priority_nice(pid)
}

/// `getpriority` called directly, with the same errno handling.
fn direct_nice(pid: u32) -> Option<i32> {
    // SAFETY: the same calls `nice` makes: clear this thread's errno slot,
    // call `getpriority` with plain integers, then read errno back.
    unsafe {
        *libc::__error() = 0;
        let value = libc::getpriority(libc::PRIO_PROCESS, pid);
        (value != -1 || *libc::__error() == 0).then_some(value)
    }
}

/// The macOS lookup must never reach `new_command` or anything else that
/// starts a program: the process list calls it for every PID it has not seen,
/// which used to mean about 1,070 `ps` spawns on the first tick of an M5 Max.
///
/// Changing `PATH` to hide `ps` is not an option in a multi-threaded test
/// binary, so the property is pinned structurally instead. The implementation
/// lives in its own file, the process list's macOS lookup is a one-line
/// delegation to it, and neither contains a way to spawn anything.
#[test]
fn the_macos_lookup_cannot_start_a_process() {
    let implementation = include_str!("../priority_macos.rs");
    let process_list = include_str!("../../process_list.rs");
    let delegation = process_list
        .split("#[cfg(target_os = \"macos\")]\nfn get_process_priority_nice")
        .nth(1)
        .and_then(|rest| rest.split("\n}\n").next())
        .expect("the macOS get_process_priority_nice definition");

    for (name, source) in [
        ("priority_macos.rs", implementation),
        ("macOS get_process_priority_nice", delegation),
    ] {
        for needle in [
            "new_command",
            "Command",
            "process::",
            "posix_spawn",
            "fork(",
            "execv",
            "system(",
        ] {
            assert!(!source.contains(needle), "{name} mentions `{needle}`");
        }
    }
    assert!(
        delegation.contains("priority_macos::priority_nice(pid)"),
        "the process list must delegate to the in-process lookup"
    );
}

/// The nice value is `getpriority`'s answer for the same process.
#[test]
fn nice_matches_getpriority_for_this_process() {
    let pid = std::process::id();
    let (priority, nice) = lookup(pid);

    assert_eq!(Some(nice), direct_nice(pid));
    // Our own task is always inspectable, so the priority is a real Mach
    // priority rather than the unknown fallback's stand-in.
    assert!((0..=127).contains(&priority), "priority {priority}");
}

/// A child started under `nice -n 7` reads its parent's nice plus 7.
///
/// `nice -n` is relative to the caller, and the caller is not always at
/// nice 0: a hosted CI runner starts jobs at nice -10, where the child reads
/// -3. The expectation is computed from this process's own value, capped at
/// `PRIO_MAX`, so the assertion is the same on a workstation (0 + 7).
#[test]
fn reads_the_nice_value_of_a_reniced_child() {
    let Some(own_nice) = direct_nice(std::process::id()) else {
        return;
    };
    // `PRIO_MAX` (20) is the top of the nice range; libc does not export it.
    let expected = (own_nice + 7).min(20);

    let Ok(mut child) = new_command("/usr/bin/nice")
        .args(["-n", "7", "/bin/sleep", "10"])
        .spawn()
    else {
        return;
    };
    let pid = child.id();

    // `nice` sets the value and then execs `sleep` under the same PID, so
    // poll until the child has got that far.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut seen = lookup(pid);
    while seen.1 != expected && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
        seen = lookup(pid);
    }

    let _ = child.kill();
    let _ = child.wait();
    assert_eq!(
        seen.1, expected,
        "child read as {seen:?} with the parent at nice {own_nice}"
    );
}

/// A PID that no longer exists falls back to the unknown pair instead of
/// failing or reporting the caller's own values.
#[test]
fn an_exited_process_falls_back() {
    let Ok(mut child) = new_command("/usr/bin/true").spawn() else {
        return;
    };
    let pid = child.id();
    let _ = child.wait();

    assert_eq!(lookup(pid), (UNKNOWN_PRIORITY, 0));
}

/// PID 0 is `kernel_task`; `getpriority` would read `who == 0` as the caller.
#[test]
fn pid_zero_is_not_read_as_the_caller() {
    assert_eq!(nice(0), None);
}
