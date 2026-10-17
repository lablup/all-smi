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

mod chassis_metrics_parser;
pub mod client;
pub mod metrics_parser;
// The russh-backed modules are absent where russh cannot build (Windows on
// ARM64, issue #459); see `emit_ssh_cfg` in build.rs. The transport-neutral
// SSH helpers below stay compiled there (the library exports them and the
// integration tests use them), but the binary has no caller for them, so
// dead-code warnings are allowed in that configuration only.
#[cfg_attr(not(all_smi_ssh), allow(dead_code))]
pub mod nvidia_smi_shim;
#[cfg_attr(not(all_smi_ssh), allow(dead_code))]
pub mod rocm_smi_shim;
#[cfg(all_smi_ssh)]
pub mod ssh_client;
#[cfg_attr(not(all_smi_ssh), allow(dead_code))]
pub mod ssh_decision;
#[cfg(all_smi_ssh)]
pub mod ssh_host_key;
#[cfg_attr(not(all_smi_ssh), allow(dead_code))]
pub mod ssh_target;
#[cfg_attr(not(all_smi_ssh), allow(dead_code))]
pub mod ssh_transport;
pub mod webhook;

pub use client::NetworkClient;
