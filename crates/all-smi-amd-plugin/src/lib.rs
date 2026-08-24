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

#![cfg(all(target_os = "linux", not(target_env = "musl")))]

mod reader;

use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};

use all_smi::device::GpuReader;
use all_smi::device::readers::amd_plugin_api::{
    AMD_PLUGIN_ABI_VERSION, AMD_PLUGIN_WIRE_FORMAT, AmdPluginApiV1, AmdPluginBuffer,
};
use serde::Serialize;

use reader::AmdGpuReader;

const LIBAMDGPU_TOP_VERSION: &str = "0.11.5";
const STATUS_OK: i32 = 0;
const STATUS_INVALID_ARGUMENT: i32 = -1;
const STATUS_FAILED: i32 = -2;

#[derive(Serialize)]
struct PluginMetadata<'a> {
    plugin_version: &'a str,
    libamdgpu_top_version: &'a str,
    wire_format: &'a str,
}

fn write_json<T: Serialize>(value: &T, out: *mut AmdPluginBuffer) -> i32 {
    if out.is_null() {
        return STATUS_INVALID_ARGUMENT;
    }
    let Ok(mut bytes) = serde_json::to_vec(value) else {
        return STATUS_FAILED;
    };
    let buffer = AmdPluginBuffer {
        ptr: bytes.as_mut_ptr(),
        len: bytes.len(),
        capacity: bytes.capacity(),
    };
    std::mem::forget(bytes);
    // SAFETY: `out` was checked for null and points to host-provided storage
    // matching the versioned ABI table that exposed this function.
    unsafe { out.write(buffer) };
    STATUS_OK
}

extern "C" fn create_reader() -> *mut c_void {
    catch_unwind(AssertUnwindSafe(|| {
        Box::into_raw(Box::new(AmdGpuReader::new())).cast::<c_void>()
    }))
    .unwrap_or(std::ptr::null_mut())
}

extern "C" fn destroy_reader(handle: *mut c_void) {
    if handle.is_null() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: the host obtained this handle from `create_reader` and the
        // ABI contract requires exactly one matching destroy call.
        unsafe { drop(Box::from_raw(handle.cast::<AmdGpuReader>())) };
    }));
}

fn read_with(
    handle: *mut c_void,
    out: *mut AmdPluginBuffer,
    collect: impl FnOnce(&AmdGpuReader, *mut AmdPluginBuffer) -> i32,
) -> i32 {
    if handle.is_null() || out.is_null() {
        return STATUS_INVALID_ARGUMENT;
    }
    catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: the handle was created by this plugin and remains owned by
        // the host for the duration of the call.
        let reader = unsafe { &*handle.cast::<AmdGpuReader>() };
        collect(reader, out)
    }))
    .unwrap_or(STATUS_FAILED)
}

extern "C" fn read_gpu_info_json(handle: *mut c_void, out: *mut AmdPluginBuffer) -> i32 {
    read_with(handle, out, |reader, out| {
        write_json(&reader.get_gpu_info(), out)
    })
}

extern "C" fn read_process_info_json(handle: *mut c_void, out: *mut AmdPluginBuffer) -> i32 {
    read_with(handle, out, |reader, out| {
        write_json(&reader.get_process_info(), out)
    })
}

extern "C" fn read_metadata_json(out: *mut AmdPluginBuffer) -> i32 {
    let metadata = PluginMetadata {
        plugin_version: env!("CARGO_PKG_VERSION"),
        libamdgpu_top_version: LIBAMDGPU_TOP_VERSION,
        wire_format: AMD_PLUGIN_WIRE_FORMAT,
    };
    write_json(&metadata, out)
}

extern "C" fn free_buffer(buffer: *mut AmdPluginBuffer) {
    if buffer.is_null() {
        return;
    }
    // SAFETY: the pointer was checked above and the ABI gives this function
    // exclusive access to the descriptor for the duration of the call.
    let buffer = unsafe { &mut *buffer };
    if !buffer.ptr.is_null() && buffer.len <= buffer.capacity {
        // SAFETY: `write_json` created this allocation from a Vec with exactly
        // these pointer, length, and capacity values, then forgot it.
        unsafe { drop(Vec::from_raw_parts(buffer.ptr, buffer.len, buffer.capacity)) };
    }
    *buffer = AmdPluginBuffer::default();
}

static API_V1: AmdPluginApiV1 = AmdPluginApiV1 {
    abi_version: AMD_PLUGIN_ABI_VERSION,
    struct_size: std::mem::size_of::<AmdPluginApiV1>(),
    create_reader: Some(create_reader),
    destroy_reader: Some(destroy_reader),
    read_gpu_info_json: Some(read_gpu_info_json),
    read_process_info_json: Some(read_process_info_json),
    read_metadata_json: Some(read_metadata_json),
    free_buffer: Some(free_buffer),
};

/// Return the immutable v1 function table.
///
/// A loader must validate both `abi_version` and `struct_size` before calling
/// any pointer in the table.
#[unsafe(no_mangle)]
pub extern "C" fn all_smi_amd_plugin_entry_v1() -> *const AmdPluginApiV1 {
    &raw const API_V1
}

#[cfg(test)]
mod tests {
    use super::{LIBAMDGPU_TOP_VERSION, PluginMetadata};

    const MANIFEST: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"));

    #[test]
    fn metadata_matches_exact_dependency_pin() {
        let expected = format!("libamdgpu_top = \"={LIBAMDGPU_TOP_VERSION}\"");
        assert!(
            MANIFEST.lines().any(|line| line.trim() == expected),
            "plugin metadata reports libamdgpu_top {LIBAMDGPU_TOP_VERSION}, but Cargo.toml has no matching exact pin"
        );
        let metadata = PluginMetadata {
            plugin_version: env!("CARGO_PKG_VERSION"),
            libamdgpu_top_version: LIBAMDGPU_TOP_VERSION,
            wire_format: all_smi::device::readers::amd_plugin_api::AMD_PLUGIN_WIRE_FORMAT,
        };
        let json = serde_json::to_string(&metadata).expect("metadata serializes");
        assert!(json.contains(LIBAMDGPU_TOP_VERSION));
    }
}
