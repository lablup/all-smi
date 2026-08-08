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

//! `amd.*` checks: ROCm, libamdgpu_top, DRI access, and build-time gating
//! (the musl target gate and the default-on `amd` cargo feature).

#[cfg(target_os = "linux")]
use std::time::Duration;

#[cfg(target_os = "linux")]
use crate::doctor::exec::try_exec;
use crate::doctor::types::{Check, CheckCtx, CheckResult, Severity};

static CHECKS: &[&Check] = &[
    &ROCM_VERSION,
    &LIBAMDGPU_TOP_ABI,
    &DRI_PERMS,
    &BUILD_GATE,
    &ADL_LIBRARY,
    &ADL_SENSORS,
];

pub fn checks() -> &'static [&'static Check] {
    CHECKS
}

/// The exact `libamdgpu_top` version pinned in `Cargo.toml`.
///
/// Cargo hands the compiler no dependency versions, so `env!` cannot reach
/// this and the value has to be transcribed. `check_libamdgpu_top` used to
/// format `env!("CARGO_PKG_VERSION")` into the ABI string, which reported
/// all-smi's own version as the dependency's (issue #362). The transcription
/// is kept honest by `pinned_version_matches_cargo_toml`, which parses the `=`
/// pin out of `Cargo.toml` and fails when the two disagree, so a pin bump that
/// forgets this constant breaks a test instead of shipping a wrong ABI
/// identifier.
///
/// The `test` arm of the `cfg` keeps the constant alive in configurations
/// where the reporting arm below is compiled out (musl, non-Linux, or the
/// `amd` feature off) so the guard test runs everywhere. Without it the
/// constant would be dead code in exactly those builds.
#[cfg(any(
    all(target_os = "linux", not(target_env = "musl"), feature = "amd"),
    test
))]
const LIBAMDGPU_TOP_PINNED_VERSION: &str = "0.11.5";

static ROCM_VERSION: Check = Check {
    id: "amd.rocm.version",
    title: "ROCm version",
    severity_on_fail: Severity::Warn,
    run: check_rocm,
};

static LIBAMDGPU_TOP_ABI: Check = Check {
    id: "amd.libamdgpu_top.abi",
    title: "libamdgpu_top ABI",
    severity_on_fail: Severity::Warn,
    run: check_libamdgpu_top,
};

static DRI_PERMS: Check = Check {
    id: "amd.dri.perms",
    title: "/dev/dri permissions",
    severity_on_fail: Severity::Warn,
    run: check_dri_perms,
};

// The check id stays `amd.build.target_env` for compatibility with existing
// bundles and docs even though it now reports the `amd` cargo feature as well.
static BUILD_GATE: Check = Check {
    id: "amd.build.target_env",
    title: "AMD build-time availability",
    severity_on_fail: Severity::Warn,
    run: check_build_gate,
};

static ADL_LIBRARY: Check = Check {
    id: "amd.adl.library",
    title: "AMD ADL library (atiadlxx.dll)",
    severity_on_fail: Severity::Info,
    run: check_adl_library,
};

static ADL_SENSORS: Check = Check {
    id: "amd.adl.sensors",
    title: "AMD ADL PMLog sensors",
    severity_on_fail: Severity::Info,
    run: check_adl_sensors,
};

/// Report whether `atiadlxx.dll` loaded and an ADL2 context was created.
fn check_adl_library(_ctx: &CheckCtx) -> CheckResult {
    #[cfg(target_os = "windows")]
    {
        use crate::device::readers::amd_adl::loader;

        let path = loader::dll_path();
        if !std::path::Path::new(path).exists() {
            return CheckResult::Skip(format!(
                "{path} not present; install AMD's graphics driver for temperature, power, fan, \
                 and clock readings"
            ));
        }
        if !loader::library_available() {
            return CheckResult::Warn(
                format!("{path} exists but ADL2_Main_Control_Create failed"),
                Some(
                    "the driver install may be partial; reinstalling AMD Software usually \
                     restores it"
                        .to_string(),
                ),
            );
        }
        match loader::selected_adapter_index() {
            Some(index) => CheckResult::Pass(format!(
                "{path} loaded, PMLog-capable adapter index {index}"
            )),
            None => CheckResult::Warn(
                format!("{path} loaded but no adapter exposes the PMLog sensor table"),
                Some(
                    "expected on pre-Vega cards, whose sensors live behind the legacy Overdrive \
                     5/6/7 entry points that all-smi does not implement"
                        .to_string(),
                ),
            ),
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        CheckResult::Skip("ADL is Windows-only".to_string())
    }
}

/// Dump the PMLog sensor table as raw `index=value` pairs.
///
/// This is the field-verification path for the sensor index mapping in
/// `device/readers/amd_adl/sensors.rs`. Those indices are transcribed
/// from AMD's public headers and cannot be checked by anything in CI:
/// no job compiles all-smi for Windows and no test can call the real
/// library. Printing the raw table lets an operator on real hardware
/// confirm or correct the mapping without a code change shipping first.
///
/// The dump is deliberately unfiltered and shows indices rather than
/// names, because an unexpected index is exactly the evidence needed.
fn check_adl_sensors(_ctx: &CheckCtx) -> CheckResult {
    #[cfg(target_os = "windows")]
    {
        use crate::device::readers::amd_adl::{loader, sensors};

        let Some(output) = loader::sample() else {
            return CheckResult::Skip(
                "no PMLog sample available (see amd.adl.library)".to_string(),
            );
        };

        let raw = sensors::supported_raw(&output);
        if raw.is_empty() {
            return CheckResult::Warn(
                "PMLog returned a table with no supported sensors".to_string(),
                None,
            );
        }

        let readout = sensors::extract(&output);
        let dump = raw
            .iter()
            .map(|(index, value)| format!("{index}={value}"))
            .collect::<Vec<_>>()
            .join(" ");

        let interpreted = format!(
            "edge={:?}C hotspot={:?}C mem={:?}C power={:?}W fan={:?}rpm gfx={:?}MHz \
             mclk={:?}MHz activity={:?}%",
            readout.temperature_edge_c,
            readout.temperature_hotspot_c,
            readout.temperature_mem_c,
            readout.power_w,
            readout.fan_rpm,
            readout.clock_gfx_mhz,
            readout.clock_mem_mhz,
            readout.activity_gfx_pct,
        );

        let count = raw.len();
        if readout.is_empty() {
            return CheckResult::Warn(
                format!(
                    "{count} sensor(s) reported supported but none passed the range guard. \
                     raw: {dump}"
                ),
                Some(
                    "this is what a shifted ADLSensorType enum looks like; please report the raw \
                     dump so the index mapping can be corrected"
                        .to_string(),
                ),
            );
        }

        CheckResult::Pass(format!(
            "{count} supported sensor(s); interpreted: {interpreted}; raw: {dump}"
        ))
    }
    #[cfg(not(target_os = "windows"))]
    {
        CheckResult::Skip("ADL is Windows-only".to_string())
    }
}

fn check_rocm(_ctx: &CheckCtx) -> CheckResult {
    #[cfg(target_os = "linux")]
    {
        // Check canonical install locations first.
        for path in &[
            "/opt/rocm/.info/version",
            "/opt/rocm/.info/version-dev",
            "/opt/rocm/lib/rocm-release-info/version",
        ] {
            if let Ok(s) = std::fs::read_to_string(path) {
                let v = s.trim();
                if !v.is_empty() {
                    return CheckResult::Pass(format!("ROCm {v} ({path})"));
                }
            }
        }
        // Fall back to `rocminfo`.
        if let Some(out) = try_exec("rocminfo", &[], Duration::from_millis(2_500))
            && out.success()
            && let Some(line) = out.stdout.lines().find(|l| l.contains("ROCm"))
        {
            return CheckResult::Pass(line.trim().to_string());
        }
        CheckResult::Skip("ROCm not detected (neither /opt/rocm nor rocminfo)".to_string())
    }
    #[cfg(not(target_os = "linux"))]
    {
        CheckResult::Skip("ROCm is Linux-only".to_string())
    }
}

fn check_libamdgpu_top(_ctx: &CheckCtx) -> CheckResult {
    // Three distinct Linux outcomes, so a user never gets told the wrong
    // reason for a missing AMD backend: linked, compiled out by the musl
    // target gate, or compiled out by the `amd` cargo feature (issue #345).
    #[cfg(all(target_os = "linux", not(target_env = "musl"), feature = "amd"))]
    {
        // The `libamdgpu_top` crate is linked at compile time; if this
        // binary was built with AMD support the dep is present. Surface
        // the dependency's pinned version as the ABI identifier, not
        // all-smi's own version (issue #362).
        CheckResult::Pass(format!(
            "linked libamdgpu_top {LIBAMDGPU_TOP_PINNED_VERSION}"
        ))
    }
    #[cfg(all(target_os = "linux", not(target_env = "musl"), not(feature = "amd")))]
    {
        CheckResult::Skip(
            "libamdgpu_top not linked: built without the `amd` cargo feature (see \
             amd.build.target_env)"
                .to_string(),
        )
    }
    #[cfg(all(target_os = "linux", target_env = "musl"))]
    {
        CheckResult::Skip(
            "libamdgpu_top not linked in musl builds (see amd.build.target_env)".to_string(),
        )
    }
    #[cfg(not(target_os = "linux"))]
    {
        CheckResult::Skip("libamdgpu_top is Linux-only".to_string())
    }
}

fn check_dri_perms(_ctx: &CheckCtx) -> CheckResult {
    #[cfg(target_os = "linux")]
    {
        let p = std::path::Path::new("/dev/dri");
        if !p.exists() {
            return CheckResult::Skip("/dev/dri missing".to_string());
        }
        match std::fs::read_dir(p) {
            Ok(iter) => {
                let nodes: Vec<String> = iter
                    .filter_map(|e| e.ok())
                    .filter_map(|e| e.file_name().into_string().ok())
                    .filter(|n| n.starts_with("render") || n.starts_with("card"))
                    .collect();
                if nodes.is_empty() {
                    CheckResult::Skip("no card* or render* nodes".to_string())
                } else {
                    CheckResult::Pass(format!("{} node(s): {}", nodes.len(), nodes.join(", ")))
                }
            }
            Err(e) => CheckResult::Fail(
                format!("/dev/dri unreadable: {e}"),
                Some("ensure the caller is in the render group".to_string()),
            ),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        CheckResult::Skip("/dev/dri is Linux-only".to_string())
    }
}

/// Report which build-time gate, if any, compiled the AMD backend out.
///
/// Two independent gates can remove it: the musl target gate, and the
/// default-on `amd` cargo feature (issue #345). The three arms below are
/// mutually exclusive and exhaustive, and each names the gate that actually
/// applies so `all-smi doctor` never blames the wrong one.
fn check_build_gate(_ctx: &CheckCtx) -> CheckResult {
    #[cfg(target_env = "musl")]
    {
        CheckResult::Warn(
            "musl build — AMD support compiled out".to_string(),
            Some("use a glibc build (x86_64-unknown-linux-gnu) for AMD GPU monitoring".to_string()),
        )
    }
    #[cfg(all(target_os = "linux", not(target_env = "musl"), not(feature = "amd")))]
    {
        CheckResult::Warn(
            "glibc build without the `amd` cargo feature: AMD support compiled out".to_string(),
            Some(
                "rebuild with the default features, or add `--features amd`, for AMD GPU \
                 monitoring; the feature is off here because something disabled it (typically a \
                 downstream `default-features = false`) to avoid linking libdrm"
                    .to_string(),
            ),
        )
    }
    #[cfg(all(
        not(target_env = "musl"),
        any(not(target_os = "linux"), feature = "amd")
    ))]
    {
        CheckResult::Pass("glibc or non-Linux target — AMD support available".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::LIBAMDGPU_TOP_PINNED_VERSION;

    /// `Cargo.toml` is embedded at compile time rather than read from a
    /// runtime path so the test does not depend on the working directory,
    /// and so editing the manifest forces a rebuild of this test.
    const MANIFEST: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"));

    /// Extract the exact version from the `libamdgpu_top` dependency line.
    ///
    /// Returns `None` when no dependency line is found or the version cannot
    /// be read, which the caller turns into a failure: a manifest the parser
    /// no longer understands must not quietly pass the guard. Comment lines
    /// mentioning the crate are skipped because they start with `#`.
    fn pinned_libamdgpu_top_version(manifest: &str) -> Option<&str> {
        let line = manifest
            .lines()
            .map(str::trim)
            .find(|l| l.starts_with("libamdgpu_top") && l.contains("version"))?;
        let after_key = line.split_once("version")?.1;
        let after_quote = after_key.split_once('"')?.1;
        let value = after_quote.split_once('"')?.0;
        Some(value.trim().trim_start_matches('=').trim())
    }

    /// The reported ABI identifier must be the pinned dependency version.
    ///
    /// This is the forcing function the `Cargo.toml` comment asks for by
    /// hand: bumping the `=` pin without updating
    /// [`LIBAMDGPU_TOP_PINNED_VERSION`] fails here rather than shipping a
    /// wrong version to whoever is debugging an AMD ABI problem.
    #[test]
    fn pinned_version_matches_cargo_toml() {
        let pinned = pinned_libamdgpu_top_version(MANIFEST).expect(
            "could not parse the libamdgpu_top version out of Cargo.toml; if the dependency \
             declaration moved or changed shape, update pinned_libamdgpu_top_version",
        );
        assert_eq!(
            pinned, LIBAMDGPU_TOP_PINNED_VERSION,
            "libamdgpu_top is pinned to {pinned} in Cargo.toml but amd.libamdgpu_top.abi reports \
             {LIBAMDGPU_TOP_PINNED_VERSION}; update LIBAMDGPU_TOP_PINNED_VERSION to match the pin"
        );
    }

    /// The pin must stay an exact `=` requirement. A caret or range would
    /// let Cargo resolve a different version than the one reported, which
    /// this guard could not detect.
    #[test]
    fn libamdgpu_top_is_pinned_exactly() {
        let line = MANIFEST
            .lines()
            .map(str::trim)
            .find(|l| l.starts_with("libamdgpu_top") && l.contains("version"))
            .expect("libamdgpu_top dependency line not found in Cargo.toml");
        assert!(
            line.contains("\"="),
            "libamdgpu_top must stay pinned with an exact `=` requirement so the reported ABI \
             version is the resolved one, got: {line}"
        );
    }
}
