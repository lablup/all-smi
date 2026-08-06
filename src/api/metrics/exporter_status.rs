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

//! Exporter self-description: the baseline every scrape carries
//! (issue #324).
//!
//! Every other exporter in [`crate::api::metrics`] self-filters, so a host
//! with no devices, or a process whose first collection cycle has not
//! landed yet, used to render a byte-empty `/metrics` body with a
//! `200 OK`. A scrape that lands in that window is recorded by Prometheus
//! as a *successful* scrape with zero samples, which is a silent hole in
//! the series rather than a failed target: it does not alert.
//!
//! This exporter is the one that never self-filters. It emits two
//! families unconditionally, from the very first request:
//!
//! * `all_smi_up` — `0` until the first collection cycle has written into
//!   `AppState`, `1` afterwards. This is what lets a consumer tell
//!   "up but not ready" apart from "up with nothing to report", which
//!   previously were the same zero bytes.
//! * `all_smi_build_info` — the constant-`1` build-info idiom used by
//!   `node_exporter` (`node_exporter_build_info`) and Prometheus itself
//!   (`prometheus_build_info`): the interesting content is in the labels,
//!   and `version` is joinable onto any other series with
//!   `* on (instance) group_left(version)`.
//!
//! `/metrics` keeps answering `200` throughout. Readiness is a separate,
//! explicitly queryable signal at `/-/ready`
//! (see [`crate::api::handlers::ready`]); this exporter is the in-band
//! half of the same contract, for consumers that only ever see a scrape.

use crate::utils::get_hostname;

use super::{MetricBuilder, MetricExporter};

/// Liveness/readiness gauge. `0` before the first collection cycle.
pub const UP_METRIC: &str = "all_smi_up";

/// Constant-`1` build-info gauge. All the content is in the labels.
pub const BUILD_INFO_METRIC: &str = "all_smi_build_info";

/// Emits [`UP_METRIC`] and [`BUILD_INFO_METRIC`].
pub struct ExporterStatusMetricExporter {
    /// Whether at least one collection cycle has populated `AppState`.
    ready: bool,
}

impl ExporterStatusMetricExporter {
    pub fn new(ready: bool) -> Self {
        Self { ready }
    }
}

impl MetricExporter for ExporterStatusMetricExporter {
    fn export_metrics(&self) -> String {
        // `get_hostname` reads a process-wide `Lazy<String>`, so this is a
        // clone rather than a syscall per scrape. It is also the exact
        // source every device reader uses for its own `instance` /
        // `hostname` labels (e.g. `MemoryInfo.instance`), so these two
        // families join cleanly onto the rest of the exposition instead
        // of introducing a second spelling of the same host.
        let hostname = get_hostname();

        // Label ordering and naming follow the house convention every
        // other exporter in this module uses: `instance` first, then
        // `hostname`. Prometheus supplies its own `instance` target
        // label, but `all-smi view --hosts` scrapes these endpoints
        // directly with no Prometheus in the middle and keys on the
        // exporter-provided one, so omitting it here would make the
        // baseline the only unattributable family in the body.
        let up_labels = [
            ("instance", hostname.as_str()),
            ("hostname", hostname.as_str()),
        ];

        let mut builder = MetricBuilder::new();
        builder
            .help(
                UP_METRIC,
                "1 once the exporter has completed a collection cycle, 0 before that",
            )
            .type_(UP_METRIC, "gauge")
            .metric(UP_METRIC, &up_labels, u8::from(self.ready));

        // `version` is the label operators actually alert on. `os` and
        // `arch` are compile-time constants and cost nothing, and this
        // is a fleet tool whose whole point is heterogeneous clusters,
        // so "which build is on which kind of box" is a question the
        // exposition should be able to answer on its own. Deliberately
        // no git revision: the crate has no build script that stamps
        // one, and adding one would make otherwise identical builds
        // differ.
        let build_labels = [
            ("instance", hostname.as_str()),
            ("hostname", hostname.as_str()),
            ("version", env!("CARGO_PKG_VERSION")),
            ("os", std::env::consts::OS),
            ("arch", std::env::consts::ARCH),
        ];
        builder
            .help(
                BUILD_INFO_METRIC,
                "Build information for the running all-smi binary; always 1",
            )
            .type_(BUILD_INFO_METRIC, "gauge")
            .metric(BUILD_INFO_METRIC, &build_labels, 1);

        builder.build()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_ready_emits_up_zero() {
        let rendered = ExporterStatusMetricExporter::new(false).export_metrics();
        assert!(
            rendered.contains("} 0\n"),
            "all_smi_up must be 0 before the first collection cycle: {rendered}"
        );
        assert!(rendered.contains("# TYPE all_smi_up gauge\n"));
        assert!(rendered.contains("all_smi_up{instance="));
    }

    #[test]
    fn ready_emits_up_one() {
        let rendered = ExporterStatusMetricExporter::new(true).export_metrics();
        let up_line = rendered
            .lines()
            .find(|l| l.starts_with("all_smi_up{"))
            .expect("all_smi_up sample line");
        assert!(up_line.ends_with(" 1"), "expected up=1, got {up_line}");
    }

    /// The whole point of this exporter: it never self-filters, so the
    /// exposition can never be byte-empty again.
    #[test]
    fn always_emits_something_in_both_states() {
        for ready in [false, true] {
            let rendered = ExporterStatusMetricExporter::new(ready).export_metrics();
            assert!(!rendered.is_empty());
            assert!(rendered.contains("all_smi_build_info{"));
        }
    }

    #[test]
    fn build_info_carries_version_os_and_arch() {
        let rendered = ExporterStatusMetricExporter::new(true).export_metrics();
        let line = rendered
            .lines()
            .find(|l| l.starts_with("all_smi_build_info{"))
            .expect("build info sample line");
        assert!(
            line.contains(&format!("version=\"{}\"", env!("CARGO_PKG_VERSION"))),
            "missing version label: {line}"
        );
        assert!(line.contains(&format!("os=\"{}\"", std::env::consts::OS)));
        assert!(line.contains(&format!("arch=\"{}\"", std::env::consts::ARCH)));
        assert!(line.ends_with(" 1"), "build info must always be 1: {line}");
    }

    /// Both families must carry the same host identity the device
    /// exporters use, or a dashboard cannot join them.
    #[test]
    fn both_families_share_one_host_identity() {
        let hostname = get_hostname();
        let rendered = ExporterStatusMetricExporter::new(true).export_metrics();
        for family in ["all_smi_up{", "all_smi_build_info{"] {
            let line = rendered
                .lines()
                .find(|l| l.starts_with(family))
                .unwrap_or_else(|| panic!("missing {family}"));
            assert!(line.contains(&format!("instance=\"{hostname}\"")), "{line}");
            assert!(line.contains(&format!("hostname=\"{hostname}\"")), "{line}");
        }
    }
}
