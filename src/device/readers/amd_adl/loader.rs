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
//! and is then cached, as is the `AdapterInfo` inventory the multi-GPU
//! path matches against (both on the same slow refresh interval). A
//! steady-state poll is therefore one `ADL2_New_QueryPMLogData_Get` on
//! a single-GPU host, and one per matched card on a multi-GPU host,
//! each reading the telemetry block the driver already maintains for
//! its own overlay; nothing submits work to the GPU.

use super::adapters::{self, AdlAdapter};
use super::ffi::{
    ADL_OK, AdapterInfo, AdapterInfoArray, Adl2AdapterAdapterInfoGet,
    Adl2AdapterNumberOfAdaptersGet, Adl2MainControlCreate, Adl2NewQueryPmLogDataGet,
    Adl2OverdriveCaps, AdlContextHandle, AdlPmLogDataBuffer, AdlPmLogDataOutput,
};
use libloading::os::windows::LOAD_LIBRARY_SEARCH_SYSTEM32;
use once_cell::sync::OnceCell;
use std::ffi::{c_int, c_void};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use windows::Win32::System::Memory::{GetProcessHeap, HEAP_FLAGS, HeapAlloc};

/// Absolute path, never a bare name. See the module docs.
const ADL_DLL_PATH: &str = r"C:\Windows\System32\atiadlxx.dll";

/// The Overdrive generation that introduced the PMLog sensor table.
/// Used only to *rank* candidate adapters, never to exclude one; see
/// [`AdlRuntime::scan_for_capable_adapter`].
const MIN_OVERDRIVE_VERSION: c_int = 7;

/// How long to wait before re-scanning after a scan found nothing.
///
/// The scan must not latch permanently. all-smi can run as a Windows
/// service and start before the display driver has brought the adapter
/// up, in which case the very first scan legitimately finds nothing; a
/// permanent latch would then report "no PMLog-capable adapter" for the
/// entire lifetime of the process and misdiagnose a healthy card as
/// pre-Vega. The same applies after a driver update or a TDR reset
/// invalidates the cached index.
///
/// This is the ADL counterpart to the DXGI factory's `IsCurrent` check.
/// ADL exposes no staleness signal, so a slow retry stands in for one.
const RESCAN_INTERVAL: Duration = Duration::from_secs(60);

/// Allocator handed to `ADL2_Main_Control_Create`.
///
/// Allocates from the process heap rather than Rust's global allocator.
/// Two reasons, both about who might free this memory: swapping in a
/// different `#[global_allocator]` (routine in a polling daemon) would
/// make Rust's allocator incompatible with the CRT `free` that ADL's own
/// teardown would use, and `HeapAlloc(GetProcessHeap(), ...)` is what
/// the UCRT allocator is built on, so it stays compatible either way.
///
/// ADL only invokes this for buffers it allocates on the caller's
/// behalf, such as `ADL2_Adapter_AdapterInfoX2_Get`. This reader calls
/// none of those: even its `ADL2_Adapter_AdapterInfo_Get` use hands ADL
/// a caller-allocated buffer, so in practice the callback should never
/// run. It exists because `ADL2_Main_Control_Create` requires a
/// non-null callback.
///
/// # Safety
///
/// Called by ADL with a byte count. Returns null on a non-positive size,
/// which ADL treats as allocation failure.
unsafe extern "system" fn adl_malloc(size: c_int) -> *mut c_void {
    if size <= 0 {
        return std::ptr::null_mut();
    }
    // SAFETY: GetProcessHeap cannot fail for the current process, and
    // HeapAlloc with a positive size either returns a valid block or
    // null.
    unsafe {
        let Ok(heap) = GetProcessHeap() else {
            return std::ptr::null_mut();
        };
        HeapAlloc(heap, HEAP_FLAGS(0), size as usize)
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
    /// `ADL2_Adapter_AdapterInfo_Get`, or `None` when the installed
    /// driver does not export it. Looked up leniently, unlike the
    /// mandatory symbols above: a driver without it simply cannot
    /// attribute on multi-GPU hosts, and must not lose the single-GPU
    /// sensor path over that.
    adapter_info_get: Option<Adl2AdapterAdapterInfoGet>,
    /// Adapter index chosen by the first capability scan.
    ///
    /// A single card exposes several ADL adapter indices, one per
    /// display output, all reporting the same telemetry. Caching the
    /// first PMLog-capable index avoids rescanning every poll and keeps
    /// the steady-state cost to one call.
    chosen_index: Option<c_int>,
    /// When the last capability scan ran, or `None` if it never has.
    ///
    /// Deliberately a timestamp rather than a boolean latch: a scan that
    /// finds nothing is retried after [`RESCAN_INTERVAL`] instead of
    /// disabling ADL for the life of the process. See the constant for
    /// why that matters.
    last_scan: Option<Instant>,
    /// Validated adapter inventory, or `None` when the last fetch
    /// failed (entry point missing, call error, or layout
    /// verification rejecting the rows).
    adapter_inventory: Option<Vec<AdlAdapter>>,
    /// When the inventory was last fetched. Refreshed after
    /// [`RESCAN_INTERVAL`] whether the fetch succeeded or not: adapter
    /// topology changes (driver update, TDR reset, eGPU hotplug)
    /// invalidate a success just as a service starting before the
    /// driver invalidates a failure, and the refresh is one
    /// registry-backed call per minute.
    inventory_scanned_at: Option<Instant>,
}

// ADL context handles are plain opaque pointers rather than thread-affine
// resources; the wrapping `Mutex` serialises actual use.
unsafe impl Send for AdlRuntime {}

impl AdlRuntime {
    fn open() -> Option<Self> {
        // SAFETY: loading a shared library runs its initialisers.
        //
        // The absolute path pins `atiadlxx.dll` itself, but with default
        // flags that module's own imports (it pulls in `atiadlxy.dll`)
        // resolve through the standard search order, which begins with
        // the *application* directory. That would leave the hijacking
        // hole this module claims to close: an attacker able to write
        // next to `all-smi.exe` gets execution through the dependency
        // rather than through the named DLL.
        //
        // `LOAD_LIBRARY_SEARCH_SYSTEM32` restricts the whole dependency
        // chain to System32.
        let library = unsafe {
            libloading::os::windows::Library::load_with_flags(
                ADL_DLL_PATH,
                LOAD_LIBRARY_SEARCH_SYSTEM32,
            )
        }
        .ok()?;
        let library: libloading::Library = library.into();

        // SAFETY: symbol lookups against a successfully loaded library.
        // The signatures are transcribed from AMD's public headers; see
        // the `ffi` module.
        let (create, number_of_adapters, overdrive_caps, query_pmlog, adapter_info_get) = unsafe {
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
            // Optional: absent on drivers old enough, and only needed
            // for multi-GPU attribution.
            let adapter_info_get = library
                .get::<Adl2AdapterAdapterInfoGet>(b"ADL2_Adapter_AdapterInfo_Get\0")
                .ok()
                .map(|symbol| *symbol);
            (
                create,
                number_of_adapters,
                overdrive_caps,
                query_pmlog,
                adapter_info_get,
            )
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
            adapter_info_get,
            chosen_index: None,
            last_scan: None,
            adapter_inventory: None,
            inventory_scanned_at: None,
        })
    }

    /// Find an adapter index that answers `ADL2_New_QueryPMLogData_Get`.
    ///
    /// `ADL2_Overdrive_Caps` is consulted but is deliberately **not** a
    /// gate. Workstation SKUs and APUs commonly report `iSupported = 0`,
    /// meaning "no user-facing tuning", while still serving the sensor
    /// table perfectly well; excluding them would silently drop exactly
    /// the professional cards this tool is aimed at. A successful PMLog
    /// read is the only capability test that means anything, so caps
    /// only decides which indices to try *first*.
    fn scan_for_capable_adapter(&mut self) {
        self.last_scan = Some(Instant::now());
        self.chosen_index = None;

        let mut count: c_int = 0;
        // SAFETY: valid context and out pointer.
        if unsafe { (self.number_of_adapters)(self.context, &mut count) } != ADL_OK || count <= 0 {
            return;
        }
        // Bound the driver's count the way `probe_adapter_info` does;
        // see `adapters::clamp_scan_count` for why this clamps rather
        // than rejects.
        let count = adapters::clamp_scan_count(count);

        let mut preferred = Vec::new();
        let mut fallback = Vec::new();
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
            // `enabled` reports whether the user turned Overdrive tuning
            // on. Reading sensors does not require it, so it is ignored.
            let _ = enabled;
            if status == ADL_OK && supported != 0 && version >= MIN_OVERDRIVE_VERSION {
                preferred.push(index);
            } else {
                fallback.push(index);
            }
        }

        for index in preferred.into_iter().chain(fallback) {
            if self.read_pmlog(index).is_some() {
                self.chosen_index = Some(index);
                return;
            }
        }
    }

    fn read_pmlog(&self, index: c_int) -> Option<AdlPmLogDataOutput> {
        // Boxed rather than a stack temporary: the buffer carries
        // trailing headroom (see `AdlPmLogDataBuffer`) so a driver that
        // writes a larger table than this file declares cannot smash
        // the stack of a long-running daemon.
        let mut buffer = Box::<AdlPmLogDataBuffer>::default();
        // The pointer is derived from the whole padded buffer and then
        // narrowed by a cast, not from the `output` field: a pointer
        // derived from the field alone is valid only for that field's
        // 2052 bytes, which would put the headroom out of bounds for
        // exactly the oversized write the headroom exists to absorb.
        let output = (&raw mut *buffer).cast::<AdlPmLogDataOutput>();
        // SAFETY: `output` addresses the start of a correctly aligned
        // allocation at least as large as ADLPMLogDataOutput, with
        // headroom beyond it. `AdlPmLogDataBuffer` is `#[repr(C)]` with
        // `output` first, which the `ffi` tests pin, and the
        // compile-time assertions in `ffi` pin the layout itself.
        let status = unsafe { (self.query_pmlog)(self.context, index, output) };
        if status != ADL_OK {
            return None;
        }
        buffer.validated().copied()
    }

    fn sample(&mut self) -> Option<AdlPmLogDataOutput> {
        let due_for_scan = match (self.chosen_index, self.last_scan) {
            // Never scanned.
            (_, None) => true,
            // A previous scan found nothing; retry on the slow interval
            // rather than giving up for the life of the process.
            (None, Some(at)) => at.elapsed() >= RESCAN_INTERVAL,
            (Some(_), _) => false,
        };
        if due_for_scan {
            self.scan_for_capable_adapter();
        }

        let index = self.chosen_index?;
        match self.read_pmlog(index) {
            Some(output) => Some(output),
            None => {
                // The cached index stopped answering: a driver update or
                // a TDR reset can invalidate it. Drop it so the next
                // poll rescans instead of failing forever.
                self.chosen_index = None;
                self.last_scan = None;
                None
            }
        }
    }

    /// One raw `AdapterInfo` fetch, uncached.
    fn probe_adapter_info(&self) -> AdapterProbe {
        let Some(adapter_info_get) = self.adapter_info_get else {
            return AdapterProbe::NoEntryPoint;
        };
        let mut count: c_int = 0;
        // SAFETY: valid context and out pointer.
        if unsafe { (self.number_of_adapters)(self.context, &mut count) } != ADL_OK
            || !adapters::plausible_adapter_count(count)
        {
            return AdapterProbe::CallFailed;
        }
        let mut buffer = AdapterInfoArray::for_count(count as usize);
        // Read before the pointer is taken so no borrow of `buffer` is
        // created while the pointer ADL writes through is live.
        let input_size = buffer.input_size();
        // SAFETY: `buffer` starts with at least `count` zeroed
        // `AdapterInfo` entries plus headroom (see `AdapterInfoArray`),
        // `input_size` reports the requested size, and the compile-time
        // assertions in `ffi` pin the layout the driver will write.
        let status = unsafe { adapter_info_get(self.context, buffer.as_mut_ptr(), input_size) };
        if status != ADL_OK {
            return AdapterProbe::CallFailed;
        }
        let layout_ok = buffer.validated().is_some();
        AdapterProbe::Rows {
            rows: buffer.requested_entries().to_vec(),
            layout_ok,
        }
    }

    /// The validated adapter inventory, refreshed on the slow interval.
    ///
    /// `None` covers every way of not having one: the entry point is
    /// missing, the call failed, or layout verification rejected the
    /// rows. All of them mean multi-GPU attribution declines, which is
    /// the designed failure mode.
    fn adapter_inventory(&mut self) -> Option<&[AdlAdapter]> {
        let due = match self.inventory_scanned_at {
            None => true,
            Some(at) => at.elapsed() >= RESCAN_INTERVAL,
        };
        if due {
            self.inventory_scanned_at = Some(Instant::now());
            self.adapter_inventory = match self.probe_adapter_info() {
                AdapterProbe::Rows {
                    rows,
                    layout_ok: true,
                } => Some(adapters::parse_adapters(&rows)),
                _ => None,
            };
        }
        self.adapter_inventory.as_deref()
    }
}

/// Outcome of one raw `AdapterInfo` fetch, granular enough for the
/// `amd.adl.adapters` doctor check to name the stage that failed.
pub enum AdapterProbe {
    /// The driver's `atiadlxx.dll` does not export
    /// `ADL2_Adapter_AdapterInfo_Get`.
    NoEntryPoint,
    /// The count or info call returned an error, or the count was
    /// implausible.
    CallFailed,
    /// The call succeeded. `layout_ok` reports whether every row passed
    /// `AdapterInfo::looks_sane`; the rows are returned either way,
    /// because when verification fails the raw bytes are exactly the
    /// evidence the doctor exists to collect.
    Rows {
        rows: Vec<AdapterInfo>,
        layout_ok: bool,
    },
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

/// Take one PMLog sample from a specific adapter index, bypassing the
/// capability scan. Used by the multi-GPU path, where the index comes
/// from `AdapterInfo`-based matching rather than from the scan.
pub fn sample_adapter(index: i32) -> Option<AdlPmLogDataOutput> {
    let guard = match runtime().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard.as_ref()?.read_pmlog(index)
}

/// The validated adapter inventory, or `None` when it is unavailable
/// for any reason (no library, no entry point, failed call, or failed
/// layout verification). Cloned out so the runtime lock is not held
/// across the caller's matching work.
pub fn adapter_inventory() -> Option<Vec<AdlAdapter>> {
    let mut guard = match runtime().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard
        .as_mut()?
        .adapter_inventory()
        .map(<[AdlAdapter]>::to_vec)
}

/// One raw, uncached `AdapterInfo` fetch for the `amd.adl.adapters`
/// doctor check. `None` when the library itself is unavailable (see
/// [`library_available`] for separating that case).
pub fn adapter_info_probe() -> Option<AdapterProbe> {
    let guard = match runtime().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    Some(guard.as_ref()?.probe_adapter_info())
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
    if runtime.last_scan.is_none() {
        runtime.scan_for_capable_adapter();
    }
    runtime.chosen_index
}

/// The absolute path the loader uses, for diagnostics.
pub fn dll_path() -> &'static str {
    ADL_DLL_PATH
}
