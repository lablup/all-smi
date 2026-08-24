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

use std::ffi::c_void;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use super::{
    AMD_PLUGIN_ABI_VERSION, AMD_PLUGIN_FILENAME, AmdPluginApiV1, AmdPluginBuffer, PluginMetadata,
    default_candidates, validate_api, validate_candidate, validate_metadata,
};

unsafe extern "C" fn create() -> *mut c_void {
    std::ptr::null_mut()
}
unsafe extern "C" fn destroy(_handle: *mut c_void) {}
unsafe extern "C" fn read(_handle: *mut c_void, _out: *mut AmdPluginBuffer) -> i32 {
    0
}
unsafe extern "C" fn metadata(_out: *mut AmdPluginBuffer) -> i32 {
    0
}
unsafe extern "C" fn free(_buffer: *mut AmdPluginBuffer) {}

fn api(version: u32, struct_size: usize) -> AmdPluginApiV1 {
    AmdPluginApiV1 {
        abi_version: version,
        struct_size,
        create_reader: Some(create),
        destroy_reader: Some(destroy),
        read_gpu_info_json: Some(read),
        read_process_info_json: Some(read),
        read_metadata_json: Some(metadata),
        free_buffer: Some(free),
    }
}

#[test]
fn search_paths_are_qualified_and_never_include_the_working_directory() {
    let candidates = default_candidates(Path::new("/opt/all-smi/bin/all-smi"));
    assert_eq!(
        candidates[0],
        PathBuf::from("/opt/all-smi/bin").join(AMD_PLUGIN_FILENAME)
    );
    assert!(candidates.iter().all(|path| path.is_absolute()));
    assert!(
        candidates
            .iter()
            .all(|path| path != Path::new(AMD_PLUGIN_FILENAME))
    );
}

#[test]
fn mismatched_or_truncated_abi_is_rejected_clearly() {
    let size = std::mem::size_of::<AmdPluginApiV1>();
    assert!(validate_api(&api(AMD_PLUGIN_ABI_VERSION, size)).is_ok());
    let mismatch = validate_api(&api(AMD_PLUGIN_ABI_VERSION + 1, size)).unwrap_err();
    assert!(mismatch.contains("ABI mismatch"));
    let truncated = validate_api(&api(AMD_PLUGIN_ABI_VERSION, size - 1)).unwrap_err();
    assert!(truncated.contains("truncated"));
    let mut null_entry = api(AMD_PLUGIN_ABI_VERSION, size);
    null_entry.create_reader = None;
    let null_error = validate_api(&null_entry).unwrap_err();
    assert!(null_error.contains("null function pointer"));
}

#[test]
fn world_writable_plugin_locations_are_refused() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let plugin_dir = temp.path().join("plugins");
    fs::create_dir(&plugin_dir).expect("plugin directory");
    fs::set_permissions(&plugin_dir, fs::Permissions::from_mode(0o777))
        .expect("world-writable mode");
    let plugin = plugin_dir.join(AMD_PLUGIN_FILENAME);
    fs::write(&plugin, b"not a library").expect("plugin fixture");
    let error = validate_candidate(&plugin).unwrap_err();
    assert!(error.contains("world-writable"), "{error}");
}

#[test]
fn mismatched_plugin_version_or_wire_format_is_rejected() {
    let mut metadata = PluginMetadata {
        plugin_version: "0.0.0".to_string(),
        libamdgpu_top_version: "0.11.5".to_string(),
        wire_format: super::AMD_PLUGIN_WIRE_FORMAT.to_string(),
    };
    let version_error = validate_metadata(&metadata).unwrap_err();
    assert!(version_error.contains("plugin version mismatch"));

    metadata.plugin_version = env!("CARGO_PKG_VERSION").to_string();
    metadata.wire_format = "incompatible-wire-format".to_string();
    let wire_error = validate_metadata(&metadata).unwrap_err();
    assert!(wire_error.contains("wire format mismatch"));
}
