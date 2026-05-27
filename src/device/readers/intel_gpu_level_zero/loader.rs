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

use super::ffi;
use libloading::{Library, Symbol};
use once_cell::sync::OnceCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::{Mutex, Once};
use tracing::debug;

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

/// Sysman is initialised by setting `ZES_ENABLE_SYSMAN=1` **before**
/// the first `zeInit` call. See
/// <https://oneapi-src.github.io/level-zero-spec/level-zero/latest/sysman/PROG.html#using-sysman>.
///
/// Issue #248 design notes picked the env-var route (Option A) over
/// `zesInit` for broader compatibility — older Intel driver stacks ship
/// a loader that exports `zeInit` but not `zesInit`.
pub(crate) const SYSMAN_ENV_KEY: &str = "ZES_ENABLE_SYSMAN";

/// One-shot env-var injector. Setting an env var inside a process is
/// `unsafe` under the 2024 edition because other threads could read
/// the environment concurrently; we therefore gate the call behind
/// [`Once`] and ensure it runs strictly before the first `zeInit`.
static SYSMAN_ENV_INIT: Once = Once::new();

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

/// Function pointer table — extracted from the loaded library once.
#[derive(Clone, Copy)]
pub(crate) struct LzApi {
    pub(crate) ze_init: ffi::ZeInit,
    pub(crate) ze_driver_get: ffi::ZeDriverGet,
    pub(crate) ze_device_get: ffi::ZeDeviceGet,
    pub(crate) zes_device_pci_get_properties: ffi::ZesDevicePciGetProperties,
    pub(crate) zes_device_enum_engine_groups: ffi::ZesDeviceEnumEngineGroups,
    pub(crate) zes_engine_get_properties: ffi::ZesEngineGetProperties,
    pub(crate) zes_engine_get_activity: ffi::ZesEngineGetActivity,
    pub(crate) zes_device_enum_power_domains: ffi::ZesDeviceEnumPowerDomains,
    pub(crate) zes_power_get_energy_counter: ffi::ZesPowerGetEnergyCounter,
}

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

/// Wrapper returned by [`try_load_library`] — owned by the static
/// runtime cell at runtime; tests drop it explicitly.
pub struct LoadedLibrary {
    pub(crate) library: Library,
    pub(crate) api: LzApi,
}

/// Attempt to load the Level Zero loader at the given path and resolve
/// every symbol we need. Returns `None` on any failure — the caller
/// degrades to the sysfs/WMI baseline. Public so the tests can probe
/// it with a deliberately-bogus path.
///
/// # Safety
///
/// Loads an arbitrary shared library and resolves C symbols. The
/// caller must ensure `path` points to a real Level Zero loader; if any
/// resolved symbol has the wrong signature, calling it later is UB.
/// In production [`LIBZE_PATHS`] only contains canonical loader
/// filenames; every resolution failure short-circuits to `None`.
pub unsafe fn try_load_library(path: &str) -> Option<LoadedLibrary> {
    unsafe {
        debug!("Level Zero: trying to load loader at {path}");
        let lib = match Library::new(path) {
            Ok(l) => l,
            Err(e) => {
                debug!("Level Zero: failed to load {path}: {e}");
                return None;
            }
        };

        // Resolve every symbol we care about. A single missing symbol
        // means the runtime does not match our expected API surface;
        // we conservatively refuse to bind.
        let ze_init: Symbol<ffi::ZeInit> = lib.get(b"zeInit\0").ok()?;
        let ze_driver_get: Symbol<ffi::ZeDriverGet> = lib.get(b"zeDriverGet\0").ok()?;
        let ze_device_get: Symbol<ffi::ZeDeviceGet> = lib.get(b"zeDeviceGet\0").ok()?;
        let zes_device_pci_get_properties: Symbol<ffi::ZesDevicePciGetProperties> =
            lib.get(b"zesDevicePciGetProperties\0").ok()?;
        let zes_device_enum_engine_groups: Symbol<ffi::ZesDeviceEnumEngineGroups> =
            lib.get(b"zesDeviceEnumEngineGroups\0").ok()?;
        let zes_engine_get_properties: Symbol<ffi::ZesEngineGetProperties> =
            lib.get(b"zesEngineGetProperties\0").ok()?;
        let zes_engine_get_activity: Symbol<ffi::ZesEngineGetActivity> =
            lib.get(b"zesEngineGetActivity\0").ok()?;
        let zes_device_enum_power_domains: Symbol<ffi::ZesDeviceEnumPowerDomains> =
            lib.get(b"zesDeviceEnumPowerDomains\0").ok()?;
        let zes_power_get_energy_counter: Symbol<ffi::ZesPowerGetEnergyCounter> =
            lib.get(b"zesPowerGetEnergyCounter\0").ok()?;

        let api = LzApi {
            ze_init: *ze_init,
            ze_driver_get: *ze_driver_get,
            ze_device_get: *ze_device_get,
            zes_device_pci_get_properties: *zes_device_pci_get_properties,
            zes_device_enum_engine_groups: *zes_device_enum_engine_groups,
            zes_engine_get_properties: *zes_engine_get_properties,
            zes_engine_get_activity: *zes_engine_get_activity,
            zes_device_enum_power_domains: *zes_device_enum_power_domains,
            zes_power_get_energy_counter: *zes_power_get_energy_counter,
        };

        Some(LoadedLibrary { library: lib, api })
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
    // Set ZES_ENABLE_SYSMAN before any L0 call, exactly once.
    SYSMAN_ENV_INIT.call_once(|| {
        // SAFETY: Inside `Once::call_once`, so no L0 thread has begun
        // yet (every L0 caller goes through `ensure_runtime`, which is
        // what we are inside of). We only set the variable when it is
        // currently unset so we never clobber an operator's
        // intentional override.
        unsafe {
            if std::env::var_os(SYSMAN_ENV_KEY).is_none() {
                std::env::set_var(SYSMAN_ENV_KEY, "1");
            }
        }
    });

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

    // SAFETY: api function pointers were resolved from the library
    // above and `lib` is still alive (we own it). Their C signatures
    // match the typedefs in `ffi`.
    let init_res = unsafe { (api.ze_init)(ffi::ZE_INIT_FLAG_DEFAULT) };
    if init_res != ffi::ZE_RESULT_SUCCESS {
        debug!("Level Zero: zeInit returned {init_res}; degrading");
        return None;
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
    let mut drivers: Vec<ffi::ze_driver_handle_t> =
        vec![std::ptr::null_mut::<c_void>(); driver_count as usize];
    // SAFETY: drivers vec is sized exactly to driver_count.
    let r = unsafe { (api.ze_driver_get)(&mut driver_count, drivers.as_mut_ptr()) };
    if r != ffi::ZE_RESULT_SUCCESS {
        debug!("Level Zero: zeDriverGet (fill) returned {r}");
        return out;
    }

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
        let mut devices: Vec<ffi::ze_device_handle_t> =
            vec![std::ptr::null_mut::<c_void>(); dev_count as usize];
        // SAFETY: devices vec is sized exactly to dev_count.
        let r = unsafe { (api.ze_device_get)(driver, &mut dev_count, devices.as_mut_ptr()) };
        if r != ffi::ZE_RESULT_SUCCESS {
            continue;
        }
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
