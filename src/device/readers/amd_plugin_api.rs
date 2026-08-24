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

//! Versioned C ABI shared by the Linux AMD runtime loader and its plugin.
//!
//! Rust values never cross this boundary. The plugin owns an opaque reader
//! handle and returns JSON in a byte buffer that must be released through the
//! same function table. Any incompatible table or wire-format change requires
//! a new ABI version and entry-point symbol.

use std::ffi::c_void;

pub const AMD_PLUGIN_ABI_VERSION: u32 = 1;
pub const AMD_PLUGIN_ENTRY_SYMBOL: &[u8] = b"all_smi_amd_plugin_entry_v1\0";
pub const AMD_PLUGIN_WIRE_FORMAT: &str = "all-smi-json-v1";

#[repr(C)]
#[derive(Debug, Default)]
pub struct AmdPluginBuffer {
    pub ptr: *mut u8,
    pub len: usize,
    pub capacity: usize,
}

pub type CreateReaderFn = unsafe extern "C" fn() -> *mut c_void;
pub type DestroyReaderFn = unsafe extern "C" fn(*mut c_void);
pub type ReadJsonFn = unsafe extern "C" fn(*mut c_void, *mut AmdPluginBuffer) -> i32;
pub type ReadMetadataFn = unsafe extern "C" fn(*mut AmdPluginBuffer) -> i32;
pub type FreeBufferFn = unsafe extern "C" fn(*mut AmdPluginBuffer);

#[repr(C)]
#[derive(Clone, Copy)]
pub struct AmdPluginApiV1 {
    pub abi_version: u32,
    pub struct_size: usize,
    pub create_reader: Option<CreateReaderFn>,
    pub destroy_reader: Option<DestroyReaderFn>,
    pub read_gpu_info_json: Option<ReadJsonFn>,
    pub read_process_info_json: Option<ReadJsonFn>,
    pub read_metadata_json: Option<ReadMetadataFn>,
    pub free_buffer: Option<FreeBufferFn>,
}
