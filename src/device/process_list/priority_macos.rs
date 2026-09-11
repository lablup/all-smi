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

//! Scheduler priority and nice value of a macOS process, read in-process.
//!
//! Two libc calls per process and nothing else. This module deliberately has
//! no way to start a program: the process list used to run `ps` once for
//! every PID it had not seen before, about 1,070 spawns on the first tick of
//! an M5 Max and one per new PID on every full refresh after that. A test in
//! `process_list` reads this file to keep it that way.

/// `PRI` reported when the priority cannot be read. The process table
/// renders this value dim.
const UNKNOWN_PRIORITY: i32 = 20;

/// Priority and nice value of `pid`, as `(priority, nice)`.
///
/// * Nice comes from `getpriority(PRIO_PROCESS, pid)`, which answers for any
///   user's process. `-1` is a legitimate nice value, so `errno` is cleared
///   first and only a `-1` with `errno` set counts as a failure (the process
///   has exited, `ESRCH`). A failure reads as 0.
/// * Priority is the task's base priority, `pti_priority` from
///   `proc_pidinfo(PROC_PIDTASKINFO)`. Unless all-smi runs as root the kernel
///   answers only for processes the caller may inspect and returns 0 bytes
///   for the rest (334 of 1,020 processes on an M5 Max as a normal user);
///   those read as [`UNKNOWN_PRIORITY`]. An unreadable priority no longer
///   drags the nice value down with it.
///
/// This is not quite the number `ps -o pri` printed before. `/bin/ps` is
/// setuid root and reports the highest current priority among a process's
/// threads, so a UI agent with base priority 31 read 37, 46 or 61 depending
/// on the moment (585 of 1,020 PIDs matched `pti_priority`). The process list
/// stores this value when it first sees a PID and never refreshes it, so the
/// column never tracked those boosts; the base priority is the stable value
/// for that cache to hold.
pub(super) fn priority_nice(pid: u32) -> (i32, i32) {
    (
        base_priority(pid).unwrap_or(UNKNOWN_PRIORITY),
        nice(pid).unwrap_or(0),
    )
}

/// `getpriority` for one process, `None` when the process cannot be found.
fn nice(pid: u32) -> Option<i32> {
    // `who == 0` means the calling process to `getpriority`, not PID 0
    // (`kernel_task`), so that PID cannot be asked about this way.
    if pid == 0 {
        return None;
    }

    // SAFETY: `__error` returns this thread's errno slot, which stays valid
    // for the life of the thread; clearing it is how a -1 nice value is told
    // apart from a failure.
    unsafe { *libc::__error() = 0 };
    // SAFETY: plain integer arguments, no memory is passed.
    let value = unsafe { libc::getpriority(libc::PRIO_PROCESS, pid) };
    // SAFETY: as above, reads this thread's errno slot.
    if value == -1 && unsafe { *libc::__error() } != 0 {
        return None;
    }
    Some(value)
}

/// The task's base scheduler priority, `None` when the kernel will not say.
fn base_priority(pid: u32) -> Option<i32> {
    let pid = libc::c_int::try_from(pid).ok()?;
    let size = std::mem::size_of::<libc::proc_taskinfo>();
    let mut info = std::mem::MaybeUninit::<libc::proc_taskinfo>::zeroed();
    // SAFETY: `info` is a writable buffer of exactly `size` bytes and
    // `proc_pidinfo` writes at most the buffer size it is given.
    let written = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTASKINFO,
            0,
            info.as_mut_ptr().cast(),
            size as libc::c_int,
        )
    };
    if usize::try_from(written).ok()? != size {
        return None;
    }
    // SAFETY: the buffer started zeroed and the kernel filled all of it
    // (checked above); every field is a plain integer.
    Some(unsafe { info.assume_init() }.pti_priority)
}

#[cfg(test)]
#[path = "priority_macos/tests.rs"]
mod tests;
