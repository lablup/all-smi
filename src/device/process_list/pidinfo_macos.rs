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

//! One `proc_pidinfo` read for one process, shared by the priority lookup
//! (`priority_macos`) and the process sampler (`sampler_macos`).
//!
//! The kernel answers these calls only for processes the caller may inspect.
//! As a normal user that is roughly two thirds of the process table (636 of
//! 985 on an M1 Ultra); for the rest it writes nothing and sets `errno` to
//! `EPERM`. A process that has exited, or a zombie, reads as `ESRCH`. The
//! two failures mean different things to a caller that keeps per-process
//! state, so they are told apart here, with `errno` cleared before the call
//! the way `priority_macos::nice` does, and read back before anything else
//! can overwrite it.

use std::mem::MaybeUninit;

/// Why a read produced no structure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PidInfoError {
    /// `ESRCH`: the kernel has no live task for this PID. The process has
    /// exited or is a zombie; either way sysinfo drops it too (its
    /// `proc_pidpath` fallback fails with the same `ESRCH`).
    Gone,
    /// Everything else: `EPERM` for a process the caller may not inspect, a
    /// short read, or a PID outside `c_int`. The process may well be alive,
    /// so a caller must not treat this as an exit.
    Unreadable,
}

/// The structures this module knows how to read, each tied to its flavor.
///
/// Sealed on purpose: `read` zero-fills a `T` and trusts the kernel to have
/// written every byte of it, which is only sound for the plain-data structs
/// listed here.
pub(super) trait PidInfo: Copy + private::Sealed {
    const FLAVOR: libc::c_int;
}

mod private {
    pub trait Sealed {}
    impl Sealed for libc::proc_taskinfo {}
    impl Sealed for libc::proc_bsdinfo {}
    impl Sealed for libc::proc_threadinfo {}
}

impl PidInfo for libc::proc_taskinfo {
    const FLAVOR: libc::c_int = libc::PROC_PIDTASKINFO;
}

impl PidInfo for libc::proc_bsdinfo {
    const FLAVOR: libc::c_int = libc::PROC_PIDTBSDINFO;
}

/// `PROC_PIDTHREADINFO` takes a thread id as its argument. This module always
/// passes 0, exactly as sysinfo does, because the sampler exists to reproduce
/// sysinfo's status column: thread id 0 resolves for about a quarter of the
/// inspectable processes and fails with `ESRCH` for the rest (265 of 636 on
/// an M1 Ultra), and sysinfo reads every such failure as "running".
impl PidInfo for libc::proc_threadinfo {
    const FLAVOR: libc::c_int = libc::PROC_PIDTHREADINFO;
}

/// Read `T`'s flavor of `proc_pidinfo` for `pid`.
pub(super) fn read<T: PidInfo>(pid: u32) -> Result<T, PidInfoError> {
    let Ok(pid) = libc::c_int::try_from(pid) else {
        return Err(PidInfoError::Unreadable);
    };
    let size = std::mem::size_of::<T>();
    let mut info = MaybeUninit::<T>::zeroed();
    // SAFETY: `__error` returns this thread's errno slot, which stays valid
    // for the life of the thread; clearing it is how a failure that leaves
    // errno untouched is told apart from a stale `ESRCH`.
    unsafe { *libc::__error() = 0 };
    // SAFETY: `info` is a writable buffer of exactly `size` bytes and
    // `proc_pidinfo` writes at most the buffer size it is given.
    let written = unsafe {
        libc::proc_pidinfo(
            pid,
            T::FLAVOR,
            0,
            info.as_mut_ptr().cast(),
            size as libc::c_int,
        )
    };
    if usize::try_from(written).ok() == Some(size) {
        // SAFETY: the buffer started zeroed and the kernel filled all of it
        // (checked above); `T` is one of the sealed plain-data structs.
        return Ok(unsafe { info.assume_init() });
    }
    // SAFETY: as above, reads this thread's errno slot.
    if unsafe { *libc::__error() } == libc::ESRCH {
        Err(PidInfoError::Gone)
    } else {
        Err(PidInfoError::Unreadable)
    }
}
