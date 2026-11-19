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

//! Keeps the NVML library loaded for the life of the process, so the load
//! and unload inside each `Nvml::init()` only move a reference count (#488).
//! See `nvml_handle` for why.

use std::sync::Mutex;
#[cfg(any(target_os = "linux", target_os = "windows"))]
use std::sync::atomic::{AtomicUsize, Ordering};

/// The library name `nvml-wrapper` 0.13 passes to `dlopen` (its `LIB_PATH`).
/// The pin must use the same name so both handles resolve to one mapping.
#[cfg(target_os = "linux")]
const NVML_LIBRARY: &str = "libnvidia-ml.so.1";

/// Module name `nvml-wrapper` 0.13 loads on Windows (its `LIB_PATH`).
#[cfg_attr(not(any(target_os = "windows", test)), allow(dead_code))]
const NVML_DLL: &str = "nvml.dll";

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

/// Where the Windows pin may load `nvml.dll` from, in the order tried.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(not(any(target_os = "windows", test)), allow(dead_code))]
pub(crate) enum DllCandidate {
    /// `nvml.dll` searched in System32 only (modern DCH drivers).
    System32,
    /// An absolute path to `nvml.dll` (the NVSMI directory of older drivers).
    Absolute(String),
}

/// Whether `path` is an absolute drive-letter path such as `C:\Program
/// Files`. Relative paths, drive-relative paths (`C:foo`), and UNC or device
/// paths are rejected, so an odd `ProgramFiles` value cannot point the pin at
/// the current directory or a network share.
#[cfg_attr(not(any(target_os = "windows", test)), allow(dead_code))]
fn is_absolute_drive_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/')
}

/// The places the Windows pin tries, given the `ProgramFiles` environment
/// value: System32 first, then `<ProgramFiles>\NVIDIA Corporation\NVSMI`
/// when `ProgramFiles` is set to an absolute path. Pure, so every platform
/// can test it.
#[cfg_attr(not(any(target_os = "windows", test)), allow(dead_code))]
pub(crate) fn windows_dll_candidates(program_files: Option<&str>) -> Vec<DllCandidate> {
    let mut candidates = vec![DllCandidate::System32];
    if let Some(dir) = program_files.map(str::trim)
        && is_absolute_drive_path(dir)
    {
        let dir = dir.trim_end_matches(['\\', '/']);
        candidates.push(DllCandidate::Absolute(format!(
            "{dir}\\NVIDIA Corporation\\NVSMI\\{NVML_DLL}"
        )));
    }
    candidates
}

#[cfg(any(target_os = "linux", target_os = "windows"))]
static LIBRARY_PIN: LibraryPin<libloading::Library> = LibraryPin::new();

/// How many times [`pin_library`] has tried to open the library.
#[cfg(any(target_os = "linux", target_os = "windows"))]
static PIN_OPEN_ATTEMPTS: AtomicUsize = AtomicUsize::new(0);

#[cfg(target_os = "linux")]
fn open_nvml_library() -> Option<libloading::Library> {
    // SAFETY: this loads the same library `Nvml::init()` loads right after,
    // so its initialisers run no code that init would not run anyway. No
    // symbol is taken from this handle; it only holds a reference so the
    // library stays mapped.
    unsafe { libloading::Library::new(NVML_LIBRARY) }.ok()
}

#[cfg(target_os = "windows")]
fn open_nvml_library() -> Option<libloading::Library> {
    use libloading::os::windows::{
        LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, LOAD_LIBRARY_SEARCH_SYSTEM32, Library,
    };

    let program_files = std::env::var("ProgramFiles").ok();
    windows_dll_candidates(program_files.as_deref())
        .into_iter()
        .find_map(|candidate| {
            // SAFETY: both candidates are the NVIDIA driver's own nvml.dll,
            // the library `Nvml::init()` loads right after, so its
            // initialisers run no code that init would not run anyway. The
            // flags keep the search away from the application and current
            // directories: System32 only for the bare name, and the DLL's
            // own directory plus System32 for its dependencies when loading
            // by absolute path. Because Windows resolves a later bare-name
            // load of an already loaded module to that module, this choice
            // decides which copy `nvml-wrapper` uses. No symbol is taken
            // from this handle; it only holds a reference so the DLL stays
            // loaded.
            let loaded = match candidate {
                DllCandidate::System32 => unsafe {
                    Library::load_with_flags(NVML_DLL, LOAD_LIBRARY_SEARCH_SYSTEM32)
                },
                DllCandidate::Absolute(path) => unsafe {
                    Library::load_with_flags(
                        path,
                        LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32,
                    )
                },
            };
            loaded.ok().map(libloading::Library::from)
        })
}

/// Keep the NVML library loaded for the life of the process. Returns whether
/// it is pinned; a missing library is not an error, and NVML init then
/// proceeds exactly as it would have without the pin. A held pin is never
/// reopened; a failed one is retried on the next call.
pub(crate) fn pin_library() -> bool {
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    {
        LIBRARY_PIN.ensure(|| {
            PIN_OPEN_ATTEMPTS.fetch_add(1, Ordering::Relaxed);
            open_nvml_library()
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
