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

//! Process-wide caches behind the process-row builders (issue #504).
//!
//! `get_all_processes` runs once per reader per API interval over every PID.
//! Without these caches each call opened `/proc/<pid>/stat` for every process
//! only to read priority and nice, and resolved every uid through NSS, which
//! is an IPC or network round trip under sssd or LDAP. Both values are fixed
//! for a process's lifetime (see [`PriorityCache`] for the one exception), so
//! they are resolved once and reused.
//!
//! Callers take each cache's lock once per row-building pass, not per PID.

#[cfg(unix)]
use std::collections::HashMap;
#[cfg(unix)]
use std::sync::{LazyLock, Mutex, MutexGuard, PoisonError};

/// Priority and nice per PID, validated by the process start time.
///
/// A PID reused by a new process has a different start time and is read
/// again. A `renice` of a running process is not picked up until it
/// restarts, the same staleness the TUI's process cache already accepts for
/// rows it built once with `new_entry_from_sysinfo`.
#[cfg(target_os = "linux")]
#[derive(Debug, Default)]
pub(super) struct PriorityCache {
    entries: HashMap<u32, (u64, i32, i32)>,
}

#[cfg(target_os = "linux")]
impl PriorityCache {
    /// The cached `(priority, nice)` for `pid`, calling `read` only when the
    /// PID is unknown or now belongs to a process with another start time.
    pub(super) fn get_or_read(
        &mut self,
        pid: u32,
        start_time: u64,
        read: &mut impl FnMut(u32) -> (i32, i32),
    ) -> (i32, i32) {
        match self.entries.get(&pid) {
            Some(&(started, priority, nice)) if started == start_time => (priority, nice),
            _ => {
                let (priority, nice) = read(pid);
                self.entries.insert(pid, (start_time, priority, nice));
                (priority, nice)
            }
        }
    }

    /// Drop the entries of PIDs `live` no longer reports, so the map never
    /// grows past the live process count.
    pub(super) fn retain_live(&mut self, mut live: impl FnMut(u32) -> bool) {
        self.entries.retain(|pid, _| live(*pid));
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(target_os = "linux")]
static PRIORITY_CACHE: LazyLock<Mutex<PriorityCache>> = LazyLock::new(Mutex::default);

/// The process-wide [`PriorityCache`]. A poisoned lock is recovered: the map
/// holds plain values that stay valid after a panic elsewhere.
#[cfg(target_os = "linux")]
pub(super) fn priority_cache() -> MutexGuard<'static, PriorityCache> {
    PRIORITY_CACHE
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// Upper bound on cached uid -> name entries. A host with more distinct
/// process owners than this clears the map and starts over rather than grow
/// without bound.
#[cfg(unix)]
pub(super) const USER_CACHE_CAP: usize = 4096;

/// Outcome of resolving a uid to a user name.
#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum UserLookup {
    /// The passwd entry's name.
    Found(String),
    /// The uid definitively has no usable entry; safe to cache as a number.
    NotFound,
    /// A transient failure (interrupted call, resource exhaustion, ...); the
    /// next call should retry rather than remember the numeric fallback.
    Transient,
}

/// uid -> user name, including uids with no passwd entry (cached as the
/// numeric uid) so an unknown uid does not hit NSS on every call. A transient
/// lookup failure returns the numeric uid without caching it.
#[cfg(unix)]
#[derive(Debug, Default)]
pub(super) struct UserCache {
    names: HashMap<u32, String>,
}

#[cfg(unix)]
impl UserCache {
    /// The name for `uid`, calling `lookup` only on a miss. `NotFound` is
    /// stored as the numeric uid, which is what the row shows; `Transient`
    /// returns the numeric uid without storing it.
    pub(super) fn resolve(&mut self, uid: u32, lookup: impl FnOnce(u32) -> UserLookup) -> String {
        if let Some(name) = self.names.get(&uid) {
            return name.clone();
        }
        let name = match lookup(uid) {
            UserLookup::Found(name) => name,
            UserLookup::NotFound => uid.to_string(),
            UserLookup::Transient => return uid.to_string(),
        };
        if self.names.len() >= USER_CACHE_CAP {
            self.names.clear();
        }
        self.names.insert(uid, name.clone());
        name
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.names.len()
    }
}

#[cfg(unix)]
static USER_CACHE: LazyLock<Mutex<UserCache>> = LazyLock::new(Mutex::default);

/// The process-wide [`UserCache`], recovered from poisoning like
/// [`priority_cache`].
#[cfg(unix)]
pub(super) fn user_cache() -> MutexGuard<'static, UserCache> {
    USER_CACHE.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Resolve a uid through the thread-safe `getpwuid_r`.
///
/// `getpwuid` returns a pointer into static storage, so two readers building
/// rows on different threads could overwrite each other's result.
/// `NotFound` covers a uid with no entry (zero return with a null result, or
/// ENOENT, ESRCH, EBADF, EPERM) and a name that is not UTF-8; any other error
/// is `Transient`.
#[cfg(unix)]
pub(super) fn lookup_user_name(uid: u32) -> UserLookup {
    use std::ffi::CStr;

    // Most passwd entries fit in 1 KiB; a large one (long GECOS, many
    // fields from LDAP) gets one retry with a bigger heap buffer.
    let mut stack_buf = [0 as libc::c_char; 1024];
    let mut heap_buf: Vec<libc::c_char> = Vec::new();
    for attempt in 0..2 {
        let buf: &mut [libc::c_char] = if attempt == 0 {
            &mut stack_buf
        } else {
            heap_buf.resize(64 * 1024, 0);
            &mut heap_buf
        };
        // SAFETY: `passwd` is a plain C struct; all-zero is a valid value and
        // `getpwuid_r` overwrites it before we read it.
        let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
        let mut result: *mut libc::passwd = std::ptr::null_mut();
        // SAFETY: every pointer is valid for the duration of the call and
        // `buf.len()` is the real size of `buf`, which the strings `pwd`
        // points into live in.
        let rc = unsafe {
            libc::getpwuid_r(
                uid as libc::uid_t,
                &mut pwd,
                buf.as_mut_ptr(),
                buf.len(),
                &mut result,
            )
        };
        if rc == libc::ERANGE && attempt == 0 {
            continue;
        }
        if rc != 0 {
            return classify_getpwuid_error(rc);
        }
        if result.is_null() || pwd.pw_name.is_null() {
            return UserLookup::NotFound;
        }
        // SAFETY: on success `pw_name` points to a NUL-terminated string
        // inside `buf`, which is still alive here.
        let name = unsafe { CStr::from_ptr(pwd.pw_name) };
        return name
            .to_str()
            .map_or(UserLookup::NotFound, |n| UserLookup::Found(n.to_string()));
    }
    UserLookup::Transient
}

/// Map a non-zero `getpwuid_r` return code to a cacheable or retryable result.
#[cfg(unix)]
fn classify_getpwuid_error(rc: i32) -> UserLookup {
    match rc {
        libc::ENOENT | libc::ESRCH | libc::EBADF | libc::EPERM => UserLookup::NotFound,
        // EINTR, EIO, EMFILE, ENFILE, ENOMEM, a second ERANGE and anything
        // else may succeed on the next call.
        _ => UserLookup::Transient,
    }
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "linux")]
    #[test]
    fn priority_is_read_once_per_process_and_again_for_a_reused_pid() {
        use super::PriorityCache;

        let mut cache = PriorityCache::default();
        let reads = std::cell::RefCell::new(Vec::new());
        let mut read = |pid: u32| {
            reads.borrow_mut().push(pid);
            (20, pid as i32 % 5)
        };

        let live = [(10u32, 100u64), (11, 110), (12, 120)];
        for _ in 0..2 {
            for (pid, start) in live {
                assert_eq!(
                    cache.get_or_read(pid, start, &mut read),
                    (20, pid as i32 % 5)
                );
            }
        }
        assert_eq!(
            *reads.borrow(),
            vec![10, 11, 12],
            "second pass must not read"
        );

        // PID 11 now belongs to a process that started later.
        cache.get_or_read(11, 999, &mut read);
        assert_eq!(*reads.borrow(), vec![10, 11, 12, 11]);
        cache.get_or_read(11, 999, &mut read);
        assert_eq!(reads.borrow().len(), 4);

        // PID 12 exited: the map shrinks to the live PIDs.
        cache.retain_live(|pid| pid != 12);
        assert_eq!(cache.len(), 2);
    }

    #[cfg(unix)]
    #[test]
    fn user_names_are_resolved_once_and_bounded() {
        use super::{USER_CACHE_CAP, UserCache, UserLookup};

        let mut cache = UserCache::default();
        let mut calls = 0;
        for _ in 0..3 {
            let name = cache.resolve(0, |_| {
                calls += 1;
                UserLookup::Found("root".to_string())
            });
            assert_eq!(name, "root");
        }
        assert_eq!(calls, 1);

        // A uid without a passwd entry is cached as its number.
        let mut misses = 0;
        for _ in 0..2 {
            let name = cache.resolve(424_242, |_| {
                misses += 1;
                UserLookup::NotFound
            });
            assert_eq!(name, "424242");
        }
        assert_eq!(misses, 1);

        // A transient failure returns the number but is retried next call.
        let mut attempts = 0;
        for _ in 0..2 {
            let name = cache.resolve(777, |_| {
                attempts += 1;
                UserLookup::Transient
            });
            assert_eq!(name, "777");
        }
        assert_eq!(attempts, 2);
        assert_eq!(
            cache.resolve(777, |_| UserLookup::Found("late".into())),
            "late"
        );

        for uid in 1..(USER_CACHE_CAP as u32 * 2) {
            cache.resolve(uid, |uid| UserLookup::Found(format!("u{uid}")));
            assert!(cache.len() <= USER_CACHE_CAP);
        }
    }

    #[cfg(unix)]
    #[test]
    fn getpwuid_r_resolves_root() {
        assert_eq!(
            super::lookup_user_name(0),
            super::UserLookup::Found("root".to_string())
        );
    }

    #[cfg(unix)]
    #[test]
    fn getpwuid_r_errors_are_cached_only_when_definitive() {
        use super::{UserLookup, classify_getpwuid_error};

        for rc in [libc::ENOENT, libc::ESRCH, libc::EBADF, libc::EPERM] {
            assert_eq!(classify_getpwuid_error(rc), UserLookup::NotFound);
        }
        for rc in [
            libc::EINTR,
            libc::EIO,
            libc::EMFILE,
            libc::ENFILE,
            libc::ENOMEM,
        ] {
            assert_eq!(classify_getpwuid_error(rc), UserLookup::Transient);
        }
    }
}
