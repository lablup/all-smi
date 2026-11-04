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

/// `gpu_metrics` table revisions the AMD companion can parse.
///
/// The authority is `libdrm_amdgpu_sys` 0.8.16, `amdgpu/gpu_metrics/mod.rs`,
/// where `GpuMetrics::from_raw` matches on the `(format_revision,
/// content_revision)` pair read from the table header. Every pair listed here
/// maps to a concrete `GpuMetrics` variant; every other pair becomes
/// `GpuMetrics::Unknown`, whose accessors all return `None`.
///
/// The `libamdgpu_top = "=0.11.5"` pin does not fix this ceiling:
/// `libamdgpu_top` requires `libdrm_amdgpu_sys >= 0.8.14`, so `Cargo.lock`
/// resolves the parser independently and `cargo update` can move it without
/// touching the pinned crate. The plugin-side test in
/// `crates/all-smi-amd-plugin/src/reader/tests.rs` compares this table against
/// `GpuMetrics::from_raw` and fails when the two drift; update the mirrored
/// version in this comment together with it.
const SUPPORTED_GPU_METRICS: &[(u8, u8)] = &[
    (1, 0),
    (1, 1),
    (1, 2),
    (1, 3),
    (1, 4),
    (1, 5),
    (2, 0),
    (2, 1),
    (2, 2),
    (2, 3),
    (2, 4),
    (3, 0),
];

/// Whether the pinned `gpu_metrics` parser understands this table revision.
///
/// `true` for exactly the `(format_revision, content_revision)` pairs in
/// [`SUPPORTED_GPU_METRICS`].
pub fn gpu_metrics_supported(format_revision: u8, content_revision: u8) -> bool {
    SUPPORTED_GPU_METRICS.contains(&(format_revision, content_revision))
}

/// Human-readable ceiling for the revisions the pinned parser accepts.
///
/// Derived from [`SUPPORTED_GPU_METRICS`] so the message shown by
/// `all-smi doctor` cannot disagree with [`gpu_metrics_supported`]. Runs of
/// consecutive content revisions in one format are collapsed, for example
/// `format 1 content 0-5, format 2 content 0-4, format 3 content 0`.
pub fn gpu_metrics_ceiling() -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut index = 0;
    while index < SUPPORTED_GPU_METRICS.len() {
        let format = SUPPORTED_GPU_METRICS[index].0;
        let first = SUPPORTED_GPU_METRICS[index].1;
        let mut last = first;
        index += 1;
        while let Some(&(next_format, next_content)) = SUPPORTED_GPU_METRICS.get(index) {
            if next_format != format || next_content != last.saturating_add(1) {
                break;
            }
            last = next_content;
            index += 1;
        }
        if first == last {
            parts.push(format!("format {format} content {first}"));
        } else {
            parts.push(format!("format {format} content {first}-{last}"));
        }
    }
    parts.join(", ")
}

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

#[cfg(test)]
mod tests {
    use super::{gpu_metrics_ceiling, gpu_metrics_supported};

    #[test]
    fn supported_pairs_match_the_parser_table() {
        for format in 0..=3u8 {
            for content in 0..=9u8 {
                let expected = matches!((format, content), (1, 0..=5) | (2, 0..=4) | (3, 0));
                assert_eq!(
                    gpu_metrics_supported(format, content),
                    expected,
                    "unexpected verdict for format {format} content {content}"
                );
            }
        }
        assert!(gpu_metrics_supported(1, 5));
        assert!(!gpu_metrics_supported(1, 9));
        assert!(!gpu_metrics_supported(2, 5));
        assert!(!gpu_metrics_supported(3, 1));
        assert!(!gpu_metrics_supported(0, 0));
    }

    #[test]
    fn ceiling_is_derived_from_the_supported_table() {
        assert_eq!(
            gpu_metrics_ceiling(),
            "format 1 content 0-5, format 2 content 0-4, format 3 content 0"
        );
    }
}
