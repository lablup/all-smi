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
/// The backend is opt-in on Linux (via the `level_zero` cargo feature) and
/// **always on for a Windows target**, regardless of the feature. Three
/// facts make that the right default rather than a convenience:
///
/// 1. It costs nothing to link. The backend pulls in no extra crates; it
///    `dlopen`s `ze_loader.dll` through `libloading`, which is already an
///    unconditional Windows dependency. Compiling it in adds no `NEEDED`
///    entry and no startup cost on a machine that does not have the DLL.
/// 2. Nothing else on Windows can supply the fields. GPU temperature,
///    power, and frequency have no WMI, DXGI, or PDH source. Without this
///    backend those columns are permanently empty on Intel hardware.
/// 3. We ship one `x86_64-pc-windows-msvc` artifact. Making the backend
///    opt-in at build time would mean publishing an Intel and a non-Intel
///    Windows package, and Intel's share of the x86 client install base
///    makes "the build without it" the wrong default for the single
///    artifact we do ship.
///
/// macOS never gets the alias. No Apple platform has a Level Zero runtime,
/// and the module is gated on `any(linux, windows)` besides.
///
/// Cargo cannot express "this feature defaults on for one target", hence
/// the alias. Every consumer then writes one uniform
/// `#[cfg(all_smi_level_zero)]` instead of repeating the disjunction, and
/// `all-smi doctor` reports the resulting state as `level_zero:` in
/// version.txt because `features:` cannot answer the question on Windows.
fn emit_level_zero_cfg() {
    // Without this, every `#[cfg(all_smi_level_zero)]` site draws an
    // `unexpected_cfgs` warning, which `-D warnings` turns into a build
    // failure in CI.
    println!("cargo::rustc-check-cfg=cfg(all_smi_level_zero)");

    // `CARGO_CFG_TARGET_OS` is the target's OS. `CARGO_FEATURE_LEVEL_ZERO`
    // is set only when the feature is enabled for this build.
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let feature_on = std::env::var_os("CARGO_FEATURE_LEVEL_ZERO").is_some();

    let enabled = match target_os.as_str() {
        "windows" => true,
        "linux" => feature_on,
        _ => false,
    };

    if enabled {
        println!("cargo::rustc-cfg=all_smi_level_zero");
    }
}
