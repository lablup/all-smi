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

//! One NVML handle for the whole process (issues #488 and #489).
//!
//! `nvml-wrapper` loads `libnvidia-ml` on every `Nvml::init()` and unloads it
//! when the handle drops, including when init fails. Each `nvmlInit_v2`
//! followed by an unload leaves one `eventfd` open in the process, so a
//! caller that builds `AllSmi` repeatedly, or polls while NVML keeps failing
//! to initialise, runs out of file descriptors after about a thousand cycles.
//!
//! Two measures bound that:
//!
//! - [`pin_library`](super::nvml_pin::pin_library) opens the library once more and never closes it, so the
//!   load and unload inside `nvml-wrapper` only move a reference count and
//!   the library is never unmapped. It runs only inside a real init attempt,
//!   and a failed pin is retried at the next attempt, so a driver installed
//!   after start is still pinned. On Windows the pin loads `nvml.dll` from
//!   System32 or the NVSMI directory only, never by bare name, so the copy
//!   `nvml-wrapper` resolves afterwards is the trusted one.
//! - [`shared_nvml`] keeps one `Arc<Nvml>` for every reader in the process,
//!   and after a failed init it refuses to try again until a back-off window
//!   (30 seconds, doubling up to 5 minutes) has passed. During the window the
//!   cached error is returned at once, so callers go straight to their
//!   `nvidia-smi` fallback.
//!
//! Init runs outside the slot's state lock, behind a separate init gate. A
//! caller that finds the gate taken does not wait: it gets an "in progress"
//! error, which is not cached and does not touch the back-off, so a stalled
//! `nvmlInit_v2` delays one thread rather than every reader in the process.
//!
//! The handle lives in a `static` and is never dropped, so NVML is not shut
//! down at exit. This is a design choice: the OS reclaims the driver
//! resources when the process exits, as it already does for any process that
//! exits without calling `nvmlShutdown`.

use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, TryLockError};
use std::time::{Duration, Instant};

use nvml_wrapper::Nvml;

use super::nvml_pin::pin_library;
#[cfg(test)]
pub(crate) use super::nvml_pin::{
    DllCandidate, LibraryPin, pin_open_attempts, windows_dll_candidates,
};

/// Wait after the first failed init before trying again.
pub(crate) const INITIAL_BACKOFF: Duration = Duration::from_secs(30);

/// Longest wait between failed init attempts.
pub(crate) const MAX_BACKOFF: Duration = Duration::from_secs(300);

/// NVML could not be initialised, or another thread is initialising it right
/// now. Displays the NVML error text unchanged, so callers that format or
/// match on it see what `Nvml::init()` reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedNvmlError {
    message: String,
    in_progress: bool,
}

impl SharedNvmlError {
    pub(crate) fn failed(message: String) -> Self {
        Self {
            message,
            in_progress: false,
        }
    }

    pub(crate) fn in_progress() -> Self {
        Self {
            message: "NVML initialisation is in progress on another thread".to_string(),
            in_progress: true,
        }
    }

    /// True when another thread was initialising NVML and this call did not
    /// wait for it. Says nothing about whether NVML works, so callers should
    /// use their fallback for this one call and keep any status they show.
    pub fn is_in_progress(&self) -> bool {
        self.in_progress
    }
}

impl fmt::Display for SharedNvmlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SharedNvmlError {}

struct SlotState<T> {
    handle: Option<Arc<T>>,
    /// Message from the last failed init, returned during the back-off.
    last_error: Option<String>,
    /// Earliest time the next init may run after a failure.
    retry_at: Option<Instant>,
    /// Length of the current back-off window; `None` after a success.
    backoff: Option<Duration>,
}

impl<T> SlotState<T> {
    /// The cached error while `now` is inside the back-off window.
    fn backed_off(&self, now: Instant) -> Option<SharedNvmlError> {
        match (self.retry_at, &self.last_error) {
            (Some(retry_at), Some(message)) if now < retry_at => {
                Some(SharedNvmlError::failed(message.clone()))
            }
            _ => None,
        }
    }
}

/// How an init attempt treats the init gate and the back-off.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Attempt {
    /// Normal reads: skip init during the back-off, and return
    /// "in progress" at once when another thread holds the gate.
    Poll,
    /// The doctor probe: wait for the gate and run init even in back-off.
    Probe,
}

/// The shared-handle state machine, generic over the handle type so tests
/// can drive it with a fake handle, a counting init function, and explicit
/// instants instead of a GPU and the wall clock.
///
/// `state` is held only for short reads and writes, never across `init` or
/// `is_live`. `gate` is held for the whole of an init attempt, so at most one
/// runs at a time.
pub(crate) struct NvmlSlot<T> {
    state: Mutex<SlotState<T>>,
    gate: Mutex<()>,
}

impl<T> NvmlSlot<T> {
    pub(crate) const fn new() -> Self {
        Self {
            state: Mutex::new(SlotState {
                handle: None,
                last_error: None,
                retry_at: None,
                backoff: None,
            }),
            gate: Mutex::new(()),
        }
    }

    fn lock(&self) -> MutexGuard<'_, SlotState<T>> {
        // `init` never runs under this lock and every write is a few plain
        // assignments, so a poisoned lock still holds consistent state.
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Start or extend the back-off and remember `message` for its duration.
    fn record_failure(state: &mut SlotState<T>, message: String, now: Instant) -> SharedNvmlError {
        let backoff = state
            .backoff
            .map_or(INITIAL_BACKOFF, |b| (b * 2).min(MAX_BACKOFF));
        state.backoff = Some(backoff);
        state.retry_at = Some(now + backoff);
        state.last_error = Some(message.clone());
        SharedNvmlError::failed(message)
    }

    /// Run one init attempt under the init gate. The state lock is taken
    /// only to re-check the slot before init and to store the outcome after
    /// it. A handle is cached only once `is_live` accepts it; a freshly
    /// initialised handle that is already dead counts as a failed init.
    fn attempt<E: fmt::Display>(
        &self,
        init: &impl Fn() -> Result<T, E>,
        is_live: &impl Fn(&T) -> bool,
        now: Instant,
        mode: Attempt,
    ) -> Result<Arc<T>, SharedNvmlError> {
        let _gate = match mode {
            Attempt::Poll => match self.gate.try_lock() {
                Ok(guard) => guard,
                // A panicking init left nothing half-written: the state is
                // only touched after init returns.
                Err(TryLockError::Poisoned(p)) => p.into_inner(),
                Err(TryLockError::WouldBlock) => return Err(SharedNvmlError::in_progress()),
            },
            Attempt::Probe => self.gate.lock().unwrap_or_else(|p| p.into_inner()),
        };
        {
            // Another thread may have finished an attempt while this one
            // waited for the gate or between the caller's check and here.
            let state = self.lock();
            if let Some(handle) = &state.handle {
                return Ok(Arc::clone(handle));
            }
            if mode == Attempt::Poll
                && let Some(err) = state.backed_off(now)
            {
                return Err(err);
            }
        }
        let outcome = match init() {
            Ok(handle) if is_live(&handle) => Ok(handle),
            Ok(_dead) => Err("NVML initialised but device_count() failed".to_string()),
            Err(e) => Err(e.to_string()),
        };
        let mut state = self.lock();
        match outcome {
            Ok(handle) => {
                let handle = Arc::new(handle);
                state.handle = Some(Arc::clone(&handle));
                state.last_error = None;
                state.retry_at = None;
                state.backoff = None;
                Ok(handle)
            }
            Err(message) => Err(Self::record_failure(&mut state, message, now)),
        }
    }

    /// The cached handle if it is still live. A dead one is dropped from the
    /// slot (the driver was reloaded); callers that still hold its `Arc` keep
    /// it until they release it.
    fn live_cached(&self, is_live: &impl Fn(&T) -> bool) -> Option<Arc<T>> {
        let cached = self.lock().handle.clone()?;
        if is_live(&cached) {
            return Some(cached);
        }
        self.invalidate(&cached);
        None
    }

    /// Whether the slot currently caches a handle.
    fn has_handle(&self) -> bool {
        self.lock().handle.is_some()
    }

    /// Clear the slot if it still holds `stale`. Returns whether it did; a
    /// caller that lost the race to another invalidation leaves the newer
    /// handle in place.
    fn invalidate(&self, stale: &Arc<T>) -> bool {
        let mut state = self.lock();
        match &state.handle {
            Some(current) if Arc::ptr_eq(current, stale) => {
                state.handle = None;
                true
            }
            _ => false,
        }
    }

    /// Return a live handle. A cached handle that `is_live` rejects is
    /// dropped and replaced by one init attempt straight away. During a
    /// back-off the cached error comes back without calling `init`, and
    /// while another thread is initialising the call returns an "in
    /// progress" error at once instead of waiting.
    pub(crate) fn get<E: fmt::Display>(
        &self,
        init: impl Fn() -> Result<T, E>,
        is_live: impl Fn(&T) -> bool,
        now: Instant,
    ) -> Result<Arc<T>, SharedNvmlError> {
        if let Some(handle) = self.live_cached(&is_live) {
            return Ok(handle);
        }
        if let Some(err) = self.lock().backed_off(now) {
            return Err(err);
        }
        self.attempt(&init, &is_live, now, Attempt::Poll)
    }

    /// Like [`get`](Self::get), but ignores the back-off: unless a live
    /// handle is cached, wait for any running attempt and then make a real
    /// one, recorded in the slot like any other. For diagnostics that must
    /// not report an error several minutes old. Only `doctor` (behind the
    /// `cli` feature) and the tests call it.
    #[cfg_attr(not(feature = "cli"), allow(dead_code))]
    pub(crate) fn probe<E: fmt::Display>(
        &self,
        init: impl Fn() -> Result<T, E>,
        is_live: impl Fn(&T) -> bool,
        now: Instant,
    ) -> Result<Arc<T>, SharedNvmlError> {
        if let Some(handle) = self.live_cached(&is_live) {
            return Ok(handle);
        }
        self.attempt(&init, &is_live, now, Attempt::Probe)
    }
}

static SHARED_NVML: NvmlSlot<Nvml> = NvmlSlot::new();

/// Pin the library, then initialise NVML. Only ever called from inside a real
/// init attempt, so the pin is never tried during a back-off or while a
/// handle is cached.
fn init_nvml() -> Result<Nvml, nvml_wrapper::error::NvmlError> {
    pin_library();
    Nvml::init()
}

fn nvml_is_live(nvml: &Nvml) -> bool {
    nvml.device_count().is_ok()
}

/// The process-wide NVML handle, initialising or re-initialising it as
/// needed. While a failed init is in back-off, returns that failure's error
/// without calling NVML; while another thread is initialising, returns an
/// error whose [`SharedNvmlError::is_in_progress`] is true.
pub(crate) fn shared_nvml() -> Result<Arc<Nvml>, SharedNvmlError> {
    SHARED_NVML.get(init_nvml, nvml_is_live, Instant::now())
}

/// The process-wide NVML handle for `doctor`: ignores the back-off and waits
/// for a running init, so the result reflects NVML now rather than a cached
/// failure up to five minutes old.
#[cfg(feature = "cli")]
pub(crate) fn probe_shared_nvml() -> Result<Arc<Nvml>, SharedNvmlError> {
    SHARED_NVML.probe(init_nvml, nvml_is_live, Instant::now())
}

/// Whether this process currently holds an initialised shared NVML handle.
/// Lets integration tests tell a real NVML host from one that fell back to
/// another reader.
#[doc(hidden)]
pub fn shared_nvml_present() -> bool {
    SHARED_NVML.has_handle()
}

#[cfg(test)]
#[path = "nvml_handle_tests.rs"]
mod tests;
