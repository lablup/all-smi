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
//!   the library is never unmapped.
//! - [`shared_nvml`] keeps one `Arc<Nvml>` for every reader in the process,
//!   and after a failed init it refuses to try again until a back-off window
//!   (30 seconds, doubling up to 5 minutes) has passed. During the window the
//!   cached error is returned at once, so callers go straight to their
//!   `nvidia-smi` fallback.
//!
//! The handle lives in a `static` and is never dropped, so NVML is not shut
//! down at exit. NVML documents that skipping `nvmlShutdown` at process exit
//! is safe.

use std::fmt;
#[cfg(any(target_os = "linux", target_os = "windows"))]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
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

/// Holds a library handle opened at most once and never released.
#[cfg_attr(
    not(any(target_os = "linux", target_os = "windows", test)),
    allow(dead_code)
)]
pub(crate) struct LibraryPin<L> {
    cell: OnceLock<Option<L>>,
}

#[cfg_attr(
    not(any(target_os = "linux", target_os = "windows", test)),
    allow(dead_code)
)]
impl<L> LibraryPin<L> {
    pub(crate) const fn new() -> Self {
        Self {
            cell: OnceLock::new(),
        }
    }

    /// Run `open` on the first call only and report whether a handle is held.
    pub(crate) fn ensure(&self, open: impl FnOnce() -> Option<L>) -> bool {
        self.cell.get_or_init(open).is_some()
    }
}

#[cfg(any(target_os = "linux", target_os = "windows"))]
static LIBRARY_PIN: LibraryPin<libloading::Library> = LibraryPin::new();

/// How many times [`pin_library`] has tried to open the library.
#[cfg(any(target_os = "linux", target_os = "windows"))]
static PIN_OPEN_ATTEMPTS: AtomicUsize = AtomicUsize::new(0);

/// Keep `libnvidia-ml` loaded for the life of the process. Returns whether
/// the library is pinned; a missing library is not an error, and NVML init
/// then fails exactly as it would have without the pin.
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
    /// back-off allows it. Init runs under the lock, so concurrent callers
    /// wait for one attempt rather than each making their own.
    fn acquire<E: fmt::Display>(
        &self,
        init: &impl Fn() -> Result<T, E>,
        now: Instant,
    ) -> Result<Arc<T>, SharedNvmlError> {
        let mut state = self.lock();
        if let Some(handle) = &state.handle {
            return Ok(Arc::clone(handle));
        }
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
            Err(e) => {
                let backoff = state
                    .backoff
                    .map_or(INITIAL_BACKOFF, |b| (b * 2).min(MAX_BACKOFF));
                let message = e.to_string();
                state.backoff = Some(backoff);
                state.retry_at = Some(now + backoff);
                state.last_error = Some(message.clone());
                Err(SharedNvmlError { message })
            }
        }
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
    /// straight away; if that fails, the back-off starts. Callers that still
    /// hold the old `Arc` keep it until they release it.
    pub(crate) fn get<E: fmt::Display>(
        &self,
        init: impl Fn() -> Result<T, E>,
        is_live: impl Fn(&T) -> bool,
        now: Instant,
    ) -> Result<Arc<T>, SharedNvmlError> {
        let handle = self.acquire(&init, now)?;
        if is_live(&handle) {
            return Ok(handle);
        }
        self.invalidate(&handle);
        drop(handle);
        self.acquire(&init, now)
    }
}

static SHARED_NVML: NvmlSlot<Nvml> = NvmlSlot::new();

/// The process-wide NVML handle. Pins `libnvidia-ml` first, then returns the
/// shared handle, initialising or re-initialising it as needed. While a
/// failed init is in back-off, returns that failure's error without calling
/// NVML.
pub(crate) fn shared_nvml() -> Result<Arc<Nvml>, SharedNvmlError> {
    pin_library();
    SHARED_NVML.get(
        Nvml::init,
        |nvml| nvml.device_count().is_ok(),
        Instant::now(),
    )
}

#[cfg(test)]
#[path = "nvml_handle_tests.rs"]
mod tests;
