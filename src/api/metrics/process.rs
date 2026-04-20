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

use super::{MetricBuilder, MetricExporter};
use crate::device::ProcessInfo;

pub struct ProcessMetricExporter<'a> {
    pub process_info: &'a [ProcessInfo],
}

impl<'a> ProcessMetricExporter<'a> {
    pub fn new(process_info: &'a [ProcessInfo]) -> Self {
        Self { process_info }
    }

    /// Public re-export of the internal `start_time` string parser so
    /// callers outside the exporter (e.g. `ParsedProcessRow::
    /// from_local_process`) can reuse the same logic without
    /// duplicating the format list.
    pub fn parse_start_time_seconds_public(start_time: &str) -> u64 {
        Self::parse_start_time_seconds(start_time)
    }

    /// Parse HH:MM:SS.cs-style `start_time` strings into wall-clock seconds.
    /// Returns 0 when the string is empty or unparseable so downstream
    /// consumers can treat "no start time" as "alive for zero seconds" and
    /// fall back to other ranking fields (e.g. cumulative CPU time).
    ///
    /// Accepted forms (drawn from the local collectors):
    /// - `HH:MM:SS`
    /// - `MM:SS`
    /// - `HH:MM:SS.cs` (Apple / BSD tooling)
    /// - `SS.cs`
    /// - `SS`
    /// - bare integer seconds
    fn parse_start_time_seconds(start_time: &str) -> u64 {
        let trimmed = start_time.trim();
        if trimmed.is_empty() {
            return 0;
        }

        // Strip subsecond component (BSD `ps` produces `...:00.12`).
        let without_fraction = trimmed.split_once('.').map(|(l, _)| l).unwrap_or(trimmed);

        let parts: Vec<&str> = without_fraction.split(':').collect();
        let mut total: u64 = 0;
        let mut multiplier: u64 = 1;
        for part in parts.iter().rev() {
            match part.parse::<u64>() {
                Ok(v) => {
                    total = total.saturating_add(v.saturating_mul(multiplier));
                    multiplier = multiplier.saturating_mul(60);
                }
                Err(_) => return 0,
            }
        }
        total
    }

    fn export_process_metrics(&self, builder: &mut MetricBuilder, process: &ProcessInfo) {
        let pid_str = process.pid.to_string();
        let device_id_str = process.device_id.to_string();

        // `gpu_index` is a primary grouping label for the cluster-wide
        // Users tab (issue #189). It mirrors `device_id` for NVIDIA /
        // AMD / others; keeping both lets dashboards that already query
        // `device_id` keep working while the new column-first clients
        // can use `gpu_index`.
        let labels = [
            ("pid", pid_str.as_str()),
            ("name", process.process_name.as_str()),
            ("user", process.user.as_str()),
            ("device_id", device_id_str.as_str()),
            ("gpu_index", device_id_str.as_str()),
            ("device_uuid", process.device_uuid.as_str()),
            ("command", process.command.as_str()),
        ];

        // Process GPU memory usage.
        builder
            .help(
                "all_smi_process_memory_used_bytes",
                "Process GPU memory used in bytes",
            )
            .type_("all_smi_process_memory_used_bytes", "gauge")
            .metric(
                "all_smi_process_memory_used_bytes",
                &labels,
                process.used_memory,
            );

        // Process start-time expressed as wall-clock seconds since the
        // process began. This feeds the `LONGEST` column on the Users
        // tab (issue #189): the remote aggregator computes
        // `max(start_time_seconds)` across every matching
        // `(host, pid)` pair owned by the user.
        //
        // Emitted as a gauge rather than a counter because the value is
        // relative (elapsed wall-clock at the moment of scrape), not a
        // monotonically-increasing Prometheus counter.
        let start_seconds = Self::parse_start_time_seconds(&process.start_time);
        builder
            .help(
                "all_smi_process_start_time_seconds",
                "Wall-clock seconds since the process started (TIME+ \
                 equivalent)",
            )
            .type_("all_smi_process_start_time_seconds", "gauge")
            .metric("all_smi_process_start_time_seconds", &labels, start_seconds);

        // CPU percent — handy for dashboards but not required by the
        // Users tab; still emitted so the tab can fall back when a
        // dashboard reuses the same scrape.
        builder
            .help(
                "all_smi_process_cpu_percent",
                "Process CPU utilization percentage",
            )
            .type_("all_smi_process_cpu_percent", "gauge")
            .metric(
                "all_smi_process_cpu_percent",
                &labels,
                format!("{:.2}", process.cpu_percent),
            );
    }
}

impl<'a> MetricExporter for ProcessMetricExporter<'a> {
    fn export_metrics(&self) -> String {
        if self.process_info.is_empty() {
            return String::new();
        }

        let mut builder = MetricBuilder::new();

        for process in self.process_info {
            self.export_process_metrics(&mut builder, process);
        }

        builder.build()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::ProcessInfo;

    fn make_process(pid: u32, user: &str, start_time: &str, used_memory: u64) -> ProcessInfo {
        ProcessInfo {
            device_id: 1,
            device_uuid: "GPU-UUID-1".to_string(),
            pid,
            process_name: "python".to_string(),
            used_memory,
            cpu_percent: 12.5,
            memory_percent: 1.0,
            memory_rss: 0,
            memory_vms: 0,
            user: user.to_string(),
            state: "R".to_string(),
            start_time: start_time.to_string(),
            cpu_time: 0,
            command: "python train.py".to_string(),
            ppid: 1,
            threads: 1,
            uses_gpu: used_memory > 0,
            priority: 20,
            nice_value: 0,
            gpu_utilization: 0.0,
        }
    }

    #[test]
    fn exporter_is_silent_on_empty_input() {
        let exporter = ProcessMetricExporter::new(&[]);
        assert_eq!(exporter.export_metrics(), "");
    }

    #[test]
    fn exporter_emits_memory_and_start_time() {
        let procs = [make_process(1234, "alice", "01:02:03", 2_000_000_000)];
        let exporter = ProcessMetricExporter::new(&procs);
        let out = exporter.export_metrics();

        assert!(
            out.contains("all_smi_process_memory_used_bytes"),
            "missing memory metric: {out}"
        );
        assert!(
            out.contains("all_smi_process_start_time_seconds"),
            "missing start-time metric: {out}"
        );
        // 01:02:03 -> 1*3600 + 2*60 + 3 = 3723
        assert!(
            out.contains("3723"),
            "expected start-seconds 3723 in output: {out}"
        );
        assert!(out.contains("user=\"alice\""), "missing user label: {out}");
        assert!(out.contains("pid=\"1234\""), "missing pid label: {out}");
        assert!(
            out.contains("gpu_index=\"1\""),
            "missing gpu_index label: {out}"
        );
    }

    #[test]
    fn parse_start_time_handles_plain_seconds() {
        assert_eq!(ProcessMetricExporter::parse_start_time_seconds("42"), 42);
    }

    #[test]
    fn parse_start_time_handles_mm_ss() {
        assert_eq!(
            ProcessMetricExporter::parse_start_time_seconds("05:30"),
            330
        );
    }

    #[test]
    fn parse_start_time_handles_hh_mm_ss() {
        assert_eq!(
            ProcessMetricExporter::parse_start_time_seconds("02:00:00"),
            7200
        );
    }

    #[test]
    fn parse_start_time_strips_fraction() {
        assert_eq!(
            ProcessMetricExporter::parse_start_time_seconds("00:10.55"),
            10
        );
    }

    #[test]
    fn parse_start_time_returns_zero_on_junk() {
        assert_eq!(ProcessMetricExporter::parse_start_time_seconds(""), 0);
        assert_eq!(ProcessMetricExporter::parse_start_time_seconds("abc"), 0);
        assert_eq!(
            ProcessMetricExporter::parse_start_time_seconds("12:xx"),
            0,
            "malformed input must not panic"
        );
    }
}
