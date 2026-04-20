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

//! Evaluation of parsed filter expressions against a row.
//!
//! Rows must implement [`DeviceRowView`]. The evaluator follows a
//! "fail closed" rule: fields that are absent on the row make the row
//! *not match*, so a filter like `temp>80` never dims a CPU-only row
//! (which has no `temp`).

use super::lexer::Op;
use super::parser::{Expr, Field, Value};

/// Row-level accessor consumed by [`eval`]. Each `*_field` getter returns
/// `None` when the underlying device does not expose the field — for
/// example, `pstate_field` on an Apple Silicon GPU or `user_field` on a
/// `GpuInfo` row.
///
/// Implementations must stick to read-only accessors so the evaluator
/// can be called during rendering without synchronisation.
#[allow(dead_code)]
pub trait DeviceRowView {
    fn temp_field(&self) -> Option<f64> {
        None
    }
    fn util_field(&self) -> Option<f64> {
        None
    }
    fn mem_pct_field(&self) -> Option<f64> {
        None
    }
    fn mem_used_field(&self) -> Option<f64> {
        None
    }
    fn mem_total_field(&self) -> Option<f64> {
        None
    }
    fn power_field(&self) -> Option<f64> {
        None
    }
    fn user_field(&self) -> Option<&str> {
        None
    }
    fn host_field(&self) -> Option<&str> {
        None
    }
    fn gpu_name_field(&self) -> Option<&str> {
        None
    }
    fn driver_field(&self) -> Option<&str> {
        None
    }
    fn index_field(&self) -> Option<f64> {
        None
    }
    fn uuid_field(&self) -> Option<&str> {
        None
    }
    fn pstate_field(&self) -> Option<f64> {
        None
    }
    fn numa_field(&self) -> Option<f64> {
        None
    }
    fn device_type_field(&self) -> Option<&str> {
        None
    }

    /// Generic presence check used when the evaluator needs to distinguish
    /// "field absent" from "field value compared false". The default
    /// implementation delegates to the individual accessors.
    fn field_absent(&self, field: Field) -> bool {
        match field {
            Field::Temp => self.temp_field().is_none(),
            Field::Util => self.util_field().is_none(),
            Field::MemPct => self.mem_pct_field().is_none(),
            Field::MemUsed => self.mem_used_field().is_none(),
            Field::MemTotal => self.mem_total_field().is_none(),
            Field::Power => self.power_field().is_none(),
            Field::User => self.user_field().is_none(),
            Field::Host => self.host_field().is_none(),
            Field::GpuName => self.gpu_name_field().is_none(),
            Field::Driver => self.driver_field().is_none(),
            Field::Index => self.index_field().is_none(),
            Field::Uuid => self.uuid_field().is_none(),
            Field::Pstate => self.pstate_field().is_none(),
            Field::Numa => self.numa_field().is_none(),
            Field::DeviceType => self.device_type_field().is_none(),
        }
    }
}

/// Evaluate `expr` against `row`. Returns `true` when the row passes the
/// filter. A row is considered a match when no filter is set; call sites
/// check for `Option<Expr>::None` before invoking this.
pub fn eval<R: DeviceRowView + ?Sized>(expr: &Expr, row: &R) -> bool {
    match expr {
        Expr::And(l, r) => eval(l, row) && eval(r, row),
        Expr::Or(l, r) => eval(l, row) || eval(r, row),
        Expr::Cmp { field, op, value } => eval_cmp(*field, *op, value, row),
    }
}

fn eval_cmp<R: DeviceRowView + ?Sized>(field: Field, op: Op, value: &Value, row: &R) -> bool {
    if row.field_absent(field) {
        // Fail closed: a filter over a field the row doesn't expose should
        // not match. This keeps `temp>80` from dimming (or hiding) CPU
        // rows, and it lets the operator combine rules across heterogenous
        // device types safely.
        return false;
    }

    if field.is_numeric() {
        let row_val = match numeric_value(field, row) {
            Some(v) => v,
            None => return false,
        };
        match value {
            Value::Number(n) => compare_numeric(row_val, op, *n),
            Value::String(s) => {
                // Allow `index==0` style where the parser may have emitted
                // Number, but also tolerate `pstate==P0` style string
                // comparisons where the user writes a tag rather than a
                // number.
                match op {
                    Op::Eq => row_val.to_string() == *s,
                    Op::Ne => row_val.to_string() != *s,
                    _ => false,
                }
            }
            Value::Regex(_) => false, // already rejected at parse time
        }
    } else {
        let row_str = match string_value(field, row) {
            Some(s) => s,
            None => return false,
        };
        match value {
            Value::String(s) => match op {
                Op::Eq => row_str == *s,
                Op::Ne => row_str != *s,
                _ => false,
            },
            Value::Regex(r) => matches!(op, Op::Match) && r.is_match(row_str),
            Value::Number(n) => match op {
                Op::Eq => row_str == n.to_string(),
                Op::Ne => row_str != n.to_string(),
                _ => false,
            },
        }
    }
}

fn compare_numeric(lhs: f64, op: Op, rhs: f64) -> bool {
    const EPS: f64 = 1e-9;
    match op {
        Op::Gt => lhs > rhs,
        Op::Ge => lhs >= rhs - EPS,
        Op::Lt => lhs < rhs,
        Op::Le => lhs <= rhs + EPS,
        Op::Eq => (lhs - rhs).abs() < EPS,
        Op::Ne => (lhs - rhs).abs() >= EPS,
        Op::Match => false, // parser rejects this combo
    }
}

fn numeric_value<R: DeviceRowView + ?Sized>(field: Field, row: &R) -> Option<f64> {
    match field {
        Field::Temp => row.temp_field(),
        Field::Util => row.util_field(),
        Field::MemPct => row.mem_pct_field(),
        Field::MemUsed => row.mem_used_field(),
        Field::MemTotal => row.mem_total_field(),
        Field::Power => row.power_field(),
        Field::Index => row.index_field(),
        Field::Pstate => row.pstate_field(),
        Field::Numa => row.numa_field(),
        _ => None,
    }
}

fn string_value<R: DeviceRowView + ?Sized>(field: Field, row: &R) -> Option<&str> {
    match field {
        Field::User => row.user_field(),
        Field::Host => row.host_field(),
        Field::GpuName => row.gpu_name_field(),
        Field::Driver => row.driver_field(),
        Field::Uuid => row.uuid_field(),
        Field::DeviceType => row.device_type_field(),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// DeviceRowView impls for the concrete device structs.
// ---------------------------------------------------------------------------

use crate::device::types::{CpuInfo, GpuInfo, ProcessInfo};

impl DeviceRowView for GpuInfo {
    fn temp_field(&self) -> Option<f64> {
        if self.temperature == 0 {
            None
        } else {
            Some(self.temperature as f64)
        }
    }
    fn util_field(&self) -> Option<f64> {
        if self.utilization < 0.0 {
            None
        } else {
            Some(self.utilization)
        }
    }
    fn mem_pct_field(&self) -> Option<f64> {
        if self.total_memory == 0 {
            return None;
        }
        Some((self.used_memory as f64 / self.total_memory as f64) * 100.0)
    }
    fn mem_used_field(&self) -> Option<f64> {
        Some(self.used_memory as f64)
    }
    fn mem_total_field(&self) -> Option<f64> {
        Some(self.total_memory as f64)
    }
    fn power_field(&self) -> Option<f64> {
        Some(self.power_consumption)
    }
    fn host_field(&self) -> Option<&str> {
        if self.hostname.is_empty() {
            None
        } else {
            Some(&self.hostname)
        }
    }
    fn gpu_name_field(&self) -> Option<&str> {
        Some(&self.name)
    }
    fn driver_field(&self) -> Option<&str> {
        self.detail.get("driver_version").map(|s| s.as_str())
    }
    fn index_field(&self) -> Option<f64> {
        self.detail.get("index").and_then(|s| s.parse::<f64>().ok())
    }
    fn uuid_field(&self) -> Option<&str> {
        if self.uuid.is_empty() {
            None
        } else {
            Some(&self.uuid)
        }
    }
    fn pstate_field(&self) -> Option<f64> {
        self.performance_state.map(|p| p as f64)
    }
    fn numa_field(&self) -> Option<f64> {
        self.numa_node_id.map(|n| n as f64)
    }
    fn device_type_field(&self) -> Option<&str> {
        Some(&self.device_type)
    }
}

impl DeviceRowView for ProcessInfo {
    fn util_field(&self) -> Option<f64> {
        Some(self.cpu_percent)
    }
    fn mem_pct_field(&self) -> Option<f64> {
        Some(self.memory_percent)
    }
    fn mem_used_field(&self) -> Option<f64> {
        Some(self.used_memory as f64)
    }
    fn user_field(&self) -> Option<&str> {
        if self.user.is_empty() {
            None
        } else {
            Some(&self.user)
        }
    }
    fn index_field(&self) -> Option<f64> {
        Some(self.device_id as f64)
    }
    fn uuid_field(&self) -> Option<&str> {
        if self.device_uuid.is_empty() {
            None
        } else {
            Some(&self.device_uuid)
        }
    }
    fn device_type_field(&self) -> Option<&str> {
        Some("PROC")
    }
}

impl DeviceRowView for CpuInfo {
    fn util_field(&self) -> Option<f64> {
        Some(self.utilization)
    }
    fn temp_field(&self) -> Option<f64> {
        self.temperature.map(|t| t as f64)
    }
    fn power_field(&self) -> Option<f64> {
        self.power_consumption
    }
    fn host_field(&self) -> Option<&str> {
        if self.hostname.is_empty() {
            None
        } else {
            Some(&self.hostname)
        }
    }
    fn device_type_field(&self) -> Option<&str> {
        Some("CPU")
    }
}

#[cfg(test)]
mod tests {
    use super::super::parser::parse;
    use super::*;
    use std::collections::HashMap;

    fn make_gpu(index: u32, temp: u32, util: f64, power: f64) -> GpuInfo {
        let mut detail = HashMap::new();
        detail.insert("index".to_string(), index.to_string());
        detail.insert("driver_version".to_string(), "550.54.15".to_string());
        GpuInfo {
            uuid: format!("GPU-{index}"),
            time: String::new(),
            name: "NVIDIA A100-SXM4-80GB".to_string(),
            device_type: "GPU".to_string(),
            host_id: "10.0.0.1:9090".to_string(),
            hostname: "dgx-01".to_string(),
            instance: String::new(),
            utilization: util,
            ane_utilization: 0.0,
            dla_utilization: None,
            tensorcore_utilization: None,
            temperature: temp,
            used_memory: 40 * 1024 * 1024 * 1024,
            total_memory: 80 * 1024 * 1024 * 1024,
            frequency: 1410,
            power_consumption: power,
            gpu_core_count: None,
            temperature_threshold_slowdown: Some(85),
            temperature_threshold_shutdown: Some(90),
            temperature_threshold_max_operating: None,
            temperature_threshold_acoustic: None,
            performance_state: Some(0),
            numa_node_id: Some(0),
            gsp_firmware_mode: None,
            gsp_firmware_version: None,
            nvlink_remote_devices: Vec::new(),
            gpm_metrics: None,
            detail,
        }
    }

    fn must_eval(input: &str, gpu: &GpuInfo) -> bool {
        let expr = parse(input)
            .unwrap()
            .unwrap_or_else(|| panic!("empty filter parsed for `{input}`"));
        eval(&expr, gpu)
    }

    #[test]
    fn temp_greater_than_matches() {
        let gpu = make_gpu(0, 90, 50.0, 300.0);
        assert!(must_eval("temp>85", &gpu));
    }

    #[test]
    fn temp_greater_than_does_not_match_when_cooler() {
        let gpu = make_gpu(0, 70, 50.0, 300.0);
        assert!(!must_eval("temp>85", &gpu));
    }

    #[test]
    fn temp_zero_is_treated_as_absent() {
        // 0 means N/A on the GPU reader side, so filter must fail closed.
        let gpu = make_gpu(0, 0, 50.0, 300.0);
        assert!(!must_eval("temp>0", &gpu));
    }

    #[test]
    fn util_less_than_matches() {
        let gpu = make_gpu(0, 60, 3.0, 100.0);
        assert!(must_eval("util<5", &gpu));
    }

    #[test]
    fn mem_pct_computed_correctly() {
        // 40 GiB / 80 GiB = 50%
        let gpu = make_gpu(0, 60, 50.0, 100.0);
        assert!(must_eval("mem_pct==50", &gpu));
        assert!(must_eval("mem_pct>=49.9", &gpu));
        assert!(must_eval("mem_pct<=50.1", &gpu));
        assert!(!must_eval("mem_pct>51", &gpu));
    }

    #[test]
    fn power_field_matches() {
        let gpu = make_gpu(0, 60, 50.0, 350.0);
        assert!(must_eval("power>=350", &gpu));
        assert!(must_eval("power<500", &gpu));
    }

    #[test]
    fn host_equality_matches() {
        let gpu = make_gpu(0, 60, 50.0, 100.0);
        assert!(must_eval("host==dgx-01", &gpu));
        assert!(!must_eval("host==other", &gpu));
    }

    #[test]
    fn host_regex_matches() {
        let gpu = make_gpu(0, 60, 50.0, 100.0);
        assert!(must_eval("host~=dgx", &gpu));
        // Regex with special characters must be quoted — the lexer keeps
        // the RHS of `~=` conservative so that unquoted queries like
        // `host~=dgx-01` still tokenise as a single ident.
        assert!(must_eval(r#"host~="^dgx""#, &gpu));
        assert!(!must_eval("host~=nothere", &gpu));
    }

    #[test]
    fn gpu_name_regex_matches() {
        let gpu = make_gpu(0, 60, 50.0, 100.0);
        assert!(must_eval("gpu_name~=A100", &gpu));
        assert!(must_eval("name~=A100", &gpu)); // synonym
    }

    #[test]
    fn device_type_equality() {
        let gpu = make_gpu(0, 60, 50.0, 100.0);
        assert!(must_eval("device_type==GPU", &gpu));
        assert!(!must_eval("device_type==NPU", &gpu));
    }

    #[test]
    fn index_equality_numeric_or_string() {
        let gpu = make_gpu(3, 60, 50.0, 100.0);
        assert!(must_eval("index==3", &gpu));
        assert!(must_eval("index!=0", &gpu));
    }

    #[test]
    fn pstate_zero_matches_when_present() {
        let gpu = make_gpu(0, 60, 50.0, 100.0);
        assert!(must_eval("pstate==0", &gpu));
    }

    #[test]
    fn pstate_absent_fails_closed() {
        let mut gpu = make_gpu(0, 60, 50.0, 100.0);
        gpu.performance_state = None;
        assert!(!must_eval("pstate==0", &gpu));
    }

    #[test]
    fn numa_node_matches_when_present() {
        let gpu = make_gpu(0, 60, 50.0, 100.0);
        assert!(must_eval("numa==0", &gpu));
    }

    #[test]
    fn numa_absent_fails_closed() {
        let mut gpu = make_gpu(0, 60, 50.0, 100.0);
        gpu.numa_node_id = None;
        assert!(!must_eval("numa==0", &gpu));
    }

    #[test]
    fn uuid_field_matches() {
        let gpu = make_gpu(5, 60, 50.0, 100.0);
        assert!(must_eval("uuid==GPU-5", &gpu));
    }

    #[test]
    fn driver_field_matches_from_detail() {
        let gpu = make_gpu(0, 60, 50.0, 100.0);
        // Driver versions look like `550.54.15` so we match by prefix
        // via regex. `550` alone is tokenised as a bare number by the
        // lexer, so quote it to force string/regex mode.
        assert!(must_eval(r#"driver~="550""#, &gpu));
    }

    #[test]
    fn and_combination_matches() {
        let gpu = make_gpu(0, 90, 50.0, 300.0);
        assert!(must_eval("temp>85 & util>=50", &gpu));
        assert!(!must_eval("temp>85 & util>99", &gpu));
    }

    #[test]
    fn or_combination_matches() {
        let gpu = make_gpu(0, 60, 50.0, 300.0);
        assert!(must_eval("temp>85 | util>=50", &gpu));
        assert!(!must_eval("temp>85 | util>99", &gpu));
    }

    #[test]
    fn parenthesized_combinations_work() {
        let gpu = make_gpu(0, 90, 3.0, 300.0);
        assert!(must_eval("(temp>85 | power>400) & util<5", &gpu));
    }

    #[test]
    fn user_field_missing_on_gpu_fails_closed() {
        let gpu = make_gpu(0, 60, 50.0, 100.0);
        assert!(!must_eval("user==alice", &gpu));
    }

    #[test]
    fn process_row_user_matches() {
        let proc = ProcessInfo {
            device_id: 0,
            device_uuid: "GPU-0".to_string(),
            pid: 1234,
            process_name: "cuda_app".to_string(),
            used_memory: 1024 * 1024 * 1024,
            cpu_percent: 99.0,
            memory_percent: 10.0,
            memory_rss: 0,
            memory_vms: 0,
            user: "alice".to_string(),
            state: "R".to_string(),
            start_time: String::new(),
            cpu_time: 0,
            command: String::new(),
            ppid: 1,
            threads: 1,
            uses_gpu: true,
            priority: 20,
            nice_value: 0,
            gpu_utilization: 99.0,
        };
        let expr = parse("user==alice").unwrap().unwrap();
        assert!(eval(&expr, &proc));
        let expr2 = parse("user==bob").unwrap().unwrap();
        assert!(!eval(&expr2, &proc));
    }

    #[test]
    fn cpu_row_temp_matches_when_present() {
        let cpu = CpuInfo {
            host_id: "h".to_string(),
            hostname: "n".to_string(),
            instance: String::new(),
            cpu_model: "Intel".to_string(),
            architecture: "x86_64".to_string(),
            platform_type: crate::device::types::CpuPlatformType::Intel,
            socket_count: 1,
            total_cores: 4,
            total_threads: 8,
            base_frequency_mhz: 2400,
            max_frequency_mhz: 3600,
            cache_size_mb: 8,
            utilization: 40.0,
            temperature: Some(75),
            power_consumption: Some(95.0),
            per_socket_info: Vec::new(),
            apple_silicon_info: None,
            per_core_utilization: Vec::new(),
            time: String::new(),
        };
        let expr = parse("temp>70").unwrap().unwrap();
        assert!(eval(&expr, &cpu));
    }

    #[test]
    fn cpu_row_temp_missing_fails_closed() {
        let cpu = CpuInfo {
            host_id: "h".to_string(),
            hostname: "n".to_string(),
            instance: String::new(),
            cpu_model: "Intel".to_string(),
            architecture: "x86_64".to_string(),
            platform_type: crate::device::types::CpuPlatformType::Intel,
            socket_count: 1,
            total_cores: 4,
            total_threads: 8,
            base_frequency_mhz: 2400,
            max_frequency_mhz: 3600,
            cache_size_mb: 8,
            utilization: 40.0,
            temperature: None,
            power_consumption: None,
            per_socket_info: Vec::new(),
            apple_silicon_info: None,
            per_core_utilization: Vec::new(),
            time: String::new(),
        };
        let expr = parse("temp>70").unwrap().unwrap();
        assert!(!eval(&expr, &cpu));
    }

    #[test]
    fn epsilon_numeric_equality_exact_match() {
        let gpu = make_gpu(0, 60, 100.0, 100.0);
        assert!(must_eval("util==100", &gpu));
    }

    #[test]
    fn numeric_equality_far_from_match() {
        let gpu = make_gpu(0, 60, 42.0, 100.0);
        assert!(!must_eval("util==100", &gpu));
    }
}
