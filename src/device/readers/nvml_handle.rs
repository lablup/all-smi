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
//! - [`pin_library`] opens the library once more and never closes it, so the
//!   load and unload inside `nvml-wrapper` only move a reference count and
//!   the library is never unmapped. It runs only inside a real init attempt,
//!   and a failed pin is retried at the next attempt, so a driver installed
//!   after start is still pinned.
//! - [`shared_nvml`] keeps one `Arc<Nvml>` for every reader in the process,
//!   and after a failed init it refuses to try again until a back-off window
//!   (30 seconds, doubling up to 5 minutes) has passed. During the window the
//!   cached error is returned at once, so callers go straight to their
//!   `nvidia-smi` fallback.
//!
//! The handle lives in a `static` and is never dropped, so NVML is not shut
//! down at exit. This is a design choice: the OS reclaims the driver
//! resources when the process exits, as it already does for any process that
//! exits without calling `nvmlShutdown`.

use std::fmt;
#[cfg(any(target_os = "linux", target_os = "windows"))]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use nvml_wrapper::Nvml;

/// Wait after the first failed init before trying again.
pub(crate) const INITIAL_BACKOFF: Duration = Duration::from_secs(30);

/// Longest wait between failed init attempts.
pub(crate) const MAX_BACKOFF: Duration = Duration::from_secs(300);

/// The library name `nvml-wrapper` 0.13 passes to `dlopen` (its `LIB_PATH`).
/// The pin must use the same name so both handles resolve to one mapping.
#[cfg(target_os = "linux")]
const NVML_LIBRARY: &str = "libnvidia-ml.so.1";
#[cfg(target_os = "windows")]
const NVML_LIBRARY: &str = "nvml.dll";

/// NVML could not be initialised. Displays the NVML error text unchanged, so
/// callers that format or match on it see what `Nvml::init()` reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedNvmlError {
    message: String,
}

impl fmt::Display for SharedNvmlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SharedNvmlError {}

/// Holds a library handle that, once opened, is never reopened or released.
/// While nothing is held, each `ensure` call tries to open it again.
#[cfg_attr(
    not(any(target_os = "linux", target_os = "windows", test)),
    allow(dead_code)
)]
pub(crate) struct LibraryPin<L> {
    cell: Mutex<Option<L>>,
}

#[cfg_attr(
    not(any(target_os = "linux", target_os = "windows", test)),
    allow(dead_code)
)]
impl<L> LibraryPin<L> {
    pub(crate) const fn new() -> Self {
        Self {
            cell: Mutex::new(None),
        }
    }

    /// Report whether a handle is held, running `open` only while none is.
    pub(crate) fn ensure(&self, open: impl FnOnce() -> Option<L>) -> bool {
        let mut cell = self.cell.lock().unwrap_or_else(|p| p.into_inner());
        if cell.is_none() {
            *cell = open();
        }
        cell.is_some()
    }
}

#[cfg(any(target_os = "linux", target_os = "windows"))]
static LIBRARY_PIN: LibraryPin<libloading::Library> = LibraryPin::new();

/// How many times [`pin_library`] has called `dlopen`.
#[cfg(any(target_os = "linux", target_os = "windows"))]
static PIN_OPEN_ATTEMPTS: AtomicUsize = AtomicUsize::new(0);

/// Keep `libnvidia-ml` loaded for the life of the process. Returns whether
/// the library is pinned; a missing library is not an error, and NVML init
/// then fails exactly as it would have without the pin. A held pin is never
/// reopened; a failed one is retried on the next call.
pub(crate) fn pin_library() -> bool {
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    {
        LIBRARY_PIN.ensure(|| {
            PIN_OPEN_ATTEMPTS.fetch_add(1, Ordering::Relaxed);
            // SAFETY: this loads the same library `Nvml::init()` loads right
            // after, so its initialisers run no code that init would not run
            // anyway. No symbol is taken from this handle; it only holds a
            // reference so the library stays mapped.
            unsafe { libloading::Library::new(NVML_LIBRARY) }.ok()
        })
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        false
    }
}

/// Number of attempts [`pin_library`] has made to open the library.
#[cfg(test)]
pub(crate) fn pin_open_attempts() -> usize {
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    {
        PIN_OPEN_ATTEMPTS.load(Ordering::Relaxed)
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        0
    }
}

struct SlotState<T> {
    handle: Option<Arc<T>>,
    /// Message from the last failed init, returned during the back-off.
    last_error: Option<String>,
    /// Earliest time the next init may run after a failure.
    retry_at: Option<Instant>,
    /// Length of the current back-off window; `None` after a success.
    backoff: Option<Duration>,
}

/// The shared-handle state machine, generic over the handle type so tests
/// can drive it with a fake handle, a counting init function, and explicit
/// instants instead of a GPU and the wall clock.
pub(crate) struct NvmlSlot<T> {
    state: Mutex<SlotState<T>>,
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
        }
    }

    fn lock(&self) -> MutexGuard<'_, SlotState<T>> {
        // A panic inside `init` leaves the state consistent (it is written
        // only after init returns), so a poisoned lock is safe to reuse.
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Return the current handle, initialising when there is none and the
    /// back-off allows it; the flag says whether this call ran init. Init runs under the lock, so concurrent callers
    /// wait for one attempt rather than each making their own.
    fn acquire<E: fmt::Display>(
        &self,
        init: &impl Fn() -> Result<T, E>,
        now: Instant,
    ) -> Result<(Arc<T>, bool), SharedNvmlError> {
        let mut state = self.lock();
        if let Some(handle) = &state.handle {
            return Ok((Arc::clone(handle), false));
        }
        Self::initialise(&mut state, init, now).map(|h| (h, true))
    }

    /// Run `init` unless the back-off forbids it. Caller holds the lock and
    /// has found no cached handle.
    fn initialise<E: fmt::Display>(
        state: &mut SlotState<T>,
        init: &impl Fn() -> Result<T, E>,
        now: Instant,
    ) -> Result<Arc<T>, SharedNvmlError> {
        if let (Some(retry_at), Some(message)) = (state.retry_at, &state.last_error)
            && now < retry_at
        {
            return Err(SharedNvmlError {
                message: message.clone(),
            });
        }
        match init() {
            Ok(handle) => {
                let handle = Arc::new(handle);
                state.handle = Some(Arc::clone(&handle));
                state.last_error = None;
                state.retry_at = None;
                state.backoff = None;
                Ok(handle)
            }
            Err(e) => Err(Self::record_failure(state, e.to_string(), now)),
        }
    }

    /// Start or extend the back-off and remember `message` for its duration.
    fn record_failure(state: &mut SlotState<T>, message: String, now: Instant) -> SharedNvmlError {
        let backoff = state
            .backoff
            .map_or(INITIAL_BACKOFF, |b| (b * 2).min(MAX_BACKOFF));
        state.backoff = Some(backoff);
        state.retry_at = Some(now + backoff);
        state.last_error = Some(message.clone());
        SharedNvmlError { message }
    }

    /// Drop `handle` if it is still the cached one and treat the liveness
    /// failure as a failed init, so a handle that initialises but never
    /// answers is not re-initialised on every call.
    fn reject_fresh(&self, handle: &Arc<T>, now: Instant) -> SharedNvmlError {
        let mut state = self.lock();
        if matches!(&state.handle, Some(c) if Arc::ptr_eq(c, handle)) {
            state.handle = None;
        }
        Self::record_failure(
            &mut state,
            "NVML initialised but device_count() failed".to_string(),
            now,
        )
    }

    /// Whether the slot currently caches a handle.
    #[allow(dead_code)] // used by `shared_nvml_present` and tests; the bin target skips both
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

    /// Return a live handle. When `is_live` rejects the shared handle (the
    /// driver was reloaded), drop it from the slot and initialise once more
    /// straight away; if that fails, or the new handle is not live either,
    /// the back-off starts. Callers that still
    /// hold the old `Arc` keep it until they release it.
    pub(crate) fn get<E: fmt::Display>(
        &self,
        init: impl Fn() -> Result<T, E>,
        is_live: impl Fn(&T) -> bool,
        now: Instant,
    ) -> Result<Arc<T>, SharedNvmlError> {
        let (handle, fresh) = self.acquire(&init, now)?;
        if is_live(&handle) {
            return Ok(handle);
        }
        // A handle that was initialised just now and is already dead is a
        // failed init, not a stale handle: do not retry it straight away.
        if fresh {
            return Err(self.reject_fresh(&handle, now));
        }
        self.invalidate(&handle);
        drop(handle);
        let (fresh, _) = self.acquire(&init, now)?;
        if is_live(&fresh) {
            return Ok(fresh);
        }
        Err(self.reject_fresh(&fresh, now))
    }
}

static SHARED_NVML: NvmlSlot<Nvml> = NvmlSlot::new();

/// The process-wide NVML handle, initialising or re-initialising it as
/// needed (each real init pins `libnvidia-ml` first). While a
/// failed init is in back-off, returns that failure's error without calling
/// NVML.
pub(crate) fn shared_nvml() -> Result<Arc<Nvml>, SharedNvmlError> {
    // The pin runs inside the init closure, so it is tried only when init
    // really executes: never during a back-off and never with a cached handle.
    SHARED_NVML.get(
        || {
            pin_library();
            Nvml::init()
        },
        |nvml| nvml.device_count().is_ok(),
        Instant::now(),
    )
}

/// Whether this process currently holds an initialised shared NVML handle.
/// Lets integration tests tell a real NVML host from one that fell back to
/// another reader.
#[doc(hidden)]
#[allow(dead_code)] // consumed by the library's public re-export, not the bin target
pub fn shared_nvml_present() -> bool {
    SHARED_NVML.has_handle()
}

#[cfg(test)]
#[path = "nvml_handle_tests.rs"]
mod tests;
