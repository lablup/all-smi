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

//! Runtime loader for the Linux AMD companion backend.
//!
//! The main crate deliberately has no `libamdgpu_top` dependency. It loads a
//! packaged `liball_smi_amd.so` through a versioned function table and treats
//! every lookup, native-dependency, ABI, and sampling failure as backend
//! unavailability rather than a process-startup failure.

use std::collections::HashSet;
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use libloading::Library;
use serde::Deserialize;
use serde::de::DeserializeOwned;

use crate::device::readers::amd_plugin_api::{
    AMD_PLUGIN_ABI_VERSION, AMD_PLUGIN_ENTRY_SYMBOL, AMD_PLUGIN_WIRE_FORMAT, AmdPluginApiV1,
    AmdPluginBuffer, ReadJsonFn,
};
use crate::device::{GpuInfo, GpuReader, ProcessInfo};

pub const AMD_PLUGIN_ENV: &str = "ALL_SMI_AMD_PLUGIN";
pub const AMD_PLUGIN_FILENAME: &str = "liball_smi_amd.so";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AmdBackendStatus {
    Loaded {
        path: PathBuf,
        abi_version: u32,
        plugin_version: String,
        libamdgpu_top_version: String,
    },
    Unavailable {
        reason: String,
    },
}

#[derive(Deserialize)]
struct PluginMetadata {
    plugin_version: String,
    libamdgpu_top_version: String,
    wire_format: String,
}

struct LoadedPlugin {
    _library: Library,
    api: AmdPluginApiV1,
    path: PathBuf,
    metadata: PluginMetadata,
}

static PLUGIN: OnceLock<Result<Arc<LoadedPlugin>, String>> = OnceLock::new();

fn plugin() -> Result<Arc<LoadedPlugin>, String> {
    PLUGIN.get_or_init(load_plugin).clone()
}

pub fn backend_status() -> AmdBackendStatus {
    match plugin() {
        Ok(plugin) => AmdBackendStatus::Loaded {
            path: plugin.path.clone(),
            abi_version: plugin.api.abi_version,
            plugin_version: plugin.metadata.plugin_version.clone(),
            libamdgpu_top_version: plugin.metadata.libamdgpu_top_version.clone(),
        },
        Err(reason) => AmdBackendStatus::Unavailable { reason },
    }
}

fn load_plugin() -> Result<Arc<LoadedPlugin>, String> {
    let candidates = configured_candidates()?;
    let mut failures = Vec::new();

    for candidate in candidates {
        if !candidate.exists() {
            failures.push(format!("{}: not found", candidate.display()));
            continue;
        }
        let path = match validate_candidate(&candidate) {
            Ok(path) => path,
            Err(error) => {
                failures.push(error);
                continue;
            }
        };
        match load_candidate(path.clone()) {
            Ok(plugin) => return Ok(Arc::new(plugin)),
            Err(error) => failures.push(format!("{}: {error}", path.display())),
        }
    }

    Err(format!(
        "AMD plugin unavailable; {}. Install {AMD_PLUGIN_FILENAME} beside the executable or in /usr/lib/all-smi, or set {AMD_PLUGIN_ENV} to a safe absolute path",
        failures.join("; ")
    ))
}

fn configured_candidates() -> Result<Vec<PathBuf>, String> {
    if let Some(override_path) = std::env::var_os(AMD_PLUGIN_ENV) {
        if override_path.is_empty() {
            return Err(format!("{AMD_PLUGIN_ENV} is set but empty"));
        }
        let path = PathBuf::from(override_path);
        if !path.is_absolute() {
            return Err(format!(
                "{AMD_PLUGIN_ENV} must be an absolute path, got {}",
                path.display()
            ));
        }
        return Ok(vec![path]);
    }

    let executable = std::env::current_exe().map_err(|error| {
        format!("cannot resolve the current executable for AMD plugin lookup: {error}")
    })?;
    Ok(default_candidates(&executable))
}

fn default_candidates(executable: &Path) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(bin_dir) = executable.parent() {
        candidates.push(bin_dir.join(AMD_PLUGIN_FILENAME));
        candidates.push(
            bin_dir
                .join("..")
                .join("lib")
                .join("all-smi")
                .join(AMD_PLUGIN_FILENAME),
        );
    }
    candidates.push(PathBuf::from("/usr/local/lib/all-smi").join(AMD_PLUGIN_FILENAME));
    candidates.push(PathBuf::from("/usr/lib/all-smi").join(AMD_PLUGIN_FILENAME));

    let mut seen = HashSet::new();
    candidates
        .into_iter()
        .filter(|candidate| seen.insert(candidate.clone()))
        .collect()
}

fn validate_candidate(candidate: &Path) -> Result<PathBuf, String> {
    if !candidate.is_absolute() {
        return Err(format!(
            "{}: plugin path is not absolute",
            candidate.display()
        ));
    }
    let canonical = candidate.canonicalize().map_err(|error| {
        format!(
            "{}: cannot canonicalize plugin path: {error}",
            candidate.display()
        )
    })?;
    if !canonical.is_file() {
        return Err(format!(
            "{}: plugin path is not a regular file",
            canonical.display()
        ));
    }
    reject_world_writable_path(&canonical)?;
    Ok(canonical)
}

#[cfg(unix)]
fn reject_world_writable_path(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;

    for component in path.ancestors() {
        let metadata = component.metadata().map_err(|error| {
            format!(
                "{}: cannot inspect permissions: {error}",
                component.display()
            )
        })?;
        if metadata.permissions().mode() & 0o002 != 0 {
            return Err(format!(
                "{}: refusing AMD plugin because {} is world-writable",
                path.display(),
                component.display()
            ));
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn reject_world_writable_path(_path: &Path) -> Result<(), String> {
    Ok(())
}

type EntryV1 = unsafe extern "C" fn() -> *const AmdPluginApiV1;

fn load_candidate(path: PathBuf) -> Result<LoadedPlugin, String> {
    // SAFETY: the path is absolute, canonicalized, and checked for a
    // world-writable file or ancestor. The library stays owned by
    // `LoadedPlugin` for longer than every copied function pointer.
    let library =
        unsafe { Library::new(&path) }.map_err(|error| format!("dlopen failed: {error}"))?;
    let api = {
        // SAFETY: the symbol name is NUL-terminated and the versioned ABI
        // contract defines this exact function signature.
        let entry = unsafe { library.get::<EntryV1>(AMD_PLUGIN_ENTRY_SYMBOL) }
            .map_err(|error| format!("missing v1 entry point: {error}"))?;
        // SAFETY: calling the entry point has no arguments and only returns a
        // pointer to immutable static storage owned by the loaded library.
        let api = unsafe { entry() };
        if api.is_null() {
            return Err("v1 entry point returned a null function table".to_string());
        }
        // SAFETY: every versioned entry point begins with these two header
        // fields. Read and validate them before copying the full v1 table, so
        // a legitimately older/truncated table is rejected without an
        // out-of-bounds read.
        let abi_version = unsafe { std::ptr::addr_of!((*api).abi_version).read() };
        let struct_size = unsafe { std::ptr::addr_of!((*api).struct_size).read() };
        validate_api_header(abi_version, struct_size)?;
        // SAFETY: the validated `struct_size` covers the complete v1 table.
        unsafe { api.read() }
    };
    validate_api(&api)?;
    let metadata: PluginMetadata = read_buffer(&api, |out| {
        // SAFETY: `validate_api` accepted the table and `out` points to a
        // live descriptor for this call.
        unsafe { (api.read_metadata_json.expect("validated function pointer"))(out) }
    })?;
    validate_metadata(&metadata)?;
    Ok(LoadedPlugin {
        _library: library,
        api,
        path,
        metadata,
    })
}

fn validate_metadata(metadata: &PluginMetadata) -> Result<(), String> {
    let host_version = env!("CARGO_PKG_VERSION");
    if metadata.plugin_version != host_version {
        return Err(format!(
            "plugin version mismatch: host requires {host_version}, plugin reports {}",
            metadata.plugin_version
        ));
    }
    if metadata.wire_format != AMD_PLUGIN_WIRE_FORMAT {
        return Err(format!(
            "wire format mismatch: host requires {AMD_PLUGIN_WIRE_FORMAT}, plugin reports {}",
            metadata.wire_format
        ));
    }
    Ok(())
}

fn validate_api(api: &AmdPluginApiV1) -> Result<(), String> {
    validate_api_header(api.abi_version, api.struct_size)?;
    if api.create_reader.is_none()
        || api.destroy_reader.is_none()
        || api.read_gpu_info_json.is_none()
        || api.read_process_info_json.is_none()
        || api.read_metadata_json.is_none()
        || api.free_buffer.is_none()
    {
        return Err("ABI v1 function table contains a null function pointer".to_string());
    }
    Ok(())
}

fn validate_api_header(abi_version: u32, struct_size: usize) -> Result<(), String> {
    if abi_version != AMD_PLUGIN_ABI_VERSION {
        return Err(format!(
            "ABI mismatch: host requires v{AMD_PLUGIN_ABI_VERSION}, plugin reports v{abi_version}"
        ));
    }
    let required = std::mem::size_of::<AmdPluginApiV1>();
    if struct_size < required {
        return Err(format!(
            "ABI v{AMD_PLUGIN_ABI_VERSION} function table is truncated: host requires {required} bytes, plugin reports {struct_size}"
        ));
    }
    Ok(())
}

fn read_buffer<T: DeserializeOwned>(
    api: &AmdPluginApiV1,
    invoke: impl FnOnce(*mut AmdPluginBuffer) -> i32,
) -> Result<T, String> {
    let mut buffer = AmdPluginBuffer::default();
    let status = invoke(&mut buffer);
    if status != 0 {
        return Err(format!("plugin call failed with status {status}"));
    }
    if buffer.len > buffer.capacity || (buffer.ptr.is_null() && buffer.len != 0) {
        return Err("plugin returned an invalid buffer descriptor".to_string());
    }
    let decoded = if buffer.len == 0 {
        Err("plugin returned an empty JSON buffer".to_string())
    } else {
        // SAFETY: the validated descriptor is owned by the plugin and remains
        // live until `free_buffer` below.
        let bytes = unsafe { std::slice::from_raw_parts(buffer.ptr, buffer.len) };
        serde_json::from_slice(bytes).map_err(|error| format!("invalid plugin JSON: {error}"))
    };
    // SAFETY: this descriptor came from the same validated API table and is
    // released exactly once, even when deserialization fails.
    unsafe { (api.free_buffer.expect("validated function pointer"))(&mut buffer) };
    decoded
}

pub struct AmdGpuReader {
    plugin: Option<Arc<LoadedPlugin>>,
    handle: Option<usize>,
    calls: Mutex<()>,
    reported_error: Mutex<Option<String>>,
}

impl AmdGpuReader {
    pub fn try_new() -> Result<Self, String> {
        let plugin = plugin()?;
        // SAFETY: the API table was validated and the library remains loaded
        // through the Arc stored in the returned reader.
        let handle = unsafe {
            (plugin
                .api
                .create_reader
                .expect("validated function pointer"))()
        };
        if handle.is_null() {
            return Err("AMD plugin could not create a reader".to_string());
        }
        Ok(Self {
            plugin: Some(plugin),
            handle: Some(handle as usize),
            calls: Mutex::new(()),
            reported_error: Mutex::new(None),
        })
    }

    pub fn new() -> Self {
        match Self::try_new() {
            Ok(reader) => reader,
            Err(error) => {
                eprintln!("AMD backend unavailable: {error}");
                Self {
                    plugin: None,
                    handle: None,
                    calls: Mutex::new(()),
                    reported_error: Mutex::new(Some(error)),
                }
            }
        }
    }

    fn collect<T: DeserializeOwned>(&self, read: ReadJsonFn) -> Vec<T> {
        let (Some(plugin), Some(handle)) = (&self.plugin, self.handle) else {
            return Vec::new();
        };
        let _call = self
            .calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match read_buffer(&plugin.api, |out| {
            // SAFETY: the handle belongs to this plugin instance, calls are
            // serialized, and `out` is live for the duration of the call.
            unsafe { read(handle as *mut c_void, out) }
        }) {
            Ok(values) => values,
            Err(error) => {
                self.report_error(error);
                Vec::new()
            }
        }
    }

    fn report_error(&self, error: String) {
        let mut previous = self
            .reported_error
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if previous.as_deref() != Some(&error) {
            eprintln!("AMD plugin sampling failed: {error}");
            *previous = Some(error);
        }
    }
}

impl Default for AmdGpuReader {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for AmdGpuReader {
    fn drop(&mut self) {
        if let (Some(plugin), Some(handle)) = (&self.plugin, self.handle.take()) {
            // SAFETY: this is the one destroy call paired with create_reader;
            // `&mut self` guarantees no sampling call is active.
            unsafe {
                (plugin
                    .api
                    .destroy_reader
                    .expect("validated function pointer"))(handle as *mut c_void)
            };
        }
    }
}

impl GpuReader for AmdGpuReader {
    fn get_gpu_info(&self) -> Vec<GpuInfo> {
        let Some(plugin) = &self.plugin else {
            return Vec::new();
        };
        self.collect(
            plugin
                .api
                .read_gpu_info_json
                .expect("validated function pointer"),
        )
    }

    fn get_process_info(&self) -> Vec<ProcessInfo> {
        let Some(plugin) = &self.plugin else {
            return Vec::new();
        };
        self.collect(
            plugin
                .api
                .read_process_info_json
                .expect("validated function pointer"),
        )
    }
}

#[cfg(test)]
#[path = "amd_tests.rs"]
mod tests;
