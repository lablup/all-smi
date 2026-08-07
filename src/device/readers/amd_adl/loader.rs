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

//! Runtime loading of `atiadlxx.dll` and the PMLog query.
//!
//! ## Nothing is linked
//!
//! The DLL is opened with `libloading` at first use. No import library
//! is referenced, so the executable carries no `atiadlxx` entry in its
//! import table and a machine without AMD's driver starts normally and
//! simply reports no ADL data. This is the same shape issue #345 asks
//! for on Linux, where `libamdgpu_top` links `libdrm` unconditionally
//! and a missing library is a loader error before `main`.
//!
//! ## DLL hijacking
//!
//! The library is loaded from an absolute `System32` path and never by
//! bare name, so the search order (which includes the application
//! directory and, in some configurations, the current directory) cannot
//! be used to substitute a different binary. Same stance as
//! `device/windows_temp/amd_ryzen.rs`.
//!
//! ## Cost per poll
//!
//! The DLL load and `ADL2_Main_Control_Create` happen once for the
//! process. The capability scan that picks an adapter index happens once
//! and is then cached. A steady-state poll is therefore a single
//! `ADL2_New_QueryPMLogData_Get`, which reads the telemetry block the
//! driver already maintains for its own overlay and submits no work to
//! the GPU.

use super::ffi::{
    ADL_OK, Adl2AdapterNumberOfAdaptersGet, Adl2MainControlCreate, Adl2NewQueryPmLogDataGet,
    Adl2OverdriveCaps, AdlContextHandle, AdlPmLogDataOutput,
};
use once_cell::sync::OnceCell;
use std::ffi::{c_int, c_void};
use std::sync::Mutex;

/// Absolute path, never a bare name. See the module docs.
const ADL_DLL_PATH: &str = r"C:\Windows\System32\atiadlxx.dll";

/// The Overdrive generation that introduced the PMLog sensor table.
/// Earlier generations expose temperature through the OD5 / OD6 / OD7
/// entry points, which this reader deliberately does not implement.
const MIN_OVERDRIVE_VERSION: c_int = 7;

/// Allocator handed to `ADL2_Main_Control_Create`.
///
/// ADL only calls this for buffers it allocates on the caller's behalf,
/// which in practice is the `AdapterInfo` family. This reader never
/// calls into that family, so the callback is expected to stay
/// unused; it exists because `ADL2_Main_Control_Create` requires a
/// non-null callback.
///
/// # Safety
///
/// Called by ADL with a byte count. Returns null on a non-positive or
/// unrepresentable size, which ADL treats as allocation failure.
unsafe extern "system" fn adl_malloc(size: c_int) -> *mut c_void {
    if size <= 0 {
        return std::ptr::null_mut();
    }
    // 16-byte alignment covers every ADL struct. The matching free is
    // ADL's contract for the caller to provide and is not wired up,
    // because nothing here requests an ADL-allocated buffer.
    match std::alloc::Layout::from_size_align(size as usize, 16) {
        // SAFETY: layout has a non-zero size.
        Ok(layout) => unsafe { std::alloc::alloc(layout) as *mut c_void },
        Err(_) => std::ptr::null_mut(),
    }
}

struct AdlRuntime {
    /// Kept alive for the process. The function pointers below are only
    /// valid while the library stays loaded.
    _library: libloading::Library,
    context: AdlContextHandle,
    number_of_adapters: Adl2AdapterNumberOfAdaptersGet,
    overdrive_caps: Adl2OverdriveCaps,
    query_pmlog: Adl2NewQueryPmLogDataGet,
    /// Adapter index chosen by the first capability scan.
    ///
    /// A single card exposes several ADL adapter indices, one per
    /// display output, all reporting the same telemetry. Caching the
    /// first PMLog-capable index avoids rescanning every poll and keeps
    /// the steady-state cost to one call.
    chosen_index: Option<c_int>,
    /// Set once the capability scan has run, so a machine with no
    /// PMLog-capable adapter does not rescan forever.
    scanned: bool,
}

// ADL context handles are plain opaque pointers rather than thread-affine
// resources; the wrapping `Mutex` serialises actual use.
unsafe impl Send for AdlRuntime {}

impl AdlRuntime {
    fn open() -> Option<Self> {
        // SAFETY: loading a shared library runs its initialisers. The
        // path is absolute and inside System32, so only a caller who
        // already has write access there could substitute the binary.
        let library = unsafe { libloading::Library::new(ADL_DLL_PATH) }.ok()?;

        // SAFETY: symbol lookups against a successfully loaded library.
        // The signatures are transcribed from AMD's public headers; see
        // the `ffi` module.
        let (create, number_of_adapters, overdrive_caps, query_pmlog) = unsafe {
            let create = *library
                .get::<Adl2MainControlCreate>(b"ADL2_Main_Control_Create\0")
                .ok()?;
            let number_of_adapters = *library
                .get::<Adl2AdapterNumberOfAdaptersGet>(b"ADL2_Adapter_NumberOfAdapters_Get\0")
                .ok()?;
            let overdrive_caps = *library
                .get::<Adl2OverdriveCaps>(b"ADL2_Overdrive_Caps\0")
                .ok()?;
            let query_pmlog = *library
                .get::<Adl2NewQueryPmLogDataGet>(b"ADL2_New_QueryPMLogData_Get\0")
                .ok()?;
            (create, number_of_adapters, overdrive_caps, query_pmlog)
        };

        let mut context: AdlContextHandle = std::ptr::null_mut();
        // The `1` requests enumeration of connected adapters only.
        // SAFETY: `adl_malloc` matches ADL_MAIN_MALLOC_CALLBACK and
        // `context` is a valid out pointer.
        let status = unsafe { create(adl_malloc, 1, &mut context) };
        if status != ADL_OK || context.is_null() {
            return None;
        }

        Some(Self {
            _library: library,
            context,
            number_of_adapters,
            overdrive_caps,
            query_pmlog,
            chosen_index: None,
            scanned: false,
        })
    }

    /// Find the first adapter index whose Overdrive generation exposes
    /// the PMLog table.
    fn scan_for_capable_adapter(&mut self) {
        self.scanned = true;

        let mut count: c_int = 0;
        // SAFETY: valid context and out pointer.
        if unsafe { (self.number_of_adapters)(self.context, &mut count) } != ADL_OK || count <= 0 {
            return;
        }

        for index in 0..count {
            let (mut supported, mut enabled, mut version) = (0, 0, 0);
            // SAFETY: valid context and three out pointers.
            let status = unsafe {
                (self.overdrive_caps)(
                    self.context,
                    index,
                    &mut supported,
                    &mut enabled,
                    &mut version,
                )
            };
            if status != ADL_OK || supported == 0 || version < MIN_OVERDRIVE_VERSION {
                continue;
            }
            // `enabled` reports whether the user has turned Overdrive
            // tuning on. Reading sensors does not require it, so it is
            // deliberately not part of the gate.
            if self.read_pmlog(index).is_some() {
                self.chosen_index = Some(index);
                return;
            }
        }
    }

    fn read_pmlog(&self, index: c_int) -> Option<AdlPmLogDataOutput> {
        let mut output = AdlPmLogDataOutput::default();
        // SAFETY: `output` is a correctly sized and aligned
        // ADLPMLogDataOutput; the compile-time assertions in `ffi` pin
        // its layout.
        let status = unsafe { (self.query_pmlog)(self.context, index, &mut output) };
        (status == ADL_OK).then_some(output)
    }

    fn sample(&mut self) -> Option<AdlPmLogDataOutput> {
        if !self.scanned {
            self.scan_for_capable_adapter();
        }
        self.read_pmlog(self.chosen_index?)
    }
}

/// Process-wide runtime. `None` once loading has failed, so a machine
/// without AMD's driver pays the failed-open cost exactly once.
static RUNTIME: OnceCell<Mutex<Option<AdlRuntime>>> = OnceCell::new();

fn runtime() -> &'static Mutex<Option<AdlRuntime>> {
    RUNTIME.get_or_init(|| Mutex::new(AdlRuntime::open()))
}

/// Take one PMLog sample, or `None` when ADL is unavailable or no
/// adapter exposes the sensor table.
pub fn sample() -> Option<AdlPmLogDataOutput> {
    let mut guard = match runtime().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard.as_mut()?.sample()
}

/// Whether `atiadlxx.dll` loaded and a context was created. Used by the
/// doctor check to separate "no AMD driver" from "driver present but no
/// PMLog-capable adapter".
pub fn library_available() -> bool {
    match runtime().lock() {
        Ok(guard) => guard.is_some(),
        Err(poisoned) => poisoned.into_inner().is_some(),
    }
}

/// The adapter index the capability scan selected, if any.
pub fn selected_adapter_index() -> Option<i32> {
    let mut guard = match runtime().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    let runtime = guard.as_mut()?;
    if !runtime.scanned {
        runtime.scan_for_capable_adapter();
    }
    runtime.chosen_index
}

/// The absolute path the loader uses, for diagnostics.
pub fn dll_path() -> &'static str {
    ADL_DLL_PATH
}
