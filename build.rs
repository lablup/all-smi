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

fn main() -> Result<(), Box<dyn std::error::Error>> {
    emit_level_zero_cfg();

    // NOTE: `#[cfg(target_os = ...)]` inside a build script describes the
    // *host*, not the build target. That is correct here, because the proto
    // compilation below needs `protoc` on the machine running the build and
    // only a Linux host ever produces the TPU client. It is the wrong idiom
    // for anything that must follow the target: see `emit_level_zero_cfg`,
    // which reads `CARGO_CFG_TARGET_OS` instead.

    // Only compile proto files on Linux (TPU is Linux-only)
    #[cfg(target_os = "linux")]
    {
        let proto_file = "proto/tpu_metric_service.proto";

        // Check if proto file exists before trying to compile
        if std::path::Path::new(proto_file).exists() {
            let include_paths = ["proto/", "/usr/include"];
            tonic_prost_build::configure()
                .build_server(false) // We only need the client
                .protoc_arg("--experimental_allow_proto3_optional")
                // Suppress clippy warnings on generated protobuf code
                .type_attribute(".", "#[allow(clippy::enum_variant_names)]")
                .compile_protos(&[proto_file], &include_paths)?;
        }
    }

    Ok(())
}

/// Emit the `all_smi_level_zero` cfg alias, which is what every consumer
/// of the Intel Level Zero backend actually gates on.
///
/// On for every Linux and Windows target. Off for everything else, macOS
/// included: no Apple platform has a Level Zero runtime.
///
/// Four facts make always-on the right default rather than a convenience:
///
/// 1. It costs nothing to link. The backend pulls in no extra crates; it
///    `dlopen`s `ze_loader.dll` / `libze_loader.so.1` through
///    `libloading`, already an unconditional dependency on both targets.
///    Compiling it in adds no `NEEDED` entry and no import-table entry.
/// 2. It costs nothing to run without the hardware. `reader_factory`
///    constructs the Intel reader only when an Intel GPU is actually
///    present, so on any other machine the loader is never opened. When
///    one is present but the runtime is absent, the failed load is cached
///    process-wide and the reader keeps its sysfs / WMI baseline.
/// 3. We ship one artifact per target. Making this opt-in at build time
///    would mean publishing an Intel and a non-Intel package for the same
///    platform, and Intel's share of the x86 client install base makes
///    "the build without it" the wrong default for the single artifact we
///    do ship. An Intel Arc owner would otherwise have to build from
///    source to get anything the vendor backend adds.
/// 4. On Windows nothing else can supply the fields at all: GPU
///    temperature, power, and frequency have no WMI, DXGI, or PDH source.
///    Linux has a sysfs baseline, so there the backend is an upgrade
///    rather than the difference between data and empty columns, but the
///    first three points apply identically.
///
/// Cargo cannot express "this feature defaults on for these targets",
/// hence the alias. Every consumer then writes one uniform
/// `#[cfg(all_smi_level_zero)]`, and `all-smi doctor` reports the
/// resulting state as `level_zero:` in version.txt because `features:`
/// cannot answer the question any more.
///
/// The `level_zero` cargo feature is kept as an accepted no-op so that
/// `--features level_zero`, and any downstream manifest that lists it,
/// keeps building. It no longer decides anything.
fn emit_level_zero_cfg() {
    // Without this, every `#[cfg(all_smi_level_zero)]` site draws an
    // `unexpected_cfgs` warning, which `-D warnings` turns into a build
    // failure in CI.
    println!("cargo::rustc-check-cfg=cfg(all_smi_level_zero)");

    // `CARGO_CFG_TARGET_OS` is the *target's* OS, unlike a
    // `#[cfg(target_os)]` in this file, which would describe the host.
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();

    if matches!(target_os.as_str(), "linux" | "windows") {
        println!("cargo::rustc-cfg=all_smi_level_zero");
    }
}
