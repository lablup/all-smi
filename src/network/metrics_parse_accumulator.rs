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

//! Per-scrape accumulator behind [`MetricsParser::parse_metrics_with_chassis`].
//!
//! Split out of the line loop so the routing of one already-tokenized line
//! is a single method, independent of how the line was tokenized (issue
//! #500 replaced the regex tokenizer; the equivalence tests drive the old
//! and new tokenizers through this same routing).

use std::collections::{HashMap, HashSet};

use crate::device::{CpuInfo, GpuInfo, MemoryInfo};
use crate::storage::info::StorageInfo;

use super::super::chassis_metrics_parser::ChassisParseState;
use super::super::prometheus_labels::Labels;
use super::{
    CpuTierParseState, MetricsParser, MigParseState, ParsedMetrics, ParsedProcessRow,
    ParsedRemoteMetrics, VgpuParseState,
};

/// Limit the maximum size of HashMaps to prevent memory exhaustion
const MAX_DEVICES_PER_TYPE: usize = 256;

/// Cap process rows to keep a pathological scrape from turning into an OOM:
/// 50 k rows is two orders of magnitude beyond any realistic node (the
/// issue #189 target is 100 nodes x 50 procs = 5 k).
const MAX_PROCESS_ROWS: usize = 50_000;

/// Longest metric name (without the `all_smi_` prefix) accepted, in bytes.
pub(crate) const MAX_METRIC_NAME_BYTES: usize = 256;

/// Longest label string (the text between the braces) accepted, in bytes.
///
/// Equal to the label parser's own input bound, so there is one limit. It
/// used to be 1024 bytes; the issue #500 measurement found an NVIDIA
/// `all_smi_gpu_info` line carrying every detail key the NVML reader can set
/// reaches 769 bytes (the mock templates peak at 246), too close to that cap
/// to keep it.
pub(crate) const MAX_LABEL_STRING_BYTES: usize = super::super::prometheus_labels::MAX_INPUT_LENGTH;

/// Lines dropped by the name or label-string cap during one scrape, reported
/// once per scrape so an over-long line is never lost silently.
#[derive(Default)]
pub(crate) struct OverlongLines {
    count: usize,
    metrics: HashSet<String>,
}

impl OverlongLines {
    /// Distinct metric names remembered for the report.
    const MAX_NAMES: usize = 16;

    pub(crate) fn note(&mut self, metric_name: &str) {
        self.count += 1;
        if self.metrics.len() < Self::MAX_NAMES {
            let end = metric_name
                .char_indices()
                .map(|(i, c)| i + c.len_utf8())
                .take_while(|&end| end <= 64)
                .last()
                .unwrap_or(0);
            self.metrics.insert(metric_name[..end].to_string());
        }
    }

    #[cfg(test)]
    pub(crate) fn count(&self) -> usize {
        self.count
    }

    pub(crate) fn report(&self, host: &str) {
        if self.count == 0 {
            return;
        }
        let mut names: Vec<&str> = self.metrics.iter().map(String::as_str).collect();
        names.sort_unstable();
        tracing::warn!(
            host,
            dropped = self.count,
            metrics = ?names,
            "dropped metric lines over the {MAX_METRIC_NAME_BYTES}-byte name or \
             {MAX_LABEL_STRING_BYTES}-byte label limit"
        );
    }
}

pub(crate) struct ParseAccumulator {
    gpu_info_map: HashMap<String, GpuInfo>,
    cpu_info_map: HashMap<String, CpuInfo>,
    memory_info_map: HashMap<String, MemoryInfo>,
    storage_info_map: HashMap<String, StorageInfo>,
    chassis_state: ChassisParseState,
    /// Keyed by (gpu_uuid, vgpu_id) for instance rows, and by
    /// (gpu_uuid, "__host__") for host-scoped metrics.
    vgpu_state: VgpuParseState,
    /// Keyed by gpu_uuid for parent host rows, and by
    /// (gpu_uuid, mig_instance) for per-instance rows.
    mig_state: MigParseState,
    /// Per-process row accumulator (issue #189). Keyed by `(pid, gpu_index)`
    /// so the three metric families (memory, start-time, cpu-percent)
    /// collapse into a single row even though they arrive on separate lines.
    process_info_map: HashMap<(u32, u32), ParsedProcessRow>,
    host_instance_name: Option<String>,
    /// Tier series + per-core `cpu_tier` labels, resolved after the loop
    /// because Prometheus line order is not guaranteed (issue #473).
    cpu_tier_state: CpuTierParseState,
}

impl ParseAccumulator {
    pub(crate) fn new() -> Self {
        Self {
            gpu_info_map: HashMap::with_capacity(16),
            cpu_info_map: HashMap::with_capacity(8),
            memory_info_map: HashMap::with_capacity(8),
            storage_info_map: HashMap::with_capacity(32),
            chassis_state: ChassisParseState::new(),
            vgpu_state: VgpuParseState::new(),
            mig_state: MigParseState::new(),
            process_info_map: HashMap::with_capacity(32),
            host_instance_name: None,
            cpu_tier_state: CpuTierParseState::new(),
        }
    }

    /// Route one tokenized line to its metric family.
    pub(crate) fn process_line(
        &mut self,
        parser: &MetricsParser,
        metric_name: &str,
        labels: &Labels<'_>,
        value: f64,
        host: &str,
    ) {
        // Extract instance name from the first metric that has it
        if self.host_instance_name.is_none()
            && let Some(instance) = labels.get("instance")
        {
            self.host_instance_name = Some(instance.to_string());
        }

        // Route vGPU and MIG lines first so they aren't swallowed by the
        // broader `gpu_` prefix below. `nvlink_*` metrics from issue #132 are
        // also hardware-detail lines that must join the per-GPU accumulator:
        // they carry the same GPU base label set as the rest of the GPU rows.
        // NPU families share the same accumulator but may identify themselves
        // with either the GPU (`gpu_uuid`/`gpu_index`) or NPU
        // (`npu_uuid`/`npu_index`) label aliases; see `process_gpu_metrics`
        // for the fallback chain.
        if metric_name.starts_with("chassis_") {
            self.chassis_state.process(metric_name, labels, value, host);
        } else if metric_name.starts_with("vgpu_") {
            self.vgpu_state.process(metric_name, labels, value, host);
        } else if metric_name == "gpu_mig_mode" || metric_name.starts_with("mig_instance_") {
            self.mig_state.process(metric_name, labels, value, host);
        } else if metric_name.starts_with("gpu_")
            || metric_name.starts_with("npu_")
            || metric_name.starts_with("nvlink_")
            || metric_name == "ane_utilization"
            || metric_name.starts_with("dla_")
        {
            if self.gpu_info_map.len() < MAX_DEVICES_PER_TYPE {
                parser.process_gpu_metrics(
                    &mut self.gpu_info_map,
                    metric_name,
                    labels,
                    value,
                    host,
                );
            }
        } else if metric_name.starts_with("cpu_") {
            if self.cpu_info_map.len() < MAX_DEVICES_PER_TYPE {
                parser.process_cpu_metrics(
                    &mut self.cpu_info_map,
                    metric_name,
                    labels,
                    value,
                    host,
                    &mut self.cpu_tier_state,
                );
            }
        } else if metric_name.starts_with("memory_") || metric_name.starts_with("swap_") {
            // Swap metrics share the same per-host accumulator as memory:
            // they carry identical `instance`/`hostname`/`index` labels and
            // are conceptually a property of the host's memory subsystem
            // (issue #220).
            if self.memory_info_map.len() < MAX_DEVICES_PER_TYPE {
                parser.process_memory_metrics(
                    &mut self.memory_info_map,
                    metric_name,
                    labels,
                    value,
                    host,
                );
            }
        } else if (metric_name.starts_with("storage_") || metric_name.starts_with("disk_"))
            && self.storage_info_map.len() < MAX_DEVICES_PER_TYPE
        {
            parser.process_storage_metrics(
                &mut self.storage_info_map,
                metric_name,
                labels,
                value,
                host,
            );
        } else if metric_name.starts_with("process_")
            && self.process_info_map.len() < MAX_PROCESS_ROWS
        {
            parser.process_process_metrics(
                &mut self.process_info_map,
                metric_name,
                labels,
                value,
                host,
            );
        }
    }

    pub(crate) fn finish(mut self, parser: &MetricsParser) -> ParsedRemoteMetrics {
        // Store instance name in detail field if available, but keep host as the key
        if let Some(instance_name) = self.host_instance_name.take() {
            parser.update_instance_names(
                &mut self.gpu_info_map,
                &mut self.cpu_info_map,
                &mut self.memory_info_map,
                &mut self.storage_info_map,
                &instance_name,
            );
        }

        // Resolve tier labels now that every line has been read. This must
        // run before the maps are flattened into vectors.
        self.cpu_tier_state.finish(&mut self.cpu_info_map);

        ParsedRemoteMetrics {
            chassis_info: self.chassis_state.finish(),
            metrics: ParsedMetrics {
                gpu_info: self.gpu_info_map.into_values().collect(),
                cpu_info: self.cpu_info_map.into_values().collect(),
                memory_info: self.memory_info_map.into_values().collect(),
                storage_info: self.storage_info_map.into_values().collect(),
                vgpu_info: self.vgpu_state.finish(),
                mig_info: self.mig_state.finish(),
                process_info: self.process_info_map.into_values().collect(),
            },
        }
    }
}
