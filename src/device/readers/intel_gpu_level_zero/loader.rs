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

//! Dynamic loading of the Level Zero loader library and one-shot
//! runtime initialisation. Split out of `intel_gpu_level_zero.rs` so
//! the public API surface stays small and the loader internals can be
//! exercised by unit tests without pulling in the refresh code path.

use super::api::{LoadedLibrary, LzApi};
use super::ffi;
use libloading::Library;
use once_cell::sync::OnceCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::{Mutex, Once};
use tracing::{debug, warn};

pub use super::api::try_load_library;

/// Upper bound on any driver-reported handle / device / domain count
/// we will allocate a buffer for. Mirrors the
/// [`MAX_DEVICES`](crate::device::readers::common_cache::MAX_DEVICES)
/// cap used by the generic device-cache layer. A buggy or hostile
/// driver returning `u32::MAX` here would otherwise trigger a ~32 GiB
/// allocation when the count is fed into `vec![ptr; count]`; capping
/// turns that into a bounded warning and a partial enumeration.
///
/// Real Intel hardware reports ~6 engines plus a handful of power
/// domains per card, so hitting this cap in production is essentially
/// impossible — it is a DoS guard, not a tuning knob.
pub(crate) const MAX_L0_HANDLES: usize = 256;

/// Clamp a driver-reported `u32` count to [`MAX_L0_HANDLES`] and emit a
/// warning (only the first time the cap is hit per process) so an
/// operator notices a misbehaving driver. Returns the capped count as
/// `(usize, u32)` — the `usize` sizes the Vec, the `u32` is what we
/// pass back into the second "fill" call of the count-then-buffer
/// idiom.
pub(crate) fn cap_handle_count(reported: u32, what: &'static str) -> (usize, u32) {
    let safe = (reported as usize).min(MAX_L0_HANDLES);
    if (reported as usize) > MAX_L0_HANDLES {
        L0_CAP_WARN.call_once(|| {
            warn!(
                "Level Zero: driver reported {reported} {what}, capping at {MAX_L0_HANDLES}; \
                 further over-cap counts will be silently truncated"
            );
        });
    }
    (safe, safe as u32)
}

/// One-shot latch around the cap-hit warning so we don't spam the log
/// every refresh tick if a host genuinely exceeds the cap.
static L0_CAP_WARN: Once = Once::new();

// Library search paths. We mirror tpu_pjrt.rs by trying the SONAME
// first (so the dynamic linker can do its usual search), then a small
// set of well-known absolute paths. dlopen handles `LD_LIBRARY_PATH`
// itself when the SONAME-only forms are passed.
#[cfg(target_os = "linux")]
pub(crate) const LIBZE_PATHS: &[&str] = &[
    "libze_loader.so.1",
    "libze_loader.so",
    "/usr/lib/x86_64-linux-gnu/libze_loader.so.1",
    "/usr/lib/x86_64-linux-gnu/libze_loader.so",
    "/usr/lib64/libze_loader.so.1",
    "/usr/lib64/libze_loader.so",
    "/usr/local/lib/libze_loader.so.1",
];

#[cfg(target_os = "windows")]
pub(crate) const LIBZE_PATHS: &[&str] = &[
    "ze_loader.dll",
    // The Intel driver installs the loader into System32 — DLL search
    // order finds it there if it's not next to the executable.
    "C:\\Windows\\System32\\ze_loader.dll",
];

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
pub(crate) const LIBZE_PATHS: &[&str] = &[];

/// Legacy Sysman initialisation environment key. Newer loaders expose
/// `zesInit`; older ones require `ZES_ENABLE_SYSMAN=1` **before** the
/// first `zeInit` call. See
/// <https://oneapi-src.github.io/level-zero-spec/level-zero/latest/sysman/PROG.html#using-sysman>.
///
/// The CLI binary sets this at process start for legacy runtime
/// compatibility. Library callers can either call `zesInit` through a
/// modern loader (handled automatically here) or set this environment
/// variable before starting threads / invoking all-smi.
pub(crate) const SYSMAN_ENV_KEY: &str = "ZES_ENABLE_SYSMAN";

/// One-shot env-var injector used only by callers that can prove they
/// are still in process startup.
static SYSMAN_ENV_INIT: Once = Once::new();

/// Enable Sysman for legacy Level Zero loaders that do not export
/// `zesInit`.
///
/// Modern loaders are initialised through `zesInit` in
/// [`initialize_runtime`], so this function exists only to preserve
/// compatibility with older Intel runtimes that still require the
/// environment-variable path.
///
/// # Safety
///
/// Must be called during single-threaded process startup, before any
/// other thread can concurrently read or mutate the process
/// environment. This is why the CLI calls it from `main()` before
/// constructing a Tokio runtime or spawning signal-handler tasks.
pub unsafe fn prepare_sysman_env_for_legacy_runtime() {
    SYSMAN_ENV_INIT.call_once(|| {
        // SAFETY: upheld by this function's contract.
        unsafe {
            if std::env::var_os(SYSMAN_ENV_KEY).is_none() {
                std::env::set_var(SYSMAN_ENV_KEY, "1");
            }
        }
    });
}

/// Process-wide initialisation latch. First caller pays the dlopen +
/// `zeInit` + driver/device enumeration cost; later callers reuse the
/// cached [`LzRuntime`]. Returns `None` when the runtime cannot be
/// loaded — the typical case on a host without the Intel L0 loader.
static LZ_RUNTIME: OnceCell<Mutex<Option<LzRuntime>>> = OnceCell::new();

/// Result of the first successful library load + `zeInit`.
pub(crate) struct LzRuntime {
    /// Keep the `libloading::Library` alive for the lifetime of the
    /// process — leak intentional. Function pointers extracted from it
    /// remain valid only while the library is loaded.
    _library: Library,
    /// Function-pointer table, populated once and reused per call.
    pub(crate) api: LzApi,
    /// Map from canonical PCI BDF string (`"DDDD:BB:DD.F"`) to the L0
    /// device handle for that card. Built at init time. Lookups during
    /// refresh are O(1).
    pub(crate) devices_by_pci: HashMap<String, zes_device_handle_t_send>,
}

unsafe impl Send for LzRuntime {}
unsafe impl Sync for LzRuntime {}

/// Wrapper around an `ffi::zes_device_handle_t` opaque pointer that
/// satisfies `Send + Sync`. The L0 spec documents that opaque handles
/// can be passed to Sysman entry points from any thread; we serialise
/// per-engine / per-power activity reads at a higher layer via the
/// per-card `Mutex` around `LevelZeroState`.
#[derive(Clone, Copy)]
pub(crate) struct zes_device_handle_t_send(pub(crate) ffi::zes_device_handle_t);
unsafe impl Send for zes_device_handle_t_send {}
unsafe impl Sync for zes_device_handle_t_send {}

impl std::fmt::Debug for zes_device_handle_t_send {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("zes_device_handle_t_send")
            .field(&(self.0 as usize))
            .finish()
    }
}

/// Lazy-initialised Level Zero runtime. The first caller pays the cost
/// of dlopen + `zeInit` + driver/device enumeration; later callers
/// reuse the cached [`LzRuntime`]. Returns `None` on init failure.
pub(crate) fn ensure_runtime() -> Option<&'static Mutex<Option<LzRuntime>>> {
    Some(LZ_RUNTIME.get_or_init(|| Mutex::new(initialize_runtime())))
}

/// Convenience wrapper used by callers that just want to run a closure
/// against the runtime, treating any layer of initialisation failure
/// as "L0 unavailable" → returns `None`.
pub(crate) fn with_runtime<R>(f: impl FnOnce(&LzRuntime) -> R) -> Option<R> {
    let lock = ensure_runtime()?;
    let guard = lock.lock().ok()?;
    let runtime = guard.as_ref()?;
    Some(f(runtime))
}

fn initialize_runtime() -> Option<LzRuntime> {
    // Try every candidate path until one loads and resolves all
    // symbols. A failure here is the normal case on a host without the
    // L0 runtime — we log at debug, never warn or error.
    let mut loaded: Option<LoadedLibrary> = None;
    for path in LIBZE_PATHS {
        // SAFETY: see `try_load_library`'s safety contract — we only
        // load canonical Level Zero loader paths.
        if let Some(lib) = unsafe { try_load_library(path) } {
            debug!("Level Zero: loaded {path}");
            loaded = Some(lib);
            break;
        }
    }
    let loaded = loaded?;

    let api = loaded.api;
    let sysman_env_enabled = std::env::var(SYSMAN_ENV_KEY)
        .map(|v| v == "1")
        .unwrap_or(false);
    if api.zes_init.is_none() && !sysman_env_enabled {
        debug!(
            "Level Zero: loader does not expose zesInit and {SYSMAN_ENV_KEY}=1 was not set before zeInit; degrading"
        );
        return None;
    }

    // SAFETY: api function pointers were resolved from the library
    // above and `lib` is still alive (we own it). Their C signatures
    // match the typedefs in `ffi`.
    let init_res = unsafe { (api.ze_init)(ffi::ZE_INIT_FLAG_DEFAULT) };
    if init_res != ffi::ZE_RESULT_SUCCESS {
        debug!("Level Zero: zeInit returned {init_res}; degrading");
        return None;
    }

    if let Some(zes_init) = api.zes_init {
        // SAFETY: optional symbol was resolved from the same live
        // Level Zero loader as the other function pointers. The spec
        // allows calling `zesInit` before or after `zeInit`, but it
        // must happen before any other Sysman function.
        let sysman_res = unsafe { (zes_init)(ffi::ZE_INIT_FLAG_DEFAULT) };
        if sysman_res != ffi::ZE_RESULT_SUCCESS {
            debug!("Level Zero: zesInit returned {sysman_res}; degrading");
            return None;
        }
    }

    let devices_by_pci = enumerate_devices(&api);
    if devices_by_pci.is_empty() {
        debug!("Level Zero: zeInit succeeded but no devices visible to L0");
    }

    Some(LzRuntime {
        _library: loaded.library,
        api,
        devices_by_pci,
    })
}

/// Walk every L0 driver and every device under each driver. Returns a
/// map from canonical PCI BDF (`"DDDD:BB:DD.F"`) to the device handle.
/// Errors at any level are downgraded: a driver that fails to
/// enumerate devices contributes zero entries instead of failing the
/// whole walk.
fn enumerate_devices(api: &LzApi) -> HashMap<String, zes_device_handle_t_send> {
    let mut out = HashMap::new();

    let mut driver_count: u32 = 0;
    // SAFETY: pointer is non-null and writable; null buffer is the
    // documented "count-only" mode.
    let r = unsafe { (api.ze_driver_get)(&mut driver_count, std::ptr::null_mut()) };
    if r != ffi::ZE_RESULT_SUCCESS || driver_count == 0 {
        debug!("Level Zero: zeDriverGet returned {r}, count {driver_count}");
        return out;
    }
    // Cap the driver-reported count to MAX_L0_HANDLES before sizing
    // the Vec — see `cap_handle_count` for the DoS rationale.
    let (drivers_cap, mut driver_count) = cap_handle_count(driver_count, "drivers");
    let mut drivers: Vec<ffi::ze_driver_handle_t> =
        vec![std::ptr::null_mut::<c_void>(); drivers_cap];
    // SAFETY: drivers vec is sized exactly to driver_count (capped).
    let r = unsafe { (api.ze_driver_get)(&mut driver_count, drivers.as_mut_ptr()) };
    if r != ffi::ZE_RESULT_SUCCESS {
        debug!("Level Zero: zeDriverGet (fill) returned {r}");
        return out;
    }
    // The driver writes back the actual number of entries populated;
    // truncate so we never iterate past the populated prefix.
    drivers.truncate((driver_count as usize).min(drivers_cap));

    for driver in drivers.iter().copied() {
        if driver.is_null() {
            continue;
        }
        let mut dev_count: u32 = 0;
        // SAFETY: per spec — null buffer = count-only.
        let r = unsafe { (api.ze_device_get)(driver, &mut dev_count, std::ptr::null_mut()) };
        if r != ffi::ZE_RESULT_SUCCESS || dev_count == 0 {
            continue;
        }
        // Cap the driver-reported count before allocating; see
        // `cap_handle_count` for the DoS rationale.
        let (devices_cap, mut dev_count) = cap_handle_count(dev_count, "devices");
        let mut devices: Vec<ffi::ze_device_handle_t> =
            vec![std::ptr::null_mut::<c_void>(); devices_cap];
        // SAFETY: devices vec is sized exactly to dev_count (capped).
        let r = unsafe { (api.ze_device_get)(driver, &mut dev_count, devices.as_mut_ptr()) };
        if r != ffi::ZE_RESULT_SUCCESS {
            continue;
        }
        devices.truncate((dev_count as usize).min(devices_cap));
        for device in devices.iter().copied() {
            if device.is_null() {
                continue;
            }
            let mut props = ffi::zes_pci_properties_t::default();
            // SAFETY: props is fully initialised with the spec-correct
            // stype/pnext; the driver populates the remaining fields.
            let r = unsafe { (api.zes_device_pci_get_properties)(device, &mut props) };
            if r != ffi::ZE_RESULT_SUCCESS {
                continue;
            }
            let bdf = format_pci_bdf(&props.address);
            out.insert(bdf, zes_device_handle_t_send(device));
        }
    }

    out
}

/// Format a PCI address as `"DDDD:BB:DD.F"` (lowercase hex) — matches
/// the layout Linux sysfs exposes via `/sys/bus/pci/devices/*` so the
/// per-card readers can perform a string equality lookup.
pub(crate) fn format_pci_bdf(addr: &ffi::zes_pci_address_t) -> String {
    format!(
        "{:04x}:{:02x}:{:02x}.{:x}",
        addr.domain, addr.bus, addr.device, addr.function
    )
}

/// Normalise the PCI bus string we get from sysfs / WMI to the format
/// produced by [`format_pci_bdf`] so map lookups succeed regardless of
/// case differences across kernels.
pub fn normalise_pci_bdf(raw: &str) -> String {
    raw.to_ascii_lowercase()
}

/// Test-only helper. Injects a synthetic device map so [`with_runtime`]
/// can be exercised without a real Level Zero loader. Calling this in
/// production is unsupported.
///
/// NOTE: Because `LZ_RUNTIME` is a process-wide `OnceCell`, the test
/// runner serialises through this entry point. The helper is a no-op
/// if the runtime has already been initialised by some other test or
/// by production code.
#[cfg(test)]
pub(crate) fn install_test_runtime(_map: HashMap<String, zes_device_handle_t_send>) {
    // No-op placeholder: full mock substitution requires a feature
    // flag the issue scope explicitly skipped (synthetic L0 runtime is
    // a follow-up). The presence of this hook reserves the public
    // shape for future tests.
}
