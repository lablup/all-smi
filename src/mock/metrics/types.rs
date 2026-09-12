//! Common types used across metrics modules

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

#[derive(Clone, Debug, PartialEq)]
pub enum PlatformType {
    Nvidia,
    Apple,
    Jetson,
    Intel,
    Amd,
    AmdGpu,
    Tenstorrent,
    Rebellions,
    Furiosa,
    Gaudi,
    /// AWS Neuron (Trainium / Inferentia).
    Neuron,
}

impl PlatformType {
    pub fn from_str(platform_str: &str) -> Self {
        match platform_str.to_lowercase().as_str() {
            "nvidia" => PlatformType::Nvidia,
            "apple" => PlatformType::Apple,
            "jetson" => PlatformType::Jetson,
            "intel" => PlatformType::Intel,
            "amd" => PlatformType::Amd,
            "amdgpu" | "amd-gpu" | "amd_gpu" => PlatformType::AmdGpu,
            "tenstorrent" | "tt" => PlatformType::Tenstorrent,
            "rebellions" | "rbln" => PlatformType::Rebellions,
            "furiosa" => PlatformType::Furiosa,
            "gaudi" | "habana" => PlatformType::Gaudi,
            "neuron" | "trainium" | "inferentia" => PlatformType::Neuron,
            _ => {
                eprintln!("Unknown platform '{platform_str}', defaulting to nvidia");
                PlatformType::Nvidia
            }
        }
    }
}
