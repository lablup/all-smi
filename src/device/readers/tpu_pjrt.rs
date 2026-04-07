// Copyright 2025 Lablup Inc.
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

//! Minimal PJRT (Platform Independent Runtime) FFI for Google TPU.
//!
//! This module attempts to load `libtpu.so` directly to access TPU metrics
//! without requiring external Go wrappers or Python.
//!
//! Note: This is a best-effort implementation. PJRT ABI is not strictly stable
//! across all versions, but basic entry points are generally consistent.

#![allow(unused)]

#[cfg(target_os = "linux")]
use libloading::{Library, Symbol};
#[cfg(target_os = "linux")]
use once_cell::sync::OnceCell;
#[cfg(target_os = "linux")]
use std::ffi::{c_char, c_void};
#[cfg(target_os = "linux")]
use std::sync::Mutex;
#[cfg(target_os = "linux")]
use tracing::debug;

#[cfg(target_os = "linux")]
const LIBTPU_PATHS: &[&str] = &[
    "libtpu.so",
    "/usr/lib/libtpu.so",
    "/usr/local/lib/libtpu.so",
    "/opt/google/tpu/libtpu.so",
];

/// Struct representing minimal TPU metrics fetched via PJRT
#[derive(Debug, Clone, Default)]
#[allow(dead_code)]
pub struct PjrtTpuMetrics {
    pub device_id: i32,
    pub chip_id: i32,
    pub global_device_id: i32,
    pub process_index: i32,
    pub memory_used_bytes: u64,
    pub memory_total_bytes: u64,
}

// --- PJRT C API Definitions (Minimal Subset) ---
// These layouts are based on OpenXLA PJRT C API (pjrt_c_api.h)
// Note: Struct layout stability is not guaranteed. We use a best-effort approach.

#[repr(C)]
#[allow(dead_code)]
struct PJRT_Error {
    _private: [u8; 0],
}

#[repr(C)]
#[allow(dead_code)]
struct PJRT_Client {
    _private: [u8; 0],
}

#[repr(C)]
#[allow(dead_code)]
struct PJRT_DeviceDescription {
    _private: [u8; 0],
}

#[repr(C)]
#[allow(dead_code)]
struct PJRT_Device {
    _private: [u8; 0],
}

#[repr(C)]
#[derive(Copy, Clone)]
#[allow(dead_code)]
struct PJRT_NamedValue {
    name: *const c_char,
    name_len: usize,
    type_: i32, // PJRT_NamedValue_Type
    value: PJRT_NamedValue_Union,
}

#[repr(C)]
#[derive(Copy, Clone)]
#[allow(dead_code)]
union PJRT_NamedValue_Union {
    bool_value: bool,
    int64_value: i64,
    float_value: f64,
    string_value: *const c_char,
    // other variants omitted
}

// --- PJRT Function Pointer Definitions ---

#[allow(non_camel_case_types)]
type PJRT_Error_Destroy = unsafe extern "C" fn(*mut PJRT_Error);
#[allow(non_camel_case_types)]
type PJRT_Error_Message = unsafe extern "C" fn(*mut PJRT_Error) -> *const c_char;
#[allow(non_camel_case_types)]
type PJRT_Error_GetCode = unsafe extern "C" fn(*mut PJRT_Error) -> i32;

#[allow(non_camel_case_types)]
type PJRT_Client_Create = unsafe extern "C" fn(
    *const PJRT_NamedValue, // args
    usize,                  // num_args
    *mut *mut PJRT_Client,  // client output
) -> *mut PJRT_Error;

#[allow(non_camel_case_types)]
type PJRT_Client_Destroy = unsafe extern "C" fn(*mut PJRT_Client) -> *mut PJRT_Error;
#[allow(non_camel_case_types)]
type PJRT_Client_Devices = unsafe extern "C" fn(
    *mut PJRT_Client,
    *mut *mut *mut PJRT_Device, // devices output (array of pointers)
    *mut usize,                 // num_devices output
) -> *mut PJRT_Error;

#[allow(non_camel_case_types)]
type PJRT_Device_GetMemoryStats = unsafe extern "C" fn(
    *mut PJRT_Device,
    *mut i64, // free_bytes
    *mut i64, // total_bytes
) -> *mut PJRT_Error;

#[allow(non_camel_case_types)]
type PJRT_Device_Id = unsafe extern "C" fn(*mut PJRT_Device) -> i32;
#[allow(non_camel_case_types)]
#[allow(dead_code)]
type PJRT_Device_GlobalId = unsafe extern "C" fn(*mut PJRT_Device) -> i32;

#[repr(C)]
struct PJRT_Api {
    struct_size: usize,
    priv_: *mut c_void,
    // Error handling
    error_destroy: PJRT_Error_Destroy,
    error_message: PJRT_Error_Message,
    error_get_code: PJRT_Error_GetCode,
    // Client
    client_create: PJRT_Client_Create,
    client_destroy: PJRT_Client_Destroy,
    client_platform_name: *mut c_void, // Skip
    client_process_index: *mut c_void, // Skip
    client_devices: PJRT_Client_Devices,
    client_addressable_devices: *mut c_void,    // Skip
    client_lookup_device: *mut c_void,          // Skip
    client_compile: *mut c_void,                // Skip
    client_compile_est: *mut c_void,            // Skip
    client_buffer_from_host: *mut c_void,       // Skip
    client_buffer_from_host_async: *mut c_void, // Skip
    client_buffer_from_scalar: *mut c_void,     // Skip
    // Device
    device_to_host_order: *mut c_void, // Skip
    device_id: PJRT_Device_Id,
    device_process_index: *mut c_void,        // Skip
    device_is_addressable: *mut c_void,       // Skip
    device_local_hardware_id: *mut c_void,    // Skip
    device_addressable_memories: *mut c_void, // Skip
    device_default_memory: *mut c_void,       // Skip
    device_memory_stats: PJRT_Device_GetMemoryStats,
    // There are more fields, but we hope these are stable at the top
}

#[cfg(target_os = "linux")]
struct LibTpu {
    _library: Library,
    api: *const PJRT_Api,
}

#[cfg(target_os = "linux")]
unsafe impl Send for LibTpu {}
#[cfg(target_os = "linux")]
unsafe impl Sync for LibTpu {}

#[cfg(target_os = "linux")]
static LIBTPU: OnceCell<Mutex<Option<LibTpu>>> = OnceCell::new();

#[cfg(target_os = "linux")]
struct PjrtClientHandle {
    client_ptr: *mut PJRT_Client,
}

#[cfg(target_os = "linux")]
unsafe impl Send for PjrtClientHandle {}
#[cfg(target_os = "linux")]
unsafe impl Sync for PjrtClientHandle {}

#[cfg(target_os = "linux")]
static PJRT_CLIENT: OnceCell<Mutex<Option<PjrtClientHandle>>> = OnceCell::new();

#[cfg(target_os = "linux")]
static STATUS_MESSAGE: Mutex<String> = Mutex::new(String::new());

#[cfg(target_os = "linux")]
pub fn get_status_message() -> Option<String> {
    let msg = STATUS_MESSAGE.lock().unwrap().clone();
    if msg.is_empty() || msg == "Ready" {
        None
    } else {
        Some(msg)
    }
}

#[cfg(target_os = "linux")]
pub fn is_libtpu_available() -> bool {
    get_libtpu()
        .map(|m| m.lock().map(|g| g.is_some()).unwrap_or(false))
        .unwrap_or(false)
}

#[cfg(target_os = "linux")]
fn get_libtpu() -> Option<&'static Mutex<Option<LibTpu>>> {
    Some(LIBTPU.get_or_init(|| {
        debug!("PJRT: Initializing LIBTPU singleton...");
        Mutex::new(load_libtpu())
    }))
}

#[cfg(target_os = "linux")]
fn load_libtpu() -> Option<LibTpu> {
    debug!("PJRT: Starting load_libtpu search...");

    // 1. Try to find in user python site-packages (Highest Priority)
    if let Some(home) = std::env::var_os("HOME") {
        let local_lib = std::path::Path::new(&home).join(".local/lib");
        debug!("PJRT: Scanning user local lib: {:?}", local_lib);
        if let Some(lib) = scan_python_dirs_for_libtpu(&local_lib) {
            return Some(lib);
        }
    }

    // 2. Try system python paths
    debug!("PJRT: Scanning /usr/local/lib...");
    if let Some(lib) = scan_python_dirs_for_libtpu(std::path::Path::new("/usr/local/lib")) {
        return Some(lib);
    }
    debug!("PJRT: Scanning /usr/lib...");
    if let Some(lib) = scan_python_dirs_for_libtpu(std::path::Path::new("/usr/lib")) {
        return Some(lib);
    }

    // 3. Try standard system paths
    debug!("PJRT: Checking standard paths...");
    for path in LIBTPU_PATHS {
        if let Some(lib) = unsafe { try_load_library(path) } {
            return Some(lib);
        }
    }

    debug!("PJRT: Library not found.");
    None
}

#[cfg(target_os = "linux")]
fn scan_python_dirs_for_libtpu(base_dir: &std::path::Path) -> Option<LibTpu> {
    if !base_dir.exists() {
        return None;
    }

    if let Ok(entries) = std::fs::read_dir(base_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if let Some(name) = path.file_name().and_then(|n| n.to_str())
                && name.starts_with("python")
            {
                let subdirs = ["site-packages", "dist-packages"];
                for subdir in subdirs {
                    let libtpu_path = path.join(subdir).join("libtpu").join("libtpu.so");
                    if libtpu_path.exists()
                        && let Some(str_path) = libtpu_path.to_str()
                        && let Some(lib) = unsafe { try_load_library(str_path) }
                    {
                        return Some(lib);
                    }
                }
            }
        }
    }
    None
}

#[cfg(target_os = "linux")]
unsafe fn try_load_library(path: &str) -> Option<LibTpu> {
    unsafe {
        debug!("PJRT: Trying to load library at: {}", path);
        let lib = match Library::new(path) {
            Ok(l) => {
                debug!("PJRT: Successfully loaded library: {}", path);
                l
            }
            Err(e) => {
                debug!("PJRT: Failed to load library: {} - Error: {}", path, e);
                return None;
            }
        };

        // Get the API table
        let get_api_sym: Symbol<unsafe extern "C" fn() -> *const PJRT_Api> = lib
            .get(b"GetPjrtApi\0")
            .ok()
            .or_else(|| lib.get(b"PJRT_GetApi\0").ok())?;

        let api = get_api_sym();
        if api.is_null() {
            return None;
        }

        // Basic sanity check: struct_size should be reasonable
        let struct_size = (*api).struct_size;
        if !(128..=10000).contains(&struct_size) {
            return None;
        }

        Some(LibTpu { _library: lib, api })
    }
}

// --- Metrics Retrieval ---

#[cfg(target_os = "linux")]
pub fn initialize_in_background() {
    {
        let mut status = STATUS_MESSAGE.lock().unwrap();
        *status = "Initializing TPU runtime...".to_string();
    }

    std::thread::spawn(|| {
        debug!("PJRT: Starting background initialization...");
        // This triggers the heavy loading and client creation
        let client_opt = get_pjrt_client();

        let mut status = STATUS_MESSAGE.lock().unwrap();
        if let Some(mutex) = client_opt {
            match mutex.lock() {
                Ok(guard) => {
                    if guard.is_some() {
                        *status = "Ready".to_string();
                        debug!("PJRT: Initialization successful.");
                    } else {
                        // Client creation failed or was skipped, but check if lib is loaded
                        if is_libtpu_available() {
                            *status = "Ready (Sysfs/Limited)".to_string();
                            debug!("PJRT: Client creation skipped, running in limited mode.");
                        } else {
                            *status = "TPU runtime initialization failed (Check logs)".to_string();
                            debug!("PJRT: Initialization failed.");
                        }
                    }
                }
                _ => {
                    *status = "TPU runtime error".to_string();
                }
            }
        } else {
            // This case shouldn't happen with get_or_init unless panic
            *status = "TPU runtime error".to_string();
        }
        debug!("PJRT: Background initialization complete.");
    });
}

#[cfg(target_os = "linux")]
fn get_pjrt_client() -> Option<&'static Mutex<Option<PjrtClientHandle>>> {
    Some(PJRT_CLIENT.get_or_init(|| {
        unsafe {
            // Helper to handle ? logic inside unsafe block
            let try_create = || -> Option<PjrtClientHandle> {
                debug!("PJRT: Loading libtpu...");
                let lib_mutex = get_libtpu()?;
                let guard = lib_mutex.lock().ok()?;
                let lib = guard.as_ref()?;

                // Debug API struct info
                let api = &*lib.api;
                debug!("PJRT: API struct size: {}", api.struct_size);
                debug!(
                    "PJRT: client_create ptr: {:p}",
                    api.client_create as *const ()
                );

                // SAFETY: We temporarily disable actual client creation because PJRT ABI
                // varies wildly between versions, causing Segfaults when calling function pointers
                // at wrong offsets.
                //
                // To fix this properly, we need to detect the PJRT version or use a stable C wrapper.
                debug!("PJRT: Skipping unsafe client creation to prevent Segfault.");

                // Return None to indicate we are not fully ready with a client,
                // but we successfully loaded the library.
                // This means we will rely on Sysfs for basic info.
                None

                /*
                // Code disabled due to Segfault risk:
                debug!("PJRT: Attempting to create client...");
                let mut client: *mut PJRT_Client = std::ptr::null_mut();

                let err = (api.client_create)(std::ptr::null(), 0, &mut client);

                if !err.is_null() {
                    debug!("PJRT: Client creation failed with error object.");
                    return None;
                }

                if client.is_null() {
                    debug!("PJRT: Client pointer is null.");
                    return None;
                }

                debug!("PJRT: Client created successfully.");
                Some(PjrtClientHandle { client_ptr: client })
                */
            };

            Mutex::new(try_create())
        }
    }))
}

#[cfg(target_os = "linux")]
pub fn get_tpu_metrics() -> Option<Vec<PjrtTpuMetrics>> {
    // Non-blocking check: ONLY proceed if client is already initialized
    // If PJRT_CLIENT is not initialized yet (background thread working), returns None immediately.
    let client_mutex = PJRT_CLIENT.get()?;

    // Check if library is loaded (should be if client is init)
    let lib_mutex = LIBTPU.get()?;
    let lib_guard = lib_mutex.lock().ok()?;
    let lib = lib_guard.as_ref()?;
    let api = unsafe { &*lib.api };

    // Lock the client mutex
    let client_guard = client_mutex.lock().ok()?;
    let client = client_guard.as_ref()?;

    let mut metrics = Vec::new();

    unsafe {
        let mut devices: *mut *mut PJRT_Device = std::ptr::null_mut();
        let mut num_devices: usize = 0;

        let err = (api.client_devices)(client.client_ptr, &mut devices, &mut num_devices);
        if !err.is_null() {
            (api.error_destroy)(err);
            return Some(Vec::new());
        }

        // Iterate over devices
        let device_slice = std::slice::from_raw_parts(devices, num_devices);
        for &device_ptr in device_slice.iter() {
            if device_ptr.is_null() {
                continue;
            }

            // Get device ID
            let dev_id = (api.device_id)(device_ptr);

            // Get memory stats
            let mut free_bytes: i64 = 0;
            let mut total_bytes: i64 = 0;
            let mem_err = (api.device_memory_stats)(device_ptr, &mut free_bytes, &mut total_bytes);

            let (used, total) = if mem_err.is_null() {
                (
                    (total_bytes - free_bytes).max(0) as u64,
                    total_bytes.max(0) as u64,
                )
            } else {
                (api.error_destroy)(mem_err);
                (0, 0)
            };

            metrics.push(PjrtTpuMetrics {
                device_id: dev_id,
                chip_id: dev_id, // Approx
                global_device_id: dev_id,
                process_index: 0,
                memory_used_bytes: used,
                memory_total_bytes: total,
            });
        }
    }

    Some(metrics)
}

#[cfg(not(target_os = "linux"))]
pub fn get_tpu_metrics() -> Option<Vec<PjrtTpuMetrics>> {
    None
}

#[cfg(not(target_os = "linux"))]
pub fn initialize_in_background() {}
