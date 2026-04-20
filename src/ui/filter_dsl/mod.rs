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

//! Small filter DSL used by the TUI `/` command.
//!
//! The DSL lets operators write things like:
//!
//! ```text
//! temp>85
//! util<5 & host~=dgx
//! user==alice | power>400
//! ```
//!
//! Entry points:
//! - [`parse`] — turn a query string into an [`Expr`], or a [`ParseError`]
//!   with column info.
//! - [`apply`] — a helper wrapping [`eval`] so callers can check whether a
//!   row passes the current filter without unpacking `Option<Expr>` by
//!   hand.

pub mod eval;
pub mod lexer;
pub mod parser;

pub use eval::DeviceRowView;
pub use parser::{Expr, parse};
#[allow(unused_imports)]
pub use parser::{Field, ParseError, Value};

/// Row-level helper used by renderers. Returns `true` when the row should
/// be rendered at full strength (either no filter is active, or the filter
/// matches). Returns `false` when the row should be dimmed or hidden.
pub fn apply<R: DeviceRowView + ?Sized>(query: Option<&Expr>, row: &R) -> bool {
    match query {
        Some(expr) => eval::eval(expr, row),
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::types::GpuInfo;
    use std::collections::HashMap;

    fn simple_gpu() -> GpuInfo {
        let mut detail = HashMap::new();
        detail.insert("index".to_string(), "0".to_string());
        GpuInfo {
            uuid: "GPU-0".to_string(),
            time: String::new(),
            name: "NVIDIA A100".to_string(),
            device_type: "GPU".to_string(),
            host_id: "h".to_string(),
            hostname: "dgx-01".to_string(),
            instance: String::new(),
            utilization: 50.0,
            ane_utilization: 0.0,
            dla_utilization: None,
            tensorcore_utilization: None,
            temperature: 85,
            used_memory: 0,
            total_memory: 0,
            frequency: 0,
            power_consumption: 0.0,
            gpu_core_count: None,
            temperature_threshold_slowdown: None,
            temperature_threshold_shutdown: None,
            temperature_threshold_max_operating: None,
            temperature_threshold_acoustic: None,
            performance_state: None,
            numa_node_id: None,
            gsp_firmware_mode: None,
            gsp_firmware_version: None,
            nvlink_remote_devices: Vec::new(),
            gpm_metrics: None,
            detail,
        }
    }

    #[test]
    fn apply_no_filter_matches_everything() {
        let gpu = simple_gpu();
        assert!(apply(None, &gpu));
    }

    #[test]
    fn apply_with_matching_filter_returns_true() {
        let gpu = simple_gpu();
        let expr = parse("temp>=85").unwrap().unwrap();
        assert!(apply(Some(&expr), &gpu));
    }

    #[test]
    fn apply_with_non_matching_filter_returns_false() {
        let gpu = simple_gpu();
        let expr = parse("temp>100").unwrap().unwrap();
        assert!(!apply(Some(&expr), &gpu));
    }
}
