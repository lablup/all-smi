//! Disk metrics mock template generator

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

use crate::mock::constants::{PLACEHOLDER_DISK_AVAIL, PLACEHOLDER_DISK_TOTAL};
use rand::{RngExt, rng};

/// Add disk metrics to template
pub fn add_disk_metrics(template: &mut String, instance_name: &str) {
    template.push_str("# HELP all_smi_disk_total_bytes Total disk space in bytes\n");
    template.push_str("# TYPE all_smi_disk_total_bytes gauge\n");

    let disk_labels = format!("instance=\"{instance_name}\", mount_point=\"/\", index=\"0\"");
    template.push_str(&format!(
        "all_smi_disk_total_bytes{{{disk_labels}}} {PLACEHOLDER_DISK_TOTAL}\n"
    ));

    template.push_str("# HELP all_smi_disk_available_bytes Available disk space in bytes\n");
    template.push_str("# TYPE all_smi_disk_available_bytes gauge\n");
    template.push_str(&format!(
        "all_smi_disk_available_bytes{{{disk_labels}}} {PLACEHOLDER_DISK_AVAIL}\n"
    ));

    // Disk utilization percentage
    template.push_str("# HELP all_smi_disk_utilization_percent Disk utilization percentage\n");
    template.push_str("# TYPE all_smi_disk_utilization_percent gauge\n");
    template.push_str(&format!(
        "all_smi_disk_utilization_percent{{{disk_labels}}} {{{{DISK_UTIL}}}}\n"
    ));

    // Disk I/O metrics
    template.push_str("# HELP all_smi_disk_read_bytes_per_sec Disk read bytes per second\n");
    template.push_str("# TYPE all_smi_disk_read_bytes_per_sec gauge\n");
    template.push_str(&format!(
        "all_smi_disk_read_bytes_per_sec{{{disk_labels}}} {{{{DISK_READ}}}}\n"
    ));

    template.push_str("# HELP all_smi_disk_write_bytes_per_sec Disk write bytes per second\n");
    template.push_str("# TYPE all_smi_disk_write_bytes_per_sec gauge\n");
    template.push_str(&format!(
        "all_smi_disk_write_bytes_per_sec{{{disk_labels}}} {{{{DISK_WRITE}}}}\n"
    ));
}

/// Render disk metrics with dynamic values
#[allow(dead_code)]
pub fn render_disk_metrics(response: String) -> String {
    let mut rng = rng();

    // Random disk size: 1TB, 4TB, or 12TB
    let disk_total = match rng.random_range(0..3) {
        0 => 1_099_511_627_776u64,  // 1TB
        1 => 4_398_046_511_104u64,  // 4TB
        _ => 13_194_139_533_312u64, // 12TB
    };

    // Available disk space (20-80% of total)
    let disk_avail = (disk_total as f64 * rng.random_range(0.2..0.8)) as u64;
    let disk_used = disk_total - disk_avail;
    let disk_util = (disk_used as f64 / disk_total as f64) * 100.0;

    // I/O rates (in bytes/sec)
    let disk_read = rng.random_range(0..100_000_000); // 0-100 MB/s
    let disk_write = rng.random_range(0..50_000_000); // 0-50 MB/s

    response
        .replace("{{DISK_TOTAL}}", &disk_total.to_string())
        .replace("{{DISK_AVAIL}}", &disk_avail.to_string())
        .replace("{{DISK_UTIL}}", &format!("{disk_util:.2}"))
        .replace("{{DISK_READ}}", &disk_read.to_string())
        .replace("{{DISK_WRITE}}", &disk_write.to_string())
}

/// Generate disk metrics for a specific size
#[allow(dead_code)]
pub fn generate_disk_metrics_with_size(total_bytes: u64) -> (u64, u64) {
    let mut rng = rng();
    let available_bytes = (total_bytes as f64 * rng.random_range(0.2..0.8)) as u64;
    (available_bytes, total_bytes)
}
