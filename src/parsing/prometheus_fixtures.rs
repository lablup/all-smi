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

//! Captured `all-smi-mock-server` expositions used by parser tests.
//!
//! Each file is one `/metrics` response from the mock server with every
//! optional family enabled:
//!
//! ```text
//! ALL_SMI_MOCK_HARDWARE_DETAILS=1 ALL_SMI_MOCK_VGPU=1 ALL_SMI_MOCK_MIG=1 \
//! ALL_SMI_MOCK_PROCESSES=1 ALL_SMI_MOCK_TOPOLOGY=1 \
//!   all-smi-mock-server --port-range 19301 --platform <platform>
//! curl -s http://127.0.0.1:19301/metrics > tests/fixtures/prometheus/mock-<platform>.prom
//! ```

/// `(platform, exposition)` for every captured mock template.
pub(crate) const ALL: &[(&str, &str)] = &[
    (
        "nvidia",
        include_str!("../../tests/fixtures/prometheus/mock-nvidia.prom"),
    ),
    (
        "amdgpu",
        include_str!("../../tests/fixtures/prometheus/mock-amdgpu.prom"),
    ),
    (
        "apple",
        include_str!("../../tests/fixtures/prometheus/mock-apple.prom"),
    ),
    (
        "jetson",
        include_str!("../../tests/fixtures/prometheus/mock-jetson.prom"),
    ),
    (
        "intel",
        include_str!("../../tests/fixtures/prometheus/mock-intel.prom"),
    ),
    (
        "tenstorrent",
        include_str!("../../tests/fixtures/prometheus/mock-tenstorrent.prom"),
    ),
    (
        "rebellions",
        include_str!("../../tests/fixtures/prometheus/mock-rebellions.prom"),
    ),
    (
        "furiosa",
        include_str!("../../tests/fixtures/prometheus/mock-furiosa.prom"),
    ),
    (
        "gaudi",
        include_str!("../../tests/fixtures/prometheus/mock-gaudi.prom"),
    ),
    (
        "neuron",
        include_str!("../../tests/fixtures/prometheus/mock-neuron.prom"),
    ),
];
