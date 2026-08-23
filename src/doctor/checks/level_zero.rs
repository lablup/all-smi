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

//! `level_zero.*` checks: the Intel oneAPI Level Zero backend's build-time
//! presence, loader library, runtime initialisation, and device visibility.
//!
//! The support bundle's `version.txt` already carries a `level_zero:` line,
//! but that reports a compile-time cfg. It cannot say whether the loader was
//! found, whether `zeInit` succeeded, or whether Sysman saw any device, and
//! those three facts are what decide whether an operator gets GPU
//! temperature, power, and frequency on Intel hardware. On Windows nothing
//! else supplies those fields at all.
//!
//! Four distinct failures used to collapse into one silent fallback, each
//! with a different remedy. Every stage is reported separately here.
//!
//! ## No check in this namespace fails
//!
//! An absent Level Zero runtime always degrades to sysfs, to WMI, or to an
//! unavailable field. It never breaks a run, so the worst outcome is `Warn`,
//! and a host with no Intel GPU reports `Skip` rather than complaining about
//! hardware it does not have.

use crate::doctor::types::{Check, CheckCtx, CheckResult, Severity};

static CHECKS: &[&Check] = &[&BUILD, &LOADER, &INIT, &DEVICES];

pub fn checks() -> &'static [&'static Check] {
    CHECKS
}

static BUILD: Check = Check {
    id: "level_zero.build",
    title: "Level Zero build-time availability",
    severity_on_fail: Severity::Info,
    run: check_build,
};

static LOADER: Check = Check {
    id: "level_zero.loader",
    title: "Level Zero loader library",
    severity_on_fail: Severity::Warn,
    run: check_loader,
};

static INIT: Check = Check {
    id: "level_zero.init",
    title: "Level Zero runtime initialisation",
    severity_on_fail: Severity::Warn,
    run: check_init,
};

static DEVICES: Check = Check {
    id: "level_zero.devices",
    title: "Level Zero device visibility",
    severity_on_fail: Severity::Info,
    run: check_devices,
};

/// Agrees with `bundle::level_zero_effective` by construction: both read the
/// same cfg, and a second, differently-worded test could disagree with the
/// `level_zero:` line sitting next to it in the same support bundle.
fn check_build(_ctx: &CheckCtx) -> CheckResult {
    if cfg!(all_smi_level_zero) {
        CheckResult::Pass(format!(
            "compiled in for {} (unconditional on this target, not the `level_zero` cargo feature)",
            std::env::consts::OS
        ))
    } else {
        CheckResult::Skip(format!(
            "not compiled in for {}: no Level Zero runtime exists for this platform",
            std::env::consts::OS
        ))
    }
}

#[cfg(all_smi_level_zero)]
mod present {
    use super::*;
    use crate::device::has_intel_gpu;
    use crate::device::readers::intel_gpu_level_zero as l0;

    pub fn loader(_ctx: &CheckCtx) -> CheckResult {
        loader_verdict(&l0::probe(), has_intel_gpu())
    }

    pub fn init(_ctx: &CheckCtx) -> CheckResult {
        init_verdict(&l0::probe(), has_intel_gpu())
    }

    pub fn devices(ctx: &CheckCtx) -> CheckResult {
        devices_verdict(&l0::probe(), has_intel_gpu(), ctx.verbose)
    }

    /// The package that ships the loader, which is not the same thing as the
    /// GPU driver on Linux and is exactly the same thing on Windows.
    fn install_hint() -> &'static str {
        if cfg!(target_os = "windows") {
            "install the Intel graphics driver, which ships ze_loader.dll"
        } else {
            "install the Level Zero loader package (Debian and Ubuntu: libze1; \
             or Intel's oneAPI runtime)"
        }
    }

    /// Whether the loader library itself was found.
    ///
    /// Deliberately **not** gated on Intel GPU presence. It is the one stage a
    /// machine without the hardware can reach, so gating it would make the
    /// whole namespace unreachable on a CI runner, and whether the runtime is
    /// installed is worth knowing before the card arrives.
    pub fn loader_verdict(probe: &l0::LevelZeroProbe, intel_gpu_present: bool) -> CheckResult {
        match probe.loaded_path {
            Some(path) => CheckResult::Pass(format!("loaded {path}")),
            None if intel_gpu_present => CheckResult::Warn(
                format!(
                    "an Intel GPU is present but no Level Zero loader was found; tried {}",
                    probe.searched_paths.join(", ")
                ),
                Some(format!(
                    "{}. Without it, temperature, power, and frequency fall back to \
                     whatever the OS baseline provides.",
                    install_hint()
                )),
            ),
            None => CheckResult::Skip(format!(
                "no Level Zero loader found and no Intel GPU present; tried {}",
                probe.searched_paths.join(", ")
            )),
        }
    }

    pub fn init_verdict(probe: &l0::LevelZeroProbe, intel_gpu_present: bool) -> CheckResult {
        if probe.loaded_path.is_none() {
            return CheckResult::Skip("no Level Zero loader to initialise".to_string());
        }
        if !intel_gpu_present {
            return CheckResult::Skip(
                "loader present but no Intel GPU on this host; nothing to initialise for"
                    .to_string(),
            );
        }
        match probe.init {
            l0::LevelZeroInit::Ok => {
                let route = match probe.sysman_route {
                    Some(l0::SysmanRoute::ZesInit) => "zesInit",
                    Some(l0::SysmanRoute::LegacyEnvVar) => "legacy ZES_ENABLE_SYSMAN=1",
                    None => "unknown route",
                };
                CheckResult::Pass(format!("initialised, Sysman enabled via {route}"))
            }
            l0::LevelZeroInit::SysmanUnavailable => CheckResult::Warn(
                "the loader exports no zesInit and ZES_ENABLE_SYSMAN was not 1 when zeInit ran, \
                 so Sysman cannot be reached"
                    .to_string(),
                Some(
                    "set ZES_ENABLE_SYSMAN=1 before the process starts, not from inside it: \
                     the variable is read at zeInit and all-smi will not mutate its own \
                     environment once threads exist. Upgrading the Level Zero runtime to one \
                     that exports zesInit removes the requirement."
                        .to_string(),
                ),
            ),
            l0::LevelZeroInit::ZeInitFailed(code) => CheckResult::Warn(
                format!("zeInit returned ze_result_t {code:#x}"),
                Some(
                    "the loader is installed but the driver rejected initialisation. Check that \
                     the Intel GPU driver matches the runtime version and that the current user \
                     may open the device."
                        .to_string(),
                ),
            ),
            l0::LevelZeroInit::ZesInitFailed(code) => CheckResult::Warn(
                format!("zesInit returned ze_result_t {code:#x}"),
                Some(
                    "core initialisation succeeded but Sysman did not. This usually means the \
                     driver lacks Sysman support or the process lacks the privilege it needs."
                        .to_string(),
                ),
            ),
            // Unreachable while `loaded_path` is set, since the loader stage
            // is what produces this variant. Handled rather than
            // `unreachable!` so a future recording change cannot panic doctor.
            l0::LevelZeroInit::LoaderMissing => {
                CheckResult::Skip("no Level Zero loader to initialise".to_string())
            }
        }
    }

    pub fn devices_verdict(
        probe: &l0::LevelZeroProbe,
        intel_gpu_present: bool,
        verbose: bool,
    ) -> CheckResult {
        if probe.init != l0::LevelZeroInit::Ok {
            return CheckResult::Skip("Level Zero did not initialise".to_string());
        }
        if !intel_gpu_present {
            return CheckResult::Skip("no Intel GPU on this host".to_string());
        }
        if probe.device_count == 0 {
            return CheckResult::Warn(
                "Level Zero initialised but Sysman enumerated no devices".to_string(),
                Some(
                    "this is the state that produces empty temperature, power, and frequency \
                     with no other symptom. Check that the Intel GPU driver is loaded and that \
                     the runtime version matches it."
                        .to_string(),
                ),
            );
        }
        // The count is always safe to print. The BDFs identify hardware, so
        // they ride behind --verbose like other identifying detail.
        let mut message = format!("{} device(s) visible to Sysman", probe.device_count);
        if verbose {
            message.push_str(&format!(" ({})", probe.device_bdfs.join(", ")));
        }
        CheckResult::Pass(message)
    }
}

#[cfg(not(all_smi_level_zero))]
mod present {
    use super::*;

    pub fn loader(_ctx: &CheckCtx) -> CheckResult {
        CheckResult::Skip("Level Zero backend not compiled in".to_string())
    }

    pub fn init(_ctx: &CheckCtx) -> CheckResult {
        CheckResult::Skip("Level Zero backend not compiled in".to_string())
    }

    pub fn devices(_ctx: &CheckCtx) -> CheckResult {
        CheckResult::Skip("Level Zero backend not compiled in".to_string())
    }
}

fn check_loader(ctx: &CheckCtx) -> CheckResult {
    present::loader(ctx)
}

fn check_init(ctx: &CheckCtx) -> CheckResult {
    present::init(ctx)
}

fn check_devices(ctx: &CheckCtx) -> CheckResult {
    present::devices(ctx)
}

#[cfg(all(test, all_smi_level_zero))]
mod tests {
    use super::present::{devices_verdict, init_verdict, loader_verdict};
    use crate::device::readers::intel_gpu_level_zero as l0;
    use crate::doctor::types::CheckResult;

    fn probe(
        loaded_path: Option<&'static str>,
        init: l0::LevelZeroInit,
        route: Option<l0::SysmanRoute>,
        bdfs: &[&str],
    ) -> l0::LevelZeroProbe {
        l0::LevelZeroProbe {
            compiled_in: true,
            searched_paths: &["libze_loader.so.1", "/usr/lib64/libze_loader.so.1"],
            loaded_path,
            init,
            sysman_route: route,
            device_count: bdfs.len(),
            device_bdfs: bdfs.iter().map(|s| (*s).to_string()).collect(),
        }
    }

    fn ready(bdfs: &[&str]) -> l0::LevelZeroProbe {
        probe(
            Some("libze_loader.so.1"),
            l0::LevelZeroInit::Ok,
            Some(l0::SysmanRoute::ZesInit),
            bdfs,
        )
    }

    // ---------- loader ----------

    /// The CI runner's shape: loader installed, no Intel GPU. It must pass,
    /// because this is the one stage a machine without the hardware reaches,
    /// and a skip here would make the namespace unreachable in CI.
    #[test]
    fn a_loader_without_hardware_still_passes() {
        let r = loader_verdict(&ready(&[]), false);
        assert!(matches!(r, CheckResult::Pass(ref m) if m.contains("libze_loader.so.1")));
    }

    /// The case worth acting on: the card is here and the runtime is not.
    #[test]
    fn hardware_without_a_loader_warns_with_a_hint() {
        let r = loader_verdict(
            &probe(None, l0::LevelZeroInit::LoaderMissing, None, &[]),
            true,
        );
        let CheckResult::Warn(message, fix) = r else {
            panic!("expected Warn, got {r:?}");
        };
        // The searched paths belong in the message: "not found" is useless
        // without saying where we looked.
        assert!(message.contains("libze_loader.so.1"), "{message}");
        assert!(fix.is_some_and(|f| !f.is_empty()));
    }

    /// No card and no runtime is an ordinary machine, not a problem.
    #[test]
    fn neither_hardware_nor_loader_is_a_skip() {
        let r = loader_verdict(
            &probe(None, l0::LevelZeroInit::LoaderMissing, None, &[]),
            false,
        );
        assert!(matches!(r, CheckResult::Skip(_)), "{r:?}");
    }

    // ---------- init ----------

    #[test]
    fn a_successful_init_names_the_sysman_route() {
        let modern = init_verdict(&ready(&["0000:03:00.0"]), true);
        assert!(matches!(modern, CheckResult::Pass(ref m) if m.contains("zesInit")));

        let legacy = init_verdict(
            &probe(
                Some("libze_loader.so.1"),
                l0::LevelZeroInit::Ok,
                Some(l0::SysmanRoute::LegacyEnvVar),
                &["0000:03:00.0"],
            ),
            true,
        );
        assert!(matches!(legacy, CheckResult::Pass(ref m) if m.contains("ZES_ENABLE_SYSMAN")));
    }

    /// The three failure modes must not read alike: each has a different fix.
    #[test]
    fn each_init_failure_is_distinguishable() {
        let sysman = init_verdict(
            &probe(
                Some("libze_loader.so.1"),
                l0::LevelZeroInit::SysmanUnavailable,
                None,
                &[],
            ),
            true,
        );
        let CheckResult::Warn(sysman_msg, sysman_fix) = sysman else {
            panic!("expected Warn");
        };
        assert!(sysman_msg.contains("zesInit"), "{sysman_msg}");
        let sysman_fix = sysman_fix.expect("SysmanUnavailable must carry a fix");
        assert!(sysman_fix.contains("ZES_ENABLE_SYSMAN=1"), "{sysman_fix}");
        assert!(
            sysman_fix.contains("before the process starts"),
            "the fix must say when to set it, not only what to set: {sysman_fix}"
        );

        let ze = init_verdict(
            &probe(
                Some("libze_loader.so.1"),
                l0::LevelZeroInit::ZeInitFailed(0x7800_0001u32 as i32),
                None,
                &[],
            ),
            true,
        );
        let CheckResult::Warn(ze_msg, _) = ze else {
            panic!("expected Warn");
        };
        assert!(ze_msg.contains("zeInit"), "{ze_msg}");
        assert_ne!(ze_msg, sysman_msg);

        let zes = init_verdict(
            &probe(
                Some("libze_loader.so.1"),
                l0::LevelZeroInit::ZesInitFailed(5),
                None,
                &[],
            ),
            true,
        );
        let CheckResult::Warn(zes_msg, _) = zes else {
            panic!("expected Warn");
        };
        assert!(zes_msg.contains("zesInit"), "{zes_msg}");
        assert_ne!(zes_msg, ze_msg);
    }

    /// The numeric result code is what an operator pastes into a search.
    #[test]
    fn an_init_failure_carries_the_result_code() {
        let r = init_verdict(
            &probe(
                Some("libze_loader.so.1"),
                l0::LevelZeroInit::ZeInitFailed(0x7800_0001u32 as i32),
                None,
                &[],
            ),
            true,
        );
        let CheckResult::Warn(message, _) = r else {
            panic!("expected Warn");
        };
        assert!(message.contains("78000001"), "{message}");
    }

    #[test]
    fn init_skips_without_a_loader_or_without_hardware() {
        let no_loader = init_verdict(
            &probe(None, l0::LevelZeroInit::LoaderMissing, None, &[]),
            true,
        );
        assert!(matches!(no_loader, CheckResult::Skip(_)), "{no_loader:?}");

        let no_gpu = init_verdict(&ready(&[]), false);
        assert!(matches!(no_gpu, CheckResult::Skip(_)), "{no_gpu:?}");
    }

    // ---------- devices ----------

    /// Initialised but empty is the silent failure this check exists for.
    #[test]
    fn zero_devices_after_a_successful_init_warns() {
        let r = devices_verdict(&ready(&[]), true, false);
        assert!(matches!(r, CheckResult::Warn(_, Some(_))), "{r:?}");
    }

    #[test]
    fn devices_are_counted_and_listed_only_when_verbose() {
        let two = ready(&["0000:03:00.0", "0000:04:00.0"]);

        let quiet = devices_verdict(&two, true, false);
        let CheckResult::Pass(message) = quiet else {
            panic!("expected Pass");
        };
        assert!(message.contains('2'), "{message}");
        assert!(
            !message.contains("0000:03:00.0"),
            "BDFs identify hardware and must stay behind --verbose: {message}"
        );

        let verbose = devices_verdict(&two, true, true);
        let CheckResult::Pass(message) = verbose else {
            panic!("expected Pass");
        };
        assert!(message.contains("0000:03:00.0"), "{message}");
        assert!(message.contains("0000:04:00.0"), "{message}");
    }

    #[test]
    fn devices_skips_when_init_did_not_succeed() {
        let r = devices_verdict(
            &probe(
                Some("libze_loader.so.1"),
                l0::LevelZeroInit::ZeInitFailed(1),
                None,
                &[],
            ),
            true,
            false,
        );
        assert!(matches!(r, CheckResult::Skip(_)), "{r:?}");
    }
}
