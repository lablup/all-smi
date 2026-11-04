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

//! `amd.gpu_metrics.revision` check.
//!
//! The companion reads `gpu_metrics` through the pinned `gpu_metrics` parser.
//! When the driver emits a table revision that parser does not know, the
//! companion silently falls back to `gpu_busy_percent` and hwmon, so a
//! degraded primary source looks exactly like an idle GPU. This check reads
//! only the four-byte table header from sysfs, so it reports the mismatch
//! without linking libdrm into the main binary.

use crate::doctor::types::{Check, CheckCtx, CheckResult, Severity};

pub(super) static GPU_METRICS_REVISION: Check = Check {
    id: "amd.gpu_metrics.revision",
    title: "AMD gpu_metrics table revision",
    severity_on_fail: Severity::Warn,
    run: check_gpu_metrics_revision,
};

#[cfg(all(target_os = "linux", not(target_env = "musl")))]
use crate::device::readers::amd_plugin_api::{gpu_metrics_ceiling, gpu_metrics_supported};

/// Sysfs class directory whose `card[0-9]*` entries expose `device/gpu_metrics`.
#[cfg(all(target_os = "linux", not(target_env = "musl")))]
const DRM_CLASS_PATH: &str = "/sys/class/drm";

fn check_gpu_metrics_revision(_ctx: &CheckCtx) -> CheckResult {
    #[cfg(all(target_os = "linux", not(target_env = "musl")))]
    {
        match observe_cards() {
            Ok(observations) => gpu_metrics_verdict(&observations),
            Err(message) => CheckResult::Skip(message),
        }
    }
    #[cfg(all(target_os = "linux", target_env = "musl"))]
    {
        CheckResult::Skip(
            "AMD gpu_metrics tables are unavailable in musl builds (the companion plugin is glibc-only)"
                .to_string(),
        )
    }
    #[cfg(not(target_os = "linux"))]
    {
        CheckResult::Skip("gpu_metrics is Linux-only".to_string())
    }
}

/// The two revision fields the parser dispatches on, read from a
/// `gpu_metrics` table header.
#[cfg(all(target_os = "linux", not(target_env = "musl")))]
#[derive(Clone, Debug, PartialEq, Eq)]
struct MetricsHeader {
    format_revision: u8,
    content_revision: u8,
}

/// One card's probe: the header that was read, or why it could not be used.
#[cfg(all(target_os = "linux", not(target_env = "musl")))]
#[derive(Clone, Debug, PartialEq, Eq)]
struct CardObservation {
    card: String,
    path: String,
    header: Result<MetricsHeader, String>,
}

/// Enumerate the GPUs that expose a `gpu_metrics` file.
///
/// Connector entries such as `card0-HDMI-A-1` are skipped: only `card`
/// followed by digits is a GPU. Cards without the file (older hardware, or a
/// render-only node) are not observations at all, so a host with none of them
/// reports `Skip` rather than an unobserved parse concern. The file is opened
/// once, without a preceding `is_file` stat, so a card cannot be skipped or
/// included on a stale answer about a path that changed in between.
#[cfg(all(target_os = "linux", not(target_env = "musl")))]
fn observe_cards() -> Result<Vec<CardObservation>, String> {
    use std::path::PathBuf;

    let entries = std::fs::read_dir(DRM_CLASS_PATH)
        .map_err(|error| format!("could not list {DRM_CLASS_PATH}: {error}"))?;

    let mut cards: Vec<(String, PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            is_card_name(&name).then(|| (name, entry.path().join("device").join("gpu_metrics")))
        })
        .collect();
    cards.sort_by(|left, right| left.0.cmp(&right.0));

    let mut observations = Vec::with_capacity(cards.len());
    for (card, path) in cards {
        let header = match read_header(&path) {
            Ok(Some(header)) => Ok(header),
            Ok(None) => continue,
            Err(reason) => Err(reason),
        };
        observations.push(CardObservation {
            card,
            path: path.display().to_string(),
            header,
        });
    }
    Ok(observations)
}

/// `card` followed by one or more ASCII digits, so `card0` matches and
/// `card0-HDMI-A-1` (a connector) does not.
#[cfg(all(target_os = "linux", not(target_env = "musl")))]
fn is_card_name(name: &str) -> bool {
    name.strip_prefix("card")
        .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|byte| byte.is_ascii_digit()))
}

/// Read the first four bytes of `gpu_metrics` and return the revision pair.
///
/// Layout: little-endian `u16` `structure_size`, then `u8` `format_revision`,
/// then `u8` `content_revision`. `Ok(None)` means the card has no table at
/// all, which is not an observation. A short or unreadable file and a zero
/// `structure_size` are errors, never a guessed revision; the caller turns
/// them into `Skip` because doctor must not report a parse concern it could
/// not observe.
#[cfg(all(target_os = "linux", not(target_env = "musl")))]
fn read_header(path: &std::path::Path) -> Result<Option<MetricsHeader>, String> {
    use std::io::{ErrorKind, Read};

    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("unreadable ({error})")),
    };
    if !file
        .metadata()
        .map_err(|error| format!("unreadable ({error})"))?
        .is_file()
    {
        return Ok(None);
    }
    let mut bytes = [0u8; 4];
    file.read_exact(&mut bytes)
        .map_err(|error| format!("shorter than four bytes ({error})"))?;
    parse_header(bytes).map(Some)
}

#[cfg(all(target_os = "linux", not(target_env = "musl")))]
fn parse_header(bytes: [u8; 4]) -> Result<MetricsHeader, String> {
    let structure_size = u16::from_le_bytes([bytes[0], bytes[1]]);
    if structure_size == 0 {
        return Err("header structure_size is 0".to_string());
    }
    Ok(MetricsHeader {
        format_revision: bytes[2],
        content_revision: bytes[3],
    })
}

/// Turn the per-card observations into a check result.
///
/// An unsupported table wins over a supported one because it is the condition
/// the check exists to surface; with several unsupported cards the newest
/// `(format, content)` pair is reported and its card named. If nothing could
/// be read, the result is `Skip` naming the paths, not `Warn`.
#[cfg(all(target_os = "linux", not(target_env = "musl")))]
fn gpu_metrics_verdict(observations: &[CardObservation]) -> CheckResult {
    if observations.is_empty() {
        return CheckResult::Skip(format!(
            "no card exposes {DRM_CLASS_PATH}/card[0-9]*/device/gpu_metrics"
        ));
    }

    let readable: Vec<(&CardObservation, &MetricsHeader)> = observations
        .iter()
        .filter_map(|observation| {
            observation
                .header
                .as_ref()
                .ok()
                .map(|header| (observation, header))
        })
        .collect();

    if let Some((observation, header)) = readable
        .iter()
        .filter(|(_, header)| {
            !gpu_metrics_supported(header.format_revision, header.content_revision)
        })
        .max_by_key(|(_, header)| (header.format_revision, header.content_revision))
    {
        return CheckResult::Warn(
            format!(
                "{} reports format {} content {}, outside the pinned parser ceiling ({}); utilization falls back to gpu_busy_percent, while temperature, power, and frequency fall back to hwmon, and any newer-table-only metric reports its default",
                observation.card,
                header.format_revision,
                header.content_revision,
                gpu_metrics_ceiling()
            ),
            Some(
                "monitoring continues through those fallbacks; the gpu_metrics-only fields need a companion built against a newer parser"
                    .to_string(),
            ),
        );
    }

    if readable.is_empty() {
        let detail = observations
            .iter()
            .map(|observation| {
                let reason = observation
                    .header
                    .as_ref()
                    .err()
                    .map(String::as_str)
                    .unwrap_or("unreadable");
                format!("{} ({}: {reason})", observation.card, observation.path)
            })
            .collect::<Vec<_>>()
            .join(", ");
        return CheckResult::Skip(format!("could not read a gpu_metrics header: {detail}"));
    }

    let observed = readable
        .iter()
        .map(|(observation, header)| {
            format!(
                "{} format {} content {}",
                observation.card, header.format_revision, header.content_revision
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    let unreadable = observations
        .iter()
        .filter_map(|observation| {
            observation
                .header
                .as_ref()
                .err()
                .map(|reason| format!("{} ({reason})", observation.card))
        })
        .collect::<Vec<_>>();
    let note = if unreadable.is_empty() {
        String::new()
    } else {
        format!("; unreadable: {}", unreadable.join(", "))
    };
    CheckResult::Pass(format!(
        "{observed} (parser ceiling: {}){note}",
        gpu_metrics_ceiling()
    ))
}

#[cfg(all(test, target_os = "linux", not(target_env = "musl")))]
mod tests {
    use super::{
        CardObservation, MetricsHeader, gpu_metrics_verdict, is_card_name, parse_header,
        read_header,
    };
    use crate::doctor::types::CheckResult;

    fn supported(card: &str, format_revision: u8, content_revision: u8) -> CardObservation {
        CardObservation {
            card: card.to_string(),
            path: format!("/sys/class/drm/{card}/device/gpu_metrics"),
            header: Ok(MetricsHeader {
                format_revision,
                content_revision,
            }),
        }
    }

    fn unreadable(card: &str, reason: &str) -> CardObservation {
        CardObservation {
            card: card.to_string(),
            path: format!("/sys/class/drm/{card}/device/gpu_metrics"),
            header: Err(reason.to_string()),
        }
    }

    #[test]
    fn only_gpu_card_entries_match() {
        assert!(is_card_name("card0"));
        assert!(is_card_name("card12"));
        assert!(!is_card_name("card"));
        assert!(!is_card_name("card0-HDMI-A-1"));
        assert!(!is_card_name("renderD128"));
        assert!(!is_card_name("card0x"));
    }

    #[test]
    fn a_card_without_a_table_is_not_an_observation() {
        let path = std::env::temp_dir().join(format!(
            "all-smi-missing-gpu-metrics-{}",
            std::process::id()
        ));
        assert_eq!(read_header(&path), Ok(None));
    }

    #[test]
    fn header_layout_is_little_endian_structure_size_then_revisions() {
        let header = parse_header([0x3c, 0x00, 1, 9]).expect("valid header");
        assert_eq!(header.format_revision, 1);
        assert_eq!(header.content_revision, 9);
    }

    #[test]
    fn a_zero_structure_size_is_not_a_revision() {
        assert!(parse_header([0, 0, 1, 9]).is_err());
    }

    #[test]
    fn no_card_with_the_file_is_a_skip() {
        assert!(matches!(
            gpu_metrics_verdict(&[]),
            CheckResult::Skip(message) if message.contains("no card exposes")
        ));
    }

    #[test]
    fn a_supported_table_passes_with_the_ceiling() {
        let result = gpu_metrics_verdict(&[supported("card0", 1, 5)]);
        let CheckResult::Pass(message) = result else {
            panic!("expected Pass, got {result:?}");
        };
        assert!(message.contains("format 1 content 5"));
        assert!(message.contains("format 2 content 0-4"));
    }

    #[test]
    fn a_newer_table_warns_and_names_the_card() {
        let result = gpu_metrics_verdict(&[supported("card0", 1, 9)]);
        let CheckResult::Warn(message, _) = result else {
            panic!("expected Warn, got {result:?}");
        };
        assert!(message.contains("card0"));
        assert!(message.contains("format 1 content 9"));
        assert!(message.contains("gpu_busy_percent"));
        assert!(message.contains("hwmon"));
    }

    #[test]
    fn the_newest_unsupported_table_is_reported_when_cards_disagree() {
        let result = gpu_metrics_verdict(&[
            supported("card0", 1, 6),
            supported("card1", 1, 9),
            supported("card2", 2, 0),
        ]);
        let CheckResult::Warn(message, _) = result else {
            panic!("expected Warn, got {result:?}");
        };
        assert!(
            message.contains("card1"),
            "newest pair must name its card: {message}"
        );
        assert!(message.contains("format 1 content 9"));
    }

    #[test]
    fn a_supported_table_wins_over_an_unreadable_one() {
        let result = gpu_metrics_verdict(&[
            supported("card0", 2, 4),
            unreadable("card1", "shorter than four bytes"),
        ]);
        let CheckResult::Pass(message) = result else {
            panic!("expected Pass, got {result:?}");
        };
        assert!(message.contains("card0 format 2 content 4"));
        assert!(
            message.contains("card1 (shorter than four bytes)"),
            "an unreadable card must still be named in the pass detail: {message}"
        );
    }

    #[test]
    fn nothing_readable_is_a_skip_naming_the_paths() {
        let result = gpu_metrics_verdict(&[unreadable("card0", "shorter than four bytes")]);
        let CheckResult::Skip(message) = result else {
            panic!("expected Skip, got {result:?}");
        };
        assert!(message.contains("/sys/class/drm/card0/device/gpu_metrics"));
        assert!(message.contains("shorter than four bytes"));
    }
}
